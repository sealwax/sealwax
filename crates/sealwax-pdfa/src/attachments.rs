//! Embedded files as PDF/A-3 wants them (ISO 19005-3, 6.8). Each file specification:
//! - names its file twice, `/F` and `/UF`;
//! - says how its file relates to the document (`/AFRelationship`, `/Unspecified` when nothing
//!   says more);
//! - is associated with what it belongs to through an `/AF` array: the catalog for the files the
//!   document embeds (`/EmbeddedFiles`), its annotation for a file attachment annotation's.
//!
//! Each embedded file stream gives its MIME type (`/Subtype`), guessed from the file's
//! extension, `application/octet-stream` when the extension says nothing.

use std::collections::HashSet;

use sealwax_core::{
    Dict, Document, Name, ObjId, Object, Result, encode_text_string, is_mime_type, mime_type_for,
};

use crate::Report;
use crate::edit::{self, Location};
use crate::metadata::text_string;
use crate::scan::Scan;

/// How deep the name tree is followed, and how many of its nodes are read.
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 100_000;

/// A file specification and what it is associated with.
struct Found {
    id: ObjId,
    /// Its key in the name tree, a text string.
    key: Option<Vec<u8>>,
    /// The annotation that attaches it, when it is a file attachment annotation's.
    annotation: Option<Location>,
}

pub(crate) fn apply(doc: &mut Document, scan: &Scan, report: &mut Report) -> Result<()> {
    let mut found = Vec::new();
    let tree = Location::of(doc.catalog_id())
        .key(b"Names")
        .key(b"EmbeddedFiles");
    name_tree(doc, &tree, &mut found, report)?;
    for location in &scan.annotations {
        let Some(annotation) = edit::read_dict(doc, location)? else {
            continue;
        };
        if annotation.get(b"Subtype").and_then(Object::as_name) != Some(b"FileAttachment") {
            continue;
        }
        let id = match annotation.get(b"FS") {
            Some(Object::Ref(id)) => *id,
            Some(Object::Dict(spec)) => {
                let id = doc.add_object(Object::Dict(spec.clone()))?;
                edit::edit_dict(doc, location, false, |dict| {
                    dict.insert("FS", Object::Ref(id));
                })?;
                id
            }
            _ => continue,
        };
        found.push(Found {
            id,
            key: None,
            annotation: Some(location.clone()),
        });
    }
    let mut associated = associations(doc, &mut found, report)?;
    let mut described = HashSet::new();
    for item in found {
        doc.check_time()?;
        let Some(Object::Dict(spec)) = doc.object(item.id)? else {
            if described.insert(item.id) {
                report.residual_add("file_specification_unreadable", 1);
            }
            continue;
        };
        if described.insert(item.id) {
            describe(doc, item.id, spec, item.key.as_deref(), report)?;
        }
        // One an `/AF` array already refers to keeps its association.
        if associated.contains(&item.id) {
            continue;
        }
        let owner = item
            .annotation
            .clone()
            .unwrap_or_else(|| Location::of(doc.catalog_id()));
        if associate(doc, &owner, item.id)? {
            associated.insert(item.id);
            report.change("embedded_file_associated", 1);
        } else {
            report.residual_add("embedded_file_not_associated", 1);
        }
    }
    Ok(())
}

/// Go through every object of the document:
/// - the file specifications its `/AF` arrays refer to are associated already: each is added to
///   `found` to be described, and returned; one written in place in the array is made an
///   indirect object first;
/// - every other file specification of an embedded file (`/EF`), wherever it is referred to
///   from (an action, a structure element), is added to `found`, to be described and associated
///   with the document.
///
/// One written in place elsewhere than in a name tree, a file attachment annotation or an `/AF`
/// array cannot be associated: it is counted as `embedded_file_not_associated`.
fn associations(
    doc: &mut Document,
    found: &mut Vec<Found>,
    report: &mut Report,
) -> Result<HashSet<ObjId>> {
    let mut associated = HashSet::new();
    for (index, id) in doc.reachable_objects()?.into_iter().enumerate() {
        if index % 1024 == 0 {
            doc.check_time()?;
        }
        let dict = match doc.object_without_data(id)? {
            Some(Object::Dict(dict)) => dict,
            Some(Object::Stream(stream)) => stream.dict,
            _ => continue,
        };
        if dict.contains_key(b"EF") {
            found.push(Found {
                id,
                key: None,
                annotation: None,
            });
        }
        let in_place: usize = dict
            .iter()
            .filter(|(key, _)| !matches!(key.as_bytes(), b"AF" | b"FS"))
            .map(|(_, value)| specs_in_place(value, 0))
            .sum();
        report.residual_add("embedded_file_not_associated", in_place);
        let (location, files) = match dict.get(b"AF") {
            Some(Object::Array(files)) => (Location::of(id).key(b"AF"), files.clone()),
            Some(Object::Ref(array)) => match doc.object(*array)? {
                Some(Object::Array(files)) => (Location::of(*array), files),
                _ => continue,
            },
            _ => continue,
        };
        for (slot, file) in files.iter().enumerate() {
            let spec = match file {
                Object::Ref(spec) => *spec,
                Object::Dict(spec) => {
                    let spec_id = doc.add_object(Object::Dict(spec.clone()))?;
                    if !replace_in_array(doc, &location, slot, spec_id)? {
                        report.residual_add("embedded_file_not_associated", 1);
                        continue;
                    }
                    spec_id
                }
                _ => {
                    report.residual_add("file_specification_unreadable", 1);
                    continue;
                }
            };
            associated.insert(spec);
            found.push(Found {
                id: spec,
                key: None,
                annotation: None,
            });
        }
    }
    Ok(associated)
}

/// The dictionaries of embedded files (`/EF`) written in place within `value`, which nothing can
/// refer to from an `/AF` array.
fn specs_in_place(value: &Object, depth: usize) -> usize {
    if depth > MAX_DEPTH {
        return 0;
    }
    match value {
        Object::Dict(dict) => {
            usize::from(dict.contains_key(b"EF"))
                + dict
                    .iter()
                    .map(|(_, value)| specs_in_place(value, depth + 1))
                    .sum::<usize>()
        }
        Object::Array(items) => items
            .iter()
            .map(|item| specs_in_place(item, depth + 1))
            .sum(),
        _ => 0,
    }
}

/// Put a reference to `id` at `slot` of the array at `location`: the array object itself when
/// `location` has no path, else the `/AF` array of the dictionary it names.
fn replace_in_array(
    doc: &mut Document,
    location: &Location,
    slot: usize,
    id: ObjId,
) -> Result<bool> {
    if location.path.is_empty() {
        let Some(Object::Array(mut items)) = doc.object(location.owner)? else {
            return Ok(false);
        };
        let Some(item) = items.get_mut(slot) else {
            return Ok(false);
        };
        *item = Object::Ref(id);
        doc.set_object(location.owner, Object::Array(items));
        return Ok(true);
    }
    let mut replaced = false;
    edit::edit_dict(doc, &Location::of(location.owner), false, |dict| {
        if let Some(Object::Array(items)) = dict.get_mut(b"AF")
            && let Some(item) = items.get_mut(slot)
        {
            *item = Object::Ref(id);
            replaced = true;
        }
    })?;
    Ok(replaced)
}

/// Gather the file specifications of the name tree at `node`: each made an indirect object, so
/// that an `/AF` array can refer to it.
fn name_tree(
    doc: &mut Document,
    root: &Location,
    found: &mut Vec<Found>,
    report: &mut Report,
) -> Result<()> {
    let mut seen: HashSet<ObjId> = HashSet::new();
    let mut stack = vec![(root.clone(), 0)];
    let mut nodes = 0;
    while let Some((node, depth)) = stack.pop() {
        nodes += 1;
        if depth > MAX_DEPTH || nodes > MAX_NODES {
            report.residual_add("embedded_files_unread", 1);
            break;
        }
        if node.path.is_empty() && !seen.insert(node.owner) {
            continue;
        }
        let Some(dict) = edit::read_dict(doc, &node)? else {
            continue;
        };
        if let Some(kids) = edit::resolved(doc, dict.get(b"Kids").unwrap_or(&Object::Null))?
            && let Some(kids) = kids.as_array()
        {
            for (index, kid) in kids.iter().enumerate() {
                let kid = match kid {
                    Object::Ref(id) => Location::of(*id),
                    Object::Dict(_) => node.key(b"Kids").index(index),
                    _ => continue,
                };
                stack.push((kid, depth + 1));
            }
        }
        let Some(Object::Array(names)) =
            edit::resolved(doc, dict.get(b"Names").unwrap_or(&Object::Null))?
        else {
            continue;
        };
        for (index, [name, value]) in names.as_chunks::<2>().0.iter().enumerate() {
            let key = name.as_string().map(<[u8]>::to_vec);
            let id = match value {
                Object::Ref(id) => *id,
                Object::Dict(spec) => {
                    let id = doc.add_object(Object::Dict(spec.clone()))?;
                    let slot = 2 * index + 1;
                    let replaced = replace_in_names(doc, &node, &dict, slot, id)?;
                    if !replaced {
                        report.residual_add("embedded_file_not_associated", 1);
                        continue;
                    }
                    id
                }
                _ => continue,
            };
            found.push(Found {
                id,
                key,
                annotation: None,
            });
        }
    }
    Ok(())
}

/// Put a reference to `id` at `slot` of the `/Names` array of the node at `node` (`dict`).
fn replace_in_names(
    doc: &mut Document,
    node: &Location,
    dict: &Dict,
    slot: usize,
    id: ObjId,
) -> Result<bool> {
    if let Some(Object::Ref(array_id)) = dict.get(b"Names") {
        let Some(Object::Array(mut names)) = doc.object(*array_id)? else {
            return Ok(false);
        };
        let Some(item) = names.get_mut(slot) else {
            return Ok(false);
        };
        *item = Object::Ref(id);
        doc.set_object(*array_id, Object::Array(names));
        return Ok(true);
    }
    let mut replaced = false;
    edit::edit_dict(doc, node, false, |node| {
        if let Some(Object::Array(names)) = node.get_mut(b"Names")
            && let Some(item) = names.get_mut(slot)
        {
            *item = Object::Ref(id);
            replaced = true;
        }
    })?;
    Ok(replaced)
}

/// Give the file specification `id` (`spec`) the entries PDF/A-3 requires, and its embedded
/// files their MIME type.
fn describe(
    doc: &mut Document,
    id: ObjId,
    mut spec: Dict,
    key: Option<&[u8]>,
    report: &mut Report,
) -> Result<()> {
    let name = spec
        .get(b"UF")
        .or_else(|| spec.get(b"F"))
        .and_then(Object::as_string)
        .or(key)
        .map(text_string)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "attachment".to_owned());
    let mut changed = false;
    if spec.get(b"Type").is_none() {
        spec.insert("Type", Name::from("Filespec"));
        changed = true;
    }
    if spec.get(b"UF").and_then(Object::as_string).is_none() {
        spec.insert("UF", Object::String(encode_text_string(&name)));
        changed = true;
    }
    if spec.get(b"F").and_then(Object::as_string).is_none() {
        let ascii: String = name
            .chars()
            .map(|c| {
                if c.is_ascii() && !c.is_ascii_control() {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        spec.insert("F", Object::String(ascii.into_bytes()));
        changed = true;
    }
    if spec
        .get(b"AFRelationship")
        .and_then(Object::as_name)
        .is_none()
    {
        spec.insert("AFRelationship", Name::from("Unspecified"));
        changed = true;
    }
    let files = match spec.get(b"EF") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    if changed {
        doc.set_object(id, Object::Dict(spec));
    }
    let mut typed = false;
    if let Some(Object::Dict(files)) = files {
        let mime = mime_type_for(&name);
        for key in [&b"F"[..], b"UF"] {
            let Some(Object::Ref(stream)) = files.get(key) else {
                continue;
            };
            let mut set = false;
            edit::edit_dict(doc, &Location::of(*stream), false, |dict| {
                let valid = dict
                    .get(b"Subtype")
                    .and_then(Object::as_name)
                    .is_some_and(is_mime_type);
                if !valid {
                    dict.insert("Subtype", Name::from(mime));
                    set = true;
                }
            })?;
            typed |= set;
        }
    }
    report.change("embedded_file_described", usize::from(changed || typed));
    Ok(())
}

/// Add `id` to the `/AF` array of the dictionary at `location`, unless it is there already.
/// Returns whether it was added.
fn associate(doc: &mut Document, location: &Location, id: ObjId) -> Result<bool> {
    let Some(dict) = edit::read_dict(doc, location)? else {
        return Ok(false);
    };
    let reference = Object::Ref(id);
    if let Some(Object::Ref(array_id)) = dict.get(b"AF") {
        let mut files = match doc.object(*array_id)? {
            Some(Object::Array(files)) => files,
            _ => Vec::new(),
        };
        if files.contains(&reference) {
            return Ok(false);
        }
        files.push(reference);
        doc.set_object(*array_id, Object::Array(files));
        return Ok(true);
    }
    let mut added = false;
    edit::edit_dict(doc, location, false, |dict| {
        let mut files = match dict.remove(b"AF") {
            Some(Object::Array(files)) => files,
            _ => Vec::new(),
        };
        if !files.contains(&reference) {
            files.push(reference);
            added = true;
        }
        dict.insert("AF", Object::Array(files));
    })?;
    Ok(added)
}

#[cfg(test)]
mod tests {
    use sealwax_core::{is_mime_type, mime_type_for};

    #[test]
    fn mime_types_and_names() {
        assert!(is_mime_type(b"text/plain"));
        assert!(is_mime_type(b"application/vnd.ms-excel"));
        assert!(!is_mime_type(b"text"));
        assert!(!is_mime_type(b"text/plain; charset=utf-8"));
        assert!(!is_mime_type(b"/plain"));
        assert_eq!(mime_type_for("Facture.XML"), "text/xml");
        assert_eq!(mime_type_for("note"), "application/octet-stream");
    }
}
