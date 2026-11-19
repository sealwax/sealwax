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
        // Past `endstream`, whose own "stream\n" would otherwise be taken for the next stream's.
        at = (end + b"endstream".len()).min(saved.len());
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

/// A document of `objects`, numbered from 1, the first the catalog; `trailer` goes into the
/// trailer.
fn build(objects: &[String], trailer: &str) -> Document {
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
            "trailer\n<< /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Document::open(out, &OpenOptions::default()).unwrap()
}

/// Every place a document may keep what a page shows, under a mark or reached from elsewhere:
/// none of it is left once the marks are applied and the pages rebuilt alone.
#[test]
fn nothing_of_what_was_redacted_is_left_anywhere() {
    let content = "BT /F1 12 Tf 100 700 Td (LEAK-VISIBLE) Tj ET \
                   BT 3 Tr /F1 12 Tf 100 680 Td (LEAK-INVISIBLE) Tj ET \
                   q 400 400 10 10 re W n BT /F1 12 Tf 100 660 Td (LEAK-CLIPPED) Tj ET Q \
                   /Span <</ActualText (LEAK-ACTUAL)>> BDC BT /F1 12 Tf 100 640 Td (x) Tj ET EMC \
                   /P <</MCID 0>> BDC BT /F1 12 Tf 100 620 Td (y) Tj ET EMC \
                   /Span /P0 BDC BT /F1 12 Tf 100 600 Td (z) Tj ET EMC \
                   /Fm Do q 110 0 0 10 100 560 cm /Im Do Q \
                   q /Pattern cs /Pt scn 100 495 40 4 re f Q \
                   BT /F1 12 Tf 100 300 Td (KEEP) Tj ET";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Metadata 14 0 R \
         /Names << /Dests << /Names [(LEAK-DEST) [3 0 R /Fit]] >> \
         /EmbeddedFiles << /Names [(a.txt) 12 0 R] >> >> \
         /StructTreeRoot 15 0 R /MarkInfo << /Marked true >> /AcroForm << /Fields [7 0 R] >> >>"
            .to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Fm 6 0 R /Im 18 0 R >> \
         /Properties << /P0 17 0 R >> /Pattern << /Pt 21 0 R >> >> \
         /Annots [7 0 R 8 0 R 9 0 R] /StructParents 0 \
         /PieceInfo << /App << /Private (LEAK-PIECE) >> >> /Thumb 20 0 R >>"
            .to_owned(),
        stream("", content),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 600 800] /Resources << /Font << /H 5 0 R >> >>",
            "BT /H 12 Tf 100 580 Td (LEAK-FORM) Tj ET",
        ),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (f) /V (LEAK-FIELD) /Rect [100 520 300 540] /F 4 >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /FreeText /Rect [100 500 300 515] /Contents (LEAK-NOTE) \
         /DA (/Helv 9 Tf 0 g) /F 4 >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /Redact /Rect [90 490 320 720] /IC [0 0 0] >>".to_owned(),
        "<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>".to_owned(),
        "<< /Title (LEAK-BOOKMARK) /Parent 10 0 R /Dest [3 0 R /Fit] >>".to_owned(),
        "<< /Type /Filespec /F (a.txt) /EF << /F 13 0 R >> >>".to_owned(),
        stream("/Type /EmbeddedFile", "LEAK-ATTACHMENT"),
        stream("/Type /Metadata /Subtype /XML", "<x:xmpmeta>LEAK-XMP</x:xmpmeta>"),
        "<< /Type /StructTreeRoot /K 16 0 R /ParentTree << /Nums [0 [16 0 R]] >> >>".to_owned(),
        "<< /Type /StructElem /S /P /P 15 0 R /Pg 3 0 R /K 0 /Alt (LEAK-STRUCT) >>".to_owned(),
        "<< /ActualText (LEAK-NAMED) >>".to_owned(),
        stream(
            "/Type /XObject /Subtype /Image /Width 11 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "LEAK-PIXELS",
        ),
        "<< /Title (LEAK-INFO) >>".to_owned(),
        stream(
            "/Width 10 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "LEAK-THUMB",
        ),
        // A shape's fill drawing text, as office suites fill a shape with a picture.
        stream(
            "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 40 4] \
             /XStep 40 /YStep 4 /Resources << /Font << /F1 5 0 R >> >>",
            "BT /F1 3 Tf 0 0 Td (LEAK-PATTERN) Tj ET",
        ),
    ];
    let mut doc = build(&objects, "/Info 19 0 R");
    let applied = apply_redactions(&mut doc, &RedactOptions::default()).unwrap();
    assert_eq!(applied.marks, 1);
    let rebuilt = doc.rebuilt_for_redaction().unwrap();
    let all = everything(&rebuilt);
    // The search reads every stream: what stays is found.
    assert!(find(&all, b"(KEEP)").is_some());
    let text = String::from_utf8_lossy(&all);
    let left: Vec<&str> = text
        .match_indices("LEAK-")
        .map(|(at, _)| {
            text[at..]
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .next()
                .unwrap_or("")
        })
        .collect();
    assert!(left.is_empty(), "left: {left:?}");
    let rebuilt = Document::open(
        rebuilt.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap();
    let kept: String = glyphs(&rebuilt, 0, false)
        .unwrap()
        .iter()
        .filter_map(|g| g.text.clone())
        .collect();
    assert!(kept.contains("KEEP"), "{kept}");
}

#[test]
fn applying_an_annotation_only_mark_requires_a_full_save() {
    let mut doc = build(
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [4 0 R 5 0 R] >>".into(),
            "<< /Type /Annot /Subtype /Redact /Rect [5 5 30 30] >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [10 10 20 20] >>".into(),
        ],
        "",
    );
    let done = apply_redactions(&mut doc, &RedactOptions::default()).unwrap();
    assert_eq!(done.links_removed, 1);
    assert_eq!(done.redacted, Default::default());
    assert!(doc.is_redacted());
    assert!(doc.save_incremental().is_err());
    assert!(doc.save(&SaveOptions::default()).is_ok());
}

#[test]
fn an_incomplete_mark_application_preserves_the_input_and_prior_edits() {
    let mut doc = marked();
    doc.set_info(&[("Title".into(), "Revision".into())].into())
        .unwrap();
    let page = doc.page_ids()[0].unwrap();
    let mut dict = doc
        .object(page)
        .unwrap()
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let mut stream_dict = sealwax_core::Dict::new();
    stream_dict.insert("Filter", sealwax_core::Name::from("UnavailableDecoder"));
    let contents = doc
        .add_object(sealwax_core::Object::Stream(sealwax_core::Stream {
            dict: stream_dict,
            data: vec![],
        }))
        .unwrap();
    dict.insert("Contents", contents);
    doc.set_object(page, sealwax_core::Object::Dict(dict));
    let before = doc.save_incremental().unwrap();
    assert!(apply_redactions(&mut doc, &RedactOptions::default()).is_err());
    assert!(doc.has_redaction_marks().unwrap());
    assert_eq!(doc.save_incremental().unwrap(), before);
}
