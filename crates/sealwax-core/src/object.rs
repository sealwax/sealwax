//! Owned PDF objects: what the modification layer holds and what the writer serializes.
//!
//! hayro-syntax reads objects borrowed from the input bytes, read-only. An object that is
//! modified or added lives here instead, owned, until the document is saved.

use std::collections::HashMap;
use std::fmt;

use hayro_syntax::object::{self as hayro, MaybeRef, ObjRef, ObjectIdentifier};

use crate::error::{Error, Result};

/// How deep arrays and dictionaries may nest inside one object. hayro-syntax bounds parsing
/// already; this keeps the conversion's recursion bounded whatever the source.
const MAX_NESTING: usize = 256;

/// The number and generation of an indirect object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjId {
    /// Object number, from 1.
    pub number: u32,
    /// Generation number: 0 for every object sealwax writes.
    pub generation: u16,
}

impl ObjId {
    /// An object of generation 0.
    pub const fn new(number: u32) -> Self {
        Self {
            number,
            generation: 0,
        }
    }

    pub(crate) fn from_hayro(reference: ObjRef) -> Option<Self> {
        Some(Self {
            number: reference.obj_number,
            generation: u16::try_from(reference.gen_number).ok()?,
        })
    }

    pub(crate) fn to_hayro(self) -> ObjectIdentifier {
        ObjectIdentifier::new(self.number, i32::from(self.generation))
    }
}

impl fmt::Display for ObjId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

/// A name, as its decoded bytes (`/A#20B` is `A B`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(pub Vec<u8>);

impl Name {
    /// The name made of `bytes`.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Its decoded bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", String::from_utf8_lossy(&self.0))
    }
}

impl From<&[u8]> for Name {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl<const N: usize> From<&[u8; N]> for Name {
    fn from(bytes: &[u8; N]) -> Self {
        Self(bytes.to_vec())
    }
}

impl From<&str> for Name {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}

/// Entries from which a dictionary keeps an index of its keys. Below, scanning a few entries is
/// faster than hashing; above, a page holding a million annotations or a resource dictionary
/// naming a million XObjects would make every lookup and insertion a scan.
const INDEXED_FROM: usize = 32;

/// A dictionary. Keys are unique; inserting an existing key replaces its value in place.
#[derive(Clone, Default)]
pub struct Dict {
    entries: Vec<(Name, Object)>,
    /// Where each key is in `entries`, once there are [`INDEXED_FROM`] of them. A dictionary
    /// read from a file with a key twice keeps both entries, and its first is the one found.
    index: Option<HashMap<Vec<u8>, usize>>,
}

impl fmt::Debug for Dict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Dict").field(&self.entries).finish()
    }
}

impl PartialEq for Dict {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Dict {
    /// An empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    fn from_entries(entries: Vec<(Name, Object)>) -> Self {
        let mut dict = Self {
            entries,
            index: None,
        };
        dict.reindex();
        dict
    }

    fn reindex(&mut self) {
        self.index = (self.entries.len() >= INDEXED_FROM).then(|| {
            let mut index = HashMap::with_capacity(self.entries.len());
            for (position, (name, _)) in self.entries.iter().enumerate() {
                index.entry(name.0.clone()).or_insert(position);
            }
            index
        });
    }

    fn position(&self, key: &[u8]) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(key).copied(),
            None => self.entries.iter().position(|(name, _)| name.0 == key),
        }
    }

    /// The value of `key`.
    pub fn get(&self, key: &[u8]) -> Option<&Object> {
        let position = self.position(key)?;
        self.entries.get(position).map(|(_, value)| value)
    }

    /// The value of `key`, to modify it.
    pub fn get_mut(&mut self, key: &[u8]) -> Option<&mut Object> {
        let position = self.position(key)?;
        self.entries.get_mut(position).map(|(_, value)| value)
    }

    /// Whether `key` is present.
    pub fn contains_key(&self, key: &[u8]) -> bool {
        self.position(key).is_some()
    }

    /// Set `key` to `value`, and return the value it replaces.
    pub fn insert(&mut self, key: impl Into<Name>, value: impl Into<Object>) -> Option<Object> {
        let key = key.into();
        let value = value.into();
        if let Some(position) = self.position(&key.0)
            && let Some((_, old)) = self.entries.get_mut(position)
        {
            return Some(std::mem::replace(old, value));
        }
        if let Some(index) = &mut self.index {
            index.insert(key.0.clone(), self.entries.len());
        }
        self.entries.push((key, value));
        if self.index.is_none() && self.entries.len() >= INDEXED_FROM {
            self.reindex();
        }
        None
    }

    /// Remove `key`, and return its value.
    pub fn remove(&mut self, key: &[u8]) -> Option<Object> {
        let position = self.position(key)?;
        let (_, value) = self.entries.remove(position);
        if self.index.is_some() {
            self.reindex();
        }
        Some(value)
    }

    /// The entries, in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.entries.iter().map(|(name, value)| (name, value))
    }

    /// The entries, to modify their values.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Name, &mut Object)> {
        self.entries.iter_mut().map(|(name, value)| (&*name, value))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there is no entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<K: Into<Name>, V: Into<Object>> FromIterator<(K, V)> for Dict {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(entries: I) -> Self {
        let mut dict = Self::new();
        for (key, value) in entries {
            dict.insert(key, value);
        }
        dict
    }
}

/// A stream: its dictionary and its data, still encoded with the filters the dictionary names.
/// `/Length` is written from the data, whatever the dictionary says.
#[derive(Clone, PartialEq)]
pub struct Stream {
    /// The stream dictionary.
    pub dict: Dict,
    /// The encoded data.
    pub data: Vec<u8>,
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("dict", &self.dict)
            .field("data_len", &self.data.len())
            .finish()
    }
}

/// A PDF object.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    /// `null`, also what a reference to a missing object reads as.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer.
    Integer(i64),
    /// A real number.
    Real(f64),
    /// A string, as its bytes (decrypted).
    String(Vec<u8>),
    /// A name.
    Name(Name),
    /// An array.
    Array(Vec<Object>),
    /// A dictionary.
    Dict(Dict),
    /// A stream: only ever an indirect object.
    Stream(Stream),
    /// A reference to an indirect object.
    Ref(ObjId),
}

impl Object {
    /// The dictionary, or the dictionary of the stream.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Self::Dict(dict) => Some(dict),
            Self::Stream(stream) => Some(&stream.dict),
            _ => None,
        }
    }

    /// The dictionary, or the dictionary of the stream, to modify it.
    pub fn as_dict_mut(&mut self) -> Option<&mut Dict> {
        match self {
            Self::Dict(dict) => Some(dict),
            Self::Stream(stream) => Some(&mut stream.dict),
            _ => None,
        }
    }

    /// The array.
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The name's bytes.
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(name) => Some(&name.0),
            _ => None,
        }
    }

    /// The integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// The number, integer or real.
    #[allow(clippy::cast_precision_loss)]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(*value as f64),
            Self::Real(value) => Some(*value),
            _ => None,
        }
    }

    /// The referenced object's identifier.
    pub fn as_ref_id(&self) -> Option<ObjId> {
        match self {
            Self::Ref(id) => Some(*id),
            _ => None,
        }
    }

    /// The string's bytes.
    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            Self::String(bytes) => Some(bytes),
            _ => None,
        }
    }
}

impl From<bool> for Object {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Object {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<i32> for Object {
    fn from(value: i32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<u32> for Object {
    fn from(value: u32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<f64> for Object {
    fn from(value: f64) -> Self {
        Self::Real(value)
    }
}

impl From<Name> for Object {
    fn from(value: Name) -> Self {
        Self::Name(value)
    }
}

impl From<Dict> for Object {
    fn from(value: Dict) -> Self {
        Self::Dict(value)
    }
}

impl From<Stream> for Object {
    fn from(value: Stream) -> Self {
        Self::Stream(value)
    }
}

impl From<ObjId> for Object {
    fn from(value: ObjId) -> Self {
        Self::Ref(value)
    }
}

impl From<Vec<Self>> for Object {
    fn from(value: Vec<Self>) -> Self {
        Self::Array(value)
    }
}

/// Whether stream data is copied when converting a stream read by hayro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamData {
    /// Copy the encoded (decrypted) data.
    Copy,
    /// Leave it empty: only the dictionary and its references are wanted.
    Skip,
}

/// Converts objects read by hayro-syntax into owned ones.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Converter {
    /// The source is encrypted: hayro has decrypted strings and stream data, so a leading
    /// `/Crypt` filter no longer applies.
    pub(crate) decrypted: bool,
    pub(crate) stream_data: StreamData,
}

impl Converter {
    /// An indirect object of the source.
    pub(crate) fn indirect(&self, object: hayro::Object<'_>) -> Result<Object> {
        match object {
            hayro::Object::Stream(stream) => Ok(Object::Stream(self.stream(&stream)?)),
            other => self.direct(other, 0),
        }
    }

    fn maybe_ref(&self, value: MaybeRef<hayro::Object<'_>>, depth: usize) -> Result<Object> {
        match value {
            MaybeRef::Ref(reference) => {
                Ok(ObjId::from_hayro(reference).map_or(Object::Null, Object::Ref))
            }
            MaybeRef::NotRef(object) => self.direct(object, depth),
        }
    }

    fn direct(&self, object: hayro::Object<'_>, depth: usize) -> Result<Object> {
        if depth > MAX_NESTING {
            return Err(Error::LimitExceeded {
                what: "object_nesting",
            });
        }
        Ok(match object {
            hayro::Object::Null(_) => Object::Null,
            hayro::Object::Boolean(value) => Object::Bool(value),
            hayro::Object::Number(number) => number_of(number),
            hayro::Object::String(string) => Object::String(string.as_bytes().to_vec()),
            hayro::Object::Name(name) => Object::Name(Name(name.to_vec())),
            hayro::Object::Array(array) => Object::Array(
                array
                    .raw_iter()
                    .map(|item| self.maybe_ref(item, depth + 1))
                    .collect::<Result<_>>()?,
            ),
            hayro::Object::Dict(dict) => Object::Dict(self.dict(&dict, depth + 1)?),
            // A stream can only be an indirect object; one met inside another object is not
            // valid PDF. It reads as null, as a reference to it would not resolve either.
            hayro::Object::Stream(_) => Object::Null,
        })
    }

    /// A value of the source: a reference stays a reference.
    pub(crate) fn value(&self, value: MaybeRef<hayro::Object<'_>>) -> Result<Object> {
        self.maybe_ref(value, 1)
    }

    /// A dictionary of the source.
    pub(crate) fn dict_of(&self, dict: &hayro::Dict<'_>) -> Result<Dict> {
        self.dict(dict, 1)
    }

    fn dict(&self, dict: &hayro::Dict<'_>, depth: usize) -> Result<Dict> {
        dict.entries()
            .map(|(key, value)| Ok((Name(key.to_vec()), self.maybe_ref(value, depth)?)))
            .collect::<Result<Vec<_>>>()
            .map(Dict::from_entries)
    }

    fn stream(&self, stream: &hayro::Stream<'_>) -> Result<Stream> {
        let mut dict = self.dict(stream.dict(), 1)?;
        dict.remove(b"Length");
        if self.decrypted {
            drop_crypt_filter(&mut dict);
        }
        let data = match self.stream_data {
            StreamData::Copy => stream.raw_data().into_owned(),
            StreamData::Skip => Vec::new(),
        };
        Ok(Stream { dict, data })
    }
}

/// hayro keeps the integer/real distinction private, but prints reals with a decimal point or
/// an exponent and integers without.
fn number_of(number: hayro::Number) -> Object {
    let text = number.to_string();
    if text
        .bytes()
        .any(|b| matches!(b, b'.' | b'e' | b'E' | b'N' | b'i'))
    {
        Object::Real(number.as_f64())
    } else {
        Object::Integer(number.as_i64())
    }
}

/// Once the data is decrypted, a leading `/Crypt` filter (ISO 32000-1, 7.4.10) and its
/// parameters no longer apply.
fn drop_crypt_filter(dict: &mut Dict) {
    let leading_crypt = match dict.get(b"Filter") {
        Some(Object::Name(name)) => name.0 == b"Crypt",
        Some(Object::Array(filters)) => filters.first().and_then(Object::as_name) == Some(b"Crypt"),
        _ => false,
    };
    if !leading_crypt {
        return;
    }
    for key in [&b"Filter"[..], b"DecodeParms", b"DP"] {
        match dict.get_mut(key) {
            Some(Object::Array(items)) if !items.is_empty() => {
                items.remove(0);
                if items.is_empty() {
                    dict.remove(key);
                }
            }
            Some(_) => {
                dict.remove(key);
            }
            None => {}
        }
    }
}
