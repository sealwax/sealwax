//! The document's outline (its bookmarks), flat and depth first, each entry with the page it
//! leads to.

use std::collections::{HashMap, HashSet};

use crate::document::Document;
use crate::error::Result;
use crate::object::{Dict, ObjId, Object};
use crate::text::decode_text_string;

/// The most entries read: an outline listing more is cut there.
const MAX_ENTRIES: usize = 100_000;
/// The deepest level read: deeper entries are left out.
const MAX_LEVEL: usize = 64;
/// How deep a name tree is followed, and how many of its nodes are read for one name.
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_NODES: usize = 4096;
/// How many times a destination may lead to another (a name to a dictionary to an array).
const MAX_INDIRECTIONS: usize = 4;

/// An entry of the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct OutlineEntry {
    /// Its depth, from 1 for the entries at the top.
    pub level: usize,
    /// Its title, decoded (PDFDocEncoding, UTF-16BE or UTF-8).
    pub title: String,
    /// The page it leads to (from 0), or `None` when it leads to no page of this document: an
    /// action of another kind, a destination missing or broken. Never a guess.
    pub page_index: Option<usize>,
}

impl Document {
    /// The outline, entry by entry in the order a reader lists them (each entry, then its
    /// children, then its next sibling), with the page each leads to: through `/Dest`, a `GoTo`
    /// action or the named actions `FirstPage` and `LastPage`; a destination array, or a named
    /// destination (`/Dests` in the catalog, or its `/Names` tree). An entry seen before (a cycle) ends its branch; an outline is cut
    /// past 100,000 entries or 64 levels.
    ///
    /// # Errors
    /// [`crate::Error::Timeout`] past the deadline; [`crate::Error::LimitExceeded`] for an
    /// object nesting deeper than sealwax accepts.
    pub fn outline(&self) -> Result<Vec<OutlineEntry>> {
        let Some(catalog) = self.dict_at(self.catalog_id())? else {
            return Ok(Vec::new());
        };
        let Some(first) = self
            .resolved_dict(catalog.get(b"Outlines"))?
            .and_then(|outlines| outlines.get(b"First").and_then(Object::as_ref_id))
        else {
            return Ok(Vec::new());
        };
        let mut reader = Destinations::new(self, catalog);
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(first, 1)];
        while let Some((id, level)) = stack.pop() {
            if entries.len() >= MAX_ENTRIES {
                break;
            }
            if !seen.insert(id) {
                continue;
            }
            if seen.len().is_multiple_of(1024) {
                self.check_time()?;
            }
            let Some(item) = self.dict_at(id)? else {
                continue;
            };
            if let Some(next) = item.get(b"Next").and_then(Object::as_ref_id) {
                stack.push((next, level));
            }
            if level < MAX_LEVEL
                && let Some(child) = item.get(b"First").and_then(Object::as_ref_id)
            {
                stack.push((child, level + 1));
            }
            let title = self
                .resolved(item.get(b"Title"))?
                .and_then(|title| title.as_string().map(decode_text_string))
                .unwrap_or_default();
            let page_index = reader.of_item(&item)?;
            entries.push(OutlineEntry {
                level,
                title,
                page_index,
            });
        }
        Ok(entries)
    }

    /// The dictionary object `id` is, if it is one.
    pub(crate) fn dict_at(&self, id: ObjId) -> Result<Option<Dict>> {
        Ok(match self.object_without_data(id)? {
            Some(Object::Dict(dict)) => Some(dict),
            _ => None,
        })
    }

    /// `object`, the object it refers to when it is a reference.
    pub(crate) fn resolved(&self, object: Option<&Object>) -> Result<Option<Object>> {
        match object {
            Some(Object::Ref(id)) => self.object_without_data(*id),
            Some(other) => Ok(Some(other.clone())),
            None => Ok(None),
        }
    }

    pub(crate) fn resolved_dict(&self, object: Option<&Object>) -> Result<Option<Dict>> {
        Ok(match self.resolved(object)? {
            Some(Object::Dict(dict)) => Some(dict),
            _ => None,
        })
    }
}

/// Reads where destinations lead.
pub(crate) struct Destinations<'d> {
    doc: &'d Document,
    /// Each page object, and its index.
    pages: HashMap<ObjId, usize>,
    catalog: Dict,
    /// Named destinations already looked up.
    named: HashMap<Vec<u8>, Option<usize>>,
}

impl<'d> Destinations<'d> {
    /// Destinations of `doc`, whose catalog is `catalog`.
    pub(crate) fn new(doc: &'d Document, catalog: Dict) -> Self {
        Self {
            doc,
            pages: doc
                .page_ids()
                .into_iter()
                .enumerate()
                .filter_map(|(index, id)| id.map(|id| (id, index)))
                .collect(),
            catalog,
            named: HashMap::new(),
        }
    }

    /// The page an outline entry leads to.
    fn of_item(&mut self, item: &Dict) -> Result<Option<usize>> {
        if item.contains_key(b"Dest") {
            let destination = self.doc.resolved(item.get(b"Dest"))?;
            return self.page_of(destination, 0);
        }
        let Some(action) = self.doc.resolved_dict(item.get(b"A"))? else {
            return Ok(None);
        };
        match action.get(b"S").and_then(Object::as_name) {
            Some(b"GoTo") => {
                let destination = self.doc.resolved(action.get(b"D"))?;
                self.page_of(destination, 0)
            }
            // The named actions that lead to a page whatever the page shown.
            Some(b"Named") => Ok(match action.get(b"N").and_then(Object::as_name) {
                Some(b"FirstPage") if !self.pages.is_empty() => Some(0),
                Some(b"LastPage") => self.doc.page_count().checked_sub(1),
                _ => None,
            }),
            _ => Ok(None),
        }
    }

    /// The page a destination leads to: an array naming the page, a name or a string (looked up
    /// in the catalog's `/Dests`, then in its `/Names` tree: producers write either for both, and
    /// readers look in both), or a dictionary holding one under `/D`.
    pub(crate) fn page_of(
        &mut self,
        destination: Option<Object>,
        depth: usize,
    ) -> Result<Option<usize>> {
        if depth > MAX_INDIRECTIONS {
            return Ok(None);
        }
        match destination {
            Some(Object::Array(items)) => Ok(items
                .first()
                .and_then(Object::as_ref_id)
                .and_then(|page| self.pages.get(&page).copied())),
            Some(Object::Dict(dict)) => {
                let inner = self.doc.resolved(dict.get(b"D"))?;
                self.page_of(inner, depth + 1)
            }
            Some(Object::Name(name)) => self.named(name.as_bytes().to_vec(), depth),
            Some(Object::String(key)) => self.named(key, depth),
            _ => Ok(None),
        }
    }

    /// The page of the named destination `key`, looked up once.
    fn named(&mut self, key: Vec<u8>, depth: usize) -> Result<Option<usize>> {
        if let Some(known) = self.named.get(&key) {
            return Ok(*known);
        }
        // A name leading to itself reads as missing.
        self.named.insert(key.clone(), None);
        let target = self.lookup(&key)?;
        let page = self.page_of(target, depth + 1)?;
        self.named.insert(key, page);
        Ok(page)
    }

    /// The destination named `key`: in the catalog's `/Dests`, else in its `/Names` tree.
    fn lookup(&self, key: &[u8]) -> Result<Option<Object>> {
        if let Some(dests) = self.doc.resolved_dict(self.catalog.get(b"Dests"))?
            && let Some(target) = dests.get(key)
        {
            return self.doc.resolved(Some(target));
        }
        let Some(names) = self.doc.resolved_dict(self.catalog.get(b"Names"))? else {
            return Ok(None);
        };
        let Some(root) = self.doc.resolved_dict(names.get(b"Dests"))? else {
            return Ok(None);
        };
        let mut budget = MAX_TREE_NODES;
        let mut seen = HashSet::new();
        self.name_tree_get(&root, key, 0, &mut budget, &mut seen)
    }

    /// The value of `key` in the name tree `node`.
    fn name_tree_get(
        &self,
        node: &Dict,
        key: &[u8],
        depth: usize,
        budget: &mut usize,
        seen: &mut HashSet<ObjId>,
    ) -> Result<Option<Object>> {
        if depth > MAX_TREE_DEPTH || *budget == 0 {
            return Ok(None);
        }
        *budget -= 1;
        if let Some(limits) = self.doc.resolved(node.get(b"Limits"))?
            && let Some([low, high]) = limits.as_array().and_then(|l| l.get(..2))
            && let (Some(low), Some(high)) = (low.as_string(), high.as_string())
            && (key < low || key > high)
        {
            return Ok(None);
        }
        if let Some(names) = self.doc.resolved(node.get(b"Names"))?
            && let Some(names) = names.as_array()
        {
            for pair in names.chunks(2) {
                if let [name, value] = pair
                    && name.as_string() == Some(key)
                {
                    return self.doc.resolved(Some(value));
                }
            }
        }
        let Some(kids) = self.doc.resolved(node.get(b"Kids"))? else {
            return Ok(None);
        };
        for kid in kids.as_array().unwrap_or_default() {
            let Some(id) = kid.as_ref_id() else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            if let Some(kid) = self.doc.dict_at(id)?
                && let Some(found) = self.name_tree_get(&kid, key, depth + 1, budget, seen)?
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
}
