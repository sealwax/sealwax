//! The straight lines and rectangles a page paints, in the page's default user space: what tables,
//! rules and check boxes are drawn with.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::font::GlyphRun;
use hayro_interpret::hayro_syntax::object::Dict;
use hayro_interpret::kurbo::{Affine, BezPath, PathEl, Point, Rect};
use hayro_interpret::pattern::Pattern;
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterSettings, Paint, RenderContent, SoftMask, interpret, interpret_page,
};
use sealwax_core::Document;

use crate::cache::with_cache;
use crate::drawn::Bounds;

/// How far from the axes, in user space units, a turned rectangle's sides may lean and still
/// count as along them.
const ALONG_AXES: f64 = 1e-6;

/// How close, in the path's own units, two coordinates must be to count as one: what hayro
/// allows when it finds a rectangle in a path.
const SAME: f64 = 1.0 / 4096.0;

/// What a page paints with straight lines (filled or stroked; clipping paths and text are not
/// painting, nor is a shading, whether `sh` paints it or a path is filled or stroked with it: it
/// paints a gradient's extent, and MuPDF's drawings leave it out too), in its default user space
/// (unrotated, y up).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Drawings {
    /// Each straight segment of a path (`l`, and the side that closes a subpath with `h`, `s`,
    /// `b` or `b*` when it ends away from its start), from where it starts to where it ends:
    /// `[x0, y0, x1, y1]`. A rectangle's sides are not repeated here. A subpath filled without
    /// being closed gets no closing side, as in MuPDF.
    pub lines: Vec<[f64; 4]>,
    /// Each rectangle whose sides stay along the axes on the page, as a box: a `re`, or a
    /// subpath of four corners, closed (by `h` or by a last side back to its start), whose sides
    /// run in turn along one axis and the other.
    pub rectangles: Vec<Bounds>,
    /// Each such rectangle that the transformation turns off the axes, as its four corners in
    /// order.
    pub quads: Vec<[[f64; 2]; 4]>,
}

/// The lines and rectangles page `page_index` (from 0) paints, in the order of its content, forms
/// included; then, with `annotations`, those its annotations' appearances paint (shown ones, in
/// the order of `/Annots`: field borders and check boxes among them), as MuPDF's drawings include
/// them. A shape both filled and stroked counts once.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have; what
/// [`Document::check_page`] refuses (the limits, the deadline);
/// [`sealwax_core::Error::Timeout`] when the deadline passes while the page is read.
pub fn drawings(
    doc: &Document,
    page_index: usize,
    annotations: bool,
) -> sealwax_core::Result<Drawings> {
    if page_index >= doc.hayro().pages().len() {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
    }
    doc.check_page(page_index)?;
    let found = with_cache(doc, |pdf, cache| {
        let pages = pdf.pages();
        let Some(page) = pages.get(page_index) else {
            return Drawings::default();
        };
        let mut collector = Collector::default();
        let everywhere = Rect::new(-1e9, -1e9, 1e9, 1e9);
        // What was read before the interpreter failed, if it did, is kept.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut context = Context::new(
                Affine::IDENTITY,
                everywhere,
                cache,
                pdf.xref(),
                InterpreterSettings::default(),
            );
            interpret(
                page.typed_operations(),
                page.resources(),
                &mut context,
                &mut collector,
            );
            if annotations {
                collector.last_filled = None;
                let mut context = Context::new(
                    Affine::IDENTITY,
                    everywhere,
                    cache,
                    pdf.xref(),
                    InterpreterSettings {
                        render_content: RenderContent::ANNOTATIONS,
                        ..InterpreterSettings::default()
                    },
                );
                interpret_page(page, &mut context, &mut collector);
            }
        }));
        collector.found
    });
    doc.check_time()?;
    Ok(found)
}

#[derive(Default)]
struct Collector {
    found: Drawings,
    /// The last shape filled, placed on the page, to count it once when it is stroked next.
    last_filled: Option<Vec<PathEl>>,
}

impl Collector {
    /// Whether `path` drawn with `mode` is the shape just filled, now stroked, where it lands on
    /// the page (it may be written anew under another matrix); remembers a shape filled.
    fn repeated(&mut self, elements: &[PathEl], transform: Affine, mode: &DrawMode) -> bool {
        let placed: Vec<PathEl> = elements.iter().map(|e| transform * *e).collect();
        let same = self
            .last_filled
            .take()
            .is_some_and(|last| same_elements(&last, &placed));
        match mode {
            DrawMode::Stroke(_) => same,
            _ => {
                self.last_filled = Some(placed);
                false
            }
        }
    }

    fn rectangle(&mut self, rect: &Rect, transform: Affine) {
        let corners = [
            transform * Point::new(rect.x0, rect.y0),
            transform * Point::new(rect.x1, rect.y0),
            transform * Point::new(rect.x1, rect.y1),
            transform * Point::new(rect.x0, rect.y1),
        ];
        if corners
            .iter()
            .any(|p| !(p.x.is_finite() && p.y.is_finite()))
        {
            return;
        }
        let along =
            |a: Point, b: Point| (a.x - b.x).abs() <= ALONG_AXES || (a.y - b.y).abs() <= ALONG_AXES;
        if along(corners[0], corners[1]) && along(corners[1], corners[2]) {
            let xs = corners.map(|p| p.x);
            let ys = corners.map(|p| p.y);
            let min = |v: [f64; 4]| v.into_iter().fold(f64::INFINITY, f64::min);
            let max = |v: [f64; 4]| v.into_iter().fold(f64::NEG_INFINITY, f64::max);
            self.found
                .rectangles
                .push([min(xs), min(ys), max(xs), max(ys)]);
        } else {
            self.found.quads.push(corners.map(|p| [p.x, p.y]));
        }
    }

    /// A straight segment, in the page's user space.
    fn line(&mut self, from: Point, to: Point, transform: Affine) {
        let (a, b) = (transform * from, transform * to);
        if [a.x, a.y, b.x, b.y].iter().all(|v| v.is_finite()) {
            self.found.lines.push([a.x, a.y, b.x, b.y]);
        }
    }

    /// One subpath: a move, what follows it, and its closing, if any.
    fn subpath(&mut self, elements: &[PathEl], transform: Affine) {
        let (start, body) = match elements.split_first() {
            Some((PathEl::MoveTo(start), body)) => (Some(*start), body),
            _ => (None, elements),
        };
        let (body, closed) = match body.split_last() {
            Some((PathEl::ClosePath, body)) => (body, true),
            _ => (body, false),
        };
        if let Some(start) = start
            && let Some(rect) = as_rectangle(start, body, closed)
        {
            self.rectangle(&rect, transform);
            return;
        }
        let mut current = start;
        for element in body {
            match *element {
                PathEl::LineTo(p) => {
                    if let Some(from) = current {
                        self.line(from, p, transform);
                    }
                    current = Some(p);
                }
                PathEl::MoveTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => {
                    current = Some(p);
                }
                PathEl::ClosePath => {}
            }
        }
        if closed
            && let (Some(from), Some(to)) = (current, start)
            && !same(from, to)
        {
            self.line(from, to, transform);
        }
    }
}

/// Whether two points of a path are one.
fn same(a: Point, b: Point) -> bool {
    (a.x - b.x).abs() <= SAME && (a.y - b.y).abs() <= SAME
}

/// Whether two paths placed on the page are one, as MuPDF compares them: in single precision,
/// which absorbs what rounding makes of their coordinates (a rectangle filled with `re` and
/// stroked with `m l l l h`, its corners computed and written, or under another matrix).
fn same_elements(a: &[PathEl], b: &[PathEl]) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    let single = |p: Point| (p.x as f32, p.y as f32);
    let one = |p: Point, q: Point| single(p) == single(q);
    a.len() == b.len()
        && a.iter().zip(b).all(|pair| match pair {
            (PathEl::MoveTo(p), PathEl::MoveTo(q)) | (PathEl::LineTo(p), PathEl::LineTo(q)) => {
                one(*p, *q)
            }
            (PathEl::QuadTo(p1, p2), PathEl::QuadTo(q1, q2)) => one(*p1, *q1) && one(*p2, *q2),
            (PathEl::CurveTo(p1, p2, p3), PathEl::CurveTo(q1, q2, q3)) => {
                one(*p1, *q1) && one(*p2, *q2) && one(*p3, *q3)
            }
            (PathEl::ClosePath, PathEl::ClosePath) => true,
            _ => false,
        })
}

/// The rectangle a subpath from `start` draws, if it draws one: four corners joined by straight
/// sides, closed by `h` or by a last side back to the start, the sides running in turn along one
/// axis and the other (as hayro finds one in a path of one subpath).
fn as_rectangle(start: Point, body: &[PathEl], closed: bool) -> Option<Rect> {
    let corners = match *body {
        [PathEl::LineTo(a), PathEl::LineTo(b), PathEl::LineTo(c)] if closed => [start, a, b, c],
        [
            PathEl::LineTo(a),
            PathEl::LineTo(b),
            PathEl::LineTo(c),
            PathEl::LineTo(d),
        ] if same(d, start) => [start, a, b, c],
        _ => return None,
    };
    let mut previous = None;
    for (index, from) in corners.iter().enumerate() {
        let to = corners[(index + 1) % 4];
        let vertical = (from.x - to.x).abs() <= SAME;
        let horizontal = (from.y - to.y).abs() <= SAME;
        if vertical == horizontal || previous == Some(vertical) {
            return None;
        }
        previous = Some(vertical);
    }
    Some(Rect::from_points(corners[0], corners[2]))
}

/// Whether nothing drawn with `props` and `mode` counts: invisible, or painted with a shading.
fn unseen(props: &DrawProps<'_>, mode: &DrawMode) -> bool {
    matches!(mode, DrawMode::Invisible)
        || matches!(&props.paint, Paint::Pattern(pattern) if matches!(**pattern, Pattern::Shading(_)))
}

impl<'a> Device<'a> for Collector {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        if unseen(&props, mode) || self.repeated(path.elements(), props.transform, mode) {
            return;
        }
        let transform = props.transform;
        let mut subpath: Vec<PathEl> = Vec::new();
        for element in path.elements() {
            match *element {
                PathEl::MoveTo(_) => {
                    self.subpath(&subpath, transform);
                    subpath.clear();
                    subpath.push(*element);
                }
                PathEl::ClosePath => {
                    subpath.push(*element);
                    self.subpath(&subpath, transform);
                    // What follows without a move starts where the closed subpath started.
                    let start = match subpath.first() {
                        Some(PathEl::MoveTo(start)) => Some(*start),
                        _ => None,
                    };
                    subpath.clear();
                    subpath.extend(start.map(PathEl::MoveTo));
                }
                _ => subpath.push(*element),
            }
        }
        self.subpath(&subpath, transform);
    }

    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, mode: &DrawMode) {
        let elements = [
            PathEl::MoveTo(Point::new(rect.x0, rect.y0)),
            PathEl::LineTo(Point::new(rect.x1, rect.y1)),
        ];
        if unseen(&props, mode) || self.repeated(&elements, props.transform, mode) {
            return;
        }
        self.rectangle(rect, props.transform);
    }

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn begin_marked_content(&mut self, _: &[u8], _: Option<&Dict<'_>>) {}

    fn end_marked_content(&mut self) {}
}
