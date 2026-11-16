//! The glyphs a page draws: text, position, advance, size, direction and font.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, OpenOptions};
use sealwax_text::{TextGlyph, glyphs};

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// One page drawing `content`, `page` added to its dictionary. Resources: /F1 Helvetica,
/// /F2 Courier, /F3 a font whose code 1 means "fi" (`ToUnicode`), /T3 a Type 3 font whose code
/// 65 is a square named `A`, /Fm a form drawing with Courier.
fn page(content: &str, page: &str) -> Document {
    document(content, page, "", "", &[])
}

/// The same, `catalog` added to the catalog, `form` to the form's dictionary, and `extra` objects
/// from number 12 on.
fn document(content: &str, page: &str, catalog: &str, form: &str, extra: &[&str]) -> Document {
    let to_unicode = stream(
        "",
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         1 begincodespacerange <00> <FF> endcodespacerange\n\
         1 beginbfchar <01> <00660069> endbfchar\nendcmap CMapName currentdict /CMap defineresource pop end end",
    );
    let mut objects = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>")),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R /T3 9 0 R >> \
                 /XObject << /Fm 11 0 R >> >> {page} >>"
            ),
        ),
        (4, stream("", content)),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_owned(),
        ),
        (6, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".to_owned()),
        (
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Times-Roman /ToUnicode 8 0 R >>"
                .to_owned(),
        ),
        (8, to_unicode),
        (
            9,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
             /CharProcs << /A 10 0 R >> /Encoding << /Differences [65 /A] >> /FirstChar 65 /LastChar 65 \
             /Widths [1000] >>"
                .to_owned(),
        ),
        (10, stream("", "1000 0 d0 0 0 1000 1000 re f")),
        (
            11,
            stream(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 600 800] /Resources << /Font << /C 6 0 R >> >> {form}"
                ),
                "BT /C 10 Tf 300 100 Td (in a form) Tj ET /Span <</MCID 0>> BDC BT /C 10 Tf 300 80 Td (xx) Tj ET EMC",
            ),
        ),
    ];
    for (at, body) in extra.iter().enumerate() {
        objects.push((12 + at, (*body).to_owned()));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in &objects {
        offsets.push(out.len());
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
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

fn text(found: &[TextGlyph]) -> String {
    found.iter().filter_map(|g| g.text.clone()).collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn glyphs_have_their_text_place_advance_size_and_font() {
    let doc = page("BT /F1 24 Tf 72 700 Td (Caf\\351) Tj ET", "");
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(text(&found), "Café");
    let c = &found[0];
    assert_eq!(c.origin, [72.0, 700.0]);
    // Helvetica's C is 722 units wide.
    assert!(
        close(c.end[0], 72.0 + 0.722 * 24.0) && close(c.end[1], 700.0),
        "{c:?}"
    );
    assert!(close(c.size, 24.0));
    assert_eq!(c.direction, [1.0, 0.0]);
    assert_eq!(c.font.as_deref(), Some("Helvetica"));
    assert!(!c.invisible);
    assert!(c.bbox[1] < 700.0 && c.bbox[3] > 700.0 + 0.7 * 24.0);
    assert!(c.ink.is_some());
    // The next glyph starts where the advance ends.
    assert!(close(found[1].origin[0], c.end[0]));
}

#[test]
fn a_ligature_means_its_letters_and_the_font_is_named_as_written() {
    let doc = page("BT /F3 10 Tf 50 50 Td <01> Tj ET", "");
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(text(&found), "fi");
    assert_eq!(found[0].font.as_deref(), Some("ABCDEF+Times-Roman"));
}

#[test]
fn the_gap_a_tj_adjustment_leaves_reads_from_the_advances() {
    let doc = page("BT /F2 10 Tf 0 0 Td [(ab) -500 (cd)] TJ ET", "");
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(text(&found), "abcd");
    // Courier: 600 units a glyph; -500 moves 5 points further at 10 points.
    let gap = found[2].origin[0] - found[1].end[0];
    assert!(close(gap, 5.0), "{gap}");
}

#[test]
fn a_rotated_page_keeps_its_default_user_space() {
    let doc = page(
        "BT /F1 10 Tf 100 200 Td (R) Tj ET",
        "/Rotate 90 /CropBox [50 70 550 770]",
    );
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(found[0].origin, [100.0, 200.0]);
}

#[test]
fn type3_invisible_and_form_glyphs_are_there_and_marked() {
    let doc = page(
        "BT /T3 12 Tf 10 10 Td (A) Tj ET BT 3 Tr /F1 12 Tf 10 30 Td (ocr) Tj ET /Fm Do",
        "",
    );
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(text(&found), "Aocrin a formxx");
    assert!(
        found[0].ink.is_none(),
        "a Type 3 glyph's outline is not read"
    );
    assert!(close(found[0].end[0] - found[0].origin[0], 12.0));
    assert!(found[1..4].iter().all(|g| g.invisible));
    assert!(
        found[4..]
            .iter()
            .all(|g| g.font.as_deref() == Some("Courier"))
    );
}

#[test]
fn the_font_follows_the_graphics_state() {
    let doc = page(
        "BT /F1 10 Tf 0 0 Td (a) Tj ET q BT /F2 10 Tf (b) Tj ET Q BT 0 20 Td (c) Tj ET",
        "",
    );
    let fonts: Vec<Option<String>> = glyphs(&doc, 0, false)
        .unwrap()
        .into_iter()
        .map(|g| g.font)
        .collect();
    assert_eq!(
        fonts,
        [
            Some("Helvetica".to_owned()),
            Some("Courier".to_owned()),
            Some("Helvetica".to_owned())
        ]
    );
}

#[test]
fn text_drawn_twice_to_look_bold_is_marked_overdrawn() {
    // Bold faked by drawing again 0.3 pt to the right; leaders whose dots overlap; a word drawn
    // twice far apart, which is no repetition.
    let doc = page(
        "BT /F1 10 Tf 50 700 Td (Bold) Tj ET BT /F1 10 Tf 50.3 700 Td (Bold) Tj ET \
         BT /F1 10 Tf 50 600 Td [(.) 200 (.) 200 (.) 200 (.)] TJ ET \
         BT /F1 10 Tf 50 500 Td (ab) Tj ET BT /F1 10 Tf 50 400 Td (ab) Tj ET",
        "",
    );
    let found = glyphs(&doc, 0, false).unwrap();
    let shown: String = found
        .iter()
        .filter(|g| !g.overdrawn)
        .filter_map(|g| g.text.clone())
        .collect();
    // "Bold" then "Bold" is no glyph-by-glyph repetition: each glyph follows a different one.
    assert_eq!(text(&found), "BoldBold....abab");
    assert_eq!(shown, "BoldBold..abab");
    // A dot 0.78 pt from the one shown before it repeats it; the next, 1.56 pt away, does not.
    let dots: Vec<bool> = found[8..12].iter().map(|g| g.overdrawn).collect();
    assert_eq!(dots, [false, true, false, true]);
}

#[test]
fn annotations_appearances_are_read_when_asked_for_and_marked() {
    // A widget whose appearance draws a filled value, and a hidden one (flag 2).
    let appearance = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /H 5 0 R >> >>",
        "/Tx BMC BT /H 10 Tf 2 5 Td (Alice) Tj ET EMC",
    );
    let doc = document(
        "BT /F1 10 Tf 50 700 Td (Name) Tj ET",
        "/Annots [12 0 R 14 0 R]",
        "",
        "",
        &[
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [100 400 200 420] /F 4 /AP << /N 13 0 R >> >>",
            &appearance,
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (hidden) /Rect [100 300 200 320] /F 2 /AP << /N 13 0 R >> >>",
        ],
    );
    assert_eq!(text(&glyphs(&doc, 0, false).unwrap()), "Name");
    let found = glyphs(&doc, 0, true).unwrap();
    assert_eq!(text(&found), "NameAlice");
    assert!(found[..4].iter().all(|g| !g.annotation));
    assert!(found[4..].iter().all(|g| g.annotation));
    // Placed by the annotation's rectangle.
    assert!(
        close(found[4].origin[0], 102.0) && close(found[4].origin[1], 405.0),
        "{:?}",
        found[4]
    );
}

fn read(found: &[TextGlyph]) -> String {
    found
        .iter()
        .map(|g| match &g.actual_text {
            Some(actual) => actual.clone(),
            None => g.text.clone().unwrap_or_default(),
        })
        .collect()
}

#[test]
fn an_actual_text_in_a_sequence_replaces_what_it_encloses_once() {
    let doc = page(
        "BT /F1 10 Tf 50 700 Td (Item) Tj ET \
         /Span <</ActualText <FEFF0009>>> BDC BT /F1 10 Tf 80 700 Td (.....) Tj ET EMC \
         BT /F1 10 Tf 120 700 Td (7) Tj ET /Span <</ActualText ()>> BDC BT /F1 10 Tf 0 0 Td (gone) Tj ET EMC",
        "",
    );
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(text(&found), "Item.....7gone");
    assert_eq!(read(&found), "Item\t7");
    let dots: Vec<Option<&str>> = found[4..9]
        .iter()
        .map(|g| g.actual_text.as_deref())
        .collect();
    assert_eq!(dots, [Some("\t"), Some(""), Some(""), Some(""), Some("")]);
    assert!(found[..4].iter().all(|g| g.actual_text.is_none()));
}

#[test]
fn an_actual_text_on_the_structure_element_replaces_the_content_tied_to_it() {
    // MCID 0 is tied to 14 (/ActualText); MCID 1 to 16, whose /ActualText is read, not that of
    // its parent 15; MCID 2 to 17, which says nothing (its parent 13 does: not read); the form's
    // MCID 0 to 18.
    let doc = document(
        "/Span <</MCID 0>> BDC BT /F1 10 Tf 50 700 Td (....) Tj ET EMC \
         /Span <</MCID 1>> BDC BT /F1 10 Tf 50 600 Td (ab) Tj ET EMC \
         /P <</MCID 2>> BDC BT /F1 10 Tf 50 500 Td (cd) Tj ET EMC /Fm Do",
        "/StructParents 0",
        "/StructTreeRoot 12 0 R",
        "/StructParents 1",
        &[
            "<< /Type /StructTreeRoot /K 13 0 R /ParentTree << /Kids [19 0 R] >> >>",
            "<< /Type /StructElem /S /Document /P 12 0 R /K [14 0 R 15 0 R 17 0 R 18 0 R] /ActualText (all) >>",
            "<< /Type /StructElem /S /Span /P 13 0 R /ActualText <FEFF0009> /K 0 >>",
            "<< /Type /StructElem /S /Span /P 13 0 R /ActualText (outer) /K 16 0 R >>",
            "<< /Type /StructElem /S /Span /P 15 0 R /ActualText (inner) /K 1 >>",
            "<< /Type /StructElem /S /P /P 13 0 R /K 2 >>",
            "<< /Type /StructElem /S /Span /P 13 0 R /ActualText (form) /K 0 >>",
            "<< /Limits [0 1] /Nums [0 [14 0 R 16 0 R 17 0 R] 1 [18 0 R]] >>",
        ],
    );
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(text(&found), "....abcdin a formxx");
    assert_eq!(read(&found), "\tinnercdin a formform");
}

#[test]
fn a_page_without_text_has_no_glyph_and_a_missing_page_is_refused() {
    let doc = page("0 0 10 10 re f", "");
    assert!(glyphs(&doc, 0, false).unwrap().is_empty());
    assert!(glyphs(&doc, 1, false).is_err());
}
