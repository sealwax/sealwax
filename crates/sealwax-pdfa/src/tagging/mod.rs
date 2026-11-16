//! Level a, N1: every piece of a page's content tied to the structure or marked as an artifact.
//!
//! For a document without a structure tree (or with an empty one), the
//! content of each page is cut into units ([`content`]), what each unit draws is found by
//! interpreting the page ([`sealwax_text::drawn`]), the units are gathered into paragraphs and
//! figures in reading order ([`layout`]), and the content is rewritten with marked-content
//! sequences that tie them to a new tree: a `Document` element holding, page after page, a `P`
//! per paragraph and a `Figure` per image, its alternate text the `/Alt` of a sequence around
//! it when the content gives one (a signature stamp described by its caller), `Image` otherwise.
//! Paths and shadings are artifacts, and so are running heads and page numbers (text in the
//! margins that comes back on several pages, or that is a page number), and images behind text.
//!
//! A document that has a tree keeps it (level N0 repairs its role map), and what its pages draw
//! outside it is tied to it the same way: content drawn after it was tagged (flattened
//! annotations, stamps), and content whose identifiers its parent tree does not lead anywhere
//! (a producer's mistake), those identifiers blanked out. The new elements go at the end of its
//! top element, the parent tree is extended.
//!
//! Nothing else in the content changes: a page renders as before.

mod content;
mod layout;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use hayro_syntax::object::{Array, Stream as HayroStream};
use sealwax_core::{Dict, Document, Name, ObjId, Object, Result, Stream};
use sealwax_text::Bounds;

use crate::Report;
use crate::edit::{self, Location};
use content::{Artifact, Content, Kind, Mark, Run};
use layout::{Item, Role, Seen};

/// The alternate text of a figure the content does not describe.
const GENERIC_ALT: &[u8] = b"Image";

/// The tag of the sequences that mark units for the analysis (never written out).
const ANALYSIS_TAG: &str = "SwUnit";

/// A structure element being built.
struct Element {
    kind: &'static str,
    page: ObjId,
    /// Its marked-content identifiers on that page.
    mcids: Vec<i64>,
    /// Alternate text, for a figure.
    alt: Option<Vec<u8>>,
}

/// A page to tie: its content, and what layout found in it (`None` when the page could not be
/// interpreted: its units are then tied in content order).
struct Page {
    id: ObjId,
    data: Vec<u8>,
    crop: Bounds,
    items: Option<Vec<Item>>,
}

/// Tie the content of a document to its structure: all of it to a new tree when it has none,
/// what is not tied yet to the tree it has otherwise.
pub(crate) fn apply(doc: &mut Document, report: &mut Report) -> Result<()> {
    let catalog = Location::of(doc.catalog_id());
    if has_structure(doc, &catalog)? {
        return complete(doc, &catalog, report);
    }
    let pages = prepare(doc, true, report)?;
    let heads = running_heads(&pages);
    report.change("running_heads_as_artifacts", heads.len());
    let mut elements: Vec<Element> = Vec::new();
    let mut parent_tree: Vec<(i64, Vec<usize>)> = Vec::new();
    for (number, page) in pages.iter().enumerate() {
        let content = Content::read(&page.data);
        if content.units.is_empty() {
            continue;
        }
        let marks = page_marks(&content, page, number, &heads, &mut elements);
        let owners = tie(doc, page, &content, &marks, &mut elements, 0)?;
        let key = i64::try_from(parent_tree.len()).unwrap_or(i64::MAX);
        edit::edit_dict(doc, &Location::of(page.id), false, |dict| {
            if owners.is_empty() {
                dict.remove(b"StructParents");
            } else {
                dict.insert("StructParents", key);
            }
        })?;
        report.change("page_content_tied", 1);
        if !owners.is_empty() {
            parent_tree.push((key, owners));
        }
    }
    forget_annotation_parents(doc)?;
    forget_form_identifiers(doc, report)?;
    write_tree(doc, &catalog, &elements, &parent_tree)?;
    report.change("structure_tree_added", 1);
    report.change("structure_elements_added", elements.len());
    Ok(())
}

/// The pages to tie, analysed, the identifiers that lead to no element blanked out. For a new
/// tree (`fresh`), every page; for a tree the document has, the pages with content not tied to
/// it.
fn prepare(doc: &Document, fresh: bool, report: &mut Report) -> Result<Vec<Page>> {
    let geometry = doc.geometry();
    let parent_tree = Location::of(doc.catalog_id())
        .key(b"StructTreeRoot")
        .key(b"ParentTree");
    let reachable = if fresh {
        HashSet::new()
    } else {
        reachable_elements(doc)?
    };
    let mut pages = Vec::new();
    for (index, id, data) in contents(doc, report) {
        doc.check_time()?;
        // The identifiers of the page that lead to an element of the tree.
        let mut leading: HashSet<usize> = HashSet::new();
        if !fresh
            && let Some(key) = edit::read_dict(doc, &Location::of(id))?
                .and_then(|page| page.get(b"StructParents").and_then(Object::as_i64))
            && let Some(entry) = parent_entry(doc, &parent_tree, key, 0)?
        {
            for (mcid, item) in entry.items.iter().enumerate() {
                if item.as_ref_id().is_some_and(|id| reachable.contains(&id)) {
                    leading.insert(mcid);
                }
            }
        }
        let cleaned = content::without_mcids(&data, |mcid| leading.contains(&mcid));
        if cleaned.is_some() {
            report.change("marked_content_identifier_removed", 1);
        }
        let data = cleaned.unwrap_or(data);
        if !fresh && Content::read(&data).units.iter().all(|unit| unit.tied) {
            continue;
        }
        let crop = geometry.get(index).map_or([0.0; 4], |page| {
            let crop = page.crop_box;
            [crop.x1, crop.y1, crop.x2, crop.y2]
        });
        let items = analyse(doc, index, &data, crop)?;
        if items.is_none() {
            report.residual_add("page_layout_unknown", 1);
        }
        pages.push(Page {
            id,
            data,
            crop,
            items,
        });
    }
    Ok(pages)
}

fn page_marks(
    content: &Content<'_>,
    page: &Page,
    number: usize,
    heads: &HashSet<(usize, usize)>,
    elements: &mut Vec<Element>,
) -> Vec<Option<Mark>> {
    match &page.items {
        Some(items) => marks(content, items, page.id, number, heads, elements),
        None => marks_in_content_order(content, page.id, elements),
    }
}

/// Rewrite the content of `page` with the sequences `marks` call for, its identifiers numbered
/// from `first_mcid`, and return the element each identifier from there on belongs to.
fn tie(
    doc: &mut Document,
    page: &Page,
    content: &Content<'_>,
    marks: &[Option<Mark>],
    elements: &mut [Element],
    first_mcid: usize,
) -> Result<Vec<usize>> {
    let runs = content.runs(marks);
    let mut owners: Vec<usize> = Vec::new();
    let rewritten = content.write(&runs, |run: &Run| match run.mark {
        Mark::Element(index) => {
            let mcid = first_mcid + owners.len();
            owners.push(index);
            let element = &mut elements[index];
            element.mcids.push(i64::try_from(mcid).unwrap_or(i64::MAX));
            (
                format!("/{} <</MCID {mcid}>> BDC", element.kind).into_bytes(),
                b"EMC".to_vec(),
            )
        }
        Mark::Artifact(Artifact::Layout) => (b"/Artifact BMC".to_vec(), b"EMC".to_vec()),
        Mark::Artifact(Artifact::Pagination) => (
            b"/Artifact <</Type /Pagination>> BDC".to_vec(),
            b"EMC".to_vec(),
        ),
        Mark::Artifact(Artifact::Background) => {
            (b"/Artifact <</Type /Layout>> BDC".to_vec(), b"EMC".to_vec())
        }
    });
    let stream = doc.add_object(flate(&rewritten))?;
    edit::edit_dict(doc, &Location::of(page.id), false, |dict| {
        dict.insert("Contents", stream);
    })?;
    Ok(owners)
}

/// Tie what is not tied yet in a document that has a tree (content drawn after it was tagged:
/// flattened annotations, stamps): new elements at the end of its top element, in page and
/// reading order, the parent tree extended.
fn complete(doc: &mut Document, catalog: &Location, report: &mut Report) -> Result<()> {
    let pages = prepare(doc, false, report)?;
    if pages.is_empty() {
        return Ok(());
    }
    let tree = catalog.key(b"StructTreeRoot");
    let Some(parent) = container(doc, catalog)? else {
        report.residual_add("page_content_untied", pages.len());
        return Ok(());
    };
    let heads = running_heads(&pages);
    report.change("running_heads_as_artifacts", heads.len());
    let mut elements: Vec<Element> = Vec::new();
    let mut tied: Vec<Tied> = Vec::new();
    for (number, page) in pages.iter().enumerate() {
        let content = Content::read(&page.data);
        let marks = page_marks(&content, page, number, &heads, &mut elements);
        let key = edit::read_dict(doc, &Location::of(page.id))?
            .and_then(|dict| dict.get(b"StructParents").and_then(Object::as_i64));
        let entry = match key {
            Some(key) => parent_entry(doc, &tree.key(b"ParentTree"), key, 0)?,
            None => None,
        };
        let first = content
            .next_mcid
            .max(entry.as_ref().map_or(0, |entry| entry.items.len()));
        let owners = tie(doc, page, &content, &marks, &mut elements, first)?;
        report.change("page_content_tied", 1);
        if !owners.is_empty() {
            tied.push(Tied {
                page: page.id,
                entry,
                first,
                owners,
            });
        }
    }
    let ids = add_elements(doc, &elements, parent)?;
    append_kids(doc, parent, &ids)?;
    let mut next_key = next_parent_key(doc, &tree)?;
    for Tied {
        page,
        entry,
        first,
        owners,
    } in tied
    {
        let (mut items, at) = match entry {
            Some(entry) => (entry.items, Some(entry.at)),
            None => (Vec::new(), None),
        };
        items.resize(first, Object::Null);
        items.extend(owners.iter().map(|owner| Object::Ref(ids[*owner])));
        match at {
            Some(EntryAt::Indirect(id)) => doc.set_object(id, Object::Array(items)),
            Some(EntryAt::Inline { node, index }) => {
                edit::edit_dict(doc, &node, false, |node| {
                    if let Some(Object::Array(nums)) = node.get_mut(b"Nums")
                        && let Some(slot) = nums.get_mut(index)
                    {
                        *slot = Object::Array(items);
                    }
                })?;
            }
            None => {
                let key = next_key;
                next_key += 1;
                let array = doc.add_object(Object::Array(items))?;
                if !add_parent_entry(doc, &tree, key, array)? {
                    report.residual_add("parent_tree_not_extended", 1);
                    continue;
                }
                edit::edit_dict(doc, &Location::of(page), false, |dict| {
                    dict.insert("StructParents", key);
                })?;
            }
        }
    }
    edit::edit_dict(doc, &tree, false, |root| {
        root.insert("ParentTreeNextKey", next_key);
    })?;
    report.change("structure_elements_added", elements.len());
    Ok(())
}

/// How many objects of a structure tree are walked.
const MAX_NODES: usize = 1 << 20;

/// The structure elements reachable from the root of the tree, through `/K`: an element the
/// parent tree leads to but the tree does not hold (no `/P`, in no `/K`) ties nothing, for a
/// validator.
fn reachable_elements(doc: &Document) -> Result<HashSet<ObjId>> {
    let mut found = HashSet::new();
    let Some(catalog) = edit::read_dict(doc, &Location::of(doc.catalog_id()))? else {
        return Ok(found);
    };
    let root = match catalog.get(b"StructTreeRoot") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    let Some(kids) = root
        .as_ref()
        .and_then(Object::as_dict)
        .and_then(|r| r.get(b"K"))
    else {
        return Ok(found);
    };
    let mut stack = vec![kids.clone()];
    let mut nodes = 0usize;
    while let Some(value) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES {
            break;
        }
        match value {
            Object::Ref(id) => {
                if !found.contains(&id)
                    && let Some(object) = doc.object(id)?
                {
                    if let Object::Dict(dict) = &object
                        && dict.contains_key(b"S")
                    {
                        found.insert(id);
                    }
                    stack.push(object);
                }
            }
            Object::Array(items) => stack.extend(items),
            Object::Dict(dict) => {
                if dict.contains_key(b"S")
                    && let Some(kids) = dict.get(b"K")
                {
                    stack.push(kids.clone());
                }
            }
            _ => {}
        }
    }
    Ok(found)
}

/// A page whose untied content was tied: its entry of the parent tree if it has one, where new
/// identifiers start, and the element of each.
struct Tied {
    page: ObjId,
    entry: Option<Entry>,
    first: usize,
    owners: Vec<usize>,
}

/// Where a page's entry of the parent tree lives: an array of its own, or an array in a node's
/// `/Nums`, at `index`.
enum EntryAt {
    Indirect(ObjId),
    Inline { node: Location, index: usize },
}

struct Entry {
    at: EntryAt,
    items: Vec<Object>,
}

/// How deep a number tree is walked.
const MAX_TREE_DEPTH: usize = 32;

/// The entry of `key` in the number tree whose node is at `node`.
fn parent_entry(doc: &Document, node: &Location, key: i64, depth: usize) -> Result<Option<Entry>> {
    if depth > MAX_TREE_DEPTH {
        return Ok(None);
    }
    let Some(dict) = edit::read_dict(doc, node)? else {
        return Ok(None);
    };
    if let Some(Object::Array(nums)) = dict.get(b"Nums") {
        for (index, pair) in nums.chunks(2).enumerate() {
            let [found, value] = pair else {
                continue;
            };
            if found.as_i64() != Some(key) {
                continue;
            }
            return Ok(match value {
                Object::Ref(id) => match doc.object(*id)? {
                    Some(Object::Array(items)) => Some(Entry {
                        at: EntryAt::Indirect(*id),
                        items,
                    }),
                    _ => None,
                },
                Object::Array(items) => Some(Entry {
                    at: EntryAt::Inline {
                        node: node.clone(),
                        index: index * 2 + 1,
                    },
                    items: items.clone(),
                }),
                _ => None,
            });
        }
    }
    let kids = match dict.get(b"Kids") {
        Some(kids) => edit::resolved(doc, kids)?,
        None => None,
    };
    let Some(Object::Array(kids)) = kids else {
        return Ok(None);
    };
    for (index, kid) in kids.iter().enumerate() {
        let Some(Object::Dict(kid_dict)) = edit::resolved(doc, kid)? else {
            continue;
        };
        if let Some(Object::Array(limits)) = kid_dict.get(b"Limits")
            && let [low, high] = limits.as_slice()
            && let (Some(low), Some(high)) = (low.as_i64(), high.as_i64())
            && !(low..=high).contains(&key)
        {
            continue;
        }
        let location = match kid {
            Object::Ref(id) => Location::of(*id),
            _ => node.key(b"Kids").index(index),
        };
        if let Some(found) = parent_entry(doc, &location, key, depth + 1)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// The largest key of the number tree at `node`.
fn largest_key(doc: &Document, node: &Object, depth: usize) -> Result<Option<i64>> {
    if depth > MAX_TREE_DEPTH {
        return Ok(None);
    }
    let Some(Object::Dict(dict)) = edit::resolved(doc, node)? else {
        return Ok(None);
    };
    let mut largest: Option<i64> = None;
    if let Some(nums) = dict.get(b"Nums")
        && let Some(Object::Array(nums)) = edit::resolved(doc, nums)?
    {
        largest = nums.iter().step_by(2).filter_map(Object::as_i64).max();
    }
    if let Some(kids) = dict.get(b"Kids")
        && let Some(Object::Array(kids)) = edit::resolved(doc, kids)?
    {
        for kid in &kids {
            if let Some(found) = largest_key(doc, kid, depth + 1)? {
                largest = Some(largest.map_or(found, |l| l.max(found)));
            }
        }
    }
    Ok(largest)
}

/// The first key a new entry of the parent tree may take.
fn next_parent_key(doc: &Document, tree: &Location) -> Result<i64> {
    let root = edit::read_dict(doc, tree)?.unwrap_or_default();
    let declared = root
        .get(b"ParentTreeNextKey")
        .and_then(Object::as_i64)
        .unwrap_or(0);
    let largest = match root.get(b"ParentTree") {
        Some(node) => largest_key(doc, node, 0)?,
        None => None,
    };
    Ok(declared
        .max(largest.map_or(0, |l| l.saturating_add(1)))
        .max(0))
}

/// Add `key` (larger than any) to the parent tree, leading to `array`. False when the tree is
/// built in a way this does not extend (an indirect `/Nums` or `/Kids` array).
fn add_parent_entry(doc: &mut Document, tree: &Location, key: i64, array: ObjId) -> Result<bool> {
    let root = tree.key(b"ParentTree");
    let Some(node) = edit::read_dict(doc, &root)? else {
        let mut numbers = Dict::new();
        numbers.insert("Nums", vec![Object::from(key), Object::Ref(array)]);
        let numbers = doc.add_object(Object::Dict(numbers))?;
        return edit::edit_dict(doc, tree, false, |root| {
            root.insert("ParentTree", numbers);
        });
    };
    if matches!(node.get(b"Nums"), Some(Object::Array(_))) {
        let mut added = false;
        edit::edit_dict(doc, &root, false, |node| {
            if let Some(Object::Array(nums)) = node.get_mut(b"Nums") {
                nums.push(Object::from(key));
                nums.push(Object::Ref(array));
                added = true;
            }
        })?;
        return Ok(added);
    }
    if matches!(node.get(b"Kids"), Some(Object::Array(_))) {
        let mut leaf = Dict::new();
        leaf.insert("Limits", vec![Object::from(key), Object::from(key)]);
        leaf.insert("Nums", vec![Object::from(key), Object::Ref(array)]);
        let leaf = doc.add_object(Object::Dict(leaf))?;
        let mut added = false;
        edit::edit_dict(doc, &root, false, |node| {
            if let Some(Object::Array(kids)) = node.get_mut(b"Kids") {
                kids.push(Object::Ref(leaf));
                added = true;
            }
        })?;
        return Ok(added);
    }
    Ok(false)
}

/// The element new elements go into: the tree's only top element (a `Document`, usually), or
/// the tree's root. `None` when the root is not an object of its own, which elements could
/// point to.
fn container(doc: &Document, catalog: &Location) -> Result<Option<ObjId>> {
    let Some(catalog) = edit::read_dict(doc, catalog)? else {
        return Ok(None);
    };
    let Some(Object::Ref(root_id)) = catalog.get(b"StructTreeRoot") else {
        return Ok(None);
    };
    let Some(Object::Dict(root)) = doc.object(*root_id)? else {
        return Ok(None);
    };
    let kids = match root.get(b"K") {
        Some(kids) => edit::resolved(doc, kids)?,
        None => None,
    };
    let only = match (root.get(b"K"), kids) {
        (_, Some(Object::Array(items))) if items.len() == 1 => items[0].as_ref_id(),
        (Some(Object::Ref(id)), Some(Object::Dict(_))) => Some(*id),
        _ => None,
    };
    if let Some(id) = only
        && let Some(Object::Dict(element)) = doc.object(id)?
        && element.contains_key(b"S")
    {
        return Ok(Some(id));
    }
    Ok(Some(*root_id))
}

/// Append `kids` to the `/K` of `parent`.
fn append_kids(doc: &mut Document, parent: ObjId, kids: &[ObjId]) -> Result<()> {
    let added: Vec<Object> = kids.iter().map(|id| Object::Ref(*id)).collect();
    let current = match doc.object(parent)? {
        Some(Object::Dict(dict)) => dict.get(b"K").cloned(),
        _ => return Ok(()),
    };
    if let Some(Object::Ref(id)) = current
        && let Some(Object::Array(mut items)) = doc.object(id)?
    {
        items.extend(added);
        doc.set_object(id, Object::Array(items));
        return Ok(());
    }
    edit::edit_dict(doc, &Location::of(parent), false, |dict| {
        let items = match dict.remove(b"K") {
            None => added,
            Some(Object::Array(mut items)) => {
                items.extend(added);
                items
            }
            Some(other) => std::iter::once(other).chain(added).collect(),
        };
        dict.insert("K", items);
    })?;
    Ok(())
}

/// The structure elements, each under `parent`.
fn add_elements(doc: &mut Document, elements: &[Element], parent: ObjId) -> Result<Vec<ObjId>> {
    let mut ids = Vec::with_capacity(elements.len());
    for element in elements {
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("StructElem"));
        dict.insert("S", Name::from(element.kind));
        dict.insert("P", parent);
        dict.insert("Pg", element.page);
        let kids: Object = match element.mcids.as_slice() {
            [single] => (*single).into(),
            many => many
                .iter()
                .map(|mcid| Object::from(*mcid))
                .collect::<Vec<_>>()
                .into(),
        };
        dict.insert("K", kids);
        if let Some(alt) = &element.alt {
            dict.insert("Alt", Object::String(alt.clone()));
        }
        ids.push(doc.add_object(Object::Dict(dict))?);
    }
    Ok(ids)
}

/// The paragraphs and figures of a page, in reading order: `None` when hayro cannot interpret
/// it.
///
/// # Errors
///
/// When interpreting the page would pass the document's limits.
fn analyse(doc: &Document, index: usize, data: &[u8], crop: Bounds) -> Result<Option<Vec<Item>>> {
    let content = Content::read(data);
    if content.units.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let instrumented = content.instrumented(ANALYSIS_TAG);
    let Some(drawn) = sealwax_text::drawn(
        doc,
        index,
        &instrumented,
        ANALYSIS_TAG.as_bytes(),
        content.units.len(),
    )?
    else {
        return Ok(None);
    };
    let seen: Vec<Option<Seen>> = content
        .units
        .iter()
        .zip(&drawn)
        .map(|(unit, drawn)| {
            (!unit.tied).then(|| match unit.kind {
                Kind::Text if drawn.glyphs.is_empty() => Seen::Unknown,
                kind => layout::seen(drawn, kind == Kind::InlineImage),
            })
        })
        .collect();
    Ok(Some(layout::layout(&seen, crop)))
}

/// How much of the page's height, at the top and at the bottom, running heads live in.
const MARGIN: f64 = 0.1;

/// Paragraphs (page, item) that are running heads or page numbers: in the top or bottom margin,
/// and either a page number, or the same text (numbers aside) at the same height on another
/// page.
fn running_heads(pages: &[Page]) -> HashSet<(usize, usize)> {
    let mut candidates: Vec<((usize, usize), String, bool)> = Vec::new();
    let mut seen_on: HashMap<String, HashSet<usize>> = HashMap::new();
    for (number, page) in pages.iter().enumerate() {
        let Some(items) = &page.items else {
            continue;
        };
        let height = page.crop[3] - page.crop[1];
        if height <= 0.0 {
            continue;
        }
        for (index, item) in items.iter().enumerate() {
            if item.role != Role::Paragraph {
                continue;
            }
            let top = item.bounds[1] >= page.crop[3] - height * MARGIN;
            let bottom = item.bounds[3] <= page.crop[1] + height * MARGIN;
            if !top && !bottom {
                continue;
            }
            let normalized = normalized(&item.text);
            if normalized.is_empty() {
                continue;
            }
            // The height, from the edge the text is closest to, within 4 points.
            #[allow(clippy::cast_possible_truncation)]
            let level = if top {
                ((page.crop[3] - item.bounds[3]) / 4.0).round() as i64
            } else {
                -((item.bounds[1] - page.crop[1]) / 4.0).round() as i64 - 1
            };
            let key = format!("{level}:{normalized}");
            seen_on.entry(key.clone()).or_default().insert(number);
            candidates.push(((number, index), key, page_number(&normalized)));
        }
    }
    candidates
        .into_iter()
        .filter(|(_, key, number)| *number || seen_on.get(key).is_some_and(|on| on.len() >= 2))
        .map(|(at, _, _)| at)
        .collect()
}

/// Text compared across pages: lower case, white space out, every run of digits one `#`.
fn normalized(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
            continue;
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// A page number alone: `3`, `- 3 -`, `page 3`, `3/10`, `page 3 of 10`, `page 3 sur 10`.
fn page_number(normalized: &str) -> bool {
    let mut rest = normalized.replace("page", "").replace("p.", "");
    for word in ["of", "sur", "de", "/", "-", "\u{2013}", "\u{2014}", "|"] {
        rest = rest.replace(word, "");
    }
    !rest.is_empty() && rest.chars().all(|c| c == '#') && rest.len() <= 2
}

/// How each unit of a page is tied, from its layout: a paragraph each `P`, a figure each
/// `Figure`, running heads and backgrounds artifacts, and what is in no item (paths, shadings,
/// nothing visible) a layout artifact. Elements are added to `elements` in reading order.
fn marks(
    content: &Content<'_>,
    items: &[Item],
    page: ObjId,
    number: usize,
    heads: &HashSet<(usize, usize)>,
    elements: &mut Vec<Element>,
) -> Vec<Option<Mark>> {
    let mut marks: Vec<Option<Mark>> = content
        .units
        .iter()
        .map(|unit| (!unit.tied).then_some(Mark::Artifact(Artifact::Layout)))
        .collect();
    for (index, item) in items.iter().enumerate() {
        let mark = if heads.contains(&(number, index)) {
            Mark::Artifact(Artifact::Pagination)
        } else {
            match item.role {
                Role::Background => Mark::Artifact(Artifact::Background),
                Role::Paragraph | Role::Figure => {
                    let figure = item.role == Role::Figure;
                    elements.push(Element {
                        kind: if figure { "Figure" } else { "P" },
                        page,
                        mcids: Vec::new(),
                        alt: figure
                            .then(|| item.alt.clone().unwrap_or_else(|| GENERIC_ALT.to_vec())),
                    });
                    Mark::Element(elements.len() - 1)
                }
            }
        };
        for unit in &item.units {
            if let Some(slot) = marks.get_mut(*unit)
                && slot.is_some()
            {
                *slot = Some(mark);
            }
        }
    }
    marks
}

/// How each unit of a page is tied when its layout is unknown: consecutive text in one
/// paragraph, each image a figure, paths and shadings artifacts.
fn marks_in_content_order(
    content: &Content<'_>,
    page: ObjId,
    elements: &mut Vec<Element>,
) -> Vec<Option<Mark>> {
    let mut marks = Vec::with_capacity(content.units.len());
    let mut paragraph: Option<usize> = None;
    let add = |elements: &mut Vec<Element>, kind, alt| {
        elements.push(Element {
            kind,
            page,
            mcids: Vec::new(),
            alt,
        });
        elements.len() - 1
    };
    for unit in &content.units {
        if unit.tied {
            marks.push(None);
            continue;
        }
        let mark = match unit.kind {
            Kind::Text => Mark::Element(*paragraph.get_or_insert_with(|| add(elements, "P", None))),
            Kind::XObject | Kind::InlineImage => {
                paragraph = None;
                Mark::Element(add(elements, "Figure", Some(GENERIC_ALT.to_vec())))
            }
            Kind::Path | Kind::Shading => Mark::Artifact(Artifact::Layout),
        };
        marks.push(Some(mark));
    }
    marks
}

/// Annotations of a document without a tree may keep the `/StructParent` of one left behind,
/// which would now lead to a page's entry of the new parent tree: taken out.
fn forget_annotation_parents(doc: &mut Document) -> Result<()> {
    let mut annotations = Vec::new();
    for id in doc.page_ids().into_iter().flatten() {
        let Some(page) = doc.object(id)? else {
            continue;
        };
        let Some(annots) = page.as_dict().and_then(|page| page.get(b"Annots")).cloned() else {
            continue;
        };
        if let Some(Object::Array(items)) = edit::resolved(doc, &annots)? {
            annotations.extend(items.iter().filter_map(Object::as_ref_id));
        }
    }
    let mut seen = HashSet::new();
    for id in annotations {
        if !seen.insert(id) {
            continue;
        }
        let has_parent = doc.object(id)?.is_some_and(|annotation| {
            annotation
                .as_dict()
                .is_some_and(|a| a.contains_key(b"StructParent"))
        });
        if has_parent {
            edit::edit_dict(doc, &Location::of(id), false, |annotation| {
                annotation.remove(b"StructParent");
            })?;
        }
    }
    Ok(())
}

/// Forms (XObjects, appearances) of a document without a tree may keep the `/MCID`s and
/// `/StructParents` of one left behind: content an artifact encloses would read as tied
/// (PDF/UA 7.1-2), and their keys would lead into the new parent tree. Taken out, the offsets of
/// the content kept, so that it draws as before.
fn forget_form_identifiers(doc: &mut Document, report: &mut Report) -> Result<()> {
    for id in doc.reachable_objects()? {
        let Some(Object::Stream(stream)) = doc.object_without_data(id)? else {
            continue;
        };
        if stream.dict.get(b"Subtype").and_then(Object::as_name) != Some(&b"Form"[..]) {
            continue;
        }
        let parents =
            stream.dict.contains_key(b"StructParents") || stream.dict.contains_key(b"StructParent");
        let blanked = edit::stream_data(doc, id, true)
            .or_else(|| edit::decoded(doc, id))
            .and_then(|data| content::without_mcids(&data, |_| false));
        match blanked {
            Some(data) => {
                let Object::Stream(compressed) = flate(&data) else {
                    continue;
                };
                let mut dict = stream.dict.clone();
                for key in [
                    &b"Filter"[..],
                    b"DecodeParms",
                    b"StructParents",
                    b"StructParent",
                ] {
                    dict.remove(key);
                }
                for (key, value) in compressed.dict.iter() {
                    dict.insert(key.clone(), value.clone());
                }
                doc.set_object(
                    id,
                    Object::Stream(Stream {
                        dict,
                        data: compressed.data,
                    }),
                );
                report.change("marked_content_identifier_removed", 1);
            }
            None if parents => {
                edit::edit_dict(doc, &Location::of(id), false, |dict| {
                    dict.remove(b"StructParents");
                    dict.remove(b"StructParent");
                })?;
            }
            None => {}
        }
    }
    Ok(())
}

/// The document has a structure tree with elements in it.
fn has_structure(doc: &Document, catalog: &Location) -> Result<bool> {
    let Some(root) = edit::read_dict(doc, catalog)? else {
        return Ok(false);
    };
    let tree = match root.get(b"StructTreeRoot") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    let Some(kids) = tree
        .as_ref()
        .and_then(Object::as_dict)
        .and_then(|tree| tree.get(b"K"))
    else {
        return Ok(false);
    };
    Ok(match edit::resolved(doc, kids)? {
        Some(Object::Array(items)) => !items.is_empty(),
        Some(Object::Dict(_)) => true,
        _ => false,
    })
}

/// The decoded content of each page, once per page object. A page whose content does not
/// decode in full is left as it is, and counted.
fn contents(doc: &Document, report: &mut Report) -> Vec<(usize, ObjId, Vec<u8>)> {
    let pdf = doc.hayro();
    let mut seen = HashSet::new();
    let mut pages = Vec::new();
    for (index, (page, id)) in pdf.pages().iter().zip(doc.page_ids()).enumerate() {
        let Some(id) = id else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let raw = page.raw();
        let data = if let Some(stream) = raw.get::<HayroStream<'_>>(b"Contents") {
            stream.decoded().ok().map(Cow::into_owned)
        } else if let Some(array) = raw.get::<Array<'_>>(b"Contents") {
            let mut joined = Some(Vec::new());
            for stream in array.iter::<HayroStream<'_>>() {
                match (stream.decoded(), joined.as_mut()) {
                    (Ok(data), Some(joined)) => {
                        joined.extend_from_slice(&data);
                        // Streams are joined as if one, with white space between them.
                        joined.push(b'\n');
                    }
                    _ => joined = None,
                }
            }
            joined
        } else {
            continue;
        };
        match data {
            Some(data) => pages.push((index, id, data)),
            None => report.residual_add("page_content_untied", 1),
        }
    }
    pages
}

fn flate(data: &[u8]) -> Object {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let compressed = encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    let mut dict = Dict::new();
    if compressed.is_empty() && !data.is_empty() {
        return Object::Stream(Stream {
            dict,
            data: data.to_vec(),
        });
    }
    dict.insert("Filter", Name::from("FlateDecode"));
    Object::Stream(Stream {
        dict,
        data: compressed,
    })
}

/// The structure tree: its root, a `Document` element, the elements, the parent tree.
fn write_tree(
    doc: &mut Document,
    catalog: &Location,
    elements: &[Element],
    parent_tree: &[(i64, Vec<usize>)],
) -> Result<()> {
    let root_id = doc.add_object(Object::Null)?;
    let document_id = doc.add_object(Object::Null)?;
    let ids = add_elements(doc, elements, document_id)?;
    let mut document = Dict::new();
    document.insert("Type", Name::from("StructElem"));
    document.insert("S", Name::from("Document"));
    document.insert("P", root_id);
    document.insert(
        "K",
        ids.iter().map(|id| Object::Ref(*id)).collect::<Vec<_>>(),
    );
    doc.set_object(document_id, Object::Dict(document));
    let mut nums = Vec::with_capacity(parent_tree.len() * 2);
    for (key, owners) in parent_tree {
        nums.push(Object::from(*key));
        nums.push(
            owners
                .iter()
                .map(|owner| Object::Ref(ids[*owner]))
                .collect::<Vec<_>>()
                .into(),
        );
    }
    let mut numbers = Dict::new();
    numbers.insert("Nums", nums);
    let mut root = Dict::new();
    root.insert("Type", Name::from("StructTreeRoot"));
    root.insert("K", document_id);
    root.insert("ParentTree", doc.add_object(Object::Dict(numbers))?);
    root.insert(
        "ParentTreeNextKey",
        i64::try_from(parent_tree.len()).unwrap_or(i64::MAX),
    );
    doc.set_object(root_id, Object::Dict(root));
    edit::edit_dict(doc, catalog, false, |catalog| {
        catalog.insert("StructTreeRoot", root_id);
    })?;
    Ok(())
}
