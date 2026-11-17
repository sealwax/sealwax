#![no_main]
//! Whatever the bytes: making them a page never panics, and a page made saves and reads back.
//!
//!     cargo +nightly fuzz run photos fuzz/corpus/photos crates/sealwax-annots/tests/data \
//!         -- -max_len=65536

use libfuzzer_sys::fuzz_target;
use sealwax_annots::photos_document;
use sealwax_core::{Document, Limits, OpenOptions, SaveOptions};

fuzz_target!(|data: &[u8]| {
    let mut limits = Limits::default();
    // A PNG is decoded: a million pixels keeps a run fast.
    limits.max_image_pixels = 1 << 20;
    let Ok((doc, report)) = photos_document(&[data], limits) else {
        return;
    };
    assert_eq!((report.added, report.unreadable.len()), (1, 0));
    let saved = doc.save(&SaveOptions::default()).expect("a page made saves");
    let reopened = Document::open(saved, &OpenOptions::default()).expect("and reads back");
    assert_eq!(reopened.page_count(), 1);
});
