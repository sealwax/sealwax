#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, OpenOptions};
use sealwax_text::text_census;

/// A one-page PDF drawing `content` with font `/F1` = object 4, the objects after it given as
/// `(number, body)`. Stream bodies get their `/Length` computed.
fn pdf(content: &str, font: &str, more: &[(u32, String)]) -> Vec<u8> {
    pdf_with(content, font, more, "", "")
}

/// As [`pdf`], with entries added to the catalog and to the page.
fn pdf_with(
    content: &str,
    font: &str,
    more: &[(u32, String)],
    catalog: &str,
    page: &str,
) -> Vec<u8> {
    let mut objects: Vec<(u32, String)> = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>")),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> \
                 /Properties << /P1 << /ActualText (x) >> >> >> /Contents 5 0 R {page} >>"
            ),
        ),
        (4, font.to_owned()),
        (5, stream("", content)),
    ];
    objects.extend(more.iter().cloned());
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in &objects {
        offsets.push((*number, out.len()));
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|(n, _)| *n).max().unwrap() + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for number in 1..size {
        let offset = offsets
            .iter()
            .find(|(n, _)| *n == number)
            .map_or(0, |(_, o)| *o);
        let kind = if offset == 0 { "f" } else { "n" };
        out.extend_from_slice(format!("{offset:010} 00000 {kind} \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

fn census(data: Vec<u8>) -> sealwax_text::TextCensus {
    text_census(&Document::open(data, &OpenOptions::default()).expect("opens"))
        .expect("within limits")
}

const HELVETICA: &str =
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

#[test]
fn standard_text_maps_to_unicode() {
    let text = census(pdf("BT /F1 12 Tf 10 10 Td (Hello) Tj ET", HELVETICA, &[]));
    assert_eq!(text.pages_with_text, 1);
    assert_eq!(text.glyphs, 5);
    assert_eq!(text.without_unicode, 0);
    assert!(!text.fails_unicode_mapping());
    assert!(!text.fails_private_use());
}

#[test]
fn private_use_glyphs_need_an_actual_text() {
    // ToUnicode sends every code to U+E001, as fonts made for symbols often do.
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
                /CMapName /Test def 1 begincodespacerange <00> <FF> endcodespacerange \
                1 beginbfrange <00> <FF> <E001> endbfrange endcmap \
                CMapName currentdict /CMap defineresource pop end end";
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 6 0 R >>";
    let bare = census(pdf(
        "BT /F1 12 Tf 10 10 Td (AB) Tj ET",
        font,
        &[(6, stream("", cmap))],
    ));
    assert_eq!(
        (bare.private_use, bare.private_use_with_actual_text),
        (2, 0)
    );
    assert!(bare.fails_private_use());

    let covered = census(pdf(
        "/Span /P1 BDC BT /F1 12 Tf 10 10 Td (AB) Tj ET EMC",
        font,
        &[(6, stream("", cmap))],
    ));
    assert_eq!(
        (covered.private_use, covered.private_use_with_actual_text),
        (2, 2)
    );
    assert_eq!(covered.in_actual_text, 2);
    assert!(!covered.fails_private_use());
}

#[test]
fn an_actual_text_on_the_structure_element_covers_its_marked_content() {
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
                /CMapName /Test def 1 begincodespacerange <00> <FF> endcodespacerange \
                1 beginbfrange <00> <FF> <E001> endbfrange endcmap \
                CMapName currentdict /CMap defineresource pop end end";
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 6 0 R >>";
    let text = census(pdf_with(
        "/P << /MCID 0 >> BDC BT /F1 12 Tf 10 10 Td (AB) Tj ET EMC",
        font,
        &[
            (6, stream("", cmap)),
            (
                7,
                "<< /Type /StructTreeRoot /K 8 0 R /ParentTree << /Nums [0 [8 0 R]] >> >>"
                    .to_owned(),
            ),
            (
                8,
                "<< /Type /StructElem /S /Span /P 9 0 R /Pg 3 0 R /K 0 >>".to_owned(),
            ),
            (
                9,
                "<< /Type /StructElem /S /P /P 7 0 R /ActualText (AB) /K [8 0 R] >>".to_owned(),
            ),
        ],
        "/StructTreeRoot 7 0 R /MarkInfo << /Marked true >>",
        "/StructParents 0",
    ));
    assert_eq!(
        (text.private_use, text.private_use_with_actual_text),
        (2, 2)
    );
    assert!(!text.fails_private_use());
}

#[test]
fn a_type3_glyph_with_an_unknown_name_has_no_unicode() {
    let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /g1 6 0 R >> \
                /Encoding << /Type /Encoding /Differences [65 /g1] >> \
                /FirstChar 65 /LastChar 65 /Widths [1000] /Resources << >> >>";
    let text = census(pdf(
        "BT /F1 12 Tf 10 10 Td (A) Tj ET",
        font,
        &[(6, stream("", "1000 0 0 0 1000 1000 d1 0 0 1000 1000 re f"))],
    ));
    assert_eq!(text.glyphs, 1);
    assert_eq!(text.without_unicode, 1);
    assert!(text.fails_unicode_mapping());
    assert_eq!(text.fonts[0].name, "Type3");
}

#[test]
fn invisible_text_is_counted_apart() {
    let text = census(pdf(
        "BT 3 Tr /F1 12 Tf 10 10 Td (OCR) Tj ET",
        HELVETICA,
        &[],
    ));
    assert_eq!((text.glyphs, text.invisible), (3, 3));
    assert_eq!(text.pages_only_invisible, 1);
}

#[test]
fn a_page_without_text_counts_no_glyph() {
    let text = census(pdf("0 0 10 10 re f", HELVETICA, &[]));
    assert_eq!((text.pages, text.pages_with_text, text.glyphs), (1, 0, 0));
}

/// The census interprets every page, so it holds to the document's limits as
/// `Document::check_pages` does, before interpreting anything (external review: it did not).
#[test]
fn the_census_holds_to_the_documents_limits() {
    let data = pdf("BT /F1 12 Tf (Hello) Tj ET", HELVETICA, &[]);
    let mut limits = sealwax_core::Limits::default();
    limits.max_operators = 2;
    let doc = Document::open(data, &OpenOptions::default().limits(limits)).expect("opens");
    assert!(matches!(
        text_census(&doc),
        Err(sealwax_core::Error::LimitExceeded {
            what: "max_operators"
        })
    ));
}
