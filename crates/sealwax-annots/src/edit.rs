//! Retouches of fields a form already has, as test fixtures make them: the name shown to users
//! changed, a check box's state renamed, appearances taken away.

use sealwax_core::{Dict, Document, Name, ObjId, Object};

use crate::fields::{FieldKind, field_kind, widgets};
use crate::{Error, Result};

fn invalid(what: &'static str) -> Error {
    Error::Pdf(sealwax_core::Error::InvalidArgument { what })
}

/// The widgets of the field named `name` (its full name), on every page: each widget's object
/// and dictionary, and its field's object.
fn widgets_of(doc: &Document, name: &str) -> Result<Vec<(ObjId, Dict, ObjId)>> {
    let mut found = Vec::new();
    for page in doc.page_ids().into_iter().flatten() {
        for widget in widgets(doc, page)? {
            if widget.name == name
                && let (Some(id), Some(field)) = (widget.id, widget.field)
            {
                found.push((id, widget.dict, field));
            }
        }
    }
    if found.is_empty() {
        return Err(invalid("field name"));
    }
    Ok(found)
}

/// Set the name shown to users (`/TU`) of the field named `name`, or remove it.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] when no widget of the document belongs to a field of
/// that name.
pub fn set_field_label(doc: &mut Document, name: &str, label: Option<&str>) -> Result<()> {
    let mut fields: Vec<ObjId> = widgets_of(doc, name)?.into_iter().map(|w| w.2).collect();
    fields.dedup();
    for field in fields {
        let Some(Object::Dict(mut dict)) = doc.object(field)? else {
            continue;
        };
        match label {
            Some(label) => {
                dict.insert(
                    "TU",
                    Object::String(sealwax_core::encode_text_string(label)),
                );
            }
            None => {
                dict.remove(b"TU");
            }
        }
        doc.set_object(field, Object::Dict(dict));
    }
    Ok(())
}

/// `dict`'s entry `key` (a dictionary, written in place or not), changed by `change`.
fn change_dict(
    doc: &mut Document,
    dict: &mut Dict,
    key: &[u8],
    change: impl FnOnce(&mut Dict),
) -> Result<()> {
    match dict.get(key).cloned() {
        Some(Object::Ref(id)) => {
            if let Some(Object::Dict(mut inner)) = doc.object(id)? {
                change(&mut inner);
                doc.set_object(id, Object::Dict(inner));
            }
        }
        Some(Object::Dict(mut inner)) => {
            change(&mut inner);
            dict.insert(key, Object::Dict(inner));
        }
        _ => {}
    }
    Ok(())
}

/// Rename the state that checks the check box named `name` (every state but `Off`) to `state`,
/// a name given as its bytes (`Fran\xE7ais` is written `/Fran#E7ais`): in its appearances
/// (`/AP /N`, `/AP /D`), its widgets' state (`/AS`) and its value (`/V`) when checked.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] when no check box has that name, or for a state
/// that is empty or `Off`.
pub fn rename_button_state(doc: &mut Document, name: &str, state: &[u8]) -> Result<()> {
    if state.is_empty() || state == b"Off" {
        return Err(invalid("button state"));
    }
    let widgets = widgets_of(doc, name)?;
    for (_, dict, _) in &widgets {
        if field_kind(doc, dict)? != FieldKind::CheckBox {
            return Err(invalid("field name"));
        }
    }
    let renamed = |states: &mut Dict| {
        let checked: Vec<Vec<u8>> = states
            .iter()
            .map(|(key, _)| key.as_bytes().to_vec())
            .filter(|key| key != b"Off")
            .collect();
        for key in checked {
            if let Some(value) = states.remove(&key) {
                states.insert(Name::new(state.to_vec()), value);
            }
        }
    };
    let mut fields = Vec::new();
    for (id, mut dict, field) in widgets {
        match dict.get(b"AP").cloned() {
            Some(Object::Ref(ap)) => {
                if let Some(Object::Dict(mut appearances)) = doc.object(ap)? {
                    change_dict(doc, &mut appearances, b"N", renamed)?;
                    change_dict(doc, &mut appearances, b"D", renamed)?;
                    doc.set_object(ap, Object::Dict(appearances));
                }
            }
            Some(Object::Dict(mut appearances)) => {
                change_dict(doc, &mut appearances, b"N", renamed)?;
                change_dict(doc, &mut appearances, b"D", renamed)?;
                dict.insert("AP", Object::Dict(appearances));
            }
            _ => {}
        }
        if dict.get(b"AS").and_then(Object::as_name) != Some(b"Off".as_slice())
            && dict.contains_key(b"AS")
        {
            dict.insert("AS", Name::new(state.to_vec()));
        }
        doc.set_object(id, Object::Dict(dict));
        fields.push(field);
    }
    fields.dedup();
    for field in fields {
        let Some(Object::Dict(mut dict)) = doc.object(field)? else {
            continue;
        };
        if let Some(value) = dict.get(b"V").and_then(Object::as_name)
            && value != b"Off"
        {
            dict.insert("V", Name::new(state.to_vec()));
            doc.set_object(field, Object::Dict(dict));
        }
    }
    Ok(())
}

/// Take away the appearances (`/AP`) of the widgets of the field named `name`, as a document a
/// reader must draw them for; returns the widgets that had some.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] when no widget of the document belongs to a field of
/// that name.
pub fn remove_field_appearances(doc: &mut Document, name: &str) -> Result<usize> {
    let mut removed = 0;
    for (id, mut dict, _) in widgets_of(doc, name)? {
        if dict.remove(b"AP").is_some() {
            removed += 1;
            doc.set_object(id, Object::Dict(dict));
        }
    }
    Ok(removed)
}
