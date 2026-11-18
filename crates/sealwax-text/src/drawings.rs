//! The straight lines and rectangles a page paints, in the page's default user space: what tables,
//! rules and check boxes are drawn with.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::font::GlyphRun;
use hayro_interpret::hayro_syntax::content::TypedIter;
use hayro_interpret::hayro_syntax::object::{Array, Dict};
use hayro_interpret::kurbo::{Affine, BezPath, PathEl, Point, Rect};
use hayro_interpret::pattern::Pattern;
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterSettings, Paint, RenderContent, SoftMask, interpret, interpret_page,
};
use sealwax_core::Document;

use crate::cache::with_cache;
use crate::drawn::Bounds;
use crate::scan::{Op, operators};

/// How far from the axes, in user space units, a turned rectangle's sides may lean and still
/// count as along them.
const ALONG_AXES: f64 = 1e-6;

/// How close, in the path's own units, two coordinates must be to count as one: what hayro
/// allows when it finds a rectangle in a path.
const SAME: f64 = 1.0 / 4096.0;

/// How far along x hayro moves the end of a line that goes nowhere, so that a round cap still
/// shows.
const NUDGE: f64 = 0.0001;

/// The tag of the sequences around a path that is one box, filled, its fourth side drawn back to
/// its start without `h`: hayro hands such a path over as a rectangle, and MuPDF gives it as four
/// lines. The sequence carries its corners (`/P`).
const OPEN_BOX: &[u8] = b"SwOpenBox";

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
    /// being closed gets no closing side, as in MuPDF. A curve whose control points lie on its
    /// ends (the first on the start and the second on the end, or both on one end) is a line
    /// here, as MuPDF draws it. A line or such a curve that goes nowhere (ends, as written, where
    /// it starts) is left out, as in MuPDF, unless it follows a move: there it is a line of no
    /// length.
    pub lines: Vec<[f64; 4]>,
    /// Each rectangle whose sides stay along the axes on the page, as a box: a `re`, or a
    /// subpath of four corners closed by `h`, whose sides run in turn along one axis and the
    /// other. A subpath whose fourth side comes back to its start is such a rectangle when it is
    /// only stroked; filled, it is four lines, as in MuPDF.
    pub rectangles: Vec<Bounds>,
    /// Each such rectangle that the transformation turns off the axes, as its four corners in
    /// order.
    pub quads: Vec<[[f64; 2]; 4]>,
    /// How each of `lines` is painted, in their order.
    pub lines_painted: Vec<Painted>,
    /// How each of `rectangles` is painted, in their order.
    pub rectangles_painted: Vec<Painted>,
    /// How each of `quads` is painted, in their order.
    pub quads_painted: Vec<Painted>,
    /// The subpath each of `lines` belongs to, in their order: lines of one subpath (from a move
    /// to the next move or closing, painted at once) share a number, no other line has it. The
    /// numbers grow in the order of the content, from 0, with gaps. A rectangle or quad is a
    /// subpath of its own.
    pub lines_subpath: Vec<usize>,
}

/// How a shape is painted: filled (`f`, `F`, `f*`), stroked (`S`, `s`), or both (`B`, `B*`, `b`,
/// `b*`, or a fill then a stroke of the path it filled, landing where it did). A box filled with
/// its fourth side drawn back to its start, which MuPDF gives as four lines, gives four lines
/// filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Painted {
    /// Its inside filled.
    Filled,
    /// Its outline stroked.
    Stroked,
    /// Both.
    FilledAndStroked,
}

impl Painted {
    /// Whether its inside is filled.
    #[must_use]
    pub fn filled(self) -> bool {
        self != Self::Stroked
    }

    /// Whether its outline is stroked.
    #[must_use]
    pub fn stroked(self) -> bool {
        self != Self::Filled
    }

    fn of(mode: &DrawMode) -> Self {
        match mode {
            DrawMode::Stroke(_) => Self::Stroked,
            DrawMode::FillAndStroke(..) => Self::FilledAndStroked,
            DrawMode::Fill(_) | DrawMode::Invisible => Self::Filled,
        }
    }
}

/// The lines and rectangles page `page_index` (from 0) paints, in the order of its content, forms
/// included; then, with `annotations`, those its annotations' appearances paint (shown ones, in
/// the order of `/Annots`: field borders and check boxes among them), as MuPDF's drawings include
/// them. A shape both filled and stroked counts once, as [`Painted::FilledAndStroked`].
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
            let content = page.page_stream().unwrap_or_default();
            let instrumented = instrument(content);
            interpret(
                TypedIter::new(instrumented.as_deref().unwrap_or(content)),
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
    /// The last shape filled, placed on the page, to count it once when it is stroked next; and
    /// how many lines, rectangles and quads were found before it, to mark what it gave stroked
    /// too.
    last_filled: Option<(Vec<PathEl>, [usize; 3])>,
    /// The marked-content sequences open: the corners of a box filled without `h` for ours.
    boxes: Vec<Option<[Point; 4]>>,
    /// How many subpaths were met: the number of the next.
    subpaths: usize,
}

impl Collector {
    /// Whether `path` drawn with `mode` is the shape just filled, now stroked, where it lands on
    /// the page (it may be written anew under another matrix): what the fill gave is then marked
    /// stroked too. Remembers a shape filled.
    fn repeated(&mut self, elements: &[PathEl], transform: Affine, mode: &DrawMode) -> bool {
        let placed: Vec<PathEl> = elements.iter().map(|e| transform * *e).collect();
        let last = self.last_filled.take();
        match mode {
            DrawMode::Stroke(_) => {
                let Some((last, [lines, rectangles, quads])) = last else {
                    return false;
                };
                if !same_elements(&last, &placed) {
                    return false;
                }
                let found = &mut self.found;
                for painted in found.lines_painted[lines..]
                    .iter_mut()
                    .chain(&mut found.rectangles_painted[rectangles..])
                    .chain(&mut found.quads_painted[quads..])
                {
                    *painted = Painted::FilledAndStroked;
                }
                true
            }
            _ => {
                let found = &self.found;
                self.last_filled = Some((
                    placed,
                    [found.lines.len(), found.rectangles.len(), found.quads.len()],
                ));
                false
            }
        }
    }

    fn rectangle(&mut self, rect: &Rect, transform: Affine, painted: Painted) {
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
            self.found.rectangles_painted.push(painted);
        } else {
            self.found.quads.push(corners.map(|p| [p.x, p.y]));
            self.found.quads_painted.push(painted);
        }
    }

    /// The number of a subpath met.
    fn next_subpath(&mut self) -> usize {
        let number = self.subpaths;
        self.subpaths += 1;
        number
    }

    /// A straight segment of subpath `subpath`, in the page's user space.
    fn line(
        &mut self,
        from: Point,
        to: Point,
        transform: Affine,
        (painted, subpath): (Painted, usize),
    ) {
        let (a, b) = (transform * from, transform * to);
        if [a.x, a.y, b.x, b.y].iter().all(|v| v.is_finite()) {
            self.found.lines.push([a.x, a.y, b.x, b.y]);
            self.found.lines_painted.push(painted);
            self.found.lines_subpath.push(subpath);
        }
    }

    /// One subpath: a move, what follows it, and its closing, if any, painted as `painted`.
    fn subpath(&mut self, elements: &[PathEl], transform: Affine, painted: Painted) {
        // Only stroked, a last side back to the start closes a box as `h` does.
        let stroked = painted == Painted::Stroked;
        let line = (painted, self.next_subpath());
        let (start, body) = match elements.split_first() {
            Some((PathEl::MoveTo(start), body)) => (Some(*start), body),
            _ => (None, elements),
        };
        let (body, closed) = match body.split_last() {
            Some((PathEl::ClosePath, body)) => (body, true),
            _ => (body, false),
        };
        if let Some(start) = start
            && let Some(rect) = as_rectangle(start, body, closed, stroked)
        {
            self.rectangle(&rect, transform, painted);
            return;
        }
        let mut current = start;
        for element in body {
            match *element {
                PathEl::LineTo(p) => {
                    if let Some(from) = current {
                        self.line(from, p, transform, line);
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
            self.line(from, to, transform, line);
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
/// sides, closed by `h`, or by a last side back to the start when only `stroked` (filled, MuPDF
/// gives that one as four lines), the sides running in turn along one axis and the other.
fn as_rectangle(start: Point, body: &[PathEl], closed: bool, stroked: bool) -> Option<Rect> {
    let corners = match *body {
        [PathEl::LineTo(a), PathEl::LineTo(b), PathEl::LineTo(c)] if closed => [start, a, b, c],
        [
            PathEl::LineTo(a),
            PathEl::LineTo(b),
            PathEl::LineTo(c),
            PathEl::LineTo(d),
        ] if stroked && same(d, start) => [start, a, b, c],
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

/// `elements` as MuPDF builds a path of them. Each curve whose control points lie on its ends (the
/// first on its start and the second on its end, or both on one end) is made a line. A line that
/// goes nowhere (to the point it starts from) is left out, unless it follows a move: there it
/// stays, of no length. Points compare exactly, as written before the matrix: a control point a
/// hair away keeps the curve, an end a hair away keeps the line.
fn built(elements: &[PathEl]) -> Vec<PathEl> {
    let (mut start, mut current) = (None::<Point>, None::<Point>);
    let mut after_move = false;
    let mut built = Vec::with_capacity(elements.len());
    for element in elements {
        let from = current;
        let element = match (*element, from) {
            // hayro moves the end of a line that goes nowhere: back where it was written.
            (PathEl::LineTo(p), Some(p0)) if p == Point::new(p0.x + NUDGE, p0.y) => {
                PathEl::LineTo(p0)
            }
            (PathEl::CurveTo(p1, p2, p3), Some(p0))
                if (p0 == p1 && (p2 == p3 || p1 == p2)) || (p1 == p2 && p2 == p3) =>
            {
                PathEl::LineTo(p3)
            }
            (element, _) => element,
        };
        current = match element {
            PathEl::MoveTo(p) => {
                start = Some(p);
                Some(p)
            }
            PathEl::LineTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => Some(p),
            PathEl::ClosePath => start,
        };
        if matches!(element, PathEl::LineTo(p) if Some(p) == from) && !after_move {
            continue;
        }
        after_move = matches!(element, PathEl::MoveTo(_));
        built.push(element);
    }
    built
}

/// `content` with each path that is one box filled without `h` (`m`, four `l` the last back to
/// the move's point, then `f`, `F`, `f*`, `B` or `B*`) in a sequence tagged [`OPEN_BOX`] giving
/// its corners; `None` when it has none.
fn instrument(content: &[u8]) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    let mut copied = 0;
    let mut path: Vec<Op> = Vec::new();
    for op in operators(content) {
        match &content[op.operator.clone()] {
            b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re" => path.push(op),
            b"W" | b"W*" => {}
            operator => {
                let filled = matches!(operator, b"f" | b"F" | b"f*" | b"B" | b"B*");
                if filled
                    && let Some(corners) = open_box(content, &path)
                    && let Some(first) = path.first()
                {
                    let out = out.get_or_insert_with(|| Vec::with_capacity(content.len() + 64));
                    out.extend_from_slice(&content[copied..first.start]);
                    out.extend_from_slice(b" /");
                    out.extend_from_slice(OPEN_BOX);
                    out.extend_from_slice(b" <</P [");
                    out.extend_from_slice(corners.as_bytes());
                    out.extend_from_slice(b"]>> BDC ");
                    out.extend_from_slice(&content[first.start..op.end]);
                    out.extend_from_slice(b" EMC ");
                    copied = op.end;
                }
                path.clear();
            }
        }
    }
    let mut out = out?;
    out.extend_from_slice(&content[copied..]);
    Some(out)
}

/// The corners of `path`, as written, when it is `m` and four `l`, the last back to the move's
/// point.
fn open_box(content: &[u8], path: &[Op]) -> Option<String> {
    let [moved, first, second, third, last] = path else {
        return None;
    };
    let point = |op: &Op, operator: &[u8]| -> Option<(f64, f64, String)> {
        if &content[op.operator.clone()] != operator {
            return None;
        }
        let operands = std::str::from_utf8(&content[op.start..op.operator.start]).ok()?;
        let mut numbers = operands.split_ascii_whitespace();
        let (x, y) = (numbers.next()?, numbers.next()?);
        if numbers.next().is_some() {
            return None;
        }
        Some((x.parse().ok()?, y.parse().ok()?, format!("{x} {y}")))
    };
    let (x0, y0, start) = point(moved, b"m")?;
    let corners = [first, second, third].map(|op| point(op, b"l"));
    let (x4, y4, _) = point(last, b"l")?;
    if (x4, y4) != (x0, y0) {
        return None;
    }
    let mut written = start;
    for corner in corners {
        let (_, _, text) = corner?;
        written.push(' ');
        written.push_str(&text);
    }
    Some(written)
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
        let painted = Painted::of(mode);
        let mut subpath: Vec<PathEl> = Vec::new();
        for element in built(path.elements()) {
            match element {
                PathEl::MoveTo(_) => {
                    self.subpath(&subpath, transform, painted);
                    subpath.clear();
                    subpath.push(element);
                }
                PathEl::ClosePath => {
                    subpath.push(element);
                    self.subpath(&subpath, transform, painted);
                    // What follows without a move starts where the closed subpath started.
                    let start = match subpath.first() {
                        Some(PathEl::MoveTo(start)) => Some(*start),
                        _ => None,
                    };
                    subpath.clear();
                    subpath.extend(start.map(PathEl::MoveTo));
                }
                _ => subpath.push(element),
            }
        }
        self.subpath(&subpath, transform, painted);
    }

    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, mode: &DrawMode) {
        let elements = [
            PathEl::MoveTo(Point::new(rect.x0, rect.y0)),
            PathEl::LineTo(Point::new(rect.x1, rect.y1)),
        ];
        if unseen(&props, mode) || self.repeated(&elements, props.transform, mode) {
            return;
        }
        let painted = Painted::of(mode);
        if painted.filled()
            && let Some(Some(corners)) = self.boxes.last()
        {
            let corners = *corners;
            let line = (painted, self.next_subpath());
            for (index, from) in corners.iter().enumerate() {
                self.line(*from, corners[(index + 1) % 4], props.transform, line);
            }
            return;
        }
        self.rectangle(rect, props.transform, painted);
    }

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn begin_marked_content(&mut self, tag: &[u8], properties: Option<&Dict<'_>>) {
        let corners = (tag == OPEN_BOX)
            .then(|| properties?.get::<Array<'_>>(b"P"))
            .flatten()
            .and_then(|corners| {
                let numbers: Vec<f64> = corners.iter::<f64>().collect();
                match numbers[..] {
                    [x0, y0, x1, y1, x2, y2, x3, y3] => Some([
                        Point::new(x0, y0),
                        Point::new(x1, y1),
                        Point::new(x2, y2),
                        Point::new(x3, y3),
                    ]),
                    _ => None,
                }
            });
        self.boxes.push(corners);
    }

    fn end_marked_content(&mut self) {
        self.boxes.pop();
    }
}
