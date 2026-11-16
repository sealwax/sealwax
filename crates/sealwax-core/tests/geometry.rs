#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{pdf, pdf_with_trailer};
use sealwax_core::{Document, Error, OpenOptions, Rect, Rotation};

fn one_page(page_entries: &str, pages_entries: &str) -> Vec<u8> {
    pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                &format!("<< /Type /Pages /Kids [3 0 R] /Count 1 {pages_entries} >>"),
            ),
            (
                3,
                &format!("<< /Type /Page /Parent 2 0 R {page_entries} >>"),
            ),
        ],
        false,
    )
}

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).expect("opens")
}

#[test]
fn media_box_is_inherited_and_crop_box_clipped_to_it() {
    let doc = open(one_page(
        "/CropBox [-10 -10 700 900]",
        "/MediaBox [0 0 612 792]",
    ));
    let [page] = doc.geometry()[..] else {
        panic!("one page")
    };
    assert_eq!(page.page_number, 1);
    assert_eq!(page.media_box, Rect::from_corners(0.0, 0.0, 612.0, 792.0));
    assert_eq!(page.crop_box, Rect::from_corners(0.0, 0.0, 612.0, 792.0));
    assert_eq!(page.user_unit, 1.0);
}

#[test]
fn crop_box_defaults_to_media_box_and_corners_are_normalized() {
    let doc = open(one_page("/MediaBox [595 842 0 0]", ""));
    let page = doc.geometry()[0];
    assert_eq!(page.media_box.to_tuple(), (0.0, 0.0, 595.0, 842.0));
    assert_eq!(page.crop_box, page.media_box);
}

#[test]
fn a_page_without_media_box_is_letter() {
    let doc = open(one_page("", ""));
    let page = doc.geometry()[0];
    assert_eq!(page.media_box.to_tuple(), (0.0, 0.0, 612.0, 792.0));
    assert_eq!(page.crop_box, page.media_box);
}

#[test]
fn a_crop_box_declared_without_media_box_is_kept_within_letter() {
    let doc = open(one_page("/CropBox [0 0 300 300]", ""));
    let page = doc.geometry()[0];
    assert_eq!(page.media_box.to_tuple(), (0.0, 0.0, 612.0, 792.0));
    assert_eq!(page.crop_box.to_tuple(), (0.0, 0.0, 300.0, 300.0));
}

#[test]
fn a_crop_box_that_leaves_nothing_visible_gives_way_to_the_media_box() {
    for crop in ["[0 0 0 0]", "[700 800 1000 1000]"] {
        let doc = open(one_page(
            &format!("/MediaBox [0 0 600 800] /CropBox {crop}"),
            "",
        ));
        let page = doc.geometry()[0];
        assert_eq!(page.crop_box, page.media_box, "/CropBox {crop}");
    }
}

#[test]
fn a_document_that_lost_a_page_is_refused() {
    // The stray parenthesis makes page 4's dictionary unreadable (pdf.js issue 22011). PDFium
    // still reads both pages; hayro drops that one, and only /Count tells.
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] ) >>",
            ),
        ],
        false,
    );
    let error = Document::open(data, &OpenOptions::default()).unwrap_err();
    assert!(
        matches!(
            error,
            Error::PagesMissing {
                found: 1,
                declared: 2
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "PDF declares 2 pages but only 1 can be read"
    );
}

#[test]
fn a_count_lower_than_the_pages_found_is_only_a_wrong_count() {
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 0 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
        ],
        false,
    );
    let doc = open(data);
    assert_eq!(doc.declared_page_count(), Some(0));
    assert_eq!(doc.page_count(), 1);
}

#[test]
fn rotation_is_normalized_modulo_360() {
    for (rotate, expected) in [
        (0, Rotation::None),
        (90, Rotation::Quarter),
        (-90, Rotation::ThreeQuarters),
        (450, Rotation::Quarter),
        (180, Rotation::Half),
    ] {
        let doc = open(one_page(
            &format!("/MediaBox [0 0 100 100] /Rotate {rotate}"),
            "",
        ));
        assert_eq!(doc.geometry()[0].rotation, expected, "/Rotate {rotate}");
    }
}

#[test]
fn rotation_is_inherited_from_the_page_tree() {
    let doc = open(one_page("/MediaBox [0 0 100 100]", "/Rotate 270"));
    assert_eq!(doc.geometry()[0].rotation.degrees(), 270);
}

#[test]
fn user_unit_is_read_from_the_page() {
    let doc = open(one_page("/MediaBox [0 0 100 100] /UserUnit 2.5", ""));
    assert_eq!(doc.geometry()[0].user_unit, 2.5);
}

#[test]
fn page_count_outside_the_accepted_range_is_refused() {
    let data = one_page("/MediaBox [0 0 100 100]", "");
    let error = Document::open(data, &OpenOptions::default().page_count(2..=1000)).unwrap_err();
    assert!(matches!(error, Error::PageCountOutOfRange { count: 1, .. }));
    assert_eq!(error.to_string(), "PDF has 1 pages, accepted: 2 to 1000");
}

#[test]
fn a_broken_xref_table_is_repaired() {
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
        ],
        true,
    );
    let doc = open(data);
    assert_eq!(doc.page_count(), 1);
    assert_eq!(
        doc.geometry()[0].media_box.to_tuple(),
        (0.0, 0.0, 200.0, 300.0)
    );
}

#[test]
fn data_that_is_not_a_pdf_is_malformed() {
    let error = Document::open(b"not a pdf at all".to_vec(), &OpenOptions::default()).unwrap_err();
    assert!(matches!(error, Error::Malformed), "{error:?}");
}

#[test]
fn data_over_the_size_limit_is_refused_before_parsing() {
    let data = one_page("/MediaBox [0 0 100 100]", "");
    let error = Document::open(data, &OpenOptions::default().max_file_bytes(10)).unwrap_err();
    assert!(matches!(
        error,
        Error::LimitExceeded {
            what: "max_file_bytes"
        }
    ));
}

#[test]
fn a_document_the_empty_password_does_not_open_requires_a_password() {
    // RC4 40-bit, standard security handler: no password matches this /U, the empty one included.
    let encrypt = format!(
        "/Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 /P -4 /O <{0}> /U <{0}> >> \
         /ID [<00112233445566778899AABBCCDDEEFF> <00112233445566778899AABBCCDDEEFF>]",
        "AB".repeat(32)
    );
    let data = pdf_with_trailer(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
        ],
        false,
        &encrypt,
    );
    let error = Document::open(data, &OpenOptions::default()).unwrap_err();
    assert!(matches!(error, Error::PasswordRequired), "{error:?}");
}

/// Pages under two nodes of a page tree that hand down different boxes, read once per node: each
/// page gets those of its own nearest ancestors.
#[test]
fn boxes_handed_down_by_shared_nodes_reach_every_page_as_read() {
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 5 /MediaBox [0 0 500 500] >>",
            ),
            (
                3,
                "<< /Type /Pages /Parent 2 0 R /Kids [5 0 R 6 0 R 7 0 R] /Count 3 \
                 /CropBox [10 10 400 400] >>",
            ),
            (
                4,
                "<< /Type /Pages /Parent 2 0 R /Kids [8 0 R 9 0 R] /Count 2 >>",
            ),
            (5, "<< /Type /Page /Parent 3 0 R >>"),
            (6, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 300 300] >>"),
            (7, "<< /Type /Page /Parent 3 0 R /CropBox [0 0 100 100] >>"),
            (8, "<< /Type /Page /Parent 4 0 R >>"),
            (9, "<< /Type /Page /Parent 4 0 R /CropBox [20 20 50 50] >>"),
        ],
        false,
    );
    let doc = open(data);
    let found: Vec<(Rect, Rect)> = doc
        .geometry()
        .iter()
        .map(|g| (g.media_box, g.crop_box))
        .collect();
    let r = |x1, y1, x2, y2| Rect::from_corners(x1, y1, x2, y2);
    assert_eq!(
        found,
        [
            (r(0.0, 0.0, 500.0, 500.0), r(10.0, 10.0, 400.0, 400.0)),
            (r(0.0, 0.0, 300.0, 300.0), r(10.0, 10.0, 300.0, 300.0)),
            (r(0.0, 0.0, 500.0, 500.0), r(0.0, 0.0, 100.0, 100.0)),
            (r(0.0, 0.0, 500.0, 500.0), r(0.0, 0.0, 500.0, 500.0)),
            (r(0.0, 0.0, 500.0, 500.0), r(20.0, 20.0, 50.0, 50.0)),
        ]
    );
    // One page's geometry by its object, as the whole list reports it.
    let ids = doc.page_ids();
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(doc.page_geometry(id.unwrap()), Some(doc.geometry()[index]));
    }
    assert_eq!(doc.page_geometry(sealwax_core::ObjId::new(2)), None);
}

/// A page tree of 20,000 pages under one node: reading the node once per page made opening
/// quadratic (100,000 pages took 48 s).
#[test]
fn a_node_with_many_kids_is_read_once() {
    let pages = 20_000u32;
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + i)).collect();
    let pages_node =
        format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /CropBox [0 0 200 200] >>");
    let mut objects: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (2, pages_node),
    ];
    for i in 0..pages {
        objects.push((
            10 + i,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>".to_owned(),
        ));
    }
    let borrowed: Vec<(u32, &str)> = objects.iter().map(|(n, b)| (*n, b.as_str())).collect();
    let started = std::time::Instant::now();
    let doc = open(pdf(&borrowed, false));
    let geometry = doc.geometry();
    assert_eq!(geometry.len(), 20_000);
    assert!(
        geometry
            .iter()
            .all(|g| g.crop_box == Rect::from_corners(0.0, 0.0, 200.0, 200.0))
    );
    // Quadratic, this takes seconds even in release.
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
}
