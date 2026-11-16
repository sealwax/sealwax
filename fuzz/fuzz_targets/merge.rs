#![no_main]
//! Whatever the bytes: a document merged with itself (flattened, its pages appended twice to an
//! empty document) never panics, and saves into a file that opens again with twice its pages and
//! twice its geometry.
//!
//!     cargo +nightly fuzz run merge fuzz/corpus/merge corpus/public -- -max_len=1048576

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, OpenOptions, PageGeometry, SaveOptions};

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions::default().max_file_bytes(4 << 20);
    let (Ok(first), Ok(second)) = (
        Document::open(data.to_vec(), &options),
        Document::open(data.to_vec(), &options),
    ) else {
        return;
    };
    let geometry = first.geometry();
    let Ok((merged, _)) = sealwax_annots::merge(vec![first, second]) else {
        return;
    };
    let Ok(saved) = merged.save(&SaveOptions::default()) else {
        return;
    };
    let reopened = Document::open(saved, &OpenOptions::default())
        .unwrap_or_else(|error| panic!("merged document does not reopen: {error}"));
    let expected: Vec<PageGeometry> = geometry
        .iter()
        .chain(&geometry)
        .enumerate()
        .map(|(index, page)| PageGeometry {
            page_number: index + 1,
            ..*page
        })
        .collect();
    assert_eq!(reopened.geometry(), expected);
});
