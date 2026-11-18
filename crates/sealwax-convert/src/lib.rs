//! Other formats converted to PDF: laid out by an engine of their own, written by
//! [`sealwax_compose`]. DOCX first, laid out by [docxide-pdf](https://github.com/sverrejb/docxide-pdf).
//!
//! The engine hands over laid-out pages (docxide's [`display`] module); this crate translates
//! them into a [`sealwax_compose::Composition`] and composes it. Font files the layout names on
//! disk are read here, at the boundary.
//!
//! What [`Limits`] bound: the deadline, checked before the layout and as pages are composed, and
//! what [`sealwax_compose::compose`] bounds. The layout itself is docxide-pdf's: neither its time
//! nor its memory (a ZIP that expands without end) is bounded, so convert documents you trust,
//! or in a process you can stop. A panic in docxide-pdf is caught as a document it cannot lay
//! out, unless the build aborts on panic.
//!
//! Not published on crates.io yet: the display list is proposed upstream
//! (sverrejb/docxide-pdf#168) and comes from a fork until docxide-pdf publishes it.

use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

pub use docxide_pdf::display;
use sealwax_compose::{
    Color, Composed, Composition, FillRule, Font, Glyph, GradientStop, Image, ImageColorSpace,
    ImageFilter, Info, Item, LineCap, LineJoin, LinearGradient, Link, LinkTarget, Page, Paint,
    Path, Segment, Stroke, TextMode, TextRun,
};
use sealwax_core::Limits;

pub use docxide_pdf::ConvertOptions as DocxOptions;

/// What a conversion can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The layout engine refused the document, or failed on it.
    #[error("the document could not be laid out")]
    Layout,
    /// The deadline passed, or a bound of [`Limits`] while composing.
    #[error(transparent)]
    Compose(#[from] sealwax_compose::Error),
}

/// A converted document and what the conversion left out.
#[derive(Debug)]
#[non_exhaustive]
pub struct Converted {
    /// The document, with what [`sealwax_compose::compose`] left out.
    pub composed: Composed,
    /// What the layout drew that its pages do not describe yet (docxide's `unsupported`).
    pub not_described: BTreeMap<&'static str, usize>,
}

/// A DOCX document as a PDF document.
///
/// # Errors
/// [`Error::Layout`] when docxide-pdf refuses the document or fails on it; [`Error::Compose`]
/// when the deadline passes or a bound of `limits` is passed.
pub fn docx_to_pdf(
    docx: &[u8],
    options: &DocxOptions,
    limits: &Limits,
) -> Result<Converted, Error> {
    limits.check_time().map_err(sealwax_compose::Error::from)?;
    let laid_out = layout_docx(docx, options)?;
    let composed = sealwax_compose::compose(&composition(&laid_out), limits)?;
    Ok(Converted {
        composed,
        not_described: laid_out.unsupported,
    })
}

/// A DOCX document laid out by docxide-pdf, its pages not yet composed.
///
/// # Errors
/// [`Error::Layout`] when docxide-pdf refuses the document, or panics on it.
pub fn layout_docx(docx: &[u8], options: &DocxOptions) -> Result<display::DisplayDocument, Error> {
    // docxide-pdf does not promise not to panic on a hostile document. What it records is kept
    // per thread and put back as a panic unwinds, so a layout that panicked leaves nothing.
    catch_unwind(AssertUnwindSafe(|| {
        docxide_pdf::layout_docx_bytes_with(docx, options)
    }))
    .map_err(|_| Error::Layout)?
    .map_err(|_| Error::Layout)
}

/// docxide's laid-out pages as a composition.
#[must_use]
pub fn composition(doc: &display::DisplayDocument) -> Composition {
    let mut files: HashMap<PathBuf, Option<Arc<Vec<u8>>>> = HashMap::new();
    let fonts = doc
        .fonts
        .iter()
        .map(|font| match &font.source {
            display::FontSource::File { path, index } => {
                let program = files
                    .entry(path.clone())
                    .or_insert_with(|| std::fs::read(path).ok().map(Arc::new))
                    .clone();
                match program {
                    Some(program) => Font::Program {
                        program,
                        face: *index,
                    },
                    // An unreadable file: no program, its text counted as skipped.
                    None => Font::Program {
                        program: Arc::new(Vec::new()),
                        face: 0,
                    },
                }
            }
            display::FontSource::Data { data, index } => Font::Program {
                program: Arc::clone(data),
                face: *index,
            },
            display::FontSource::Standard(name) => Font::Standard((*name).to_owned()),
        })
        .collect();
    Composition {
        pages: doc.pages.iter().map(page).collect(),
        fonts,
        images: doc.images.iter().map(image).collect(),
        info: Info {
            title: doc.info.title.clone(),
            author: doc.info.author.clone(),
            subject: doc.info.subject.clone(),
            keywords: doc.info.keywords.clone(),
            language: doc.info.language.clone(),
        },
    }
}

fn matrix(m: &display::Matrix) -> [f64; 6] {
    m.map(f64::from)
}

fn page(page: &display::Page) -> Page {
    Page {
        width: f64::from(page.width),
        height: f64::from(page.height),
        items: page.items.iter().map(item).collect(),
        links: page.links.iter().map(link).collect(),
    }
}

fn item(item: &display::Item) -> Item {
    match item {
        display::Item::Group(items) => Item::Group(items.iter().map(self::item).collect()),
        display::Item::Transform(m) => Item::Transform(matrix(m)),
        display::Item::Clip { path, fill_rule } => Item::Clip {
            path: self::path(path),
            fill_rule: rule(*fill_rule),
        },
        display::Item::Fill {
            path,
            fill_rule,
            paint,
        } => Item::Fill {
            path: self::path(path),
            fill_rule: rule(*fill_rule),
            paint: self::paint(*paint),
        },
        display::Item::Stroke { path, paint, style } => Item::Stroke {
            path: self::path(path),
            paint: self::paint(*paint),
            stroke: stroke(style),
        },
        display::Item::FillGradient {
            path,
            fill_rule,
            gradient,
        } => Item::FillGradient {
            path: self::path(path),
            fill_rule: rule(*fill_rule),
            gradient: LinearGradient {
                matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                start: gradient.start.map(f64::from),
                end: gradient.end.map(f64::from),
                stops: gradient
                    .stops
                    .iter()
                    .map(|s| GradientStop {
                        offset: f64::from(s.offset),
                        color: s.color.map(f64::from),
                    })
                    .collect(),
                extend: gradient.extend,
                linear_light: gradient.linear_light,
            },
        },
        display::Item::Text(run) => Item::Text(text(run)),
        display::Item::Image(index) => Item::Image(*index),
    }
}

fn path(path: &display::Path) -> Path {
    let f = f64::from;
    Path(
        path.0
            .iter()
            .map(|segment| match *segment {
                display::Segment::MoveTo(x, y) => Segment::MoveTo(f(x), f(y)),
                display::Segment::LineTo(x, y) => Segment::LineTo(f(x), f(y)),
                display::Segment::CubicTo(a, b, c, d, e, g) => {
                    Segment::CubicTo(f(a), f(b), f(c), f(d), f(e), f(g))
                }
                display::Segment::Rect(x, y, w, h) => Segment::Rect(f(x), f(y), f(w), f(h)),
                display::Segment::Close => Segment::Close,
            })
            .collect(),
    )
}

fn rule(rule: display::FillRule) -> FillRule {
    match rule {
        display::FillRule::NonZero => FillRule::NonZero,
        display::FillRule::EvenOdd => FillRule::EvenOdd,
    }
}

fn paint(paint: display::Paint) -> Paint {
    Paint {
        color: match paint.color {
            display::Color::Gray(g) => Color::Gray(f64::from(g)),
            display::Color::Rgb(r, g, b) => Color::Rgb(f64::from(r), f64::from(g), f64::from(b)),
        },
        alpha: f64::from(paint.alpha),
    }
}

fn stroke(style: &display::StrokeStyle) -> Stroke {
    Stroke {
        width: f64::from(style.width),
        cap: match style.cap {
            display::LineCap::Butt => LineCap::Butt,
            display::LineCap::Round => LineCap::Round,
            display::LineCap::Square => LineCap::Square,
        },
        join: match style.join {
            display::LineJoin::Miter => LineJoin::Miter,
            display::LineJoin::Round => LineJoin::Round,
            display::LineJoin::Bevel => LineJoin::Bevel,
        },
        miter_limit: f64::from(style.miter_limit),
        dash: style.dash.0.iter().map(|d| f64::from(*d)).collect(),
        dash_phase: f64::from(style.dash.1),
    }
}

fn text(run: &display::TextRun) -> TextRun {
    TextRun {
        font: run.font,
        size: f64::from(run.size),
        matrix: matrix(&run.matrix),
        horizontal_scaling: f64::from(run.horizontal_scaling),
        mode: match run.mode {
            display::TextMode::Fill => TextMode::Fill,
            display::TextMode::Stroke => TextMode::Stroke,
            display::TextMode::FillStroke => TextMode::FillStroke,
            display::TextMode::Invisible => TextMode::Invisible,
        },
        fill: paint(run.fill),
        stroke: run.stroke.map(|(p, width)| (paint(p), f64::from(width))),
        glyphs: run
            .glyphs
            .iter()
            .map(|g| Glyph {
                id: g.id,
                x: f64::from(g.x),
                advance: f64::from(g.advance),
                text: g.text.clone(),
            })
            .collect(),
    }
}

fn image(image: &display::Image) -> Image {
    Image {
        width: image.width,
        height: image.height,
        color_space: match &image.color_space {
            display::ImageColorSpace::Gray => ImageColorSpace::Gray,
            display::ImageColorSpace::Rgb => ImageColorSpace::Rgb,
            display::ImageColorSpace::Cmyk => ImageColorSpace::Cmyk,
            display::ImageColorSpace::IndexedRgb { palette } => {
                ImageColorSpace::IndexedRgb(palette.clone())
            }
        },
        bits_per_component: image.bits_per_component,
        filter: match image.filter {
            display::ImageFilter::Jpeg => ImageFilter::Jpeg,
            display::ImageFilter::Flate => ImageFilter::Flate,
            display::ImageFilter::None => ImageFilter::None,
        },
        data: image.data.clone(),
        soft_mask: image.soft_mask.as_deref().map(|m| Box::new(self::image(m))),
        interpolate: image.interpolate,
    }
}

fn link(link: &display::Link) -> Link {
    Link {
        rect: link.rect.map(f64::from),
        target: match &link.target {
            display::LinkTarget::Uri(uri) => LinkTarget::Uri(uri.clone()),
            display::LinkTarget::Page { page, y } => LinkTarget::Page {
                index: *page,
                top: f64::from(*y),
            },
        },
        description: link.text.clone(),
    }
}
