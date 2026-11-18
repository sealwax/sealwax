//! The logical structure (tagged PDF) and the outline, written once the pages are.
//!
//! The structure tree follows ISO 32000-1, 14.7: an element per [`Element`] reached from the
//! root, its marked content and link annotations as kids, and the parent tree that leads back
//! from each marked sequence (by page and id) and each annotation to its element.

use std::collections::{BTreeMap, BTreeSet};

use sealwax_core::{
    Dict, Document, Limits, Name, NewOutlineEntry, ObjId, Object, encode_text_string,
};

use crate::model::{Element, HeaderScope, Kid, ListNumbering, OutlineEntry, Structure, TableCell};
use crate::{Error, Skipped};

/// The deadline is checked once per this many elements.
const CHECK_EVERY: usize = 1024;

/// What the pages hold that the structure ties to its elements.
pub(crate) struct Pages<'a> {
    pub(crate) ids: &'a [ObjId],
    /// For each page, the marked-content ids the composition gives, with the ids written.
    pub(crate) mcids: &'a [BTreeMap<u32, u32>],
    /// For each page, the annotation written for each of its links, if one was.
    pub(crate) links: &'a [Vec<Option<ObjId>>],
}

/// The elements reached from the root, each once, with where each is written.
struct Tree<'a> {
    elements: &'a [Element],
    /// `None`: not reached; `Some(None)`: the root; `Some(Some(p))`: a kid of element `p`.
    parents: Vec<Option<Option<usize>>>,
    ids: Vec<Option<ObjId>>,
}

impl<'a> Tree<'a> {
    /// The tree of `elements` from the first: an element listed again, or by an index out of
    /// range, is not a kid.
    fn new(elements: &'a [Element]) -> Self {
        let mut parents: Vec<Option<Option<usize>>> = vec![None; elements.len()];
        if let Some(root) = parents.first_mut() {
            *root = Some(None);
        }
        let mut stack = vec![0];
        while let Some(index) = stack.pop() {
            let Some(element) = elements.get(index) else {
                continue;
            };
            for kid in &element.kids {
                if let Kid::Element(child) = *kid
                    && let Some(parent @ None) = parents.get_mut(child)
                {
                    *parent = Some(Some(index));
                    stack.push(child);
                }
            }
        }
        Self {
            elements,
            parents,
            ids: Vec::new(),
        }
    }

    fn id(&self, index: usize) -> Option<ObjId> {
        self.ids.get(index).copied().flatten()
    }

    /// Whether element `child` is the kid of element `index` in the tree.
    fn is_kid(&self, child: usize, index: usize) -> bool {
        self.parents.get(child) == Some(&Some(Some(index)))
    }

    /// The `StructElem` of element `index`, under `parent`.
    fn dict(
        &self,
        index: usize,
        parent: ObjId,
        owners: &mut Owners<'_>,
        skipped: &mut Skipped,
    ) -> Option<Dict> {
        let element = self.elements.get(index)?;
        let id = self.id(index)?;
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("StructElem"));
        let kind = if element.kind.is_empty() {
            "NonStruct"
        } else {
            &element.kind
        };
        dict.insert("S", Name::from(kind));
        dict.insert("P", Object::Ref(parent));
        let (kids, page) = self.kids(index, id, owners, skipped);
        if let Some(page) = page {
            dict.insert("Pg", Object::Ref(page));
        }
        dict.insert("K", Object::Array(kids));
        add_properties(&mut dict, element);
        Some(dict)
    }

    /// The kids of element `index` (written at `id`): its elements in the tree, and the marked
    /// content and annotations it claims first. Content on the page of the first it claims, its
    /// page, is a bare id.
    fn kids(
        &self,
        index: usize,
        id: ObjId,
        owners: &mut Owners<'_>,
        skipped: &mut Skipped,
    ) -> (Vec<Object>, Option<ObjId>) {
        let mut kids = Vec::new();
        let mut listed = BTreeSet::new();
        let mut own_page: Option<ObjId> = None;
        let Some(element) = self.elements.get(index) else {
            return (kids, own_page);
        };
        for kid in &element.kids {
            match *kid {
                Kid::Element(child) => match self.id(child) {
                    Some(child_id) if self.is_kid(child, index) && listed.insert(child) => {
                        kids.push(Object::Ref(child_id));
                    }
                    _ => skipped.count("structure elements listed twice or out of range", 1),
                },
                Kid::Content { page, mcid } => match owners.content(page, mcid, id) {
                    Some((written, page)) => {
                        let own = *own_page.get_or_insert(page);
                        kids.push(if own == page {
                            Object::Integer(i64::from(written))
                        } else {
                            let mut reference = Dict::new();
                            reference.insert("Type", Name::from("MCR"));
                            reference.insert("Pg", Object::Ref(page));
                            reference.insert("MCID", Object::Integer(i64::from(written)));
                            Object::Dict(reference)
                        });
                    }
                    None => skipped.count("structure content not drawn", 1),
                },
                Kid::Link { page, link } => match owners.link(page, link, id) {
                    Some((annotation, page)) => {
                        let mut reference = Dict::new();
                        reference.insert("Type", Name::from("OBJR"));
                        reference.insert("Obj", Object::Ref(annotation));
                        reference.insert("Pg", Object::Ref(page));
                        kids.push(Object::Dict(reference));
                    }
                    None => skipped.count("structure links not written", 1),
                },
            }
        }
        (kids, own_page)
    }
}

/// Writes `structure`: its elements, the structure tree root and its parent tree, the pages'
/// `/StructParents` and `/Tabs`, the annotations' `/StructParent`, and `/MarkInfo`. What does
/// not tie to the pages is left out and counted.
pub(crate) fn write(
    doc: &mut Document,
    structure: &Structure,
    pages: &Pages<'_>,
    skipped: &mut Skipped,
    limits: &Limits,
) -> Result<(), Error> {
    let mut tree = Tree::new(&structure.elements);
    let outside = tree
        .parents
        .iter()
        .filter(|parent| parent.is_none())
        .count();
    if outside > 0 {
        skipped.count("structure elements outside the tree", outside);
    }
    if tree.parents.is_empty() {
        return Ok(());
    }
    let root = doc.add_object(Object::Null)?;
    for parent in &tree.parents {
        let id = match parent {
            Some(_) => Some(doc.add_object(Object::Null)?),
            None => None,
        };
        tree.ids.push(id);
    }
    let Some(top) = tree.id(0) else {
        return Ok(());
    };
    let mut owners = Owners::new(pages);
    for (index, parent) in tree.parents.iter().enumerate() {
        if index % CHECK_EVERY == 0 {
            limits.check_time()?;
        }
        let Some(parent) = parent else {
            continue;
        };
        let parent = parent.and_then(|parent| tree.id(parent)).unwrap_or(root);
        if let (Some(dict), Some(id)) = (
            tree.dict(index, parent, &mut owners, skipped),
            tree.id(index),
        ) {
            doc.set_object(id, Object::Dict(dict));
        }
    }
    owners.write(doc, root, top, skipped)
}

/// An element's alternate and actual text, language, identifier and attributes.
fn add_properties(dict: &mut Dict, element: &Element) {
    for (key, text) in [
        ("Alt", &element.alt),
        ("ActualText", &element.actual_text),
        ("Lang", &element.lang),
    ] {
        if let Some(text) = text {
            dict.insert(key, Object::String(encode_text_string(text)));
        }
    }
    if let Some(id) = &element.id {
        dict.insert("ID", Object::String(id.as_bytes().to_vec()));
    }
    let attributes: Vec<Object> = [
        element.table_cell.map(table_attributes),
        element.list_numbering.map(list_attributes),
    ]
    .into_iter()
    .flatten()
    .map(Object::Dict)
    .collect();
    if !attributes.is_empty() {
        dict.insert("A", Object::Array(attributes));
    }
}

/// What each written marked sequence and link annotation belongs to, as elements claim them.
struct Owners<'a> {
    pages: &'a Pages<'a>,
    /// For each page, the element of each id written.
    content: Vec<BTreeMap<u32, ObjId>>,
    /// Annotations claimed, with their elements, in the order claimed.
    links: Vec<(ObjId, ObjId)>,
    claimed_links: BTreeSet<ObjId>,
}

impl<'a> Owners<'a> {
    fn new(pages: &'a Pages<'a>) -> Self {
        Self {
            pages,
            content: vec![BTreeMap::new(); pages.ids.len()],
            links: Vec::new(),
            claimed_links: BTreeSet::new(),
        }
    }

    /// The id written for `mcid` on `page`, now `element`'s, and the page's object, if it was
    /// drawn and is not another's.
    fn content(&mut self, page: usize, mcid: u32, element: ObjId) -> Option<(u32, ObjId)> {
        let page_id = *self.pages.ids.get(page)?;
        let written = *self.pages.mcids.get(page)?.get(&mcid)?;
        let owners = self.content.get_mut(page)?;
        if owners.contains_key(&written) {
            return None;
        }
        owners.insert(written, element);
        Some((written, page_id))
    }

    /// The annotation of link `link` on `page`, now `element`'s, and the page's object, if it
    /// was written and is not another's.
    fn link(&mut self, page: usize, link: usize, element: ObjId) -> Option<(ObjId, ObjId)> {
        let page_id = *self.pages.ids.get(page)?;
        let annotation = (*self.pages.links.get(page)?.get(link)?)?;
        if !self.claimed_links.insert(annotation) {
            return None;
        }
        self.links.push((annotation, element));
        Some((annotation, page_id))
    }

    /// The structure tree root at `root`, its element `top`; the parent tree; the keys pages
    /// and annotations find their elements by; `/MarkInfo`.
    fn write(
        self,
        doc: &mut Document,
        root: ObjId,
        top: ObjId,
        skipped: &mut Skipped,
    ) -> Result<(), Error> {
        let page_count = self.pages.ids.len();
        let mut nums = Vec::new();
        for (index, (owners, page)) in self.content.iter().zip(self.pages.ids).enumerate() {
            let drawn = self.pages.mcids.get(index).map_or(0, BTreeMap::len);
            if owners.len() < drawn {
                skipped.count("marked content outside the structure", drawn - owners.len());
            }
            let mut dict = page_dict(doc, *page)?;
            // Tab order follows the structure on every page (PDF/UA, 7.18.3).
            dict.insert("Tabs", Name::from("S"));
            if !owners.is_empty() {
                // Ids are written from 0 on each page: one entry each, null for an id no
                // element claims.
                let array: Vec<Object> = (0..drawn)
                    .map(|written| {
                        u32::try_from(written)
                            .ok()
                            .and_then(|written| owners.get(&written))
                            .map_or(Object::Null, |owner| Object::Ref(*owner))
                    })
                    .collect();
                let key = key(index);
                nums.push(Object::Integer(key));
                nums.push(Object::Ref(doc.add_object(Object::Array(array))?));
                dict.insert("StructParents", Object::Integer(key));
            }
            doc.set_object(*page, Object::Dict(dict));
        }
        for (k, (annotation, element)) in self.links.iter().enumerate() {
            let key = key(page_count + k);
            nums.push(Object::Integer(key));
            nums.push(Object::Ref(*element));
            let mut dict = page_dict(doc, *annotation)?;
            dict.insert("StructParent", Object::Integer(key));
            doc.set_object(*annotation, Object::Dict(dict));
        }
        let mut parent_tree = Dict::new();
        parent_tree.insert("Nums", Object::Array(nums));
        let mut tree = Dict::new();
        tree.insert("Type", Name::from("StructTreeRoot"));
        tree.insert("K", Object::Ref(top));
        tree.insert("ParentTree", parent_tree);
        tree.insert(
            "ParentTreeNextKey",
            Object::Integer(key(page_count + self.links.len())),
        );
        doc.set_object(root, Object::Dict(tree));
        let catalog = doc.catalog_id();
        let mut dict = page_dict(doc, catalog)?;
        dict.insert("StructTreeRoot", Object::Ref(root));
        let mut mark_info = Dict::new();
        mark_info.insert("Marked", Object::Bool(true));
        dict.insert("MarkInfo", mark_info);
        doc.set_object(catalog, Object::Dict(dict));
        Ok(())
    }
}

/// A parent tree key: a count of pages and annotations, far below `i64::MAX`.
fn key(index: usize) -> i64 {
    i64::try_from(index).unwrap_or(i64::MAX)
}

/// The dictionary at `id`, empty if it is none.
fn page_dict(doc: &Document, id: ObjId) -> Result<Dict, Error> {
    Ok(match doc.object(id)? {
        Some(Object::Dict(dict)) => dict,
        _ => Dict::new(),
    })
}

/// The `/Table` attributes of a cell: its header scope and column span.
fn table_attributes(cell: TableCell) -> Dict {
    let mut dict = Dict::new();
    dict.insert("O", Name::from("Table"));
    if let Some(scope) = cell.header_scope {
        let scope = match scope {
            HeaderScope::Row => "Row",
            HeaderScope::Column => "Column",
            HeaderScope::Both => "Both",
        };
        dict.insert("Scope", Name::from(scope));
    }
    if cell.col_span > 1 {
        dict.insert("ColSpan", Object::Integer(i64::from(cell.col_span)));
    }
    dict
}

/// The `/List` attributes of a list: how its items are numbered.
fn list_attributes(numbering: ListNumbering) -> Dict {
    let name = match numbering {
        ListNumbering::Unordered => "Unordered",
        ListNumbering::Ordered => "Ordered",
        ListNumbering::Description => "Description",
        ListNumbering::Disc => "Disc",
        ListNumbering::Circle => "Circle",
        ListNumbering::Square => "Square",
        ListNumbering::Decimal => "Decimal",
        ListNumbering::LowerRoman => "LowerRoman",
        ListNumbering::UpperRoman => "UpperRoman",
        ListNumbering::LowerAlpha => "LowerAlpha",
        ListNumbering::UpperAlpha => "UpperAlpha",
    };
    let mut dict = Dict::new();
    dict.insert("O", Name::from("List"));
    dict.insert("ListNumbering", Name::from(name));
    dict
}

/// The outline: each entry nested under the nearest entry before it of a smaller level, as
/// deep as its ancestors make it. An entry for a page out of range leads nowhere and is
/// counted. The document opens with the outline shown.
pub(crate) fn write_outline(
    doc: &mut Document,
    entries: &[OutlineEntry],
    page_count: usize,
    skipped: &mut Skipped,
) -> Result<(), Error> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut open: Vec<u32> = Vec::new();
    let mut outline = Vec::with_capacity(entries.len());
    for entry in entries {
        while open.last().is_some_and(|level| *level >= entry.level) {
            open.pop();
        }
        open.push(entry.level);
        let page_index = (entry.page < page_count).then_some(entry.page);
        if page_index.is_none() {
            skipped.count("outline entries to pages out of range", 1);
        }
        outline.push(NewOutlineEntry {
            level: open.len(),
            title: entry.title.clone(),
            page_index,
            top: entry.top,
        });
    }
    doc.set_outline(&outline)?;
    let catalog = doc.catalog_id();
    let mut dict = page_dict(doc, catalog)?;
    dict.insert("PageMode", Name::from("UseOutlines"));
    doc.set_object(catalog, Object::Dict(dict));
    Ok(())
}
