//! The fields of an interactive form, as their widgets place them on the pages: read, filled,
//! read back (ISO 32000-1, 12.7).
//!
//! A field shows on a page through its widgets, each one occurrence of it: a name repeated on two
//! pages is two occurrences, never merged here. The value is the field's (`/V`, inherited from its
//! parents); a button shows the state its widget's `/AS` names. Names of button states are bytes,
//! kept as the document spells them: a state is written back as it was read, never re-encoded.

use std::collections::{BTreeMap, HashMap, HashSet};

use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Result, decode_text_string};

use crate::draw;
use crate::flatten::{self, Font, Normal};
use crate::pdf;
use crate::text;

/// Field flags (ISO 32000-1, 12.7.3.1, 12.7.4.2, 12.7.4.4).
const READ_ONLY: i64 = 1;
const REQUIRED: i64 = 1 << 1;
const RADIO: i64 = 1 << 15;
const PUSH_BUTTON: i64 = 1 << 16;
const COMBO: i64 = 1 << 17;
const EDIT: i64 = 1 << 18;

/// What a field is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum FieldKind {
    Text,
    CheckBox,
    Radio,
    PushButton,
    /// A choice shown as a drop-down list (combo box).
    ComboBox,
    /// A choice shown as a scrolling list.
    ListBox,
    Signature,
    /// A field of no known type.
    Other,
}

/// A field's value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum FieldValue {
    /// A text string: a text or a choice field's.
    Text(String),
    /// A name: a button's state, or a choice given as a name. Its bytes, as the document has them.
    Name(Vec<u8>),
    /// Several values: a list box with more than one selected.
    List(Vec<String>),
}

/// One option of a choice field.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum FieldChoice {
    /// The value is what is shown.
    Single(String),
    /// The value exported, and what is shown.
    Pair { export: String, display: String },
}

/// One occurrence of a field: a widget on a page.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Field {
    /// The full name: the partial names (`/T`) from the top of the form, joined by periods.
    pub name: String,
    /// The name shown to users (`/TU`).
    pub label: Option<String>,
    pub kind: FieldKind,
    /// The page the widget is on, from 0.
    pub page_index: usize,
    /// The widget's rectangle, in the page's default user space (unrotated, y up).
    pub rect: Rect,
    /// The page's crop box, in the same space.
    pub crop_box: Rect,
    /// The field's value (`/V`, inherited).
    pub value: Option<FieldValue>,
    /// A button's current state (`/AS`), its bytes.
    pub state: Option<Vec<u8>>,
    pub required: bool,
    pub read_only: bool,
    /// The maximum length of a text field (`/MaxLen`).
    pub max_length: Option<i64>,
    /// A choice field's options (`/Opt`).
    pub choices: Vec<FieldChoice>,
    /// A combo box that takes text typed beside its options (`Edit`).
    pub editable: bool,
    /// A button's states, the names of its normal appearances (`/AP /N`), as the document spells
    /// them (bytes), `Off` included, in the order of their bytes.
    pub button_states: Vec<Vec<u8>>,
    /// A button's caption (`/MK /CA`).
    pub button_caption: Option<String>,
}

/// What filling a form did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Filled {
    /// The occurrences written: widgets of a field named in the values.
    pub occurrences: usize,
    /// Characters of text values that WinAnsiEncoding does not have, drawn as `?` in the
    /// appearance (the value itself keeps them).
    pub characters_replaced: usize,
    /// The names given that were not written, each field left as it was, in the order of their
    /// bytes: no field on the pages has that name, the field takes no value (a signature, a push
    /// button), a check box or radio button given a state none of its widgets has (a state given
    /// as its UTF-8 text while the document spells it `Fran#E7ais` included), and, filling
    /// [`FillOptions::strict`]ly, a value the field does not allow.
    pub refused: Vec<String>,
    /// Filling with [`FillOptions::keep_filled`]: the fields left as they were because they held
    /// another value, in the order of their names' bytes.
    pub kept: Vec<String>,
    /// Each field written, by full name: the value it holds now and the one it held before.
    pub written: BTreeMap<String, Written>,
}

/// A field written: its value, and the one it held before, each read as one text (a text or
/// choice field's value, several selected joined by `, `, empty when it has none; a button's
/// state, `Off` when it has none).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Written {
    pub value: String,
    pub previous: String,
}

impl Written {
    /// A field written `value`, which held `previous` before: what [`unfill_fields`] undoes.
    #[must_use]
    pub fn new(value: &str, previous: &str) -> Self {
        Self {
            value: value.to_owned(),
            previous: previous.to_owned(),
        }
    }
}

/// How [`fill_fields_with`] fills.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct FillOptions {
    /// Leave a field holding another value than the one given as it is (named in
    /// [`Filled::kept`]): a value typed by hand is not overwritten. An empty field, or a button
    /// `Off`, is filled.
    pub keep_filled: bool,
    /// Fill as a reader lets a person type: refuse (named in [`Filled::refused`], the field left
    /// as it was) a read-only field, a text longer than the field's maximum length, characters
    /// its font cannot draw (WinAnsiEncoding's, which would be drawn as `?`), and a choice the
    /// field does not offer (a combo box that takes typed text excepted).
    pub strict: bool,
}

impl FillOptions {
    /// Keep the fields holding another value, or not.
    #[must_use]
    pub fn keep_filled(mut self, keep: bool) -> Self {
        self.keep_filled = keep;
        self
    }

    /// Refuse what a reader would not let a person type, or not.
    #[must_use]
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }
}

/// A widget of a page, and the field it belongs to.
pub(crate) struct Widget {
    /// The widget's object, when it is one (not written in place in `/Annots`).
    pub(crate) id: Option<ObjId>,
    pub(crate) dict: Dict,
    /// The terminal field: the widget itself when it has a `/T`, else its parent.
    pub(crate) field: Option<ObjId>,
    pub(crate) name: String,
}

/// The widgets of page `index`, in the order of its `/Annots`.
pub(crate) fn widgets(doc: &Document, page: ObjId) -> Result<Vec<Widget>> {
    let Some(Object::Dict(page)) = doc.object(page)? else {
        return Ok(Vec::new());
    };
    let items = match page.get(b"Annots") {
        Some(annots) => match pdf::resolved(doc, annots)? {
            Some(Object::Array(items)) => items,
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    if u64::try_from(items.len()).unwrap_or(u64::MAX) > doc.limits().max_annotations {
        return Err(sealwax_core::Error::LimitExceeded {
            what: "max_annotations",
        });
    }
    let mut found = Vec::new();
    for item in items {
        let Some(Object::Dict(dict)) = pdf::resolved(doc, &item)? else {
            continue;
        };
        if dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Widget") {
            continue;
        }
        let id = item.as_ref_id();
        let field = if dict.contains_key(b"T") {
            id
        } else {
            dict.get(b"Parent").and_then(Object::as_ref_id)
        };
        let name = full_name(doc, &dict)?;
        found.push(Widget {
            id,
            dict,
            field,
            name,
        });
    }
    Ok(found)
}

/// The full name of the field a widget belongs to: its partial names from the top, each decoded as
/// the text string it is, joined by periods.
fn full_name(doc: &Document, widget: &Dict) -> Result<String> {
    let mut parts = Vec::new();
    let mut current = widget.clone();
    for _ in 0..pdf::MAX_DEPTH {
        if let Some(Object::String(part)) = current.get(b"T") {
            parts.push(decode_text_string(part));
        }
        match pdf::dict(doc, current.get(b"Parent"))? {
            Some(parent) => current = parent,
            None => break,
        }
    }
    parts.reverse();
    Ok(parts.join("."))
}

pub(crate) fn field_kind(doc: &Document, widget: &Dict) -> Result<FieldKind> {
    let flags = pdf::inherited(doc, widget, b"Ff")?
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Ok(
        match pdf::inherited(doc, widget, b"FT")?
            .as_ref()
            .and_then(Object::as_name)
        {
            Some(b"Tx") => FieldKind::Text,
            Some(b"Btn") if flags & PUSH_BUTTON != 0 => FieldKind::PushButton,
            Some(b"Btn") if flags & RADIO != 0 => FieldKind::Radio,
            Some(b"Btn") => FieldKind::CheckBox,
            Some(b"Ch") if flags & COMBO != 0 => FieldKind::ComboBox,
            Some(b"Ch") => FieldKind::ListBox,
            Some(b"Sig") => FieldKind::Signature,
            _ => FieldKind::Other,
        },
    )
}

/// A name's bytes as text: UTF-8 when they are, Latin-1 otherwise.
#[must_use]
pub fn name_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|b| char::from(*b)).collect(),
    }
}

fn value_of(doc: &Document, value: &Object) -> Result<Option<FieldValue>> {
    Ok(match value {
        Object::String(bytes) => Some(FieldValue::Text(decode_text_string(bytes))),
        Object::Name(name) => Some(FieldValue::Name(name.as_bytes().to_vec())),
        Object::Array(items) => {
            let mut values = Vec::new();
            for item in items {
                match pdf::resolved(doc, item)? {
                    Some(Object::String(bytes)) => values.push(decode_text_string(&bytes)),
                    Some(Object::Name(name)) => values.push(name_text(name.as_bytes())),
                    _ => {}
                }
            }
            Some(FieldValue::List(values))
        }
        _ => None,
    })
}

fn text_of(doc: &Document, value: &Object) -> Result<Option<String>> {
    Ok(match pdf::resolved(doc, value)? {
        Some(Object::String(bytes)) => Some(decode_text_string(&bytes)),
        Some(Object::Name(name)) => Some(name_text(name.as_bytes())),
        _ => None,
    })
}

/// The names of a widget's normal appearances, in the order of their bytes.
fn states(doc: &Document, widget: &Dict) -> Result<Vec<Vec<u8>>> {
    Ok(match flatten::normal_appearance(doc, widget)? {
        Some(Normal::States(states)) => states
            .iter()
            .map(|(name, _)| name.as_bytes().to_vec())
            .collect(),
        _ => Vec::new(),
    })
}

/// The occurrences of fields on page `page_index` (from 0), in the order of its widgets; every
/// page's when `page_index` is `None`.
///
/// # Errors
/// [`sealwax_core::Error::LimitExceeded`] for a page with more annotations than
/// `max_annotations`; [`sealwax_core::Error::Timeout`] past the deadline;
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have.
pub fn fields(doc: &Document, page_index: Option<usize>) -> Result<Vec<Field>> {
    let pages = doc.page_ids();
    let geometry = doc.geometry();
    let indexes: Vec<usize> = match page_index {
        Some(index) if index < pages.len() => vec![index],
        Some(_) => {
            return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
        }
        None => (0..pages.len()).collect(),
    };
    let mut found = Vec::new();
    for index in indexes {
        doc.check_time()?;
        let (Some(Some(page)), Some(page_geometry)) = (pages.get(index), geometry.get(index))
        else {
            continue;
        };
        for widget in widgets(doc, *page)? {
            let dict = &widget.dict;
            let Some(rect) = pdf::rect(dict.get(b"Rect")) else {
                continue;
            };
            let kind = field_kind(doc, dict)?;
            let flags = pdf::inherited(doc, dict, b"Ff")?
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let value = match pdf::inherited(doc, dict, b"V")? {
                Some(value) => value_of(doc, &value)?,
                None => None,
            };
            let label = match pdf::inherited(doc, dict, b"TU")? {
                Some(value) => text_of(doc, &value)?,
                None => None,
            };
            let mut choices = Vec::new();
            if let Some(Object::Array(options)) = pdf::inherited(doc, dict, b"Opt")? {
                for option in &options {
                    match pdf::resolved(doc, option)? {
                        Some(Object::Array(pair)) => {
                            if let [export, display] = pair.as_slice()
                                && let (Some(export), Some(display)) =
                                    (text_of(doc, export)?, text_of(doc, display)?)
                            {
                                choices.push(FieldChoice::Pair { export, display });
                            }
                        }
                        Some(other) => {
                            if let Some(text) = text_of(doc, &other)? {
                                choices.push(FieldChoice::Single(text));
                            }
                        }
                        None => {}
                    }
                }
            }
            let button = matches!(
                kind,
                FieldKind::CheckBox | FieldKind::Radio | FieldKind::PushButton
            );
            let button_caption = if button {
                match pdf::dict(doc, dict.get(b"MK"))? {
                    Some(characteristics) => match characteristics.get(b"CA") {
                        Some(value) => text_of(doc, value)?,
                        None => None,
                    },
                    None => None,
                }
            } else {
                None
            };
            found.push(Field {
                name: widget.name,
                label,
                kind,
                page_index: index,
                rect,
                crop_box: page_geometry.crop_box,
                value,
                state: dict
                    .get(b"AS")
                    .and_then(Object::as_name)
                    .map(<[u8]>::to_vec),
                required: flags & REQUIRED != 0,
                read_only: flags & READ_ONLY != 0,
                max_length: pdf::inherited(doc, dict, b"MaxLen")?.and_then(|v| v.as_i64()),
                choices,
                editable: kind == FieldKind::ComboBox && flags & EDIT != 0,
                button_states: if matches!(kind, FieldKind::CheckBox | FieldKind::Radio) {
                    states(doc, dict)?
                } else {
                    Vec::new()
                },
                button_caption,
            });
        }
    }
    Ok(found)
}

/// Every occurrence's value, by full name, in the order the occurrences come: a text or choice
/// field's value, a button's state (`/AS`, `Off` when it has none), each as text (a name's bytes
/// read as UTF-8, else Latin-1).
///
/// # Errors
/// As [`fields`].
pub fn field_values(doc: &Document) -> Result<Vec<(String, Vec<String>)>> {
    let mut values: Vec<(String, Vec<String>)> = Vec::new();
    for field in fields(doc, None)? {
        let value = match field.kind {
            FieldKind::CheckBox | FieldKind::Radio => {
                name_text(field.state.as_deref().unwrap_or(b"Off"))
            }
            _ => match field.value {
                Some(FieldValue::Text(text)) => text,
                Some(FieldValue::Name(name)) => name_text(&name),
                Some(FieldValue::List(list)) => list.join(", "),
                None => String::new(),
            },
        };
        match values.iter_mut().find(|(name, _)| *name == field.name) {
            Some((_, list)) => list.push(value),
            None => values.push((field.name, vec![value])),
        }
    }
    Ok(values)
}

/// A value to fill a field with: text for a text or choice field, a state's name for a button
/// (its bytes, compared with the widget's states as the document spells them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillValue {
    Text(String),
    Bytes(Vec<u8>),
}

impl FillValue {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Text(text) => text.as_bytes(),
            Self::Bytes(bytes) => bytes,
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Bytes(bytes) => name_text(bytes),
        }
    }
}

/// Fill the fields `values` names (by full name), every occurrence of each:
/// - a text or choice field gets the value (`/V`, on the field: the widget's parent when the widget
///   is a kid), and an appearance drawn for it, as a reader draws it (font size and colour of its
///   `/DA`, automatic size, alignment, maximum length); an empty value clears it;
/// - a button gets the state the value means, found in this order: the state of that name (its
///   bytes, never re-encoded); a state whose name reads as that text (`Fran#E7ais` for
///   `Français`); the state of the widget whose export value (`/Opt`) it is; `Off` for an empty
///   value or `off`, `no`, `n`, `false`, `0`, `unchecked`, `non`; and, for a button with one
///   state besides `Off` (a check box, a radio button alone), that state for `yes`, `y`, `on`,
///   `true`, `1`, `x`, `checked`, `oui` (trimmed, in any case). Each widget that has the state
///   shows it, the others `Off`, and the field's value becomes its name.
///
/// A name no field on the pages has, a signature field or push button, and a button given a value
/// that means none of its states, or could mean several, are refused, the field left as it was
/// ([`Filled::refused`]). Returns
/// the occurrences written, and each field written with the value it held before
/// ([`Filled::written`]).
///
/// # Errors
/// As [`fields`]; [`sealwax_core::Error::LimitExceeded`] when a new object cannot be numbered.
pub fn fill_fields(doc: &mut Document, values: &HashMap<String, FillValue>) -> Result<Filled> {
    fill_fields_with(doc, values, &FillOptions::default())
}

/// As [`fill_fields`], with `options`: the fields already holding another value kept
/// ([`FillOptions::keep_filled`]), what a reader would not let a person type refused
/// ([`FillOptions::strict`]).
///
/// # Errors
/// As [`fill_fields`].
pub fn fill_fields_with(
    doc: &mut Document,
    values: &HashMap<String, FillValue>,
    options: &FillOptions,
) -> Result<Filled> {
    let mut filled = Filled::default();
    let found = fields(doc, None)?;
    let by_name = by_name(&found);
    let mut names: Vec<&String> = values.keys().collect();
    names.sort();
    let buttons: HashSet<&str> = names
        .iter()
        .filter(|name| by_name.get(name.as_str()).is_some_and(|o| is_button(o)))
        .map(|name| name.as_str())
        .collect();
    let exports = button_exports(doc, &buttons)?;
    let mut accepted: HashMap<&str, FillValue> = HashMap::new();
    for name in names {
        let value = &values[name];
        let Some(occurrences) = by_name.get(name.as_str()) else {
            filled.refused.push(name.clone());
            continue;
        };
        let exported = exports.get(name.as_str()).map_or(&[][..], Vec::as_slice);
        let Some((written, value)) = takes(occurrences, value, exported, options.strict) else {
            filled.refused.push(name.clone());
            continue;
        };
        let previous = held(occurrences);
        if options.keep_filled && !empty(occurrences, &previous) && previous != written {
            filled.kept.push(name.clone());
            continue;
        }
        filled.written.insert(
            name.clone(),
            Written {
                value: written,
                previous,
            },
        );
        accepted.insert(name, value);
    }
    let acro_form = match doc.object(doc.catalog_id())? {
        Some(Object::Dict(catalog)) => {
            pdf::dict(doc, catalog.get(b"AcroForm"))?.unwrap_or_default()
        }
        _ => Dict::new(),
    };
    let mut font: Option<ObjId> = None;
    for page in doc.page_ids().into_iter().flatten() {
        doc.check_time()?;
        for widget in widgets(doc, page)? {
            let Some(value) = accepted.get(widget.name.as_str()) else {
                continue;
            };
            let Some(widget_id) = widget.id else {
                continue;
            };
            let kind = field_kind(doc, &widget.dict)?;
            let field_id = widget.field.unwrap_or(widget_id);
            match kind {
                FieldKind::CheckBox | FieldKind::Radio => {
                    let wanted = value.bytes();
                    let has_state = states(doc, &widget.dict)?
                        .iter()
                        .any(|state| state == wanted);
                    let state: &[u8] = if has_state && !wanted.is_empty() {
                        wanted
                    } else {
                        b"Off"
                    };
                    set_key(doc, widget_id, "AS", Object::Name(Name::from(state)))?;
                    let field_value: &[u8] = if wanted.is_empty() { b"Off" } else { wanted };
                    set_key(doc, field_id, "V", Object::Name(Name::from(field_value)))?;
                }
                FieldKind::Text | FieldKind::ComboBox | FieldKind::ListBox => {
                    let text = value.text();
                    set_key(
                        doc,
                        field_id,
                        "V",
                        Object::String(sealwax_core::encode_text_string(&text)),
                    )?;
                    if field_id != widget_id {
                        remove_key(doc, widget_id, "V")?;
                    }
                    let Some(Object::Dict(dict)) = doc.object(widget_id)? else {
                        continue;
                    };
                    let Some(rect) = pdf::rect(dict.get(b"Rect")) else {
                        continue;
                    };
                    let choice = kind != FieldKind::Text;
                    let field = flatten::text_field(doc, &acro_form, &dict, choice)?;
                    // Drawn in the widget's own frame: turned by `/MK /R`, its sides swap.
                    let turns = flatten::widget_turns(doc, &dict)?;
                    let (mut width, mut height) = (rect.x2 - rect.x1, rect.y2 - rect.y1);
                    if turns % 2 == 1 {
                        (width, height) = (height, width);
                    }
                    let (drawing, replaced) = draw::text_field(&field, width, height);
                    filled.characters_replaced += replaced;
                    let font_id = match font {
                        Some(id) => id,
                        None => *font.insert(doc.add_object(flatten::font_dict(Font::Helvetica))?),
                    };
                    let mut fonts = Dict::new();
                    fonts.insert("Helv", Object::Ref(font_id));
                    let mut resources = Dict::new();
                    resources.insert("Font", fonts);
                    let form = doc.add_object(flatten::turned_form_stream(
                        &drawing, width, height, resources, turns,
                    ))?;
                    let mut appearances = Dict::new();
                    appearances.insert("N", Object::Ref(form));
                    set_key(doc, widget_id, "AP", Object::Dict(appearances))?;
                }
                _ => continue,
            }
            filled.occurrences += 1;
        }
    }
    Ok(filled)
}

/// Give each field `written` names the value it held before, unless it was changed since (its
/// value is no longer the one written) or refuses it: those are returned, in the order of their
/// names' bytes, left as they are. A name the form no longer has is passed over.
///
/// # Errors
/// As [`fill_fields`].
pub fn unfill_fields(
    doc: &mut Document,
    written: &BTreeMap<String, Written>,
) -> Result<Vec<String>> {
    let found = fields(doc, None)?;
    let by_name = by_name(&found);
    let mut kept = Vec::new();
    let mut restored = HashMap::new();
    for (name, field) in written {
        let Some(occurrences) = by_name.get(name.as_str()) else {
            continue;
        };
        if held(occurrences) != field.value {
            kept.push(name.clone());
            continue;
        }
        let value = if is_button(occurrences) {
            // The state as the document spells it: its bytes, which may not be UTF-8.
            occurrences
                .iter()
                .flat_map(|occurrence| &occurrence.button_states)
                .find(|state| name_text(state) == field.previous)
                .map_or_else(
                    || FillValue::Text(String::new()),
                    |state| FillValue::Bytes(state.clone()),
                )
        } else {
            FillValue::Text(field.previous.clone())
        };
        restored.insert(name.clone(), value);
    }
    let filled = fill_fields(doc, &restored)?;
    kept.extend(filled.refused);
    kept.sort();
    kept.dedup();
    Ok(kept)
}

/// The occurrences of each name.
fn by_name(found: &[Field]) -> HashMap<&str, Vec<&Field>> {
    let mut by_name: HashMap<&str, Vec<&Field>> = HashMap::new();
    for field in found {
        by_name.entry(field.name.as_str()).or_default().push(field);
    }
    by_name
}

fn is_button(occurrences: &[&Field]) -> bool {
    occurrences
        .first()
        .is_some_and(|field| matches!(field.kind, FieldKind::CheckBox | FieldKind::Radio))
}

/// A field's value as one text: a text or choice field's (several joined by `, `, empty when it
/// has none), a button's state (`Off` when it has none).
fn held(occurrences: &[&Field]) -> String {
    let Some(first) = occurrences.first() else {
        return String::new();
    };
    match &first.value {
        Some(FieldValue::Text(text)) => text.clone(),
        Some(FieldValue::Name(name)) => name_text(name),
        Some(FieldValue::List(list)) => list.join(", "),
        None if is_button(occurrences) => occurrences
            .iter()
            .filter_map(|occurrence| occurrence.state.as_deref())
            .find(|state| *state != b"Off")
            .map_or_else(|| "Off".to_owned(), name_text),
        None => String::new(),
    }
}

/// A field with no value: an empty text, a button `Off`.
fn empty(occurrences: &[&Field], value: &str) -> bool {
    value.is_empty() || (is_button(occurrences) && value == "Off")
}

/// A button's export values, each with the state of its widget.
type Exports = Vec<(String, Vec<u8>)>;

/// The export values of the buttons `names` names (`/Opt`, one per widget in the order of the
/// field's `/Kids`), each with the state of its widget, by full name.
fn button_exports(doc: &Document, names: &HashSet<&str>) -> Result<HashMap<String, Exports>> {
    let mut exports: HashMap<String, Exports> = HashMap::new();
    if names.is_empty() {
        return Ok(exports);
    }
    for page in doc.page_ids().into_iter().flatten() {
        doc.check_time()?;
        for widget in widgets(doc, page)? {
            if !names.contains(widget.name.as_str()) {
                continue;
            }
            let Some(Object::Array(options)) = pdf::inherited(doc, &widget.dict, b"Opt")? else {
                continue;
            };
            // The widget's place among its field's kids; a widget that is its own field is the
            // only one.
            let index = match (widget.field, widget.id) {
                (Some(field), Some(id)) if field != id => {
                    let Some(Object::Dict(parent)) = doc.object(field)? else {
                        continue;
                    };
                    let kids = match parent.get(b"Kids") {
                        Some(kids) => match pdf::resolved(doc, kids)? {
                            Some(Object::Array(kids)) => kids,
                            _ => continue,
                        },
                        None => continue,
                    };
                    let Some(index) = kids.iter().position(|kid| *kid == Object::Ref(id)) else {
                        continue;
                    };
                    index
                }
                _ => 0,
            };
            let Some(option) = options.get(index) else {
                continue;
            };
            let Some(export) = text_of(doc, option)? else {
                continue;
            };
            let Some(state) = states(doc, &widget.dict)?
                .into_iter()
                .find(|state| state.as_slice() != b"Off")
            else {
                continue;
            };
            exports
                .entry(widget.name.clone())
                .or_default()
                .push((export, state));
        }
    }
    Ok(exports)
}

/// Values that check a check box, whatever its state is named; compared trimmed, ignoring
/// case.
const CHECKED: [&str; 8] = ["yes", "y", "on", "true", "1", "x", "checked", "oui"];
/// Values that uncheck a button.
const UNCHECKED: [&str; 8] = ["", "off", "no", "n", "false", "0", "unchecked", "non"];

/// The state a button takes for `value`, by order of precedence: the state of that name (its
/// bytes); a state whose name reads as that text (`Fran#E7ais` for `Français`); the state of
/// the widget whose export value (`/Opt`) it is; `Off` for a value of [`UNCHECKED`]; the one
/// state other than `Off` its widgets have, for a value of [`CHECKED`]. Empty for `Off`; `None`
/// when no state is meant, or several could be.
fn button_state(
    occurrences: &[&Field],
    value: &FillValue,
    exports: &[(String, Vec<u8>)],
) -> Option<Vec<u8>> {
    let mut on: Vec<&Vec<u8>> = Vec::new();
    for state in occurrences.iter().flat_map(|o| &o.button_states) {
        if state.as_slice() != b"Off" && !on.contains(&state) {
            on.push(state);
        }
    }
    let wanted = value.bytes();
    if wanted == b"Off" {
        return Some(Vec::new());
    }
    if let Some(state) = on.iter().find(|state| state.as_slice() == wanted) {
        return Some((*state).clone());
    }
    let text = value.text();
    if let Some(state) = on.iter().find(|state| name_text(state) == text) {
        return Some((*state).clone());
    }
    if let Some((_, state)) = exports.iter().find(|(export, _)| *export == text) {
        return Some(state.clone());
    }
    let alias = text.trim().to_lowercase();
    if UNCHECKED.contains(&alias.as_str()) {
        return Some(Vec::new());
    }
    match on.as_slice() {
        [only] if CHECKED.contains(&alias.as_str()) => Some((*only).clone()),
        _ => None,
    }
}

/// The value the field will hold once given `value`, read as [`held`] reads it, and the value
/// to write (a button's state, by its bytes); `None` when the field refuses it.
fn takes(
    occurrences: &[&Field],
    value: &FillValue,
    exports: &[(String, Vec<u8>)],
    strict: bool,
) -> Option<(String, FillValue)> {
    let first = occurrences.first()?;
    if strict && first.read_only {
        return None;
    }
    match first.kind {
        FieldKind::CheckBox | FieldKind::Radio => {
            let state = button_state(occurrences, value, exports)?;
            let written = if state.is_empty() {
                "Off".to_owned()
            } else {
                name_text(&state)
            };
            Some((written, FillValue::Bytes(state)))
        }
        FieldKind::Text | FieldKind::ComboBox | FieldKind::ListBox => {
            let text = value.text();
            if strict {
                let drawn =
                    |c: char| matches!(c, '\n' | '\r' | '\t') || text::win_ansi(c).is_some();
                if !text.chars().all(drawn) {
                    return None;
                }
                let too_long = first.max_length.is_some_and(|max| {
                    i64::try_from(text.chars().count()).unwrap_or(i64::MAX) > max
                });
                if first.kind == FieldKind::Text && too_long {
                    return None;
                }
                let offered = first.choices.iter().any(|choice| match choice {
                    FieldChoice::Single(export) | FieldChoice::Pair { export, .. } => {
                        *export == text
                    }
                });
                if first.kind != FieldKind::Text && !text.is_empty() && !first.editable && !offered
                {
                    return None;
                }
            }
            Some((text.clone(), FillValue::Text(text)))
        }
        _ => None,
    }
}

fn set_key(doc: &mut Document, id: ObjId, key: &str, value: Object) -> Result<()> {
    match doc.object(id)? {
        Some(Object::Dict(mut dict)) => {
            dict.insert(key, value);
            doc.set_object(id, Object::Dict(dict));
        }
        Some(Object::Stream(mut stream)) => {
            stream.dict.insert(key, value);
            doc.set_object(id, Object::Stream(stream));
        }
        _ => {}
    }
    Ok(())
}

fn remove_key(doc: &mut Document, id: ObjId, key: &str) -> Result<()> {
    if let Some(Object::Dict(mut dict)) = doc.object(id)?
        && dict.remove(key.as_bytes()).is_some()
    {
        doc.set_object(id, Object::Dict(dict));
    }
    Ok(())
}
