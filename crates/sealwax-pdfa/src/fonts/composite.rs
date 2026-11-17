//! A composite font without a program gets one: the standard font that stands in for it, its
//! glyphs put in the order of the font's CIDs.
//!
//! A CIDFont's CFF program without CIDFont operators has its CIDs read as glyph numbers
//! (ISO 32000-1, 9.7.4.2): glyph `n` of the program written draws what CID `n` means, read from
//! the font's `ToUnicode` CMap, with the width `/W` gives it, so that no glyph moves. Only fonts
//! encoded `Identity-H` or `Identity-V` are given one: their codes are their CIDs. A font keeps
//! none, and stays `font_not_embedded`, when its CIDs cannot be read as text (no `ToUnicode`) or
//! one it shows has no glyph in the standard font (Cyrillic, most often): a reader with the
//! original font shows that text, which a program drawing it blank would lose.

use std::collections::{HashMap, HashSet};

use hayro_cmap::{BfString, CMap};
use sealwax_core::{Dict, Document, Name, ObjId, Object, Result};

use super::cff::Placed;
use super::standard::{NONSYMBOLIC, SYMBOLIC, Standard, strip_subset};
use super::{Program, cid_widths, descriptor_hints, embedded};
use crate::Report;
use crate::color::flate_stream;
use crate::edit::{self, Location};
use crate::scan::FontUse;

/// The width of a CID `/W` does not give, when `/DW` gives none either (ISO 32000-1, 9.7.4.3).
const DEFAULT_WIDTH: f64 = 1000.0;

/// Characters a standard font draws with the glyph of another: a no-break space as a space, a
/// soft hyphen as a hyphen (ISO 32000-1, annex D, notes on WinAnsiEncoding).
const SAME_GLYPH: [(&str, &str); 2] = [("\u{a0}", " "), ("\u{ad}", "-")];

/// The glyph of `program` that draws `text`.
fn glyph_for(program: &Program, text: &str) -> Option<u16> {
    let text = SAME_GLYPH
        .iter()
        .find(|(from, _)| *from == text)
        .map_or(text, |(_, to)| *to);
    program.by_text.get(text).copied()
}

/// Where a composite font's CIDFont is.
fn cid_font_location(font: &Dict, location: &Location) -> Option<Location> {
    let descendants = match font.get(b"DescendantFonts")? {
        Object::Ref(id) => Location::of(*id),
        _ => location.key(b"DescendantFonts"),
    };
    Some(descendants.index(0))
}

/// Whether the composite font at `location` has no program: its CIDFont's descriptor names none.
pub(super) fn lacks_program(doc: &Document, location: &Location) -> Result<bool> {
    let Some(font) = edit::read_dict(doc, location)? else {
        return Ok(false);
    };
    let Some(cid_location) = cid_font_location(&font, location) else {
        return Ok(false);
    };
    if edit::read_dict(doc, &cid_location)?.is_none() {
        return Ok(false);
    }
    let descriptor = edit::read_dict(doc, &cid_location.key(b"FontDescriptor"))?;
    Ok(!embedded(descriptor.as_ref()))
}

/// Give the composite font at `location` the program of the standard font that stands in for
/// it, its glyphs in the order of the font's CIDs.
pub(super) fn embed(
    doc: &mut Document,
    location: &Location,
    used: &FontUse,
    programs: &mut HashMap<Standard, Option<Program>>,
    descriptors: &mut HashSet<ObjId>,
    report: &mut Report,
) -> Result<()> {
    let Some(font) = edit::read_dict(doc, location)? else {
        return Ok(());
    };
    let identity = matches!(
        font.get(b"Encoding").and_then(Object::as_name),
        Some(b"Identity-H" | b"Identity-V")
    );
    let Some(last) = used.cids.iter().max().copied() else {
        return Ok(());
    };
    if !identity {
        return Ok(());
    }
    let Some(to_unicode) = font
        .get(b"ToUnicode")
        .and_then(Object::as_ref_id)
        .and_then(|id| edit::stream_data(doc, id, false))
        .and_then(|data| CMap::parse(&data, |_| None))
    else {
        report.residual_add("composite_font_without_to_unicode", 1);
        return Ok(());
    };
    let Some(cid_location) = cid_font_location(&font, location) else {
        return Ok(());
    };
    let Some(cid_font) = edit::read_dict(doc, &cid_location)? else {
        return Ok(());
    };
    let descriptor_location = cid_location.key(b"FontDescriptor");
    let descriptor = if cid_font.contains_key(b"FontDescriptor") {
        edit::read_dict(doc, &descriptor_location)?
    } else {
        None
    };
    let shared = cid_font
        .get(b"FontDescriptor")
        .and_then(Object::as_ref_id)
        .is_some_and(|id| !descriptors.insert(id));
    let base_font = [&cid_font, &font]
        .iter()
        .find_map(|dict| dict.get(b"BaseFont").and_then(Object::as_name))
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_default();
    let (flags, weight, angle) = descriptor_hints(descriptor.as_ref());
    // A symbolic font's CIDs mean characters the standard symbolic fonts may not have.
    let Some(standard) = Standard::substitute(&base_font, flags, weight, angle)
        .filter(|standard| !standard.symbolic())
    else {
        report.residual_add("font_without_substitute", 1);
        return Ok(());
    };
    let Some(program) = programs
        .entry(standard)
        .or_insert_with(|| Program::new(standard))
        .as_ref()
    else {
        report.residual_add("font_without_substitute", 1);
        return Ok(());
    };

    let widths = match cid_font.get(b"W") {
        Some(value) => cid_widths(doc, value)?,
        None => Default::default(),
    };
    let default = cid_font
        .get(b"DW")
        .and_then(Object::as_f64)
        .unwrap_or(DEFAULT_WIDTH);
    let mut glyphs = Vec::with_capacity(usize::from(last) + 1);
    let mut missing = false;
    for cid in 0..=last {
        let source = if cid == 0 {
            Some(0)
        } else {
            to_unicode
                .lookup_bf_string(u32::from(cid))
                .map(|text| match text {
                    BfString::Char(c) => c.to_string(),
                    BfString::String(text) => text,
                })
                .and_then(|text| glyph_for(program, &text))
        };
        missing |= source.is_none() && used.cids.contains(&cid);
        glyphs.push(Placed {
            source,
            width: widths.get(&cid).copied().unwrap_or(default),
        });
    }
    if missing {
        // A CID shown that the standard font cannot draw: drawn blank, its text would be lost.
        report.residual_add("composite_font_glyph_missing", 1);
        return Ok(());
    }
    let Some(bytes) = program.cff.reordered(&glyphs) else {
        report.residual_add("program_not_written", 1);
        return Ok(());
    };
    let mut dict = Dict::new();
    dict.insert("Subtype", Name::from("CIDFontType0C"));
    let file = doc.add_object(Object::Stream(flate_stream(dict, &bytes)))?;

    let font_name = Name::new(strip_subset(&base_font).as_bytes().to_vec());
    let metrics = standard.metrics();
    let bbox = program.cff.bbox();
    let italic_angle = program.cff.italic_angle();
    let complete = |d: &mut Dict| {
        d.insert("Type", Name::from("FontDescriptor"));
        if !d.contains_key(b"FontName") {
            d.insert("FontName", font_name.clone());
        }
        let own = d.get(b"Flags").and_then(Object::as_i64).unwrap_or(0);
        d.insert(
            "Flags",
            (own & !(SYMBOLIC | NONSYMBOLIC)) | standard.flags(),
        );
        if !d.contains_key(b"FontBBox") {
            d.insert(
                "FontBBox",
                bbox.iter().map(|v| Object::Real(*v)).collect::<Vec<_>>(),
            );
        }
        if !d.contains_key(b"ItalicAngle") {
            d.insert("ItalicAngle", italic_angle);
        }
        for (key, value) in [
            ("Ascent", metrics.ascent),
            ("Descent", metrics.descent),
            ("CapHeight", metrics.cap_height),
            ("StemV", metrics.stem_v),
        ] {
            if !d.contains_key(key.as_bytes()) {
                d.insert(key, value);
            }
        }
        d.remove(b"FontFile");
        d.remove(b"FontFile2");
        d.insert("FontFile3", file);
    };
    let new_descriptor = match descriptor {
        Some(_) if !shared => {
            edit::edit_dict(doc, &descriptor_location, false, complete)?;
            None
        }
        existing => {
            let mut d = existing.unwrap_or_default();
            complete(&mut d);
            Some(doc.add_object(Object::Dict(d))?)
        }
    };
    edit::edit_dict(doc, &cid_location, false, |dict| {
        // A CFF program: the CIDFont is of type 0, its CIDs the program's glyphs.
        dict.insert("Subtype", Name::from("CIDFontType0"));
        dict.remove(b"CIDToGIDMap");
        if let Some(id) = new_descriptor {
            dict.insert("FontDescriptor", id);
        }
    })?;
    report.change("composite_font_embedded", 1);
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use sealwax_core::{Document, Object, OpenOptions, SaveOptions};

    use super::super::cff::{Cff, Placed};
    use super::super::standard::Standard;
    use crate::{Level, Options, convert};

    /// Arial, not embedded, encoded `Identity-H`, its CIDs Arial's own glyph numbers: "Hello"
    /// is 43 72 79 79 82. Its `ToUnicode` CMap says what they mean.
    fn arial(to_unicode: &str, w: &str) -> Vec<u8> {
        let content = "BT /F1 24 Tf 20 100 Td <002B0048004F004F0052> Tj ET";
        let cmap = format!(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
             /CMapName /Adobe-Identity-UCS def /CMapType 2 def \
             1 begincodespacerange <0000> <FFFF> endcodespacerange \
             {to_unicode} endcmap CMapName currentdict /CMap defineresource pop end end"
        );
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
             /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
                .to_owned(),
            "<< /Type /Font /Subtype /Type0 /BaseFont /ArialMT /Encoding /Identity-H \
             /DescendantFonts [6 0 R] /ToUnicode 8 0 R >>"
                .to_owned(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /ArialMT \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor 7 0 R /DW 1000 /W [{w}] /CIDToGIDMap /Identity >>"
            ),
            "<< /Type /FontDescriptor /FontName /ArialMT /Flags 32 /FontBBox [-665 -325 2000 1040] \
             /ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 >>"
                .to_owned(),
            format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
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
        out
    }

    const HELLO: &str = "2 beginbfchar <002B> <0048> <0048> <0065> endbfchar \
                         1 beginbfrange <004F> <0052> [<006C> <0070> <0071> <006F>] endbfrange";
    const WIDTHS: &str = "43 [722] 72 [556] 79 [222] 82 [556]";

    fn object(doc: &Document, value: &Object) -> Object {
        match value {
            Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
            other => other.clone(),
        }
    }

    #[test]
    fn a_composite_font_gets_the_standard_glyphs_in_the_order_of_its_cids() {
        let mut doc = Document::open(arial(HELLO, WIDTHS), &OpenOptions::default()).unwrap();
        let report = convert(&mut doc, &Options::new(Level::B)).unwrap();
        assert_eq!(report.changes().get("composite_font_embedded"), Some(&1));
        assert!(report.residual().is_empty(), "{:?}", report.residual());
        let doc = Document::open(
            doc.save(&SaveOptions::default()).unwrap(),
            &OpenOptions::default(),
        )
        .unwrap();
        let page = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
        let resources = object(&doc, page.as_dict().unwrap().get(b"Resources").unwrap());
        let fonts = object(&doc, resources.as_dict().unwrap().get(b"Font").unwrap());
        let font = object(&doc, fonts.as_dict().unwrap().get(b"F1").unwrap());
        let descendants = object(
            &doc,
            font.as_dict().unwrap().get(b"DescendantFonts").unwrap(),
        );
        let cid_font = object(&doc, &descendants.as_array().unwrap()[0]);
        let cid_font = cid_font.as_dict().unwrap();
        assert_eq!(
            cid_font.get(b"Subtype").and_then(Object::as_name),
            Some(&b"CIDFontType0"[..])
        );
        assert!(!cid_font.contains_key(b"CIDToGIDMap"));
        let descriptor = object(&doc, cid_font.get(b"FontDescriptor").unwrap());
        let Object::Stream(file) = object(
            &doc,
            descriptor.as_dict().unwrap().get(b"FontFile3").unwrap(),
        ) else {
            panic!("no program");
        };
        assert_eq!(
            file.dict.get(b"Subtype").and_then(Object::as_name),
            Some(&b"CIDFontType0C"[..])
        );
        let mut program = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(file.data.as_slice()),
            &mut program,
        )
        .unwrap();
        let written = Cff::parse(&program).unwrap();
        let helvetica = Cff::parse(Standard::Helvetica.program()).unwrap();
        // One glyph per CID up to the last shown; each drawn as the substitute's glyph of its
        // meaning, with the width `/W` gives it.
        assert_eq!(written.glyph_count(), 83);
        for (cid, name, width) in [
            (43, "H", 722.0),
            (72, "e", 556.0),
            (79, "l", 222.0),
            (82, "o", 556.0),
        ] {
            assert_eq!(written.width(cid), Some(width), "{name}");
            assert_eq!(written.name(cid), Some(name));
            assert!(!written.draws_nothing(cid), "{name}");
            assert_eq!(
                helvetica.width(helvetica.glyph(name).unwrap()),
                Some(width),
                "{name}"
            );
        }
        // A CID not shown draws nothing, at the default width.
        assert!(written.draws_nothing(50));
        assert_eq!(written.width(50), Some(1000.0));
    }

    #[test]
    fn many_cids_make_a_program_each_glyph_named_once() {
        let helvetica = Cff::parse(Standard::Helvetica.program()).unwrap();
        let a = helvetica.glyph("a").unwrap();
        let blank = Placed {
            source: None,
            width: 1000.0,
        };
        let mut glyphs = vec![blank; 30_000];
        for cid in [100, 29_999] {
            glyphs[cid] = Placed {
                source: Some(a),
                width: 556.0,
            };
        }
        let started = std::time::Instant::now();
        let program = helvetica.reordered(&glyphs).unwrap();
        assert!(started.elapsed().as_secs() < 2, "{:?}", started.elapsed());
        let written = Cff::parse(&program).unwrap();
        assert_eq!(written.glyph_count(), 30_000);
        // The first to draw a glyph keeps its name; the second gets its own.
        assert_eq!(written.name(100), Some("a"));
        assert_eq!(written.name(29_999), Some("cid29999"));
        assert!(!written.draws_nothing(29_999));
        assert!(written.draws_nothing(5));
    }

    #[test]
    fn a_cid_shown_the_standard_font_cannot_draw_leaves_the_font_as_it_was() {
        // CID 0x52 ("o") left out of the CMap; then shown as a Cyrillic letter.
        let partial = "2 beginbfchar <002B> <0048> <0048> <0065> endbfchar \
                       1 beginbfchar <004F> <006C> endbfchar";
        let cyrillic = "3 beginbfchar <002B> <0048> <0048> <0065> <0052> <043E> endbfchar \
                        1 beginbfchar <004F> <006C> endbfchar";
        for cmap in [partial, cyrillic] {
            let mut doc = Document::open(arial(cmap, WIDTHS), &OpenOptions::default()).unwrap();
            let report = convert(&mut doc, &Options::new(Level::B)).unwrap();
            assert_eq!(report.changes().get("composite_font_embedded"), None);
            assert_eq!(
                report.residual().get("composite_font_glyph_missing"),
                Some(&1)
            );
            assert_eq!(report.residual().get("font_not_embedded"), Some(&1));
        }
        // A no-break space and a soft hyphen are drawn as a space and a hyphen.
        let aliases = "3 beginbfchar <002B> <0048> <0048> <00A0> <0052> <00AD> endbfchar \
                       1 beginbfchar <004F> <006C> endbfchar";
        let mut doc = Document::open(arial(aliases, WIDTHS), &OpenOptions::default()).unwrap();
        let report = convert(&mut doc, &Options::new(Level::B)).unwrap();
        assert_eq!(report.changes().get("composite_font_embedded"), Some(&1));
    }
}
