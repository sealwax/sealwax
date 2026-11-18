#![no_main]
//! Whatever docxide-pdf lays out of a DOCX document: its pages compose without a panic, and the
//! document saves and reads back with as many pages.
//!
//!     cargo +nightly fuzz run convert fuzz/corpus/convert crates/sealwax-convert/tests/data \
//!         -- -max_len=65536

use std::sync::atomic::{AtomicBool, Ordering};

use libfuzzer_sys::fuzz_target;
use sealwax_convert::{DocxOptions, composition, layout_docx};
use sealwax_core::{Document, Limits, OpenOptions, SaveOptions};

/// Set while docxide-pdf lays out: its panics are its own, caught by `layout_docx`.
static LAYING_OUT: AtomicBool = AtomicBool::new(false);

/// libFuzzer aborts at any panic, before unwinding: a panic while docxide-pdf lays out is let
/// unwind to the catch of `layout_docx`; any other still aborts.
fn tolerate_layout_panics() {
    let abort = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !LAYING_OUT.load(Ordering::SeqCst) {
            abort(info);
        }
    }));
}

fuzz_target!(init: tolerate_layout_panics(), |data: &[u8]| {
    LAYING_OUT.store(true, Ordering::SeqCst);
    let laid_out = layout_docx(data, &DocxOptions::default());
    LAYING_OUT.store(false, Ordering::SeqCst);
    let Ok(laid_out) = laid_out else {
        return;
    };
    // What compose returns as an error is a bound of the limits passed: no failure.
    let Ok(composed) = sealwax_compose::compose(&composition(&laid_out), &Limits::default()) else {
        return;
    };
    let saved = composed
        .document
        .save(&SaveOptions::default())
        .expect("a composed document saves");
    let reopened = Document::open(saved, &OpenOptions::default()).expect("and reads back");
    assert_eq!(reopened.page_count(), laid_out.pages.len());
});
