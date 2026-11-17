//! Text redacted: the glyphs under an area are gone from the file, the others stay where they
//! were.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read as _;

use sealwax_core::{Document, OpenOptions, Rect, SaveOptions, XrefFormat};
use sealwax_text::{TextGlyph, glyphs, redact_text};

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// Pages 3 and 4 drawing `first` and `second`; resources: /F1 Helvetica, /F2 a composite font
/// (Identity-H, widths in `/W`, not embedded), /Fm a form drawing "inside form private" with
/// Helvetica; optional content /oc1, turned off. `catalog` goes into the catalog.
fn document(first: &str, second: &str) -> Document {
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [12 0 R] /D << /OFF [12 0 R] >> >> >>"
            .to_owned(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /Resources 7 0 R /MediaBox [0 0 600 800] >>"
            .to_owned(),
        "<< /Type /Page /Parent 2 0 R /Contents 5 0 R >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /Contents 6 0 R >>".to_owned(),
        stream("", first),
        stream("", second),
        "<< /Font << /F1 8 0 R /F2 9 0 R >> /XObject << /Fm 11 0 R >> /Properties << /oc1 12 0 R >> >>"
            .to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
        "<< /Type /Font /Subtype /Type0 /BaseFont /Sans /Encoding /Identity-H /DescendantFonts [10 0 R] >>"
            .to_owned(),
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sans /CIDSystemInfo << /Registry (Adobe) \
         /Ordering (Identity) /Supplement 0 >> /DW 500 /W [1 [600 700 800]] >>"
            .to_owned(),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 600 800] /Resources << /Font << /H 8 0 R >> >>",
            "BT /H 12 Tf 100 500 Td (inside form private) Tj ET",
        ),
        "<< /Type /OCG /Name (hidden) >>".to_owned(),
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

fn reopened(doc: &Document) -> Document {
    Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap()
}

/// The file as a parser reads it: its bytes, every stream inflated.
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

fn text(found: &[TextGlyph]) -> String {
    found.iter().filter_map(|g| g.text.clone()).collect()
}

/// The area over the glyphs of `word` on page `page`, a tenth of a point inside them.
fn area_over(doc: &Document, page: usize, word: &str) -> Rect {
    let found = glyphs(doc, page, false).unwrap();
    let all = text(&found);
    let start = all
        .find(word)
        .unwrap_or_else(|| panic!("{word} not in {all}"));
    let chosen = &found[start..start + word.len()];
    let x1 = chosen.iter().map(|g| g.bbox[0]).fold(f64::MAX, f64::min);
    let y1 = chosen.iter().map(|g| g.bbox[1]).fold(f64::MAX, f64::min);
    let x2 = chosen.iter().map(|g| g.bbox[2]).fold(f64::MIN, f64::max);
    let y2 = chosen.iter().map(|g| g.bbox[3]).fold(f64::MIN, f64::max);
    Rect::from_corners(x1 + 0.1, y1 + 0.1, x2 - 0.1, y2 - 0.1)
}

/// The origins of the glyphs of `word` on page `page`.
fn origins(doc: &Document, page: usize, word: &str) -> Vec<[f64; 2]> {
    let found = glyphs(doc, page, false).unwrap();
    let all = text(&found);
    let start = all
        .find(word)
        .unwrap_or_else(|| panic!("{word} not in {all}"));
    found[start..start + word.len()]
        .iter()
        .map(|g| g.origin)
        .collect()
}

fn same_places(a: &[[f64; 2]], b: &[[f64; 2]]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(p, q)| (p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3)
}

#[test]
fn a_word_redacted_is_gone_and_what_follows_does_not_move() {
    let shows = [
        "BT /F1 12 Tf 72 700 Td (Hello secret world) Tj ET",
        "BT /F1 12 Tf 72 700 Td [(Hel) -20 (lo sec) 30.5 (ret wor) -40 (ld)] TJ ET",
        "BT /F1 12 Tf 14 TL 2 Tc 3 Tw 72 714 Td (x) Tj (Hello secret world) ' ET",
        "BT /F1 12 Tf 14 TL 72 714 Td (x) Tj 4 1.5 (Hello secret world) \" ET",
        // Turned a quarter, scaled twice, stretched by a fifth.
        "q 2 0 0 2 0 0 cm BT /F1 6 Tf 0 1 -1 0 150 50 Tm 120 Tz (Hello secret world) Tj ET Q",
    ];
    for content in shows {
        let mut doc = document(content, "");
        let before = origins(&doc, 0, "world");
        let area = area_over(&doc, 0, "secret");
        let done = redact_text(&mut doc, 0, &[area]).unwrap();
        assert_eq!(done.glyphs, 6, "{content}");
        let doc = reopened(&doc);
        let after = text(&glyphs(&doc, 0, false).unwrap());
        assert!(
            !after.contains("secret") && after.contains("Hello"),
            "{content}: {after}"
        );
        assert!(
            same_places(&before, &origins(&doc, 0, "world")),
            "{content}: the rest stays where it was"
        );
        assert!(find(&everything(&doc), b"secret").is_none(), "{content}");
    }
}

#[test]
fn a_form_loses_its_text_on_the_page_redacted_only() {
    let mut doc = document("/Fm Do", "/Fm Do");
    let area = area_over(&doc, 0, "private");
    let done = redact_text(&mut doc, 0, &[area]).unwrap();
    assert_eq!((done.glyphs, done.forms), (7, 1));
    let doc = reopened(&doc);
    assert_eq!(text(&glyphs(&doc, 0, false).unwrap()), "inside form ");
    assert_eq!(
        text(&glyphs(&doc, 1, false).unwrap()),
        "inside form private"
    );
}

#[test]
fn a_form_drawn_twice_loses_its_text_where_the_area_is_only_and_again() {
    // The form drawn at its place, then lowered by 300 points: only the first drawing is under
    // the area.
    let content = "/Fm Do q 1 0 0 1 0 -300 cm /Fm Do Q";
    let mut doc = document(content, "");
    let area = area_over(&doc, 0, "private");
    assert_eq!(redact_text(&mut doc, 0, &[area]).unwrap().forms, 1);
    let doc = reopened(&doc);
    assert_eq!(
        text(&glyphs(&doc, 0, false).unwrap()),
        "inside form inside form private"
    );
    // Redacted again, lower: the first copy stays as it is.
    let mut doc = doc;
    let lower = Rect::from_corners(area.x1, area.y1 - 300.0, area.x2, area.y2 - 300.0);
    redact_text(&mut doc, 0, &[lower]).unwrap();
    let doc = reopened(&doc);
    assert_eq!(
        text(&glyphs(&doc, 0, false).unwrap()),
        "inside form inside form "
    );
}

#[test]
fn hidden_text_and_a_composite_font_are_redacted_too() {
    // Under an area: text in a layer turned off, and codes of a composite font (two bytes each,
    // 600, 700 and 800 wide) of which the second is covered.
    let content = "/OC /oc1 BDC BT /F1 12 Tf 72 600 Td (hidden secret) Tj ET EMC \
                   BT /F2 10 Tf 72 500 Td <000100020003> Tj ET";
    let mut doc = document(content, "");
    let hidden = Rect::from_corners(60.0, 590.0, 200.0, 620.0);
    // The second code spans 78 to 85 (6 points for the first, 7 for itself).
    let second = Rect::from_corners(79.0, 495.0, 84.0, 510.0);
    let done = redact_text(&mut doc, 0, &[hidden, second]).unwrap();
    assert_eq!(done.glyphs, 14);
    let doc = reopened(&doc);
    let all = everything(&doc);
    assert!(find(&all, b"secret").is_none());
    // The third code kept where it was: 72 + 6 + 7.
    let content = String::from_utf8_lossy(&all).into_owned();
    assert!(content.contains("<0001> -700 <0003>"), "{content}");
}

#[test]
fn nothing_under_the_areas_changes_nothing() {
    let content = "BT /F1 12 Tf 72 700 Td (Hello) Tj ET";
    let mut doc = document(content, "");
    let done = redact_text(
        &mut doc,
        0,
        &[Rect::from_corners(300.0, 300.0, 400.0, 400.0)],
    )
    .unwrap();
    assert_eq!(done.glyphs, 0);
    assert!(!doc.is_modified());
    assert!(redact_text(&mut doc, 5, &[]).is_err());
    assert!(
        redact_text(
            &mut doc,
            0,
            &[Rect::from_corners(0.0, 0.0, f64::INFINITY, 1.0)]
        )
        .is_err()
    );
}

#[test]
fn a_turned_line_loses_only_what_the_area_covers() {
    // Turned about 24 degrees: an area on the middle of "secret" takes its glyphs whose boxes it
    // reaches, not the neighbours a box upright around them would.
    let content = "BT /F1 12 Tf 0.9135 0.4067 -0.4067 0.9135 72 500 Tm (Hello secret world) Tj ET";
    let mut doc = document(content, "");
    let found = glyphs(&doc, 0, false).unwrap();
    let all = text(&found);
    let start = all.find("secret").unwrap();
    // A small square on each glyph's middle, a fifth of an em above its baseline.
    let areas: Vec<Rect> = found[start..start + 6]
        .iter()
        .map(|g| {
            let middle = [
                (g.origin[0] + g.end[0]) / 2.0,
                (g.origin[1] + g.end[1]) / 2.0,
            ];
            let lift = [-g.direction[1] * 2.4, g.direction[0] * 2.4];
            let (x, y) = (middle[0] + lift[0], middle[1] + lift[1]);
            Rect::from_corners(x - 0.5, y - 0.5, x + 0.5, y + 0.5)
        })
        .collect();
    let done = redact_text(&mut doc, 0, &areas).unwrap();
    assert_eq!(done.glyphs, 6);
    let after = text(&glyphs(&reopened(&doc), 0, false).unwrap());
    assert_eq!(after, "Hello  world");
}

#[test]
fn a_document_changed_since_it_was_opened_is_redacted_as_it_stands() {
    let mut doc = document("BT /F1 12 Tf 72 700 Td (old) Tj ET", "");
    // The page's content replaced, unsaved: the redaction reads the new one.
    let page = doc.page_ids()[0].unwrap();
    let content = doc
        .add_object(sealwax_core::Object::Stream(sealwax_core::Stream {
            dict: sealwax_core::Dict::new(),
            data: b"BT /F1 12 Tf 72 700 Td (Hello secret world) Tj ET".to_vec(),
        }))
        .unwrap();
    let Some(sealwax_core::Object::Dict(mut dict)) = doc.object(page).unwrap() else {
        panic!("no page");
    };
    dict.insert("Contents", sealwax_core::Object::Ref(content));
    doc.set_object(page, sealwax_core::Object::Dict(dict));
    let area = area_over(&reopened(&doc), 0, "secret");
    assert_eq!(redact_text(&mut doc, 0, &[area]).unwrap().glyphs, 6);
    assert_eq!(
        text(&glyphs(&reopened(&doc), 0, false).unwrap()),
        "Hello  world"
    );
}
