//! What a page's links lead to, and the labels pages are shown with.

use crate::document::Document;
use crate::error::{Error, Result};
use crate::geometry::Rect;
use crate::object::{Dict, Object};
use crate::outline::Destinations;
use crate::text::decode_text_string;

/// How deep the page labels' number tree is followed, and how many of its nodes are read.
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_NODES: usize = 4096;
/// The longest numeral a label is written with: past it (a range starting at a huge number), the
/// number is written in decimal.
const MAX_NUMERAL: usize = 64;

/// A link of a page: where it is clicked, and where it leads.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Link {
    /// Its rectangle, in the page's default user space (unrotated, y up).
    pub rect: Rect,
    pub target: LinkTarget,
}

/// Where a link leads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum LinkTarget {
    /// A page of this document, from 0: through a destination (named ones resolved), a `GoTo`
    /// action, or the named actions `FirstPage` and `LastPage`.
    Page(usize),
    /// A URI (`URI` action), as written.
    Uri(String),
    /// Another file (`GoToR`, `Launch`): its name as written, and the page there (from 0) when
    /// the destination gives it by number.
    File {
        name: String,
        page_index: Option<usize>,
    },
    /// A named action that leads elsewhere than to a fixed page: `NextPage`, `PrevPage`, a
    /// viewer's own.
    Named(String),
    /// Nowhere sealwax can tell: a destination missing or broken, or an action of another kind;
    /// the action's type when it has one (`JavaScript`, `SubmitForm`...).
    Unknown { action: Option<String> },
}

impl Document {
    /// The links of page `page_index` (from 0), in the order of its `/Annots`, each with its
    /// rectangle and where it leads. Never a guess: what cannot be followed is
    /// [`LinkTarget::Unknown`].
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a page the document does not have;
    /// [`Error::Timeout`] past the deadline.
    pub fn links(&self, page_index: usize) -> Result<Vec<Link>> {
        let Some(page) = self.page_ids().get(page_index).copied() else {
            return Err(Error::InvalidArgument { what: "page index" });
        };
        let Some((annotations, _)) = page
            .map(|id| self.page_annotations(id))
            .transpose()?
            .flatten()
        else {
            return Ok(Vec::new());
        };
        let catalog = self.dict_at(self.catalog_id())?.unwrap_or_default();
        let mut destinations = Destinations::new(self, catalog);
        let mut links = Vec::new();
        let max = usize::try_from(self.limits().max_annotations).unwrap_or(usize::MAX);
        for (index, annotation) in annotations.iter().take(max).enumerate() {
            if index % 1024 == 1023 {
                self.check_time()?;
            }
            let Some(annotation) = self.annotation(annotation)? else {
                continue;
            };
            if annotation.get(b"Subtype").and_then(Object::as_name) != Some(b"Link") {
                continue;
            }
            let Some(rect) = self.rect(annotation.get(b"Rect"))? else {
                continue;
            };
            let target = self.link_target(&annotation, &mut destinations)?;
            links.push(Link { rect, target });
        }
        Ok(links)
    }

    /// The rectangle `object` is, when it is one of four numbers.
    fn rect(&self, object: Option<&Object>) -> Result<Option<Rect>> {
        let Some(Object::Array(items)) = self.resolved(object)? else {
            return Ok(None);
        };
        let mut numbers = [0.0; 4];
        for (slot, item) in numbers.iter_mut().zip(&items) {
            match self.resolved(Some(item))? {
                Some(Object::Integer(n)) => *slot = n as f64,
                Some(Object::Real(n)) if n.is_finite() => *slot = n,
                _ => return Ok(None),
            }
        }
        Ok((items.len() == 4)
            .then(|| Rect::from_corners(numbers[0], numbers[1], numbers[2], numbers[3])))
    }

    fn link_target(&self, link: &Dict, destinations: &mut Destinations<'_>) -> Result<LinkTarget> {
        let unknown = |action: Option<&[u8]>| LinkTarget::Unknown {
            action: action.map(|a| String::from_utf8_lossy(a).into_owned()),
        };
        if link.contains_key(b"Dest") {
            let destination = self.resolved(link.get(b"Dest"))?;
            return Ok(destinations
                .page_of(destination, 0)?
                .map_or_else(|| unknown(None), LinkTarget::Page));
        }
        let Some(action) = self.resolved_dict(link.get(b"A"))? else {
            return Ok(unknown(None));
        };
        let kind = action.get(b"S").and_then(Object::as_name);
        Ok(match kind {
            Some(b"GoTo") => {
                let destination = self.resolved(action.get(b"D"))?;
                destinations
                    .page_of(destination, 0)?
                    .map_or_else(|| unknown(kind), LinkTarget::Page)
            }
            Some(b"URI") => match self.resolved(action.get(b"URI"))? {
                Some(Object::String(uri)) => LinkTarget::Uri(String::from_utf8_lossy(&uri).into()),
                _ => unknown(kind),
            },
            Some(b"GoToR" | b"Launch") => match self.file_name(action.get(b"F"))? {
                Some(name) => {
                    let page_index = match self.resolved(action.get(b"D"))? {
                        Some(Object::Array(items)) => items
                            .first()
                            .and_then(Object::as_i64)
                            .and_then(|page| usize::try_from(page).ok()),
                        _ => None,
                    };
                    LinkTarget::File { name, page_index }
                }
                None => unknown(kind),
            },
            Some(b"Named") => match action.get(b"N").and_then(Object::as_name) {
                Some(b"FirstPage") if self.page_count() > 0 => LinkTarget::Page(0),
                Some(b"LastPage") if self.page_count() > 0 => {
                    LinkTarget::Page(self.page_count() - 1)
                }
                Some(name) => LinkTarget::Named(String::from_utf8_lossy(name).into_owned()),
                None => unknown(kind),
            },
            _ => unknown(kind),
        })
    }

    /// The name a file specification gives: a string, or a dictionary's `/UF`, else its `/F`.
    fn file_name(&self, spec: Option<&Object>) -> Result<Option<String>> {
        Ok(match self.resolved(spec)? {
            Some(Object::String(name)) => Some(decode_text_string(&name)),
            Some(Object::Dict(dict)) => {
                let mut found = None;
                for key in [&b"UF"[..], b"F"] {
                    if let Some(Object::String(name)) = self.resolved(dict.get(key))? {
                        found = Some(decode_text_string(&name));
                        break;
                    }
                }
                found
            }
            _ => None,
        })
    }

    /// The label each page is shown with (`/PageLabels`): a prefix, then the page's number in
    /// its range written in decimal, roman numerals or letters ("i", "ii", "1", "A-1"). `None`
    /// when the document gives no labels (readers then show the pages' numbers). A page before
    /// the first range is labelled with its number.
    ///
    /// # Errors
    /// [`Error::Timeout`] past the deadline.
    pub fn page_labels(&self) -> Result<Option<Vec<String>>> {
        let Some(catalog) = self.dict_at(self.catalog_id())? else {
            return Ok(None);
        };
        let Some(root) = self.resolved_dict(catalog.get(b"PageLabels"))? else {
            return Ok(None);
        };
        let count = self.page_count();
        // The ranges, by the page they start at.
        let mut ranges: Vec<(usize, Dict)> = Vec::new();
        let mut stack = vec![(root, 0)];
        let mut read = 0;
        while let Some((node, depth)) = stack.pop() {
            read += 1;
            if read > MAX_TREE_NODES {
                break;
            }
            if read % 256 == 0 {
                self.check_time()?;
            }
            if let Some(Object::Array(nums)) = self.resolved(node.get(b"Nums"))? {
                for pair in nums.as_chunks::<2>().0 {
                    let Some(start) = pair[0].as_i64().and_then(|n| usize::try_from(n).ok()) else {
                        continue;
                    };
                    if start < count
                        && let Some(range) = self.resolved_dict(Some(&pair[1]))?
                    {
                        ranges.push((start, range));
                    }
                }
            }
            if depth < MAX_TREE_DEPTH
                && let Some(Object::Array(kids)) = self.resolved(node.get(b"Kids"))?
            {
                for kid in kids.iter().rev() {
                    if let Some(kid) = self.resolved_dict(Some(kid))? {
                        stack.push((kid, depth + 1));
                    }
                }
            }
        }
        if ranges.is_empty() {
            return Ok(None);
        }
        ranges.sort_by_key(|(start, _)| *start);
        let mut labels = Vec::with_capacity(count);
        let mut next = 0;
        for page in 0..count {
            while ranges.get(next).is_some_and(|(start, _)| *start <= page) {
                next += 1;
            }
            let label = match next.checked_sub(1).map(|at| &ranges[at]) {
                Some((start, range)) => self.label(range, page - start)?,
                None => (page + 1).to_string(),
            };
            labels.push(label);
        }
        Ok(Some(labels))
    }

    /// The label of the `offset`-th page (from 0) of the range `range` describes.
    fn label(&self, range: &Dict, offset: usize) -> Result<String> {
        let mut label = match self.resolved(range.get(b"P"))? {
            Some(Object::String(prefix)) => decode_text_string(&prefix),
            _ => String::new(),
        };
        let first = range
            .get(b"St")
            .and_then(Object::as_i64)
            .filter(|start| *start >= 1)
            .and_then(|start| u64::try_from(start).ok())
            .unwrap_or(1);
        let number = first.saturating_add(offset as u64);
        let numeral = match range.get(b"S").and_then(Object::as_name) {
            Some(b"D") => Some(number.to_string()),
            Some(b"R") => Some(roman(number)),
            Some(b"r") => Some(roman(number).to_lowercase()),
            Some(b"A") => Some(letters(number)),
            Some(b"a") => Some(letters(number).to_lowercase()),
            _ => None,
        };
        if let Some(numeral) = numeral {
            label.push_str(&numeral);
        }
        Ok(label)
    }
}

/// `number` in upper-case roman numerals, a thousand an `M`; in decimal past [`MAX_NUMERAL`].
fn roman(number: u64) -> String {
    if number / 1000 > MAX_NUMERAL as u64 {
        return number.to_string();
    }
    const DIGITS: [(u64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut left = number;
    let mut out = String::new();
    for (value, digits) in DIGITS {
        while left >= value {
            out.push_str(digits);
            left -= value;
        }
    }
    out
}

/// `number` in upper-case letters: A to Z, then AA to ZZ, AAA... (ISO 32000-1, 12.4.2); in
/// decimal past [`MAX_NUMERAL`].
fn letters(number: u64) -> String {
    let Some(index) = number.checked_sub(1) else {
        return String::new();
    };
    let times = index / 26 + 1;
    if times > MAX_NUMERAL as u64 {
        return number.to_string();
    }
    let letter = char::from(b'A' + (index % 26) as u8);
    std::iter::repeat_n(letter, times as usize).collect()
}

#[cfg(test)]
mod tests {
    use super::{letters, roman};

    #[test]
    fn numerals_are_written_as_readers_write_them() {
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(roman(4), "IV");
        assert_eq!(letters(1), "A");
        assert_eq!(letters(27), "AA");
        assert_eq!(letters(53), "AAA");
        assert_eq!(letters(0), "");
        // A huge start does not make a huge label.
        assert_eq!(roman(u64::MAX), u64::MAX.to_string());
        assert_eq!(letters(1 << 40), (1_u64 << 40).to_string());
    }
}
