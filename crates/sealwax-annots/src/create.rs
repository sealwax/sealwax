//! Form fields created on a page: a text field, a date field, a check box, each with its
//! appearance drawn, placed where the page shows it; and another widget for a field a form has.

use std::collections::HashMap;

use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Rotation, encode_text_string};

use crate::draw::COMBO;
use crate::fields::{
    FieldKind, FillValue, draw_text_widget, field_kind, fields, fill_fields, states, widgets,
};
use crate::flatten::{Font, font_dict, turned_form_stream};
use crate::pdf;
use crate::text::number;
use crate::{Error, Result};

/// The form's default appearance of text, as fields created without a font size take it:
/// Helvetica, its size fitted to the field, black.
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
    /// A drop-down list of [`NewField::choices`] (a combo box), none chosen.
    ComboBox,
    /// A signature field, not signed.
    Signature,
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
    /// The size of a text or date field's text, in points; `None`, the size fitted to the field
    /// (`0 Tf`). Some readers do not keep a value typed into a field whose size is fitted. A
    /// check box's mark is always fitted.
    pub font_size: Option<f64>,
    /// A combo box's options, in order: the value each exports, and what it shows (`/Opt`).
    pub choices: Vec<(String, String)>,
    /// The name shown to users (`/TU`), for any kind.
    pub label: Option<String>,
    /// The most characters a text or date field takes (`/MaxLen`).
    pub max_length: Option<u32>,
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
            font_size: None,
            choices: Vec::new(),
            label: None,
            max_length: None,
        }
    }

    /// Required or not.
    #[must_use]
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// The size of its text, in points ([`NewField::font_size`]); `None` fits it to the field.
    #[must_use]
    pub fn font_size(mut self, size: Option<f64>) -> Self {
        self.font_size = size;
        self
    }

    /// A combo box's options: the value each exports, and what it shows.
    #[must_use]
    pub fn choices(mut self, choices: Vec<(String, String)>) -> Self {
        self.choices = choices;
        self
    }

    /// The name shown to users (`/TU`).
    #[must_use]
    pub fn label(mut self, label: Option<String>) -> Self {
        self.label = label;
        self
    }

    /// The most characters a text or date field takes (`/MaxLen`).
    #[must_use]
    pub fn max_length(mut self, max_length: Option<u32>) -> Self {
        self.max_length = max_length;
        self
    }
}

/// The font sizes a field created takes, in points.
pub const FIELD_FONT_SIZES: std::ops::RangeInclusive<f64> = 1.0..=1000.0;

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
/// period, a page the document does not have, a rectangle without area, a font size outside
/// [`FIELD_FONT_SIZES`], a combo box without options (or options for another kind), or a
/// maximum length of 0 or for a field other than a text or date one.
pub fn add_field(doc: &mut Document, field: &NewField) -> Result<ObjId> {
    if field.name.is_empty() || field.name.contains('.') {
        return Err(sealwax_core::Error::InvalidArgument { what: "field name" }.into());
    }
    if (field.kind == NewFieldKind::ComboBox) == field.choices.is_empty() {
        return Err(sealwax_core::Error::InvalidArgument { what: "choices" }.into());
    }
    let texts = matches!(field.kind, NewFieldKind::Text | NewFieldKind::Date);
    if field.max_length.is_some_and(|length| length == 0 || !texts) {
        return Err(sealwax_core::Error::InvalidArgument { what: "max length" }.into());
    }
    let text_appearance = match field.font_size {
        None => TEXT_APPEARANCE.to_vec(),
        Some(size) if FIELD_FONT_SIZES.contains(&size) => {
            format!("/Helv {} Tf 0 g", number(size)).into_bytes()
        }
        Some(_) => return Err(sealwax_core::Error::InvalidArgument { what: "font size" }.into()),
    };
    let mut names: Vec<String> = fields(doc, None)?.into_iter().map(|f| f.name).collect();
    names.extend(
        form_tree(doc)?
            .into_iter()
            .filter(|node| !node.name.is_empty())
            .map(|node| node.name),
    );
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
    if let Some(label) = &field.label {
        widget.insert("TU", Object::String(encode_text_string(label)));
    }
    let mut flags = if field.required { 2_i64 } else { 0 };
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
            widget.insert("DA", Object::String(text_appearance));
            if field.kind == NewFieldKind::Date {
                widget.insert("AA", date_actions());
            }
            if let Some(length) = field.max_length {
                widget.insert("MaxLen", i64::from(length));
            }
            widget.insert("MK", characteristics);
        }
        NewFieldKind::ComboBox => {
            widget.insert("FT", Name::from("Ch"));
            flags |= COMBO;
            widget.insert("DA", Object::String(text_appearance));
            let options: Vec<Object> = field
                .choices
                .iter()
                .map(|(export, shown)| {
                    if export == shown {
                        Object::String(encode_text_string(shown))
                    } else {
                        Object::Array(vec![
                            Object::String(encode_text_string(export)),
                            Object::String(encode_text_string(shown)),
                        ])
                    }
                })
                .collect();
            widget.insert("Opt", options);
            widget.insert("MK", characteristics);
        }
        NewFieldKind::Signature => {
            widget.insert("FT", Name::from("Sig"));
            widget.insert("MK", characteristics);
            // Its border only, until it is signed.
            let border = doc.add_object(turned_form_stream(
                &check_box(width, height, false),
                width,
                height,
                Dict::new(),
                turns,
            ))?;
            let mut appearances = Dict::new();
            appearances.insert("N", Object::Ref(border));
            widget.insert("AP", appearances);
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
    if flags != 0 {
        widget.insert("Ff", flags);
    }
    let id = doc.add_object(Object::Dict(widget))?;
    add_to_form(doc, id)?;
    add_to_page(doc, page, id)?;
    if !matches!(field.kind, NewFieldKind::CheckBox | NewFieldKind::Signature) {
        // The empty appearance, drawn as filling draws it: its border, turned with the page.
        let values = HashMap::from([(field.name.clone(), FillValue::Text(String::new()))]);
        fill_fields(doc, &values)?;
        // A combo box drawn empty has no option chosen: no value.
        if field.kind == NewFieldKind::ComboBox
            && let Some(Object::Dict(mut widget)) = doc.object(id)?
        {
            widget.remove(b"V");
            doc.set_object(id, Object::Dict(widget));
        }
    }
    Ok(id)
}

/// The keys of a field's dictionary, as opposed to its widget's when the two are one dictionary
/// (ISO 32000-1, tables 220, 222, 227, 229, 231 and 232): what a field holds for all its widgets.
const FIELD_KEYS: &[&[u8]] = &[
    b"FT", b"T", b"TU", b"TM", b"Ff", b"V", b"DV", b"DA", b"Q", b"DS", b"RV", b"MaxLen", b"Opt",
    b"TI", b"I", b"Lock", b"SV",
];
/// The triggers of a field's additional actions (table 196), as opposed to its widget's.
const FIELD_TRIGGERS: &[&[u8]] = &[b"K", b"F", b"V", b"C"];

/// Give the field named `name` (its full name) another widget, on page `page_index` at `rect` on
/// the page as it shows, as [`add_field`] places one: the same field shown twice, on one page or
/// two, one value for both. The field keeps what it is (its kind, flags, value, maximum length,
/// name shown, options); the widget takes the look of the field's first one (its border, its
/// colours, a check box's states), turned with its own page, and shows the field's value. A
/// field that is one dictionary with its widget is first made the parent of that widget, where
/// the field was. Returns the widget.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] when no widget on the pages belongs to a field of
/// that name; for a field that is no text field, combo box, list box or check box (`field
/// kind`: a radio button's widgets are its options, a signature field has one); a page the
/// document does not have; a rectangle without area.
pub fn add_widget(
    doc: &mut Document,
    name: &str,
    page_index: usize,
    rect: [f64; 4],
) -> Result<ObjId> {
    let invalid = |what| Error::Pdf(sealwax_core::Error::InvalidArgument { what });
    let mut first = None;
    for page in doc.page_ids().into_iter().flatten() {
        first = widgets(doc, page)?
            .into_iter()
            .find(|widget| widget.name == name && widget.id.is_some() && widget.field.is_some());
        if first.is_some() {
            break;
        }
    }
    let Some((Some(model_id), Some(field_id), model)) =
        first.map(|widget| (widget.id, widget.field, widget.dict))
    else {
        return Err(invalid("field name"));
    };
    let kind = field_kind(doc, &model)?;
    if !matches!(
        kind,
        FieldKind::Text | FieldKind::ComboBox | FieldKind::ListBox | FieldKind::CheckBox
    ) {
        return Err(invalid("field kind"));
    }
    let Some(Some(page)) = doc.page_ids().get(page_index).copied() else {
        return Err(invalid("page index"));
    };
    let geometry = doc.geometry()[page_index];
    let rect = shown_to_user(rect, geometry.crop_box, geometry.rotation)
        .ok_or(invalid("field rectangle"))?;
    let field_id = if field_id == model_id {
        own_field(doc, model_id)?
    } else {
        field_id
    };
    let Some(Object::Dict(model)) = doc.object(model_id)? else {
        return Err(Error::Pdf(sealwax_core::Error::Malformed));
    };
    let turns = i64::from(geometry.rotation.degrees() / 90);
    let mut widget = Dict::new();
    widget.insert("Type", Name::from("Annot"));
    widget.insert("Subtype", Name::from("Widget"));
    widget.insert("Parent", Object::Ref(field_id));
    widget.insert("Rect", pdf::numbers(&[rect.x1, rect.y1, rect.x2, rect.y2]));
    widget.insert("P", Object::Ref(page));
    widget.insert("F", model.get(b"F").cloned().unwrap_or(Object::Integer(4)));
    // Its look, the model's: turned with its own page.
    let mut characteristics = pdf::dict(doc, model.get(b"MK"))?.unwrap_or_default();
    characteristics.remove(b"R");
    if turns != 0 {
        characteristics.insert("R", turns * 90);
    }
    widget.insert("MK", characteristics);
    for key in [&b"BS"[..], b"Border", b"DA", b"Q", b"H"] {
        if let Some(value) = model.get(key) {
            widget.insert(Name::new(key.to_vec()), value.clone());
        }
    }
    if kind == FieldKind::CheckBox {
        let (mut width, mut height) = (rect.x2 - rect.x1, rect.y2 - rect.y1);
        if turns % 2 == 1 {
            (width, height) = (height, width);
        }
        let mut checked: Vec<Vec<u8>> = states(doc, &model)?
            .into_iter()
            .filter(|state| state != b"Off")
            .collect();
        if checked.is_empty() {
            checked.push(b"Yes".to_vec());
        }
        let fonts = form_fonts(doc)?;
        let mut font = Dict::new();
        font.insert("ZaDb", Object::Ref(fonts.check));
        let mut resources = Dict::new();
        resources.insert("Font", font);
        let mut drawn = Dict::new();
        for (state, on) in checked
            .iter()
            .map(|state| (state.as_slice(), true))
            .chain([(&b"Off"[..], false)])
        {
            let form = doc.add_object(turned_form_stream(
                &check_box(width, height, on),
                width,
                height,
                resources.clone(),
                turns,
            ))?;
            drawn.insert(Name::new(state.to_vec()), Object::Ref(form));
        }
        let mut appearances = Dict::new();
        appearances.insert("N", drawn);
        widget.insert("AP", appearances);
        let value = pdf::inherited(doc, &model, b"V")?;
        let shown = match value.as_ref().and_then(Object::as_name) {
            Some(state) if checked.iter().any(|on| on == state) => state.to_vec(),
            _ => b"Off".to_vec(),
        };
        widget.insert("AS", Name::new(shown));
    }
    let id = doc.add_object(Object::Dict(widget))?;
    add_to_kids(doc, field_id, id)?;
    add_to_page(doc, page, id)?;
    if kind != FieldKind::CheckBox {
        let (acro_form, _) = form(doc)?;
        draw_text_widget(doc, &acro_form, id, kind != FieldKind::Text, &mut None)?;
    }
    Ok(id)
}

/// The field of `merged`, a field and its widget in one dictionary, made a dictionary of its own
/// where the field was (its parent's `/Kids`, or the form's `/Fields`, and the form's calculation
/// order), `merged` its widget under it: what is the field's (its keys, its triggers) moves to
/// it, the rest (the widget's, and the object pages and the structure refer to) stays.
fn own_field(doc: &mut Document, merged: ObjId) -> Result<ObjId> {
    let Some(Object::Dict(mut widget)) = doc.object(merged)? else {
        return Err(Error::Pdf(sealwax_core::Error::Malformed));
    };
    let mut field = Dict::new();
    for key in FIELD_KEYS.iter().chain([&&b"Parent"[..]]) {
        if let Some(value) = widget.remove(key) {
            field.insert(Name::new(key.to_vec()), value);
        }
    }
    if let Some(mut actions) = pdf::dict(doc, widget.get(b"AA"))? {
        let mut moved = Dict::new();
        for key in FIELD_TRIGGERS {
            if let Some(action) = actions.remove(key) {
                moved.insert(Name::new(key.to_vec()), action);
            }
        }
        if !moved.is_empty() {
            field.insert("AA", moved);
            if actions.is_empty() {
                widget.remove(b"AA");
            } else {
                widget.insert("AA", actions);
            }
        }
    }
    let parent = field.get(b"Parent").and_then(Object::as_ref_id);
    field.insert("Kids", Object::Array(vec![Object::Ref(merged)]));
    let id = doc.add_object(Object::Dict(field))?;
    widget.insert("Parent", Object::Ref(id));
    doc.set_object(merged, Object::Dict(widget));
    if let Some(parent) = parent {
        let Some(Object::Dict(mut dict)) = doc.object(parent)? else {
            return Err(Error::Pdf(sealwax_core::Error::Malformed));
        };
        if let Some(kids) = replaced(doc, dict.get(b"Kids"), merged, id)? {
            dict.insert("Kids", kids);
            doc.set_object(parent, Object::Dict(dict));
        }
    }
    // The calculation order names fields at any depth; the form's fields, those at the top.
    let (mut form, location) = form(doc)?;
    let lists: &[&[u8]] = if parent.is_some() {
        &[b"CO"]
    } else {
        &[b"Fields", b"CO"]
    };
    for key in lists {
        if let Some(list) = replaced(doc, form.get(key), merged, id)? {
            form.insert(Name::new(key.to_vec()), list);
        }
    }
    store_form(doc, form, location)?;
    Ok(id)
}

/// `list` (an array, or a reference to one) with `from` replaced by `to`: the array to write in
/// its place when it is written in place; a referred array is written over, and `None` returned.
fn replaced(
    doc: &mut Document,
    list: Option<&Object>,
    from: ObjId,
    to: ObjId,
) -> Result<Option<Object>> {
    let swap = |items: Vec<Object>| -> Vec<Object> {
        items
            .into_iter()
            .map(|item| match item {
                Object::Ref(id) if id == from => Object::Ref(to),
                item => item,
            })
            .collect()
    };
    Ok(match list {
        Some(Object::Ref(id)) => {
            if let Some(Object::Array(items)) = doc.object(*id)? {
                doc.set_object(*id, Object::Array(swap(items)));
            }
            None
        }
        Some(Object::Array(items)) => Some(Object::Array(swap(items.clone()))),
        _ => None,
    })
}

/// `widget` added to the `/Kids` of `field`.
fn add_to_kids(doc: &mut Document, field: ObjId, widget: ObjId) -> Result<()> {
    let Some(Object::Dict(mut dict)) = doc.object(field)? else {
        return Err(Error::Pdf(sealwax_core::Error::Malformed));
    };
    match dict.get(b"Kids").cloned() {
        Some(Object::Ref(id)) => {
            let mut kids = match doc.object(id)? {
                Some(Object::Array(kids)) => kids,
                _ => Vec::new(),
            };
            kids.push(Object::Ref(widget));
            doc.set_object(id, Object::Array(kids));
        }
        Some(Object::Array(mut kids)) => {
            kids.push(Object::Ref(widget));
            dict.insert("Kids", Object::Array(kids));
            doc.set_object(field, Object::Dict(dict));
        }
        _ => {
            dict.insert("Kids", Object::Array(vec![Object::Ref(widget)]));
            doc.set_object(field, Object::Dict(dict));
        }
    }
    Ok(())
}

/// A node of the form's field tree, from its `/Fields` down.
pub(crate) struct FormNode {
    /// Its full name; empty for a node without a partial name of its own nor above it.
    pub(crate) name: String,
    /// Its object, when it is one.
    pub(crate) id: Option<ObjId>,
    /// It has a partial name (`/T`) and none of its kids has: a field that holds a value.
    pub(crate) terminal: bool,
}

/// The nodes of the form's field tree, those without a widget on a page included; 32 levels deep
/// and 100,000 nodes at most, each once.
pub(crate) fn form_tree(doc: &Document) -> Result<Vec<FormNode>> {
    let (form, _) = form(doc)?;
    let top = match form.get(b"Fields") {
        Some(value) => match pdf::resolved(doc, value)? {
            Some(Object::Array(top)) => top,
            _ => return Ok(Vec::new()),
        },
        None => return Ok(Vec::new()),
    };
    let mut nodes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(Object, String, usize)> =
        top.into_iter().map(|f| (f, String::new(), 0)).collect();
    while let Some((value, parent, depth)) = stack.pop() {
        if depth > 32 || nodes.len() > 100_000 {
            break;
        }
        let id = match value {
            Object::Ref(id) if !seen.insert(id) => continue,
            Object::Ref(id) => Some(id),
            _ => None,
        };
        let Some(Object::Dict(node)) = pdf::resolved(doc, &value)? else {
            continue;
        };
        let mut name = parent;
        let named = if let Some(Object::String(partial)) = node.get(b"T") {
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(&sealwax_core::decode_text_string(partial));
            true
        } else {
            false
        };
        let kids = match node.get(b"Kids") {
            Some(kids) => match pdf::resolved(doc, kids)? {
                Some(Object::Array(kids)) => kids,
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        let mut named_kids = false;
        for kid in &kids {
            if let Some(Object::Dict(kid)) = pdf::resolved(doc, kid)? {
                named_kids |= kid.contains_key(b"T");
            }
        }
        nodes.push(FormNode {
            name: name.clone(),
            id,
            terminal: named && !named_kids,
        });
        stack.extend(kids.into_iter().map(|kid| (kid, name.clone(), depth + 1)));
    }
    Ok(nodes)
}

/// The matrix from a page as it shows (turned by `rotation`, from the top-left corner of
/// `crop_box`, y down) to its default user space: [`shown_to_user`]'s, for a drawing.
pub(crate) fn shown_matrix(crop_box: Rect, rotation: Rotation) -> [f64; 6] {
    match rotation {
        Rotation::None => [1.0, 0.0, 0.0, -1.0, crop_box.x1, crop_box.y2],
        Rotation::Quarter => [0.0, 1.0, 1.0, 0.0, crop_box.x1, crop_box.y1],
        Rotation::Half => [-1.0, 0.0, 0.0, 1.0, crop_box.x2, crop_box.y1],
        Rotation::ThreeQuarters => [0.0, -1.0, -1.0, 0.0, crop_box.x2, crop_box.y2],
    }
}

/// `rect` given on a page as it shows (turned by `rotation`, from the top-left corner of
/// `crop_box`, y down) in the page's default user space; `None` without area.
pub(crate) fn shown_to_user(rect: [f64; 4], crop_box: Rect, rotation: Rotation) -> Option<Rect> {
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
pub(crate) enum FormLocation {
    /// In the catalog.
    Inline,
    /// An object of its own.
    Object(ObjId),
}

/// The form (an empty one when the document has none), and where it is written.
pub(crate) fn form(doc: &Document) -> Result<(Dict, FormLocation)> {
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

pub(crate) fn store_form(doc: &mut Document, form: Dict, location: FormLocation) -> Result<()> {
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
