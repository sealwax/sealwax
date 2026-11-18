//! What a document says about itself: its version, whether it is encrypted or linearized, its
//! form and signature fields, its PDF/A claim, its document information. Read without failing:
//! what cannot be read is left out, or reads as absent.

use std::collections::BTreeMap;

use hayro_syntax::object::{Dict as HayroDict, Stream as HayroStream};

use crate::census::{form, pdfa_claim};
use crate::document::Document;
use crate::object::{Dict, Object};
use crate::text::decode_text_string;

/// How far into the file the linearization dictionary is looked for: it is the first object.
const LINEARIZATION_WINDOW: usize = 1024;

/// What a document says about itself. A description, never a verification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Metadata {
    /// The version it declares: its header's, or its catalog's when later ("1.7", "2.0").
    pub version: String,
    /// Encrypted (its trailer names an `/Encrypt` dictionary), whether or not it needs a
    /// password to open.
    pub encrypted: bool,
    /// How, when it is and its `/Encrypt` dictionary reads.
    pub encryption: Option<Encryption>,
    /// Linearized for fast web view: its first object is a linearization dictionary.
    pub linearized: bool,
    /// Its interactive form has at least one field.
    pub has_forms: bool,
    /// Its form has at least one signature field, signed or not.
    pub has_signatures: bool,
    /// The PDF/A part and conformance its XMP metadata claims (`pdfaid:part`, then
    /// `pdfaid:conformance`): "3A", "2B", "1" when the conformance is missing.
    pub pdfa: Option<String>,
    /// The document information dictionary's text entries (`Title`, `Author`, `Subject`,
    /// `Keywords`, `Creator`, `Producer`, `CreationDate`, `ModDate`, others), decoded; dates as
    /// written (`D:20240131120000+01'00'`).
    pub info: BTreeMap<String, String>,
}

/// How a document is encrypted, as its `/Encrypt` dictionary says.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Encryption {
    /// The security handler (`/Filter`): `Standard` for passwords.
    pub filter: String,
    /// `/V`, the algorithm.
    pub version: i64,
    /// `/R`, the revision of the standard security handler.
    pub revision: i64,
    /// The length of the key, in bits.
    pub key_bits: u32,
    /// How the streams are encrypted: `RC4`, `AES` (128-bit `AESV2` or 256-bit `AESV3`),
    /// `None` (`Identity`, or a crypt filter that does not encrypt), `Unknown`.
    pub method: String,
}

impl Encryption {
    /// As PyMuPDF describes it: `Standard V5 R6 256-bit AES`.
    #[must_use]
    pub fn description(&self) -> String {
        format!(
            "{} V{} R{} {}-bit {}",
            self.filter, self.version, self.revision, self.key_bits, self.method
        )
    }
}

/// The encryption `/Encrypt` dictionary `encrypt` describes, `resolve` reading what it refers
/// to. As PyMuPDF describes it: from 1.27.2, on documents it encrypted, and some it encrypted
/// then changed.
fn encryption(encrypt: &Dict, resolve: &dyn Fn(&Object) -> Option<Object>) -> Encryption {
    let name = |dict: &Dict, key: &[u8]| {
        dict.get(key).and_then(resolve).and_then(|value| {
            value
                .as_name()
                .map(|name| String::from_utf8_lossy(name).into_owned())
        })
    };
    let integer = |dict: &Dict, key: &[u8]| {
        dict.get(key)
            .and_then(resolve)
            .and_then(|value| value.as_i64())
    };
    let version = integer(encrypt, b"V").unwrap_or(0);
    // From version 4, streams are encrypted by the crypt filter `/StmF` names, in `/CF`.
    let (method, filter_length) = if version >= 4 {
        match name(encrypt, b"StmF").as_deref() {
            None | Some("Identity") => ("None", None),
            Some(stream_filter) => {
                let crypt = encrypt.get(b"CF").and_then(resolve).and_then(|filters| {
                    filters
                        .as_dict()
                        .and_then(|filters| filters.get(stream_filter.as_bytes()))
                        .and_then(resolve)
                });
                let crypt = crypt.as_ref().and_then(Object::as_dict);
                let method = match crypt.and_then(|crypt| name(crypt, b"CFM")).as_deref() {
                    Some("V2") => "RC4",
                    Some("AESV2") => "AESV2",
                    Some("AESV3") => "AESV3",
                    Some("None") => "None",
                    _ => "Unknown",
                };
                (method, crypt.and_then(|crypt| integer(crypt, b"Length")))
            }
        }
    } else {
        ("RC4", None)
    };
    // A crypt filter's length is in bytes, though some write bits.
    let filter_bits = filter_length.map(|length| if length <= 32 { length * 8 } else { length });
    let length = integer(encrypt, b"Length");
    let key_bits = match method {
        "AESV3" => 256,
        "AESV2" => 128,
        _ if version >= 5 => 256,
        _ if version <= 1 => 40,
        _ => filter_bits
            .or(length)
            .unwrap_or(if version >= 4 { 128 } else { 40 }),
    };
    Encryption {
        filter: name(encrypt, b"Filter").unwrap_or_default(),
        version,
        revision: integer(encrypt, b"R").unwrap_or(0),
        key_bits: u32::try_from(key_bits).unwrap_or(0),
        method: match method {
            "AESV2" | "AESV3" => "AES",
            other => other,
        }
        .to_owned(),
    }
}

impl Document {
    /// What the document says about itself (see [`Metadata`]). Never fails: an entry that
    /// cannot be read is left out.
    #[must_use]
    pub fn metadata(&self) -> Metadata {
        let pdf = &self.pdf;
        let version = format!("{:?}", pdf.version());
        let version = version
            .strip_prefix("Pdf")
            .filter(|digits| digits.len() == 2)
            .map(|digits| format!("{}.{}", &digits[..1], &digits[1..]))
            .unwrap_or(version);
        let resolve = |value: &Object| match value {
            Object::Ref(id) => self.object_without_data(*id).ok().flatten(),
            other => Some(other.clone()),
        };
        let encryption = self
            .base_trailer()
            .encrypt()
            .and_then(resolve)
            .and_then(|encrypt| {
                encrypt
                    .as_dict()
                    .map(|encrypt| encryption(encrypt, &resolve))
            });
        let mut metadata = Metadata {
            version,
            encrypted: self.is_encrypted(),
            encryption,
            linearized: linearized(self.data()),
            ..Metadata::default()
        };
        if let Some(catalog) = pdf.xref().get::<HayroDict<'_>>(pdf.xref().root_id()) {
            if let Some(acroform) = catalog.get::<HayroDict<'_>>(b"AcroForm") {
                let form = form(&acroform);
                metadata.has_forms = form.fields > 0;
                metadata.has_signatures =
                    form.signed_signature_fields + form.empty_signature_fields > 0;
            }
            metadata.pdfa = catalog
                .get::<HayroStream<'_>>(b"Metadata")
                .filter(|stream| self.limits().check_stream(stream).is_ok())
                .and_then(|stream| stream.decoded().ok().and_then(|xmp| pdfa_claim(&xmp)));
        }
        if let Some(info) = self.info_id()
            && let Ok(Some(Object::Dict(info))) = self.object_without_data(info)
        {
            for (key, value) in info.iter() {
                let value = match value {
                    Object::Ref(id) => self.object_without_data(*id).ok().flatten(),
                    other => Some(other.clone()),
                };
                let text = match &value {
                    Some(Object::String(bytes)) => decode_text_string(bytes),
                    Some(Object::Name(name)) => String::from_utf8_lossy(name.as_bytes()).into(),
                    _ => continue,
                };
                metadata
                    .info
                    .insert(String::from_utf8_lossy(key.as_bytes()).into_owned(), text);
            }
        }
        metadata
    }
}

/// Whether the first object of `data` is a linearization dictionary.
fn linearized(data: &[u8]) -> bool {
    let head = &data[..data.len().min(LINEARIZATION_WINDOW)];
    let Some(start) = head.windows(3).position(|w| w == b"obj") else {
        return false;
    };
    let body = &head[start..];
    let end = body
        .windows(6)
        .position(|w| w == b"endobj")
        .unwrap_or(body.len());
    body[..end].windows(11).any(|w| w == b"/Linearized")
}

#[cfg(test)]
mod tests {
    use super::encryption;
    use crate::object::{Dict, Name, Object};

    /// An `/Encrypt` dictionary of version 4, its streams' crypt filter `filter`.
    fn version_4(filter: Option<Dict>, stream_filter: &str) -> Dict {
        let mut encrypt = Dict::new();
        encrypt.insert("Filter", Name::from("Standard"));
        encrypt.insert("V", 4);
        encrypt.insert("R", 4);
        encrypt.insert("Length", 128);
        encrypt.insert("StmF", Name::from(stream_filter));
        if let Some(filter) = filter {
            let mut filters = Dict::new();
            filters.insert("StdCF", filter);
            encrypt.insert("CF", filters);
        }
        encrypt
    }

    fn crypt_filter(method: &str, length: i64) -> Dict {
        let mut filter = Dict::new();
        filter.insert("CFM", Name::from(method));
        filter.insert("Length", length);
        filter
    }

    fn described(encrypt: &Dict) -> String {
        encryption(encrypt, &|value: &Object| Some(value.clone())).description()
    }

    #[test]
    fn crypt_filters_give_the_method_and_aes_its_own_length() {
        // As PyMuPDF describes them (the first two among documents encrypted in the wild).
        let rc4 = version_4(Some(crypt_filter("V2", 16)), "StdCF");
        assert_eq!(described(&rc4), "Standard V4 R4 128-bit RC4");
        let aes = version_4(Some(crypt_filter("AESV2", 16)), "StdCF");
        assert_eq!(described(&aes), "Standard V4 R4 128-bit AES");
        // AES-128 whatever the lengths say.
        let mut aes_40 = version_4(Some(crypt_filter("AESV2", 5)), "StdCF");
        aes_40.insert("Length", 40);
        assert_eq!(described(&aes_40), "Standard V4 R4 128-bit AES");
        let none = version_4(Some(crypt_filter("None", 16)), "StdCF");
        assert_eq!(described(&none), "Standard V4 R4 128-bit None");
        // Streams left as they are; a crypt filter missing.
        assert_eq!(
            described(&version_4(None, "Identity")),
            "Standard V4 R4 128-bit None"
        );
        assert_eq!(
            described(&version_4(None, "StdCF")),
            "Standard V4 R4 128-bit Unknown"
        );
    }
}
