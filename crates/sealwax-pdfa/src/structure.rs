//! Level a: a structure tree (ISO 19005-3, 6.7.2 to 6.7.4), at its minimal level (N0).
//!
//! - The catalog says the document is tagged (`/MarkInfo << /Marked true >>`).
//! - A document without a structure tree has been given one by [`crate::tagging`] (level N1),
//!   its content tied to it. Should it still have none, it gets one holding a single `Document`
//!   element.
//! - A tree the document has is kept. Its role map is repaired where veraPDF checks it: every
//!   structure type that is not standard leads to a standard one, without a cycle, and a
//!   standard type is not mapped to another. A type nothing maps becomes `NonStruct`, whose
//!   descendants readers handle normally (`Private` would make them skip its descendants).
//! - A language identifier that is not one is removed, and the catalog's `/Lang` is set as
//!   [`crate::language`] decides.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use sealwax_core::{Dict, Document, Name, ObjId, Object, Result};

use crate::Report;
use crate::edit::{self, Location};
use crate::language;
use crate::metadata::text_string;

/// The standard structure types of PDF 1.7 (ISO 32000-1, 14.8.4).
const STANDARD: &[&[u8]] = &[
    b"Document",
    b"Part",
    b"Art",
    b"Sect",
    b"Div",
    b"BlockQuote",
    b"Caption",
    b"TOC",
    b"TOCI",
    b"Index",
    b"NonStruct",
    b"Private",
    b"P",
    b"H",
    b"H1",
    b"H2",
    b"H3",
    b"H4",
    b"H5",
    b"H6",
    b"L",
    b"LI",
    b"Lbl",
    b"LBody",
    b"Table",
    b"TR",
    b"TH",
    b"TD",
    b"THead",
    b"TBody",
    b"TFoot",
    b"Span",
    b"Quote",
    b"Note",
    b"Reference",
    b"BibEntry",
    b"Code",
    b"Link",
    b"Annot",
    b"Ruby",
    b"RB",
    b"RT",
    b"RP",
    b"Warichu",
    b"WT",
    b"WP",
    b"Figure",
    b"Formula",
    b"Form",
];

/// How many objects of a structure tree are walked: the rest is not looked at.
const MAX_NODES: usize = 1 << 20;

fn standard(name: &[u8]) -> bool {
    STANDARD.contains(&name)
}

/// Where the language comes from, when the document declares none.
pub(crate) struct Languages<'a, F: FnOnce() -> Option<&'static str>> {
    /// The caller's.
    pub(crate) given: Option<&'a str>,
    /// The one the words of the document tell.
    pub(crate) detect: F,
}

/// Tag the document at level N0, and settle its language.
pub(crate) fn apply<F: FnOnce() -> Option<&'static str>>(
    doc: &mut Document,
    languages: Languages<'_, F>,
    report: &mut Report,
) -> Result<()> {
    let catalog = Location::of(doc.catalog_id());
    mark(doc, &catalog, report)?;
    tree(doc, &catalog, report)?;
    lang(doc, &catalog, languages, report)
}

/// `/MarkInfo << /Marked true >>` (6.7.2.2).
fn mark(doc: &mut Document, catalog: &Location, report: &mut Report) -> Result<()> {
    let info = match edit::read_dict(doc, catalog)?.and_then(|root| root.get(b"MarkInfo").cloned())
    {
        Some(value) => edit::resolved(doc, &value)?,
        None => None,
    };
    let marked = match info
        .as_ref()
        .and_then(|info| info.as_dict()?.get(b"Marked"))
    {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    if marked == Some(Object::Bool(true)) {
        return Ok(());
    }
    let edited = info.as_ref().and_then(Object::as_dict).is_some()
        && edit::edit_dict(doc, &catalog.key(b"MarkInfo"), false, |info| {
            info.insert("Marked", true);
        })?;
    if !edited {
        let mut info = Dict::new();
        info.insert("Marked", true);
        edit::edit_dict(doc, catalog, false, |root| {
            root.insert("MarkInfo", info);
        })?;
    }
    report.change("marked", 1);
    Ok(())
}

/// A structure tree (6.7.3.3) whose role map leads every type to a standard one (6.7.3.4).
fn tree(doc: &mut Document, catalog: &Location, report: &mut Report) -> Result<()> {
    let root = edit::read_dict(doc, catalog)?.unwrap_or_default();
    let existing = match root.get(b"StructTreeRoot") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    if existing.as_ref().and_then(Object::as_dict).is_none() {
        let tree_id = doc.add_object(Object::Null)?;
        let mut element = Dict::new();
        element.insert("Type", Name::from("StructElem"));
        element.insert("S", Name::from("Document"));
        element.insert("P", tree_id);
        let element_id = doc.add_object(Object::Dict(element))?;
        let mut tree = Dict::new();
        tree.insert("Type", Name::from("StructTreeRoot"));
        tree.insert("K", element_id);
        doc.set_object(tree_id, Object::Dict(tree));
        edit::edit_dict(doc, catalog, false, |root| {
            root.insert("StructTreeRoot", tree_id);
        })?;
        report.change("structure_tree_added", 1);
        return Ok(());
    }
    let tree = catalog.key(b"StructTreeRoot");
    let walked = walk(doc, &tree, report)?;
    for element in &walked.bad_languages {
        edit::edit_dict(doc, element, false, |element| {
            element.remove(b"Lang");
        })?;
    }
    report.change("language_malformed_removed", walked.bad_languages.len());
    role_map(doc, &tree, &walked.types, report)
}

/// What the walk of a structure tree found.
#[derive(Default)]
struct Walked {
    /// The structure types of the elements.
    types: BTreeSet<Vec<u8>>,
    /// Elements whose `/Lang` is no language identifier.
    bad_languages: Vec<Location>,
}

fn walk(doc: &Document, tree: &Location, report: &mut Report) -> Result<Walked> {
    let mut walked = Walked::default();
    let Some(root) = edit::read_dict(doc, tree)? else {
        return Ok(walked);
    };
    let Some(kids) = root.get(b"K") else {
        return Ok(walked);
    };
    let mut stack = vec![(kids.clone(), tree.key(b"K"))];
    let mut seen: HashSet<ObjId> = HashSet::new();
    let mut nodes = 0usize;
    while let Some((value, location)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES {
            report.residual_add("structure_too_large", 1);
            break;
        }
        match value {
            Object::Ref(id) => {
                if seen.insert(id)
                    && let Some(object) = doc.object(id)?
                {
                    stack.push((object, Location::of(id)));
                }
            }
            Object::Array(items) => {
                for (index, item) in items.into_iter().enumerate() {
                    stack.push((item, location.index(index)));
                }
            }
            Object::Dict(element) => {
                if let Some(kind) = element.get(b"S")
                    && let Some(Object::Name(kind)) = edit::resolved(doc, kind)?
                {
                    walked.types.insert(kind.as_bytes().to_vec());
                }
                if let Some(tag) = element.get(b"Lang") {
                    let tag = edit::resolved(doc, tag)?;
                    let fine = tag
                        .as_ref()
                        .and_then(Object::as_string)
                        .is_some_and(|tag| language::valid(&text_string(tag)));
                    if !fine {
                        walked.bad_languages.push(location.clone());
                    }
                }
                if let Some(kids) = element.get(b"K") {
                    stack.push((kids.clone(), location.key(b"K")));
                }
            }
            _ => {}
        }
    }
    Ok(walked)
}

/// Where a structure type leads through the role map: `Err` with the type the chain stops at,
/// for want of a mapping or because it comes back to it.
fn lead<'m>(map: &'m BTreeMap<Vec<u8>, Option<Vec<u8>>>, kind: &'m [u8]) -> Result<(), &'m [u8]> {
    let mut seen: HashSet<&[u8]> = HashSet::new();
    let mut current = kind;
    loop {
        if standard(current) {
            return Ok(());
        }
        if !seen.insert(current) {
            return Err(current);
        }
        match map.get(current) {
            Some(Some(next)) => current = next,
            _ => return Err(current),
        }
    }
}

fn role_map(
    doc: &mut Document,
    tree: &Location,
    types: &BTreeSet<Vec<u8>>,
    report: &mut Report,
) -> Result<()> {
    let location = tree.key(b"RoleMap");
    let mut map: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
    if let Some(dict) = edit::read_dict(doc, &location)? {
        for (key, value) in dict.iter() {
            let value = edit::resolved(doc, value)?;
            let value = value.as_ref().and_then(Object::as_name).map(<[u8]>::to_vec);
            map.insert(key.as_bytes().to_vec(), value);
        }
    }
    // A standard type mapped to anything but a standard type (6.7.3.4, test 3).
    let remapped: Vec<Vec<u8>> = map
        .iter()
        .filter(|(key, value)| standard(key) && !value.as_deref().is_some_and(standard))
        .map(|(key, _)| key.clone())
        .collect();
    for key in &remapped {
        map.remove(key);
    }
    let kinds: Vec<Vec<u8>> = types
        .iter()
        .chain(map.keys())
        .filter(|kind| !standard(kind))
        .cloned()
        .collect();
    let mut added: BTreeSet<Vec<u8>> = BTreeSet::new();
    for kind in &kinds {
        // Each step maps one more type: as many steps as types at most.
        for _ in 0..=kinds.len() {
            let stop = match lead(&map, kind) {
                Ok(()) => break,
                Err(stop) => stop.to_vec(),
            };
            map.insert(stop.clone(), Some(b"NonStruct".to_vec()));
            added.insert(stop);
        }
    }
    if remapped.is_empty() && added.is_empty() {
        return Ok(());
    }
    edit::edit_dict(doc, &location, true, |dict| {
        for key in &remapped {
            dict.remove(key);
        }
        for key in &added {
            dict.insert(Name::new(key.clone()), Name::from("NonStruct"));
        }
    })?;
    report.change("role_map_standard_type_freed", remapped.len());
    report.change("role_map_type_to_nonstruct", added.len());
    Ok(())
}

/// The catalog's `/Lang` (6.7.4), as [`crate::language`] decides it.
fn lang<F: FnOnce() -> Option<&'static str>>(
    doc: &mut Document,
    catalog: &Location,
    languages: Languages<'_, F>,
    report: &mut Report,
) -> Result<()> {
    let root = edit::read_dict(doc, catalog)?.unwrap_or_default();
    if let Some(value) = root.get(b"Lang") {
        let tag = edit::resolved(doc, value)?;
        if tag
            .as_ref()
            .and_then(Object::as_string)
            .is_some_and(|tag| language::valid(&text_string(tag)))
        {
            return Ok(());
        }
        edit::edit_dict(doc, catalog, false, |root| {
            root.remove(b"Lang");
        })?;
        report.change("language_malformed_removed", 1);
    }
    let (tag, source) = match languages.given {
        Some(tag) => (tag, "language_given"),
        None => match (languages.detect)() {
            Some(tag) => (tag, "language_detected"),
            None => return Ok(()),
        },
    };
    edit::edit_dict(doc, catalog, false, |root| {
        root.insert("Lang", Object::String(tag.as_bytes().to_vec()));
    })?;
    report.change(source, 1);
    Ok(())
}
