//! Documents made of the pages of others: an empty document, and pages appended from another,
//! with everything they reach copied and renumbered.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use crate::document::{Document, OpenOptions};
use crate::error::{Error, Result};
use crate::object::{Dict, Name, ObjId, Object};

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
    /// `null` instead.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for pages `other` does not have; as
    /// [`Document::append_pages`].
    pub fn append_page_range(&mut self, other: &Document, pages: Range<usize>) -> Result<usize> {
        let ids = other.page_ids();
        if pages.start > pages.end || pages.end > ids.len() {
            return Err(Error::InvalidArgument { what: "page range" });
        }
        let geometry = other.geometry();
        let inherited = other.inherited_resources()?;
        let mut copies: HashMap<ObjId, ObjId> = HashMap::new();
        let mut pending: Vec<ObjId> = Vec::new();
        let mut left_out = None;
        for (index, id) in ids.iter().enumerate() {
            let id = id.ok_or(Error::Unsupported {
                what: "a page that is not an indirect object",
            })?;
            if let Entry::Vacant(slot) = copies.entry(id) {
                let copy = if pages.contains(&index) {
                    self.add_object(Object::Null)?
                } else if let Some(null) = left_out {
                    null
                } else {
                    let null = self.add_object(Object::Null)?;
                    left_out = Some(null);
                    null
                };
                slot.insert(copy);
            }
        }
        let mut appended = Vec::with_capacity(pages.len());
        for (((index, id), page_geometry), resources) in ids
            .iter()
            .flatten()
            .enumerate()
            .zip(&geometry)
            .zip(inherited)
        {
            if !pages.contains(&index) {
                continue;
            }
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
            page.insert("Type", Name::from("Page"));
            page.insert("MediaBox", rect(page_geometry.media_box));
            page.insert("CropBox", rect(page_geometry.crop_box));
            page.insert("Rotate", i64::from(page_geometry.rotation.degrees()));
            if let Some(resources) = resources
                && !page.contains_key(b"Resources")
            {
                page.insert("Resources", resources);
            }
            let copied = copy(self, Object::Dict(page), &mut copies, &mut pending, 0)?;
            self.set_object(copies[id], copied);
            appended.push((copies[id], *page_geometry));
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
        let count = appended.len();
        self.appended.extend(appended);
        Ok(count)
    }
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
