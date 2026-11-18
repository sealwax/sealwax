#![no_main]
//! Whatever the bytes: interpreting every page to count what its glyphs map to, reading each
//! page's glyphs (its content instrumented to name fonts, the structure looked up for
//! `/ActualText`, its annotations drawn), its words, listing its images and extracting the
//! first, and redacting the first page's middle (text, images, paths), never panics
//! (hayro-interpret runs here with the `unsafe` and `images` features of hayro-syntax on); a
//! page redacted saves and reads back.
//!
//!     cargo +nightly fuzz run text fuzz/corpus/text corpus/public -- -max_len=1048576

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, OpenOptions, Rect, SaveOptions};
use sealwax_text::{
    RedactOptions, extract_image, glyphs, images, redact, search, text_census, words,
};

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
        if let Ok(found) = words(&document, page) {
            for word in &found {
                assert!(!word.text.is_empty());
            }
            if let Some(word) = found.first() {
                let _ = search(&document, page, &word.text);
            }
        }
        if let Ok(pictures) = images(&document, page) {
            for picture in &pictures {
                assert!(picture.bbox[0] <= picture.bbox[2] || picture.bbox[0].is_nan());
            }
            if !pictures.is_empty() {
                let _ = extract_image(&document, page, 0);
            }
        }
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
    if document.page_count() > 0 {
        let mut redacted = document;
        let middle = Rect::from_corners(100.0, 100.0, 400.0, 500.0);
        if redact(&mut redacted, 0, &[middle], &RedactOptions::default()).is_ok() {
            let saved = redacted.save(&SaveOptions::default()).expect("a page redacted saves");
            Document::open(saved, &OpenOptions::default()).expect("and reads back");
        }
    }
});
