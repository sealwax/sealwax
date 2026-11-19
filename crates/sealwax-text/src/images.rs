//! The images a page draws: where, how large, how they are stored; and one of them extracted, as
//! the file it is stored as when it is one (JPEG, JPEG 2000), else as a PNG of its pixels.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::font::GlyphRun;
use hayro_interpret::hayro_syntax::content::TypedIter;
use hayro_interpret::hayro_syntax::object::{Array, Dict, Name, Stream};
use hayro_interpret::kurbo::{Affine, BezPath, Point, Rect};
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageData, ImageDrawProps,
    InterpreterSettings, SoftMask, interpret,
};
use sealwax_core::{Document, ObjId};

use crate::cache::with_cache;
use crate::drawn::Bounds;

/// The most images listed for a page: a page drawing more is cut there.
const MAX_IMAGES: usize = 100_000;

/// An image a page draws.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PageImage {
    /// The matrix mapping its unit square into the page's default user space (unrotated, y up).
    pub transform: [f64; 6],
    /// The box its unit square covers there.
    pub bbox: Bounds,
    /// Its size in pixels, as stored.
    pub width: u32,
    pub height: u32,
    /// The image XObject drawn (serialized as its number); `None` for an image written in the
    /// content (`BI`).
    #[cfg_attr(feature = "serde", serde(serialize_with = "object_number"))]
    pub object: Option<ObjId>,
    /// Its colour space family (`DeviceRGB`, `ICCBased`, `Indexed`...), when its dictionary
    /// names one; `None` for a stencil mask, or an inline image naming a resource.
    pub color_space: Option<String>,
    pub bits_per_component: Option<u8>,
    /// Its filters, in the order they decode (`DCTDecode`, `FlateDecode`...).
    pub filters: Vec<String>,
    /// A stencil mask (`/ImageMask`): it paints the fill colour where it is set.
    pub stencil: bool,
    /// It has a soft mask (`/SMask`, `/SMaskInData`) or a mask (`/Mask`).
    pub masked: bool,
}

#[cfg(feature = "serde")]
fn object_number<S: serde::Serializer>(id: &Option<ObjId>, out: S) -> Result<S::Ok, S::Error> {
    match id {
        Some(id) => out.serialize_some(&id.number),
        None => out.serialize_none(),
    }
}

/// How an extracted image is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ImageFormat {
    /// The JPEG file it is stored as (`DCTDecode`).
    Jpeg,
    /// The JPEG 2000 file it is stored as (`JPXDecode`).
    Jpeg2000,
    /// A PNG of its pixels, as a reader shows them: RGB or gray, with its soft mask as alpha.
    Png,
}

/// An image extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedImage {
    pub format: ImageFormat,
    pub data: Vec<u8>,
    /// Its size in pixels: as decoded for a PNG, as stored for the others.
    pub width: u32,
    pub height: u32,
}

/// The images page `page_index` (from 0) draws, in the order of its content, forms included: an
/// image drawn twice is listed twice. Annotations' appearances are left out.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have; what
/// [`Document::check_page`] refuses (the limits, the deadline);
/// [`sealwax_core::Error::Timeout`] when the deadline passes while the page is read.
pub fn images(doc: &Document, page_index: usize) -> sealwax_core::Result<Vec<PageImage>> {
    let mut lister = Lister::default();
    walk(doc, page_index, &mut lister)?;
    Ok(lister.found)
}

/// The `index`-th image (from 0) of [`images`] on page `page_index`: the bytes it is stored as
/// when they are a file of their own (a JPEG or JPEG 2000 without mask, decode array or other
/// filter), else a PNG of its pixels as decoded (a stencil mask black where it paints, white
/// elsewhere).
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have, or an image
/// it does not draw; [`sealwax_core::Error::LimitExceeded`] for an image of more pixels than
/// [`sealwax_core::Limits::max_image_pixels`]; [`sealwax_core::Error::Malformed`] for one that
/// cannot be decoded; as [`images`].
pub fn extract_image(
    doc: &Document,
    page_index: usize,
    index: usize,
) -> sealwax_core::Result<ExtractedImage> {
    let mut extractor = Extractor {
        index,
        seen: 0,
        max_pixels: doc.limits().max_image_pixels,
        found: None,
    };
    walk(doc, page_index, &mut extractor)?;
    match extractor.found {
        Some(Ok(image)) => Ok(image),
        Some(Err(error)) => Err(error),
        None if extractor.seen <= index => Err(sealwax_core::Error::InvalidArgument {
            what: "image index",
        }),
        None => Err(sealwax_core::Error::Malformed),
    }
}

/// Interpret page `page_index`'s content with `device`.
fn walk<'d>(
    doc: &Document,
    page_index: usize,
    device: &mut (impl for<'a> Device<'a> + 'd),
) -> sealwax_core::Result<()> {
    let current = doc.reopened_if_modified()?;
    let doc = current.as_deref().unwrap_or(doc);
    if page_index >= doc.hayro().pages().len() {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
    }
    doc.check_page(page_index)?;
    with_cache(doc, |pdf, cache| {
        let pages = pdf.pages();
        let Some(page) = pages.get(page_index) else {
            return;
        };
        // What was read before the interpreter failed, if it did, is kept.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut context = Context::new(
                Affine::IDENTITY,
                Rect::new(-1e9, -1e9, 1e9, 1e9),
                cache,
                pdf.xref(),
                InterpreterSettings::default(),
            );
            let content = page.page_stream().unwrap_or_default();
            interpret(
                TypedIter::new(content),
                page.resources(),
                &mut context,
                device,
            );
        }));
    });
    doc.check_time()
}

/// The matrix mapping an image's unit square into user space, from the one hayro draws it with,
/// which maps its pixels, the first row on top.
fn unit_square(drawn: Affine, width: u32, height: u32) -> Affine {
    let (w, h) = (f64::from(width), f64::from(height));
    drawn * Affine::new([w, 0.0, 0.0, -h, 0.0, h])
}

/// The image `stream` describes, its unit square mapped by `transform`.
fn describe(stream: &Stream<'_>, transform: Affine, width: u32, height: u32) -> PageImage {
    let dict = stream.dict();
    let flag = |long: &[u8], short: &[u8]| {
        dict.get::<bool>(long)
            .or_else(|| dict.get::<bool>(short))
            .unwrap_or(false)
    };
    let stencil = flag(b"ImageMask", b"IM");
    let color_space = if stencil {
        None
    } else {
        dict.get::<Name<'_>>(b"ColorSpace")
            .or_else(|| dict.get::<Name<'_>>(b"CS"))
            .and_then(|name| family(name.as_ref()))
            .or_else(|| {
                dict.get::<Array<'_>>(b"ColorSpace")
                    .or_else(|| dict.get::<Array<'_>>(b"CS"))
                    .and_then(|array| array.iter::<Name<'_>>().next())
                    .and_then(|name| family(name.as_ref()))
            })
    };
    let bits_per_component = dict
        .get::<u8>(b"BitsPerComponent")
        .or_else(|| dict.get::<u8>(b"BPC"))
        .or(stencil.then_some(1));
    let masked = dict.contains_key(b"SMask")
        || dict.contains_key(b"Mask")
        || dict.get::<i32>(b"SMaskInData").is_some_and(|v| v > 0);
    let corners =
        [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(x, y)| transform * Point::new(x, y));
    let bbox = [
        corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min),
        corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min),
        corners
            .iter()
            .map(|p| p.x)
            .fold(f64::NEG_INFINITY, f64::max),
        corners
            .iter()
            .map(|p| p.y)
            .fold(f64::NEG_INFINITY, f64::max),
    ];
    PageImage {
        transform: transform.as_coeffs(),
        bbox,
        width,
        height,
        object: dict.obj_id().map(|id| ObjId {
            number: id.obj_number,
            generation: u16::try_from(id.gen_number).unwrap_or(0),
        }),
        color_space,
        bits_per_component,
        filters: filters(dict),
        stencil,
        masked,
    }
}

/// The colour space family a name gives, inline abbreviations read; `None` for a name of the
/// resources.
fn family(name: &[u8]) -> Option<String> {
    let full: &[u8] = match name {
        b"G" => b"DeviceGray",
        b"RGB" => b"DeviceRGB",
        b"CMYK" => b"DeviceCMYK",
        b"I" => b"Indexed",
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"Indexed" | b"ICCBased" | b"CalGray"
        | b"CalRGB" | b"Lab" | b"Separation" | b"DeviceN" | b"Pattern" => name,
        _ => return None,
    };
    Some(String::from_utf8_lossy(full).into_owned())
}

/// The filters of an image's dictionary (`/Filter`, or `/F` inline), abbreviations read.
fn filters(dict: &Dict<'_>) -> Vec<String> {
    let names: Vec<Vec<u8>> = match dict
        .get::<Name<'_>>(b"Filter")
        .or_else(|| dict.get::<Name<'_>>(b"F"))
    {
        Some(name) => vec![name.as_ref().to_vec()],
        None => dict
            .get::<Array<'_>>(b"Filter")
            .or_else(|| dict.get::<Array<'_>>(b"F"))
            .map(|array| {
                array
                    .iter::<Name<'_>>()
                    .map(|n| n.as_ref().to_vec())
                    .collect()
            })
            .unwrap_or_default(),
    };
    names
        .iter()
        .map(|name| {
            let full: &[u8] = match name.as_slice() {
                b"AHx" => b"ASCIIHexDecode",
                b"A85" => b"ASCII85Decode",
                b"LZW" => b"LZWDecode",
                b"Fl" => b"FlateDecode",
                b"RL" => b"RunLengthDecode",
                b"CCF" => b"CCITTFaxDecode",
                b"DCT" => b"DCTDecode",
                other => other,
            };
            String::from_utf8_lossy(full).into_owned()
        })
        .collect()
}

/// The stream of an image drawn.
fn stream_of<'s>(image: &'s Image<'_, '_>) -> &'s Stream<'s> {
    match image {
        Image::Raster(raster) => raster.stream(),
        Image::Stencil(stencil) => stencil.stream(),
    }
}

#[derive(Default)]
struct Lister {
    found: Vec<PageImage>,
}

impl<'a> Device<'a> for Lister {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        if self.found.len() < MAX_IMAGES {
            let (width, height) = (image.width(), image.height());
            let transform = unit_square(props.transform, width, height);
            self.found
                .push(describe(stream_of(&image), transform, width, height));
        }
    }
}

struct Extractor {
    index: usize,
    seen: usize,
    max_pixels: u64,
    found: Option<sealwax_core::Result<ExtractedImage>>,
}

impl<'a> Device<'a> for Extractor {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        let at = self.seen;
        self.seen += 1;
        if at != self.index || self.found.is_some() {
            return;
        }
        let (width, height) = (image.width(), image.height());
        if u64::from(width) * u64::from(height) > self.max_pixels {
            self.found = Some(Err(sealwax_core::Error::LimitExceeded {
                what: "max_image_pixels",
            }));
            return;
        }
        let stream = stream_of(&image);
        let described = describe(
            stream,
            unit_square(props.transform, width, height),
            width,
            height,
        );
        let has_decode = stream.dict().contains_key(b"Decode") || stream.dict().contains_key(b"D");
        let stored = match described.filters.as_slice() {
            [only] if only == "DCTDecode" => Some(ImageFormat::Jpeg),
            [only] if only == "JPXDecode" => Some(ImageFormat::Jpeg2000),
            _ => None,
        };
        if let Some(format) = stored
            && !described.masked
            && !described.stencil
            && !has_decode
        {
            self.found = Some(Ok(ExtractedImage {
                format,
                data: stream.raw_data().into_owned(),
                width,
                height,
            }));
            return;
        }
        let max_pixels = self.max_pixels;
        let mut found = None;
        match image {
            Image::Raster(raster) => raster.with_rgba(
                |data, alpha| {
                    let (width, height) = (data.width(), data.height());
                    if u64::from(width) * u64::from(height) > max_pixels {
                        found = Some(Err(sealwax_core::Error::LimitExceeded {
                            what: "max_image_pixels",
                        }));
                        return;
                    }
                    let (channels, samples) = match data {
                        ImageData::Rgb(rgb) => (3, rgb.data),
                        ImageData::Luma(luma) => (1, luma.data),
                    };
                    let alpha = alpha.map(|a| resized(&a.data, a.width, a.height, width, height));
                    found = Some(png(width, height, channels, &samples, alpha.as_deref()));
                },
                None,
            ),
            Image::Stencil(stencil) => stencil.with_stencil(
                |luma, _| {
                    if u64::from(luma.width) * u64::from(luma.height) > max_pixels {
                        found = Some(Err(sealwax_core::Error::LimitExceeded {
                            what: "max_image_pixels",
                        }));
                        return;
                    }
                    // Painted where set: black there, white elsewhere.
                    let samples: Vec<u8> = luma.data.iter().map(|v| 255 - v).collect();
                    found = Some(png(luma.width, luma.height, 1, &samples, None));
                },
                None,
            ),
        }
        self.found = Some(found.unwrap_or(Err(sealwax_core::Error::Malformed)));
    }
}

/// `alpha`, `width` by `height`, brought to `to_width` by `to_height` (nearest sample): a soft
/// mask may have a size of its own.
fn resized(alpha: &[u8], width: u32, height: u32, to_width: u32, to_height: u32) -> Vec<u8> {
    if (width, height) == (to_width, to_height) || width == 0 || height == 0 {
        return alpha.to_vec();
    }
    let mut out = Vec::with_capacity(to_width as usize * to_height as usize);
    for y in 0..to_height {
        let source_y = (u64::from(y) * u64::from(height) / u64::from(to_height)) as usize;
        for x in 0..to_width {
            let source_x = (u64::from(x) * u64::from(width) / u64::from(to_width)) as usize;
            out.push(
                alpha
                    .get(source_y * width as usize + source_x)
                    .copied()
                    .unwrap_or(255),
            );
        }
    }
    out
}

/// A PNG of `samples` (`channels` of 8 bits a pixel), with `alpha` when given.
fn png(
    width: u32,
    height: u32,
    channels: u8,
    samples: &[u8],
    alpha: Option<&[u8]>,
) -> sealwax_core::Result<ExtractedImage> {
    let pixels = width as usize * height as usize;
    let (color, data) = match (channels, alpha) {
        (3, None) => (png::ColorType::Rgb, samples.to_vec()),
        (1, None) => (png::ColorType::Grayscale, samples.to_vec()),
        (3, Some(alpha)) => (
            png::ColorType::Rgba,
            samples
                .as_chunks::<3>()
                .0
                .iter()
                .zip(alpha)
                .flat_map(|([r, g, b], a)| [*r, *g, *b, *a])
                .collect(),
        ),
        (_, Some(alpha)) => (
            png::ColorType::GrayscaleAlpha,
            samples
                .iter()
                .zip(alpha)
                .flat_map(|(g, a)| [*g, *a])
                .collect(),
        ),
        _ => (png::ColorType::Grayscale, samples.to_vec()),
    };
    let expected = pixels * data.len().checked_div(pixels).unwrap_or(0);
    if pixels == 0 || data.len() != expected {
        return Err(sealwax_core::Error::Malformed);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|_| sealwax_core::Error::Malformed)?;
        writer
            .write_image_data(&data)
            .map_err(|_| sealwax_core::Error::Malformed)?;
    }
    Ok(ExtractedImage {
        format: ImageFormat::Png,
        data: out,
        width,
        height,
    })
}
