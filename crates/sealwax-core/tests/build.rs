//! Documents built from nothing: pages of a given size, their crop box, the outline, the
//! document information and an XMP packet, read back once saved.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use sealwax_core::{
    Document, Error, Name, NewOutlineEntry, Object, OpenOptions, OutlineEntry, Rect, SaveOptions,
};

fn reopened(doc: &Document) -> Document {
    let saved = doc.save(&SaveOptions::default()).unwrap();
    Document::open(saved, &OpenOptions::default()).unwrap()
}

fn entry(level: usize, title: &str, page_index: Option<usize>) -> NewOutlineEntry {
    NewOutlineEntry {
        level,
        title: title.to_owned(),
        page_index,
        top: None,
    }
}

#[test]
fn pages_added_have_their_size_and_their_crop_box() {
    let mut doc = Document::empty().unwrap();
    assert_eq!(doc.add_page(600.0, 800.0).unwrap(), 0);
    assert_eq!(doc.add_page(595.0, 842.0).unwrap(), 1);
    // Partly outside the page: what lies outside is left out.
    doc.set_crop_box(0, Rect::from_corners(50.0, 30.0, 650.0, 730.0))
        .unwrap();
    let crop = Rect::from_corners(50.0, 30.0, 600.0, 730.0);
    assert_eq!(doc.geometry()[0].crop_box, crop);
    let doc = reopened(&doc);
    let geometry = doc.geometry();
    assert_eq!(geometry.len(), 2);
    assert_eq!(
        geometry[0].media_box,
        Rect::from_corners(0.0, 0.0, 600.0, 800.0)
    );
    assert_eq!(geometry[0].crop_box, crop);
    assert_eq!(
        geometry[1].crop_box,
        Rect::from_corners(0.0, 0.0, 595.0, 842.0)
    );
}

#[test]
fn a_crop_box_on_a_page_of_the_input_is_its_geometry_at_once() {
    let mut doc = Document::empty().unwrap();
    doc.add_page(600.0, 800.0).unwrap();
    let mut doc = reopened(&doc);
    doc.set_crop_box(0, Rect::from_corners(10.0, 10.0, 300.0, 400.0))
        .unwrap();
    assert_eq!(
        doc.geometry()[0].crop_box,
        Rect::from_corners(10.0, 10.0, 300.0, 400.0)
    );
    assert_eq!(
        reopened(&doc).geometry()[0].crop_box,
        Rect::from_corners(10.0, 10.0, 300.0, 400.0)
    );
}

#[test]
fn sizes_and_crop_boxes_out_of_reach_are_refused() {
    let mut doc = Document::empty().unwrap();
    for (width, height) in [(0.0, 100.0), (100.0, f64::NAN), (20_000.0, 100.0)] {
        assert!(matches!(
            doc.add_page(width, height),
            Err(Error::InvalidArgument { what: "page size" })
        ));
    }
    doc.add_page(100.0, 100.0).unwrap();
    assert!(matches!(
        doc.set_crop_box(0, Rect::from_corners(200.0, 200.0, 300.0, 300.0)),
        Err(Error::InvalidArgument { what: "crop box" })
    ));
    assert!(matches!(
        doc.set_crop_box(3, Rect::from_corners(0.0, 0.0, 50.0, 50.0)),
        Err(Error::InvalidArgument { what: "page index" })
    ));
}

#[test]
fn the_outline_reads_back_as_written() {
    let mut doc = Document::empty().unwrap();
    for _ in 0..3 {
        doc.add_page(100.0, 100.0).unwrap();
    }
    let entries = [
        entry(1, "Chapter", Some(0)),
        entry(2, "Section", Some(1)),
        entry(3, "Été", Some(2)),
        entry(2, "Other section", None),
        entry(1, "Appendix", Some(2)),
    ];
    doc.set_outline(&entries).unwrap();
    let read = reopened(&doc).outline().unwrap();
    let expected: Vec<OutlineEntry> = entries
        .iter()
        .map(|e| OutlineEntry {
            level: e.level,
            title: e.title.clone(),
            page_index: e.page_index,
        })
        .collect();
    assert_eq!(read, expected);
    // Replaced, then removed.
    doc.set_outline(&[entry(1, "Only", Some(1))]).unwrap();
    assert_eq!(reopened(&doc).outline().unwrap().len(), 1);
    doc.set_outline(&[]).unwrap();
    assert!(reopened(&doc).outline().unwrap().is_empty());
}

#[test]
fn an_outline_entry_with_a_top_leads_to_that_height() {
    let mut doc = Document::empty().unwrap();
    doc.add_page(100.0, 800.0).unwrap();
    let mut heading = entry(1, "Heading", Some(0));
    heading.top = Some(700.5);
    doc.set_outline(&[heading, entry(1, "Page", Some(0))])
        .unwrap();
    let doc = reopened(&doc);
    let dict = |id| match doc.object(id).unwrap() {
        Some(Object::Dict(dict)) => dict,
        other => panic!("not a dictionary: {other:?}"),
    };
    let reference = |object: Option<&Object>| match object {
        Some(Object::Ref(id)) => *id,
        other => panic!("not a reference: {other:?}"),
    };
    let outlines = dict(reference(dict(doc.catalog_id()).get(b"Outlines")));
    let first = dict(reference(outlines.get(b"First")));
    let page = doc.page_ids()[0].unwrap();
    assert_eq!(
        first.get(b"Dest"),
        Some(&Object::Array(vec![
            Object::Ref(page),
            Object::Name(Name::from("XYZ")),
            Object::Integer(0),
            Object::Real(700.5),
            Object::Null,
        ]))
    );
    let second = dict(reference(first.get(b"Next")));
    assert_eq!(
        second.get(b"Dest"),
        Some(&Object::Array(vec![
            Object::Ref(page),
            Object::Name(Name::from("Fit"))
        ]))
    );
}

#[test]
fn outline_levels_and_pages_out_of_reach_are_refused() {
    let mut doc = Document::empty().unwrap();
    doc.add_page(100.0, 100.0).unwrap();
    for entries in [
        vec![entry(2, "Too deep", Some(0))],
        vec![entry(1, "A", Some(0)), entry(3, "Skips a level", Some(0))],
        vec![entry(0, "No level", Some(0))],
    ] {
        assert!(matches!(
            doc.set_outline(&entries),
            Err(Error::InvalidArgument {
                what: "outline level"
            })
        ));
    }
    assert!(matches!(
        doc.set_outline(&[entry(1, "Nowhere", Some(5))]),
        Err(Error::InvalidArgument { what: "page index" })
    ));
}

#[test]
fn information_and_xmp_read_back_in_the_metadata() {
    let mut doc = Document::empty().unwrap();
    doc.add_page(100.0, 100.0).unwrap();
    let info: BTreeMap<String, String> = [
        ("Title", "Éte"),
        ("Author", "Tests"),
        ("CreationDate", "D:20240131120000Z"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    doc.set_info(&info).unwrap();
    doc.set_xmp(
        b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
          xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
          rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\" pdfaid:part=\"2\" \
          pdfaid:conformance=\"B\"/></rdf:RDF></x:xmpmeta>",
    )
    .unwrap();
    let metadata = reopened(&doc).metadata();
    assert_eq!(metadata.info, info);
    assert_eq!(metadata.pdfa.as_deref(), Some("2B"));
    doc.set_info(&BTreeMap::new()).unwrap();
    assert!(reopened(&doc).metadata().info.is_empty());
}
