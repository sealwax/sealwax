//! What each unit of a page's content draws, in the page's default user space: the glyphs,
//! images and paths that tagging (PDF/A level a, N1) ties to the structure.
//!
//! The caller marks each unit of the content with a marked-content sequence of its own (a tag,
//! and an `/I` integer in an inline property list), and the content is interpreted with the
//! page's resources: whatever is drawn inside a sequence, through forms and Type 3 glyphs
//! included, belongs to its unit.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::font::{Glyph as HayroGlyph, GlyphRun};
use hayro_interpret::hayro_cmap::BfString;
use hayro_interpret::hayro_syntax::content::TypedIter;
use hayro_interpret::hayro_syntax::object::{Dict, String as HayroString};
use hayro_interpret::kurbo::{Affine, BezPath, Point, Rect, Shape as _};
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret,
};
use sealwax_core::Document;

/// The units per em of glyph space in hayro's glyph transforms (`hayro_font::UNITS_PER_EM`,
/// which hayro-interpret does not export).
const UNITS_PER_EM: f64 = 1000.0;

/// A box: left, bottom, right, top.
pub type Bounds = [f64; 4];

/// A glyph drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawnGlyph {
    /// Where its origin lands.
    pub origin: [f64; 2],
    /// The direction and length of one em upward, from the origin: the font size as drawn
    /// (and the direction of the line, at a right angle).
    pub up: [f64; 2],
    /// The box of its outline, if it has one (a space has none).
    pub ink: Option<Bounds>,
    /// What it means, when the font tells.
    pub text: Option<String>,
    /// Drawn in an invisible render mode (an OCR layer over a scan, usually).
    pub invisible: bool,
}

/// What one unit draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drawn {
    pub glyphs: Vec<DrawnGlyph>,
    /// The box of each image (the unit square it is drawn in, transformed).
    pub images: Vec<Bounds>,
    /// The box of each path filled or stroked, shadings included.
    pub paths: Vec<Bounds>,
    /// The `/Alt` of the innermost marked-content sequence around the unit that has one (a
    /// `/Span` describing a picture), as the string's bytes.
    pub alt: Option<Vec<u8>>,
}

/// Interpret `content` with the resources of page `page_index` of `doc`, and return what each
/// unit draws: the unit marked `tag` with `/I` *n* is entry *n* of the result, which has
/// `units` entries. `None` when the page is not found or its interpretation fails.
///
/// # Errors
///
/// When interpreting `content` would pass [`Document::limits`]
/// ([`Document::check_content`]): nothing is interpreted then.
pub fn drawn(
    doc: &Document,
    page_index: usize,
    content: &[u8],
    tag: &[u8],
    units: usize,
) -> sealwax_core::Result<Option<Vec<Drawn>>> {
    let pdf = doc.hayro();
    let pages = pdf.pages();
    if page_index >= pages.len() {
        return Ok(None);
    }
    doc.check_content(page_index, content)?;
    let Some(page) = pages.get(page_index) else {
        return Ok(None);
    };
    let cache = InterpreterCache::new();
    let mut collector = Collector {
        tag,
        open: Vec::new(),
        drawn: vec![Drawn::default(); units],
        outlines: HashMap::new(),
    };
    let everywhere = Rect::new(-1e9, -1e9, 1e9, 1e9);
    let interpreted = catch_unwind(AssertUnwindSafe(|| {
        let mut context = Context::new(
            Affine::IDENTITY,
            everywhere,
            &cache,
            pdf.xref(),
            InterpreterSettings::default(),
        );
        interpret(
            TypedIter::new(content),
            page.resources(),
            &mut context,
            &mut collector,
        );
    }));
    Ok(interpreted.ok().map(|()| collector.drawn))
}

struct Collector<'t> {
    tag: &'t [u8],
    /// The unit of each open marked-content sequence, if it marks one, and its `/Alt`.
    open: Vec<(Option<usize>, Option<Vec<u8>>)>,
    drawn: Vec<Drawn>,
    /// The box of each outline, in glyph space, by glyph.
    outlines: HashMap<u128, Option<Rect>>,
}

impl Collector<'_> {
    fn current(&mut self) -> Option<&mut Drawn> {
        let unit = self.open.iter().rev().find_map(|(unit, _)| *unit)?;
        self.drawn.get_mut(unit)
    }
}

fn bounds(rect: Rect) -> Option<Bounds> {
    let found = [rect.x0, rect.y0, rect.x1, rect.y1];
    found.iter().all(|v| v.is_finite()).then_some(found)
}

fn text(unicode: BfString) -> String {
    match unicode {
        BfString::Char(c) => c.to_string(),
        BfString::String(s) => s,
    }
}

impl<'a> Device<'a> for Collector<'_> {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, _: &DrawMode) {
        let rect = (props.transform * path.clone()).bounding_box();
        if let (Some(found), Some(drawn)) = (bounds(rect), self.current()) {
            drawn.paths.push(found);
        }
    }

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        let invisible = matches!(mode, DrawMode::Invisible);
        let mut glyphs = Vec::with_capacity(run.glyphs().len());
        for glyph in run.glyphs() {
            let transform = props.transform * glyph.transform();
            let origin = transform * Point::ZERO;
            let up = transform * Point::new(0.0, UNITS_PER_EM) - origin;
            let ink = match &**glyph {
                HayroGlyph::Outline(outline) => {
                    let outline_box = *self
                        .outlines
                        .entry(outline.identifier().cache_key())
                        .or_insert_with(|| {
                            let path = outline.outline();
                            (!path.elements().is_empty()).then(|| path.bounding_box())
                        });
                    outline_box.and_then(|rect| {
                        let corners = [
                            Point::new(rect.x0, rect.y0),
                            Point::new(rect.x1, rect.y0),
                            Point::new(rect.x0, rect.y1),
                            Point::new(rect.x1, rect.y1),
                        ]
                        .map(|corner| transform * corner);
                        let (mut low, mut high) = (corners[0], corners[0]);
                        for corner in &corners[1..] {
                            low = Point::new(low.x.min(corner.x), low.y.min(corner.y));
                            high = Point::new(high.x.max(corner.x), high.y.max(corner.y));
                        }
                        bounds(Rect::from_points(low, high))
                    })
                }
                HayroGlyph::Type3(_) => None,
            };
            if !(origin.x.is_finite()
                && origin.y.is_finite()
                && up.x.is_finite()
                && up.y.is_finite())
            {
                continue;
            }
            glyphs.push(DrawnGlyph {
                origin: [origin.x, origin.y],
                up: [up.x, up.y],
                ink,
                text: glyph.as_unicode().map(text),
                invisible,
            });
        }
        if let Some(drawn) = self.current() {
            drawn.glyphs.extend(glyphs);
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, props: ImageDrawProps<'a>) {
        let rect = (props.transform * Rect::new(0.0, 0.0, 1.0, 1.0).to_path(0.1)).bounding_box();
        if let (Some(found), Some(drawn)) = (bounds(rect), self.current()) {
            drawn.images.push(found);
        }
    }

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn begin_marked_content(&mut self, tag: &[u8], properties: Option<&Dict<'_>>) {
        let unit = (tag == self.tag)
            .then(|| properties?.get::<i64>(b"I"))
            .flatten()
            .and_then(|unit| usize::try_from(unit).ok())
            .filter(|unit| *unit < self.drawn.len());
        let alt = properties
            .and_then(|properties| properties.get::<HayroString<'_>>(b"Alt"))
            .map(|alt| alt.as_bytes().to_vec());
        if let Some(unit) = unit
            && let Some(alt) = self.open.iter().rev().find_map(|(_, alt)| alt.clone())
        {
            self.drawn[unit].alt = Some(alt);
        }
        self.open.push((unit, alt));
    }

    fn end_marked_content(&mut self) {
        self.open.pop();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use sealwax_core::{Document, OpenOptions};

    use super::drawn;

    /// A one-page document with Helvetica as /F1 and an image as /Im1.
    fn document() -> Document {
        let content = b"q";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                String::from_utf8_lossy(content)
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Length 1 >>\nstream\nA\nendstream"
                .to_owned(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        Document::open(out, &OpenOptions::default()).unwrap()
    }

    #[test]
    fn each_unit_gets_what_it_draws() {
        let doc = document();
        let content = b"/U <</I 0>> BDC BT /F1 10 Tf 20 100 Td (Ab) Tj ET EMC \
            /U <</I 1>> BDC q 50 0 0 40 10 10 cm /Im1 Do Q EMC \
            /Span <</Alt (A picture)>> BDC /U <</I 2>> BDC 0 0 m 100 0 l S EMC EMC \
            BT /F1 10 Tf (unmarked) Tj ET";
        let drawn = drawn(&doc, 0, content, b"U", 3).unwrap().unwrap();
        assert_eq!(drawn[2].alt.as_deref(), Some(&b"A picture"[..]));
        assert_eq!(drawn[1].alt, None);
        let glyphs = &drawn[0].glyphs;
        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs[0].text.as_deref(), Some("A"));
        assert_eq!(glyphs[0].origin, [20.0, 100.0]);
        assert!((glyphs[0].up[1] - 10.0).abs() < 1e-9 && glyphs[0].up[0].abs() < 1e-9);
        let ink = glyphs[0].ink.unwrap();
        assert!(
            ink[0] >= 19.9 && ink[2] < 27.0 && ink[1] >= 99.9 && ink[3] < 108.0,
            "{ink:?}"
        );
        // Helvetica's A is 6.67 wide at 10 points.
        assert!((glyphs[1].origin[0] - 26.67).abs() < 0.01);
        assert_eq!(drawn[1].images, [[10.0, 10.0, 60.0, 50.0]]);
        assert_eq!(drawn[2].paths.len(), 1);
        assert!(drawn[1].glyphs.is_empty() && drawn[2].glyphs.is_empty());
    }
}
