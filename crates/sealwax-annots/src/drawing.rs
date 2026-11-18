//! Text, lines and rectangles drawn on a page, and links to addresses: what test fixtures put on
//! pages. Positions are given on the page as it shows (turned by its rotation, from the top-left
//! corner of its crop box, y down, in points), as fields are; text reads upright as it shows.
//!
//! Each call draws a form XObject after the page's content. Text is set in a standard font,
//! not embedded, in `WinAnsiEncoding`; a line of text is the font's height (from its descent to
//! its ascent, as MuPDF reads the fonts it draws standard fonts with) below the line before.

use std::fmt::Write as _;
use std::io::Write as _;

use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Stream};

use crate::annotate;
use crate::create::{shown_matrix, shown_to_user};
use crate::page::{self, Placed};
use crate::pdf;
use crate::text::{encode, literal};
use crate::{Error, Result};

/// The standard fonts text is drawn in, with their height in em (ascent to descent), and the
/// short names PyMuPDF knows them by.
const TEXT_FONTS: [(&str, &str, f64); 12] = [
    ("Helvetica", "helv", 1.374),
    ("Helvetica-Bold", "hebo", 1.377),
    ("Helvetica-Oblique", "heit", 1.354),
    ("Helvetica-BoldOblique", "hebi", 1.382),
    ("Times-Roman", "tiro", 1.334),
    ("Times-Bold", "tibo", 1.385),
    ("Times-Italic", "tiit", 1.221),
    ("Times-BoldItalic", "tibi", 1.296),
    ("Courier", "cour", 1.249),
    ("Courier-Bold", "cobo", 1.4),
    ("Courier-Oblique", "coit", 1.237),
    ("Courier-BoldOblique", "cobi", 1.39),
];

/// What [`draw_shapes`] draws.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Text whose first line's baseline starts at `at`; `\n` starts the next line. `font` is a
    /// standard font other than Symbol and ZapfDingbats, by its name (`Helvetica-Bold`) or
    /// PyMuPDF's (`hebo`); `color` fills it (RGB, 0 to 1).
    Text {
        at: [f64; 2],
        text: String,
        font: String,
        size: f64,
        color: [f64; 3],
    },
    /// A straight line, `width` points wide.
    Line {
        from: [f64; 2],
        to: [f64; 2],
        width: f64,
        color: [f64; 3],
    },
    /// A rectangle (x1, y1, x2, y2), its border `width` points wide in `color`, filled with
    /// `fill`; one of them at least.
    Rectangle {
        rect: [f64; 4],
        width: f64,
        color: Option<[f64; 3]>,
        fill: Option<[f64; 3]>,
    },
}

/// The standard font named `name` (its name or PyMuPDF's), and its height in em.
fn text_font(name: &str) -> Option<(&'static str, f64)> {
    TEXT_FONTS
        .iter()
        .find(|(full, short, _)| *full == name || *short == name)
        .map(|(full, _, height)| (*full, *height))
}

fn invalid(what: &'static str) -> Error {
    Error::Pdf(sealwax_core::Error::InvalidArgument { what })
}

fn finite(values: &[f64]) -> bool {
    values.iter().all(|v| v.is_finite())
}

fn color(color: [f64; 3]) -> Result<String> {
    if !color.iter().all(|c| (0.0..=1.0).contains(c)) {
        return Err(invalid("color"));
    }
    Ok(color.map(number).join(" "))
}

/// A number as a content stream writes it: four decimals at most, no trailing zeros.
fn number(value: f64) -> String {
    let mut text = format!("{value:.4}");
    if text.contains('.') {
        let trimmed = text.trim_end_matches('0').trim_end_matches('.').len();
        text.truncate(trimmed);
    }
    if text == "-0" {
        text = "0".to_owned();
    }
    text
}

/// The page object and geometry of page `page_index`.
fn page(doc: &Document, page_index: usize) -> Result<(ObjId, Rect, sealwax_core::Rotation)> {
    let geometry = doc.geometry();
    match (doc.page_ids().get(page_index), geometry.get(page_index)) {
        (Some(Some(id)), Some(page)) => Ok((*id, page.crop_box, page.rotation)),
        _ => Err(invalid("page index")),
    }
}

/// Draw `shapes` on page `page_index` (from 0), in order, after its content; returns the
/// characters of text the font cannot show, drawn as `?`.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have, a value
/// that is not a number, a font that is not one of the standard fonts text is drawn in, a
/// size not between 0 and 1,000 points, a color component not between 0 and 1, a negative
/// width, a rectangle neither stroked nor filled; nothing is drawn then.
pub fn draw_shapes(doc: &mut Document, page_index: usize, shapes: &[Shape]) -> Result<usize> {
    let (page_id, crop_box, rotation) = page(doc, page_index)?;
    // Bytes: text is in WinAnsiEncoding.
    let mut drawing: Vec<u8> = Vec::new();
    let mut fonts: Vec<&'static str> = Vec::new();
    let mut replaced = 0;
    for shape in shapes {
        match shape {
            Shape::Text {
                at,
                text,
                font,
                size,
                color: fill,
            } => {
                let (font, height) = text_font(font).ok_or_else(|| invalid("font"))?;
                if !finite(at) || !(size.is_finite() && *size > 0.0 && *size <= 1000.0) {
                    return Err(invalid("text"));
                }
                let index = match fonts.iter().position(|f| *f == font) {
                    Some(index) => index,
                    None => {
                        fonts.push(font);
                        fonts.len() - 1
                    }
                };
                let _ = write!(
                    drawing,
                    "q {} rg BT /F{index} {} Tf",
                    color(*fill)?,
                    number(*size)
                );
                for (line, content) in text.split('\n').enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let y = at[1] + line as f64 * size * height;
                    let shown = literal(&encode(content.trim_end_matches('\r'), &mut replaced));
                    let _ = write!(drawing, "\n1 0 0 -1 {} {} Tm ", number(at[0]), number(y));
                    drawing.extend_from_slice(&shown);
                    drawing.extend_from_slice(b" Tj");
                }
                drawing.extend_from_slice(b" ET Q\n");
            }
            Shape::Line {
                from,
                to,
                width,
                color: stroke,
            } => {
                if !finite(from) || !finite(to) || !(width.is_finite() && *width >= 0.0) {
                    return Err(invalid("line"));
                }
                let _ = writeln!(
                    drawing,
                    "q {} w {} RG {} {} m {} {} l S Q",
                    number(*width),
                    color(*stroke)?,
                    number(from[0]),
                    number(from[1]),
                    number(to[0]),
                    number(to[1])
                );
            }
            Shape::Rectangle {
                rect,
                width,
                color: stroke,
                fill,
            } => {
                if !finite(rect) || !(width.is_finite() && *width >= 0.0) {
                    return Err(invalid("rectangle"));
                }
                let (operator, mut colors) = match (stroke, fill) {
                    (Some(_), Some(_)) => ("B", String::new()),
                    (Some(_), None) => ("S", String::new()),
                    (None, Some(_)) => ("f", String::new()),
                    (None, None) => return Err(invalid("rectangle")),
                };
                if let Some(stroke) = stroke {
                    let _ = write!(colors, "{} RG ", color(*stroke)?);
                }
                if let Some(fill) = fill {
                    let _ = write!(colors, "{} rg ", color(*fill)?);
                }
                let _ = writeln!(
                    drawing,
                    "q {} w {colors}{} {} {} {} re {operator} Q",
                    number(*width),
                    number(rect[0].min(rect[2])),
                    number(rect[1].min(rect[3])),
                    number((rect[2] - rect[0]).abs()),
                    number((rect[3] - rect[1]).abs())
                );
            }
        }
    }
    if drawing.is_empty() {
        return Ok(replaced);
    }
    let mut font_resources = Dict::new();
    for (index, font) in fonts.iter().enumerate() {
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("Font"));
        dict.insert("Subtype", Name::from("Type1"));
        dict.insert("BaseFont", Name::from(*font));
        dict.insert("Encoding", Name::from("WinAnsiEncoding"));
        font_resources.insert(format!("F{index}").as_str(), dict);
    }
    let mut resources = Dict::new();
    if !font_resources.is_empty() {
        resources.insert("Font", font_resources);
    }
    // The form covers the page as it shows, in the units it is drawn in.
    let (width, height) = if rotation.degrees() % 180 == 90 {
        (crop_box.height(), crop_box.width())
    } else {
        (crop_box.width(), crop_box.height())
    };
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("XObject"));
    dict.insert("Subtype", Name::from("Form"));
    dict.insert(
        "BBox",
        vec![0.0.into(), 0.0.into(), width.into(), height.into()],
    );
    dict.insert("Resources", resources);
    let form = doc.add_object(Object::Stream(Stream {
        dict,
        data: drawing,
    }))?;
    let placed = [Placed {
        xobject: form,
        matrix: shown_matrix(crop_box, rotation),
        alt: None,
    }];
    page::draw(doc, page_id, &placed, &mut pdf::HandedDown::default())?;
    Ok(replaced)
}

/// Add a link to `uri` over `rect` (x1, y1, x2, y2) on page `page_index` (from 0), without a
/// border.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have, a rectangle
/// without area, or an empty address.
pub fn add_link(doc: &mut Document, page_index: usize, rect: [f64; 4], uri: &str) -> Result<()> {
    let (page_id, crop_box, rotation) = page(doc, page_index)?;
    let area = shown_to_user(rect, crop_box, rotation).ok_or_else(|| invalid("rectangle"))?;
    if uri.is_empty() {
        return Err(invalid("address"));
    }
    let mut action = Dict::new();
    action.insert("S", Name::from("URI"));
    action.insert("URI", Object::String(uri.as_bytes().to_vec()));
    let mut link = Dict::new();
    link.insert("Type", Name::from("Annot"));
    link.insert("Subtype", Name::from("Link"));
    link.insert(
        "Rect",
        vec![
            area.x1.into(),
            area.y1.into(),
            area.x2.into(),
            area.y2.into(),
        ],
    );
    link.insert("Border", vec![0.into(), 0.into(), 0.into()]);
    link.insert("F", 4);
    link.insert("P", page_id);
    link.insert("A", action);
    let link = doc.add_object(Object::Dict(link))?;
    annotate::add_to_page(doc, page_id, link)
}
