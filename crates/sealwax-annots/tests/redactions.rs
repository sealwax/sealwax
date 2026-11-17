//! Redaction marks applied: what is under them gone from the file, their overlay painted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read as _;

use sealwax_annots::apply_redactions;
use sealwax_core::{Document, OpenOptions, SaveOptions, XrefFormat};
use sealwax_text::{RedactOptions, glyphs};

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// "Hello secret world" at 72 700, a field filled "SECRETVALUE" at 300 690, a mark over both
/// (black, its overlay text "REDACTED"), a link under the mark and one away from it.
fn marked() -> Document {
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] /DA (/Helv 0 Tf 0 g) >> >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R 7 0 R 8 0 R 9 0 R] >>"
            .to_owned(),
        stream("", "BT /F1 12 Tf 72 700 Td (Hello secret world) Tj ET"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_owned(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (account) /V (SECRETVALUE) /Rect [300 690 450 710] /F 4 >>".to_owned(),
        "<< /Type /Annot /Subtype /Redact /Rect [100 685 460 715] /IC [0 0 0] /OverlayText (REDACTED) /DA (/Helv 9 Tf 1 g) \
         /QuadPoints [100 715 460 715 100 685 460 685] >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /Link /Rect [310 692 330 708] /A << /S /URI /URI (mailto:hidden@example.org) >> >>".to_owned(),
        "<< /Type /Annot /Subtype /Link /Rect [72 100 200 120] /A << /S /URI /URI (https://example.org) >> >>".to_owned(),
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
    Document::open(out, &OpenOptions::default()).unwrap()
}

fn everything(doc: &Document) -> Vec<u8> {
    let saved = doc
        .save(&SaveOptions::default().xref(XrefFormat::Table))
        .unwrap();
    let mut all = saved.clone();
    let mut at = 0;
    while let Some(start) = find(&saved[at..], b"stream\n").map(|i| at + i + 7) {
        let end = find(&saved[start..], b"endstream").map_or(saved.len(), |i| start + i);
        let mut inflated = Vec::new();
        if flate2::read::ZlibDecoder::new(&saved[start..end])
            .read_to_end(&mut inflated)
            .is_ok()
        {
            all.extend_from_slice(&inflated);
        }
        at = end;
    }
    all
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn what_marks_cover_is_gone_and_their_overlay_painted() {
    let mut doc = marked();
    let applied = apply_redactions(&mut doc, &RedactOptions::default()).unwrap();
    assert_eq!(
        (applied.marks, applied.pages, applied.links_removed),
        (1, 1, 1)
    );
    // "secret world" and the field's value, flattened first, then redacted.
    assert!(applied.redacted.glyphs >= 6 + 11, "{applied:?}");
    let doc = doc.rebuilt_for_redaction().unwrap();
    let all = everything(&doc);
    for gone in [
        &b"secret"[..],
        b"SECRETVALUE",
        b"hidden@example.org",
        b"/Redact",
    ] {
        assert!(
            find(&all, gone).is_none(),
            "{}",
            String::from_utf8_lossy(gone)
        );
    }
    assert!(
        find(&all, b"https://example.org").is_some(),
        "the other link stays"
    );
    let doc = Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap();
    let text: String = glyphs(&doc, 0, false)
        .unwrap()
        .iter()
        .filter_map(|g| g.text.clone())
        .collect();
    assert!(
        text.starts_with("Hello") && text.contains("REDACTED"),
        "{text}"
    );
}

#[test]
fn a_document_without_marks_is_left_as_it_is() {
    let mut doc = Document::open(
        marked().save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap();
    // Without its mark: the field stays a field.
    let page = doc.page_ids()[0].unwrap();
    let Some(sealwax_core::Object::Dict(mut dict)) = doc.object(page).unwrap() else {
        panic!()
    };
    let annots: Vec<sealwax_core::Object> =
        dict.get(b"Annots").unwrap().as_array().unwrap().to_vec();
    let kept: Vec<sealwax_core::Object> = annots
        .into_iter()
        .filter(|a| {
            doc.object(a.as_ref_id().unwrap())
                .unwrap()
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"Subtype")
                .and_then(sealwax_core::Object::as_name)
                != Some(b"Redact")
        })
        .collect();
    dict.insert("Annots", sealwax_core::Object::Array(kept));
    doc.set_object(page, sealwax_core::Object::Dict(dict));
    let applied = apply_redactions(&mut doc, &RedactOptions::default()).unwrap();
    assert_eq!(applied.marks, 0);
    assert_eq!(sealwax_annots::fields(&doc, None).unwrap().len(), 1);
}
