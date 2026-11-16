//! What a document says about itself and its outline, and pages taken out of it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use sealwax_core::{Document, Object, OpenOptions, OutlineEntry, SaveOptions};

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).unwrap()
}

fn entry(level: usize, title: &str, page_index: Option<usize>) -> OutlineEntry {
    OutlineEntry {
        level,
        title: title.to_owned(),
        page_index,
    }
}

/// Three pages, an outline: One (page 1) with children Été (a string of the name tree, page 3)
/// and Intro (a name, found in the name tree, page 1); Two (a name of `/Dests`, page 2) with
/// children Web (a URI action) and Last (the `LastPage` named action); Broken (a name nobody
/// defines), whose `/Next` leads back to One.
fn outlined() -> Vec<u8> {
    common::pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Dests << /chap2 [4 0 R /Fit] >> \
                 /Names << /Dests 20 0 R >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            (
                10,
                "<< /Type /Outlines /First 11 0 R /Last 13 0 R /Count 3 >>",
            ),
            (
                11,
                "<< /Title (One) /Parent 10 0 R /Next 12 0 R /First 14 0 R /Last 15 0 R \
                 /Dest [3 0 R /XYZ 0 100 0] >>",
            ),
            (
                12,
                "<< /Title (Two) /Parent 10 0 R /Prev 11 0 R /Next 13 0 R /First 16 0 R \
                 /Last 16 0 R /Dest /chap2 >>",
            ),
            (
                13,
                "<< /Title (Broken) /Parent 10 0 R /Prev 12 0 R /Next 11 0 R /Dest /missing >>",
            ),
            (
                14,
                "<< /Title <FEFF00C9007400E9> /Parent 11 0 R /Next 15 0 R /A << /S /GoTo /D (sec) >> >>",
            ),
            (15, "<< /Title (Intro) /Parent 11 0 R /Dest /intro >>"),
            (
                16,
                "<< /Title (Web) /Parent 12 0 R /Next 17 0 R /A << /S /URI /URI (https://example.org) >> >>",
            ),
            (
                17,
                "<< /Title (Last) /Parent 12 0 R /A << /S /Named /N /LastPage >> >>",
            ),
            (20, "<< /Kids [21 0 R] >>"),
            (
                21,
                "<< /Limits [(a) (z)] /Names [(intro) [3 0 R /Fit] (sec) << /D [5 0 R /Fit] >>] >>",
            ),
        ],
        false,
    )
}

#[test]
fn the_outline_is_flat_depth_first_with_named_destinations_resolved() {
    assert_eq!(
        open(outlined()).outline().unwrap(),
        [
            entry(1, "One", Some(0)),
            entry(2, "Été", Some(2)),
            entry(2, "Intro", Some(0)),
            entry(1, "Two", Some(1)),
            entry(2, "Web", None),
            entry(2, "Last", Some(2)),
            entry(1, "Broken", None),
        ]
    );
}

#[test]
fn a_document_without_outline_has_none_and_a_self_parented_one_ends() {
    let plain = common::pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
        ],
        false,
    );
    assert!(open(plain).outline().unwrap().is_empty());
    // An entry whose child is itself, and a destination naming a page object of no page.
    let looping = common::pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            (10, "<< /First 11 0 R >>"),
            (
                11,
                "<< /Title (Loop) /First 11 0 R /Next 11 0 R /Dest [10 0 R /Fit] >>",
            ),
        ],
        false,
    );
    assert_eq!(open(looping).outline().unwrap(), [entry(1, "Loop", None)]);
}

fn xmp(part: &str) -> String {
    let data = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\" \
         pdfaid:part=\"{part}\" pdfaid:conformance=\"A\"/></rdf:RDF></x:xmpmeta>"
    );
    format!(
        "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

#[test]
fn metadata_reads_the_claims_the_form_and_the_information_as_written() {
    let metadata_stream = xmp("3");
    let data = common::pdf_with_trailer(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R /AcroForm << /Fields [7 0 R] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [7 0 R] >>",
            ),
            (6, &metadata_stream),
            (
                7,
                "<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1) /Rect [0 0 0 0] /P 3 0 R >>",
            ),
            (
                8,
                "<< /Title (Report) /Author <FEFF00C9006C006F00640069006500> \
                 /CreationDate (D:20240131120000+01'00') /Trapped /False /Pages 12 /Producer 9 0 R >>",
            ),
            (9, "(sealwax)"),
        ],
        false,
        "/Info 8 0 R",
    );
    let metadata = open(data).metadata();
    assert_eq!(metadata.version, "1.7");
    assert!(!metadata.encrypted && !metadata.linearized);
    assert!(metadata.has_forms && metadata.has_signatures);
    assert_eq!(metadata.pdfa.as_deref(), Some("3A"));
    let info: Vec<(&str, &str)> = metadata
        .info
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        info,
        [
            ("Author", "Élodie"),
            ("CreationDate", "D:20240131120000+01'00'"),
            ("Producer", "sealwax"),
            ("Title", "Report"),
            ("Trapped", "False"),
        ]
    );
}

#[test]
fn a_linearized_document_says_so_and_a_bare_one_says_little() {
    let linearized = common::pdf(
        &[
            (
                9,
                "<< /Linearized 1 /L 1000 /O 3 /E 500 /N 1 /T 900 /H [100 50] >>",
            ),
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
        ],
        false,
    );
    let metadata = open(linearized).metadata();
    assert!(metadata.linearized);
    assert!(!metadata.has_forms && !metadata.has_signatures && metadata.pdfa.is_none());
    assert!(metadata.info.is_empty());
}

/// Five pages of different widths; page 2 links to a web page and to page 5.
fn five_pages() -> Vec<u8> {
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (
            2,
            "<< /Type /Pages /Kids [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R] /Count 5 >>".to_owned(),
        ),
        (
            20,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /A << /S /URI /URI (https://example.org) >> >>"
                .to_owned(),
        ),
        (
            21,
            "<< /Type /Annot /Subtype /Link /Rect [50 0 100 50] /Dest [14 0 R /Fit] >>".to_owned(),
        ),
    ];
    for page in 0..5u32 {
        let content = format!("0 0 {} 10 re f", 10 * (page + 1));
        objects.push((
            30 + page,
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ));
        let annots = if page == 1 {
            " /Annots [20 0 R 21 0 R]"
        } else {
            ""
        };
        objects.push((
            10 + page,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 200] /Contents {} 0 R{annots} >>",
                100 + page,
                30 + page
            ),
        ));
    }
    let objects: Vec<(u32, &str)> = objects.iter().map(|(n, b)| (*n, b.as_str())).collect();
    common::pdf(&objects, false)
}

#[test]
fn pages_taken_out_keep_their_content_and_links() {
    let doc = open(five_pages());
    let extracted = doc.extract_pages(1..3).unwrap();
    let reopened = open(extracted.save(&SaveOptions::default()).unwrap());
    let widths: Vec<f64> = reopened
        .geometry()
        .iter()
        .map(|g| g.media_box.width())
        .collect();
    assert_eq!(widths, [101.0, 102.0]);
    let page = reopened.page_ids()[0].unwrap();
    let Some(Object::Dict(page)) = reopened.object(page).unwrap() else {
        panic!("a page");
    };
    let annots = page.get(b"Annots").and_then(Object::as_array).unwrap();
    assert_eq!(annots.len(), 2);
    let link = |at: usize| -> sealwax_core::Dict {
        match annots[at]
            .as_ref_id()
            .map(|id| reopened.object(id).unwrap())
        {
            Some(Some(Object::Dict(dict))) => dict,
            _ => panic!("an annotation"),
        }
    };
    let action = link(0)
        .get(b"A")
        .and_then(Object::as_dict)
        .cloned()
        .unwrap();
    assert_eq!(
        action.get(b"URI").and_then(Object::as_string),
        Some(&b"https://example.org"[..])
    );
    // Page 5 was left out: its link leads nowhere, and the page was not copied.
    let dest = link(1)
        .get(b"Dest")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
        .unwrap();
    let target = dest[0].as_ref_id().map(|id| reopened.object(id).unwrap());
    assert!(
        matches!(target, None | Some(None | Some(Object::Null))),
        "{dest:?}"
    );
    assert_eq!(reopened.page_count(), 2);
}

#[test]
fn an_empty_or_outside_range_is_refused() {
    let doc = open(five_pages());
    for range in [2..2, 4..6, 5..6] {
        assert!(matches!(
            doc.extract_pages(range),
            Err(sealwax_core::Error::InvalidArgument { what: "page range" })
        ));
    }
    assert_eq!(doc.extract_pages(0..5).unwrap().page_count(), 5);
}
