//! docxide's laid-out pages translated into a [`Composition`].

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use sealwax_compose::{
    ArtifactKind, Color, Composition, Element, FillRule, Font, Glyph, GradientStop, HeaderScope,
    Image, ImageColorSpace, ImageFilter, Info, Item, Kid, LineCap, LineJoin, LinearGradient, Link,
    LinkTarget, ListNumbering, Mark, OutlineEntry, Page, Paint, Path, Segment, Stroke, Structure,
    TableCell, TextMode, TextRun,
};

use crate::display;

/// docxide's laid-out pages as a composition, with what the pages hold that it lacks.
#[derive(Debug)]
#[non_exhaustive]
pub struct Translated {
    pub composition: Composition,
    /// What the layout drew that its pages do not describe (docxide's `unsupported`), and
    /// what they describe that this crate does not translate: a kind newer than it, counted as
    /// `unknown ...` and left out (a mark kept as an artifact, so that sequences stay balanced).
    pub not_described: BTreeMap<&'static str, usize>,
}

/// docxide's laid-out pages as a composition. Font files the layout names on disk are read
/// here.
#[must_use]
pub fn composition(doc: &display::Document) -> Translated {
    let mut translation = Translation::default();
    for (&kind, &count) in &doc.unsupported {
        translation.count(unsupported(kind), count);
    }
    let fonts = doc
        .fonts
        .iter()
        .map(|font| translation.font(font))
        .collect();
    let composition = Composition {
        pages: doc
            .pages
            .iter()
            .map(|page| translation.page(page))
            .collect(),
        fonts,
        images: doc
            .images
            .iter()
            .map(|image| translation.image(image))
            .collect(),
        info: Info {
            title: doc.info.title.clone(),
            author: doc.info.author.clone(),
            subject: doc.info.subject.clone(),
            keywords: doc.info.keywords.clone(),
            language: doc.info.language.clone(),
            pdf_ua: doc.info.pdf_ua,
        },
        structure: doc
            .structure
            .as_ref()
            .map(|structure| translation.structure(structure)),
        outline: doc
            .outline
            .iter()
            .map(|entry| OutlineEntry {
                level: u32::from(entry.level),
                title: entry.title.clone(),
                page: entry.page,
                top: Some(f64::from(entry.top)),
            })
            .collect(),
    };
    Translated {
        composition,
        not_described: translation.not_described,
    }
}

/// How [`crate::Converted::not_described`] names a kind of docxide's `unsupported`.
fn unsupported(kind: display::Unsupported) -> &'static str {
    use display::Unsupported as U;
    match kind {
        U::Pattern => "pattern not described",
        U::GradientTextOrStroke => "gradient text or stroke",
        U::UnnamedXObject => "xobject without a name",
        U::UnrecordedXObject => "xobject not recorded",
        U::UnrecordedSoftMask => "soft mask not recorded",
        U::FormTooDeep => "form nested too deep",
        U::FontWithoutSource => "font without a source",
        U::UnknownFont => "text in an unknown font",
        U::MarkedContent => "marked content not described",
        _ => "unknown drawing not described",
    }
}

/// The state of one translation: the font files read, and what is not described.
#[derive(Default)]
struct Translation {
    files: HashMap<PathBuf, Option<Arc<Vec<u8>>>>,
    not_described: BTreeMap<&'static str, usize>,
}

impl Translation {
    fn count(&mut self, what: &'static str, count: usize) {
        *self.not_described.entry(what).or_default() += count;
    }

    fn font(&mut self, font: &display::Font) -> Font {
        match &font.source {
            display::FontSource::File { path, index } => {
                let program = self
                    .files
                    .entry(path.clone())
                    .or_insert_with(|| std::fs::read(path).ok().map(Arc::new))
                    .clone();
                match program {
                    Some(program) => Font::Program {
                        program,
                        face: *index,
                    },
                    // An unreadable file: no program, its text counted as skipped.
                    None => no_program(),
                }
            }
            display::FontSource::Data { data, index } => Font::Program {
                program: Arc::clone(data),
                face: *index,
            },
            display::FontSource::Standard(name) => Font::Standard((*name).to_owned()),
            _ => {
                self.count("unknown font source", 1);
                no_program()
            }
        }
    }

    fn structure(&mut self, structure: &display::Structure) -> Structure {
        Structure {
            elements: structure
                .elements
                .iter()
                .map(|element| self.element(element))
                .collect(),
        }
    }

    fn element(&mut self, element: &display::Element) -> Element {
        Element {
            kind: element.kind.name().to_owned(),
            kids: element
                .kids
                .iter()
                .filter_map(|kid| match *kid {
                    display::Kid::Element(index) => Some(Kid::Element(index)),
                    display::Kid::Content { page, mcid } => Some(Kid::Content { page, mcid }),
                    display::Kid::Link { page, link } => Some(Kid::Link { page, link }),
                    _ => {
                        self.count("unknown structure kid", 1);
                        None
                    }
                })
                .collect(),
            alt: element.alt.clone(),
            actual_text: element.actual_text.clone(),
            lang: element.lang.clone(),
            id: element.id.clone(),
            table_cell: element.table_cell.map(|cell| TableCell {
                header_scope: cell.header_scope.map(|scope| match scope {
                    display::HeaderScope::Row => HeaderScope::Row,
                    display::HeaderScope::Column => HeaderScope::Column,
                    display::HeaderScope::Both => HeaderScope::Both,
                }),
                col_span: cell.col_span,
            }),
            list_numbering: element.list_numbering.map(list_numbering),
        }
    }

    fn page(&mut self, page: &display::Page) -> Page {
        Page {
            width: f64::from(page.width),
            height: f64::from(page.height),
            items: self.items(&page.items),
            links: page.links.iter().map(|link| self.link(link)).collect(),
        }
    }

    fn items(&mut self, items: &[display::Item]) -> Vec<Item> {
        items.iter().filter_map(|item| self.item(item)).collect()
    }

    fn item(&mut self, item: &display::Item) -> Option<Item> {
        Some(match item {
            display::Item::Group(items) => Item::Group(self.items(items)),
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
                paint: self.paint(*paint),
            },
            display::Item::Stroke { path, paint, style } => Item::Stroke {
                path: self::path(path),
                paint: self.paint(*paint),
                stroke: stroke(style),
            },
            display::Item::FillGradient {
                path,
                fill_rule,
                gradient,
            } => Item::FillGradient {
                path: self::path(path),
                fill_rule: rule(*fill_rule),
                gradient: self::gradient(gradient),
            },
            display::Item::Text(run) => Item::Text(self.text(run)),
            display::Item::Image(index) => Item::Image(*index),
            display::Item::BeginMarked(mark) => Item::BeginMarked(self.mark(*mark)),
            display::Item::EndMarked => Item::EndMarked,
            _ => {
                self.count("unknown item", 1);
                return None;
            }
        })
    }

    fn mark(&mut self, mark: display::Mark) -> Mark {
        match mark {
            display::Mark::Content { kind, mcid } => Mark::Element {
                tag: kind.name().to_owned(),
                mcid,
            },
            display::Mark::Artifact(kind) => Mark::Artifact(kind.and_then(|kind| match kind {
                display::ArtifactKind::Pagination => Some(ArtifactKind::Pagination),
                _ => {
                    self.count("unknown artifact kind", 1);
                    None
                }
            })),
            _ => {
                self.count("unknown mark", 1);
                Mark::Artifact(None)
            }
        }
    }

    fn paint(&mut self, paint: display::Paint) -> Paint {
        Paint {
            color: match paint.color {
                display::Color::Gray(g) => Color::Gray(f64::from(g)),
                display::Color::Rgb(r, g, b) => {
                    Color::Rgb(f64::from(r), f64::from(g), f64::from(b))
                }
                _ => {
                    self.count("unknown colour", 1);
                    Color::Gray(0.0)
                }
            },
            alpha: f64::from(paint.alpha),
        }
    }

    fn text(&mut self, run: &display::TextRun) -> TextRun {
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
                _ => {
                    self.count("unknown text mode", 1);
                    TextMode::Fill
                }
            },
            fill: self.paint(run.fill),
            stroke: Some((self.paint(run.stroke.paint), f64::from(run.stroke.width))),
            glyphs: run
                .glyphs
                .iter()
                .map(|glyph| Glyph {
                    id: self.glyph_id(glyph.id),
                    x: f64::from(glyph.x),
                    advance: f64::from(glyph.advance),
                    text: glyph.text.clone(),
                })
                .collect(),
        }
    }

    /// The glyph's id as a composition gives it: docxide gives WinAnsi codes for the fonts it
    /// writes as simple fonts, the standard 14 (a composition draws those by code), and glyph
    /// ids for the others.
    fn glyph_id(&mut self, id: display::GlyphId) -> u16 {
        match id {
            display::GlyphId::Gid(gid) => gid,
            display::GlyphId::WinAnsi(code) => u16::from(code),
            _ => {
                // `.notdef`, which composing leaves out.
                self.count("unknown glyph id", 1);
                0
            }
        }
    }

    fn image(&mut self, image: &display::Image) -> Image {
        let color_space = match &image.color_space {
            display::ImageColorSpace::Gray => ImageColorSpace::Gray,
            display::ImageColorSpace::Rgb => ImageColorSpace::Rgb,
            display::ImageColorSpace::Cmyk => ImageColorSpace::Cmyk,
            display::ImageColorSpace::IndexedRgb { palette } => {
                ImageColorSpace::IndexedRgb(palette.clone())
            }
            _ => {
                self.count("unknown image colour space", 1);
                return invisible_image();
            }
        };
        let filter = match image.filter {
            display::ImageFilter::Jpeg => ImageFilter::Jpeg,
            display::ImageFilter::Flate => ImageFilter::Flate,
            display::ImageFilter::None => ImageFilter::None,
            _ => {
                self.count("unknown image filter", 1);
                return invisible_image();
            }
        };
        Image {
            width: image.width,
            height: image.height,
            color_space,
            bits_per_component: image.bits_per_component,
            filter,
            data: image.data.clone(),
            soft_mask: image
                .soft_mask
                .as_deref()
                .map(|mask| Box::new(self.image(mask))),
            interpolate: image.interpolate,
        }
    }

    fn link(&mut self, link: &display::Link) -> Link {
        let target = match &link.target {
            display::LinkTarget::Uri(uri) => LinkTarget::Uri(uri.clone()),
            display::LinkTarget::Page { page, y } => LinkTarget::Page {
                index: *page,
                top: f64::from(*y),
            },
            _ => {
                // A page out of range: composing leaves the link out.
                self.count("unknown link target", 1);
                LinkTarget::Page {
                    index: usize::MAX,
                    top: 0.0,
                }
            }
        };
        Link {
            rect: link.rect.map(f64::from),
            target,
            description: link.text.clone(),
        }
    }
}

/// A font without a program: composing leaves out its text and counts it.
fn no_program() -> Font {
    Font::Program {
        program: Arc::new(Vec::new()),
        face: 0,
    }
}

/// A picture of one transparent pixel, in place of one that cannot be translated: the
/// pictures stay where [`Item::Image`] finds them.
fn invisible_image() -> Image {
    let pixel = |soft_mask| Image {
        width: 1,
        height: 1,
        color_space: ImageColorSpace::Gray,
        bits_per_component: 8,
        filter: ImageFilter::None,
        data: vec![0],
        soft_mask,
        interpolate: false,
    };
    pixel(Some(Box::new(pixel(None))))
}

fn list_numbering(numbering: display::ListNumbering) -> ListNumbering {
    use display::ListNumbering as N;
    match numbering {
        N::Unordered => ListNumbering::Unordered,
        N::Ordered => ListNumbering::Ordered,
        N::Description => ListNumbering::Description,
        N::Disc => ListNumbering::Disc,
        N::Circle => ListNumbering::Circle,
        N::Square => ListNumbering::Square,
        N::Decimal => ListNumbering::Decimal,
        N::LowerRoman => ListNumbering::LowerRoman,
        N::UpperRoman => ListNumbering::UpperRoman,
        N::LowerAlpha => ListNumbering::LowerAlpha,
        N::UpperAlpha => ListNumbering::UpperAlpha,
    }
}

fn matrix(m: &display::Matrix) -> [f64; 6] {
    m.map(f64::from)
}

fn gradient(gradient: &display::LinearGradient) -> LinearGradient {
    LinearGradient {
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
        dash: style.dash.iter().map(|d| f64::from(*d)).collect(),
        dash_phase: f64::from(style.dash_phase),
    }
}
