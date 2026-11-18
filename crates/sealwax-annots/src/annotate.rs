//! Annotations made as a reader's tools make them: ink, text markup, shapes, lines and free
//! text, each a PDF annotation with its appearance drawn (ISO 32000-1, 12.5.6), so that every
//! reader shows it alike and flattening keeps it.
//!
//! Coordinates are the page's default user space (unrotated, y up), as an XFDF gives them and as
//! [`crate::fields`] and the glyphs of a page are read.

use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, encode_text_string};

use crate::draw;
use crate::flatten::{Font, font_dict, form_stream};
use crate::pdf;
use crate::text::number;
use crate::xfdf::{Align, Text, VerticalAlign};
use crate::{Error, Result};

/// The most points an annotation is made of, strokes and quadrilaterals counted together.
const MAX_POINTS: usize = 100_000;
/// Bézier control points for a quarter of an ellipse.
const KAPPA: f64 = 0.552_284_75;

/// What an annotation is, and where.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AnnotationKind {
    /// Strokes drawn by hand, each a list of points (`Ink`).
    Ink { strokes: Vec<Vec<[f64; 2]>> },
    /// Text highlighted: one quadrilateral per line, its corners top-left, top-right,
    /// bottom-left, bottom-right, as Acrobat writes `/QuadPoints`.
    Highlight { quads: Vec<[f64; 8]> },
    /// Text underlined, by quadrilaterals as [`AnnotationKind::Highlight`].
    Underline { quads: Vec<[f64; 8]> },
    /// Text struck out, by quadrilaterals as [`AnnotationKind::Highlight`].
    StrikeOut { quads: Vec<[f64; 8]> },
    /// Text underlined with a wavy line, by quadrilaterals as [`AnnotationKind::Highlight`].
    Squiggly { quads: Vec<[f64; 8]> },
    /// A rectangle, `[x1, y1, x2, y2]`, its border inside it.
    Square { rect: [f64; 4] },
    /// An ellipse in a rectangle, its border inside it.
    Circle { rect: [f64; 4] },
    /// A straight line.
    Line { from: [f64; 2], to: [f64; 2] },
    /// A closed polygon.
    Polygon { points: Vec<[f64; 2]> },
    /// An open polyline.
    PolyLine { points: Vec<[f64; 2]> },
    /// Text in a rectangle, Helvetica at `size` points, wrapped to its width.
    FreeText {
        rect: [f64; 4],
        text: String,
        size: f64,
    },
}

/// An annotation to add ([`add_annotation`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct NewAnnotation {
    /// The page, from 0.
    pub page_index: usize,
    pub kind: AnnotationKind,
    /// RGB, each from 0 to 1: the strokes, the markup, the border, the text.
    pub color: [f64; 3],
    /// The inside of a square, circle or polygon, the background of a free text.
    pub fill: Option<[f64; 3]>,
    /// The width of the strokes and borders, in points; 0 draws no border.
    pub width: f64,
    /// From 0 (invisible) to 1 (opaque).
    pub opacity: f64,
    /// Who made it (`/T`).
    pub author: Option<String>,
    /// Its text, or a comment on it (`/Contents`).
    pub contents: Option<String>,
}

impl NewAnnotation {
    /// An annotation of `kind` on page `page_index`, opaque, one point wide, black, a highlight
    /// yellow.
    #[must_use]
    pub fn new(page_index: usize, kind: AnnotationKind) -> Self {
        let color = match kind {
            AnnotationKind::Highlight { .. } => [1.0, 1.0, 0.0],
            _ => [0.0; 3],
        };
        Self {
            page_index,
            kind,
            color,
            fill: None,
            width: 1.0,
            opacity: 1.0,
            author: None,
            contents: None,
        }
    }

    #[must_use]
    pub fn color(mut self, color: [f64; 3]) -> Self {
        self.color = color;
        self
    }

    #[must_use]
    pub fn fill(mut self, fill: Option<[f64; 3]>) -> Self {
        self.fill = fill;
        self
    }

    #[must_use]
    pub fn width(mut self, width: f64) -> Self {
        self.width = width;
        self
    }

    #[must_use]
    pub fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    #[must_use]
    pub fn author(mut self, author: Option<String>) -> Self {
        self.author = author;
        self
    }

    #[must_use]
    pub fn contents(mut self, contents: Option<String>) -> Self {
        self.contents = contents;
        self
    }
}

fn invalid(what: &'static str) -> Error {
    Error::Pdf(sealwax_core::Error::InvalidArgument { what })
}

/// Add an annotation to its page, its appearance drawn: printed, its colour and opacity (a
/// highlight multiplies the colours under it, so that the text shows through), its border
/// solid. A free text's characters WinAnsiEncoding does not have are drawn as `?`; its
/// `/Contents` keeps them. Returns the annotation.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have, a number
/// that is not finite, a colour or opacity outside 0 to 1, a negative width or size, no point,
/// more than 100,000 points, or a rectangle without area.
pub fn add_annotation(doc: &mut Document, annotation: &NewAnnotation) -> Result<ObjId> {
    let Some(Some(page)) = doc.page_ids().get(annotation.page_index).copied() else {
        return Err(invalid("page index"));
    };
    let unit = |v: &f64| v.is_finite() && (0.0..=1.0).contains(v);
    if !annotation.color.iter().all(unit) || !annotation.fill.iter().flatten().all(unit) {
        return Err(invalid("annotation colour"));
    }
    if !unit(&annotation.opacity) {
        return Err(invalid("annotation opacity"));
    }
    if !(annotation.width.is_finite() && annotation.width >= 0.0) {
        return Err(invalid("annotation width"));
    }
    let shape = Shape::of(&annotation.kind)?;
    let half = annotation.width / 2.0;
    // Room for the strokes' width, and for a squiggle under the text.
    let margin = match &annotation.kind {
        AnnotationKind::Squiggly { quads } => quads
            .iter()
            .map(|quad| quad_height(quad) / 8.0)
            .fold(half, f64::max),
        AnnotationKind::Square { .. }
        | AnnotationKind::Circle { .. }
        | AnnotationKind::FreeText { .. } => 0.0,
        _ => half,
    };
    let bounds = shape
        .bounds(margin)
        .ok_or_else(|| invalid("annotation geometry"))?;
    let (width, height) = (bounds.x2 - bounds.x1, bounds.y2 - bounds.y1);
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("Annot"));
    dict.insert("Subtype", Name::from(subtype(&annotation.kind)));
    dict.insert(
        "Rect",
        pdf::numbers(&[bounds.x1, bounds.y1, bounds.x2, bounds.y2]),
    );
    dict.insert("P", Object::Ref(page));
    // Printed.
    dict.insert("F", 4_i64);
    dict.insert("C", pdf::numbers(&annotation.color));
    if annotation.opacity < 1.0 {
        dict.insert("CA", Object::Real(annotation.opacity));
    }
    if let Some(author) = &annotation.author {
        dict.insert("T", Object::String(encode_text_string(author)));
    }
    let mut resources = Dict::new();
    let mut state = Dict::new();
    state.insert("Type", Name::from("ExtGState"));
    state.insert("CA", Object::Real(annotation.opacity));
    state.insert("ca", Object::Real(annotation.opacity));
    let drawing = match &annotation.kind {
        AnnotationKind::FreeText {
            rect: _,
            text,
            size,
        } => {
            if !(size.is_finite() && *size > 0.0) {
                return Err(invalid("font size"));
            }
            let contents = annotation.contents.clone().unwrap_or_else(|| text.clone());
            dict.insert("Contents", Object::String(encode_text_string(&contents)));
            let [r, g, b] = annotation.color;
            dict.insert(
                "DA",
                Object::String(
                    format!(
                        "/Helv {} Tf {} {} {} rg",
                        number(*size),
                        number(r),
                        number(g),
                        number(b)
                    )
                    .into_bytes(),
                ),
            );
            border(&mut dict, annotation.width);
            let mut fonts = Dict::new();
            fonts.insert(
                "Helv",
                Object::Ref(doc.add_object(font_dict(Font::Helvetica))?),
            );
            resources.insert("Font", fonts);
            let free = Text {
                page_number: annotation.page_index + 1,
                rect: bounds,
                contents: text.clone(),
                lines: None,
                size: *size,
                color: annotation.color.to_vec(),
                align: Align::Left,
                vertical: VerticalAlign::Top,
                border: annotation.width,
                border_color: (annotation.width > 0.0).then(|| annotation.color.to_vec()),
                fill: annotation.fill.map(|fill| fill.to_vec()),
                rotation: 0,
                font: None,
            };
            draw::free_text(&free, width, height).0
        }
        kind => {
            if let Some(contents) = &annotation.contents {
                dict.insert("Contents", Object::String(encode_text_string(contents)));
            }
            write_geometry(&mut dict, kind);
            if matches!(
                kind,
                AnnotationKind::Ink { .. }
                    | AnnotationKind::Square { .. }
                    | AnnotationKind::Circle { .. }
                    | AnnotationKind::Line { .. }
                    | AnnotationKind::Polygon { .. }
                    | AnnotationKind::PolyLine { .. }
            ) {
                border(&mut dict, annotation.width);
            }
            if let (Some(fill), true) = (
                annotation.fill,
                matches!(
                    kind,
                    AnnotationKind::Square { .. }
                        | AnnotationKind::Circle { .. }
                        | AnnotationKind::Polygon { .. }
                ),
            ) {
                dict.insert("IC", pdf::numbers(&fill));
            }
            if matches!(kind, AnnotationKind::Highlight { .. }) {
                state.insert("BM", Name::from("Multiply"));
            }
            let mut out = format!("1 0 0 1 {} {} cm ", number(-bounds.x1), number(-bounds.y1));
            out.push_str(&shape.drawing(annotation));
            out.into_bytes()
        }
    };
    let mut states = Dict::new();
    states.insert("GS0", Object::Dict(state));
    resources.insert("ExtGState", states);
    let mut appearance = b"/GS0 gs ".to_vec();
    appearance.extend_from_slice(&drawing);
    let form = doc.add_object(form_stream(&appearance, width, height, resources))?;
    let mut appearances = Dict::new();
    appearances.insert("N", Object::Ref(form));
    dict.insert("AP", appearances);
    let id = doc.add_object(Object::Dict(dict))?;
    let name = format!("sealwax-{}", id.number);
    if let Some(Object::Dict(mut written)) = doc.object(id)? {
        written.insert("NM", Object::String(name.into_bytes()));
        doc.set_object(id, Object::Dict(written));
    }
    add_to_page(doc, page, id)?;
    Ok(id)
}

/// A solid border `width` wide (`/BS`).
fn border(dict: &mut Dict, width: f64) {
    let mut style = Dict::new();
    style.insert("W", Object::Real(width));
    style.insert("S", Name::from("S"));
    dict.insert("BS", style);
}

fn subtype(kind: &AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Ink { .. } => "Ink",
        AnnotationKind::Highlight { .. } => "Highlight",
        AnnotationKind::Underline { .. } => "Underline",
        AnnotationKind::StrikeOut { .. } => "StrikeOut",
        AnnotationKind::Squiggly { .. } => "Squiggly",
        AnnotationKind::Square { .. } => "Square",
        AnnotationKind::Circle { .. } => "Circle",
        AnnotationKind::Line { .. } => "Line",
        AnnotationKind::Polygon { .. } => "Polygon",
        AnnotationKind::PolyLine { .. } => "PolyLine",
        AnnotationKind::FreeText { .. } => "FreeText",
    }
}

/// What the annotation's dictionary says of its geometry: `/InkList`, `/QuadPoints`, `/L`,
/// `/Vertices`.
fn write_geometry(dict: &mut Dict, kind: &AnnotationKind) {
    let flat = |points: &[[f64; 2]]| -> Object {
        pdf::numbers(&points.iter().flatten().copied().collect::<Vec<_>>())
    };
    match kind {
        AnnotationKind::Ink { strokes } => {
            dict.insert(
                "InkList",
                Object::Array(strokes.iter().map(|stroke| flat(stroke)).collect()),
            );
        }
        AnnotationKind::Highlight { quads }
        | AnnotationKind::Underline { quads }
        | AnnotationKind::StrikeOut { quads }
        | AnnotationKind::Squiggly { quads } => {
            dict.insert(
                "QuadPoints",
                pdf::numbers(&quads.iter().flatten().copied().collect::<Vec<_>>()),
            );
        }
        AnnotationKind::Line { from, to } => {
            dict.insert("L", pdf::numbers(&[from[0], from[1], to[0], to[1]]));
        }
        AnnotationKind::Polygon { points } | AnnotationKind::PolyLine { points } => {
            dict.insert("Vertices", flat(points));
        }
        AnnotationKind::Square { .. }
        | AnnotationKind::Circle { .. }
        | AnnotationKind::FreeText { .. } => {}
    }
}

/// The height of a quadrilateral: from its top-left corner to its bottom-left one.
fn quad_height(quad: &[f64; 8]) -> f64 {
    (quad[0] - quad[4]).hypot(quad[1] - quad[5])
}

/// The geometry to draw, checked.
enum Shape<'a> {
    Strokes(&'a [Vec<[f64; 2]>]),
    Quads(&'a [[f64; 8]]),
    Box([f64; 4]),
    Path(Vec<[f64; 2]>, bool),
}

impl<'a> Shape<'a> {
    fn of(kind: &'a AnnotationKind) -> Result<Self> {
        let finite = |values: &mut dyn Iterator<Item = &f64>| {
            let mut all = true;
            for v in values {
                all &= v.is_finite();
            }
            all
        };
        let shape = match kind {
            AnnotationKind::Ink { strokes } => {
                if strokes.iter().all(Vec::is_empty) {
                    return Err(invalid("ink strokes"));
                }
                Self::Strokes(strokes)
            }
            AnnotationKind::Highlight { quads }
            | AnnotationKind::Underline { quads }
            | AnnotationKind::StrikeOut { quads }
            | AnnotationKind::Squiggly { quads } => {
                if quads.is_empty() {
                    return Err(invalid("quadrilaterals"));
                }
                Self::Quads(quads)
            }
            AnnotationKind::Square { rect }
            | AnnotationKind::Circle { rect }
            | AnnotationKind::FreeText { rect, .. } => {
                let [x1, y1, x2, y2] = *rect;
                let normalized = [x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)];
                if !(normalized[2] > normalized[0] && normalized[3] > normalized[1]) {
                    return Err(invalid("annotation rectangle"));
                }
                Self::Box(normalized)
            }
            AnnotationKind::Line { from, to } => {
                if from == to {
                    return Err(invalid("line ends"));
                }
                Self::Path(vec![*from, *to], false)
            }
            AnnotationKind::Polygon { points } | AnnotationKind::PolyLine { points } => {
                if points.len() < 2 {
                    return Err(invalid("annotation points"));
                }
                Self::Path(
                    points.clone(),
                    matches!(kind, AnnotationKind::Polygon { .. }),
                )
            }
        };
        let count = match &shape {
            Self::Strokes(strokes) => strokes.iter().map(Vec::len).sum(),
            Self::Quads(quads) => quads.len() * 4,
            Self::Box(_) => 2,
            Self::Path(points, _) => points.len(),
        };
        if count > MAX_POINTS {
            return Err(invalid("annotation points"));
        }
        let all_finite = match &shape {
            Self::Strokes(strokes) => finite(&mut strokes.iter().flatten().flatten()),
            Self::Quads(quads) => finite(&mut quads.iter().flatten()),
            Self::Box(rect) => finite(&mut rect.iter()),
            Self::Path(points, _) => finite(&mut points.iter().flatten()),
        };
        if !all_finite {
            return Err(invalid("annotation geometry"));
        }
        Ok(shape)
    }

    /// Every point, for the bounds.
    fn points(&self) -> Vec<[f64; 2]> {
        match self {
            Self::Strokes(strokes) => strokes.iter().flatten().copied().collect(),
            Self::Quads(quads) => quads
                .iter()
                .flat_map(|q| [[q[0], q[1]], [q[2], q[3]], [q[4], q[5]], [q[6], q[7]]])
                .collect(),
            Self::Box([x1, y1, x2, y2]) => vec![[*x1, *y1], [*x2, *y2]],
            Self::Path(points, _) => points.clone(),
        }
    }

    /// The rectangle the annotation covers, `margin` around its points; `None` without area.
    fn bounds(&self, margin: f64) -> Option<Rect> {
        let points = self.points();
        let first = points.first()?;
        let (mut x1, mut y1, mut x2, mut y2) = (first[0], first[1], first[0], first[1]);
        for [x, y] in &points {
            (x1, y1, x2, y2) = (x1.min(*x), y1.min(*y), x2.max(*x), y2.max(*y));
        }
        let (mut x1, mut y1, mut x2, mut y2) = (x1 - margin, y1 - margin, x2 + margin, y2 + margin);
        // A dot of ink, a horizontal line drawn without width: a point of room at least.
        if x2 - x1 < 1.0 {
            (x1, x2) = (x1 - 0.5, x2 + 0.5);
        }
        if y2 - y1 < 1.0 {
            (y1, y2) = (y1 - 0.5, y2 + 0.5);
        }
        let rect = Rect::from_corners(x1, y1, x2, y2);
        (rect.x2 > rect.x1 && rect.y2 > rect.y1).then_some(rect)
    }

    /// The content that draws it, in default user space.
    fn drawing(&self, annotation: &NewAnnotation) -> String {
        let stroke = draw::color(&annotation.color, true).unwrap_or_default();
        let fill = annotation
            .fill
            .and_then(|fill| draw::color(&fill, false))
            .unwrap_or_default();
        let fill_color = draw::color(&annotation.color, false).unwrap_or_default();
        let w = annotation.width;
        let point = |p: &[f64; 2]| format!("{} {}", number(p[0]), number(p[1]));
        let mut out = String::new();
        match (self, &annotation.kind) {
            (Self::Strokes(strokes), _) => {
                out.push_str(&format!("1 J 1 j {} w {stroke} ", number(w)));
                for stroke in strokes.iter().filter(|s| !s.is_empty()) {
                    out.push_str(&format!("{} m ", point(&stroke[0])));
                    for p in stroke.iter().skip(1) {
                        out.push_str(&format!("{} l ", point(p)));
                    }
                    if stroke.len() == 1 {
                        // A single point is a dot: the round cap of a line of no length.
                        out.push_str(&format!("{} l ", point(&stroke[0])));
                    }
                    out.push_str("S ");
                }
            }
            (Self::Quads(quads), kind) => {
                for q in *quads {
                    let (tl, tr, bl, br) = ([q[0], q[1]], [q[2], q[3]], [q[4], q[5]], [q[6], q[7]]);
                    let h = quad_height(q);
                    let along = |a: [f64; 2], b: [f64; 2], t: f64| -> [f64; 2] {
                        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
                    };
                    match kind {
                        AnnotationKind::Highlight { .. } => {
                            out.push_str(&format!(
                                "{fill_color} {} m {} l {} l {} l h f ",
                                point(&tl),
                                point(&tr),
                                point(&br),
                                point(&bl)
                            ));
                        }
                        AnnotationKind::Underline { .. } | AnnotationKind::StrikeOut { .. } => {
                            let t = if matches!(kind, AnnotationKind::Underline { .. }) {
                                // A little above the bottom, as the descenders go below it.
                                0.93
                            } else {
                                0.5
                            };
                            let (a, b) = (along(tl, bl, t), along(tr, br, t));
                            out.push_str(&format!(
                                "{stroke} {} w {} m {} l S ",
                                number((h / 14.0).max(0.5)),
                                point(&a),
                                point(&b)
                            ));
                        }
                        _ => {
                            // A wave along the bottom, a period of a third of the line's height.
                            let length = (br[0] - bl[0]).hypot(br[1] - bl[1]);
                            let period = (h / 3.0).max(1.0);
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            let steps = ((length / period * 2.0).ceil() as usize).clamp(1, 10_000);
                            let amplitude = h / 16.0;
                            // Perpendicular to the line, toward the top.
                            let (ux, uy) = (
                                (tl[0] - bl[0]) / h.max(f64::EPSILON),
                                (tl[1] - bl[1]) / h.max(f64::EPSILON),
                            );
                            out.push_str(&format!("{stroke} {} w ", number((h / 20.0).max(0.4))));
                            for step in 0..=steps {
                                #[allow(clippy::cast_precision_loss)]
                                let t = step as f64 / steps as f64;
                                let base = along(bl, br, t);
                                let offset = if step % 2 == 0 { -amplitude } else { amplitude };
                                let p = [base[0] + ux * offset, base[1] + uy * offset];
                                out.push_str(&format!(
                                    "{} {} ",
                                    point(&p),
                                    if step == 0 { "m" } else { "l" }
                                ));
                            }
                            out.push_str("S ");
                        }
                    }
                }
            }
            (Self::Box([x1, y1, x2, y2]), kind) => {
                let inset = w / 2.0;
                let (x1, y1, x2, y2) = (x1 + inset, y1 + inset, x2 - inset, y2 - inset);
                let paint = match (annotation.fill.is_some(), w > 0.0) {
                    (true, true) => "B",
                    (true, false) => "f",
                    (false, _) => "S",
                };
                out.push_str(&format!("{} w {stroke} {fill} ", number(w)));
                if matches!(kind, AnnotationKind::Circle { .. }) {
                    let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
                    let (rx, ry) = ((x2 - x1) / 2.0, (y2 - y1) / 2.0);
                    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
                    let n = number;
                    out.push_str(&format!(
                        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h {paint} ",
                        n(cx + rx), n(cy),
                        n(cx + rx), n(cy + ky), n(cx + kx), n(cy + ry), n(cx), n(cy + ry),
                        n(cx - kx), n(cy + ry), n(cx - rx), n(cy + ky), n(cx - rx), n(cy),
                        n(cx - rx), n(cy - ky), n(cx - kx), n(cy - ry), n(cx), n(cy - ry),
                        n(cx + kx), n(cy - ry), n(cx + rx), n(cy - ky), n(cx + rx), n(cy),
                    ));
                } else {
                    out.push_str(&format!(
                        "{} {} {} {} re {paint} ",
                        number(x1),
                        number(y1),
                        number(x2 - x1),
                        number(y2 - y1)
                    ));
                }
            }
            (Self::Path(points, closed), _) => {
                let paint = match (*closed, annotation.fill.is_some(), w > 0.0) {
                    (true, true, true) => "b",
                    (true, true, false) => "f",
                    (true, false, _) => "s",
                    (false, _, _) => "S",
                };
                out.push_str(&format!("1 j {} w {stroke} {fill} ", number(w)));
                out.push_str(&format!("{} m ", point(&points[0])));
                for p in points.iter().skip(1) {
                    out.push_str(&format!("{} l ", point(p)));
                }
                out.push_str(paint);
                out.push(' ');
            }
        }
        out
    }
}

/// `annotation` added to the annotations of `page`.
pub(crate) fn add_to_page(doc: &mut Document, page: ObjId, annotation: ObjId) -> Result<()> {
    let Some(Object::Dict(mut dict)) = doc.object(page)? else {
        return Err(Error::Pdf(sealwax_core::Error::Malformed));
    };
    match dict.get(b"Annots").cloned() {
        Some(Object::Ref(id)) => {
            let mut list = match doc.object(id)? {
                Some(Object::Array(list)) => list,
                _ => Vec::new(),
            };
            list.push(Object::Ref(annotation));
            doc.set_object(id, Object::Array(list));
        }
        Some(Object::Array(mut list)) => {
            list.push(Object::Ref(annotation));
            dict.insert("Annots", Object::Array(list));
            doc.set_object(page, Object::Dict(dict));
        }
        _ => {
            dict.insert("Annots", Object::Array(vec![Object::Ref(annotation)]));
            doc.set_object(page, Object::Dict(dict));
        }
    }
    Ok(())
}
