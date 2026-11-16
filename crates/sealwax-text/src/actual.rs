//! What content reads as when the document says so: an `/ActualText` (ISO 32000-1, 14.9.4) in a
//! marked-content sequence's properties, or on the structure element a sequence's `/MCID` ties it
//! to. Producers put one around what draws a different text than it reads: leaders drawn for a
//! tab, a word split by a hyphen, letters drawn as images. The outermost one replaces all the
//! content inside it, and readers read it instead.
//!
//! The standard has an element's `/ActualText` replace its children's content too; readers do
//! not look past the element the content is tied to (MuPDF), and producers leave on ancestors
//! texts that would replace whole paragraphs with a character. Ancestors are not looked at.

use std::collections::HashMap;

use hayro_interpret::hayro_syntax::object::{
    Array, Dict, Object, ObjectIdentifier, String as PdfString,
};
use hayro_interpret::hayro_syntax::xref::XRef;
use sealwax_core::decode_text_string;

/// How deep a number tree is followed, and how many of its nodes are read for one key.
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_NODES: usize = 4096;
/// How many content items one page or form ties to the structure, at most.
const MAX_ITEMS: usize = 1 << 20;

/// What a replacement text replaces: each is read once, on the first glyph it covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    /// A structure element.
    Element(ObjectIdentifier),
    /// A marked-content sequence, or a structure element written directly in its parent: the
    /// how-manieth.
    Sequence(usize),
}

/// A replacement text and what it replaces.
pub(crate) type Replacement = (Key, String);

/// The structure tree's `/ParentTree`, read as content asks.
pub(crate) struct Structure<'a> {
    parent_tree: Option<Dict<'a>>,
    /// The structure elements of each `/StructParents` read, by `/MCID`.
    items: HashMap<i64, Vec<Option<Dict<'a>>>>,
    /// Sequences and direct elements seen.
    sequences: usize,
}

impl<'a> Structure<'a> {
    pub(crate) fn new(xref: &'a XRef) -> Self {
        let parent_tree = xref
            .get::<Dict<'a>>(xref.root_id())
            .and_then(|catalog| catalog.get::<Dict<'a>>(b"StructTreeRoot"))
            .and_then(|root| root.get::<Dict<'a>>(b"ParentTree"));
        Self {
            parent_tree,
            items: HashMap::new(),
            sequences: 0,
        }
    }

    /// A replacement text written in a sequence's properties.
    pub(crate) fn in_properties(&mut self, properties: &Dict<'_>) -> Option<Replacement> {
        let text = properties.get::<PdfString<'_>>(b"ActualText")?;
        Some((self.next_sequence(), decode_text_string(text.as_bytes())))
    }

    /// The replacement text of the content item `mcid` of the page or form whose
    /// `/StructParents` is `parents`.
    pub(crate) fn of_item(&mut self, parents: i64, mcid: i64) -> Option<Replacement> {
        if !self.items.contains_key(&parents) {
            let elements = self
                .parent_tree
                .as_ref()
                .and_then(|tree| number_tree_get(tree, parents, 0, &mut { MAX_TREE_NODES }))
                .and_then(|value| match value {
                    Object::Array(array) => Some(
                        array
                            .iter::<Object<'a>>()
                            .take(MAX_ITEMS)
                            .map(|item| match item {
                                Object::Dict(element) => Some(element),
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            self.items.insert(parents, elements);
        }
        let element = self
            .items
            .get(&parents)
            .and_then(|elements| elements.get(usize::try_from(mcid).ok()?))
            .cloned()
            .flatten()?;
        self.of_element(element)
    }

    /// The replacement text of the structure element at `/StructParent` `key`: a form drawn as
    /// one content item.
    pub(crate) fn of_object(&mut self, key: i64) -> Option<Replacement> {
        let element = self
            .parent_tree
            .as_ref()
            .and_then(|tree| number_tree_get(tree, key, 0, &mut { MAX_TREE_NODES }))
            .and_then(|value| match value {
                Object::Dict(element) => Some(element),
                _ => None,
            })?;
        self.of_element(element)
    }

    /// The replacement text of `element`, if it has one.
    fn of_element(&mut self, element: Dict<'a>) -> Option<Replacement> {
        let text = element.get::<PdfString<'_>>(b"ActualText")?;
        let key = match element.obj_id() {
            Some(id) => Key::Element(id),
            None => self.next_sequence(),
        };
        Some((key, decode_text_string(text.as_bytes())))
    }

    fn next_sequence(&mut self) -> Key {
        self.sequences += 1;
        Key::Sequence(self.sequences)
    }
}

/// The value of `key` in the number tree `node`.
fn number_tree_get<'a>(
    node: &Dict<'a>,
    key: i64,
    depth: usize,
    budget: &mut usize,
) -> Option<Object<'a>> {
    if depth > MAX_TREE_DEPTH || *budget == 0 {
        return None;
    }
    *budget -= 1;
    if let Some(limits) = node.get::<Array<'a>>(b"Limits") {
        let mut bounds = limits.iter::<i64>();
        if let (Some(low), Some(high)) = (bounds.next(), bounds.next())
            && !(low..=high).contains(&key)
        {
            return None;
        }
    }
    if let Some(nums) = node.get::<Array<'a>>(b"Nums") {
        let mut entries = nums.iter::<Object<'a>>();
        while let (Some(at), Some(value)) = (entries.next(), entries.next()) {
            if let Object::Number(at) = at
                && at.as_i64() == key
            {
                return Some(value);
            }
        }
    }
    for kid in node.get::<Array<'a>>(b"Kids")?.iter::<Dict<'a>>() {
        if let Some(found) = number_tree_get(&kid, key, depth + 1, budget) {
            return Some(found);
        }
    }
    None
}
