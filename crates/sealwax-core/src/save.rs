//! Saving a document: in full, or as an incremental update.
//!
//! - **Full:** every object reachable from the trailer (`/Root`, `/Info`), renumbered from 1
//!   in breadth-first order, decrypted, stream data copied as is (still encoded). Unreachable
//!   objects are dropped and references to missing objects become `null`.
//! - **Incremental:** the input bytes untouched, then the modified and added objects, then a
//!   cross-reference section of the same kind as the last one, whose `/Prev` points to it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use sha2::{Digest, Sha256};

use crate::document::{Document, OpenOptions};
use crate::error::{Error, Result};
use crate::limits::Limits;
use crate::object::{Dict, Name, ObjId, Object, Stream, StreamData};
use crate::serialize::{self, Refs, SameRefs};
use crate::trailer::XrefKind;

/// PDF 1.7, the base of PDF/A-2 and PDF/A-3, then a comment of four bytes above 127 so that
/// the file reads as binary (ISO 19005-2, 6.1.2).
const HEADER: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";

/// Objects packed in each object stream (qpdf uses the same).
const OBJECTS_PER_STREAM: usize = 100;

/// How a full save writes its cross-reference information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum XrefFormat {
    /// A compressed cross-reference stream, every object that is not a stream packed into
    /// compressed object streams (PDF 1.5, allowed by PDF/A-2 and PDF/A-3).
    #[default]
    Stream,
    /// A classic cross-reference table and no object stream: readable by every PDF reader.
    Table,
}

/// How to save a document in full.
#[derive(Debug, Clone, Default)]
pub struct SaveOptions {
    xref: XrefFormat,
}

impl SaveOptions {
    /// The cross-reference format. Default: [`XrefFormat::Stream`].
    pub fn xref(mut self, format: XrefFormat) -> Self {
        self.xref = format;
        self
    }
}

/// Where an object of the output is.
#[derive(Debug, Clone, Copy)]
enum Entry {
    /// The head of the free list (object 0).
    Free,
    /// At this offset, with this generation.
    InFile(usize, u16),
    /// Inside an object stream, at this index.
    InStream { stream: u32, index: u32 },
}

/// Old identifier to new one, for the objects a full save writes.
struct Renumbered(HashMap<ObjId, ObjId>);

impl Refs for Renumbered {
    fn map(&self, id: ObjId) -> Option<ObjId> {
        self.0.get(&id).copied()
    }
}

/// What a full save reads: the document, with the page patches over it.
///
/// Each object is read from the document once, then served from memory: in a damaged file,
/// hayro can resolve the same identifier to another object once it has repaired its
/// cross-reference (hayro issue #1746), and the graph walked must be the graph written.
struct Source<'a> {
    doc: &'a Document,
    patches: HashMap<ObjId, Object>,
    read: RefCell<HashMap<ObjId, Option<Object>>>,
}

impl<'a> Source<'a> {
    fn new(doc: &'a Document) -> Self {
        Self {
            doc,
            patches: HashMap::new(),
            read: RefCell::new(HashMap::new()),
        }
    }

    fn load(&self, id: ObjId) -> Result<Option<Object>> {
        if let Some(object) = self.patches.get(&id) {
            return Ok(Some(object.clone()));
        }
        if let Some(object) = self.read.borrow().get(&id) {
            return Ok(object.clone());
        }
        let object = self.doc.load(id, StreamData::Copy)?;
        self.read.borrow_mut().insert(id, object.clone());
        Ok(object)
    }
}

pub(crate) fn full(doc: &Document, options: &SaveOptions) -> Result<Vec<u8>> {
    doc.check_time()?;
    // Readers refuse a file without pages, hayro included: it could not be read back.
    if doc.page_count() == 0 {
        return Err(Error::Unsupported {
            what: "saving a document without pages",
        });
    }
    let mut source = Source::new(doc);
    source.patches = page_patches(&source)?;
    let catalog = doc.catalog_id();
    let roots: Vec<ObjId> = [Some(catalog), doc.info_id()]
        .into_iter()
        .flatten()
        .collect();
    let order = reachable(&source, &roots)?;
    let count = u32::try_from(order.len()).map_err(|_| Error::LimitExceeded {
        what: "object_count",
    })?;
    let refs = Renumbered(
        order
            .iter()
            .zip(1..=count)
            .map(|(old, number)| (*old, ObjId::new(number)))
            .collect(),
    );
    let new_catalog = refs.map(catalog).ok_or(Error::Malformed)?;

    let mut out = HEADER.to_vec();
    let mut entries: Vec<(u32, Entry)> = vec![(0, Entry::Free)];
    let mut packer = Packer::new(count + 1);
    for (old, number) in order.iter().zip(1..=count) {
        if number.is_multiple_of(1024) {
            doc.check_time()?;
        }
        // Every object of `order` was read in the first pass.
        let object = source.load(*old)?.unwrap_or(Object::Null);
        if options.xref == XrefFormat::Stream && !matches!(object, Object::Stream(_)) {
            packer.push(number, &object, &refs, &mut out, &mut entries);
        } else {
            entries.push((number, Entry::InFile(out.len(), 0)));
            serialize::indirect(&mut out, ObjId::new(number), &object, &refs);
        }
    }
    packer.flush(&mut out, &mut entries);

    let second_id = digest(&out);
    let first_id = doc
        .base_trailer()
        .id()
        .map_or_else(|| second_id.clone(), |(first, _)| first);
    let mut trailer = Dict::new();
    trailer.insert("Root", new_catalog);
    if let Some(info) = doc.info_id().and_then(|id| refs.map(id)) {
        trailer.insert("Info", info);
    }
    trailer.insert(
        "ID",
        vec![Object::String(first_id), Object::String(second_id)],
    );
    let (kind, xref_number) = match options.xref {
        XrefFormat::Table => (XrefKind::Table, None),
        XrefFormat::Stream => (XrefKind::Stream, Some(packer.next_number)),
    };
    let size = xref_number.unwrap_or(packer.next_number - 1) + 1;
    trailer.insert("Size", size);
    finish(&mut out, kind, entries, trailer, xref_number);
    check(doc, &out, false)?;
    Ok(out)
}

pub(crate) fn incremental(doc: &Document) -> Result<Vec<u8>> {
    doc.check_time()?;
    let base = doc.base_trailer();
    if base.encrypted() {
        return Err(Error::Unsupported {
            what: "incremental save of an encrypted document",
        });
    }
    let mut out = doc.data().to_vec();
    if doc.overlay().is_empty() && doc.info_id() == base.info() {
        return Ok(out);
    }
    let Some(prev) = base
        .startxref
        .filter(|_| base.is_sound(doc.pdf(), doc.limits()))
    else {
        return Err(Error::Unsupported {
            what: "incremental save of a document whose cross-reference is damaged",
        });
    };
    if !matches!(out.last(), Some(b'\n' | b'\r')) {
        out.push(b'\n');
    }

    let mut entries = Vec::new();
    for (id, object) in doc.overlay() {
        entries.push((id.number, Entry::InFile(out.len(), id.generation)));
        serialize::indirect(&mut out, *id, object, &SameRefs);
    }

    let second_id = digest(&out);
    let first_id = base
        .id()
        .map_or_else(|| second_id.clone(), |(first, _)| first);
    let mut trailer = base.dict.clone();
    // What described the previous section, or its stream, does not carry over.
    for key in [
        &b"Prev"[..],
        b"XRefStm",
        b"Type",
        b"W",
        b"Index",
        b"Filter",
        b"DecodeParms",
        b"Length",
        b"DL",
        b"F",
        b"FFilter",
        b"FDecodeParms",
    ] {
        trailer.remove(key);
    }
    match doc.info_id() {
        Some(info) => trailer.insert("Info", info),
        None => trailer.remove(b"Info"),
    };
    trailer.insert(
        "ID",
        vec![Object::String(first_id), Object::String(second_id)],
    );
    trailer.insert("Prev", i64::try_from(prev).unwrap_or(i64::MAX));

    let highest = entries.iter().map(|(number, _)| *number).max().unwrap_or(0);
    let xref_number = match base.kind {
        XrefKind::Stream => Some(doc.next_free_number()?),
        XrefKind::Table => None,
    };
    let size = [
        base.size().unwrap_or(0),
        highest + 1,
        xref_number.map_or(0, |n| n + 1),
    ]
    .into_iter()
    .max()
    .unwrap_or(1);
    trailer.insert("Size", size);
    finish(&mut out, base.kind, entries, trailer, xref_number);
    check(doc, &out, true)?;
    Ok(out)
}

/// A save is read back before it is returned: it must open and, when nothing was modified, have
/// the input's pages and geometry. On a damaged input a broken output would otherwise go
/// unnoticed until a reader, or a signature, fails on it.
///
/// An incremental update never changes the page count either: the revisions sealwax appends
/// (signatures, validation data, annotations) do not touch the page tree, and DocMDP would not
/// allow it after a certification.
///
/// The document's deadline holds for this reading too, and is checked once more before the save
/// is returned.
fn check(doc: &Document, out: &[u8], incremental: bool) -> Result<()> {
    let options = OpenOptions::default().limits(Limits {
        deadline: doc.limits().deadline,
        ..Limits::default()
    });
    let saved = Document::open(out.to_vec(), &options).map_err(|error| match error {
        Error::Timeout => Error::Timeout,
        _ => Error::SaveCheck {
            what: "the saved document does not open",
        },
    })?;
    let unmodified = doc.overlay().is_empty() && doc.info_id() == doc.base_trailer().info();
    // Pages appended (a merge) must all be there too.
    let appended = !doc.appended.is_empty();
    if (unmodified || incremental || appended) && saved.page_count() != doc.page_count() {
        return Err(Error::SaveCheck { what: "page count" });
    }
    if unmodified && saved.geometry() != doc.geometry() {
        return Err(Error::SaveCheck {
            what: "page geometry",
        });
    }
    doc.check_time()
}

/// What a full save changes in the pages, so that each reads back as hayro read it:
/// - every page carries what it inherited (`/MediaBox`, `/CropBox`, `/Rotate`, `/Resources`), as
///   qpdf does. A damaged file can resolve the same parent differently from one read to the
///   next (a repaired cross-reference, hayro issue #1746); the page no longer depends on it;
/// - when the catalog's `/Pages` does not lead to a page tree, hayro still finds the page
///   objects by scanning the file. Written as is, the output would have no page at all: a new
///   page tree lists the pages found.
fn page_patches(source: &Source<'_>) -> Result<HashMap<ObjId, Object>> {
    let doc = source.doc;
    let mut patches = HashMap::new();
    let geometry = doc.geometry();
    let inherited = doc.inherited_resources()?;
    let page_ids = doc.page_ids();
    let catalog_id = doc.catalog_id();
    // A catalog that is no dictionary (a stream a later revision gave its number, in a fuzzed
    // file) leads to no page, though hayro found them: a catalog of its own is written, over the
    // tree rebuilt, and what the object held is not kept.
    let mut catalog = match source.load(catalog_id)? {
        Some(Object::Dict(catalog)) => catalog,
        _ => {
            let mut catalog = Dict::new();
            catalog.insert("Type", Name::from("Catalog"));
            catalog
        }
    };
    // The tree is kept only when walking it gives back the pages hayro found, in the same order.
    let found: Vec<Option<ObjId>> = match catalog.get(b"Pages").and_then(Object::as_ref_id) {
        Some(root) => tree_leaves(source, root, page_ids.len())?
            .into_iter()
            .map(Some)
            .collect(),
        None => Vec::new(),
    };
    let rebuild = found != page_ids && !page_ids.is_empty() && page_ids.iter().all(Option::is_some);
    let tree_id = if rebuild {
        Some(ObjId::new(doc.next_free_number()?))
    } else {
        None
    };

    let rect = |r: crate::geometry::Rect| -> Object {
        vec![r.x1.into(), r.y1.into(), r.x2.into(), r.y2.into()].into()
    };
    for ((id, geometry), resources) in page_ids.iter().zip(&geometry).zip(inherited) {
        let Some(id) = id else {
            continue;
        };
        let Some(Object::Dict(mut page)) = source.load(*id)? else {
            continue;
        };
        let mut changed = false;
        // A page the caller changed keeps what the caller set; what it inherited and does not
        // set itself is pushed onto it, like on every page. For the others, the dictionary read
        // now can differ from the one measured (a repaired cross-reference): what the document
        // reported wins.
        if doc.overlay().contains_key(id) {
            let degrees = i64::from(geometry.rotation.degrees());
            let inherited: [(&str, Option<Object>); 4] = [
                ("Resources", resources),
                ("MediaBox", Some(rect(geometry.media_box))),
                (
                    "CropBox",
                    (geometry.crop_box != geometry.media_box).then(|| rect(geometry.crop_box)),
                ),
                ("Rotate", (degrees != 0).then_some(Object::Integer(degrees))),
            ];
            for (key, value) in inherited {
                if let Some(value) = value
                    && !page.contains_key(key.as_bytes())
                {
                    page.insert(key, value);
                    changed = true;
                }
            }
            if let Some(tree_id) = tree_id {
                page.insert("Type", Name::from("Page"));
                page.insert("Parent", tree_id);
                changed = true;
            }
            if changed {
                patches.insert(*id, Object::Dict(page));
            }
            continue;
        }
        if page.get(b"MediaBox").and_then(as_rect) != Some(geometry.media_box) {
            page.insert("MediaBox", rect(geometry.media_box));
            changed = true;
        }
        let crop_reads_back = match page.get(b"CropBox").and_then(as_rect) {
            Some(own) => {
                let clipped = own.intersect(&geometry.media_box);
                let visible = clipped.width() > 0.0 && clipped.height() > 0.0;
                (if visible { clipped } else { geometry.media_box }) == geometry.crop_box
            }
            None => geometry.crop_box == geometry.media_box,
        };
        if !crop_reads_back {
            page.insert("CropBox", rect(geometry.crop_box));
            changed = true;
        }
        let degrees = geometry.rotation.degrees();
        let own_degrees = page
            .get(b"Rotate")
            .and_then(Object::as_i64)
            .map_or(0, |r| r.rem_euclid(360));
        if own_degrees != i64::from(degrees) {
            page.insert("Rotate", i64::from(degrees));
            changed = true;
        }
        if let Some(resources) = resources {
            page.insert("Resources", resources);
            changed = true;
        }
        if let Some(tree_id) = tree_id {
            page.insert("Type", Name::from("Page"));
            page.insert("Parent", tree_id);
            changed = true;
        }
        if changed {
            patches.insert(*id, Object::Dict(page));
        }
    }

    if let Some(tree_id) = tree_id {
        let mut tree = Dict::new();
        tree.insert("Type", Name::from("Pages"));
        tree.insert(
            "Kids",
            page_ids
                .iter()
                .flatten()
                .map(|id| Object::Ref(*id))
                .collect::<Vec<_>>(),
        );
        tree.insert("Count", i64::try_from(page_ids.len()).unwrap_or(i64::MAX));
        patches.insert(tree_id, Object::Dict(tree));
        catalog.insert("Pages", tree_id);
        patches.insert(catalog_id, Object::Dict(catalog));
    }
    Ok(patches)
}

/// The leaves of the page tree under `root`, in document order: what a reader would take as
/// the pages. Stops once past `expected` leaves.
fn tree_leaves(source: &Source<'_>, root: ObjId, expected: usize) -> Result<Vec<ObjId>> {
    const MAX_DEPTH: usize = 64;
    let mut leaves = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(root, 0)];
    while let Some((id, depth)) = stack.pop() {
        if depth > MAX_DEPTH || !seen.insert(id) {
            continue;
        }
        let Some(object) = source.load(id)? else {
            continue;
        };
        let Some(dict) = object.as_dict() else {
            continue;
        };
        match dict.get(b"Kids").and_then(Object::as_array) {
            Some(kids) => stack.extend(
                kids.iter()
                    .rev()
                    .filter_map(Object::as_ref_id)
                    .map(|kid| (kid, depth + 1)),
            ),
            None => {
                leaves.push(id);
                if leaves.len() > expected {
                    break;
                }
            }
        }
    }
    Ok(leaves)
}

/// A rectangle as the geometry reads it: normalized, and at the `f32` precision hayro-syntax
/// reads rectangles with.
#[allow(clippy::cast_possible_truncation)]
fn as_rect(object: &Object) -> Option<crate::geometry::Rect> {
    let value = |item: &Object| item.as_f64().map(|v| f64::from(v as f32));
    match object.as_array()? {
        [a, b, c, d] => Some(crate::geometry::Rect::from_corners(
            value(a)?,
            value(b)?,
            value(c)?,
            value(d)?,
        )),
        _ => None,
    }
}

/// The objects reachable from `roots`, breadth first, those that exist only.
fn reachable(source: &Source<'_>, roots: &[ObjId]) -> Result<Vec<ObjId>> {
    let mut order = Vec::new();
    let mut seen: HashSet<ObjId> = roots.iter().copied().collect();
    let mut queue: VecDeque<ObjId> = roots.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        if order.len() % 1024 == 1023 {
            source.doc.check_time()?;
        }
        let Some(object) = source.load(id)? else {
            continue;
        };
        order.push(id);
        visit_refs(&object, &mut |reference| {
            if seen.insert(reference) {
                queue.push_back(reference);
            }
        });
    }
    Ok(order)
}

pub(crate) fn visit_refs(object: &Object, found: &mut impl FnMut(ObjId)) {
    match object {
        Object::Ref(id) => found(*id),
        Object::Array(items) => items.iter().for_each(|item| visit_refs(item, found)),
        Object::Dict(dict) => dict.iter().for_each(|(_, value)| visit_refs(value, found)),
        Object::Stream(stream) => stream
            .dict
            .iter()
            .for_each(|(_, value)| visit_refs(value, found)),
        _ => {}
    }
}

/// Packs objects into object streams of [`OBJECTS_PER_STREAM`], numbered after the objects.
struct Packer {
    next_number: u32,
    pending: Vec<(u32, Vec<u8>)>,
}

impl Packer {
    fn new(next_number: u32) -> Self {
        Self {
            next_number,
            pending: Vec::new(),
        }
    }

    fn push(
        &mut self,
        number: u32,
        object: &Object,
        refs: &impl Refs,
        out: &mut Vec<u8>,
        entries: &mut Vec<(u32, Entry)>,
    ) {
        let mut body = Vec::new();
        serialize::direct(&mut body, object, refs);
        self.pending.push((number, body));
        if self.pending.len() == OBJECTS_PER_STREAM {
            self.flush(out, entries);
        }
    }

    /// Writes the pending objects as one object stream (ISO 32000-1, 7.5.7).
    fn flush(&mut self, out: &mut Vec<u8>, entries: &mut Vec<(u32, Entry)>) {
        if self.pending.is_empty() {
            return;
        }
        let stream_number = self.next_number;
        self.next_number += 1;
        let mut header = Vec::new();
        let mut body = Vec::new();
        for ((number, bytes), index) in self.pending.iter().zip(0..) {
            serialize::push_display(&mut header, format_args!("{number} {} ", body.len()));
            body.extend_from_slice(bytes);
            body.push(b'\n');
            entries.push((
                *number,
                Entry::InStream {
                    stream: stream_number,
                    index,
                },
            ));
        }
        let first = header.len();
        let count = self.pending.len();
        header.extend_from_slice(&body);
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("ObjStm"));
        dict.insert("N", i64::try_from(count).unwrap_or(i64::MAX));
        dict.insert("First", i64::try_from(first).unwrap_or(i64::MAX));
        dict.insert("Filter", Name::from("FlateDecode"));
        let stream = Stream {
            dict,
            data: deflate(&header),
        };
        entries.push((stream_number, Entry::InFile(out.len(), 0)));
        serialize::indirect(
            out,
            ObjId::new(stream_number),
            &Object::Stream(stream),
            &SameRefs,
        );
        self.pending.clear();
    }
}

/// Writes the cross-reference section for `entries`, the trailer, `startxref` and `%%EOF`.
/// For a stream, `xref_number` is the number of the cross-reference stream itself.
fn finish(
    out: &mut Vec<u8>,
    kind: XrefKind,
    mut entries: Vec<(u32, Entry)>,
    mut trailer: Dict,
    xref_number: Option<u32>,
) {
    let xref_offset = out.len();
    match (kind, xref_number) {
        (XrefKind::Stream, Some(number)) => {
            entries.push((number, Entry::InFile(xref_offset, 0)));
            entries.sort_by_key(|(number, _)| *number);
            let widest = entries
                .iter()
                .map(|(_, entry)| match entry {
                    Entry::Free => 0,
                    Entry::InFile(offset, _) => *offset,
                    Entry::InStream { stream, .. } => *stream as usize,
                })
                .max()
                .unwrap_or(0);
            let width = bytes_needed(widest);
            let mut data = Vec::with_capacity(entries.len() * (width + 3));
            for (_, entry) in &entries {
                let (kind, field2, field3) = match *entry {
                    Entry::Free => (0, 0, 0xffff),
                    Entry::InFile(offset, generation) => (1, offset, usize::from(generation)),
                    Entry::InStream { stream, index } => (2, stream as usize, index as usize),
                };
                data.push(kind);
                data.extend_from_slice(&field2.to_be_bytes()[size_of::<usize>() - width..]);
                data.extend_from_slice(&field3.to_be_bytes()[size_of::<usize>() - 2..]);
            }
            trailer.insert("Type", Name::from("XRef"));
            trailer.insert(
                "W",
                vec![
                    Object::Integer(1),
                    Object::Integer(i64::try_from(width).unwrap_or(8)),
                    Object::Integer(2),
                ],
            );
            trailer.insert(
                "Index",
                runs(&entries)
                    .into_iter()
                    .flat_map(|(start, count)| [Object::from(start), Object::from(count)])
                    .collect::<Vec<_>>(),
            );
            trailer.insert("Filter", Name::from("FlateDecode"));
            let stream = Stream {
                dict: trailer,
                data: deflate(&data),
            };
            serialize::indirect(out, ObjId::new(number), &Object::Stream(stream), &SameRefs);
        }
        _ => {
            entries.sort_by_key(|(number, _)| *number);
            out.extend_from_slice(b"xref\n");
            let mut at = 0;
            for (start, count) in runs(&entries) {
                serialize::push_display(out, format_args!("{start} {count}\n"));
                for (_, entry) in &entries[at..at + count as usize] {
                    match entry {
                        Entry::InFile(offset, generation) => serialize::push_display(
                            out,
                            format_args!("{offset:010} {generation:05} n\r\n"),
                        ),
                        // Object streams are only written with a cross-reference stream.
                        Entry::Free | Entry::InStream { .. } => {
                            out.extend_from_slice(b"0000000000 65535 f\r\n");
                        }
                    }
                }
                at += count as usize;
            }
            out.extend_from_slice(b"trailer\n");
            serialize::dict(out, &trailer, &SameRefs);
            out.push(b'\n');
        }
    }
    serialize::push_display(out, format_args!("startxref\n{xref_offset}\n%%EOF\n"));
}

/// Consecutive numbers of `entries` (sorted) as `(first, count)`.
fn runs(entries: &[(u32, Entry)]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (number, _) in entries {
        match runs.last_mut() {
            Some((start, count)) if *start + *count == *number => *count += 1,
            _ => runs.push((*number, 1)),
        }
    }
    runs
}

fn bytes_needed(value: usize) -> usize {
    let bits = usize::BITS - value.leading_zeros();
    (bits as usize).div_ceil(8).max(1)
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::with_capacity(data.len() / 4), Compression::default());
    // Writing into a Vec cannot fail.
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

/// 16 bytes identifying this version of the file: the start of its SHA-256.
fn digest(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data)[..16].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_group_consecutive_numbers() {
        let entries: Vec<(u32, Entry)> = [0, 1, 2, 5, 6, 9]
            .into_iter()
            .map(|n| (n, Entry::Free))
            .collect();
        assert_eq!(runs(&entries), vec![(0, 3), (5, 2), (9, 1)]);
    }

    #[test]
    fn field_width_fits_the_largest_value() {
        assert_eq!(bytes_needed(0), 1);
        assert_eq!(bytes_needed(255), 1);
        assert_eq!(bytes_needed(256), 2);
        assert_eq!(bytes_needed(16_777_216), 4);
    }
}
