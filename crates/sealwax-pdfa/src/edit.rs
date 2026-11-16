//! Changing a dictionary where it lives. A dictionary nested directly inside an indirect object
//! (the resources of a page, a font inside them) is changed by rewriting that object.

use std::io::Read as _;

use sealwax_core::{Dict, Document, Name, ObjId, Object, Result};

/// How many references a path may follow: a cycle stops there.
const MAX_HOPS: usize = 64;

/// The largest stream [`stream_data`] decodes itself: font programs and CMaps are far smaller.
const MAX_STREAM: u64 = 64 << 20;

/// One step from a dictionary or an array to one of its values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Step {
    Key(Vec<u8>),
    Index(usize),
}

/// Where a dictionary lives: an indirect object, and the steps leading to it from there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Location {
    pub(crate) owner: ObjId,
    pub(crate) path: Vec<Step>,
}

impl Location {
    /// The indirect object itself.
    pub(crate) fn of(owner: ObjId) -> Self {
        Self {
            owner,
            path: Vec::new(),
        }
    }

    /// The value of `key` in the dictionary here.
    pub(crate) fn key(&self, key: &[u8]) -> Self {
        let mut path = self.path.clone();
        path.push(Step::Key(key.to_vec()));
        Self {
            owner: self.owner,
            path,
        }
    }

    /// The item at `index` of the array here.
    pub(crate) fn index(&self, index: usize) -> Self {
        let mut path = self.path.clone();
        path.push(Step::Index(index));
        Self {
            owner: self.owner,
            path,
        }
    }
}

fn step<'o>(object: &'o Object, step: &Step) -> Option<&'o Object> {
    match step {
        Step::Key(key) => object.as_dict()?.get(key),
        Step::Index(index) => object.as_array()?.get(*index),
    }
}

fn step_mut<'o>(object: &'o mut Object, step: &Step, create: bool) -> Option<&'o mut Object> {
    match step {
        Step::Key(key) => {
            let dict = object.as_dict_mut()?;
            if create && !dict.contains_key(key) {
                dict.insert(Name::new(key.clone()), Dict::new());
            }
            dict.get_mut(key)
        }
        Step::Index(index) => match object {
            Object::Array(items) => items.get_mut(*index),
            _ => None,
        },
    }
}

/// The indirect object holding the dictionary at `location`, and the direct steps from it.
/// References along the path are followed; with `create`, a missing key starts a direct
/// dictionary there.
fn resolve(
    doc: &Document,
    location: &Location,
    create: bool,
) -> Result<Option<(ObjId, Object, Vec<Step>)>> {
    let mut owner = location.owner;
    let mut path: &[Step] = &location.path;
    'hops: for _ in 0..MAX_HOPS {
        let Some(object) = doc.object(owner)? else {
            return Ok(None);
        };
        let mut current = &object;
        for (done, next) in path.iter().enumerate() {
            match step(current, next) {
                Some(Object::Ref(id)) => {
                    owner = *id;
                    path = &path[done + 1..];
                    continue 'hops;
                }
                Some(value) => current = value,
                None if create && matches!(next, Step::Key(_)) && current.as_dict().is_some() => {
                    return Ok(Some((owner, object, path.to_vec())));
                }
                None => return Ok(None),
            }
        }
        return Ok(current
            .as_dict()
            .is_some()
            .then(|| (owner, object, path.to_vec())));
    }
    Ok(None)
}

/// Apply `f` to the dictionary at `location` (a stream's dictionary when it is a stream), and
/// store the object it lives in. With `create`, missing keys along the path become empty
/// dictionaries. Returns whether the dictionary was found.
pub(crate) fn edit_dict(
    doc: &mut Document,
    location: &Location,
    create: bool,
    f: impl FnOnce(&mut Dict),
) -> Result<bool> {
    let Some((owner, mut object, path)) = resolve(doc, location, create)? else {
        return Ok(false);
    };
    let mut current = &mut object;
    for next in &path {
        let Some(value) = step_mut(current, next, create) else {
            return Ok(false);
        };
        current = value;
    }
    let Some(dict) = current.as_dict_mut() else {
        return Ok(false);
    };
    f(dict);
    doc.set_object(owner, object);
    Ok(true)
}

/// The dictionary at `location`, as an owned copy (a stream's dictionary when it is one).
pub(crate) fn read_dict(doc: &Document, location: &Location) -> Result<Option<Dict>> {
    let Some((_, object, path)) = resolve(doc, location, false)? else {
        return Ok(None);
    };
    let mut current = &object;
    for next in &path {
        let Some(value) = step(current, next) else {
            return Ok(None);
        };
        current = value;
    }
    Ok(current.as_dict().cloned())
}

/// The decoded data of a stream of the input.
pub(crate) fn decoded(doc: &Document, id: ObjId) -> Option<Vec<u8>> {
    let identifier =
        hayro_syntax::object::ObjectIdentifier::new(id.number, i32::from(id.generation));
    let stream = doc
        .hayro()
        .xref()
        .get::<hayro_syntax::object::Stream<'_>>(identifier)?;
    // Past the document's bound, a stream reads as one that does not decode.
    doc.limits().check_stream(&stream).ok()?;
    stream.decoded().ok().map(std::borrow::Cow::into_owned)
}

/// Follow `value` if it is a reference.
pub(crate) fn resolved(doc: &Document, value: &Object) -> Result<Option<Object>> {
    match value {
        Object::Ref(id) => doc.object(*id),
        other => Ok(Some(other.clone())),
    }
}

/// The decoded data of a stream: the conversion's version when it changed or added it. With
/// `strict`, a Flate stream that does not decode to its end is refused, rather than read as far
/// as it goes.
pub(crate) fn stream_data(doc: &Document, id: ObjId, strict: bool) -> Option<Vec<u8>> {
    let Ok(Some(Object::Stream(stream))) = doc.object(id) else {
        return None;
    };
    let flate_only = match stream.dict.get(b"Filter") {
        None => return Some(stream.data),
        Some(Object::Name(name)) => name.as_bytes() == b"FlateDecode",
        Some(Object::Array(items)) => {
            matches!(items.as_slice(), [Object::Name(name)] if name.as_bytes() == b"FlateDecode")
        }
        Some(_) => false,
    } && !stream.dict.contains_key(b"DecodeParms");
    if flate_only {
        let mut out = Vec::new();
        let read = flate2::read::ZlibDecoder::new(stream.data.as_slice())
            .take(MAX_STREAM)
            .read_to_end(&mut out);
        if read.is_ok() {
            return Some(out);
        }
        if strict {
            return None;
        }
    }
    decoded(doc, id)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Write as _;

    use sealwax_core::{Document, ObjId, OpenOptions};

    use super::stream_data;

    /// A document whose object 4 is a Flate stream cut off before its end.
    fn document() -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        // Bytes that hardly compress, so that half the stream holds half the data.
        let mut state = 1u32;
        let bytes: Vec<u8> = (0..4000)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                state.to_be_bytes()[0]
            })
            .collect();
        encoder.write_all(&bytes).unwrap();
        let mut data = encoder.finish().unwrap();
        data.truncate(data.len() / 2);
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_vec(),
            [
                format!(
                    "<< /Filter /FlateDecode /Length {} >>\nstream\n",
                    data.len()
                )
                .into_bytes(),
                data,
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        out
    }

    #[test]
    fn a_damaged_stream_is_refused_only_when_asked() {
        let doc = Document::open(document(), &OpenOptions::default()).unwrap();
        let id = ObjId {
            number: 4,
            generation: 0,
        };
        assert!(stream_data(&doc, id, true).is_none());
        // Read as far as it goes otherwise.
        let partial = stream_data(&doc, id, false).unwrap();
        assert!(!partial.is_empty() && partial.len() < 4000);
    }
}
