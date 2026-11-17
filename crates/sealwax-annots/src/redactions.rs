//! Redaction marks applied (`/Redact`, ISO 32000-1, 12.5.6.23): what the pages draw under them
//! taken out, their overlay painted, the marks gone.

use sealwax_core::{Dict, Document, Object, Rect};
use sealwax_text::{RedactOptions, Redacted, redact};

use crate::Result;
use crate::draw;
use crate::flatten::{self, Font, Scope, font_dict, form_stream};
use crate::page::{self, Placed};
use crate::pdf;
use crate::xfdf::{Align, Text, VerticalAlign};

/// What [`apply_redactions`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Redactions {
    /// Redaction marks applied.
    pub marks: usize,
    /// Pages that had some.
    pub pages: usize,
    /// Links under a mark, taken out with it (their target could tell what was redacted).
    pub links_removed: usize,
    /// What was taken out of the pages' content, all pages counted.
    pub redacted: Redacted,
}

/// A redaction mark, read.
struct Mark {
    /// The areas it covers: its quadrilaterals' boxes, else its rectangle.
    areas: Vec<Rect>,
    /// `/IC`: the colour the areas are painted with once the content is out.
    fill: Option<Vec<f64>>,
    /// `/OverlayText`, and the size its `/DA` gives.
    overlay: Option<(String, f64)>,
}

/// Apply every redaction mark of the document, as a reader applies them:
/// 1. every annotation but links and the marks is flattened into its page first, the form taken
///    out (a field's value under a mark is page content, then, and goes with it);
/// 2. what each page draws under its marks is taken out (see [`redact`]: glyphs, images' pixels,
///    paths, as `options` asks);
/// 3. each mark's areas are painted with its interior colour (`/IC`), when it has one, and its
///    overlay text written in them;
/// 4. the marks are taken out, and the links under them.
///
/// The document still holds what the pages no longer reach (an image replaced, the content
/// before): follow with [`Document::rebuilt_for_redaction`] and a full save.
///
/// # Errors
/// As [`redact`] and flattening.
pub fn apply_redactions(doc: &mut Document, options: &RedactOptions) -> Result<Redactions> {
    let mut applied = Redactions::default();
    let pages = doc.page_ids();
    let mut marked = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let Some(page) = page else {
            continue;
        };
        let marks = marks(doc, *page)?;
        if !marks.is_empty() {
            marked.push((index, *page, marks));
        }
    }
    if marked.is_empty() {
        return Ok(applied);
    }
    flatten::flatten_scope(doc, Scope::AllButRedactions)?;
    let mut handed_down = pdf::HandedDown::default();
    for (index, page_id, marks) in marked {
        doc.check_time()?;
        let areas: Vec<Rect> = marks.iter().flat_map(|m| m.areas.iter().copied()).collect();
        let done = redact(doc, index, &areas, options)?;
        let total = &mut applied.redacted;
        total.glyphs += done.glyphs;
        total.operators += done.operators;
        total.forms += done.forms;
        total.strings_unread += done.strings_unread;
        total.images += done.images;
        total.images_removed += done.images_removed;
        total.paths += done.paths;
        applied.links_removed += remove_marks(doc, page_id, &areas)?;
        let mut placed = Vec::new();
        for mark in &marks {
            for area in &mark.areas {
                if let Some(overlay) = overlay(doc, mark, *area)? {
                    placed.push(overlay);
                }
            }
        }
        page::draw(doc, page_id, &placed, &mut handed_down)?;
        applied.marks += marks.len();
        applied.pages += 1;
    }
    Ok(applied)
}

/// The redaction marks of a page.
fn marks(doc: &Document, page: sealwax_core::ObjId) -> Result<Vec<Mark>> {
    let Some(Object::Dict(dict)) = doc.object(page)? else {
        return Ok(Vec::new());
    };
    let items = match dict.get(b"Annots") {
        Some(annots) => match pdf::resolved(doc, annots)? {
            Some(Object::Array(items)) => items,
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    let mut found = Vec::new();
    for item in items {
        let Some(Object::Dict(annotation)) = pdf::resolved(doc, &item)? else {
            continue;
        };
        if annotation.get(b"Subtype").and_then(Object::as_name) != Some(b"Redact") {
            continue;
        }
        let quads = annotation
            .get(b"QuadPoints")
            .and_then(pdf::number_array)
            .unwrap_or_default();
        let mut areas: Vec<Rect> = quads
            .as_chunks::<8>()
            .0
            .iter()
            .filter_map(|q| {
                let xs = [q[0], q[2], q[4], q[6]];
                let ys = [q[1], q[3], q[5], q[7]];
                let rect = Rect::from_corners(
                    xs.iter().copied().fold(f64::MAX, f64::min),
                    ys.iter().copied().fold(f64::MAX, f64::min),
                    xs.iter().copied().fold(f64::MIN, f64::max),
                    ys.iter().copied().fold(f64::MIN, f64::max),
                );
                (rect.x2 > rect.x1 && rect.y2 > rect.y1).then_some(rect)
            })
            .collect();
        if areas.is_empty()
            && let Some(rect) = pdf::rect(annotation.get(b"Rect"))
        {
            areas.push(rect);
        }
        if areas.is_empty() {
            continue;
        }
        let fill = annotation
            .get(b"IC")
            .and_then(pdf::number_array)
            .filter(|c| matches!(c.len(), 1 | 3 | 4));
        let overlay = match annotation.get(b"OverlayText") {
            Some(Object::String(bytes)) => {
                let size = annotation
                    .get(b"DA")
                    .and_then(Object::as_string)
                    .and_then(font_size)
                    .filter(|size| *size > 0.0)
                    .unwrap_or(10.0);
                Some((sealwax_core::decode_text_string(bytes), size))
            }
            _ => None,
        };
        found.push(Mark {
            areas,
            fill,
            overlay,
        });
    }
    Ok(found)
}

/// The size `Tf` sets in a default appearance string.
fn font_size(da: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(da).ok()?;
    let words: Vec<&str> = text.split_ascii_whitespace().collect();
    let at = words.iter().position(|w| *w == "Tf")?;
    words.get(at.checked_sub(1)?)?.parse().ok()
}

/// The marks taken out of the page's annotations, and the links under an area; how many links.
fn remove_marks(doc: &mut Document, page: sealwax_core::ObjId, areas: &[Rect]) -> Result<usize> {
    let Some(Object::Dict(mut dict)) = doc.object(page)? else {
        return Ok(0);
    };
    let Some(annots) = dict.get(b"Annots").cloned() else {
        return Ok(0);
    };
    let items = match pdf::resolved(doc, &annots)? {
        Some(Object::Array(items)) => items,
        _ => return Ok(0),
    };
    let mut kept = Vec::new();
    let mut links = 0;
    for item in items {
        let Some(Object::Dict(annotation)) = pdf::resolved(doc, &item)? else {
            continue;
        };
        match annotation.get(b"Subtype").and_then(Object::as_name) {
            Some(b"Redact" | b"Popup") => {}
            Some(b"Link")
                if pdf::rect(annotation.get(b"Rect")).is_some_and(|rect| {
                    areas.iter().any(|area| {
                        rect.x1 < area.x2
                            && rect.x2 > area.x1
                            && rect.y1 < area.y2
                            && rect.y2 > area.y1
                    })
                }) =>
            {
                links += 1;
            }
            _ => kept.push(item),
        }
    }
    if kept.is_empty() {
        dict.remove(b"Annots");
    } else {
        dict.insert("Annots", Object::Array(kept));
    }
    doc.set_object(page, Object::Dict(dict));
    Ok(links)
}

/// What paints a mark's area once its content is out: its interior colour, its overlay text.
fn overlay(doc: &mut Document, mark: &Mark, area: Rect) -> Result<Option<Placed>> {
    if mark.fill.is_none() && mark.overlay.is_none() {
        return Ok(None);
    }
    let (width, height) = (area.x2 - area.x1, area.y2 - area.y1);
    let mut drawing = Vec::new();
    if let Some(fill) = &mark.fill
        && let Some(color) = draw::color(fill, false)
    {
        drawing.extend_from_slice(format!("q {color} 0 0 {width} {height} re f Q ").as_bytes());
    }
    let mut resources = Dict::new();
    if let Some((text, size)) = &mark.overlay {
        let free = Text {
            page_number: 1,
            rect: area,
            contents: text.clone(),
            lines: None,
            size: *size,
            color: vec![0.0],
            align: Align::Left,
            vertical: VerticalAlign::Top,
            border: 0.0,
            border_color: None,
            fill: None,
        };
        drawing.extend_from_slice(&draw::free_text(&free, width, height).0);
        let mut fonts = Dict::new();
        fonts.insert(
            "Helv",
            Object::Ref(doc.add_object(font_dict(Font::Helvetica))?),
        );
        resources.insert("Font", fonts);
    }
    let form = doc.add_object(form_stream(&drawing, width, height, resources))?;
    Ok(Some(Placed {
        xobject: form,
        matrix: [1.0, 0.0, 0.0, 1.0, area.x1, area.y1],
        alt: None,
    }))
}
