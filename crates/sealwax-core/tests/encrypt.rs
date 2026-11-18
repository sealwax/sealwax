//! Saving encrypted: RC4 128-bit, AES-128 and AES-256, with or without a user password, read
//! back with each password, and described as PyMuPDF describes it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use sealwax_core::{
    Cipher, Document, Encrypt, Error, OpenOptions, PERMISSIONS, SaveOptions, XrefFormat,
};

fn document() -> Document {
    let mut doc = Document::empty().unwrap();
    doc.add_page(600.0, 800.0).unwrap();
    doc.add_page(300.0, 400.0).unwrap();
    let info = BTreeMap::from([("Title".to_owned(), "Chiffré".to_owned())]);
    doc.set_info(&info).unwrap();
    doc
}

fn encrypt(cipher: Cipher, user: &str) -> Encrypt {
    Encrypt {
        cipher,
        owner_password: "owner".to_owned(),
        user_password: user.to_owned(),
        permissions: 4 | 256,
        seed: [7; 32],
    }
}

fn open(data: &[u8], password: Option<&str>) -> Result<Document, Error> {
    let mut options = OpenOptions::default();
    if let Some(password) = password {
        options = options.password(password.to_owned());
    }
    Document::open(data.to_vec(), &options)
}

#[test]
fn documents_saved_encrypted_open_with_each_password() {
    let doc = document();
    for (cipher, description) in [
        (Cipher::Rc4, "Standard V2 R3 128-bit RC4"),
        (Cipher::Aes128, "Standard V4 R4 128-bit AES"),
        (Cipher::Aes256, "Standard V5 R6 256-bit AES"),
    ] {
        for user in ["", "user"] {
            for xref in [XrefFormat::Stream, XrefFormat::Table] {
                let saved = doc
                    .save(
                        &SaveOptions::default()
                            .xref(xref)
                            .encrypt(encrypt(cipher, user)),
                    )
                    .unwrap();
                // hayro opens a document of revision 2 to 4 with its user password only (it
                // does not recover it from the owner's); qpdf and PyMuPDF take both.
                let owner: &[&str] = if cipher == Cipher::Aes256 {
                    &["owner"]
                } else {
                    &[]
                };
                let passwords: Vec<&str> = if user.is_empty() {
                    [&[""][..], owner].concat()
                } else {
                    // Without its password, it does not open.
                    assert!(
                        matches!(open(&saved, None), Err(Error::PasswordRequired)),
                        "{description} {xref:?}"
                    );
                    [&["user"][..], owner].concat()
                };
                for password in passwords {
                    let read = open(&saved, Some(password)).unwrap();
                    assert_eq!(read.page_count(), 2);
                    let metadata = read.metadata();
                    assert_eq!(
                        metadata.encryption.map(|e| e.description()).as_deref(),
                        Some(description)
                    );
                    // Strings are decrypted: the title reads back.
                    assert_eq!(
                        metadata.info.get("Title").map(String::as_str),
                        Some("Chiffré")
                    );
                }
                assert!(matches!(
                    open(&saved, Some("wrong")),
                    Err(Error::PasswordRequired)
                ));
            }
        }
    }
}

#[test]
fn an_owner_password_and_known_permissions_are_required() {
    let doc = document();
    let mut no_owner = encrypt(Cipher::Aes256, "");
    no_owner.owner_password.clear();
    assert!(matches!(
        doc.save(&SaveOptions::default().encrypt(no_owner)),
        Err(Error::InvalidArgument {
            what: "owner password"
        })
    ));
    let mut unknown = encrypt(Cipher::Aes128, "");
    unknown.permissions = PERMISSIONS | 1;
    assert!(matches!(
        doc.save(&SaveOptions::default().encrypt(unknown)),
        Err(Error::InvalidArgument {
            what: "permissions"
        })
    ));
}

#[test]
fn encryption_options_never_show_the_passwords() {
    let shown = format!("{:?}", encrypt(Cipher::Aes256, "user"));
    assert!(
        !shown.contains("owner") && !shown.contains("user"),
        "{shown}"
    );
}
