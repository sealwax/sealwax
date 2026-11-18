//! Documents built from nothing, as test fixtures are: blank pages of a given size, the outline,
//! the document information, an XMP packet.

use std::collections::BTreeMap;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::geometry::Rect;
use crate::object::{Dict, Name, ObjId, Object, Stream};
use crate::text::encode_text_string;

/// The largest side of a page, in points (ISO 32000-1, annex C).
const MAX_PAGE_SIDE: f64 = 14_400.0;

/// An entry of an outline to write ([`Document::set_outline`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutlineEntry {
    /// Its depth: 1 for the entries at the top, at most one more than the entry before.
    pub level: usize,
    pub title: String,
    /// The page it leads to (from 0), or none.
    pub page_index: Option<usize>,
}

impl Document {
    /// Add a blank page of `width` by `height` points after the others; returns its index.
    ///
    /// A page added is part of the document's pages ([`Document::page_count`],
    /// [`Document::geometry`]), and is drawn on as any other; reading what it shows (text,
    /// rendering) needs the document saved and opened again.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a side that is not a number between 1 and 14,400 points.
    pub fn add_page(&mut self, width: f64, height: f64) -> Result<usize> {
        let valid = |side: f64| side.is_finite() && (1.0..=MAX_PAGE_SIDE).contains(&side);
        if !(valid(width) && valid(height)) {
            return Err(Error::InvalidArgument { what: "page size" });
        }
        let mut page = Dict::new();
        page.insert("Resources", Dict::new());
        self.push_page(page, Rect::from_corners(0.0, 0.0, width, height))?;
        Ok(self.page_count() - 1)
    }

    /// Replace the outline (bookmarks) by `entries`, in the order a reader lists them (each
    /// entry, then its children). Each leads to the top of its page, if it has one; all are
    /// shown open. No entry removes the outline.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a first entry not at level 1, a level more than one
    /// deeper than the entry before, or a page the document does not have.
    pub fn set_outline(&mut self, entries: &[NewOutlineEntry]) -> Result<()> {
        let pages = self.page_ids();
        let mut previous = 0;
        for entry in entries {
            if entry.level == 0 || entry.level > previous + 1 {
                return Err(Error::InvalidArgument {
                    what: "outline level",
                });
            }
            if entry
                .page_index
                .is_some_and(|index| !matches!(pages.get(index), Some(Some(_))))
            {
                return Err(Error::InvalidArgument { what: "page index" });
            }
            previous = entry.level;
        }
        let catalog_id = self.catalog_id();
        let Some(Object::Dict(mut catalog)) = self.object(catalog_id)? else {
            return Err(Error::Malformed);
        };
        if entries.is_empty() {
            catalog.remove(b"Outlines");
            self.set_object(catalog_id, Object::Dict(catalog));
            return Ok(());
        }
        let root = self.add_object(Object::Null)?;
        let mut ids = Vec::with_capacity(entries.len());
        for _ in entries {
            ids.push(self.add_object(Object::Null)?);
        }
        // Each entry's parent: the last entry before it one level up, else the root.
        let mut parents = Vec::with_capacity(entries.len());
        let mut open: Vec<usize> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            open.truncate(entry.level - 1);
            parents.push(open.last().copied());
            open.push(index);
        }
        let children = |parent: Option<usize>| -> Vec<usize> {
            (0..entries.len())
                .filter(|index| parents[*index] == parent)
                .collect()
        };
        // Every entry open: a node counts all its descendants.
        let descendants = |index: usize| -> usize {
            entries[index + 1..]
                .iter()
                .take_while(|entry| entry.level > entries[index].level)
                .count()
        };
        let mut items: BTreeMap<usize, Dict> = BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let mut item = Dict::new();
            item.insert("Title", Object::String(encode_text_string(&entry.title)));
            item.insert("Parent", parents[index].map_or(root, |parent| ids[parent]));
            if let Some(Some(page)) = entry.page_index.and_then(|page| pages.get(page)) {
                item.insert(
                    "Dest",
                    vec![Object::Ref(*page), Object::Name(Name::from("Fit"))],
                );
            }
            let kids = children(Some(index));
            if let (Some(first), Some(last)) = (kids.first(), kids.last()) {
                item.insert("First", ids[*first]);
                item.insert("Last", ids[*last]);
                item.insert("Count", i64::try_from(descendants(index)).unwrap_or(0));
            }
            items.insert(index, item);
        }
        for parent in std::iter::once(None).chain((0..entries.len()).map(Some)) {
            let kids = children(parent);
            for pair in kids.windows(2) {
                if let [a, b] = pair {
                    if let Some(item) = items.get_mut(a) {
                        item.insert("Next", ids[*b]);
                    }
                    if let Some(item) = items.get_mut(b) {
                        item.insert("Prev", ids[*a]);
                    }
                }
            }
        }
        let top = children(None);
        let mut outlines = Dict::new();
        outlines.insert("Type", Name::from("Outlines"));
        if let (Some(first), Some(last)) = (top.first(), top.last()) {
            outlines.insert("First", ids[*first]);
            outlines.insert("Last", ids[*last]);
        }
        outlines.insert("Count", i64::try_from(entries.len()).unwrap_or(0));
        self.set_object(root, Object::Dict(outlines));
        for (index, item) in items {
            self.set_object(ids[index], Object::Dict(item));
        }
        catalog.insert("Outlines", root);
        self.set_object(catalog_id, Object::Dict(catalog));
        Ok(())
    }

    /// Replace the document information dictionary by `entries` (`Title`, `Author`,
    /// `CreationDate` as PDF writes dates, `D:20240131120000Z`, any other), each a text string.
    /// No entry removes it.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for an empty key; what [`Document::add_object`] returns.
    pub fn set_info(&mut self, entries: &BTreeMap<String, String>) -> Result<()> {
        if entries.keys().any(String::is_empty) {
            return Err(Error::InvalidArgument {
                what: "information key",
            });
        }
        if entries.is_empty() {
            self.set_info_id(None);
            return Ok(());
        }
        let mut info = Dict::new();
        for (key, value) in entries {
            info.insert(key.as_str(), Object::String(encode_text_string(value)));
        }
        let id = self.add_object(Object::Dict(info))?;
        self.set_info_id(Some(id));
        Ok(())
    }

    /// Make `packet` the document's XMP metadata (the catalog's `/Metadata`), as given.
    ///
    /// # Errors
    /// What [`Document::add_object`] returns; [`Error::Malformed`] when the catalog cannot be
    /// read.
    pub fn set_xmp(&mut self, packet: &[u8]) -> Result<()> {
        let catalog_id = self.catalog_id();
        let Some(Object::Dict(mut catalog)) = self.object(catalog_id)? else {
            return Err(Error::Malformed);
        };
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("Metadata"));
        dict.insert("Subtype", Name::from("XML"));
        let stream: ObjId = self.add_object(Object::Stream(Stream {
            dict,
            data: packet.to_vec(),
        }))?;
        catalog.insert("Metadata", stream);
        self.set_object(catalog_id, Object::Dict(catalog));
        Ok(())
    }
}
