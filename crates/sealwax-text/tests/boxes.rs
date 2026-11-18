//! Glyph sizes and boxes as MuPDF gives them: the size of an em stretched or squeezed, a box
//! from the font's descent to its ascent. The values expected are those PyMuPDF 1.27.2 gives for
//! the same pages.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, OpenOptions};
use sealwax_text::{TextGlyph, glyphs};

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// One page drawing `content` with the fonts `fonts` (resource entries) and the forms `forms`,
/// its annotations `annots`; `extra` objects from number 5 on.
fn document(content: &str, fonts: &str, forms: &str, annots: &str, extra: &[&str]) -> Document {
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << {fonts} >> /XObject << {forms} >> >> /Annots [{annots}] >>"
            ),
        ),
        (4, stream("", content)),
    ];
    for (at, body) in extra.iter().enumerate() {
        objects.push((5 + at, (*body).to_owned()));
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

/// A font not embedded, named `name`, with `descriptor` (font descriptor entries) when given.
fn font(name: &str, descriptor: Option<&str>) -> String {
    let described = descriptor.map_or(String::new(), |entries| {
        let widths = vec!["600"; 95].join(" ");
        format!(
            " /FontDescriptor << /Type /FontDescriptor /FontName /{name} {entries} >> \
             /FirstChar 32 /LastChar 126 /Widths [{widths}]"
        )
    });
    format!(
        "<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding{described} >>"
    )
}

/// How far the box of `glyph` reaches above and below its origin, and its size.
fn reach(glyph: &TextGlyph) -> (f64, f64, f64) {
    let [_, bottom, _, top] = glyph.bbox;
    (glyph.size, top - glyph.origin[1], glyph.origin[1] - bottom)
}

fn assert_reach(glyph: &TextGlyph, (size, above, below): (f64, f64, f64)) {
    let (found_size, found_above, found_below) = reach(glyph);
    assert!(
        (found_size - size).abs() < 0.01
            && (found_above - above).abs() < 0.01
            && (found_below - below).abs() < 0.01,
        "size {found_size}, above {found_above}, below {found_below}; \
         not {size}, {above}, {below}"
    );
}

#[test]
fn an_em_stretched_or_squeezed_is_drawn_smaller_and_boxes_reach_the_font_s_ascent() {
    // Helvetica not embedded: MuPDF's ascent 1.075 and descent 0.299 em.
    let helvetica = format!("/H {}", font("Helvetica", None));
    for (text_state, expected) in [
        ("1 0 0 1 72 700 Tm", (10.0, 10.75, 2.99)),
        ("0.8 0 0 1 72 700 Tm", (8.944, 10.75, 2.99)),
        ("1 0 0 0.8 72 700 Tm", (8.944, 8.6, 2.392)),
        ("80 Tz 1 0 0 1 72 700 Tm", (8.944, 10.75, 2.99)),
    ] {
        let doc = document(
            &format!("BT /H 10 Tf {text_state} (H) Tj ET"),
            &helvetica,
            "",
            "",
            &[],
        );
        let found = glyphs(&doc, 0, false).unwrap();
        assert_reach(&found[0], expected);
    }
}

#[test]
fn the_ascent_and_descent_come_from_the_descriptor_then_from_the_font_drawn() {
    let fonts = [
        // The descriptor's, each side on its own; a positive descent is read as negative.
        font("A", Some("/Flags 32 /Ascent 900 /Descent -250")),
        font("B", Some("/Flags 32 /Ascent 900 /Descent 250")),
        // Less than an em apart: stretched to an em, in proportion.
        font("C", Some("/Flags 32 /Ascent 400 /Descent -100")),
        // An ascent at 0: the usual 0.8 and 0.2.
        font("D", Some("/Flags 32 /Ascent 0 /Descent -250")),
        // No ascent nor descent, a name MuPDF does not know: the usual.
        font("Verdana", Some("/Flags 32")),
        // No descriptor: the font MuPDF draws it with, a standard one under its other names,
        // else Times-Roman.
        font("Arial,Bold", None),
        font("Verdana", None),
        // Spaces aside.
        font("Times#20New#20Roman", None),
        // The descriptor first, a standard font as well.
        font("Times-Roman", Some("/Flags 34 /Ascent 700 /Descent -100")),
    ];
    let resources: String = fonts
        .iter()
        .enumerate()
        .map(|(i, font)| format!("/F{i} {font} "))
        .collect();
    let content: String = (0..fonts.len())
        .map(|i| format!("BT /F{i} 10 Tf 72 {} Td (H) Tj ET ", 700 - 30 * i))
        .collect();
    let doc = document(&content, &resources, "", "", &[]);
    let found = glyphs(&doc, 0, false).unwrap();
    let expected = [
        (10.0, 9.0, 2.5),
        (10.0, 9.0, 2.5),
        (10.0, 8.0, 2.0),
        (10.0, 8.0, 2.0),
        (10.0, 8.0, 2.0),
        (10.0, 10.7, 3.07),
        (10.0, 10.53, 2.81),
        (10.0, 10.53, 2.81),
        (10.0, 8.75, 1.25),
    ];
    assert_eq!(found.len(), expected.len());
    for (glyph, expected) in found.iter().zip(expected) {
        assert_reach(glyph, expected);
    }
}

#[test]
fn a_font_drawn_with_another_keeps_its_em_and_its_widths() {
    // Verdana, not embedded, drawn with a font of other widths: its glyphs are stretched to
    // the 600 units /Widths gives, its em is not.
    let doc = document(
        "BT /V 10 Tf 72 700 Td (H) Tj ET",
        &format!("/V {}", font("Verdana", Some("/Flags 32"))),
        "",
        "",
        &[],
    );
    let found = glyphs(&doc, 0, false).unwrap();
    assert_reach(&found[0], (10.0, 8.0, 2.0));
    assert!((found[0].end[0] - found[0].origin[0] - 6.0).abs() < 1e-9);
}

#[test]
fn type3_boxes_come_from_the_font_box_else_from_its_glyphs() {
    let type3 = |bbox: &str, matrix: &str| {
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox {bbox} /FontMatrix {matrix} \
             /CharProcs << /H 5 0 R /x 6 0 R >> /Encoding << /Differences [72 /H 120 /x] >> \
             /FirstChar 72 /LastChar 120 /Widths [{}] /Resources << >> >>",
            vec!["600"; 49].join(" ")
        )
    };
    let fonts = format!(
        "/A {} /B {} /C {}",
        type3("[0 -200 600 800]", "[0.001 0 0 0.001 0 0]"),
        type3("[0 -100 300 400]", "[0.002 0 0 0.002 0 0]"),
        // No box: the union of its glyphs' (0 to 700, and -300 to 400).
        type3("[0 0 0 0]", "[0.001 0 0 0.001 0 0]"),
    );
    let doc = document(
        "BT /A 10 Tf 72 700 Td (H) Tj ET BT /B 10 Tf 72 650 Td (H) Tj ET \
         BT /C 10 Tf 72 600 Td (H) Tj ET",
        &fonts,
        "",
        "",
        &[
            &stream("", "600 0 50 0 550 700 d1 50 0 500 700 re f"),
            &stream("", "600 0 50 -300 550 400 d1 50 -300 500 700 re f"),
        ],
    );
    let found = glyphs(&doc, 0, false).unwrap();
    assert_reach(&found[0], (10.0, 8.0, 2.0));
    assert_reach(&found[1], (10.0, 8.0, 2.0));
    assert_reach(&found[2], (10.0, 7.0, 3.0));
}

#[test]
fn glyphs_of_a_form_drawing_with_several_fonts_get_each_their_own() {
    let form = stream(
        &format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 600 800] /Resources << /Font << /A {} /B {} >> >>",
            font("Helvetica", None),
            font("Times-Roman", Some("/Flags 34 /Ascent 700 /Descent -100")),
        ),
        "BT /A 10 Tf 72 700 Td (H) Tj ET BT /B 10 Tf 72 650 Td (H) Tj ET",
    );
    let doc = document("/Fm Do", "", "/Fm 5 0 R", "", &[&form]);
    let found = glyphs(&doc, 0, false).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].font.as_deref(), Some("Helvetica"));
    assert_reach(&found[0], (10.0, 10.75, 2.99));
    assert_eq!(found[1].font.as_deref(), Some("Times-Roman"));
    assert_reach(&found[1], (10.0, 8.75, 1.25));
}

#[test]
fn an_annotation_s_text_has_its_font_and_its_box() {
    let appearance = stream(
        &format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 20] /Resources << /Font << /Helv {} >> >>",
            font("Helvetica", None)
        ),
        "BT /Helv 10 Tf 2 5 Td (H) Tj ET",
    );
    let note = "<< /Type /Annot /Subtype /FreeText /Rect [100 500 300 520] /F 4 \
                /DA (/Helv 10 Tf) /AP << /N 6 0 R >> >>";
    let doc = document("", "", "", "5 0 R", &[note, &appearance]);
    let found = glyphs(&doc, 0, true).unwrap();
    assert_eq!(found.len(), 1);
    let glyph = &found[0];
    assert!(glyph.annotation);
    assert_eq!(glyph.origin, [102.0, 505.0]);
    assert_eq!(glyph.font.as_deref(), Some("Helvetica"));
    assert_reach(glyph, (10.0, 10.75, 2.99));
}
