//! PDF pages rendered to images: one page to a colour PNG ([`render_png`]), or every page to the
//! grayscale PDF a fax sends ([`grayscale`]).
//!
//! Each page is rendered whole (what a reader shows: text, images in any colour space, shadings,
//! annotations, flattened first) to one 8-bit gray image, and the output has one page per image,
//! of the page's size in points. Rendering is hayro's: against PDFium at 100 dpi, gray levels
//! averaged over 4 x 4 pixels differ by about one level at the median, the same ink, glyphs drawn
//! a little differently.

use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use hayro::hayro_interpret::{InterpreterSettings, TransformExt as _};
use hayro::hayro_syntax::Pdf;
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{PixmapSettings, RenderCache, RenderSettings, render as render_page, render_into};
use sealwax_core::{Dict, Document, Name, Object, OpenOptions, Rect, SaveOptions, Stream};

/// The fine resolution of a fax.
pub const DEFAULT_DPI: f32 = 200.0;
/// The longest side of a rendered page, in pixels: 17 inches (tabloid) at 200 dpi. A larger
/// page is rendered at a lower resolution.
pub const MAX_SIDE_PX: f32 = 3400.0;

/// How far past a whole number of pixels a side may reach and not take one more.
const PIXEL_TOLERANCE: f64 = 1e-3;

/// Everything that can go wrong.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Reading, flattening or writing the document failed.
    #[error(transparent)]
    Pdf(#[from] sealwax_core::Error),
    #[error(transparent)]
    Annots(#[from] sealwax_annots::Error),
    /// The renderer cannot read the flattened document.
    #[error("PDF cannot be rendered")]
    Unrenderable,
}

/// Result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// How to render a page ([`render`]).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct RenderOptions {
    /// Pixels per point: 150 / 72 for 150 dpi.
    pub scale: f64,
    /// Quarter turns clockwise (counterclockwise when negative) added to the page's rotation:
    /// the image is the page as it would show turned so.
    pub quarter_turns: i32,
}

impl RenderOptions {
    /// At `scale` pixels per point, not turned.
    #[must_use]
    pub fn new(scale: f64) -> Self {
        Self {
            scale,
            quarter_turns: 0,
        }
    }

    /// Turned `quarter_turns` more quarter turns clockwise.
    #[must_use]
    pub fn quarter_turns(mut self, quarter_turns: i32) -> Self {
        self.quarter_turns = quarter_turns;
        self
    }
}

/// A rendered page: `pixels` holds its `width` x `height` pixels, row by row from the top, four
/// bytes each (red, green, blue, alpha), opaque over white: what a canvas's `ImageData` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raster {
    /// In pixels.
    pub width: u32,
    /// In pixels.
    pub height: u32,
    /// RGBA, 8 bits each.
    pub pixels: Vec<u8>,
}

impl Raster {
    /// The image as a PNG, 8-bit RGB.
    ///
    /// # Errors
    /// [`Error::Unrenderable`] when it cannot be encoded.
    pub fn png(&self) -> Result<Vec<u8>> {
        let rgb: Vec<u8> = self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|[r, g, b, _]| [*r, *g, *b])
            .collect();
        png_rgb(&rgb, self.width, self.height)
    }
}

/// Page `page_index` (from 0) rendered as a reader shows it (its annotations and the fields'
/// values included, the document left as it is), turned as `options` say, to RGBA pixels over
/// white. The image covers the page's crop box as shown (rotated), at `options.scale` pixels per
/// point: its sides are the rotated crop box's, times the scale, rounded up as MuPDF and PDFium
/// size their images (a thousandth of a pixel over a whole number does not count), and the page
/// is drawn at that scale exactly. The user unit is not applied, as PDF readers render.
///
/// `cancelled` is asked between the steps of the work (reading the page, interpreting it,
/// drawing it): once it answers `true`, the rendering stops and returns `None`, nothing drawn.
///
/// A modified document (a field filled) is rendered as it stands: saved and read again once,
/// until it changes again ([`Document::reopened_if_modified`]).
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have or a scale
/// that is not a positive number; [`sealwax_core::Error::LimitExceeded`] naming
/// `max_render_pixels` for an image larger than [`sealwax_core::Limits::max_render_pixels`] (or
/// with a side past 65,535 pixels), or the bound of [`Document::check_page`] the page passes;
/// [`sealwax_core::Error::Timeout`] past the deadline; [`Error::Unrenderable`] when the renderer
/// fails.
pub fn render(
    doc: &Document,
    page_index: usize,
    options: &RenderOptions,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Raster>> {
    let scale = options.scale;
    if !(scale.is_finite() && scale > 0.0) {
        return Err(sealwax_core::Error::InvalidArgument { what: "scale" }.into());
    }
    if cancelled() {
        return Ok(None);
    }
    let reopened = doc.reopened_if_modified()?;
    let doc = reopened.as_deref().unwrap_or(doc);
    // The renderer has no bounds of its own.
    doc.check_page(page_index)?;
    let pages = doc.hayro().pages();
    let page = pages
        .get(page_index)
        .ok_or(sealwax_core::Error::InvalidArgument { what: "page index" })?;
    let (width, height) = page.render_dimensions();
    let turns = options.quarter_turns.rem_euclid(4);
    let (width, height) = if turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };
    let too_large = sealwax_core::Error::LimitExceeded {
        what: "max_render_pixels",
    };
    let pixels = |points: f32| -> Option<u16> {
        let pixels = (f64::from(points) * scale - PIXEL_TOLERANCE)
            .ceil()
            .max(1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        (pixels <= f64::from(u16::MAX)).then_some(pixels as u16)
    };
    let (Some(columns), Some(rows)) = (pixels(width), pixels(height)) else {
        return Err(too_large.into());
    };
    if u64::from(columns) * u64::from(rows) > doc.limits().max_render_pixels {
        return Err(too_large.into());
    }
    if cancelled() {
        return Ok(None);
    }
    // The page as shown, then turned in its own points: the quarter turns a `/Rotate` adds.
    let (w, h) = (f64::from(width), f64::from(height));
    let turned = match turns {
        1 => Affine::new([0.0, 1.0, -1.0, 0.0, w, 0.0]),
        2 => Affine::new([-1.0, 0.0, 0.0, -1.0, w, h]),
        3 => Affine::new([0.0, -1.0, 1.0, 0.0, 0.0, h]),
        _ => Affine::IDENTITY,
    };
    let cache = RenderCache::new();
    // A panic of the renderer is an error of this page, not of the process.
    let drawn = catch_unwind(AssertUnwindSafe(|| {
        let mut context = RenderContext::new(columns, rows);
        let transform = Affine::scale(scale) * turned * page.initial_transform(true).to_kurbo();
        render_into(
            page,
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &mut context,
            transform,
        );
        context.flush();
        if cancelled() {
            return None;
        }
        let mut pixmap = Pixmap::new(columns, rows);
        context.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..RasterizerSettings::default()
            },
        );
        Some(pixmap)
    }))
    .map_err(|_| Error::Unrenderable)?;
    doc.check_time()?;
    let Some(pixmap) = drawn else {
        return Ok(None);
    };
    Ok(Some(Raster {
        width: u32::from(columns),
        height: u32::from(rows),
        pixels: opaque_rgba(pixmap.data_as_u8_slice()),
    }))
}

/// Page `page_index` (from 0) rendered at `scale` pixels per point, as [`render`] renders it, to
/// a PNG image, 8-bit RGB.
///
/// # Errors
/// As [`render`].
pub fn render_png(doc: &Document, page_index: usize, scale: f64) -> Result<Vec<u8>> {
    let options = RenderOptions::new(scale);
    match render(doc, page_index, &options, &|| false)? {
        Some(raster) => raster.png(),
        None => Err(Error::Unrenderable),
    }
}

/// Premultiplied RGBA pixels over white, as opaque RGBA.
fn opaque_rgba(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for [r, g, b, a] in rgba.as_chunks::<4>().0 {
        let paper = 255 - a;
        out.extend_from_slice(&[
            r.saturating_add(paper),
            g.saturating_add(paper),
            b.saturating_add(paper),
            255,
        ]);
    }
    out
}

fn png_rgb(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(rgb))
        .map_err(|_| Error::Unrenderable)?;
    Ok(out)
}

/// Every page of `data` rendered to one gray image at `dpi` (lower for a page whose longest side
/// would pass [`MAX_SIDE_PX`]), in a new PDF of as many pages, each of the size the page shows
/// (its crop box, rotated, in points, times its user unit). Pages are rendered one at a time.
///
/// # Errors
/// As [`Document::open`] for `data` (a password-protected PDF is refused), as
/// [`Document::check_pages`] (the options' limits), as [`sealwax_annots::flatten`], and
/// [`Error::Unrenderable`], also when the renderer fails on a page. Past the limits' deadline,
/// [`sealwax_core::Error::Timeout`].
pub fn grayscale(data: &[u8], dpi: f32, options: &OpenOptions) -> Result<Vec<u8>> {
    let mut doc = Document::open(data.to_vec(), options)?;
    // The renderer has no bounds of its own.
    doc.check_pages()?;
    let geometry = doc.geometry();
    sealwax_annots::flatten(&mut doc)?;
    let flattened = doc.save(&SaveOptions::default())?;
    let pdf = Pdf::new(flattened).map_err(|_| Error::Unrenderable)?;
    if pdf.pages().len() != geometry.len() {
        return Err(Error::Unrenderable);
    }
    let mut out = Document::empty()?;
    let cache = RenderCache::new();
    for (page, page_geometry) in pdf.pages().iter().zip(&geometry) {
        doc.check_time()?;
        let crop = page_geometry.crop_box;
        let unit = if page_geometry.user_unit > 0.0 {
            page_geometry.user_unit
        } else {
            1.0
        };
        #[allow(clippy::cast_possible_truncation)]
        let longest = (crop.width().max(crop.height()) * unit) as f32;
        let page_dpi = if longest > 0.0 {
            dpi.min(MAX_SIDE_PX * 72.0 / longest)
        } else {
            dpi
        };
        // The page as shown (rotated), in whole pixels as PDF SDKs size their bitmaps (the integer
        // part, in double precision); hayro computes in single precision, which loses a column
        // (612 points at 200 dpi are 1,699.99998 pixels there).
        let (shown_width, shown_height) = match page_geometry.rotation.degrees() {
            90 | 270 => (crop.height(), crop.width()),
            _ => (crop.width(), crop.height()),
        };
        let scale = f64::from(page_dpi) * unit / 72.0;
        let axis = |points: f64| -> f32 {
            if points.is_nan() || points <= 0.0 {
                #[allow(clippy::cast_possible_truncation)]
                return scale as f32;
            }
            let pixels = (points * scale + 1e-6).floor().max(1.0);
            #[allow(clippy::cast_possible_truncation)]
            let factor = ((pixels + 0.01) / points) as f32;
            factor
        };
        let settings = PixmapSettings {
            x_scale: axis(shown_width),
            y_scale: axis(shown_height),
            ..PixmapSettings::default()
        };
        // A panic of the renderer is an error of this page, not of the process.
        let pixmap = catch_unwind(AssertUnwindSafe(|| {
            render_page(
                page,
                &cache,
                &InterpreterSettings::default(),
                &RenderSettings::default(),
                &settings,
            )
        }))
        .map_err(|_| Error::Unrenderable)?;
        let (width, height) = (pixmap.width(), pixmap.height());
        let gray = luminance(pixmap.data_as_u8_slice());
        let image = out.add_object(gray_image(&gray, width, height))?;
        let size = |pixels: u16| f64::from(pixels) * 72.0 / f64::from(page_dpi);
        let (page_width, page_height) = (size(width), size(height));
        let content = format!(
            "q {} 0 0 {} 0 0 cm /Im0 Do Q",
            number(page_width),
            number(page_height)
        );
        let contents = out.add_object(flate(Dict::new(), content.as_bytes()))?;
        let mut xobjects = Dict::new();
        xobjects.insert("Im0", Object::Ref(image));
        let mut resources = Dict::new();
        resources.insert("XObject", xobjects);
        let mut new_page = Dict::new();
        new_page.insert("Resources", resources);
        new_page.insert("Contents", Object::Ref(contents));
        out.push_page(
            new_page,
            Rect::from_corners(0.0, 0.0, page_width, page_height),
        )?;
    }
    Ok(out.save(&SaveOptions::default())?)
}

/// The luminance (ITU-R BT.601) of premultiplied RGBA pixels, over white.
fn luminance(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|[r, g, b, a]| {
            let paper = 255.0 - f32::from(*a);
            let gray = 0.299 * (f32::from(*r) + paper)
                + 0.587 * (f32::from(*g) + paper)
                + 0.114 * (f32::from(*b) + paper);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let gray = gray.round().clamp(0.0, 255.0) as u8;
            gray
        })
        .collect()
}

fn gray_image(samples: &[u8], width: u16, height: u16) -> Object {
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("XObject"));
    dict.insert("Subtype", Name::from("Image"));
    dict.insert("Width", i64::from(width));
    dict.insert("Height", i64::from(height));
    dict.insert("ColorSpace", Name::from("DeviceGray"));
    dict.insert("BitsPerComponent", 8i64);
    flate(dict, samples)
}

fn flate(mut dict: Dict, data: &[u8]) -> Object {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let compressed = encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    if compressed.is_empty() && !data.is_empty() {
        return Object::Stream(Stream {
            dict,
            data: data.to_vec(),
        });
    }
    dict.insert("Filter", Name::from("FlateDecode"));
    Object::Stream(Stream {
        dict,
        data: compressed,
    })
}

/// A number in a content stream: at most four decimals, no trailing zero.
fn number(value: f64) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}
