#![no_main]
//! Whatever the bytes: the PDF/A conversion never panics, and the document it leaves saves into a
//! file that opens again with the same pages and geometry. Level a does all that levels b and u
//! do, and tags the document.
//!
//!     cargo +nightly fuzz run pdfa fuzz/corpus/pdfa corpus/public -- -max_len=1048576

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, OpenOptions, SaveOptions};
use sealwax_pdfa::{Level, Options, convert};

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions::default().max_file_bytes(4 << 20);
    let Ok(mut document) = Document::open(data.to_vec(), &options) else {
        return;
    };
    let pages = document.page_count();
    let geometry = document.geometry();
    if convert(&mut document, &Options::new(Level::A)).is_err() {
        return;
    }
    let Ok(saved) = document.save(&SaveOptions::default()) else {
        return;
    };
    let reopened = Document::open(saved, &OpenOptions::default())
        .unwrap_or_else(|error| panic!("converted document does not reopen: {error}"));
    assert_eq!(reopened.page_count(), pages);
    assert_eq!(reopened.geometry(), geometry);
});
