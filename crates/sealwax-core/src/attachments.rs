//! The files a document embeds: listed (its `/EmbeddedFiles` and its file attachment
//! annotations), extracted, and added.

use std::collections::HashSet;
use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use md5::{Digest as _, Md5};

use crate::document::Document;
use crate::error::{Error, Result};
use crate::geometry::Rect;
use crate::object::{Dict, Name, ObjId, Object, Stream};
use crate::text::{decode_text_string, encode_text_string};

/// How deep the name tree is followed, and how many of its nodes are read.
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_NODES: usize = 100_000;
/// The longest name a file added may have, in characters.
const MAX_NAME: usize = 1024;

/// MIME types by file extension, lower case.
const MIME_TYPES: [(&str, &str); 18] = [
    ("csv", "text/csv"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("gif", "image/gif"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("json", "application/json"),
    ("p7s", "application/pkcs7-signature"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("txt", "text/plain"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xml", "text/xml"),
    ("zip", "application/zip"),
];

/// The relationships a file may have with the document (`/AFRelationship`, ISO 32000-2, 14.13).
const RELATIONSHIPS: [&str; 8] = [
    "Source",
    "Data",
    "Alternative",
    "Supplement",
    "EncryptedPayload",
    "FormData",
    "Schema",
    "Unspecified",
];

/// The MIME type of a file named `name`, by its extension; `application/octet-stream` when the
/// extension says nothing.
pub fn mime_type_for(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    extension
        .and_then(|extension| {
            MIME_TYPES
                .iter()
                .find(|(known, _)| *known == extension)
                .map(|(_, mime)| *mime)
        })
        .unwrap_or("application/octet-stream")
}

/// Whether `name` is a MIME type, a type and a subtype without parameters (`text/plain`).
pub fn is_mime_type(name: &[u8]) -> bool {
    let part = |part: &[u8]| {
        !part.is_empty()
            && part
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"-_+.".contains(b))
    };
    let mut parts = name.splitn(2, |b| *b == b'/');
    matches!((parts.next(), parts.next()), (Some(kind), Some(sub)) if part(kind) && part(sub))
}

/// A file a document embeds.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Attachment {
    /// Its key in the document's `/EmbeddedFiles`; `None` for a file attachment annotation's.
    pub key: Option<String>,
    /// Its file name (`/UF`, else `/F`).
    pub name: String,
    pub description: Option<String>,
    /// Its MIME type (`/Subtype` of its stream), when it gives one.
    pub mime_type: Option<String>,
    /// Its size as declared (`/Params /Size`).
    pub size: Option<u64>,
    /// Its dates as written (`/Params /CreationDate`, `/ModDate`: `D:20261118120000Z`).
    pub created: Option<String>,
    pub modified: Option<String>,
    /// How it relates to the document (`/AFRelationship`: `Source`, `Data`, `Alternative`...).
    pub relationship: Option<String>,
    /// For a file attachment annotation: its page (from 0) and its rectangle there.
    pub page_index: Option<usize>,
    pub rect: Option<Rect>,
}

/// A file to embed ([`Document::add_attachment`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NewAttachment {
    /// Its file name, also its key in `/EmbeddedFiles`: not empty, not taken.
    pub name: String,
    pub data: Vec<u8>,
    pub description: Option<String>,
    /// Its MIME type (`text/xml`); guessed from the name's extension when `None`.
    pub mime_type: Option<String>,
    /// How it relates to the document: `Source`, `Data`, `Alternative`, `Supplement`,
    /// `EncryptedPayload`, `FormData`, `Schema`, or `Unspecified` when `None`.
    pub relationship: Option<String>,
    /// When it last changed, as a PDF date (`D:20261118120000Z`), which PDF/A-3 asks for.
    pub modified: Option<String>,
}

impl NewAttachment {
    pub fn new(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            data: data.into(),
            description: None,
            mime_type: None,
            relationship: None,
            modified: None,
        }
    }
}

/// A file specification found, and where.
struct Found {
    key: Option<String>,
    spec: Dict,
    page_index: Option<usize>,
    rect: Option<Rect>,
}

impl Document {
    /// The files the document embeds: those of its `/EmbeddedFiles` in the order of their keys,
    /// then those its pages' file attachment annotations embed, page by page. A file
    /// specification that embeds no file (a reference to a file outside) is left out.
    ///
    /// # Errors
    /// [`Error::Timeout`] past the deadline; as [`Document::save`] for a document modified since
    /// it was opened, read as it stands.
    pub fn attachments(&self) -> Result<Vec<Attachment>> {
        let view = self.reopened_if_modified()?;
        let doc = view.as_deref().unwrap_or(self);
        doc.found_attachments()?
            .into_iter()
            .map(|found| doc.described(found))
            .collect::<Result<Vec<_>>>()
            .map(|all| all.into_iter().flatten().collect())
    }

    /// The bytes of the `index`-th file (from 0) of [`Document::attachments`], decoded.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a file the document does not embed;
    /// [`Error::LimitExceeded`] past [`crate::Limits::max_stream_bytes`]; [`Error::Malformed`]
    /// for a stream that cannot be decoded; as [`Document::attachments`].
    pub fn attachment_data(&self, index: usize) -> Result<Vec<u8>> {
        let view = self.reopened_if_modified()?;
        let doc = view.as_deref().unwrap_or(self);
        let stream = doc
            .found_attachments()?
            .into_iter()
            .filter_map(|found| doc.embedded_stream(&found.spec).transpose())
            .nth(index)
            .transpose()?
            .ok_or(Error::InvalidArgument {
                what: "attachment index",
            })?;
        let Some(stream) = doc
            .hayro()
            .xref()
            .get::<hayro_syntax::object::Stream<'_>>(stream.to_hayro())
        else {
            return Err(Error::Malformed);
        };
        doc.limits().check_stream(&stream)?;
        stream
            .decoded()
            .map(|data| data.into_owned())
            .map_err(|_| Error::Malformed)
    }

    /// Embed `file` in the document: an embedded file stream (compressed, its MIME type, size,
    /// date and MD5 checksum in `/Params`), a file specification naming it in `/F` and `/UF`,
    /// with its description and relationship, added to `/EmbeddedFiles` (rewritten as one sorted
    /// node) and to the catalog's `/AF`, as PDF/A-3 associates a document's files.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a name empty, too long or taken, a MIME type or a
    /// relationship it cannot take; [`Error::LimitExceeded`] past
    /// [`crate::Limits::max_stream_bytes`] or when a new object cannot be numbered.
    pub fn add_attachment(&mut self, file: &NewAttachment) -> Result<()> {
        if file.name.is_empty() || file.name.chars().count() > MAX_NAME {
            return Err(Error::InvalidArgument {
                what: "attachment name",
            });
        }
        let mime = file
            .mime_type
            .clone()
            .unwrap_or_else(|| mime_type_for(&file.name).to_owned());
        if !is_mime_type(mime.as_bytes()) {
            return Err(Error::InvalidArgument {
                what: "attachment MIME type",
            });
        }
        let relationship = file.relationship.as_deref().unwrap_or("Unspecified");
        if !RELATIONSHIPS.contains(&relationship) {
            return Err(Error::InvalidArgument {
                what: "attachment relationship",
            });
        }
        if u64::try_from(file.data.len()).unwrap_or(u64::MAX) > self.limits().max_stream_bytes {
            return Err(Error::LimitExceeded {
                what: "max_stream_bytes",
            });
        }
        // The name tree as it stands, its entries in order.
        let catalog_id = self.catalog_id();
        let catalog = self.dict_at(catalog_id)?.unwrap_or_default();
        let names = self
            .resolved_dict(catalog.get(b"Names"))?
            .unwrap_or_default();
        let mut entries = match self.resolved_dict(names.get(b"EmbeddedFiles"))? {
            Some(root) => self.name_tree_entries(root)?,
            None => Vec::new(),
        };
        if entries
            .iter()
            .any(|(key, _)| decode_text_string(key) == file.name)
        {
            return Err(Error::InvalidArgument {
                what: "attachment name",
            });
        }

        let mut params = Dict::new();
        params.insert("Size", i64::try_from(file.data.len()).unwrap_or(i64::MAX));
        if let Some(modified) = &file.modified {
            params.insert("ModDate", Object::String(modified.clone().into_bytes()));
        }
        params.insert("CheckSum", Object::String(Md5::digest(&file.data).to_vec()));
        let mut stream = Dict::new();
        stream.insert("Type", Name::from("EmbeddedFile"));
        stream.insert("Subtype", Name::new(mime.into_bytes()));
        stream.insert("Params", params);
        stream.insert("Filter", Name::from("FlateDecode"));
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        let _ = encoder.write_all(&file.data);
        let data = encoder.finish().unwrap_or_default();
        let stream_id = self.add_object(Object::Stream(Stream { dict: stream, data }))?;

        let name = Object::String(encode_text_string(&file.name));
        let mut spec = Dict::new();
        spec.insert("Type", Name::from("Filespec"));
        spec.insert("F", name.clone());
        spec.insert("UF", name.clone());
        let mut embedded = Dict::new();
        embedded.insert("F", stream_id);
        embedded.insert("UF", stream_id);
        spec.insert("EF", embedded);
        if let Some(description) = &file.description {
            spec.insert("Desc", Object::String(encode_text_string(description)));
        }
        spec.insert(
            "AFRelationship",
            Name::new(relationship.as_bytes().to_vec()),
        );
        let spec_id = self.add_object(Object::Dict(spec))?;

        // `/EmbeddedFiles` rewritten as one node, its keys sorted (ISO 32000-1, 7.9.6).
        let Object::String(key) = name else {
            return Err(Error::Malformed);
        };
        entries.push((key, Object::Ref(spec_id)));
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut tree = Dict::new();
        tree.insert(
            "Names",
            entries
                .into_iter()
                .flat_map(|(key, value)| [Object::String(key), value])
                .collect::<Vec<_>>(),
        );
        let tree_id = self.add_object(Object::Dict(tree))?;
        let mut names = names;
        names.insert("EmbeddedFiles", tree_id);
        let mut catalog = self.dict_at(catalog_id)?.unwrap_or_default();
        match catalog.get(b"Names") {
            Some(Object::Ref(id)) => {
                let id = *id;
                self.set_object(id, Object::Dict(names));
            }
            _ => {
                catalog.insert("Names", names);
            }
        }
        match catalog.get(b"AF") {
            Some(Object::Ref(id)) => {
                let id = *id;
                let mut files = match self.object(id)? {
                    Some(Object::Array(files)) => files,
                    _ => Vec::new(),
                };
                files.push(Object::Ref(spec_id));
                self.set_object(id, Object::Array(files));
            }
            other => {
                let mut files = match other {
                    Some(Object::Array(files)) => files.clone(),
                    _ => Vec::new(),
                };
                files.push(Object::Ref(spec_id));
                catalog.insert("AF", files);
            }
        }
        self.set_object(catalog_id, Object::Dict(catalog));
        Ok(())
    }

    /// The file specifications of `/EmbeddedFiles`, then of the pages' file attachment
    /// annotations.
    fn found_attachments(&self) -> Result<Vec<Found>> {
        let mut found = Vec::new();
        let catalog = self.dict_at(self.catalog_id())?.unwrap_or_default();
        let names = self
            .resolved_dict(catalog.get(b"Names"))?
            .unwrap_or_default();
        if let Some(root) = self.resolved_dict(names.get(b"EmbeddedFiles"))? {
            let mut entries = self.name_tree_entries(root)?;
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (key, value) in entries {
                if let Some(spec) = self.resolved_dict(Some(&value))? {
                    found.push(Found {
                        key: Some(decode_text_string(&key)),
                        spec,
                        page_index: None,
                        rect: None,
                    });
                }
            }
        }
        let max = usize::try_from(self.limits().max_annotations).unwrap_or(usize::MAX);
        for (page_index, page) in self.page_ids().into_iter().enumerate() {
            self.check_time()?;
            let Some((annotations, _)) = page
                .map(|id| self.page_annotations(id))
                .transpose()?
                .flatten()
            else {
                continue;
            };
            for annotation in annotations.iter().take(max) {
                let Some(annotation) = self.annotation(annotation)? else {
                    continue;
                };
                if annotation.get(b"Subtype").and_then(Object::as_name) != Some(b"FileAttachment") {
                    continue;
                }
                if let Some(spec) = self.resolved_dict(annotation.get(b"FS"))? {
                    found.push(Found {
                        key: None,
                        spec,
                        page_index: Some(page_index),
                        rect: self.rect(annotation.get(b"Rect"))?,
                    });
                }
            }
        }
        Ok(found)
    }

    /// The embedded file stream of the file specification `spec` (`/EF /UF`, else `/EF /F`).
    fn embedded_stream(&self, spec: &Dict) -> Result<Option<ObjId>> {
        let Some(embedded) = self.resolved_dict(spec.get(b"EF"))? else {
            return Ok(None);
        };
        Ok([&b"UF"[..], b"F"]
            .iter()
            .find_map(|key| embedded.get(key).and_then(Object::as_ref_id)))
    }

    /// What the file specification `found` says of its file; `None` when it embeds none.
    fn described(&self, found: Found) -> Result<Option<Attachment>> {
        let Some(stream_id) = self.embedded_stream(&found.spec)? else {
            return Ok(None);
        };
        let text = |object: Option<Object>| match object {
            Some(Object::String(bytes)) => Some(decode_text_string(&bytes)),
            _ => None,
        };
        let mut name = None;
        for key in [&b"UF"[..], b"F", b"Unix", b"DOS", b"Mac"] {
            if let Some(found) = text(self.resolved(found.spec.get(key))?) {
                name = Some(found);
                break;
            }
        }
        let stream = match self.object_without_data(stream_id)? {
            Some(Object::Stream(stream)) => stream.dict,
            Some(Object::Dict(dict)) => dict,
            _ => Dict::new(),
        };
        let params = self
            .resolved_dict(stream.get(b"Params"))?
            .unwrap_or_default();
        Ok(Some(Attachment {
            name: name.or_else(|| found.key.clone()).unwrap_or_default(),
            key: found.key,
            description: text(self.resolved(found.spec.get(b"Desc"))?),
            mime_type: stream
                .get(b"Subtype")
                .and_then(Object::as_name)
                .map(|mime| String::from_utf8_lossy(mime).into_owned()),
            size: params
                .get(b"Size")
                .and_then(Object::as_i64)
                .and_then(|size| u64::try_from(size).ok()),
            created: text(self.resolved(params.get(b"CreationDate"))?),
            modified: text(self.resolved(params.get(b"ModDate"))?),
            relationship: found
                .spec
                .get(b"AFRelationship")
                .and_then(Object::as_name)
                .map(|name| String::from_utf8_lossy(name).into_owned()),
            page_index: found.page_index,
            rect: found.rect,
        }))
    }

    /// The entries of the name tree `root`: each key (as written) and its value.
    fn name_tree_entries(&self, root: Dict) -> Result<Vec<(Vec<u8>, Object)>> {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(root, 0)];
        let mut nodes = 0;
        while let Some((node, depth)) = stack.pop() {
            nodes += 1;
            if nodes > MAX_TREE_NODES {
                break;
            }
            if nodes % 1024 == 0 {
                self.check_time()?;
            }
            if let Some(Object::Array(names)) = self.resolved(node.get(b"Names"))? {
                for [key, value] in names.as_chunks::<2>().0 {
                    if let Some(key) = self
                        .resolved(Some(key))?
                        .and_then(|k| k.as_string().map(<[u8]>::to_vec))
                    {
                        entries.push((key, value.clone()));
                    }
                }
            }
            if depth >= MAX_TREE_DEPTH {
                continue;
            }
            if let Some(Object::Array(kids)) = self.resolved(node.get(b"Kids"))? {
                for kid in kids.iter().rev() {
                    if let Some(id) = kid.as_ref_id()
                        && !seen.insert(id)
                    {
                        continue;
                    }
                    if let Some(kid) = self.resolved_dict(Some(kid))? {
                        stack.push((kid, depth + 1));
                    }
                }
            }
        }
        Ok(entries)
    }
}
