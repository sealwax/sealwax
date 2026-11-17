//! What goes with a flattening another engine makes: page boxes written on the pages, the form
//! taken out after, and a redacted copy rebuilt of its pages alone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use sealwax_core::{Document, Error, Object, OpenOptions, SaveOptions, XrefFormat};

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).unwrap()
}

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// Every object of `doc`, saved with a cross-reference table, in the clear.
fn saved(doc: &Document) -> Vec<u8> {
    doc.save(&SaveOptions::default().xref(XrefFormat::Table))
        .unwrap()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn page(doc: &Document, index: usize) -> sealwax_core::Dict {
    let id = doc.page_ids()[index].unwrap();
    match doc.object(id).unwrap() {
        Some(Object::Dict(page)) => page,
        other => panic!("not a page: {other:?}"),
    }
}

#[test]
fn inherited_boxes_and_rotation_are_written_on_each_page() {
    // A Legal page tree with a crop box and a quarter turn; the second page has its own box.
    let mut doc = open(common::pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 1008] \
                 /CropBox [10 10 600 1000] /Rotate 90 >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /CropBox [0 0 300 300] \
                 /Rotate 0 >>",
            ),
        ],
        false,
    ));
    let before = doc.geometry();
    assert_eq!(doc.own_page_boxes().unwrap(), 1);
    let first = page(&doc, 0);
    let numbers = |key: &[u8]| -> Vec<f64> {
        first
            .get(key)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_f64().unwrap())
            .collect()
    };
    assert_eq!(numbers(b"MediaBox"), [0.0, 0.0, 612.0, 1008.0]);
    assert_eq!(numbers(b"CropBox"), [10.0, 10.0, 600.0, 1000.0]);
    assert_eq!(first.get(b"Rotate").and_then(Object::as_i64), Some(90));
    let after = open(saved(&doc));
    assert_eq!(after.geometry(), before);
    // A document whose pages inherit nothing is left as it is.
    let mut plain = open(saved(&doc));
    assert_eq!(plain.own_page_boxes().unwrap(), 0);
    assert!(!plain.is_modified());
}

/// One page with a widget flagged `flags`, a link, and a form holding the widget's field.
fn flattened_elsewhere(flags: u32) -> Document {
    open(common::pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [5 0 R 6 0 R] >>",
            ),
            (
                5,
                &format!(
                    "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (FIELDVALUE) \
                     /Rect [10 10 100 30] /F {flags} >>"
                ),
            ),
            (
                6,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] \
                 /A << /S /URI /URI (https://example.org) >> >>",
            ),
        ],
        false,
    ))
}

#[test]
fn after_a_flattening_hidden_widgets_and_the_form_go() {
    for flags in [2, 32, 34] {
        let mut doc = flattened_elsewhere(flags);
        assert_eq!(doc.clear_flattened_form().unwrap(), 1, "{flags}");
        let bytes = saved(&doc);
        assert!(!contains(&bytes, "FIELDVALUE"), "{flags}: the value stayed");
        assert!(!contains(&bytes, "AcroForm"));
        assert!(contains(&bytes, "https://example.org"), "the link stays");
    }
    // A visible widget: the flattening did not happen; nothing is changed.
    let mut doc = flattened_elsewhere(4);
    assert!(matches!(
        doc.clear_flattened_form(),
        Err(Error::NotFlattened { page: 1 })
    ));
    assert!(!doc.is_modified());
}

#[test]
fn redaction_marks_are_found() {
    let marked = |subtype: &str| {
        open(common::pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    &format!(
                        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                         /Annots [<< /Type /Annot /Subtype /{subtype} /Rect [0 0 10 10] >>] >>"
                    ),
                ),
            ],
            false,
        ))
    };
    assert!(marked("Redact").has_redaction_marks().unwrap());
    assert!(!marked("Square").has_redaction_marks().unwrap());
}

#[test]
fn a_redacted_copy_keeps_its_pages_and_nothing_that_may_hide_what_was_redacted() {
    let content = stream("", "/OC /Layer BDC BT (PAGECONTENT) Tj ET EMC");
    let piece = stream("", "BT (OLDPAGE) Tj ET");
    let thumb = stream(
        "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        "OLDTHUMB",
    );
    let xmp = stream("/Type /Metadata /Subtype /XML", "SECRETXMP");
    let hidden = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        "BT (HIDDENANNOT) Tj ET",
    );
    let widget = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        "BT (WIDGETLOOK) Tj ET",
    );
    let doc = open(common::pdf_with_trailer(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Metadata 11 0 R /Outlines 12 0 R \
                 /Names << /EmbeddedFiles << /Names [(SECRETFILE) 13 0 R] >> >> \
                 /AcroForm << /Fields [8 0 R] >> \
                 /OCProperties << /OCGs [20 0 R] /D << /OFF [20 0 R] >> >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Resources << /Properties << /Layer 20 0 R >> >> /PieceInfo << /App << \
                 /Private 5 0 R >> >> /Thumb 6 0 R /AA << /O << /S /JavaScript /JS (SECRETJS) >> >> \
                 /Annots [7 0 R 8 0 R 9 0 R] >>",
            ),
            (4, &content),
            (5, &piece),
            (6, &thumb),
            (
                7,
                "<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] /F 2 /AP << /N 15 0 R >> >>",
            ),
            (
                8,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (SECRETVALUE) \
                 /Rect [10 10 100 30] /F 4 /AP << /N 16 0 R >> >>",
            ),
            (
                9,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] \
                 /A << /S /URI /URI (https://example.org) >> >>",
            ),
            (10, "<< /Title (SECRETINFO) >>"),
            (11, &xmp),
            (12, "<< /Type /Outlines /Count 0 >>"),
            (13, "<< /Type /Filespec /F (SECRETFILE.txt) >>"),
            (15, &hidden),
            (16, &widget),
            (20, "<< /Type /OCG /Name (Layer) >>"),
        ],
        false,
        "/Info 10 0 R",
    ));
    let rebuilt = doc.rebuilt_for_redaction().unwrap();
    let bytes = saved(&rebuilt);
    assert!(contains(&bytes, "PAGECONTENT"));
    assert!(contains(&bytes, "https://example.org"), "the link stays");
    for secret in [
        "OLDPAGE",
        "OLDTHUMB",
        "SECRETJS",
        "SECRETXMP",
        "SECRETINFO",
        "SECRETFILE",
        "SECRETVALUE",
        "WIDGETLOOK",
        "HIDDENANNOT",
    ] {
        assert!(!contains(&bytes, secret), "{secret} kept");
    }
    // The layer turned off is still off: the catalog's /OFF and the page's /Properties name one
    // object.
    let rebuilt = open(bytes);
    let catalog = rebuilt.object(rebuilt.catalog_id()).unwrap().unwrap();
    let properties = catalog.as_dict().unwrap().get(b"OCProperties").unwrap();
    let off = properties
        .as_dict()
        .unwrap()
        .get(b"D")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"OFF")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .as_ref_id()
        .unwrap();
    let page = page(&rebuilt, 0);
    let layer = page
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Properties")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Layer")
        .unwrap()
        .as_ref_id()
        .unwrap();
    assert_eq!(off, layer);
    assert!(!page.contains_key(b"PieceInfo") && !page.contains_key(b"Thumb"));
    assert_eq!(page.get(b"Annots").unwrap().as_array().unwrap().len(), 1);
}
