//! Documents made of pages of others, their forms kept: arranged, extracted, inserted, merged.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use sealwax_annots::{FillValue, field_values, fields, fill_fields};
use sealwax_core::{Document, Error, OpenOptions, PagePick, SaveOptions, XrefFormat};

fn build(objects: &[(u32, String)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in objects {
        offsets.push((*number, out.len()));
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|(n, _)| *n).max().unwrap() + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for number in 1..size {
        match offsets.iter().find(|(n, _)| *n == number) {
            Some((_, offset)) => {
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
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

/// Three pages, each drawing its marker (`PAGEzero`, `PAGEone`, `PAGEtwo`). Field `name` has a
/// widget on pages 1 and 2 (appearances `WIDGETzero`, `WIDGETone`), field `city` one on page 3
/// (`WIDGETtwo`).
fn form_bytes() -> Vec<u8> {
    let appearance = |marker: &str| {
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 100 20]",
            &format!("BT ({marker}) Tj ET"),
        )
    };
    let page = |contents: u32, annot: u32| {
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents {contents} 0 R \
             /Annots [{annot} 0 R] >>"
        )
    };
    let widget = |parent: u32, page: u32, appearance: u32| {
        format!(
            "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R /P {page} 0 R \
             /Rect [10 10 110 30] /F 4 /AP << /N {appearance} 0 R >> >>"
        )
    };
    build(&[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 20 0 R] \
             /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 30 0 R >> >> /NeedAppearances true >> >>"
                .to_owned(),
        ),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".to_owned(),
        ),
        (3, page(6, 11)),
        (4, page(7, 12)),
        (5, page(8, 21)),
        (6, stream("", "BT (PAGEzero) Tj ET")),
        (7, stream("", "BT (PAGEone) Tj ET")),
        (8, stream("", "BT (PAGEtwo) Tj ET")),
        (
            10,
            "<< /FT /Tx /T (name) /V (shared) /Kids [11 0 R 12 0 R] >>".to_owned(),
        ),
        (11, widget(10, 3, 13)),
        (12, widget(10, 4, 14)),
        (13, appearance("WIDGETzero")),
        (14, appearance("WIDGETone")),
        (20, "<< /FT /Tx /T (city) /Kids [21 0 R] >>".to_owned()),
        (21, widget(20, 5, 22)),
        (22, appearance("WIDGETtwo")),
        (
            30,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ),
    ])
}

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).unwrap()
}

/// The document saved with a cross-reference table, every object in the clear.
fn saved(doc: &Document) -> Vec<u8> {
    doc.save(&SaveOptions::default().xref(XrefFormat::Table))
        .unwrap()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn names(doc: &Document) -> Vec<(String, usize)> {
    fields(doc, None)
        .unwrap()
        .into_iter()
        .map(|field| (field.name, field.page_index))
        .collect()
}

#[test]
fn a_page_taken_out_keeps_its_fields_and_nothing_of_the_others() {
    let source = open(form_bytes());
    let out = source.arrange(&[(1, 0)]).unwrap();
    let bytes = saved(&out);
    for present in ["PAGEone", "WIDGETone", "(name)", "(shared)"] {
        assert!(contains(&bytes, present), "{present} missing");
    }
    for absent in ["PAGEzero", "PAGEtwo", "WIDGETzero", "WIDGETtwo", "(city)"] {
        assert!(!contains(&bytes, absent), "{absent} leaked");
    }
    // The field is the form's, fillable, with its one widget left.
    let mut out = open(bytes);
    assert_eq!(names(&out), [("name".to_owned(), 0)]);
    let values: HashMap<String, FillValue> =
        [("name".to_owned(), FillValue::Text("Zoé".into()))].into();
    assert_eq!(fill_fields(&mut out, &values).unwrap().occurrences, 1);
    let values: HashMap<String, Vec<String>> = field_values(&open(saved(&out)))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(values["name"], ["Zoé"]);
}

#[test]
fn extracting_without_the_form_leaks_no_widget_of_another_page() {
    // The widget kept leads to its field, whose other widget is on a page left out: before, it
    // came along with its appearance.
    let source = open(form_bytes());
    let bytes = saved(&source.extract_pages(1..2).unwrap());
    assert!(contains(&bytes, "WIDGETone"));
    for absent in ["PAGEzero", "PAGEtwo", "WIDGETzero", "WIDGETtwo"] {
        assert!(!contains(&bytes, absent), "{absent} leaked");
    }
    // The form stays behind: the widget is an annotation of the page, no field of a form.
    let out = open(bytes);
    let catalog = out.object(out.catalog_id()).unwrap().unwrap();
    assert!(!catalog.as_dict().unwrap().contains_key(b"AcroForm"));
}

#[test]
fn pages_come_in_the_order_asked_turned_as_asked() {
    let source = open(form_bytes());
    let out = open(saved(&source.arrange(&[(2, 0), (0, 1)]).unwrap()));
    assert_eq!(out.page_count(), 2);
    let rotations: Vec<u16> = out
        .geometry()
        .iter()
        .map(|g| g.rotation.degrees())
        .collect();
    assert_eq!(rotations, [0, 90]);
    assert_eq!(
        names(&out),
        [("city".to_owned(), 0), ("name".to_owned(), 1)]
    );
    let bytes = saved(&out);
    assert!(!contains(&bytes, "PAGEone") && !contains(&bytes, "WIDGETone"));
    // Quarter turns add to the page's rotation, both ways, and wrap.
    for (turns, degrees) in [(-1, 270), (4, 0), (5, 90), (2, 180)] {
        let turned = source.arrange(&[(0, turns)]).unwrap();
        assert_eq!(turned.geometry()[0].rotation.degrees(), degrees, "{turns}");
    }
}

#[test]
fn the_same_form_merged_twice_gives_two_forms() {
    let (first, second) = (open(form_bytes()), open(form_bytes()));
    let picks: Vec<PagePick> = (0..2)
        .flat_map(|source| (0..3).map(move |page| PagePick::new(source, page, 0)))
        .collect();
    let mut out = open(saved(
        &Document::compose(&[&first, &second], &picks, true).unwrap(),
    ));
    assert_eq!(out.page_count(), 6);
    assert_eq!(
        names(&out),
        [
            ("name".to_owned(), 0),
            ("name".to_owned(), 1),
            ("city".to_owned(), 2),
            ("name (2)".to_owned(), 3),
            ("name (2)".to_owned(), 4),
            ("city (2)".to_owned(), 5),
        ]
    );
    // Filling one leaves the other as it was.
    let values: HashMap<String, FillValue> =
        [("name (2)".to_owned(), FillValue::Text("spouse".into()))].into();
    assert_eq!(fill_fields(&mut out, &values).unwrap().occurrences, 2);
    let values: HashMap<String, Vec<String>> = field_values(&open(saved(&out)))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(values["name"], ["shared", "shared"]);
    assert_eq!(values["name (2)"], ["spouse", "spouse"]);
}

#[test]
fn documents_are_inserted_where_asked_with_their_forms() {
    let (base, inserted) = (open(form_bytes()), open(form_bytes()));
    let out = open(saved(&base.insert(1, &[&inserted]).unwrap()));
    assert_eq!(out.page_count(), 6);
    // The base's first page, the inserted document (its fields renamed), the base's others.
    assert_eq!(
        names(&out),
        [
            ("name".to_owned(), 0),
            ("name (2)".to_owned(), 1),
            ("name (2)".to_owned(), 2),
            ("city (2)".to_owned(), 3),
            ("name".to_owned(), 4),
            ("city".to_owned(), 5),
        ]
    );
    let bytes = saved(&out);
    let order: Vec<usize> = ["PAGEzero", "PAGEone", "PAGEtwo"]
        .iter()
        .map(|marker| {
            bytes
                .windows(marker.len())
                .filter(|w| *w == marker.as_bytes())
                .count()
        })
        .collect();
    assert_eq!(order, [2, 2, 2]);
    assert_eq!(base.insert(3, &[&inserted]).unwrap().page_count(), 6);
}

#[test]
fn an_empty_list_a_missing_page_or_a_page_twice_is_refused() {
    let source = open(form_bytes());
    for pages in [&[][..], &[(3, 0)], &[(0, 0), (0, 1)]] {
        assert!(
            matches!(
                source.arrange(pages),
                Err(Error::InvalidArgument { what: "page list" })
            ),
            "{pages:?}"
        );
    }
    assert!(matches!(
        Document::compose(&[&source], &[PagePick::new(1, 0, 0)], true),
        Err(Error::InvalidArgument { .. })
    ));
    assert!(matches!(
        source.insert(4, &[&source]),
        Err(Error::InvalidArgument { .. })
    ));
}
