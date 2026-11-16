#![no_main]
//! Whatever the bytes: opening a document, then reading its pages, geometry and census, and
//! checking and measuring what interpreting its pages would cost, never panics. hayro-syntax
//! already refuses what it cannot read; this keeps our own code honest.
//!
//!     cargo +nightly fuzz run open fuzz/corpus/open corpus/public -- -max_len=1048576
//!
//! The first directory collects the inputs the fuzzer finds; the next ones are read-only seeds.

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, OpenOptions};

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions::default().max_file_bytes(4 << 20);
    if let Ok(document) = Document::open(data.to_vec(), &options) {
        let _ = document.page_count();
        let _ = document.declared_page_count();
        let _ = document.geometry();
        let _ = document.census();
        let _ = document.check_pages();
        let _ = document.measure_pages();
    }
});
