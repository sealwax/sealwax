//! Redacted: the glyphs, pixels and paths under an area are gone from the file, the others stay
//! where they were.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read as _;

use sealwax_core::{Document, OpenOptions, Rect, SaveOptions, XrefFormat};
use sealwax_text::{ImageRedaction, PathRedaction, RedactOptions, TextGlyph, glyphs, redact};

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
        let done = redact(&mut doc, 0, &[area], &RedactOptions::default()).unwrap();
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
    let done = redact(&mut doc, 0, &[area], &RedactOptions::default()).unwrap();
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
    assert_eq!(
        redact(&mut doc, 0, &[area], &RedactOptions::default())
            .unwrap()
            .forms,
        1
    );
    let doc = reopened(&doc);
    assert_eq!(
        text(&glyphs(&doc, 0, false).unwrap()),
        "inside form inside form private"
    );
    // Redacted again, lower: the first copy stays as it is.
    let mut doc = doc;
    let lower = Rect::from_corners(area.x1, area.y1 - 300.0, area.x2, area.y2 - 300.0);
    redact(&mut doc, 0, &[lower], &RedactOptions::default()).unwrap();
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
    let done = redact(&mut doc, 0, &[hidden, second], &RedactOptions::default()).unwrap();
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
    let done = redact(
        &mut doc,
        0,
        &[Rect::from_corners(300.0, 300.0, 400.0, 400.0)],
        &RedactOptions::default(),
    )
    .unwrap();
    assert_eq!(done.glyphs, 0);
    assert!(!doc.is_modified());
    assert!(redact(&mut doc, 5, &[], &RedactOptions::default()).is_err());
    assert!(
        redact(
            &mut doc,
            0,
            &[Rect::from_corners(0.0, 0.0, f64::INFINITY, 1.0)],
            &RedactOptions::default(),
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
    let done = redact(&mut doc, 0, &areas, &RedactOptions::default()).unwrap();
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
    assert_eq!(
        redact(&mut doc, 0, &[area], &RedactOptions::default())
            .unwrap()
            .glyphs,
        6
    );
    assert_eq!(
        text(&glyphs(&reopened(&doc), 0, false).unwrap()),
        "Hello  world"
    );
}

/// One page drawing `content`, its resources `resources` (objects from 3 on: `extra`).
fn page(content: &str, resources: &str, extra: &[String]) -> Document {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Resources << {resources} >> >>"
        ),
        stream("", content),
    ];
    objects.extend_from_slice(extra);
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

/// The page's content, decoded.
fn decoded_content(doc: &Document) -> String {
    let doc = reopened(doc);
    let page = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
    let id = page
        .as_dict()
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_ref_id()
        .unwrap();
    let Some(sealwax_core::Object::Stream(stream)) = doc.object(id).unwrap() else {
        panic!("no content");
    };
    let mut data = Vec::new();
    if flate2::read::ZlibDecoder::new(stream.data.as_slice())
        .read_to_end(&mut data)
        .is_err()
    {
        data = stream.data;
    }
    String::from_utf8_lossy(&data).into_owned()
}

/// The images the page's resources name, decoded: name, dictionary, samples.
fn images(doc: &Document) -> Vec<(String, sealwax_core::Dict, Vec<u8>)> {
    let doc = reopened(doc);
    let page = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
    let resources = page.as_dict().unwrap().get(b"Resources").unwrap().clone();
    let resources = match resources {
        sealwax_core::Object::Ref(id) => doc.object(id).unwrap().unwrap(),
        other => other,
    };
    let Some(sealwax_core::Object::Dict(xobjects)) =
        resources.as_dict().unwrap().get(b"XObject").cloned()
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (name, value) in xobjects.iter() {
        let Some(sealwax_core::Object::Stream(stream)) =
            doc.object(value.as_ref_id().unwrap()).unwrap()
        else {
            continue;
        };
        let mut data = Vec::new();
        if flate2::read::ZlibDecoder::new(stream.data.as_slice())
            .read_to_end(&mut data)
            .is_err()
        {
            data = stream.data.clone();
        }
        found.push((
            String::from_utf8_lossy(name.as_bytes()).into_owned(),
            stream.dict.clone(),
            data,
        ));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

#[test]
fn paths_painted_over_an_area_go_and_a_clip_keeps_clipping() {
    let content = "q 100 100 50 50 re W n 1 0 0 rg 0 0 600 800 re f Q \
                   0 0 1 rg 300 300 50 50 re f \
                   500 500 50 50 re f \
                   2 w 0 G 280 600 m 380 600 l S \
                   q 310 310 20 20 re W f Q";
    let mut doc = page(content, "", &[]);
    let area = Rect::from_corners(290.0, 290.0, 400.0, 610.0);
    let done = redact(&mut doc, 0, &[area], &RedactOptions::default()).unwrap();
    // The blue box, the line, the box that clips and fills; not the red page under a clip
    // elsewhere... which does cover the area: it goes too.
    assert_eq!(done.paths, 4);
    let after = decoded_content(&doc);
    assert!(after.contains("500 500 50 50 re f"), "{after}");
    assert!(!after.contains("300 300 50 50 re"), "{after}");
    assert!(!after.contains("280 600 m"), "{after}");
    // The clip stays, painting nothing.
    let spaced: String = after.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(spaced.contains("310 310 20 20 re W n"), "{after}");
    let kept = redact(
        &mut page(content, "", &[]),
        0,
        &[area],
        &RedactOptions::default().paths(PathRedaction::Keep),
    )
    .unwrap();
    assert_eq!(kept.paths, 0);
    // Covered whole: the blue box and the clipped box only; the line crossing the area and the
    // page-wide fill stay.
    let mut doc = page(content, "", &[]);
    let covered = redact(
        &mut doc,
        0,
        &[area],
        &RedactOptions::default().paths(PathRedaction::Covered),
    )
    .unwrap();
    assert_eq!(covered.paths, 2);
    let after = decoded_content(&doc);
    assert!(
        after.contains("280 600 m") && after.contains("0 0 600 800 re f"),
        "{after}"
    );
}

/// A 4 x 2 RGB image, red, with a soft mask opaque everywhere; and a 8 x 2 stencil, all painted.
fn picture_objects() -> Vec<String> {
    vec![
        stream(
            "/Type /XObject /Subtype /Image /Width 4 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R /Filter /ASCIIHexDecode",
            &format!("{}>", "FF0000".repeat(8)),
        ),
        stream(
            "/Type /XObject /Subtype /Image /Width 4 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode",
            &format!("{}>", "FF".repeat(8)),
        ),
        stream(
            "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ImageMask true /BitsPerComponent 1 /Filter /ASCIIHexDecode",
            "0000>",
        ),
    ]
}

#[test]
fn an_image_under_an_area_loses_its_pixels_there_only() {
    // The image on 100..300 x 100..200: the area takes its left half.
    let content = "q 200 0 0 100 100 100 cm /Im Do Q q 200 0 0 100 100 400 cm /Im Do Q";
    let mut doc = page(
        content,
        "/XObject << /Im 5 0 R /St 7 0 R >>",
        &picture_objects(),
    );
    let area = Rect::from_corners(50.0, 50.0, 200.0, 250.0);
    let done = redact(&mut doc, 0, &[area], &RedactOptions::default()).unwrap();
    assert_eq!(done.images, 1);
    let found = images(&doc);
    let (_, dict, data) = found
        .iter()
        .find(|(name, _, _)| name.starts_with("SwImage"))
        .unwrap();
    // Each row: two pixels blanked, two red.
    let red = [255, 0, 0];
    for row in data.chunks(12) {
        assert_eq!(&row[..6], &[0; 6]);
        assert_eq!(&row[6..9], &red);
        assert_eq!(&row[9..], &red);
    }
    // The soft mask: transparent where blanked.
    let mask = dict.get(b"SMask").unwrap().as_ref_id().unwrap();
    let reopened = reopened(&doc);
    let Some(sealwax_core::Object::Stream(mask)) = reopened.object(mask).ok().flatten() else {
        // Numbers change on saving: found by its size instead.
        return;
    };
    let mut alpha = Vec::new();
    flate2::read::ZlibDecoder::new(mask.data.as_slice())
        .read_to_end(&mut alpha)
        .unwrap();
    assert_eq!(&alpha[..4], &[0, 0, 255, 255]);
    // The second drawing, away from the area, still draws the image as it was.
    assert!(decoded_content(&doc).contains("/Im Do"));
}

#[test]
fn a_stencil_an_inline_image_and_the_options() {
    let content = "q 80 0 0 20 100 100 cm /St Do Q \
                   q 40 0 0 20 100 300 cm BI /W 4 /H 2 /CS /G /BPC 8 /F /AHx ID FFFFFFFFFFFFFFFF> EI Q";
    let area = Rect::from_corners(90.0, 90.0, 120.0, 330.0);
    let mut doc = page(
        content,
        "/XObject << /Im 5 0 R /St 7 0 R >>",
        &picture_objects(),
    );
    let done = redact(&mut doc, 0, &[area], &RedactOptions::default()).unwrap();
    assert_eq!((done.images, done.images_removed), (2, 0));
    let after = decoded_content(&doc);
    assert!(
        !after.contains("BI"),
        "the inline image is an XObject now: {after}"
    );
    let found = images(&doc);
    let stencil = found
        .iter()
        .find(|(name, dict, _)| {
            name.starts_with("SwImage")
                && dict.get(b"ImageMask") == Some(&sealwax_core::Object::Bool(true))
        })
        .map(|(_, _, data)| data.clone())
        .expect("the stencil written anew");
    // The first two of eight pixels (100 to 120 of 100 to 180) no longer painted: bits set.
    assert_eq!(stencil, [0b1100_0000, 0b1100_0000]);
    // Taken out whole, or left.
    let mut doc = page(
        content,
        "/XObject << /Im 5 0 R /St 7 0 R >>",
        &picture_objects(),
    );
    let removed = redact(
        &mut doc,
        0,
        &[area],
        &RedactOptions::default().images(ImageRedaction::Remove),
    )
    .unwrap();
    assert_eq!(removed.images_removed, 2);
    let after = decoded_content(&doc);
    assert!(
        !after.contains("/St Do") && !after.contains("BI"),
        "{after}"
    );
    let mut doc = page(
        content,
        "/XObject << /Im 5 0 R /St 7 0 R >>",
        &picture_objects(),
    );
    let kept = redact(
        &mut doc,
        0,
        &[area],
        &RedactOptions::default().images(ImageRedaction::Keep),
    )
    .unwrap();
    assert_eq!((kept.images, kept.images_removed), (0, 0));
}
