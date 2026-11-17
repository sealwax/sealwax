//! Documents made of the pages of others: an empty document, and pages appended from another,
//! with everything they reach copied and renumbered.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use crate::document::{Document, OpenOptions};
use crate::error::{Error, Result};
use crate::geometry::PageGeometry;
use crate::object::{Dict, Name, ObjId, Object};
use crate::text::{decode_text_string, encode_text_string};

/// How deep a copied object may nest.
const MAX_NESTING: usize = 64;

/// Keys a copied page loses: they point into the source's page tree and article threads.
const PAGE_LEFT_BEHIND: [&[u8]; 2] = [b"Parent", b"B"];
/// Keys any copied object loses: they point into the source's structure tree.
const LEFT_BEHIND: [&[u8]; 2] = [b"StructParents", b"StructParent"];

/// The smallest PDF: a catalog and a page tree without pages.
const EMPTY: &[u8] = b"%PDF-1.7\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
xref\n0 3\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n\
trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n110\n%%EOF\n";

impl Document {
    /// A document without pages, to append the pages of others to: a catalog and an empty page
    /// tree, nothing else. It cannot be saved until it has a page.
    ///
    /// # Errors
    /// None in practice: the document is built in.
    pub fn empty() -> Result<Self> {
        Self::open(EMPTY.to_vec(), &OpenOptions::default())
    }

    /// Append the pages of `other` after this document's, in order. Each page is copied with
    /// everything it reaches (contents, resources, annotations), renumbered; an object several
    /// pages share is copied once; a reference to another page of `other` (a link's destination)
    /// leads to its copy. Each page carries the geometry and resources it had in `other`. What
    /// leads into `other`'s page tree, structure tree or article threads is left behind: those
    /// are not copied, nor are `other`'s outlines, form or catalog. Returns the pages appended.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when a page of `other` is not an indirect object;
    /// [`Error::LimitExceeded`] when an object nests deeper than sealwax accepts;
    /// [`Error::Timeout`] past either document's deadline. This document may then hold some of
    /// the copied objects, unreachable, which a save leaves out.
    pub fn append_pages(&mut self, other: &Document) -> Result<usize> {
        self.append_page_range(other, 0..other.page_count())
    }

    /// Append pages `pages` of `other` (from 0, the end excluded), as [`Document::append_pages`]
    /// appends them all. A reference to one of `other`'s pages left out (a link to it) leads to
    /// `null` instead, and nothing of those pages is copied, their annotations included (a field's
    /// widgets on them, reached through the field, among them).
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for pages `other` does not have; as
    /// [`Document::append_pages`].
    pub fn append_page_range(&mut self, other: &Document, pages: Range<usize>) -> Result<usize> {
        if pages.start > pages.end || pages.end > other.page_count() {
            return Err(Error::InvalidArgument { what: "page range" });
        }
        let wanted: Vec<(usize, i32)> = pages.map(|page| (page, 0)).collect();
        let copied = self.copy_pages(other, &wanted, false)?;
        let count = copied.pages.len();
        self.appended.extend(copied.pages);
        Ok(count)
    }

    /// A new document of the pages `picks` names, in that order, each copied from its source with
    /// everything it reaches (as [`Document::append_pages`] copies them) and turned as the pick
    /// says. Nothing of a source's pages left out is copied, their annotations included.
    ///
    /// With `keep_form`, the sources' forms come along: each field keeps its widgets on the pages
    /// copied, and only those (a field left without one is left out); the fields join the new
    /// document's form, with the defaults (`/DA`, `/DR`, `/Q`, `/NeedAppearances`) of the first
    /// source that has them; and a top field whose name an earlier source already gave is
    /// renamed `name (2)`, `name (3)`..., two fields of one name being one field to a reader.
    /// Without it, the forms are left behind, as [`Document::append_pages`] leaves them. The
    /// sources' outlines, structure trees and catalogs are left behind either way.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] when `picks` is empty, or names a source or a page that does
    /// not exist, or a page twice; as [`Document::append_pages`].
    pub fn compose(sources: &[&Document], picks: &[PagePick], keep_form: bool) -> Result<Document> {
        let invalid = Error::InvalidArgument { what: "page list" };
        let mut named = std::collections::HashSet::new();
        for pick in picks {
            let Some(source) = sources.get(pick.source) else {
                return Err(invalid);
            };
            if pick.page >= source.page_count() || !named.insert((pick.source, pick.page)) {
                return Err(invalid);
            }
        }
        let first = picks.first().ok_or(invalid)?;
        let mut out = Document::empty()?;
        out.limits = sources[first.source].limits;
        let mut placed: Vec<Option<(ObjId, PageGeometry)>> = vec![None; picks.len()];
        let mut fields: Vec<ObjId> = Vec::new();
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut defaults = Dict::new();
        for (index, source) in sources.iter().enumerate() {
            let positions: Vec<usize> = (0..picks.len())
                .filter(|at| picks[*at].source == index)
                .collect();
            if positions.is_empty() {
                continue;
            }
            let wanted: Vec<(usize, i32)> = positions
                .iter()
                .map(|at| (picks[*at].page, picks[*at].quarter_turns))
                .collect();
            let copied = out.copy_pages(source, &wanted, keep_form)?;
            for (at, page) in positions.iter().zip(copied.pages) {
                placed[*at] = Some(page);
            }
            if keep_form {
                for (key, value) in copied.defaults.iter() {
                    if !defaults.contains_key(key.as_bytes()) {
                        defaults.insert(key.clone(), value.clone());
                    }
                }
                out.adopt_fields(&copied.widgets, &mut fields, &mut names)?;
            }
        }
        out.appended.extend(placed.into_iter().flatten());
        if keep_form && !fields.is_empty() {
            let mut form = defaults;
            form.insert(
                "Fields",
                Object::Array(fields.into_iter().map(Object::Ref).collect()),
            );
            let catalog_id = out.catalog_id();
            if let Some(Object::Dict(mut catalog)) = out.object(catalog_id)? {
                catalog.insert("AcroForm", Object::Dict(form));
                out.set_object(catalog_id, Object::Dict(catalog));
            }
        }
        Ok(out)
    }

    /// A new document of this one's pages `pages`, in that order, each turned the quarter turns
    /// given (clockwise; counterclockwise when negative), its form kept for those pages
    /// ([`Document::compose`]): turning, deleting, moving, reordering and taking out pages are
    /// each one arrangement. Pages left out are not copied, nor anything only they reach.
    ///
    /// # Errors
    /// As [`Document::compose`]: [`Error::InvalidArgument`] when `pages` is empty, or names a
    /// page the document does not have, or a page twice.
    pub fn arrange(&self, pages: &[(usize, i32)]) -> Result<Document> {
        let picks: Vec<PagePick> = pages
            .iter()
            .map(|(page, turns)| PagePick::new(0, *page, *turns))
            .collect();
        Self::compose(&[self], &picks, true)
    }

    /// A new document of this one's pages with every page of `others`, one document after the
    /// other, inserted before page `at` (from 0; the page count to insert at the end), the forms
    /// kept ([`Document::compose`]; a field an inserted document names as an earlier one does is
    /// renamed).
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] when `at` is past the last page; as [`Document::compose`].
    pub fn insert(&self, at: usize, others: &[&Document]) -> Result<Document> {
        if at > self.page_count() {
            return Err(Error::InvalidArgument {
                what: "insert position",
            });
        }
        let mut sources: Vec<&Document> = vec![self];
        sources.extend_from_slice(others);
        let mut picks: Vec<PagePick> = (0..at).map(|page| PagePick::new(0, page, 0)).collect();
        for (index, other) in others.iter().enumerate() {
            picks.extend((0..other.page_count()).map(|page| PagePick::new(index + 1, page, 0)));
        }
        picks.extend((at..self.page_count()).map(|page| PagePick::new(0, page, 0)));
        Self::compose(&sources, &picks, true)
    }

    /// Copy pages `wanted` of `other` (each with the quarter turns added to its rotation), in that
    /// order, without adding them to this document's pages; with `keep_form`, the defaults of
    /// `other`'s form too. What only `other`'s other pages reach is never copied: those pages
    /// lead to `null`, and so do their annotations.
    fn copy_pages(
        &mut self,
        other: &Document,
        wanted: &[(usize, i32)],
        keep_form: bool,
    ) -> Result<Copied> {
        let ids = other.page_ids();
        let geometry = other.geometry();
        let inherited = other.inherited_resources()?;
        let kept: std::collections::HashSet<usize> = wanted.iter().map(|(page, _)| *page).collect();
        let mut copies: HashMap<ObjId, ObjId> = HashMap::new();
        let mut pending: Vec<ObjId> = Vec::new();
        let mut left_out = None;
        let mut null = |doc: &mut Document| -> Result<ObjId> {
            if let Some(null) = left_out {
                return Ok(null);
            }
            let id = doc.add_object(Object::Null)?;
            left_out = Some(id);
            Ok(id)
        };
        let mut kept_annotations = std::collections::HashSet::new();
        let mut other_annotations = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let id = id.ok_or(Error::Unsupported {
                what: "a page that is not an indirect object",
            })?;
            let annotations = annotation_ids(other, id)?;
            if kept.contains(&index) {
                kept_annotations.extend(annotations);
            } else {
                other_annotations.extend(annotations);
            }
            if let Entry::Vacant(slot) = copies.entry(id) {
                let copy = if kept.contains(&index) {
                    self.add_object(Object::Null)?
                } else {
                    null(self)?
                };
                slot.insert(copy);
            }
        }
        for annotation in other_annotations {
            if !kept_annotations.contains(&annotation) && !copies.contains_key(&annotation) {
                let null = null(self)?;
                copies.insert(annotation, null);
            }
        }
        let mut pages = Vec::with_capacity(wanted.len());
        for (index, quarter_turns) in wanted {
            let (Some(Some(id)), Some(page_geometry)) = (ids.get(*index), geometry.get(*index))
            else {
                return Err(Error::InvalidArgument { what: "page range" });
            };
            other.check_time()?;
            self.check_time()?;
            let Some(Object::Dict(mut page)) = other.object(*id)? else {
                return Err(Error::Unsupported {
                    what: "a page that is not a dictionary",
                });
            };
            let rect = |r: crate::geometry::Rect| -> Object {
                vec![r.x1.into(), r.y1.into(), r.x2.into(), r.y2.into()].into()
            };
            for key in PAGE_LEFT_BEHIND {
                page.remove(key);
            }
            let rotation = page_geometry.rotation.turned(*quarter_turns);
            page.insert("Type", Name::from("Page"));
            page.insert("MediaBox", rect(page_geometry.media_box));
            page.insert("CropBox", rect(page_geometry.crop_box));
            page.insert("Rotate", i64::from(rotation.degrees()));
            if let Some(Some(resources)) = inherited.get(*index)
                && !page.contains_key(b"Resources")
            {
                page.insert("Resources", resources.clone());
            }
            let copied = copy(self, Object::Dict(page), &mut copies, &mut pending, 0)?;
            self.set_object(copies[id], copied);
            pages.push((
                copies[id],
                PageGeometry {
                    rotation,
                    ..*page_geometry
                },
            ));
        }
        let mut defaults = Dict::new();
        if keep_form && let Some(form) = acro_form(other)? {
            for key in FORM_DEFAULTS {
                if let Some(value) = form.get(key) {
                    let value = copy(self, value.clone(), &mut copies, &mut pending, 0)?;
                    defaults.insert(Name::from(key), value);
                }
            }
        }
        let mut copied: usize = 0;
        while let Some(id) = pending.pop() {
            copied += 1;
            if copied.is_multiple_of(1024) {
                other.check_time()?;
                self.check_time()?;
            }
            let object = other.object(id)?.unwrap_or(Object::Null);
            let copied = copy(self, object, &mut copies, &mut pending, 0)?;
            self.set_object(copies[&id], copied);
        }
        let mut widgets = Vec::new();
        if keep_form {
            for (page, _) in &pages {
                for annotation in annotation_ids(self, *page)? {
                    if let Some(Object::Dict(dict)) = self.object(annotation)?
                        && dict.get(b"Subtype").and_then(Object::as_name) == Some(&b"Widget"[..])
                    {
                        widgets.push(annotation);
                    }
                }
            }
        }
        Ok(Copied {
            pages,
            widgets,
            defaults,
        })
    }

    /// Make the fields of `widgets` (copied) part of this document's form: each top field, its
    /// `/Kids` pruned to what leads to one of `widgets`, is added to `fields`, renamed when one
    /// of `names` (those of earlier sources) is its name.
    fn adopt_fields(
        &mut self,
        widgets: &[ObjId],
        fields: &mut Vec<ObjId>,
        names: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        let kept: std::collections::HashSet<ObjId> = widgets.iter().copied().collect();
        let mut tops = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for widget in widgets {
            let mut node = *widget;
            for _ in 0..MAX_NESTING {
                let parent = match self.object(node)? {
                    Some(Object::Dict(dict)) => dict.get(b"Parent").and_then(Object::as_ref_id),
                    _ => None,
                };
                match parent {
                    Some(parent) if matches!(self.object(parent)?, Some(Object::Dict(_))) => {
                        node = parent;
                    }
                    _ => break,
                }
            }
            if seen.insert(node) {
                tops.push(node);
            }
        }
        let earlier = names.clone();
        let mut visited = std::collections::HashSet::new();
        for top in tops {
            if !self.prune(top, &kept, &mut visited, 0)? {
                continue;
            }
            if let Some(Object::Dict(mut field)) = self.object(top)?
                && let Some(Object::String(partial)) = field.get(b"T").cloned()
            {
                let name = decode_text_string(&partial);
                let mut unique = name.clone();
                let mut number = 2;
                while earlier.contains(&unique) {
                    unique = format!("{name} ({number})");
                    number += 1;
                }
                if unique != name {
                    field.insert("T", Object::String(encode_text_string(&unique)));
                    self.set_object(top, Object::Dict(field));
                }
                names.insert(unique);
            }
            fields.push(top);
        }
        Ok(())
    }

    /// Whether field `id` leads to one of `kept` (or is one); its `/Kids` pruned to those that do.
    fn prune(
        &mut self,
        id: ObjId,
        kept: &std::collections::HashSet<ObjId>,
        visited: &mut std::collections::HashSet<ObjId>,
        depth: usize,
    ) -> Result<bool> {
        if depth > MAX_NESTING || !visited.insert(id) {
            return Ok(false);
        }
        let Some(Object::Dict(mut field)) = self.object(id)? else {
            return Ok(false);
        };
        let widget = kept.contains(&id);
        let kids = match field.get(b"Kids").cloned() {
            Some(Object::Array(kids)) => kids,
            Some(Object::Ref(kids)) => match self.object(kids)? {
                Some(Object::Array(kids)) => kids,
                _ => return Ok(widget),
            },
            _ => return Ok(widget),
        };
        let mut leading = Vec::with_capacity(kids.len());
        for kid in &kids {
            if let Some(kid) = kid.as_ref_id()
                && self.prune(kid, kept, visited, depth + 1)?
            {
                leading.push(Object::Ref(kid));
            }
        }
        if leading.is_empty() && !widget {
            return Ok(false);
        }
        if leading.len() != kids.len() {
            if leading.is_empty() {
                field.remove(b"Kids");
            } else {
                field.insert("Kids", Object::Array(leading));
            }
            self.set_object(id, Object::Dict(field));
        }
        Ok(true)
    }
}

/// A page to copy into a new document ([`Document::compose`]): page `page` (from 0) of source
/// `source`, turned `quarter_turns` quarter turns clockwise more than it is (counterclockwise
/// when negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagePick {
    /// The source, by its position among the sources.
    pub source: usize,
    /// The page, from 0.
    pub page: usize,
    /// Quarter turns clockwise added to its rotation.
    pub quarter_turns: i32,
}

impl PagePick {
    /// Page `page` of source `source`, turned `quarter_turns` quarter turns more.
    #[must_use]
    pub fn new(source: usize, page: usize, quarter_turns: i32) -> Self {
        Self {
            source,
            page,
            quarter_turns,
        }
    }
}

/// The keys of a form that say how its fields look, carried along with them.
const FORM_DEFAULTS: [&[u8]; 4] = [b"DA", b"DR", b"Q", b"NeedAppearances"];

/// What [`Document::copy_pages`] copied.
struct Copied {
    /// Each page copied, in the order asked, with its geometry.
    pages: Vec<(ObjId, PageGeometry)>,
    /// The widgets on those pages, copied.
    widgets: Vec<ObjId>,
    /// The defaults of the source's form, copied.
    defaults: Dict,
}

/// The annotations of page `page` of `doc`, by object.
fn annotation_ids(doc: &Document, page: ObjId) -> Result<Vec<ObjId>> {
    let Some(Object::Dict(page)) = doc.object(page)? else {
        return Ok(Vec::new());
    };
    let annotations = match page.get(b"Annots") {
        Some(Object::Array(annotations)) => annotations.clone(),
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Array(annotations)) => annotations,
            _ => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };
    Ok(annotations.iter().filter_map(Object::as_ref_id).collect())
}

/// The form of `doc`'s catalog, if it has one.
fn acro_form(doc: &Document) -> Result<Option<Dict>> {
    let Some(Object::Dict(catalog)) = doc.object(doc.catalog_id())? else {
        return Ok(None);
    };
    Ok(match catalog.get(b"AcroForm") {
        Some(Object::Dict(form)) => Some(form.clone()),
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Dict(form)) => Some(form),
            _ => None,
        },
        _ => None,
    })
}

impl Document {
    /// Add a new page after the others: `page` gets `/Type /Page` and `media_box`, which is its
    /// geometry (no crop box, no rotation). Its contents and resources are the caller's, added
    /// with [`Document::add_object`]. Returns the page object.
    ///
    /// # Errors
    /// As [`Document::add_object`].
    pub fn push_page(&mut self, mut page: Dict, media_box: crate::geometry::Rect) -> Result<ObjId> {
        page.insert("Type", Name::from("Page"));
        page.insert(
            "MediaBox",
            vec![
                media_box.x1.into(),
                media_box.y1.into(),
                media_box.x2.into(),
                media_box.y2.into(),
            ],
        );
        let id = self.add_object(Object::Dict(page))?;
        let geometry = crate::geometry::PageGeometry {
            page_number: self.page_count() + 1,
            media_box,
            crop_box: media_box,
            rotation: crate::geometry::Rotation::None,
            user_unit: 1.0,
        };
        self.appended.push((id, geometry));
        Ok(id)
    }
}

/// `object` with its references to `other`'s objects replaced by references to their copies,
/// copies of the objects not seen yet being made later (`pending`).
fn copy(
    doc: &mut Document,
    object: Object,
    copies: &mut HashMap<ObjId, ObjId>,
    pending: &mut Vec<ObjId>,
    depth: usize,
) -> Result<Object> {
    if depth > MAX_NESTING {
        return Err(Error::LimitExceeded {
            what: "object nesting",
        });
    }
    Ok(match object {
        Object::Ref(id) => {
            let copy = match copies.get(&id) {
                Some(copy) => *copy,
                None => {
                    let copy = doc.add_object(Object::Null)?;
                    copies.insert(id, copy);
                    pending.push(id);
                    copy
                }
            };
            Object::Ref(copy)
        }
        Object::Array(items) => Object::Array(
            items
                .into_iter()
                .map(|item| copy(doc, item, copies, pending, depth + 1))
                .collect::<Result<_>>()?,
        ),
        Object::Dict(dict) => Object::Dict(copy_dict(doc, dict, copies, pending, depth)?),
        Object::Stream(mut stream) => {
            stream.dict = copy_dict(doc, stream.dict, copies, pending, depth)?;
            Object::Stream(stream)
        }
        other => other,
    })
}

fn copy_dict(
    doc: &mut Document,
    dict: Dict,
    copies: &mut HashMap<ObjId, ObjId>,
    pending: &mut Vec<ObjId>,
    depth: usize,
) -> Result<Dict> {
    let mut out = Dict::new();
    for (key, value) in dict.iter() {
        if LEFT_BEHIND.contains(&key.as_bytes()) {
            continue;
        }
        out.insert(
            key.clone(),
            copy(doc, value.clone(), copies, pending, depth + 1)?,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use crate::{Document, Object, OpenOptions, Rect, SaveOptions};

    /// A PDF of `pages` pages, each `width` wide, drawing its number, with a link from the last
    /// page to the first and the page tree giving the resources.
    fn source(pages: u32, width: u32) -> Vec<u8> {
        let mut objects: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R /Outlines << /Count 0 >> >>".to_owned(),
            String::new(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        objects[1] = format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} /MediaBox [0 0 {width} 400] \
             /Resources << /Font << /F1 3 0 R >> >> >>",
            kids.join(" ")
        );
        for i in 0..pages {
            let content = format!("BT /F1 12 Tf 10 10 Td ({i}) Tj ET");
            let annots = if i + 1 == pages && pages > 1 {
                " /Annots [<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>]"
            } else {
                ""
            };
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /Contents {} 0 R /StructParents {i}{annots} >>",
                5 + 2 * i
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn open(data: Vec<u8>) -> Document {
        Document::open(data, &OpenOptions::default()).unwrap()
    }

    #[test]
    fn pages_of_several_documents_make_one() {
        let mut merged = Document::empty().unwrap();
        assert_eq!(merged.page_count(), 0);
        assert!(merged.save(&SaveOptions::default()).is_err());
        assert_eq!(merged.append_pages(&open(source(2, 300))).unwrap(), 2);
        assert_eq!(merged.append_pages(&open(source(3, 500))).unwrap(), 3);
        assert_eq!(merged.page_count(), 5);
        let saved = open(merged.save(&SaveOptions::default()).unwrap());
        assert_eq!(saved.page_count(), 5);
        let widths: Vec<f64> = saved.geometry().iter().map(|g| g.media_box.x2).collect();
        assert_eq!(widths, [300.0, 300.0, 500.0, 500.0, 500.0]);
        assert_eq!(saved.geometry()[4].page_number, 5);
        let ids: Vec<_> = saved.page_ids().into_iter().map(Option::unwrap).collect();
        for (index, id) in ids.iter().enumerate() {
            let Some(Object::Dict(page)) = saved.object(*id).unwrap() else {
                panic!("a page");
            };
            // What the page inherited, it carries; what led into the source's structure, not.
            assert!(page.contains_key(b"Resources"));
            assert!(!page.contains_key(b"StructParents"));
            let Some(Object::Ref(contents)) = page.get(b"Contents").cloned() else {
                panic!("contents");
            };
            let Some(Object::Stream(stream)) = saved.object(contents).unwrap() else {
                panic!("a stream");
            };
            let number = [0, 1, 0, 1, 2][index];
            assert!(String::from_utf8_lossy(&stream.data).contains(&format!("({number}) Tj")));
        }
        // The link of each source's last page leads to that source's first page, copied.
        for (last, first) in [(1, 0), (4, 2)] {
            let Some(Object::Dict(page)) = saved.object(ids[last]).unwrap() else {
                panic!("a page");
            };
            let annots = page.get(b"Annots").unwrap().as_array().unwrap().to_vec();
            let link = match &annots[0] {
                Object::Ref(id) => saved.object(*id).unwrap().unwrap(),
                other => other.clone(),
            };
            let dest = link
                .as_dict()
                .unwrap()
                .get(b"Dest")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(dest[0].as_ref_id(), Some(ids[first]));
        }
        // The source's outlines stay behind.
        let catalog = saved.object(saved.catalog_id()).unwrap().unwrap();
        assert!(!catalog.as_dict().unwrap().contains_key(b"Outlines"));
    }

    #[test]
    fn appended_pages_keep_their_geometry() {
        let mut data = source(1, 300);
        // A crop box and a rotation on the page tree.
        let text = String::from_utf8_lossy(&data).replace(
            "/MediaBox [0 0 300 400]",
            "/MediaBox [0 0 300 400] /CropBox [10 10 290 390] /Rotate 90",
        );
        data = text.into_bytes();
        // The offsets moved: hayro repairs the cross-reference.
        let source = open(data);
        assert_eq!(source.page_count(), 1);
        let mut merged = Document::empty().unwrap();
        merged.append_pages(&source).unwrap();
        let saved = open(merged.save(&SaveOptions::default()).unwrap());
        let geometry = saved.geometry()[0];
        assert_eq!(
            geometry.crop_box,
            Rect::from_corners(10.0, 10.0, 290.0, 390.0)
        );
        assert_eq!(geometry.rotation.degrees(), 90);
        assert_eq!(geometry, source.geometry()[0]);
    }
}
