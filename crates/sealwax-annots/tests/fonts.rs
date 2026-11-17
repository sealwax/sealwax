//! Free texts drawn with the TrueType fonts a caller gives: chosen by the font the XFDF names,
//! embedded as a subset, read back as the characters typed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use read_fonts::TableProvider as _;
use sealwax_annots::{ApplyOptions, TextFonts, apply_xfdf_with};
use sealwax_core::{Document, Error as PdfError, Object, OpenOptions, SaveOptions};

// --- a TrueType program made here ---------------------------------------------------------------

fn be16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn be32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// A TrueType program named `name` whose glyph `i + 1` is a box of width `500 + 10 i` for
/// `characters[i]`, glyph 0 its `.notdef`. Advances differ, so that widths tell glyphs apart.
fn program(name: &str, characters: &[char]) -> Vec<u8> {
    let mut unique: Vec<char> = Vec::new();
    for c in characters {
        if !unique.contains(c) {
            unique.push(*c);
        }
    }
    let characters = unique.as_slice();
    let count = u16::try_from(characters.len() + 1).unwrap();
    let advance = |glyph: u16| {
        if glyph == 0 {
            500
        } else {
            500 + 10 * (glyph - 1)
        }
    };
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for glyph in 0..count {
        be32(&mut loca, u32::try_from(glyf.len()).unwrap());
        let width = advance(glyph) - 100;
        // One contour, a box from (50, 0) to (50 + width, 700), corners on the curve.
        let mut g = Vec::new();
        for value in [1u16, 50, 0, 50 + width, 700, 3, 0] {
            be16(&mut g, value);
        }
        g.extend_from_slice(&[0x01; 4]);
        for dx in [
            50i16,
            i16::try_from(width).unwrap(),
            0,
            -i16::try_from(width).unwrap(),
        ] {
            g.extend_from_slice(&dx.to_be_bytes());
        }
        for dy in [0i16, 0, 700, 0] {
            g.extend_from_slice(&dy.to_be_bytes());
        }
        while g.len() % 4 != 0 {
            g.push(0);
        }
        glyf.extend_from_slice(&g);
    }
    be32(&mut loca, u32::try_from(glyf.len()).unwrap());

    let mut head = Vec::new();
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0);
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, 0);
    be16(&mut head, 1000);
    head.extend_from_slice(&[0; 16]);
    for value in [0i16, -200, 1000, 800] {
        head.extend_from_slice(&value.to_be_bytes());
    }
    be16(&mut head, 0);
    be16(&mut head, 8);
    be16(&mut head, 2);
    be16(&mut head, 1);
    be16(&mut head, 0);

    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    for value in [800i16, -200, 0] {
        hhea.extend_from_slice(&value.to_be_bytes());
    }
    be16(&mut hhea, advance(count - 1));
    hhea.extend_from_slice(&[0; 22]);
    be16(&mut hhea, count);

    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0000_5000);
    be16(&mut maxp, count);

    let mut hmtx = Vec::new();
    for glyph in 0..count {
        be16(&mut hmtx, advance(glyph));
        be16(&mut hmtx, 50);
    }

    // cmap format 4: a segment for each character, then the closing one.
    let mut codes: Vec<(u16, u16)> = characters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            (
                u16::try_from(u32::from(*c)).unwrap(),
                u16::try_from(i + 1).unwrap(),
            )
        })
        .collect();
    codes.sort_unstable();
    let segments = u16::try_from(codes.len() + 1).unwrap();
    let mut sub = Vec::new();
    be16(&mut sub, 4);
    be16(&mut sub, 16 + 8 * segments);
    be16(&mut sub, 0);
    be16(&mut sub, 2 * segments);
    let power = 15 - segments.leading_zeros() as u16;
    be16(&mut sub, 2 * (1 << power));
    be16(&mut sub, power);
    be16(&mut sub, 2 * segments - 2 * (1 << power));
    for (code, _) in &codes {
        be16(&mut sub, *code);
    }
    be16(&mut sub, 0xFFFF);
    be16(&mut sub, 0);
    for (code, _) in &codes {
        be16(&mut sub, *code);
    }
    be16(&mut sub, 0xFFFF);
    for (code, glyph) in &codes {
        be16(&mut sub, glyph.wrapping_sub(*code));
    }
    be16(&mut sub, 1);
    for _ in 0..segments {
        be16(&mut sub, 0);
    }
    let mut cmap = Vec::new();
    be16(&mut cmap, 0);
    be16(&mut cmap, 1);
    be16(&mut cmap, 3);
    be16(&mut cmap, 1);
    be32(&mut cmap, 12);
    cmap.extend_from_slice(&sub);

    // name: the PostScript name (6), Windows Unicode.
    let encoded: Vec<u8> = name.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut name_table = Vec::new();
    for value in [
        0u16,
        1,
        18,
        3,
        1,
        0x409,
        6,
        u16::try_from(encoded.len()).unwrap(),
        0,
    ] {
        be16(&mut name_table, value);
    }
    name_table.extend_from_slice(&encoded);

    let mut post = Vec::new();
    be32(&mut post, 0x0003_0000);
    post.extend_from_slice(&[0; 28]);

    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
        (b"name", name_table),
        (b"post", post),
    ];
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, u16::try_from(tables.len()).unwrap());
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, table) in &tables {
        out.extend_from_slice(*tag);
        be32(&mut out, 0);
        be32(&mut out, u32::try_from(offset).unwrap());
        be32(&mut out, u32::try_from(table.len()).unwrap());
        body.extend_from_slice(table);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() + body.len();
    }
    out.extend_from_slice(&body);
    out
}

// --- the documents ------------------------------------------------------------------------------

fn page() -> Document {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
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

fn free_text(rect: &str, text: &str, font: &str) -> String {
    format!(
        "<freetext page=\"0\" rect=\"{rect}\" flags=\"print,readonly,locked\" width=\"0\" FontSize=\"14\">\
         <contents>{text}</contents><defaultappearance>0 0 0 rg /{font} 14 Tf</defaultappearance>\
         <defaultstyle>font: {font} 14pt; text-align: left; color: #000000</defaultstyle></freetext>"
    )
}

fn exported(annotations: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><xfdf xmlns=\"http://ns.adobe.com/xfdf/\" \
         xml:space=\"preserve\"><fields /><add>{}</add><modify /><delete /></xfdf>",
        annotations.concat()
    )
}

fn saved(doc: &Document) -> Document {
    Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap()
}

/// The text page 0 shows, and the fonts that show it.
fn shown(doc: &Document) -> (String, Vec<String>) {
    let glyphs = sealwax_text::glyphs(doc, 0, false).unwrap();
    let text = glyphs.iter().filter_map(|g| g.text.clone()).collect();
    let mut fonts: Vec<String> = glyphs.iter().filter_map(|g| g.font.clone()).collect();
    fonts.dedup();
    (text, fonts)
}

const LATIN: &str = "Łukasz Wójcik?";
const SCRIPT: &str = "Jean Dupont";

#[test]
fn a_default_font_draws_every_character_it_has_and_is_embedded() {
    let latin: Vec<char> = LATIN.chars().collect();
    let fonts = TextFonts::new().default_font(program("Latin-Regular", &latin));
    let mut doc = page();
    let xfdf = exported(&[free_text("100,600,400,630", "Łukasz Wójcik", "Helvetica")]);
    let applied =
        apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new().fonts(fonts), |_| None).unwrap();
    assert_eq!((applied.texts, applied.characters_replaced), (1, 0));
    let doc = saved(&doc);
    let (text, fonts) = shown(&doc);
    assert_eq!(text, "Łukasz Wójcik");
    assert_eq!(fonts.len(), 1);
    assert!(fonts[0].ends_with("+Latin-Regular"), "{fonts:?}");
    assert_eq!(fonts[0].find('+'), Some(6));
}

#[test]
fn the_font_the_xfdf_names_is_taken_and_others_take_the_default() {
    let script: Vec<char> = SCRIPT.chars().collect();
    let latin: Vec<char> = LATIN.chars().collect();
    let fonts = TextFonts::new()
        .named("caveat", program("Caveat-Regular", &script))
        .default_font(program("Latin-Regular", &latin));
    let mut doc = page();
    let xfdf = exported(&[
        free_text("100,600,400,630", SCRIPT, "Caveat"),
        free_text("100,500,400,530", "Łukasz", "Helvetica"),
    ]);
    let applied =
        apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new().fonts(fonts), |_| None).unwrap();
    assert_eq!((applied.texts, applied.characters_replaced), (2, 0));
    let doc = saved(&doc);
    let (text, fonts) = shown(&doc);
    assert_eq!(text, "Jean DupontŁukasz");
    assert!(fonts[0].ends_with("+Caveat-Regular"), "{fonts:?}");
    assert!(fonts[1].ends_with("+Latin-Regular"), "{fonts:?}");
}

#[test]
fn a_character_the_font_lacks_is_drawn_as_its_question_mark_and_counted() {
    let fonts = TextFonts::new().default_font(program("Latin-Regular", &['a', 'b', '?', ' ']));
    let mut doc = page();
    let xfdf = exported(&[free_text("100,600,400,630", "ab ŁŁ", "Helvetica")]);
    let applied =
        apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new().fonts(fonts), |_| None).unwrap();
    assert_eq!(applied.characters_replaced, 2);
    assert_eq!(shown(&saved(&doc)).0, "ab ??");
}

#[test]
fn without_fonts_texts_stay_in_helvetica() {
    let mut doc = page();
    let xfdf = exported(&[free_text("100,600,400,630", "Łukasz", "Caveat")]);
    let applied = apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new(), |_| None).unwrap();
    assert_eq!(applied.characters_replaced, 1);
    let (text, fonts) = shown(&saved(&doc));
    assert_eq!(text, "?ukasz");
    assert_eq!(fonts, ["Helvetica"]);
}

#[test]
fn a_font_that_is_no_truetype_program_is_refused_before_anything_changes() {
    let mut doc = page();
    let xfdf = exported(&[free_text("100,600,400,630", "a", "Helvetica")]);
    let fonts = TextFonts::new().default_font(b"not a font".to_vec());
    let refused = apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new().fonts(fonts), |_| None);
    assert!(matches!(
        refused,
        Err(sealwax_annots::Error::Pdf(PdfError::InvalidArgument {
            what: "font"
        }))
    ));
    assert!(!doc.is_modified());
}

#[test]
fn the_program_embedded_keeps_the_glyphs_used_only_and_its_widths_are_the_program_s() {
    let latin: Vec<char> = LATIN.chars().collect();
    let program = program("Latin-Regular", &latin);
    let fonts = TextFonts::new().default_font(program.clone());
    let mut doc = page();
    let xfdf = exported(&[free_text("100,600,400,630", "ók", "Helvetica")]);
    apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new().fonts(fonts), |_| None).unwrap();
    let doc = saved(&doc);
    // Find the CID font and its program.
    let mut cid_font = None;
    for number in 1..200 {
        if let Ok(Some(Object::Dict(dict))) = doc.object(sealwax_core::ObjId {
            number,
            generation: 0,
        }) && dict.get(b"Subtype").and_then(Object::as_name) == Some(b"CIDFontType2")
        {
            cid_font = Some(dict);
        }
    }
    let cid_font = cid_font.expect("a CIDFontType2");
    // ó (glyph 9, advance 580) is drawn first, so it is code 1 of the subset; k (glyph 3,
    // advance 520) code 2.
    let widths = cid_font.get(b"W").and_then(Object::as_array).unwrap();
    let pairs: Vec<(i64, f64)> = widths
        .chunks(2)
        .map(|pair| {
            (
                pair[0].as_i64().unwrap(),
                pair[1].as_array().unwrap()[0].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(pairs, [(1, 580.0), (2, 520.0)]);
    // The program embedded: .notdef and the glyphs used, with their outlines, nothing else.
    let descriptor = cid_font
        .get(b"FontDescriptor")
        .and_then(Object::as_ref_id)
        .unwrap();
    let Some(Object::Dict(descriptor)) = doc.object(descriptor).unwrap() else {
        panic!("a descriptor");
    };
    let file = descriptor
        .get(b"FontFile2")
        .and_then(Object::as_ref_id)
        .unwrap();
    let Some(Object::Stream(file)) = doc.object(file).unwrap() else {
        panic!("a program");
    };
    let mut embedded = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::ZlibDecoder::new(file.data.as_slice()),
        &mut embedded,
    )
    .unwrap();
    assert!(embedded.len() < program.len());
    let font = read_fonts::FontRef::new(&embedded).unwrap();
    assert_eq!(font.maxp().unwrap().num_glyphs(), 3);
    let long = font.head().unwrap().index_to_loc_format() == 1;
    let loca = font
        .table_data(read_fonts::types::Tag::new(b"loca"))
        .unwrap();
    let offsets: Vec<u32> = if long {
        loca.as_bytes()
            .chunks(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    } else {
        loca.as_bytes()
            .chunks(2)
            .map(|b| 2 * u32::from(u16::from_be_bytes([b[0], b[1]])))
            .collect()
    };
    let with_outline: Vec<usize> = (0..offsets.len() - 1)
        .filter(|g| offsets[g + 1] > offsets[*g])
        .collect();
    assert_eq!(with_outline, [0, 1, 2]);
}

#[test]
fn helvetica_bold_is_drawn_in_helvetica_bold_with_its_widths() {
    let mut doc = page();
    let xfdf = exported(&[
        free_text("100,600,400,630", "Bold", "Helvetica-Bold"),
        free_text("100,500,400,530", "Bold", "Helvetica"),
    ]);
    let applied = apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new(), |_| None).unwrap();
    assert_eq!(applied.characters_replaced, 0);
    let doc = saved(&doc);
    let glyphs = sealwax_text::glyphs(&doc, 0, false).unwrap();
    let fonts: Vec<&str> = glyphs.iter().filter_map(|g| g.font.as_deref()).collect();
    assert_eq!(
        fonts,
        ["Helvetica-Bold"; 4]
            .iter()
            .chain(&["Helvetica"; 4])
            .copied()
            .collect::<Vec<_>>()
    );
    // "Bold" at 14 points: 722 + 611 + 278 + 611 in Helvetica-Bold, 667 + 556 + 222 + 556 in
    // Helvetica, thousandths of the size.
    let width = |range: std::ops::Range<usize>| {
        glyphs[range.end - 1].end[0] - glyphs[range.start].origin[0]
    };
    assert!((width(0..4) - 2.222 * 14.0).abs() < 1e-6, "{}", width(0..4));
    assert!((width(4..8) - 2.001 * 14.0).abs() < 1e-6, "{}", width(4..8));
    // A program given for Helvetica-Bold takes its place.
    let fonts = TextFonts::new().named(
        "Helvetica-Bold",
        program("Bold-Font", &['B', 'o', 'l', 'd']),
    );
    let mut doc = page();
    apply_xfdf_with(&mut doc, &xfdf, &ApplyOptions::new().fonts(fonts), |_| None).unwrap();
    let (text, fonts) = shown(&saved(&doc));
    assert_eq!(text, "BoldBold");
    assert!(
        fonts[0].ends_with("+Bold-Font") && fonts[1] == "Helvetica",
        "{fonts:?}"
    );
}
