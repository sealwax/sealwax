#![no_main]
//! Whatever the bytes: interpreting every page to count what its glyphs map to, and reading each
//! page's glyphs (its content instrumented to name fonts, the structure looked up for
//! `/ActualText`, its annotations drawn), never panics (hayro-interpret runs here with the
//! `unsafe` and `images` features of hayro-syntax on).
//!
//!     cargo +nightly fuzz run text fuzz/corpus/text corpus/public -- -max_len=1048576

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, OpenOptions};
use sealwax_text::{glyphs, text_census};

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions::default().max_file_bytes(4 << 20);
    let Ok(document) = Document::open(data.to_vec(), &options) else {
        return;
    };
    if let Ok(text) = text_census(&document) {
        assert!(text.invisible <= text.glyphs);
        assert!(text.private_use_with_actual_text <= text.private_use);
    }
    for page in 0..document.page_count().min(4) {
        let Ok(found) = glyphs(&document, page, true) else {
            continue;
        };
        for glyph in found {
            // Zero for a text matrix flattened vertically.
            assert!(glyph.size.is_finite() && glyph.size >= 0.0);
            let [dx, dy] = glyph.direction;
            assert!(((dx * dx + dy * dy) - 1.0).abs() < 1e-6);
            assert!(!glyph.overdrawn || glyph.text.is_some());
        }
    }
});
