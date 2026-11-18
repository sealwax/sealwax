//! The glyphs a page draws: text, position, advance, size, direction and font.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, OpenOptions};
use sealwax_text::{TextGlyph, glyphs, search, text_blocks, words};

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
    with_fonts(content, page, catalog, form, "", extra)
}

fn with_fonts(
    content: &str,
    page: &str,
    catalog: &str,
    form: &str,
    fonts: &str,
    extra: &[&str],
) -> Document {
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
                 /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R /T3 9 0 R {fonts} >> \
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
    assert!(search(&doc, 0, "Alice", false).unwrap().is_empty());
    assert_eq!(search(&doc, 0, "Alice", true).unwrap().len(), 1);
    assert_eq!(words(&doc, 0, true).unwrap().len(), 2);
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

#[test]
fn words_and_lines_use_the_unpainted_advance_and_baseline_offset() {
    for (points, expected_words, expected_lines) in [
        (0, 1, 1),
        (2, 1, 1),
        (4, 2, 1),
        (6, 2, 1),
        (10, 2, 1),
        (15, 2, 1),
        (20, 2, 2),
        (40, 2, 2),
    ] {
        let doc = page(
            &format!("BT /F2 20 Tf 50 700 Td [(ab) {} (cd)] TJ ET", -50 * points),
            "",
        );
        let blocks = text_blocks(&doc, 0, false).unwrap();
        let found = words(&doc, 0, false).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), expected_lines, "{points}");
        assert_eq!(found.len(), expected_words, "{points}");
        assert_eq!(
            found.iter().map(|w| w.text.as_str()).collect::<String>(),
            "abcd"
        );
        for (line_index, line) in blocks[0].lines.iter().enumerate() {
            assert_eq!(line.direction, [1.0, 0.0]);
            for (word_index, word) in line.words.iter().enumerate() {
                assert_eq!(
                    (word.block, word.line, word.word),
                    (0, line_index, word_index)
                );
            }
        }
        assert!(close(found[0].bbox[0], 50.0));
        assert!(close(
            found[0].bbox[2],
            if expected_words == 1 {
                98.0 + f64::from(points)
            } else {
                74.0
            }
        ));
    }

    for (drop, blocks) in [(18, 1), (24, 1), (30, 1), (34, 2), (42, 2)] {
        let doc = page(
            &format!("BT /F2 20 Tf 50 700 Td (first) Tj 0 -{drop} Td (second) Tj ET"),
            "",
        );
        assert_eq!(text_blocks(&doc, 0, false).unwrap().len(), blocks, "{drop}");
    }
}

#[test]
fn content_order_ocr_overdraw_and_actual_text_survive_word_grouping() {
    let doc = page(
        "BT /F2 20 Tf 3 Tr 50 100 Td (OCR) Tj ET \
        BT /F2 20 Tf 50 700 Td (B) Tj ET BT /F2 20 Tf 50 700 Td (B) Tj ET \
        BT /F2 20 Tf 50 600 Td /Span << /ActualText (lake shore) >> BDC (xxxxx) Tj EMC (.) Tj ET",
        "",
    );
    let w = words(&doc, 0, false).unwrap();
    assert_eq!(
        w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
        ["OCR", "B", "lake", "shore."]
    );
    assert!(close(w[2].bbox[0], 50.0) && close(w[2].bbox[2], 74.0));
    assert!(close(w[3].bbox[0], 80.0) && close(w[3].bbox[2], 122.0));
    assert!(search(&doc, 0, "xxxxx", false).unwrap().is_empty());
    let partial = search(&doc, 0, "ak", false).unwrap();
    assert!(close(partial[0][0][0][0], 56.0) && close(partial[0][0][1][0], 68.0));
}

#[test]
fn search_finds_parts_of_words_phrases_and_dehyphenated_lines() {
    let doc = page(
        "BT /F2 20 Tf 50 700 Td (first) Tj 0 -24 Td (second) Tj \
        0 -24 Td (multi-) Tj 0 -24 Td (line Red-fox red-Fox) Tj ET",
        "",
    );
    let phrase = search(&doc, 0, "sT   SE", false).unwrap();
    assert_eq!(phrase.len(), 1);
    assert_eq!(phrase[0].len(), 2);
    assert!(close(phrase[0][0][0][0], 86.0) && close(phrase[0][0][1][0], 110.0));
    assert!(close(phrase[0][1][0][0], 50.0) && close(phrase[0][1][1][0], 74.0));
    assert!(search(&doc, 0, "stse", false).unwrap().is_empty());
    let hyphenated = search(&doc, 0, "multiline", false).unwrap();
    assert_eq!(hyphenated.len(), 1);
    assert_eq!(hyphenated[0].len(), 2);
    assert!(close(hyphenated[0][0][1][0], 110.0));
    assert_eq!(search(&doc, 0, "red-FOX", false).unwrap().len(), 2);
    assert!(search(&doc, 0, "   ", false).unwrap().is_empty());
}

#[test]
fn folded_unicode_and_ligatures_keep_their_source_positions() {
    let doc = page(
        "BT /F3 20 Tf 50 700 Td <01> Tj ET \
        BT /F1 20 Tf 50 650 Td (Stra\\337e) Tj ET",
        "",
    );
    let whole = search(&doc, 0, "FI", false).unwrap()[0][0];
    let second = search(&doc, 0, "i", false).unwrap()[0][0];
    assert!(close(second[0][0], (whole[0][0] + whole[1][0]) / 2.0));
    assert!(close(second[1][0], whole[1][0]));
    assert_eq!(search(&doc, 0, "STRASSE", false).unwrap().len(), 1);
    assert_eq!(search(&doc, 0, "s", false).unwrap().len(), 2);
}

#[test]
fn rotated_and_sheared_highlights_follow_the_text_matrix() {
    let doc = page(
        "BT /F2 20 Tf 0.8 0.6 -0.3 1 100 120 Tm (abc) Tj ET",
        "/Rotate 90 /CropBox [50 60 400 450]",
    );
    let line = &text_blocks(&doc, 0, false).unwrap()[0].lines[0];
    assert!(close(line.direction[0], 0.8) && close(line.direction[1], 0.6));
    let [tl, tr, bl, br] = search(&doc, 0, "b", false).unwrap()[0][0];
    assert!(close(tr[0] - tl[0], 0.8 * 12.0) && close(tr[1] - tl[1], 0.6 * 12.0));
    assert!(close(br[0] - bl[0], tr[0] - tl[0]));
    assert!(close(tl[0] - bl[0], -0.3 * (tl[1] - bl[1])));
    let b = &glyphs(&doc, 0, false).unwrap()[1];
    assert!(close(tl[0].min(bl[0]), b.bbox[0]) && close(tr[1].max(br[1]), b.bbox[3]));
}

#[test]
fn vertical_cid_advances_and_columns_are_measured_in_writing_order() {
    let cmap = stream(
        "",
        "begincmap 1 begincodespacerange <0000> <ffff> endcodespacerange \
        2 beginbfchar <0001> <0041> <0002> <0042> endbfchar endcmap",
    );
    let doc = with_fonts(
        "BT /V 20 Tf 200 700 Td <00010002> Tj -24 0 Td <0002> Tj ET",
        "",
        "",
        "",
        "/V 12 0 R",
        &[
            "<< /Type /Font /Subtype /Type0 /BaseFont /Sans /Encoding /Identity-V /DescendantFonts [13 0 R] /ToUnicode 14 0 R >>",
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 600 /DW2 [880 -1000] /W2 [1 [-800 300 860]] >>",
            &cmap,
        ],
    );
    let glyph = &glyphs(&doc, 0, false).unwrap()[0];
    assert_eq!(glyph.direction, [0.0, -1.0]);
    assert!(close(glyph.origin[0], 200.0) && close(glyph.origin[1], 700.0));
    assert!(close(glyph.end[0], 200.0) && close(glyph.end[1], 684.0));
    for (v, expected) in glyph.bbox.iter().zip([194.0, 684.0, 206.0, 700.0]) {
        assert!(close(*v, expected));
    }
    let w = words(&doc, 0, false).unwrap();
    assert_eq!(
        w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
        ["AB", "B"]
    );
    assert_eq!((w[1].block, w[1].line), (0, 1));
    let lines = &text_blocks(&doc, 0, false).unwrap()[0].lines;
    assert_eq!(lines[0].direction, [0.0, -1.0]);
    let q = search(&doc, 0, "AB", false).unwrap()[0][0];
    for (corner, expected) in q.iter().zip([
        [206.0, 700.0],
        [206.0, 664.0],
        [194.0, 700.0],
        [194.0, 664.0],
    ]) {
        assert!(
            close(corner[0], expected[0]) && close(corner[1], expected[1]),
            "{q:?}"
        );
    }
}

#[test]
fn layout_obeys_crop_without_changing_glyph_visibility_or_coordinates() {
    let doc = page(
        "BT /F2 12 Tf 50 700 Td (outside) Tj ET BT /F2 12 Tf 80 200 Td (inside) Tj ET",
        "/CropBox [70 80 250 300] /Rotate 270",
    );
    assert_eq!(text(&glyphs(&doc, 0, false).unwrap()), "outsideinside");
    let w = words(&doc, 0, false).unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].text, "inside");
    assert!(close(w[0].bbox[0], 80.0));
    assert!(search(&doc, 0, "outside", false).unwrap().is_empty());
    assert!(search(&doc, 1, "", false).is_err());
}

#[test]
fn expansion_by_replacement_is_charged_to_the_document_limit() {
    use sealwax_core::{Error, Limits, SaveOptions};
    let doc = page(
        &format!(
            "BT /F2 12 Tf /Span << /ActualText ({}) >> BDC (x) Tj EMC ET",
            "w".repeat(201)
        ),
        "",
    );
    let mut limits = Limits::default();
    limits.max_page_cost = 200;
    let doc = Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default().limits(limits),
    )
    .unwrap();
    assert!(matches!(
        words(&doc, 0, false),
        Err(Error::LimitExceeded {
            what: "max_page_cost"
        })
    ));
    assert!(matches!(
        search(&doc, 0, "w", false),
        Err(Error::LimitExceeded {
            what: "max_page_cost"
        })
    ));
}

#[test]
fn paragraphs_and_column_restarts_compare_the_starts_of_consecutive_lines() {
    let doc = page(
        "BT /F2 20 Tf 50 700 Td (long first line) Tj 0 -24 Td (short) Tj \
        18 -24 Td (indented) Tj 0 -24 Td (continued) Tj 170 26 Td (next column) Tj ET",
        "",
    );
    let blocks = text_blocks(&doc, 0, false).unwrap();
    assert_eq!(
        blocks.iter().map(|b| b.lines.len()).collect::<Vec<_>>(),
        [2, 2, 1]
    );
    assert_eq!(blocks[1].lines[0].words[0].text, "indented");
    assert_eq!(blocks[2].lines[0].words[0].text, "next");
}

#[test]
fn stroked_text_is_extracted_and_fill_then_stroke_appears_once() {
    let doc = page(
        "BT /F2 20 Tf 50 700 Td (A) Tj 1 Tr (B) Tj (C) Tj 2 Tr (D) Tj ET",
        "",
    );
    assert_eq!(words(&doc, 0, false).unwrap()[0].text, "ABCD");
}
