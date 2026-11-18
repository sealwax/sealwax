//! The files a document embeds: listed, extracted, added.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use sealwax_core::{Document, Error, NewAttachment, Object, OpenOptions, Rect, SaveOptions};

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).unwrap()
}

fn reopened(doc: &Document) -> Document {
    open(doc.save(&SaveOptions::default()).unwrap())
}

/// A tree of two leaves (b.xml, then a.txt and "é.pdf"), a file specification that embeds
/// nothing, and a file attachment annotation on page 2.
fn attached() -> Vec<u8> {
    common::pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Kids [5 0 R 6 0 R] >> >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>"),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [13 0 R] >>",
            ),
            (5, "<< /Limits [(b.xml) (b.xml)] /Names [(b.xml) 7 0 R] >>"),
            (
                6,
                "<< /Limits [(a.txt) (outside)] /Names [(a.txt) 8 0 R (outside) 11 0 R (\\351.pdf) 9 0 R] >>",
            ),
            (
                7,
                "<< /Type /Filespec /F (b.xml) /EF << /F 10 0 R >> /AFRelationship /Data >>",
            ),
            (
                8,
                "<< /Type /Filespec /F (a.txt) /UF <FEFF0061002E007400780074> /Desc (Notes) /EF << /F 12 0 R >> >>",
            ),
            (
                9,
                "<< /Type /Filespec /UF <FEFF00E9002E007000640066> /EF << /UF 14 0 R >> >>",
            ),
            (
                10,
                "<< /Type /EmbeddedFile /Subtype /text#2Fxml /Params << /Size 7 /ModDate (D:20261118120000Z) >> /Length 7 >>\nstream\n<a>1</a>\nendstream",
            ),
            (11, "<< /Type /Filespec /F (outside.pdf) >>"),
            (
                12,
                "<< /Type /EmbeddedFile /Filter /ASCIIHexDecode /Length 11 >>\nstream\n6869207468>\nendstream",
            ),
            (
                13,
                "<< /Type /Annot /Subtype /FileAttachment /Rect [10 20 30 40] /FS << /Type /Filespec /F (scan.png) /EF << /F 15 0 R >> >> >>",
            ),
            (14, "<< /Length 4 >>\nstream\n%PDF\nendstream"),
            (
                15,
                "<< /Subtype /image#2Fpng /Length 3 >>\nstream\nPNG\nendstream",
            ),
        ],
        false,
    )
}

#[test]
fn the_files_a_document_embeds_are_listed_and_extracted() {
    let doc = open(attached());
    let files = doc.attachments().unwrap();
    let names: Vec<(&str, Option<&str>)> = files
        .iter()
        .map(|f| (f.name.as_str(), f.key.as_deref()))
        .collect();
    // Keys in order, the one embedding nothing left out, then the annotation's.
    assert_eq!(
        names,
        [
            ("a.txt", Some("a.txt")),
            ("b.xml", Some("b.xml")),
            ("é.pdf", Some("é.pdf")),
            ("scan.png", None),
        ]
    );
    let xml = &files[1];
    assert_eq!(xml.mime_type.as_deref(), Some("text/xml"));
    assert_eq!(
        (xml.size, xml.modified.as_deref()),
        (Some(7), Some("D:20261118120000Z"))
    );
    assert_eq!(xml.relationship.as_deref(), Some("Data"));
    assert_eq!(files[0].description.as_deref(), Some("Notes"));
    assert_eq!(
        (files[3].page_index, files[3].rect),
        (Some(1), Some(Rect::from_corners(10.0, 20.0, 30.0, 40.0)))
    );
    assert_eq!(doc.attachment_data(0).unwrap(), b"hi th");
    assert_eq!(doc.attachment_data(1).unwrap(), b"<a>1</a>");
    assert_eq!(doc.attachment_data(3).unwrap(), b"PNG");
    assert!(matches!(
        doc.attachment_data(4),
        Err(Error::InvalidArgument {
            what: "attachment index"
        })
    ));
}

#[test]
fn a_file_added_is_listed_and_reads_back() {
    let mut doc = open(attached());
    let mut file = NewAttachment::new("facture.xml", b"<invoice/>".to_vec());
    file.description = Some("Factur-X".to_owned());
    file.relationship = Some("Alternative".to_owned());
    file.modified = Some("D:20261118100000Z".to_owned());
    doc.add_attachment(&file).unwrap();
    doc.add_attachment(&NewAttachment::new("Résumé.bin", vec![0, 1, 2]))
        .unwrap();
    // Listed at once, before any save.
    assert_eq!(doc.attachments().unwrap().len(), 6);
    let doc = reopened(&doc);
    let files = doc.attachments().unwrap();
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "a.txt",
            "b.xml",
            "facture.xml",
            "é.pdf",
            "Résumé.bin",
            "scan.png"
        ]
    );
    let invoice = &files[2];
    assert_eq!(
        (
            invoice.mime_type.as_deref(),
            invoice.relationship.as_deref(),
            invoice.description.as_deref(),
            invoice.size,
            invoice.modified.as_deref()
        ),
        (
            Some("text/xml"),
            Some("Alternative"),
            Some("Factur-X"),
            Some(10),
            Some("D:20261118100000Z")
        )
    );
    assert_eq!(doc.attachment_data(2).unwrap(), b"<invoice/>");
    assert_eq!(
        files[4].mime_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(doc.attachment_data(4).unwrap(), [0, 1, 2]);
    // Associated with the document, as PDF/A-3 asks.
    let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
    let associated = match catalog.as_dict().unwrap().get(b"AF") {
        Some(Object::Array(files)) => files.len(),
        other => panic!("{other:?}"),
    };
    assert_eq!(associated, 2);
}

#[test]
fn files_that_cannot_be_added_are_refused() {
    let mut doc = open(attached());
    for (file, what) in [
        (NewAttachment::new("", vec![1]), "attachment name"),
        (NewAttachment::new("b.xml", vec![1]), "attachment name"),
        (
            {
                let mut file = NewAttachment::new("x.bin", vec![1]);
                file.mime_type = Some("text plain".to_owned());
                file
            },
            "attachment MIME type",
        ),
        (
            {
                let mut file = NewAttachment::new("x.bin", vec![1]);
                file.relationship = Some("Friend".to_owned());
                file
            },
            "attachment relationship",
        ),
    ] {
        match doc.add_attachment(&file) {
            Err(Error::InvalidArgument { what: refused }) => assert_eq!(refused, what),
            other => panic!("{what}: {other:?}"),
        }
    }
    // A document without any: its tree made.
    let mut doc = Document::empty().unwrap();
    doc.add_page(100.0, 100.0).unwrap();
    doc.add_attachment(&NewAttachment::new("a.txt", b"a".to_vec()))
        .unwrap();
    assert_eq!(reopened(&doc).attachment_data(0).unwrap(), b"a");
}

/// A one-page document whose `/EmbeddedFiles` is `tree` (object 3), with `extra` objects from 4,
/// and `catalog` added to its catalog.
fn with_tree(tree: &str, extra: &[String], catalog: &str) -> Document {
    let mut objects = vec![
        (
            1,
            format!(
                "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 3 0 R >> {catalog} >>"
            ),
        ),
        (2, "<< /Type /Pages /Kids [99 0 R] /Count 1 >>".to_owned()),
        (3, tree.to_owned()),
    ];
    for (index, body) in extra.iter().enumerate() {
        objects.push((u32::try_from(index).unwrap() + 4, body.clone()));
    }
    objects.push((
        99,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".to_owned(),
    ));
    let objects: Vec<(u32, &str)> = objects.iter().map(|(n, b)| (*n, b.as_str())).collect();
    open(common::pdf(&objects, false))
}

#[test]
fn a_tree_that_cannot_be_read_whole_is_not_rewritten() {
    let spec = |name: &str, stream: u32| {
        format!("<< /Type /Filespec /F ({name}) /EF << /F {stream} 0 R >> >>")
    };
    let file = "<< /Length 1 >>\nstream\nx\nendstream".to_owned();
    // A file 40 levels down, past what is followed.
    let mut extra: Vec<String> = (0..40)
        .map(|level| format!("<< /Kids [{} 0 R] >>", level + 5))
        .collect();
    extra[39] = format!("<< /Names [(deep.txt) {} 0 R] >>", 44);
    extra.push(spec("deep.txt", 45));
    extra.push(file.clone());
    let mut deep = with_tree("<< /Kids [4 0 R] >>", &extra, "");
    // A key that is a name, not a string.
    let mut named = with_tree(
        "<< /Names [/b.txt 4 0 R (c.txt) 6 0 R] >>",
        &[spec("b.txt", 5), file.clone(), spec("c.txt", 5)],
        "",
    );
    for doc in [&mut deep, &mut named] {
        // Read, what can be is listed.
        doc.attachments().unwrap();
        assert!(matches!(
            doc.add_attachment(&NewAttachment::new("new.txt", b"n".to_vec())),
            Err(Error::Unsupported { .. })
        ));
        assert!(!doc.is_modified());
    }
    assert_eq!(named.attachments().unwrap().len(), 1);
}

#[test]
fn a_document_claiming_pdfa_1_or_2_is_given_no_file() {
    let xmp = |part: &str| {
        let data = format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><pdfaid:part>{part}</pdfaid:part>\
             <pdfaid:conformance>B</pdfaid:conformance></x:xmpmeta>"
        );
        format!(
            "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        )
    };
    for (part, refused) in [("1", true), ("2", true), ("3", false)] {
        let mut doc = with_tree("<< /Names [] >>", &[xmp(part)], "/Metadata 4 0 R");
        let added = doc.add_attachment(&NewAttachment::new("a.txt", b"a".to_vec()));
        assert_eq!(
            matches!(added, Err(Error::Unsupported { .. })),
            refused,
            "PDF/A-{part}: {added:?}"
        );
    }
}
