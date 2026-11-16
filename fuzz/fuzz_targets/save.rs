#![no_main]
//! Whatever the bytes: saving never panics, and what a save returns opens again with the same
//! pages and geometry. A save may refuse a damaged input (an error), never return a file that
//! does not read back: that file would be the start of every signature that follows.
//!
//!     cargo +nightly fuzz run save fuzz/corpus/save corpus/public -- -max_len=1048576

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, Object, OpenOptions, SaveOptions, XrefFormat};

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions::default().max_file_bytes(4 << 20);
    let Ok(mut document) = Document::open(data.to_vec(), &options) else {
        return;
    };
    let pages = document.page_count();
    let geometry = document.geometry();
    for format in [XrefFormat::Stream, XrefFormat::Table] {
        let Ok(saved) = document.save(&SaveOptions::default().xref(format)) else {
            continue;
        };
        let reopened = Document::open(saved, &OpenOptions::default())
            .unwrap_or_else(|error| panic!("{format:?} save does not reopen: {error}"));
        assert_eq!(reopened.page_count(), pages, "{format:?}");
        assert_eq!(reopened.geometry(), geometry, "{format:?}");
    }
    let catalog = document.catalog_id();
    if let Ok(Some(object)) = document.object(catalog) {
        document.set_object(catalog, object);
    }
    if document.add_object(Object::Null).is_err() {
        return;
    }
    if let Ok(updated) = document.save_incremental() {
        assert!(
            updated.starts_with(data),
            "the input is a prefix of its update"
        );
        let reopened = Document::open(updated, &OpenOptions::default())
            .unwrap_or_else(|error| panic!("incremental save does not reopen: {error}"));
        assert_eq!(reopened.page_count(), pages, "incremental");
    }
});
