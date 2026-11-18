//! Text looked for on a page, as PyMuPDF's `search_for` finds it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, OpenOptions};
use sealwax_text::search;

/// One page drawing `content` with Helvetica as /F1, Courier as /F2 (both WinAnsi).
fn page(content: &str) -> Document {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>"
            .to_owned(),
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

/// How many boxes each occurrence of `needle` has.
fn found(content: &str, needle: &str) -> Vec<usize> {
    search(&page(content), 0, needle)
        .unwrap()
        .iter()
        .map(Vec::len)
        .collect()
}

#[test]
fn case_white_space_and_ligatures_are_read_alike() {
    let content = "BT /F1 10 Tf 72 700 Td (Total due: 120 EUR, total paid) Tj ET";
    assert_eq!(found(content, "TOTAL"), [1, 1]);
    assert_eq!(found(content, "due:   120"), [1]);
    assert_eq!(found(content, "missing"), Vec::<usize>::new());
    assert_eq!(found(content, ""), Vec::<usize>::new());
    // A ligature looked for reads as its letters.
    assert_eq!(
        found("BT /F1 10 Tf 72 700 Td (office) Tj ET", "o\u{FB03}ce"),
        [1]
    );
    // Occurrences do not overlap.
    assert_eq!(found("BT /F1 10 Tf 72 700 Td (aaaa) Tj ET", "aa"), [1, 1]);
}

#[test]
fn an_occurrence_across_lines_has_a_box_on_each() {
    let doc = page("BT /F1 10 Tf 72 700 Td (the total) Tj 0 -12 Td (amount due) Tj ET");
    let hits = search(&doc, 0, "total amount").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].len(), 2);
    assert!(hits[0][0][1] > hits[0][1][1]);
    // A hyphen ending a line goes, the lines joined, as PyMuPDF's search dehyphenates them;
    // so does a soft hyphen.
    let hyphenated = "BT /F1 10 Tf 72 700 Td (self-) Tj 0 -12 Td (service) Tj ET";
    assert_eq!(found(hyphenated, "selfservice"), [2]);
    assert_eq!(found(hyphenated, "self-service"), Vec::<usize>::new());
    assert_eq!(
        found(
            "BT /F1 10 Tf 72 700 Td (inter\\255) Tj 0 -12 Td (national) Tj ET",
            "international"
        ),
        [2]
    );
}

#[test]
fn a_text_too_long_or_a_page_missing_is_refused() {
    let doc = page("BT /F1 10 Tf 72 700 Td (word) Tj ET");
    assert!(matches!(
        search(&doc, 0, &"x".repeat(1025)),
        Err(sealwax_core::Error::InvalidArgument {
            what: "search text"
        })
    ));
    assert!(matches!(
        search(&doc, 1, "word"),
        Err(sealwax_core::Error::InvalidArgument { what: "page index" })
    ));
}
