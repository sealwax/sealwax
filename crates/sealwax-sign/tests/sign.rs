#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use sealwax_core::{Document, Object, OpenOptions};
use sealwax_sign::{Algorithm, Error, Kind, Options, Signer, prepare, sign};
use sha2::{Digest, Sha256};

use common::{TestSigner, document, verify};

#[test]
fn a_certification_then_an_approval_both_verify() {
    let signer = TestSigner::new();
    let base = document();
    let mut options = Options::new("Signature1", Kind::certification());
    options.reason = Some("Document certification".to_owned());
    options.location = Some("sealwax test".to_owned());
    options.signing_time = Some(1_794_746_096);
    let certified = sign(&base, &signer, &options, None).unwrap().pdf;
    assert!(certified.starts_with(&base), "an incremental revision");
    let options = Options::new("Signature2", Kind::Approval);
    let approved = sign(&certified, &signer, &options, None).unwrap().pdf;
    assert!(approved.starts_with(&certified));
    assert_eq!(
        verify(&approved, signer.public_key()),
        [
            ("Signature1".to_owned(), true),
            ("Signature2".to_owned(), true)
        ]
    );
    // The certification is the DocMDP signature, P=3.
    let doc = Document::open(approved, &OpenOptions::default()).unwrap();
    let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
    let perms = catalog
        .as_dict()
        .unwrap()
        .get(b"Perms")
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let docmdp = doc
        .object(perms.get(b"DocMDP").unwrap().as_ref_id().unwrap())
        .unwrap()
        .unwrap();
    let docmdp = docmdp.as_dict().unwrap();
    assert_eq!(
        docmdp.get(b"M").unwrap().as_string(),
        Some(&b"D:20261115123456+00'00'"[..])
    );
    let reference = docmdp.get(b"Reference").unwrap().as_array().unwrap()[0].clone();
    let params = reference
        .as_dict()
        .unwrap()
        .get(b"TransformParams")
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(params.get(b"P").and_then(Object::as_i64), Some(3));
}

#[test]
fn a_second_certification_a_field_name_twice_and_too_little_room_are_refused() {
    let signer = TestSigner::new();
    let certified = sign(
        &document(),
        &signer,
        &Options::new("A", Kind::certification()),
        None,
    )
    .unwrap()
    .pdf;
    assert!(matches!(
        sign(
            &certified,
            &signer,
            &Options::new("B", Kind::certification()),
            None
        ),
        Err(Error::NotFirstSignature)
    ));
    assert!(matches!(
        sign(
            &certified,
            &signer,
            &Options::new("A", Kind::Approval),
            None
        ),
        Err(Error::FieldExists(_))
    ));
    // A name outside ASCII is the same name in whichever encoding the file holds it, and a full
    // name is one partial name per level: a period or nothing is no name (external review: two
    // fields "Signé" were accepted).
    let accented = sign(
        &certified,
        &signer,
        &Options::new("Signé", Kind::Approval),
        None,
    )
    .unwrap()
    .pdf;
    assert!(matches!(
        sign(&accented, &signer, &Options::new("Signé", Kind::Approval), None),
        Err(Error::FieldExists(name)) if name == "Signé"
    ));
    for name in ["", "A.B"] {
        assert!(matches!(
            sign(
                &certified,
                &signer,
                &Options::new(name, Kind::Approval),
                None
            ),
            Err(Error::FieldName(_))
        ));
    }
    let mut tight = Options::new("C", Kind::Approval);
    tight.reserved_contents_bytes = 64;
    assert!(matches!(
        sign(&certified, &signer, &tight, None),
        Err(Error::ContentsTooLarge { reserved: 64, .. })
    ));
}

#[test]
fn the_digest_is_that_of_the_message_the_signer_gets() {
    let signer = TestSigner::new();
    let prepared = prepare(
        &document(),
        &signer.certificate,
        &[],
        Algorithm::EcdsaSha256,
        &Options::new("S", Kind::Approval),
    )
    .unwrap();
    let message = prepared.message().unwrap();
    assert_eq!(
        *prepared.digest(),
        <[u8; 32]>::from(Sha256::digest(&message))
    );
    let value = signer.sign(&message, prepared.digest()).unwrap();
    let signed = prepared.finalize(&value, None).unwrap();
    assert_eq!(
        verify(&signed, signer.public_key()),
        [("S".to_owned(), true)]
    );
}
