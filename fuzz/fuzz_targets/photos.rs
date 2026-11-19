#![no_main]
//! Whatever the bytes: making them a page never panics, and a page made saves and reads back.
//!
//!     cargo +nightly fuzz run photos fuzz/corpus/photos crates/sealwax-annots/tests/data \
//!         -- -max_len=65536

use libfuzzer_sys::fuzz_target;
use sealwax_annots::{insert_image, photos_document};
use sealwax_core::{Document, Limits, OpenOptions, Rect, SaveOptions};

fuzz_target!(|data: &[u8]| {
    let mut limits = Limits::default();
    // A PNG is decoded: a million pixels keeps a run fast.
    limits.max_image_pixels = 1 << 20;
    let Ok((mut doc, report)) = photos_document(&[data], limits) else {
        return;
    };
    assert_eq!((report.added, report.unreadable.len()), (1, 0));
    let rect = Rect::from_corners(10.0, 20.0, 130.0, 100.0);
    insert_image(&mut doc, 0, rect, data, true).expect("a readable photo can be placed");
    let saved = doc.save(&SaveOptions::default()).expect("a page made saves");
    let reopened = Document::open(saved, &OpenOptions::default()).expect("and reads back");
    assert_eq!(reopened.page_count(), 1);
});
