//! Annotations flattened into page content.
//!
//! Each annotation that shows something is painted into its page at its rectangle, form widgets
//! with their current state; then every annotation but links, and the form, are taken out, so that
//! nothing stays editable. Links stay: they draw nothing, are not editable, and keep a document's
//! navigation.
//!
//! What the document had is painted, never dropped. A box or a radio button that is off shows its
//! `/Off` appearance, as in a viewer.

use std::collections::{BTreeMap, HashMap};

use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Result, decode_text_string};

use crate::draw::{self, BorderStyle, Frame, TextField};
use crate::generate;
use crate::page::{self, Placed};
use crate::pdf;

/// Annotation flags (ISO 32000-1, 12.5.3): a hidden or no-view annotation is never shown.
const HIDDEN: i64 = 1 << 1;
const NO_VIEW: i64 = 1 << 5;
/// Field flags (12.7.4).
const RADIO: i64 = 1 << 15;
const PUSH_BUTTON: i64 = 1 << 16;

/// What flattening did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Flattened {
    /// Appearances the document had, painted into their pages.
    pub painted: usize,
    /// Appearances drawn for annotations that had none (or a form that asked for new ones).
    pub drawn: usize,
    /// Links kept as annotations.
    pub links_kept: usize,
    /// Annotations taken out without anything painted, by subtype: no appearance and nothing
    /// this crate draws (a widget showing nothing, a hidden annotation, a state that is off, are
    /// not counted).
    pub dropped: BTreeMap<String, usize>,
    /// Characters of field values that WinAnsiEncoding does not have, drawn as `?`.
    pub characters_replaced: usize,
}

/// The kind of a form field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Text,
    Choice,
    CheckBox,
    Radio,
    Other,
}

/// A generated form's font.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Font {
    Helvetica,
    HelveticaBold,
    ZapfDingbats,
}

/// What an annotation's `/AP /N` is.
pub(crate) enum Normal {
    Stream(ObjId),
    States(Dict),
}

struct Painter {
    acro_form: Dict,
    need_appearances: bool,
    fonts: HashMap<Font, ObjId>,
    report: Flattened,
}

/// What a flattening paints into the pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every annotation but links.
    All,
    /// Every annotation but links and redaction marks, which stay to be applied.
    AllButRedactions,
    /// Form widgets only: the other annotations (comments, highlights, links) stay as they are.
    Fields,
}

/// Flatten every annotation of the document but links, and take its form out.
pub(crate) fn flatten(doc: &mut Document) -> Result<Flattened> {
    flatten_scope(doc, Scope::All)
}

/// Flatten the annotations `scope` names, and take the form out.
pub(crate) fn flatten_scope(doc: &mut Document, scope: Scope) -> Result<Flattened> {
    let catalog_id = doc.catalog_id();
    let acro_form = match doc.object(catalog_id)? {
        Some(Object::Dict(catalog)) => {
            pdf::dict(doc, catalog.get(b"AcroForm"))?.unwrap_or_default()
        }
        _ => Dict::new(),
    };
    let need_appearances = acro_form.get(b"NeedAppearances") == Some(&Object::Bool(true));
    let mut painter = Painter {
        acro_form,
        need_appearances,
        fonts: HashMap::new(),
        report: Flattened::default(),
    };
    let mut handed_down = pdf::HandedDown::default();
    for page_id in doc.page_ids().into_iter().flatten() {
        let Some(Object::Dict(mut page)) = doc.object(page_id)? else {
            continue;
        };
        let Some(annots) = page.get(b"Annots").cloned() else {
            continue;
        };
        doc.check_time()?;
        let items = match pdf::resolved(doc, &annots)? {
            Some(Object::Array(items)) => items,
            _ => Vec::new(),
        };
        if u64::try_from(items.len()).unwrap_or(u64::MAX) > doc.limits().max_annotations {
            return Err(sealwax_core::Error::LimitExceeded {
                what: "max_annotations",
            });
        }
        let mut kept = Vec::new();
        let mut placed = Vec::new();
        for item in items {
            let Some(Object::Dict(annotation)) = pdf::resolved(doc, &item)? else {
                continue;
            };
            match annotation.get(b"Subtype").and_then(Object::as_name) {
                Some(b"Link") => {
                    kept.push(item);
                    painter.report.links_kept += 1;
                }
                Some(subtype) if scope == Scope::Fields && subtype != b"Widget" => {
                    kept.push(item);
                }
                Some(b"Redact") if scope == Scope::AllButRedactions => kept.push(item),
                Some(b"Popup") => {}
                _ => {
                    if let Some(placement) = painter.placement(doc, &annotation)? {
                        placed.push(placement);
                    }
                }
            }
        }
        if kept.is_empty() {
            page.remove(b"Annots");
        } else {
            page.insert("Annots", Object::Array(kept));
        }
        page::store(doc, page_id, page);
        page::draw(doc, page_id, &placed, &mut handed_down)?;
    }
    // The catalog is rewritten only when it has something to lose: on a damaged file, the one
    // read now can differ from the one the pages were found from.
    if let Some(Object::Dict(mut catalog)) = doc.object(catalog_id)?
        && (catalog.contains_key(b"AcroForm") || catalog.contains_key(b"Perms"))
    {
        catalog.remove(b"AcroForm");
        // The signature permissions point at signature fields that no longer exist.
        catalog.remove(b"Perms");
        doc.set_object(catalog_id, Object::Dict(catalog));
    }
    Ok(painter.report)
}

/// The annotations a page lists, resolved: `None` for a page without `/Annots`. Refused past
/// `max_annotations`, as flattening refuses them.
fn page_annotations(doc: &Document, page: &Dict) -> Result<Option<Vec<(Object, Dict)>>> {
    let Some(annots) = page.get(b"Annots") else {
        return Ok(None);
    };
    let items = match pdf::resolved(doc, annots)? {
        Some(Object::Array(items)) => items,
        _ => Vec::new(),
    };
    if u64::try_from(items.len()).unwrap_or(u64::MAX) > doc.limits().max_annotations {
        return Err(sealwax_core::Error::LimitExceeded {
            what: "max_annotations",
        });
    }
    let mut found = Vec::with_capacity(items.len());
    for item in items {
        if let Some(Object::Dict(annotation)) = pdf::resolved(doc, &item)? {
            found.push((item, annotation));
        }
    }
    Ok(Some(found))
}

/// The pages (from 0) with an annotation other than a link: those flattening or
/// [`remove_annotations`] changes.
pub(crate) fn annotated_pages(doc: &Document) -> Result<Vec<usize>> {
    let mut found = Vec::new();
    for (index, page_id) in doc.page_ids().into_iter().enumerate() {
        let Some(page_id) = page_id else {
            continue;
        };
        let Some(Object::Dict(page)) = doc.object(page_id)? else {
            continue;
        };
        let annotations = page_annotations(doc, &page)?.unwrap_or_default();
        if annotations
            .iter()
            .any(|(_, a)| a.get(b"Subtype").and_then(Object::as_name) != Some(b"Link"))
        {
            found.push(index);
        }
    }
    Ok(found)
}

/// Take every annotation but links out of the pages, nothing painted, and the form with them:
/// what the pages show is their content alone. Returns how many were taken out (their popups
/// not counted).
pub(crate) fn remove_annotations(doc: &mut Document) -> Result<usize> {
    let mut removed = 0;
    for page_id in doc.page_ids().into_iter().flatten() {
        let Some(Object::Dict(mut page)) = doc.object(page_id)? else {
            continue;
        };
        doc.check_time()?;
        let Some(annotations) = page_annotations(doc, &page)? else {
            continue;
        };
        let mut kept = Vec::new();
        for (item, annotation) in annotations {
            match annotation.get(b"Subtype").and_then(Object::as_name) {
                Some(b"Link") => kept.push(item),
                Some(b"Popup") => {}
                _ => removed += 1,
            }
        }
        if kept.is_empty() {
            page.remove(b"Annots");
        } else {
            page.insert("Annots", Object::Array(kept));
        }
        page::store(doc, page_id, page);
    }
    let catalog_id = doc.catalog_id();
    if let Some(Object::Dict(mut catalog)) = doc.object(catalog_id)?
        && (catalog.contains_key(b"AcroForm") || catalog.contains_key(b"Perms"))
    {
        catalog.remove(b"AcroForm");
        catalog.remove(b"Perms");
        doc.set_object(catalog_id, Object::Dict(catalog));
    }
    Ok(removed)
}

impl Painter {
    /// The appearance to paint and the matrix that puts it on the annotation's rectangle.
    fn placement(&mut self, doc: &mut Document, annotation: &Dict) -> Result<Option<Placed>> {
        let flags = annotation.get(b"F").and_then(Object::as_i64).unwrap_or(0);
        if flags & (HIDDEN | NO_VIEW) != 0 {
            return Ok(None);
        }
        let Some(rect) = pdf::rect(annotation.get(b"Rect")) else {
            return Ok(None);
        };
        let Some((appearance, generated, target)) = self.appearance(doc, annotation, rect)? else {
            return Ok(None);
        };
        let rect = target.unwrap_or(rect);
        let Some(Object::Stream(mut stream)) = doc.object(appearance)? else {
            return Ok(None);
        };
        let Some(bbox) = pdf::rect(stream.dict.get(b"BBox")) else {
            return Ok(None);
        };
        let matrix = stream
            .dict
            .get(b"Matrix")
            .and_then(pdf::number_array)
            .and_then(|m| <[f64; 6]>::try_from(m).ok())
            .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let Some(placement) = placement_matrix(bbox, matrix, rect) else {
            return Ok(None);
        };
        // Painted with `Do`, the appearance must say it is a form XObject.
        if stream.dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Form")
            || stream.dict.get(b"Type").and_then(Object::as_name) != Some(b"XObject")
        {
            stream.dict.insert("Type", Name::from("XObject"));
            stream.dict.insert("Subtype", Name::from("Form"));
            doc.set_object(appearance, Object::Stream(stream));
        }
        if generated {
            self.report.drawn += 1;
        } else {
            self.report.painted += 1;
        }
        Ok(Some(Placed {
            xobject: appearance,
            matrix: placement,
            alt: None,
        }))
    }

    /// The appearance an annotation shows, whether it was drawn here, and the rectangle it goes
    /// on when that is not `/Rect`.
    fn appearance(
        &mut self,
        doc: &mut Document,
        annotation: &Dict,
        rect: Rect,
    ) -> Result<Option<(ObjId, bool, Option<Rect>)>> {
        let normal = normal_appearance(doc, annotation)?;
        let subtype = annotation
            .get(b"Subtype")
            .and_then(Object::as_name)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let widget = subtype == b"Widget";
        let (width, height) = (rect.x2 - rect.x1, rect.y2 - rect.y1);
        if widget {
            let kind = kind(doc, annotation)?;
            if matches!(kind, Kind::Text | Kind::Choice)
                && (normal.is_none() || self.need_appearances)
            {
                let field = self.text_field(doc, annotation, kind == Kind::Choice)?;
                let (drawing, replaced) = draw::text_field(&field, width, height);
                self.report.characters_replaced += replaced;
                let form = self.form(doc, &drawing, width, height, Some(Font::Helvetica))?;
                return Ok(Some((form, true, None)));
            }
            if kind == Kind::CheckBox && normal.is_none() {
                let checked = checked(doc, annotation)?;
                let drawing = draw::check_box(checked, &frame(doc, annotation)?, width, height);
                let form = self.form(doc, &drawing, width, height, Some(Font::ZapfDingbats))?;
                return Ok(Some((form, true, None)));
            }
        }
        match normal {
            Some(Normal::States(states)) => {
                // The appearance of the current state, `/Off` included: an unchecked box shows its
                // frame, as in a viewer. Without `/AS`, the field's value names the state.
                let state = match annotation.get(b"AS").and_then(Object::as_name) {
                    Some(state) => Some(state.to_vec()),
                    None => pdf::inherited(doc, annotation, b"V")?
                        .as_ref()
                        .and_then(Object::as_name)
                        .map(<[u8]>::to_vec),
                };
                let Some(state) = state else {
                    return Ok(None);
                };
                match states.get(&state) {
                    Some(Object::Ref(id))
                        if matches!(doc.object(*id)?, Some(Object::Stream(_))) =>
                    {
                        Ok(Some((*id, false, None)))
                    }
                    _ => Ok(None),
                }
            }
            Some(Normal::Stream(id)) => Ok(Some((id, false, None))),
            None if subtype == b"FreeText" => {
                let free = self.free_text(doc, annotation, rect)?;
                let (drawing, replaced) = draw::free_text(&free, width, height);
                self.report.characters_replaced += replaced;
                let form = self.form(doc, &drawing, width, height, Some(Font::Helvetica))?;
                Ok(Some((form, true, None)))
            }
            None => match generate::generate(doc, annotation, &subtype)? {
                Some(generated) => {
                    let mut dict = Dict::new();
                    dict.insert("Type", Name::from("XObject"));
                    dict.insert("Subtype", Name::from("Form"));
                    let r = generated.rect;
                    dict.insert("BBox", pdf::numbers(&[r.x1, r.y1, r.x2, r.y2]));
                    dict.insert("Resources", generated.resources);
                    let form = doc.add_object(pdf::flate(dict, &generated.drawing))?;
                    Ok(Some((form, true, Some(generated.rect))))
                }
                None => {
                    if !widget {
                        let name = String::from_utf8_lossy(&subtype).into_owned();
                        *self.report.dropped.entry(name).or_default() += 1;
                    }
                    Ok(None)
                }
            },
        }
    }

    /// A form XObject `width` by `height` with `drawing`, and the font it uses.
    fn form(
        &mut self,
        doc: &mut Document,
        drawing: &[u8],
        width: f64,
        height: f64,
        font: Option<Font>,
    ) -> Result<ObjId> {
        let mut resources = Dict::new();
        if let Some(font) = font {
            let id = self.font(doc, font)?;
            let mut fonts = Dict::new();
            let name = match font {
                Font::Helvetica => "Helv",
                Font::HelveticaBold => "HeBo",
                Font::ZapfDingbats => "ZaDb",
            };
            fonts.insert(name, Object::Ref(id));
            resources.insert("Font", fonts);
        }
        doc.add_object(form_stream(drawing, width, height, resources))
    }

    fn font(&mut self, doc: &mut Document, font: Font) -> Result<ObjId> {
        if let Some(id) = self.fonts.get(&font) {
            return Ok(*id);
        }
        let id = doc.add_object(font_dict(font))?;
        self.fonts.insert(font, id);
        Ok(id)
    }

    /// A free text annotation without appearance, to draw as the XFDF's are drawn: its
    /// `/Contents` in Helvetica at the size and colour of its `/DA` (or the form's), aligned by
    /// `/Q`, from the top, its background `/C` and its border (`/BS`, 1 by default) in the text's
    /// colour, as PDFium draws them.
    fn free_text(
        &self,
        doc: &Document,
        annotation: &Dict,
        rect: Rect,
    ) -> Result<crate::xfdf::Text> {
        let da = match annotation.get(b"DA") {
            Some(value) => pdf::resolved(doc, value)?,
            None => match self.acro_form.get(b"DA") {
                Some(value) => pdf::resolved(doc, value)?,
                None => None,
            },
        };
        let da = match da {
            Some(Object::String(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
            _ => String::new(),
        };
        let appearance = crate::xfdf::default_appearance(&da);
        let contents = match annotation.get(b"Contents") {
            Some(value) => match pdf::resolved(doc, value)? {
                Some(Object::String(bytes)) => decode_text_string(&bytes),
                _ => String::new(),
            },
            None => String::new(),
        };
        let align = match annotation.get(b"Q").and_then(Object::as_i64) {
            Some(1) => crate::xfdf::Align::Center,
            Some(2) => crate::xfdf::Align::Right,
            _ => crate::xfdf::Align::Left,
        };
        let border = border_width(doc, annotation)?;
        let color = appearance.color.unwrap_or_else(|| vec![0.0]);
        Ok(crate::xfdf::Text {
            page_number: 0,
            rect,
            contents,
            lines: None,
            size: appearance.size.filter(|s| *s > 0.0).unwrap_or(12.0),
            border_color: Some(color.clone()),
            color,
            align,
            vertical: crate::xfdf::VerticalAlign::Top,
            border,
            fill: colour(doc, annotation.get(b"C"))?,
            rotation: 0,
            font: None,
        })
    }

    /// What a text or choice field shows, from the field and the form.
    fn text_field(&self, doc: &Document, widget: &Dict, choice: bool) -> Result<TextField> {
        text_field(doc, &self.acro_form, widget, choice)
    }
}

/// What a text or choice field shows, from the field and the form `acro_form`: its value (a
/// choice's export values by their display text), the font size and colour of its `/DA`, its
/// alignment, flags, maximum length and frame.
pub(crate) fn text_field(
    doc: &Document,
    acro_form: &Dict,
    widget: &Dict,
    choice: bool,
) -> Result<TextField> {
    let da = match pdf::inherited(doc, widget, b"DA")? {
        Some(Object::String(da)) => Some(da),
        _ => match acro_form.get(b"DA") {
            Some(value) => match pdf::resolved(doc, value)? {
                Some(Object::String(da)) => Some(da),
                _ => None,
            },
            None => None,
        },
    };
    let appearance =
        crate::xfdf::default_appearance(&String::from_utf8_lossy(&da.unwrap_or_default()));
    let quadding = match pdf::inherited(doc, widget, b"Q")? {
        Some(value) => value.as_i64(),
        None => acro_form.get(b"Q").and_then(Object::as_i64),
    }
    .unwrap_or(0);
    Ok(TextField {
        lines: value_lines(doc, widget)?,
        choice,
        flags: pdf::inherited(doc, widget, b"Ff")?
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        size: appearance.size.unwrap_or(0.0),
        color: appearance.color,
        quadding,
        max_length: pdf::inherited(doc, widget, b"MaxLen")?.and_then(|v| v.as_i64()),
        frame: frame(doc, widget)?,
    })
}

/// A form XObject stream.
pub(crate) fn form_stream(drawing: &[u8], width: f64, height: f64, resources: Dict) -> Object {
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("XObject"));
    dict.insert("Subtype", Name::from("Form"));
    dict.insert("BBox", pdf::numbers(&[0.0, 0.0, width, height]));
    dict.insert("Resources", resources);
    pdf::flate(dict, drawing)
}

/// A form XObject stream drawn in a frame turned `turns` quarter turns counterclockwise (a
/// widget's `/MK /R`): its `/Matrix` turns it back onto the widget.
pub(crate) fn turned_form_stream(
    drawing: &[u8],
    width: f64,
    height: f64,
    resources: Dict,
    turns: i64,
) -> Object {
    let mut form = form_stream(drawing, width, height, resources);
    let matrix: Option<[f64; 6]> = match turns {
        1 => Some([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
        2 => Some([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
        3 => Some([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
        _ => None,
    };
    if let (Some(matrix), Object::Stream(stream)) = (matrix, &mut form) {
        stream.dict.insert("Matrix", pdf::numbers(&matrix));
    }
    form
}

/// The quarter turns counterclockwise a widget's `/MK /R` gives it (0 to 3).
pub(crate) fn widget_turns(doc: &Document, widget: &Dict) -> Result<i64> {
    let characteristics = pdf::dict(doc, widget.get(b"MK"))?.unwrap_or_default();
    Ok(characteristics
        .get(b"R")
        .and_then(Object::as_i64)
        .map_or(0, |degrees| (degrees / 90).rem_euclid(4)))
}

/// A standard font, not embedded: the PDF/A conversion embeds its program.
pub(crate) fn font_dict(font: Font) -> Object {
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("Font"));
    dict.insert("Subtype", Name::from("Type1"));
    match font {
        Font::Helvetica => {
            dict.insert("BaseFont", Name::from("Helvetica"));
            dict.insert("Encoding", Name::from("WinAnsiEncoding"));
        }
        Font::HelveticaBold => {
            dict.insert("BaseFont", Name::from("Helvetica-Bold"));
            dict.insert("Encoding", Name::from("WinAnsiEncoding"));
        }
        Font::ZapfDingbats => {
            dict.insert("BaseFont", Name::from("ZapfDingbats"));
        }
    }
    Object::Dict(dict)
}

/// The Helvetica font of the forms and texts drawn into pages.
pub(crate) fn helvetica() -> Object {
    font_dict(Font::Helvetica)
}

/// The matrix A of ISO 32000-1, 12.5.5: the form's `/BBox`, transformed by its `/Matrix`, onto
/// the annotation's `/Rect`. The page's content is in default user space, unrotated, as `/Rect`
/// is: the page's `/Rotate` and crop box apply to the painted appearance as they applied to the
/// annotation.
fn placement_matrix(bbox: Rect, matrix: [f64; 6], rect: Rect) -> Option<[f64; 6]> {
    let [a, b, c, d, e, f] = matrix;
    let corners = [
        (bbox.x1, bbox.y1),
        (bbox.x1, bbox.y2),
        (bbox.x2, bbox.y1),
        (bbox.x2, bbox.y2),
    ]
    .map(|(x, y)| (x * a + y * c + e, x * b + y * d + f));
    let left = corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let right = corners
        .iter()
        .map(|p| p.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let top = corners
        .iter()
        .map(|p| p.1)
        .fold(f64::NEG_INFINITY, f64::max);
    if !(right - left > 0.0 && top - bottom > 0.0) {
        return None;
    }
    let scale_x = (rect.x2 - rect.x1) / (right - left);
    let scale_y = (rect.y2 - rect.y1) / (top - bottom);
    let placement = [
        scale_x,
        0.0,
        0.0,
        scale_y,
        rect.x1 - left * scale_x,
        rect.y1 - bottom * scale_y,
    ];
    placement.iter().all(|v| v.is_finite()).then_some(placement)
}

pub(crate) fn normal_appearance(doc: &Document, annotation: &Dict) -> Result<Option<Normal>> {
    let Some(appearances) = pdf::dict(doc, annotation.get(b"AP"))? else {
        return Ok(None);
    };
    Ok(match appearances.get(b"N") {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Stream(_)) => Some(Normal::Stream(*id)),
            Some(Object::Dict(states)) => Some(Normal::States(states)),
            _ => None,
        },
        Some(Object::Dict(states)) => Some(Normal::States(states.clone())),
        _ => None,
    })
}

pub(crate) fn kind(doc: &Document, widget: &Dict) -> Result<Kind> {
    let field_type = pdf::inherited(doc, widget, b"FT")?;
    Ok(match field_type.as_ref().and_then(Object::as_name) {
        Some(b"Btn") => {
            let flags = pdf::inherited(doc, widget, b"Ff")?
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if flags & PUSH_BUTTON != 0 {
                Kind::Other
            } else if flags & RADIO != 0 {
                Kind::Radio
            } else {
                Kind::CheckBox
            }
        }
        Some(b"Tx") => Kind::Text,
        Some(b"Ch") => Kind::Choice,
        _ => Kind::Other,
    })
}

/// A check box is checked when its `/AS` is not `/Off`, or, without `/AS`, when its value is
/// not.
fn checked(doc: &Document, widget: &Dict) -> Result<bool> {
    if let Some(state) = widget.get(b"AS") {
        return Ok(state.as_name() != Some(b"Off"));
    }
    Ok(match pdf::inherited(doc, widget, b"V")? {
        Some(Object::Name(name)) => name.as_bytes() != b"Off",
        Some(Object::Null) | None => false,
        Some(_) => true,
    })
}

/// What a text or choice field shows: its value, a choice's export values by their display text.
fn value_lines(doc: &Document, widget: &Dict) -> Result<Vec<String>> {
    let as_text = |value: &Object| -> Option<String> {
        match value {
            Object::String(bytes) => Some(decode_text_string(bytes)),
            Object::Name(name) => Some(String::from_utf8_lossy(name.as_bytes()).into_owned()),
            _ => None,
        }
    };
    let values: Vec<String> = match pdf::inherited(doc, widget, b"V")? {
        Some(Object::Array(items)) => {
            let mut values = Vec::new();
            for item in &items {
                if let Some(text) = pdf::resolved(doc, item)?.as_ref().and_then(as_text) {
                    values.push(text);
                }
            }
            values
        }
        Some(value) => as_text(&value).into_iter().collect(),
        None => Vec::new(),
    };
    let mut shown: HashMap<String, String> = HashMap::new();
    if let Some(Object::Array(options)) = pdf::inherited(doc, widget, b"Opt")? {
        for option in &options {
            if let Some(Object::Array(pair)) = pdf::resolved(doc, option)?
                && let [export, display] = pair.as_slice()
                && let (Some(export), Some(display)) = (
                    pdf::resolved(doc, export)?.as_ref().and_then(as_text),
                    pdf::resolved(doc, display)?.as_ref().and_then(as_text),
                )
            {
                shown.insert(export, display);
            }
        }
    }
    Ok(values
        .into_iter()
        .map(|value| shown.get(&value).cloned().unwrap_or(value))
        .collect())
}

/// The components of a colour array, `None` for anything else.
fn colour(doc: &Document, value: Option<&Object>) -> Result<Option<Vec<f64>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    Ok(pdf::resolved(doc, value)?
        .as_ref()
        .and_then(pdf::number_array)
        .filter(|c| matches!(c.len(), 1 | 3 | 4)))
}

/// The border width of `/BS`, 1 by default.
fn border_width(doc: &Document, annotation: &Dict) -> Result<f64> {
    Ok(pdf::dict(doc, annotation.get(b"BS"))?
        .and_then(|style| style.get(b"W").and_then(Object::as_f64))
        .filter(|w| w.is_finite() && *w >= 0.0)
        .unwrap_or(1.0))
}

fn frame(doc: &Document, widget: &Dict) -> Result<Frame> {
    let characteristics = pdf::dict(doc, widget.get(b"MK"))?.unwrap_or_default();
    let style = match pdf::dict(doc, widget.get(b"BS"))? {
        Some(style) => match style.get(b"S").and_then(Object::as_name) {
            Some(b"D") => {
                let dash = match style.get(b"D") {
                    Some(value) => pdf::resolved(doc, value)?
                        .as_ref()
                        .and_then(pdf::number_array)
                        .unwrap_or_else(|| vec![3.0]),
                    None => vec![3.0],
                };
                BorderStyle::Dashed(dash)
            }
            Some(b"B") => BorderStyle::Beveled,
            Some(b"I") => BorderStyle::Inset,
            Some(b"U") => BorderStyle::Underline,
            _ => BorderStyle::Solid,
        },
        None => BorderStyle::Solid,
    };
    Ok(Frame {
        background: colour(doc, characteristics.get(b"BG"))?,
        border: colour(doc, characteristics.get(b"BC"))?,
        border_width: border_width(doc, widget)?,
        style,
    })
}
