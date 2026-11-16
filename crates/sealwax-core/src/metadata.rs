//! What a document says about itself: its version, whether it is encrypted or linearized, its
//! form and signature fields, its PDF/A claim, its document information. Read without failing:
//! what cannot be read is left out, or reads as absent.

use std::collections::BTreeMap;

use hayro_syntax::object::{Dict as HayroDict, Stream as HayroStream};

use crate::census::{form, pdfa_claim};
use crate::document::Document;
use crate::object::Object;
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
        let mut metadata = Metadata {
            version,
            encrypted: self.is_encrypted(),
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
