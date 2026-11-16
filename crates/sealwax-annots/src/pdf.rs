//! Reading and writing the objects this crate touches.

use std::collections::HashMap;
use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use sealwax_core::{Dict, Document, ObjId, Object, Rect, Result, Stream};

/// How many parents a field, or a page, may have: deeper is a loop.
pub(crate) const MAX_DEPTH: usize = 64;

/// `value`, followed if it is a reference.
pub(crate) fn resolved(doc: &Document, value: &Object) -> Result<Option<Object>> {
    match value {
        Object::Ref(id) => doc.object(*id),
        other => Ok(Some(other.clone())),
    }
}

/// The dictionary `value` is or refers to (a stream's dictionary for a stream).
pub(crate) fn dict(doc: &Document, value: Option<&Object>) -> Result<Option<Dict>> {
    let Some(value) = value else {
        return Ok(None);
    };
    Ok(match resolved(doc, value)? {
        Some(Object::Dict(dict)) => Some(dict),
        Some(Object::Stream(stream)) => Some(stream.dict),
        _ => None,
    })
}

/// `key` in `node` or in the nearest of its parents (`/Parent` chain) that has it, resolved.
pub(crate) fn inherited(doc: &Document, node: &Dict, key: &[u8]) -> Result<Option<Object>> {
    let mut current = node.clone();
    for _ in 0..MAX_DEPTH {
        if let Some(value) = current.get(key) {
            return resolved(doc, value);
        }
        match dict(doc, current.get(b"Parent"))? {
            Some(parent) => current = parent,
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// What the nodes of the page tree hand down to their pages, read once per node and key: a node
/// with many kids is a large dictionary, converted again for each of them otherwise. Pages may
/// change while it is in use, the nodes above them may not.
#[derive(Debug, Default)]
pub(crate) struct HandedDown(HashMap<(ObjId, &'static [u8]), Option<Object>>);

impl HandedDown {
    /// `key` in `node` or in the nearest of its parents (`/Parent` chain) that has it, resolved.
    pub(crate) fn get(
        &mut self,
        doc: &Document,
        node: &Dict,
        key: &'static [u8],
    ) -> Result<Option<Object>> {
        if let Some(value) = node.get(key) {
            return resolved(doc, value);
        }
        // The parents not known yet, nearest first.
        let mut unknown = Vec::new();
        let mut found = None;
        let mut parent = node.get(b"Parent").cloned();
        for _ in 1..MAX_DEPTH {
            let Some(reference) = parent else { break };
            if let Object::Ref(id) = reference
                && let Some(known) = self.0.get(&(id, key))
            {
                found = known.clone();
                break;
            }
            let Some(dict) = dict(doc, Some(&reference))? else {
                break;
            };
            if let Object::Ref(id) = reference {
                unknown.push(id);
            }
            if let Some(value) = dict.get(key) {
                found = resolved(doc, value)?;
                break;
            }
            parent = dict.get(b"Parent").cloned();
        }
        for id in unknown {
            self.0.insert((id, key), found.clone());
        }
        Ok(found)
    }
}

/// A stream compressed with `FlateDecode`.
pub(crate) fn flate(mut dict: Dict, data: &[u8]) -> Object {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    // Writing into a Vec cannot fail.
    let compressed = encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    if compressed.is_empty() && !data.is_empty() {
        return Object::Stream(Stream {
            dict,
            data: data.to_vec(),
        });
    }
    dict.insert("Filter", sealwax_core::Name::from("FlateDecode"));
    Object::Stream(Stream {
        dict,
        data: compressed,
    })
}

/// An array of numbers.
pub(crate) fn numbers(values: &[f64]) -> Object {
    Object::Array(values.iter().map(|v| Object::Real(*v)).collect())
}

/// A rectangle given as an array of four finite numbers, normalized.
pub(crate) fn rect(value: Option<&Object>) -> Option<Rect> {
    let values = value?.as_array()?;
    let [x1, y1, x2, y2] = values else {
        return None;
    };
    let read = |v: &Object| v.as_f64().filter(|v| v.is_finite());
    Some(Rect::from_corners(
        read(x1)?,
        read(y1)?,
        read(x2)?,
        read(y2)?,
    ))
}

/// The numbers of an array, `None` when one is not a finite number.
pub(crate) fn number_array(value: &Object) -> Option<Vec<f64>> {
    value
        .as_array()?
        .iter()
        .map(|v| v.as_f64().filter(|v| v.is_finite()))
        .collect()
}
