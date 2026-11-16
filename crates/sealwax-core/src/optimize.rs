//! A PDF rewritten smaller without changing what it shows: streams left unfiltered are
//! compressed, and the save writes only what is reachable, in object streams. Images are not
//! recompressed: that would change what pages show.

use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::document::{Document, OpenOptions};
use crate::error::Result;
use crate::object::{Name, Object};
use crate::save::SaveOptions;

/// Why the input was kept as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Kept {
    /// A rewrite would drop its encryption.
    Encrypted,
    /// A rewrite would break its signatures.
    Signed,
    /// The rewrite is not smaller.
    NotSmaller,
}

/// What [`optimize`] returns.
#[derive(Debug, Clone)]
pub struct Optimized {
    /// The rewritten document, or the input when it was kept.
    pub data: Vec<u8>,
    /// Why the input was kept, if it was.
    pub kept: Option<Kept>,
    /// Streams compressed by the rewrite.
    pub streams_compressed: usize,
}

/// `data` rewritten smaller, or as it was: when it is encrypted or signed, or when the rewrite is
/// not smaller.
///
/// # Errors
/// As [`Document::open`] when `data` cannot be read, and as [`Document::save`].
pub fn optimize(data: &[u8], options: &OpenOptions) -> Result<Optimized> {
    let mut doc = Document::open(data.to_vec(), options)?;
    let census = doc.census();
    let kept = |kept| Optimized {
        data: data.to_vec(),
        kept: Some(kept),
        streams_compressed: 0,
    };
    if census.encrypted {
        return Ok(kept(Kept::Encrypted));
    }
    if census.form.signed_signature_fields > 0 {
        return Ok(kept(Kept::Signed));
    }
    let streams_compressed = doc.compress_streams()?;
    let rewritten = doc.save(&SaveOptions::default())?;
    if rewritten.len() >= data.len() {
        return Ok(kept(Kept::NotSmaller));
    }
    Ok(Optimized {
        data: rewritten,
        kept: None,
        streams_compressed,
    })
}

impl Document {
    /// Compress with `FlateDecode` every reachable stream that has no filter, where that makes
    /// it smaller; the XMP metadata stays unfiltered (PDF/A wants it readable). Returns the
    /// streams compressed.
    ///
    /// # Errors
    /// [`crate::Error::LimitExceeded`] when an object nests deeper than sealwax accepts;
    /// [`crate::Error::Timeout`] past the deadline.
    pub fn compress_streams(&mut self) -> Result<usize> {
        let mut compressed = 0;
        for id in self.reachable_objects()? {
            self.check_time()?;
            let Some(Object::Stream(dict_only)) = self.object_without_data(id)? else {
                continue;
            };
            let filtered = match dict_only.dict.get(b"Filter") {
                None => false,
                Some(Object::Array(filters)) => !filters.is_empty(),
                Some(_) => true,
            };
            if filtered
                || dict_only.dict.get(b"Type").and_then(Object::as_name) == Some(b"Metadata")
            {
                continue;
            }
            let Some(Object::Stream(mut stream)) = self.object(id)? else {
                continue;
            };
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
            let Ok(data) = encoder
                .write_all(&stream.data)
                .and_then(|()| encoder.finish())
            else {
                continue;
            };
            if data.len() >= stream.data.len() {
                continue;
            }
            stream.dict.insert("Filter", Name::from("FlateDecode"));
            stream.dict.remove(b"DecodeParms");
            stream.data = data;
            self.set_object(id, Object::Stream(stream));
            compressed += 1;
        }
        Ok(compressed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Kept, optimize};
    use crate::{Document, OpenOptions};

    fn pdf(content: &str, catalog_more: &str) -> Vec<u8> {
        let objects = [
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_more} >>"),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            // Unreachable: a save leaves it out.
            "<< /Unused true >>".to_owned(),
        ];
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

    #[test]
    fn content_is_compressed_and_the_page_kept() {
        let content = "0 g 10 10 5 5 re f\n".repeat(200);
        let input = pdf(&content, "");
        let optimized = optimize(&input, &OpenOptions::default()).unwrap();
        assert_eq!(optimized.kept, None);
        assert_eq!(optimized.streams_compressed, 1);
        assert!(optimized.data.len() < input.len() / 4);
        let doc = Document::open(optimized.data, &OpenOptions::default()).unwrap();
        assert_eq!(doc.page_count(), 1);
    }

    #[test]
    fn a_signed_or_small_document_is_kept() {
        let small = pdf("0 g", "");
        let optimized = optimize(&small, &OpenOptions::default()).unwrap();
        assert_eq!(optimized.kept, Some(Kept::NotSmaller));
        assert_eq!(optimized.data, small);
        let signed = pdf(
            &"0 g\n".repeat(500),
            "/AcroForm << /Fields [<< /FT /Sig /T (s) /V << /Type /Sig /Contents <00> >> >>] >>",
        );
        let optimized = optimize(&signed, &OpenOptions::default()).unwrap();
        assert_eq!(optimized.kept, Some(Kept::Signed));
        assert_eq!(optimized.data, signed);
    }
}
