//! A page's words, in lines and blocks, as MuPDF's structured text makes them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, OpenOptions};
use sealwax_text::{TextWord, words};

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

/// The words of `content`, each as (text, block, line, word).
fn read(content: &str) -> Vec<(String, usize, usize, usize)> {
    words(&page(content), 0)
        .unwrap()
        .into_iter()
        .map(
            |TextWord {
                 text,
                 block,
                 line,
                 word,
                 ..
             }| (text, block, line, word),
        )
        .collect()
}

fn texts(content: &str) -> Vec<String> {
    read(content).into_iter().map(|w| w.0).collect()
}

#[test]
fn words_are_cut_at_spaces_and_at_gaps_wide_enough() {
    // A space, then a gap of 0.01 em (nothing), 0.3 em (a space), a step back of 0.4 em
    // (glyphs overlapping: nothing). As PyMuPDF reads it.
    assert_eq!(
        texts("BT /F1 10 Tf 72 700 Td [(one two) -10 (three) -300 (four) 400 (five)] TJ ET"),
        ["one", "twothree", "fourfive"]
    );
    // Shown apart, in two text objects, the second where the first ended plus 0.3 em.
    let ended = 72.0 + 10.0 * (556.0 + 556.0 + 556.0) / 1000.0;
    assert_eq!(
        texts(&format!(
            "BT /F1 10 Tf 72 700 Td (abc) Tj ET BT /F1 10 Tf {} 700 Td (def) Tj ET",
            ended + 3.0
        )),
        ["abc", "def"]
    );
}

#[test]
fn lines_and_blocks_follow_baselines_columns_and_indents() {
    // As PyMuPDF numbers them.
    let words = read(
        "BT /F1 10 Tf 72 700 Td (First line) Tj 0 -12 Td (second line) Tj 10 -12 Td (indented) \
         Tj 200 0 Td (column) Tj -210 -40 Td (far below) Tj ET",
    );
    let shape: Vec<(usize, usize, usize)> = words.iter().map(|w| (w.1, w.2, w.3)).collect();
    assert_eq!(
        shape,
        [
            (0, 0, 0),
            (0, 0, 1),
            (0, 1, 0),
            (0, 1, 1),
            // A line starting right of where the one before started: a paragraph.
            (1, 0, 0),
            // Same baseline, far along: another line, same block.
            (1, 1, 0),
            // Farther than 1.5 em below: a new block.
            (2, 0, 0),
            (2, 0, 1),
        ]
    );
}

#[test]
fn bold_faked_counts_once_and_what_is_clipped_away_not_at_all() {
    // Each glyph drawn twice in place.
    assert_eq!(
        texts("BT /F1 10 Tf 72 700 Td [(b) 556 (b) (o) 556 (o) (l) 222 (l) (d) 556 (d)] TJ ET"),
        ["bold"]
    );
    assert_eq!(
        texts(
            "q 0 0 100 800 re W n BT /F1 10 Tf 72 700 Td (kept) Tj ET \
             BT /F1 10 Tf 300 700 Td (hidden) Tj ET Q BT /F1 10 Tf 72 600 Td (out) Tj ET \
             BT /F1 10 Tf 700 600 Td (beyond) Tj ET"
        ),
        ["kept", "out"]
    );
}

#[test]
fn a_word_s_box_holds_its_glyphs() {
    let doc = page("BT /F2 10 Tf 100 500 Td (abc def) Tj ET");
    let found = words(&doc, 0).unwrap();
    // Courier: 0.6 em a glyph.
    assert_eq!(found.len(), 2);
    assert!((found[0].bbox[0] - 100.0).abs() < 1e-6 && (found[0].bbox[2] - 118.0).abs() < 1e-6);
    assert!((found[1].bbox[0] - 124.0).abs() < 1e-6 && (found[1].bbox[2] - 142.0).abs() < 1e-6);
    assert!(found[0].bbox[1] < 500.0 && found[0].bbox[3] > 500.0);
    assert!(matches!(
        words(&doc, 1),
        Err(sealwax_core::Error::InvalidArgument { what: "page index" })
    ));
}
