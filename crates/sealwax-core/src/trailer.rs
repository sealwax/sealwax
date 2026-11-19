//! The trailer of the document as read: hayro-syntax keeps it private, and saving needs it.
//!
//! A full save carries over `/Info` and the first half of `/ID`. An incremental save also
//! needs where the last cross-reference section starts (its `/Prev`) and its kind, since an
//! update is written in the same kind as the section it follows.

use std::collections::HashSet;

use hayro_syntax::Pdf;
use hayro_syntax::object::{Dict as HayroDict, Stream as HayroStream};
use hayro_syntax::reader::{Reader, ReaderContext, ReaderExt};

use crate::limits::Limits;
use crate::object::{Converter, Dict, ObjId, Object, StreamData};

/// How many bytes from the end `startxref` is looked for in.
const STARTXREF_WINDOW: usize = 4096;

/// Entries of one section checked at most before an incremental save; beyond, the section is
/// not trusted.
const MAX_CHECKED_ENTRIES: usize = 4_000_000;

/// Sections of the `/Prev` chain checked at most before an incremental save.
const MAX_SECTIONS: usize = 1_000;

/// The kind of a cross-reference section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum XrefKind {
    /// `xref` table followed by `trailer`.
    Table,
    /// Cross-reference stream (PDF 1.5).
    Stream,
}

/// The trailer of the last cross-reference section of the input.
#[derive(Debug, Clone)]
pub(crate) struct BaseTrailer {
    /// Offset of the last section, when `startxref` points at one that reads and that names the
    /// catalog hayro opened. `None` means the cross-reference had to be repaired. Its entries
    /// are only checked by [`BaseTrailer::is_sound`].
    pub(crate) startxref: Option<usize>,
    pub(crate) kind: XrefKind,
    /// The trailer dictionary (of the cross-reference stream, for that kind). Empty when none
    /// matching the catalog was found.
    pub(crate) dict: Dict,
}

impl BaseTrailer {
    pub(crate) fn read(data: &[u8], root: ObjId) -> Self {
        let names_root = |dict: &Dict| dict.get(b"Root").and_then(Object::as_ref_id) == Some(root);
        if let Some(offset) = startxref(data)
            && let Some((kind, dict)) = section_at(data, offset)
            && names_root(&dict)
        {
            return Self {
                startxref: Some(offset),
                kind,
                dict,
            };
        }
        // Repaired file: the last `trailer` keyword, if it names the same catalog.
        let dict = rfind(data, b"trailer", data.len())
            .and_then(|at| dict_at(data, at + b"trailer".len()))
            .filter(names_root)
            .unwrap_or_default();
        Self {
            startxref: None,
            kind: XrefKind::Table,
            dict,
        }
    }

    pub(crate) fn revisions_after(
        &self,
        data: &[u8],
        end: usize,
        limits: &Limits,
    ) -> crate::Result<Option<usize>> {
        let Some(signed) = data.get(..end).and_then(startxref) else {
            return Ok(None);
        };
        let mut offset = self.startxref;
        let mut seen = HashSet::new();
        while let Some(at) = offset {
            limits.check_time()?;
            if at == signed {
                return Ok(Some(seen.len()));
            }
            if at < end || !seen.insert(at) || seen.len() > MAX_SECTIONS {
                break;
            }
            offset = section_at(data, at)
                .and_then(|(_, dict)| usize::try_from(dict.get(b"Prev")?.as_i64()?).ok());
        }
        Ok(None)
    }

    pub(crate) fn info(&self) -> Option<ObjId> {
        self.dict.get(b"Info").and_then(Object::as_ref_id)
    }

    /// The two halves of `/ID`, when both are strings.
    pub(crate) fn id(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        match self.dict.get(b"ID")?.as_array()? {
            [Object::String(first), Object::String(second), ..] => {
                Some((first.clone(), second.clone()))
            }
            _ => None,
        }
    }

    /// `/Size`: one more than the highest object number of the file.
    pub(crate) fn size(&self) -> Option<u32> {
        self.dict
            .get(b"Size")
            .and_then(Object::as_i64)
            .and_then(|size| u32::try_from(size).ok())
    }

    pub(crate) fn encrypted(&self) -> bool {
        self.dict.contains_key(b"Encrypt")
    }

    /// `/Encrypt`, as written: a reference, most often.
    pub(crate) fn encrypt(&self) -> Option<&Object> {
        self.dict.get(b"Encrypt")
    }

    /// Whether every object the last section says is in use starts where it says. A section
    /// can be found and name the right catalog while its offsets are wrong: hayro then repairs
    /// silently, and an update appended to it would chain to a broken section.
    ///
    /// Every section of the chain is checked, through `/Prev` and `/XRefStm`: an update chains
    /// to all of them, and a reader that has to repair one of them can read the whole file
    /// differently once the update is there.
    pub(crate) fn is_sound(&self, pdf: &Pdf, limits: &Limits) -> bool {
        let data = pdf.data().as_ref();
        let Some(last) = self.startxref else {
            return false;
        };
        let mut pending = vec![last];
        let mut seen = HashSet::new();
        let mut checked = 0;
        while let Some(offset) = pending.pop() {
            if !seen.insert(offset) {
                continue;
            }
            checked += 1;
            if checked > MAX_SECTIONS {
                return false;
            }
            let Some((kind, dict)) = section_at(data, offset) else {
                return false;
            };
            let entries = match kind {
                XrefKind::Table => table_entries(data, offset),
                XrefKind::Stream => stream_entries(pdf, limits, data, offset, &dict),
            };
            let sound = entries.is_some_and(|entries| {
                entries
                    .iter()
                    .all(|&(number, generation, at)| object_at(data, at, number, generation))
            });
            if !sound {
                return false;
            }
            for key in [&b"Prev"[..], b"XRefStm"] {
                match dict.get(key).map(|value| value.as_i64()) {
                    None => {}
                    Some(Some(next)) if usize::try_from(next).is_ok_and(|n| n < data.len()) => {
                        pending.push(usize::try_from(next).unwrap_or(0));
                    }
                    Some(_) => return false,
                }
            }
        }
        true
    }
}

/// `(number, generation, offset)` of the objects in use of the table at `offset`.
fn table_entries(data: &[u8], offset: usize) -> Option<Vec<(u64, u64, usize)>> {
    let mut at = offset;
    skip_white_space(data, &mut at);
    if !data.get(at..)?.starts_with(b"xref") {
        return None;
    }
    at += b"xref".len();
    let mut entries = Vec::new();
    loop {
        skip_white_space(data, &mut at);
        if data.get(at..)?.starts_with(b"trailer") {
            return Some(entries);
        }
        let first = read_unsigned(data, &mut at)?;
        skip_white_space(data, &mut at);
        let count = read_unsigned(data, &mut at)?;
        for number in first..first.checked_add(count)? {
            skip_white_space(data, &mut at);
            let object_offset = read_unsigned(data, &mut at)?;
            skip_white_space(data, &mut at);
            let generation = read_unsigned(data, &mut at)?;
            skip_white_space(data, &mut at);
            match data.get(at)? {
                b'n' => entries.push((number, generation, usize::try_from(object_offset).ok()?)),
                b'f' => {}
                _ => return None,
            }
            at += 1;
            if entries.len() > MAX_CHECKED_ENTRIES {
                return None;
            }
        }
    }
}

/// `(number, generation, offset)` of the objects in use of the cross-reference stream at
/// `offset` (ISO 32000-1, 7.5.8.3).
fn stream_entries(
    pdf: &Pdf,
    limits: &Limits,
    data: &[u8],
    offset: usize,
    dict: &Dict,
) -> Option<Vec<(u64, u64, usize)>> {
    let mut at = offset;
    skip_white_space(data, &mut at);
    let number = u32::try_from(read_unsigned(data, &mut at)?).ok()?;
    skip_white_space(data, &mut at);
    let generation = u16::try_from(read_unsigned(data, &mut at)?).ok()?;
    let id = ObjId { number, generation };
    let stream = pdf.xref().get::<HayroStream<'_>>(id.to_hayro())?;
    // A section too large to decode is one an update cannot chain to.
    limits.check_stream(&stream).ok()?;
    let decoded = stream.decoded_data().ok()?;

    let widths: Vec<usize> = dict
        .get(b"W")?
        .as_array()?
        .iter()
        .map(|w| {
            w.as_i64()
                .and_then(|w| usize::try_from(w).ok())
                .filter(|w| *w <= 8)
        })
        .collect::<Option<_>>()?;
    let [w_type, w_offset, w_generation] = widths[..] else {
        return None;
    };
    let row = w_type + w_offset + w_generation;
    if row == 0 {
        return None;
    }
    let size = u64::from(
        dict.get(b"Size")
            .and_then(Object::as_i64)
            .and_then(|s| u32::try_from(s).ok())?,
    );
    let index: Vec<u64> = match dict.get(b"Index").and_then(Object::as_array) {
        Some(items) => items
            .iter()
            .map(|i| i.as_i64().and_then(|i| u64::try_from(i).ok()))
            .collect::<Option<_>>()?,
        None => vec![0, size],
    };
    let field = |bytes: &[u8]| {
        bytes
            .iter()
            .fold(0_u64, |acc, b| (acc << 8) | u64::from(*b))
    };

    let mut entries = Vec::new();
    let mut rows = decoded.chunks_exact(row);
    for &[first, count] in index.as_chunks::<2>().0 {
        for number in first..first.checked_add(count)? {
            let bytes = rows.next()?;
            let kind = if w_type == 0 {
                1
            } else {
                field(&bytes[..w_type])
            };
            if kind == 1 {
                let object_offset = field(&bytes[w_type..w_type + w_offset]);
                let generation = field(&bytes[w_type + w_offset..]);
                entries.push((number, generation, usize::try_from(object_offset).ok()?));
                if entries.len() > MAX_CHECKED_ENTRIES {
                    return None;
                }
            }
        }
    }
    Some(entries)
}

/// Whether `N G obj` starts at `at`, white space allowed before.
fn object_at(data: &[u8], mut at: usize, number: u64, generation: u64) -> bool {
    skip_white_space(data, &mut at);
    if read_unsigned(data, &mut at) != Some(number) {
        return false;
    }
    skip_white_space(data, &mut at);
    if read_unsigned(data, &mut at) != Some(generation) {
        return false;
    }
    skip_white_space(data, &mut at);
    data.get(at..).is_some_and(|rest| rest.starts_with(b"obj"))
}

fn skip_white_space(data: &[u8], at: &mut usize) {
    while data
        .get(*at)
        .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | b'\0'))
    {
        *at += 1;
    }
}

fn read_unsigned(data: &[u8], at: &mut usize) -> Option<u64> {
    let start = *at;
    let mut value: u64 = 0;
    while let Some(digit) = data.get(*at).filter(|b| b.is_ascii_digit()) {
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(digit - b'0'))?;
        *at += 1;
    }
    (*at > start).then_some(value)
}

/// The offset `startxref` gives, if it is inside the file.
fn startxref(data: &[u8]) -> Option<usize> {
    let at = rfind(data, b"startxref", STARTXREF_WINDOW)?;
    let mut reader = Reader::new(data.get(at + b"startxref".len()..)?);
    reader.skip_white_spaces_and_comments();
    let offset = usize::try_from(reader.read_without_context::<i64>()?).ok()?;
    (offset < data.len()).then_some(offset)
}

/// The cross-reference section at `offset`: a table and its trailer, or a stream's dictionary.
fn section_at(data: &[u8], offset: usize) -> Option<(XrefKind, Dict)> {
    let mut reader = Reader::new(data.get(offset..)?);
    reader.skip_white_spaces_and_comments();
    let start = offset + reader.offset();
    if data.get(start..)?.starts_with(b"xref") {
        let trailer = find(data, b"trailer", start)?;
        return Some((XrefKind::Table, dict_at(data, trailer + b"trailer".len())?));
    }
    // `N G obj << /Type /XRef ... >> stream`
    let header = data.get(..data.len().min(start + 64))?;
    let obj = find(header, b"obj", start)?;
    let dict = dict_at(data, obj + b"obj".len())?;
    (dict.get(b"Type").and_then(Object::as_name) == Some(b"XRef"))
        .then_some((XrefKind::Stream, dict))
}

/// The dictionary that starts at `offset`, after white space. References are kept as such.
fn dict_at(data: &[u8], offset: usize) -> Option<Dict> {
    let mut reader = Reader::new(data.get(offset..)?);
    reader.skip_white_spaces_and_comments();
    let dict = reader.read_with_context::<HayroDict<'_>>(&ReaderContext::dummy())?;
    let converter = Converter {
        decrypted: false,
        stream_data: StreamData::Skip,
    };
    converter.dict_of(&dict).ok()
}

/// Last occurrence of `needle` within the `window` last bytes.
fn rfind(data: &[u8], needle: &[u8], window: usize) -> Option<usize> {
    let start = data.len().saturating_sub(window);
    data.get(start..)?
        .windows(needle.len())
        .rposition(|w| w == needle)
        .map(|at| start + at)
}

/// First occurrence of `needle` at or after `from`.
fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| from + at)
}
