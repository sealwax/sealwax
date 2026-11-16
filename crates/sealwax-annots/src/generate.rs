//! Appearances of annotations that have none, drawn as PDFium draws them
//! (`core/fpdfdoc/cpdf_generateap.cpp`, BSD-3-Clause: `THIRD_PARTY_NOTICES.md`, and
//! `LICENSE-PDFIUM` beside this file): circles, squares, ink, highlights, underlines,
//! strike-outs, squiggles and notes. PDFium draws them when
//! it renders such a document: flattened, the page then renders as it did, which is what the
//! benchmark checks. Coordinates are computed in single precision and written with six
//! significant digits, as PDFium does: anti-aliasing then lands on the same pixels.
//!
//! Each drawing is in default user space, its `/BBox` the rectangle it is drawn on, placed on
//! that same rectangle: the annotation's `/Rect`; the bounding box of its `/QuadPoints` for text
//! markup; `/Rect` grown by half the border for ink; a 20 x 20 note at the corner of `/Rect` for
//! a note.

use sealwax_core::{Dict, Document, Name, Object, Rect, Result};

use crate::pdf;
use crate::text::number;

/// What PDFium draws for an annotation without appearance.
pub(crate) struct Generated {
    pub(crate) drawing: Vec<u8>,
    /// The form's `/BBox`, and where it is placed.
    pub(crate) rect: Rect,
    /// `/ExtGState` resources: `/GS`, the annotation's opacity and blend mode.
    pub(crate) resources: Dict,
}

/// A rectangle as PDFium holds one: four floats, in the order given.
#[derive(Debug, Clone, Copy)]
struct FloatRect {
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
}

impl FloatRect {
    fn normalized(self) -> Self {
        Self {
            left: self.left.min(self.right),
            bottom: self.bottom.min(self.top),
            right: self.left.max(self.right),
            top: self.bottom.max(self.top),
        }
    }

    fn deflated(self, by: f32) -> Self {
        Self {
            left: self.left + by,
            bottom: self.bottom + by,
            right: self.right - by,
            top: self.top - by,
        }
    }

    fn rect(self) -> Rect {
        Rect::from_corners(
            f64::from(self.left),
            f64::from(self.bottom),
            f64::from(self.right),
            f64::from(self.top),
        )
    }
}

/// A float as C++ streams write one: six significant digits, no trailing zero.
fn g(value: f32) -> String {
    if value == 0.0 || !value.is_finite() {
        return "0".to_owned();
    }
    let value = f64::from(value);
    #[allow(clippy::cast_possible_truncation)]
    let exponent = value.abs().log10().floor() as i32;
    let decimals = usize::try_from((5 - exponent).max(0)).unwrap_or(0);
    let text = format!("{value:.decimals$}");
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        &text
    };
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

fn point(x: f32, y: f32) -> String {
    format!("{} {}", g(x), g(y))
}

#[allow(clippy::cast_possible_truncation)]
fn float(value: &Object) -> f32 {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .map_or(0.0, |v| v as f32)
}

/// `/Rect` as PDFium reads it, not normalized.
fn raw_rect(doc: &Document, annotation: &Dict) -> Result<Option<FloatRect>> {
    let Some(value) = annotation.get(b"Rect") else {
        return Ok(None);
    };
    let Some(Object::Array(items)) = pdf::resolved(doc, value)? else {
        return Ok(None);
    };
    let [left, bottom, right, top] = items.as_slice() else {
        return Ok(None);
    };
    Ok(Some(FloatRect {
        left: float(left),
        bottom: float(bottom),
        right: float(right),
        top: float(top),
    }))
}

/// A colour of an annotation (`/C`, `/IC`): `None` when absent, `Some(None)` for a colour PDFium
/// reads as transparent.
fn colour(doc: &Document, value: Option<&Object>) -> Result<Option<Option<Vec<f64>>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let components = pdf::resolved(doc, value)?
        .as_ref()
        .and_then(pdf::number_array)
        .unwrap_or_default();
    Ok(Some(
        matches!(components.len(), 1 | 3 | 4).then_some(components),
    ))
}

/// `GetColorStringWithDefault`: the colour operator, or nothing for a transparent colour.
fn colour_operator(colour: Option<&[f64]>, stroke: bool) -> String {
    colour
        .and_then(|c| crate::draw::color(c, stroke))
        .map(|op| format!("{op}\n"))
        .unwrap_or_default()
}

/// `GetBorderWidth`: `/BS /W`, else the third item of `/Border`, else 1.
fn border_width(doc: &Document, annotation: &Dict) -> Result<f32> {
    if let Some(style) = pdf::dict(doc, annotation.get(b"BS"))?
        && let Some(width) = style.get(b"W")
    {
        return Ok(pdf::resolved(doc, width)?.as_ref().map_or(0.0, float));
    }
    if let Some(border) = annotation.get(b"Border")
        && let Some(Object::Array(items)) = pdf::resolved(doc, border)?
        && items.len() > 2
    {
        return Ok(float(&items[2]));
    }
    Ok(1.0)
}

/// `GetDashPatternString`: `/BS /D` for a dashed style, else the fourth item of `/Border`, at
/// most ten values.
fn dash_pattern(doc: &Document, annotation: &Dict) -> Result<String> {
    let mut dash: Option<Object> = None;
    if let Some(style) = pdf::dict(doc, annotation.get(b"BS"))? {
        if style.get(b"S").and_then(Object::as_name) == Some(b"D")
            && let Some(value) = style.get(b"D")
        {
            dash = pdf::resolved(doc, value)?;
        }
    } else if let Some(border) = annotation.get(b"Border")
        && let Some(Object::Array(items)) = pdf::resolved(doc, border)?
        && items.len() == 4
    {
        dash = pdf::resolved(doc, &items[3])?;
    }
    let Some(Object::Array(items)) = dash else {
        return Ok(String::new());
    };
    if items.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from("[");
    for value in items.iter().take(10) {
        out.push_str(&number(f64::from(float(value))));
        out.push(' ');
    }
    out.push_str("] 0 d\n");
    Ok(out)
}

/// `GenerateExtGStateDict`: `/GS`, with the annotation's opacity (`/CA`, else 1) and `blend`.
fn resources(doc: &Document, annotation: &Dict, blend: &str) -> Result<Dict> {
    let opacity = match annotation.get(b"CA") {
        Some(value) => pdf::resolved(doc, value)?.as_ref().map_or(0.0, float),
        None => 1.0,
    };
    let mut state = Dict::new();
    state.insert("Type", Name::from("ExtGState"));
    state.insert("CA", Object::Real(f64::from(opacity)));
    state.insert("ca", Object::Real(f64::from(opacity)));
    state.insert("AIS", false);
    state.insert("BM", Name::from(blend));
    let mut states = Dict::new();
    states.insert("GS", state);
    let mut resources = Dict::new();
    resources.insert("ExtGState", states);
    Ok(resources)
}

/// The rectangles of `/QuadPoints`, as PDFium reads them: points 3 (bottom left) and 2 (top
/// right) of each quadrilateral, normalized.
fn quads(doc: &Document, annotation: &Dict) -> Result<Vec<FloatRect>> {
    let Some(value) = annotation.get(b"QuadPoints") else {
        return Ok(Vec::new());
    };
    let Some(Object::Array(items)) = pdf::resolved(doc, value)? else {
        return Ok(Vec::new());
    };
    Ok(items
        .as_chunks::<8>()
        .0
        .iter()
        .map(|q| {
            FloatRect {
                left: float(&q[4]),
                bottom: float(&q[5]),
                right: float(&q[2]),
                top: float(&q[3]),
            }
            .normalized()
        })
        .collect())
}

fn union(rects: &[FloatRect]) -> Option<FloatRect> {
    let first = *rects.first()?;
    Some(rects.iter().skip(1).fold(first, |a, b| FloatRect {
        left: a.left.min(b.left),
        bottom: a.bottom.min(b.bottom),
        right: a.right.max(b.right),
        top: a.top.max(b.top),
    }))
}

/// PDFium's appearance for an annotation without one, `None` for a subtype it does not draw
/// (or ink without strokes, text markup without quadrilaterals).
pub(crate) fn generate(
    doc: &Document,
    annotation: &Dict,
    subtype: &[u8],
) -> Result<Option<Generated>> {
    let Some(raw) = raw_rect(doc, annotation)? else {
        return Ok(None);
    };
    Ok(match subtype {
        b"Circle" | b"Square" => Some(shape(doc, annotation, raw, subtype == b"Circle")?),
        b"Ink" => ink(doc, annotation, raw)?,
        b"Highlight" | b"Underline" | b"StrikeOut" | b"Squiggly" => {
            markup(doc, annotation, subtype)?
        }
        b"Text" => Some(note(doc, annotation, raw)?),
        _ => None,
    })
}

/// `GenerateCircleAP` and `GenerateSquareAP`.
fn shape(doc: &Document, annotation: &Dict, raw: FloatRect, circle: bool) -> Result<Generated> {
    let interior = colour(doc, annotation.get(b"IC"))?;
    let mut drawing = String::from("/GS gs ");
    drawing.push_str(&colour_operator(
        interior.clone().flatten().as_deref(),
        false,
    ));
    let stroke = colour(doc, annotation.get(b"C"))?.unwrap_or(Some(vec![0.0, 0.0, 0.0]));
    drawing.push_str(&colour_operator(stroke.as_deref(), true));
    let width = border_width(doc, annotation)?;
    let stroked = width > 0.0;
    if stroked {
        drawing.push_str(&format!("{} w ", g(width)));
        drawing.push_str(&dash_pattern(doc, annotation)?);
    }
    let mut shape = raw.normalized();
    if stroked {
        shape = shape.deflated(width / 2.0);
    }
    // An `/IC` array, even of a colour PDFium does not read, fills.
    let filled = matches!(&interior, Some(Some(_)))
        || annotation
            .get(b"IC")
            .and_then(Object::as_array)
            .is_some_and(|c| !c.is_empty());
    let paint = match (stroked, filled) {
        (true, true) => "b",
        (true, false) => "s",
        (false, true) => "f",
        (false, false) => "n",
    };
    if circle {
        let mx = (shape.left + shape.right) / 2.0;
        let my = (shape.top + shape.bottom) / 2.0;
        // 4 tan(pi / 8) / 3: control points of a quarter circle.
        const L: f32 = 0.5523;
        #[allow(clippy::cast_possible_truncation)]
        let dx = (f64::from(L * (shape.right - shape.left)) / 2.0) as f32;
        #[allow(clippy::cast_possible_truncation)]
        let dy = (f64::from(L * (shape.top - shape.bottom)) / 2.0) as f32;
        drawing.push_str(&format!("{} m\n", point(mx, shape.top)));
        drawing.push_str(&format!(
            "{} {} {} c\n",
            point(mx + dx, shape.top),
            point(shape.right, my + dy),
            point(shape.right, my)
        ));
        drawing.push_str(&format!(
            "{} {} {} c\n",
            point(shape.right, my - dy),
            point(mx + dx, shape.bottom),
            point(mx, shape.bottom)
        ));
        drawing.push_str(&format!(
            "{} {} {} c\n",
            point(mx - dx, shape.bottom),
            point(shape.left, my - dy),
            point(shape.left, my)
        ));
        drawing.push_str(&format!(
            "{} {} {} c\n",
            point(shape.left, my + dy),
            point(mx - dx, shape.top),
            point(mx, shape.top)
        ));
        drawing.push_str(paint);
        drawing.push('\n');
    } else {
        drawing.push_str(&format!(
            "{} {} {} re {paint}\n",
            point(shape.left, shape.bottom),
            g(shape.right - shape.left),
            g(shape.top - shape.bottom)
        ));
    }
    Ok(Generated {
        drawing: drawing.into_bytes(),
        rect: raw.rect(),
        resources: resources(doc, annotation, "Normal")?,
    })
}

/// `GenerateInkAP`: each path of `/InkList`, on `/Rect` grown by half the border (as given: a
/// `/Rect` whose corners are swapped shrinks, as in PDFium).
fn ink(doc: &Document, annotation: &Dict, raw: FloatRect) -> Result<Option<Generated>> {
    let Some(value) = annotation.get(b"InkList") else {
        return Ok(None);
    };
    let Some(Object::Array(paths)) = pdf::resolved(doc, value)? else {
        return Ok(None);
    };
    let width = border_width(doc, annotation)?;
    if paths.is_empty() || width <= 0.0 {
        return Ok(None);
    }
    let mut drawing = String::from("/GS gs ");
    let stroke = colour(doc, annotation.get(b"C"))?.unwrap_or(Some(vec![0.0, 0.0, 0.0]));
    drawing.push_str(&colour_operator(stroke.as_deref(), true));
    drawing.push_str(&format!("{} w ", g(width)));
    drawing.push_str(&dash_pattern(doc, annotation)?);
    for path in &paths {
        let Some(Object::Array(points)) = pdf::resolved(doc, path)? else {
            continue;
        };
        if points.len() < 2 {
            continue;
        }
        drawing.push_str(&format!(
            "{} m ",
            point(float(&points[0]), float(&points[1]))
        ));
        for pair in points.as_chunks::<2>().0 {
            drawing.push_str(&format!("{} l ", point(float(&pair[0]), float(&pair[1]))));
        }
        drawing.push_str("S\n");
    }
    let grown = FloatRect {
        left: raw.left - width / 2.0,
        bottom: raw.bottom - width / 2.0,
        right: raw.right + width / 2.0,
        top: raw.top + width / 2.0,
    };
    Ok(Some(Generated {
        drawing: drawing.into_bytes(),
        rect: grown.rect(),
        resources: resources(doc, annotation, "Normal")?,
    }))
}

/// `GenerateHighlightAP`, `GenerateUnderlineAP`, `GenerateStrikeOutAP`, `GenerateSquigglyAP`:
/// drawn on the bounding box of `/QuadPoints`.
fn markup(doc: &Document, annotation: &Dict, subtype: &[u8]) -> Result<Option<Generated>> {
    let rects = quads(doc, annotation)?;
    let Some(bounds) = union(&rects) else {
        return Ok(None);
    };
    let highlight = subtype == b"Highlight";
    let default = if highlight {
        vec![1.0, 1.0, 0.0]
    } else {
        vec![0.0, 0.0, 0.0]
    };
    let colour_value = colour(doc, annotation.get(b"C"))?.unwrap_or(Some(default));
    let mut drawing = String::from("/GS gs ");
    drawing.push_str(&colour_operator(colour_value.as_deref(), !highlight));
    if matches!(subtype, b"Underline" | b"Squiggly") {
        drawing.push_str("1 w ");
    }
    for r in &rects {
        match subtype {
            b"Highlight" => drawing.push_str(&format!(
                "{} m {} l {} l {} l h f\n",
                point(r.left, r.top),
                point(r.right, r.top),
                point(r.right, r.bottom),
                point(r.left, r.bottom)
            )),
            b"Underline" => drawing.push_str(&format!(
                "{} m {} l S\n",
                point(r.left, r.bottom + 1.0),
                point(r.right, r.bottom + 1.0)
            )),
            b"StrikeOut" => {
                let y = (r.top + r.bottom) / 2.0;
                drawing.push_str(&format!(
                    "1 w {} m {} l S\n",
                    point(r.left, y),
                    point(r.right, y)
                ));
            }
            _ => {
                // Squiggly: a zigzag 2 points high and 2 wide along the bottom.
                const DELTA: f32 = 2.0;
                let (top, bottom) = (r.bottom + DELTA, r.bottom);
                drawing.push_str(&format!("{} m ", point(r.left, top)));
                let mut x = r.left + DELTA;
                let mut upwards = false;
                let mut steps = 0usize;
                while x < r.right && steps < MAX_SQUIGGLE_STEPS {
                    drawing.push_str(&format!(
                        "{} l ",
                        point(x, if upwards { top } else { bottom })
                    ));
                    x += DELTA;
                    upwards = !upwards;
                    steps += 1;
                }
                let remainder = r.right - (x - DELTA);
                let end = if upwards {
                    bottom + remainder
                } else {
                    top - remainder
                };
                drawing.push_str(&format!("{} l S\n", point(r.right, end)));
            }
        }
    }
    let blend = if highlight { "Multiply" } else { "Normal" };
    Ok(Some(Generated {
        drawing: drawing.into_bytes(),
        rect: bounds.rect(),
        resources: resources(doc, annotation, blend)?,
    }))
}

/// The most segments of one squiggle: a line longer than any page's.
const MAX_SQUIGGLE_STEPS: usize = 100_000;

/// `GenerateTextAP` and `GenerateTextSymbolAP`: a 20 x 20 yellow note with three lines, at the
/// first corner of `/Rect` as given.
fn note(doc: &Document, annotation: &Dict, raw: FloatRect) -> Result<Generated> {
    let note = FloatRect {
        left: raw.left,
        bottom: raw.bottom,
        right: raw.left + 20.0,
        top: raw.bottom + 20.0,
    };
    let tip = 4.0;
    let mut outer = note.deflated(0.5);
    outer.bottom += tip;
    let tip_left = outer.left + tip;
    let tip_right = tip_left + tip;
    let tip_bottom = outer.bottom - tip;
    let tip_middle = (tip_left + tip_right) / 2.0;
    let mut drawing = String::from("/GS gs 1 1 0 rg\n0 0 0 RG\n1 w\n");
    for (x, y, op) in [
        (outer.left, outer.bottom, "m"),
        (outer.left, outer.top, "l"),
        (outer.right, outer.top, "l"),
        (outer.right, outer.bottom, "l"),
        (tip_right, outer.bottom, "l"),
        (tip_middle, tip_bottom, "l"),
        (tip_left, outer.bottom, "l"),
        (outer.left, outer.bottom, "l"),
    ] {
        drawing.push_str(&format!("{} {op}\n", point(x, y)));
    }
    let (left, right) = (outer.left + 2.0, outer.right - 2.0);
    let step = (outer.top - outer.bottom) / 4.0;
    let mut top = outer.top;
    for _ in 0..3 {
        top -= step;
        drawing.push_str(&format!(
            "{} m\n{} l\n",
            point(left, top),
            point(right, top)
        ));
    }
    drawing.push_str("B*\n");
    Ok(Generated {
        drawing: drawing.into_bytes(),
        rect: note.rect(),
        resources: resources(doc, annotation, "Normal")?,
    })
}

#[cfg(test)]
mod tests {
    use super::g;

    #[test]
    fn floats_are_written_as_cpp_streams_write_them() {
        assert_eq!(g(107.789_6), "107.79");
        assert_eq!(g(639.948_6), "639.949");
        assert_eq!(g(1.0), "1");
        assert_eq!(g(0.5), "0.5");
        assert_eq!(g(-12.25), "-12.25");
        assert_eq!(g(0.000_123_456_7), "0.000123457");
        assert_eq!(g(0.0), "0");
    }
}
