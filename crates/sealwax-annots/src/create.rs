//! Form fields created on a page: a text field, a date field, a check box, each with its
//! appearance drawn, placed where the page shows it.

use std::collections::HashMap;

use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Rotation, encode_text_string};

use crate::fields::{FillValue, fields, fill_fields};
use crate::flatten::{Font, font_dict, turned_form_stream};
use crate::pdf;
use crate::{Error, Result};

/// The form's default appearance of text, as fields created take it: Helvetica, its size fitted
/// to the field, black.
const TEXT_APPEARANCE: &[u8] = b"/Helv 0 Tf 0 g";
/// A check box's: ZapfDingbats, whose `4` is the check mark.
const CHECK_APPEARANCE: &[u8] = b"/ZaDb 0 Tf 0 g";
/// The border of a field created: gray, one point wide, no background, so that a line printed
/// under the field still shows.
const BORDER_GRAY: f64 = 0.5;

/// What a field created is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NewFieldKind {
    /// A text field.
    Text,
    /// A text field that takes a date, formatted `yyyy-mm-dd` by a reader that runs the
    /// Acrobat date actions it carries.
    Date,
    /// A check box, whose states are `/Yes` and `/Off`.
    CheckBox,
}

/// A field to create ([`add_field`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct NewField {
    /// Its name: not empty, without a period (a period separates the parts of a full name).
    pub name: String,
    /// Text, date or check box.
    pub kind: NewFieldKind,
    /// The page, from 0.
    pub page_index: usize,
    /// `[x1, y1, x2, y2]` on the page as it shows: turned by its rotation, from the top-left
    /// corner of its crop box, y down, in points.
    pub rect: [f64; 4],
    /// The field must be filled (`Required`).
    pub required: bool,
}

impl NewField {
    /// A field of `kind` named `name`, at `rect` of page `page_index` as it shows, not required.
    #[must_use]
    pub fn new(name: &str, kind: NewFieldKind, page_index: usize, rect: [f64; 4]) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            page_index,
            rect,
            required: false,
        }
    }

    /// Required or not.
    #[must_use]
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }
}

/// Create a field on its page: a widget with its appearance (a gray border, one point wide, no
/// background; a check box's check mark), at the place `field.rect` gives on the page as it
/// shows (its rotation applied, its crop box's top-left corner the origin), turned with the
/// page so that its text reads upright there (`/MK /R`). The form is created if the document has
/// none, with a default appearance and its fonts; the fields it has are left as they are.
/// Returns the widget.
///
/// # Errors
/// [`sealwax_core::Error::FieldNameTaken`] when the form already has a field of that name (or
/// fields under it); [`sealwax_core::Error::InvalidArgument`] for an empty name or one with a
/// period, a page the document does not have, or a rectangle without area.
pub fn add_field(doc: &mut Document, field: &NewField) -> Result<ObjId> {
    if field.name.is_empty() || field.name.contains('.') {
        return Err(sealwax_core::Error::InvalidArgument { what: "field name" }.into());
    }
    let mut names: Vec<String> = fields(doc, None)?.into_iter().map(|f| f.name).collect();
    names.extend(form_names(doc)?);
    let taken = names.iter().any(|existing| {
        *existing == field.name
            || existing
                .strip_prefix(&field.name)
                .is_some_and(|rest| rest.starts_with('.'))
    });
    if taken {
        return Err(sealwax_core::Error::FieldNameTaken.into());
    }
    let Some(Some(page)) = doc.page_ids().get(field.page_index).copied() else {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" }.into());
    };
    let geometry = doc.geometry()[field.page_index];
    let rect = shown_to_user(field.rect, geometry.crop_box, geometry.rotation).ok_or(
        sealwax_core::Error::InvalidArgument {
            what: "field rectangle",
        },
    )?;
    let fonts = form_fonts(doc)?;
    let turns = i64::from(geometry.rotation.degrees() / 90);
    let mut widget = Dict::new();
    widget.insert("Type", Name::from("Annot"));
    widget.insert("Subtype", Name::from("Widget"));
    widget.insert("T", Object::String(encode_text_string(&field.name)));
    widget.insert("Rect", pdf::numbers(&[rect.x1, rect.y1, rect.x2, rect.y2]));
    widget.insert("P", Object::Ref(page));
    // Printed.
    widget.insert("F", 4_i64);
    if field.required {
        widget.insert("Ff", 2_i64);
    }
    let mut characteristics = Dict::new();
    characteristics.insert("BC", pdf::numbers(&[BORDER_GRAY; 3]));
    if turns != 0 {
        characteristics.insert("R", turns * 90);
    }
    let mut border = Dict::new();
    border.insert("W", 1_i64);
    border.insert("S", Name::from("S"));
    widget.insert("BS", border);
    let (mut width, mut height) = (rect.x2 - rect.x1, rect.y2 - rect.y1);
    if turns % 2 == 1 {
        (width, height) = (height, width);
    }
    match field.kind {
        NewFieldKind::Text | NewFieldKind::Date => {
            widget.insert("FT", Name::from("Tx"));
            widget.insert("DA", Object::String(TEXT_APPEARANCE.to_vec()));
            if field.kind == NewFieldKind::Date {
                widget.insert("AA", date_actions());
            }
            widget.insert("MK", characteristics);
        }
        NewFieldKind::CheckBox => {
            widget.insert("FT", Name::from("Btn"));
            widget.insert("DA", Object::String(CHECK_APPEARANCE.to_vec()));
            characteristics.insert("CA", Object::String(b"4".to_vec()));
            widget.insert("MK", characteristics);
            widget.insert("V", Name::from("Off"));
            widget.insert("AS", Name::from("Off"));
            let mut resources = Dict::new();
            let mut font = Dict::new();
            font.insert("ZaDb", Object::Ref(fonts.check));
            resources.insert("Font", font);
            let off = doc.add_object(turned_form_stream(
                &check_box(width, height, false),
                width,
                height,
                resources.clone(),
                turns,
            ))?;
            let on = doc.add_object(turned_form_stream(
                &check_box(width, height, true),
                width,
                height,
                resources,
                turns,
            ))?;
            let mut states = Dict::new();
            states.insert("Yes", Object::Ref(on));
            states.insert("Off", Object::Ref(off));
            let mut appearances = Dict::new();
            appearances.insert("N", states);
            widget.insert("AP", appearances);
        }
    }
    let id = doc.add_object(Object::Dict(widget))?;
    add_to_form(doc, id)?;
    add_to_page(doc, page, id)?;
    if field.kind != NewFieldKind::CheckBox {
        // The empty appearance, drawn as filling draws it: its border, turned with the page.
        let values = HashMap::from([(field.name.clone(), FillValue::Text(String::new()))]);
        fill_fields(doc, &values)?;
    }
    Ok(id)
}

/// The full names of the form's fields, from its `/Fields` down, those without a widget on a page
/// included.
fn form_names(doc: &Document) -> Result<Vec<String>> {
    let (form, _) = form(doc)?;
    let top = match form.get(b"Fields") {
        Some(value) => match pdf::resolved(doc, value)? {
            Some(Object::Array(top)) => top,
            _ => return Ok(Vec::new()),
        },
        None => return Ok(Vec::new()),
    };
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(Object, String, usize)> =
        top.into_iter().map(|f| (f, String::new(), 0)).collect();
    while let Some((value, parent, depth)) = stack.pop() {
        if depth > 32 || names.len() > 100_000 {
            break;
        }
        if let Object::Ref(id) = value
            && !seen.insert(id)
        {
            continue;
        }
        let Some(Object::Dict(node)) = pdf::resolved(doc, &value)? else {
            continue;
        };
        let mut name = parent;
        if let Some(Object::String(partial)) = node.get(b"T") {
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(&sealwax_core::decode_text_string(partial));
            names.push(name.clone());
        }
        if let Some(kids) = node.get(b"Kids")
            && let Some(Object::Array(kids)) = pdf::resolved(doc, kids)?
        {
            stack.extend(kids.into_iter().map(|kid| (kid, name.clone(), depth + 1)));
        }
    }
    Ok(names)
}

/// `rect` given on a page as it shows (turned by `rotation`, from the top-left corner of
/// `crop_box`, y down) in the page's default user space; `None` without area.
fn shown_to_user(rect: [f64; 4], crop_box: Rect, rotation: Rotation) -> Option<Rect> {
    if !rect.iter().all(|v| v.is_finite()) {
        return None;
    }
    let point = |u: f64, v: f64| -> (f64, f64) {
        match rotation {
            Rotation::None => (crop_box.x1 + u, crop_box.y2 - v),
            Rotation::Quarter => (crop_box.x1 + v, crop_box.y1 + u),
            Rotation::Half => (crop_box.x2 - u, crop_box.y1 + v),
            Rotation::ThreeQuarters => (crop_box.x2 - v, crop_box.y2 - u),
        }
    };
    let (ax, ay) = point(rect[0], rect[1]);
    let (bx, by) = point(rect[2], rect[3]);
    let user = Rect::from_corners(ax.min(bx), ay.min(by), ax.max(bx), ay.max(by));
    (user.x2 > user.x1 && user.y2 > user.y1).then_some(user)
}

/// The Acrobat actions that keep a date as `yyyy-mm-dd`: formatting it, and checking what is
/// typed.
fn date_actions() -> Dict {
    let action = |script: &str| -> Object {
        let mut action = Dict::new();
        action.insert("S", Name::from("JavaScript"));
        action.insert("JS", Object::String(script.as_bytes().to_vec()));
        Object::Dict(action)
    };
    let mut actions = Dict::new();
    actions.insert("F", action("AFDate_FormatEx(\"yyyy-mm-dd\");"));
    actions.insert("K", action("AFDate_KeystrokeEx(\"yyyy-mm-dd\");"));
    actions
}

/// A check box's appearance: its border, with the check mark when `checked`.
fn check_box(width: f64, height: f64, checked: bool) -> Vec<u8> {
    let mut drawing = format!(
        "q {BORDER_GRAY} G 1 w 0.5 0.5 {} {} re S Q",
        width - 1.0,
        height - 1.0
    );
    if checked {
        // ZapfDingbats' check mark is 0.846 em wide, 0.692 em above the baseline.
        let size = (width.min(height) * 0.8).max(1.0);
        let x = (width - 0.846 * size) / 2.0;
        let y = (height - 0.692 * size) / 2.0;
        drawing.push_str(&format!(
            " q BT 0 g /ZaDb {size:.2} Tf {x:.2} {y:.2} Td (4) Tj ET Q"
        ));
    }
    drawing.into_bytes()
}

/// The fonts fields created use, in the form's resources.
struct FormFonts {
    check: ObjId,
}

/// The form's fonts, `/Helv` and `/ZaDb` added to its resources when it has none of those
/// names, and the form created when the document has none.
fn form_fonts(doc: &mut Document) -> Result<FormFonts> {
    let (mut form, location) = form(doc)?;
    let mut resources = pdf::dict(doc, form.get(b"DR"))?.unwrap_or_default();
    let mut fonts = pdf::dict(doc, resources.get(b"Font"))?.unwrap_or_default();
    let mut font = |doc: &mut Document, name: &str, kind: Font| -> Result<ObjId> {
        if let Some(Object::Ref(id)) = fonts.get(name.as_bytes()) {
            return Ok(*id);
        }
        let id = doc.add_object(font_dict(kind))?;
        fonts.insert(name, Object::Ref(id));
        Ok(id)
    };
    font(doc, "Helv", Font::Helvetica)?;
    let check = font(doc, "ZaDb", Font::ZapfDingbats)?;
    resources.insert("Font", fonts);
    form.insert("DR", resources);
    if !form.contains_key(b"DA") {
        form.insert("DA", Object::String(TEXT_APPEARANCE.to_vec()));
    }
    if !form.contains_key(b"Fields") {
        form.insert("Fields", Object::Array(Vec::new()));
    }
    store_form(doc, form, location)?;
    Ok(FormFonts { check })
}

/// Where the form's dictionary is written.
enum FormLocation {
    /// In the catalog.
    Inline,
    /// An object of its own.
    Object(ObjId),
}

/// The form (an empty one when the document has none), and where it is written.
fn form(doc: &Document) -> Result<(Dict, FormLocation)> {
    let Some(Object::Dict(catalog)) = doc.object(doc.catalog_id())? else {
        return Err(Error::Pdf(sealwax_core::Error::Malformed));
    };
    Ok(match catalog.get(b"AcroForm") {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Dict(form)) => (form, FormLocation::Object(*id)),
            _ => (Dict::new(), FormLocation::Inline),
        },
        Some(Object::Dict(form)) => (form.clone(), FormLocation::Inline),
        _ => (Dict::new(), FormLocation::Inline),
    })
}

fn store_form(doc: &mut Document, form: Dict, location: FormLocation) -> Result<()> {
    match location {
        FormLocation::Object(id) => doc.set_object(id, Object::Dict(form)),
        FormLocation::Inline => {
            let catalog_id = doc.catalog_id();
            if let Some(Object::Dict(mut catalog)) = doc.object(catalog_id)? {
                catalog.insert("AcroForm", form);
                doc.set_object(catalog_id, Object::Dict(catalog));
            }
        }
    }
    Ok(())
}

/// `field` added to the form's `/Fields`.
fn add_to_form(doc: &mut Document, field: ObjId) -> Result<()> {
    let (mut form, location) = form(doc)?;
    let mut list = match form.get(b"Fields") {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Array(list)) => list,
            _ => Vec::new(),
        },
        Some(Object::Array(list)) => list.clone(),
        _ => Vec::new(),
    };
    list.push(Object::Ref(field));
    form.insert("Fields", Object::Array(list));
    store_form(doc, form, location)
}

/// `widget` added to the annotations of `page`.
fn add_to_page(doc: &mut Document, page: ObjId, widget: ObjId) -> Result<()> {
    let Some(Object::Dict(mut dict)) = doc.object(page)? else {
        return Err(Error::Pdf(sealwax_core::Error::Malformed));
    };
    match dict.get(b"Annots").cloned() {
        Some(Object::Ref(id)) => {
            let mut list = match doc.object(id)? {
                Some(Object::Array(list)) => list,
                _ => Vec::new(),
            };
            list.push(Object::Ref(widget));
            doc.set_object(id, Object::Array(list));
        }
        Some(Object::Array(mut list)) => {
            list.push(Object::Ref(widget));
            dict.insert("Annots", Object::Array(list));
            doc.set_object(page, Object::Dict(dict));
        }
        _ => {
            dict.insert("Annots", Object::Array(vec![Object::Ref(widget)]));
            doc.set_object(page, Object::Dict(dict));
        }
    }
    Ok(())
}
