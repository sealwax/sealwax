//! A filled form made a template again: its values cleared, what a person added over its pages
//! taken out, and what named the person who filled it.
//!
//! What the pages' content draws is left as it is: a value printed on a page is not a field's.

use std::collections::{BTreeSet, HashMap};

use sealwax_core::{Dict, Document, Name, Object, encode_text_string};

use crate::Result;
use crate::create::{form, form_tree, store_form};
use crate::fields::{FieldKind, FillValue, field_kind, fields, fill_fields, widgets};
use crate::pdf;

/// The markup annotations a person adds over a page (ISO 32000-1, 12.5.6), their pop-ups
/// included. Links and widgets are the form's own.
const ADDED: &[&[u8]] = &[
    b"Text",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Stamp",
    b"Caret",
    b"Ink",
    b"FileAttachment",
    b"Sound",
    b"Redact",
    b"Popup",
];

/// What a document holds that making it a template clears or takes out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct TemplateContent {
    pub pages: usize,
    /// The fields that hold a value: each name once, those without a widget on a page included.
    pub fields: usize,
    /// The annotations a person added over the pages (markup and their pop-ups).
    pub annotations: usize,
}

/// How [`clear_template`] makes a template.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TemplateOptions {
    /// The template's title (`/Title` of its information).
    pub title: String,
    /// Keep the annotations a person added over the pages.
    pub keep_annotations: bool,
}

impl TemplateOptions {
    /// A template titled `title`, the annotations added taken out.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            keep_annotations: false,
        }
    }

    /// Keep the annotations added, or not.
    #[must_use]
    pub fn keep_annotations(mut self, keep: bool) -> Self {
        self.keep_annotations = keep;
        self
    }
}

/// What [`clear_template`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Cleared {
    /// The fields cleared, each name once.
    pub fields: usize,
    /// The annotations taken out.
    pub annotations_removed: usize,
}

/// What `doc` holds that [`clear_template`] clears or takes out.
///
/// # Errors
/// As [`fields`].
pub fn template_content(doc: &Document) -> Result<TemplateContent> {
    let mut annotations = 0;
    for page in doc.page_ids().into_iter().flatten() {
        doc.check_time()?;
        annotations += added_annotations(doc, &page_annotations(doc, page)?)?.len();
    }
    Ok(TemplateContent {
        pages: doc.page_count(),
        fields: field_names(doc)?.len(),
        annotations,
    })
}

/// Make `doc` a template: every field cleared (no value, a button `Off`, a list box's
/// selection gone), its empty appearance drawn; the XFA form taken out; unless
/// [`TemplateOptions::keep_annotations`], the annotations a person added over the pages taken
/// out (markup and their pop-ups: links and widgets stay); the information's title set, its
/// author, subject and keywords taken out, and the XMP metadata that repeats them too.
///
/// The values and appearances replaced are no longer reached: a full save
/// ([`Document::save`]) leaves them out of the file. An incremental save would keep them in the
/// revision it adds to.
///
/// # Errors
/// As [`fields`]; [`sealwax_core::Error::LimitExceeded`] when a new object cannot be numbered.
pub fn clear_template(doc: &mut Document, options: &TemplateOptions) -> Result<Cleared> {
    let cleared = field_names(doc)?.len();
    // The empty appearances first: the filled ones are then reached from nowhere.
    let texts: HashMap<String, FillValue> = fields(doc, None)?
        .into_iter()
        .filter(|field| {
            matches!(
                field.kind,
                FieldKind::Text | FieldKind::ComboBox | FieldKind::ListBox
            )
        })
        .map(|field| (field.name, FillValue::Text(String::new())))
        .collect();
    fill_fields(doc, &texts)?;
    // The values: on the field tree, and on the widgets of the pages (a widget the tree does not
    // reach, or holding a value of its own).
    for node in form_tree(doc)? {
        if let Some(id) = node.id {
            clear_values(doc, id, false)?;
        }
    }
    let mut annotations_removed = 0;
    for page in doc.page_ids().into_iter().flatten() {
        doc.check_time()?;
        for widget in widgets(doc, page)? {
            let Some(id) = widget.id else {
                continue;
            };
            let button = matches!(
                field_kind(doc, &widget.dict)?,
                FieldKind::CheckBox | FieldKind::Radio
            );
            clear_values(doc, id, button)?;
            if let Some(field) = widget.field {
                clear_values(doc, field, false)?;
            }
        }
        if !options.keep_annotations {
            annotations_removed += remove_added(doc, page)?;
        }
    }
    let (mut acro_form, location) = form(doc)?;
    if acro_form.remove(b"XFA").is_some() {
        store_form(doc, acro_form, location)?;
    }
    let catalog_id = doc.catalog_id();
    if let Some(Object::Dict(mut catalog)) = doc.object(catalog_id)? {
        // The XMP packet repeats the information; a reader asked to render the XFA has none.
        let metadata = catalog.remove(b"Metadata").is_some();
        let rendering = catalog.remove(b"NeedsRendering").is_some();
        if metadata || rendering {
            doc.set_object(catalog_id, Object::Dict(catalog));
        }
    }
    set_information(doc, &options.title)?;
    Ok(Cleared {
        fields: cleared,
        annotations_removed,
    })
}

/// The names of the fields that hold a value, each once.
fn field_names(doc: &Document) -> Result<BTreeSet<String>> {
    let mut names: BTreeSet<String> = form_tree(doc)?
        .into_iter()
        .filter(|node| node.terminal)
        .map(|node| node.name)
        .collect();
    names.extend(fields(doc, None)?.into_iter().map(|field| field.name));
    Ok(names)
}

/// The value of field or widget `id` taken out (`/V`, its rich text `/RV`, a list box's
/// selection `/I`); a button's widget turned `Off`.
fn clear_values(doc: &mut Document, id: sealwax_core::ObjId, button: bool) -> Result<()> {
    let Some(Object::Dict(mut dict)) = doc.object(id)? else {
        return Ok(());
    };
    let mut changed = false;
    for key in [&b"V"[..], b"RV", b"I"] {
        changed |= dict.remove(key).is_some();
    }
    if button && dict.get(b"AS").and_then(Object::as_name) != Some(&b"Off"[..]) {
        dict.insert("AS", Name::from("Off"));
        changed = true;
    }
    if changed {
        doc.set_object(id, Object::Dict(dict));
    }
    Ok(())
}

/// The entries of a page's `/Annots`.
fn page_annotations(doc: &Document, page: sealwax_core::ObjId) -> Result<Vec<Object>> {
    let Some(Object::Dict(dict)) = doc.object(page)? else {
        return Ok(Vec::new());
    };
    Ok(match dict.get(b"Annots") {
        Some(annots) => match pdf::resolved(doc, annots)? {
            Some(Object::Array(items)) => items,
            _ => Vec::new(),
        },
        None => Vec::new(),
    })
}

/// The positions in `items` of the annotations a person added.
fn added_annotations(doc: &Document, items: &[Object]) -> Result<Vec<usize>> {
    let mut added = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(Object::Dict(annotation)) = pdf::resolved(doc, item)?
            && let Some(subtype) = annotation.get(b"Subtype").and_then(Object::as_name)
            && ADDED.contains(&subtype)
        {
            added.push(index);
        }
    }
    Ok(added)
}

/// The annotations a person added taken out of `page`; how many.
fn remove_added(doc: &mut Document, page: sealwax_core::ObjId) -> Result<usize> {
    let items = page_annotations(doc, page)?;
    let added = added_annotations(doc, &items)?;
    if added.is_empty() {
        return Ok(0);
    }
    let kept: Vec<Object> = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| (!added.contains(&index)).then_some(item))
        .collect();
    let Some(Object::Dict(mut dict)) = doc.object(page)? else {
        return Ok(0);
    };
    match dict.get(b"Annots") {
        // An array of its own: written there, the page left as it is.
        Some(Object::Ref(id)) => doc.set_object(*id, Object::Array(kept)),
        _ => {
            dict.insert("Annots", Object::Array(kept));
            doc.set_object(page, Object::Dict(dict));
        }
    }
    Ok(added.len())
}

/// The information's title set, its author, subject and keywords taken out.
fn set_information(doc: &mut Document, title: &str) -> Result<()> {
    let id = doc.info_id();
    let mut info = match id {
        Some(id) => match doc.object(id)? {
            Some(Object::Dict(info)) => info,
            _ => Dict::new(),
        },
        None => Dict::new(),
    };
    for key in [&b"Author"[..], b"Subject", b"Keywords"] {
        info.remove(key);
    }
    info.insert("Title", Object::String(encode_text_string(title)));
    match id {
        Some(id) => doc.set_object(id, Object::Dict(info)),
        None => {
            let id = doc.add_object(Object::Dict(info))?;
            doc.set_info_id(Some(id));
        }
    }
    Ok(())
}
