//! The integrity of the signatures a document holds, and what sealwax refuses to add over one
//! that is not intact.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use sealwax_sign::{
    Broken, Error, Integrity, Kind, Options, SignatureKind, sign, verify_signatures,
};

use common::{TestSigner, document};

fn certified_then_approved() -> (TestSigner, Vec<u8>) {
    let signer = TestSigner::new();
    let certified = sign(
        &document(),
        &signer,
        &Options::new("C", Kind::certification()),
        None,
    )
    .unwrap()
    .pdf;
    let approved = sign(
        &certified,
        &signer,
        &Options::new("A", Kind::Approval),
        None,
    )
    .unwrap()
    .pdf;
    (signer, approved)
}

#[test]
fn signatures_are_intact_and_know_what_followed_them() {
    assert!(verify_signatures(&document()).unwrap().is_empty());
    let (_, pdf) = certified_then_approved();
    let checks = verify_signatures(&pdf).unwrap();
    assert_eq!(checks.len(), 2);
    let certification = checks.iter().find(|c| c.field == "C").unwrap();
    let approval = checks.iter().find(|c| c.field == "A").unwrap();
    assert_eq!(certification.kind, SignatureKind::Certification);
    assert_eq!(approval.kind, SignatureKind::Approval);
    for check in &checks {
        assert_eq!(check.integrity, Integrity::Intact, "{}", check.field);
        assert_eq!(check.sub_filter, "ETSI.CAdES.detached");
        assert_eq!(check.signer.as_deref(), Some("CN=sealwax test,O=sealwax"));
        assert_eq!(check.timestamp_time, None);
    }
    assert_eq!(certification.later_revisions, 1, "the approval's revision");
    assert_eq!(approval.later_revisions, 0);
}

/// A byte changed in what a signature covers breaks it, and nothing is signed over it unless
/// asked; a byte range that leaves part of the revision out breaks it too.
#[test]
fn a_changed_document_breaks_its_signatures_and_is_refused() {
    let (signer, pdf) = certified_then_approved();
    // The binary comment of the header, which every signature covers.
    let mut changed = pdf.clone();
    let at = changed.iter().position(|b| *b == b'%').unwrap() + 10;
    changed[at] ^= 0x01;
    let checks = verify_signatures(&changed).unwrap();
    assert!(
        checks
            .iter()
            .all(|check| check.integrity == Integrity::Broken(Broken::Digest)),
        "{checks:?}"
    );
    match sign(&changed, &signer, &Options::new("B", Kind::Approval), None) {
        Err(Error::BrokenSignature { problem, .. }) => assert_eq!(problem, Broken::Digest),
        other => panic!("not refused: {:?}", other.map(|s| s.pdf.len())),
    }
    let mut anyway = Options::new("B", Kind::Approval);
    anyway.allow_broken_signatures = true;
    assert!(sign(&changed, &signer, &anyway, None).is_ok());

    // The approval's byte range made to stop short of its revision's end.
    let key = b"/ByteRange [0 ";
    let range_at = pdf.windows(key.len()).rposition(|w| w == key).unwrap();
    let close = range_at + pdf[range_at..].iter().position(|b| *b == b']').unwrap();
    let original = std::str::from_utf8(&pdf[range_at..close]).unwrap();
    let numbers: Vec<usize> = original["/ByteRange [".len()..]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    let shorter = format!(
        "/ByteRange [0 {} {} {}",
        numbers[1],
        numbers[2],
        numbers[3] - 8
    );
    let padded = format!("{shorter:<width$}", width = original.len());
    let mut cut = pdf.clone();
    cut[range_at..close].copy_from_slice(padded.as_bytes());
    let checks = verify_signatures(&cut).unwrap();
    let approval = checks.iter().find(|c| c.field == "A").unwrap();
    assert_eq!(approval.integrity, Integrity::Broken(Broken::ByteRange));
}

#[test]
fn revision_counts_follow_the_trailers_when_an_object_contains_pdf_text() {
    let (_, bytes) = certified_then_approved();
    let mut doc =
        sealwax_core::Document::open(bytes, &sealwax_core::OpenOptions::default()).unwrap();
    doc.set_info(
        &[(
            "Title".to_owned(),
            "Documenting the %%EOF marker".to_owned(),
        )]
        .into(),
    )
    .unwrap();
    let checks = verify_signatures(&doc.save_incremental().unwrap()).unwrap();
    assert_eq!(
        checks.iter().map(|s| s.later_revisions).collect::<Vec<_>>(),
        [2, 1]
    );
    assert!(checks.iter().all(|s| s.is_intact()));
}
