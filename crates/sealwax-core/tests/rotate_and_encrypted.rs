//! Pages turned where they are, keeping what the document has; encrypted documents refused when
//! asked.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, Error, Object, OpenOptions, SaveOptions};

const OWNER_PROTECTED: &[u8] = include_bytes!("data/owner-protected.pdf");

fn pdf(objects: &[&str]) -> Vec<u8> {
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
            "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn pages_turned_in_place_keep_the_outline_and_the_information() {
    // Two pages inheriting a rotation of 90 from their parent; an outline; information.
    let data = pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /PageMode /UseOutlines >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /Rotate 90 /MediaBox [0 0 600 800] >>",
        "<< /Type /Page /Parent 2 0 R >>",
        "<< /Type /Page /Parent 2 0 R >>",
        "<< /Type /Outlines /Count 0 >>",
        "<< /Title (Kept) >>",
    ]);
    let mut doc = Document::open(data, &OpenOptions::default()).unwrap();
    doc.rotate_pages(&[(0, 1), (1, -1), (1, -1)]).unwrap();
    let turned: Vec<u16> = doc
        .geometry()
        .iter()
        .map(|g| g.rotation.degrees())
        .collect();
    assert_eq!(turned, [180, 270]);
    let saved = Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap();
    let turned: Vec<u16> = saved
        .geometry()
        .iter()
        .map(|g| g.rotation.degrees())
        .collect();
    assert_eq!(turned, [180, 270]);
    let catalog = saved.object(saved.catalog_id()).unwrap().unwrap();
    assert!(catalog.as_dict().unwrap().contains_key(b"Outlines"));
    assert_eq!(
        saved.metadata().info.get("Title").map(String::as_str),
        Some("Kept")
    );
    // A page the document does not have: nothing turned.
    let before: Vec<u16> = doc
        .geometry()
        .iter()
        .map(|g| g.rotation.degrees())
        .collect();
    assert!(doc.rotate_pages(&[(0, 1), (2, 1)]).is_err());
    let after: Vec<u16> = doc
        .geometry()
        .iter()
        .map(|g| g.rotation.degrees())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn an_encrypted_document_is_refused_when_asked_only() {
    let refused = Document::open(
        OWNER_PROTECTED,
        &OpenOptions::default().refuse_encrypted(true),
    );
    assert!(matches!(refused, Err(Error::Encrypted)), "{refused:?}");
    // By default it opens, its user password being empty, and is saved without encryption.
    let doc = Document::open(OWNER_PROTECTED, &OpenOptions::default()).unwrap();
    assert!(doc.is_encrypted());
    let saved = doc.save(&SaveOptions::default()).unwrap();
    assert!(!saved.windows(8).any(|w| w == b"/Encrypt"));
    // A document that is not encrypted opens either way.
    let plain = Document::open(saved, &OpenOptions::default().refuse_encrypted(true)).unwrap();
    assert!(matches!(
        plain.object(plain.catalog_id()).unwrap(),
        Some(Object::Dict(_))
    ));
}
