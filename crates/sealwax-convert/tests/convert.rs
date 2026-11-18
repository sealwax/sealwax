//! DOCX documents converted: laid out by docxide-pdf, written by sealwax-compose, read back by
//! sealwax: pages, text, links, the structure of the document and its bookmarks.
//! `data/two-pages.docx` is written by `data/make_docx.py`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use sealwax_convert::{DocxOptions, Error, docx_to_pdf, layout_docx};
use sealwax_core::{Dict, Document, Limits, Object, OpenOptions, OutlineEntry, SaveOptions};

const TWO_PAGES: &[u8] = include_bytes!("data/two-pages.docx");

fn converted(docx: &[u8]) -> Document {
    let converted = docx_to_pdf(docx, &DocxOptions::default(), &Limits::default()).unwrap();
    let bytes = converted
        .composed
        .document
        .save(&SaveOptions::default())
        .unwrap();
    Document::open(bytes, &OpenOptions::default()).unwrap()
}

/// The text a page reads back, whitespace aside: whether a space is drawn is the layout's choice.
fn text(doc: &Document, page: usize) -> String {
    sealwax_text::glyphs(doc, page, false)
        .unwrap()
        .into_iter()
        .filter_map(|glyph| glyph.text)
        .collect::<String>()
        .split_whitespace()
        .collect()
}

fn dict(doc: &Document, object: &Object) -> Dict {
    match object {
        Object::Ref(id) => match doc.object(*id).unwrap() {
            Some(Object::Dict(dict)) => dict,
            other => panic!("not a dictionary: {other:?}"),
        },
        Object::Dict(dict) => dict.clone(),
        other => panic!("not a dictionary: {other:?}"),
    }
}

#[test]
fn pages_and_their_text_read_back() {
    let doc = converted(TWO_PAGES);
    assert_eq!(doc.page_count(), 2);
    assert_eq!(
        text(&doc, 0),
        "AheadingHellofromasyntheticdocument.Alink•Firstitem•Seconditem"
    );
    assert_eq!(text(&doc, 1), "Secondpage.");
}

#[test]
fn a_link_leads_to_its_address() {
    let doc = converted(TWO_PAGES);
    let page = doc.page_ids()[0].unwrap();
    let page = dict(&doc, &Object::Ref(page));
    let Some(Object::Array(annotations)) = page.get(b"Annots") else {
        panic!("no annotations");
    };
    let [annotation] = annotations.as_slice() else {
        panic!("{} annotations", annotations.len());
    };
    let action = dict(&doc, dict(&doc, annotation).get(b"A").unwrap());
    assert_eq!(
        action.get(b"URI"),
        Some(&Object::String(b"https://example.org/".to_vec()))
    );
}

/// The structure types in reading order.
fn structure(doc: &Document) -> Vec<String> {
    let catalog = dict(doc, &Object::Ref(doc.catalog_id()));
    let root = dict(doc, catalog.get(b"StructTreeRoot").unwrap());
    let mut kinds = Vec::new();
    let mut stack = vec![root.get(b"K").unwrap().clone()];
    while let Some(kid) = stack.pop() {
        let Object::Ref(_) = kid else {
            continue;
        };
        let element = dict(doc, &kid);
        if let Some(Object::Name(kind)) = element.get(b"S") {
            kinds.push(String::from_utf8_lossy(kind.as_bytes()).into_owned());
        }
        if let Some(Object::Array(kids)) = element.get(b"K") {
            stack.extend(kids.iter().rev().cloned());
        }
    }
    kinds
}

#[test]
fn the_structure_of_the_document_is_kept() {
    let doc = converted(TWO_PAGES);
    assert_eq!(
        structure(&doc),
        [
            "Document", "H1", "P", "P", "Link", "L", "LI", "Lbl", "LBody", "LI", "Lbl", "LBody",
            "P"
        ]
    );
}

#[test]
fn the_heading_is_a_bookmark() {
    let doc = converted(TWO_PAGES);
    assert_eq!(
        doc.outline().unwrap(),
        [OutlineEntry {
            level: 1,
            title: "A heading".to_owned(),
            page_index: Some(0),
        }]
    );
}

#[test]
fn a_plain_document_is_described_and_written_whole() {
    let converted = docx_to_pdf(TWO_PAGES, &DocxOptions::default(), &Limits::default()).unwrap();
    assert!(converted.not_described.is_empty());
    assert_eq!(converted.composed.skipped.iter().count(), 0);
}

#[test]
fn what_is_not_a_docx_is_refused() {
    let options = DocxOptions::default();
    assert!(matches!(
        docx_to_pdf(b"not a docx", &options, &Limits::default()),
        Err(Error::Layout)
    ));
    assert!(matches!(
        layout_docx(
            &TWO_PAGES[..TWO_PAGES.len() / 2],
            &options,
            &Limits::default()
        ),
        Err(Error::Layout)
    ));
}

/// `docx` with the first part its central directory lists declared `size` bytes unpacked.
fn declaring(docx: &[u8], size: u32) -> Vec<u8> {
    let mut docx = docx.to_vec();
    let header = docx
        .windows(4)
        .position(|window| window == b"PK\x01\x02")
        .expect("a central directory");
    docx[header + 24..header + 28].copy_from_slice(&size.to_le_bytes());
    docx
}

fn unpacked_too_large(result: Result<impl std::fmt::Debug, Error>) -> bool {
    matches!(
        result,
        Err(Error::Compose(sealwax_compose::Error::Pdf(
            sealwax_core::Error::LimitExceeded {
                what: "max_unpacked_bytes"
            }
        )))
    )
}

#[test]
fn a_document_that_unpacks_past_the_bound_is_refused_before_the_layout() {
    // A part declared at 4 GB, as a ZIP bomb declares its parts: refused on the declaration.
    let bomb = declaring(TWO_PAGES, u32::MAX);
    assert!(unpacked_too_large(docx_to_pdf(
        &bomb,
        &DocxOptions::default(),
        &Limits::default()
    )));
    // The parts of a real document, past a bound set below them.
    let mut limits = Limits::default();
    limits.max_unpacked_bytes = 1_000;
    assert!(unpacked_too_large(layout_docx(
        TWO_PAGES,
        &DocxOptions::default(),
        &limits
    )));
    assert!(layout_docx(TWO_PAGES, &DocxOptions::default(), &Limits::default()).is_ok());
}

#[test]
fn a_passed_deadline_stops_the_conversion() {
    let limits = Limits::default().timeout(Duration::ZERO);
    assert!(matches!(
        docx_to_pdf(TWO_PAGES, &DocxOptions::default(), &limits),
        Err(Error::Compose(sealwax_compose::Error::Pdf(
            sealwax_core::Error::Timeout
        )))
    ));
}
