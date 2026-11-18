//! What goes with a flattening another engine makes (a viewer that paints the annotations and
//! applies the redactions): the page boxes it needs on each page beforehand, the form it leaves
//! behind afterwards, and, when it redacted, a document rebuilt of its pages alone.

use crate::document::Document;
use crate::error::{Error, Result};
use crate::merge::{CopyRules, PagePick};
use crate::object::{Dict, ObjId, Object};

/// How deep the page tree is looked into for what a page inherits.
const MAX_TREE_DEPTH: usize = 64;

/// What a page inherits from its page tree that a flattening engine may not look for there.
const INHERITED: [&[u8]; 3] = [b"MediaBox", b"CropBox", b"Rotate"];

/// The annotation flags that hide an annotation: `Hidden` (2) and `NoView` (32).
const HIDING: i64 = 2 | 32;

impl Document {
    /// Write onto each page the media box, crop box and rotation it inherits from its page tree
    /// and does not hold itself. An engine that does not look for them there (PDFium's
    /// flattening writes US Letter where a page holds no box) then keeps the page as it is: a
    /// Legal page stays Legal. A document whose pages inherit none of them is left as it is.
    /// Returns the pages changed.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] when an object nests deeper than sealwax accepts.
    pub fn own_page_boxes(&mut self) -> Result<usize> {
        let mut changed = 0;
        for id in self.page_ids().into_iter().flatten() {
            self.check_time()?;
            let Some(Object::Dict(mut page)) = self.object(id)? else {
                continue;
            };
            let mut added = false;
            for key in INHERITED {
                if page.contains_key(key) {
                    continue;
                }
                if let Some(value) = self.inherited(&page, key)? {
                    page.insert(key, value);
                    added = true;
                }
            }
            if added {
                self.set_object(id, Object::Dict(page));
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// The value of `key` the nearest ancestor of `page` in the page tree gives, if any.
    fn inherited(&self, page: &Dict, key: &[u8]) -> Result<Option<Object>> {
        let mut parent = page.get(b"Parent").and_then(Object::as_ref_id);
        for _ in 0..MAX_TREE_DEPTH {
            let Some(id) = parent else {
                return Ok(None);
            };
            let Some(Object::Dict(node)) = self.object(id)? else {
                return Ok(None);
            };
            if let Some(value) = node.get(key) {
                return Ok(Some(match value {
                    Object::Ref(id) => self.object(*id)?.unwrap_or(Object::Null),
                    other => other.clone(),
                }));
            }
            parent = node.get(b"Parent").and_then(Object::as_ref_id);
        }
        Ok(None)
    }

    /// After another engine flattened the form, take out what it left: the widgets it did not
    /// paint because they are hidden (`Hidden` or `NoView`), then the form itself (`/AcroForm`,
    /// which still holds the fields' values). A full save then leaves out what nothing reaches
    /// any more. Returns the widgets taken out.
    ///
    /// # Errors
    /// [`Error::NotFlattened`] when a page still holds a visible widget: the flattening did not
    /// happen, and nothing is changed.
    pub fn clear_flattened_form(&mut self) -> Result<usize> {
        let mut pages: Vec<(ObjId, Vec<Object>)> = Vec::new();
        let mut removed = 0;
        for (index, id) in self.page_ids().into_iter().enumerate() {
            self.check_time()?;
            let Some(id) = id else {
                continue;
            };
            let Some((annotations, _)) = self.page_annotations(id)? else {
                continue;
            };
            let mut kept = Vec::with_capacity(annotations.len());
            for annotation in &annotations {
                let Some(dict) = self.annotation(annotation)? else {
                    kept.push(annotation.clone());
                    continue;
                };
                if !is_widget(&dict) {
                    kept.push(annotation.clone());
                } else if hidden(&dict) {
                    removed += 1;
                } else {
                    return Err(Error::NotFlattened { page: index + 1 });
                }
            }
            if kept.len() != annotations.len() {
                pages.push((id, kept));
            }
        }
        for (id, kept) in pages {
            if let Some(Object::Dict(mut page)) = self.object(id)? {
                if kept.is_empty() {
                    page.remove(b"Annots");
                } else {
                    page.insert("Annots", Object::Array(kept));
                }
                self.set_object(id, Object::Dict(page));
            }
        }
        let catalog_id = self.catalog_id();
        if let Some(Object::Dict(mut catalog)) = self.object(catalog_id)?
            && catalog.remove(b"AcroForm").is_some()
        {
            self.set_object(catalog_id, Object::Dict(catalog));
        }
        Ok(removed)
    }

    /// Whether a page of the document holds a redaction mark (a `/Redact` annotation).
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] when an object nests deeper than sealwax accepts.
    pub fn has_redaction_marks(&self) -> Result<bool> {
        for id in self.page_ids().into_iter().flatten() {
            self.check_time()?;
            let Some((annotations, _)) = self.page_annotations(id)? else {
                continue;
            };
            for annotation in &annotations {
                if let Some(dict) = self.annotation(annotation)?
                    && dict.get(b"Subtype").and_then(Object::as_name) == Some(&b"Redact"[..])
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// A new document of this one's pages alone, for a copy whose redactions another engine
    /// applied: what a page may hide the redacted content in goes, and so does everything that
    /// is not a page.
    ///
    /// - Each page keeps only `/Type`, its boxes, `/Rotate`, `/UserUnit`, `/Resources`,
    ///   `/Contents`, `/Annots` and `/Group` (not `/PieceInfo`, which may hold the page as it
    ///   was, nor `/Thumb`, its thumbnail from before, nor `/Metadata`, `/AA`...), what it
    ///   inherits written on it.
    /// - The widgets and the hidden annotations (`Hidden` or `NoView`) are taken out of every
    ///   page, and nothing only they reach is copied.
    /// - The optional content (`/OCProperties`) is kept, so that a layer turned off stays off.
    /// - The rest stays behind: the form, the document information, the XMP metadata, the
    ///   outline, the embedded files, the structure tree (a tagged document is no longer
    ///   tagged), and the streams the pages no longer draw.
    ///
    /// # Errors
    /// As [`Document::compose`].
    pub fn rebuilt_for_redaction(&self) -> Result<Document> {
        let picks: Vec<PagePick> = (0..self.page_count())
            .map(|page| PagePick::new(0, page, 0))
            .collect();
        Self::compose_with(&[self], &picks, &CopyRules::redacted())
    }

    /// The annotations of page `id`, as written in its `/Annots` (references or dictionaries),
    /// and whether `/Annots` is a reference.
    pub(crate) fn page_annotations(&self, id: ObjId) -> Result<Option<(Vec<Object>, bool)>> {
        let Some(Object::Dict(page)) = self.object(id)? else {
            return Ok(None);
        };
        Ok(match page.get(b"Annots") {
            Some(Object::Array(annotations)) => Some((annotations.clone(), false)),
            Some(Object::Ref(array)) => match self.object(*array)? {
                Some(Object::Array(annotations)) => Some((annotations, true)),
                _ => None,
            },
            _ => None,
        })
    }

    /// The dictionary of an annotation as `/Annots` gives it.
    pub(crate) fn annotation(&self, annotation: &Object) -> Result<Option<Dict>> {
        Ok(match annotation {
            Object::Ref(id) => match self.object(*id)? {
                Some(Object::Dict(dict)) => Some(dict),
                _ => None,
            },
            Object::Dict(dict) => Some(dict.clone()),
            _ => None,
        })
    }
}

/// Whether `annotation` is a widget.
pub(crate) fn is_widget(annotation: &Dict) -> bool {
    annotation.get(b"Subtype").and_then(Object::as_name) == Some(&b"Widget"[..])
}

/// Whether `annotation` is hidden (`Hidden` or `NoView`).
pub(crate) fn hidden(annotation: &Dict) -> bool {
    annotation
        .get(b"F")
        .and_then(Object::as_i64)
        .is_some_and(|flags| flags & HIDING != 0)
}
