#![no_main]
//! Whatever the font program a document gives: a page drawn with it composes without a panic
//! escaping (the program embedded, or left out and counted), and the page saves and reads back.
//!
//!     cargo +nightly fuzz run compose fuzz/corpus/compose crates/sealwax-compose/tests/data \
//!         -- -max_len=65536

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use sealwax_compose::{
    Color, Composition, Font, Glyph, Item, Page, Paint, TextMode, TextRun, compose,
};
use sealwax_core::{Document, Limits, OpenOptions, SaveOptions};

/// libFuzzer aborts at any panic, before unwinding: a panic in the subsetter, which sealwax
/// catches as a program it cannot cut, is let unwind to that catch; any other still aborts.
fn tolerate_subsetter_panics() {
    let abort = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let in_subsetter = info
            .location()
            .is_some_and(|location| location.file().contains("/subsetter-"));
        if !in_subsetter {
            abort(info);
        }
    }));
}

fuzz_target!(init: tolerate_subsetter_panics(), |data: &[u8]| {
    // The first glyphs, and one the length picks: past the program's glyphs as often as not.
    let ids = [0, 1, 2, 3, u16::try_from(data.len() % 4096).unwrap_or(0)];
    let glyphs = ids
        .iter()
        .enumerate()
        .map(|(i, id)| Glyph {
            id: *id,
            x: 6.0 * i as f64,
            advance: 500.0,
            text: "x".to_owned(),
        })
        .collect();
    let face = u32::from(data.first().is_some_and(|b| b % 2 == 1));
    let composition = Composition {
        pages: vec![Page {
            width: 100.0,
            height: 100.0,
            items: vec![Item::Text(TextRun {
                font: 0,
                size: 12.0,
                matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 50.0],
                horizontal_scaling: 1.0,
                mode: TextMode::Fill,
                fill: Paint::opaque(Color::Gray(0.0)),
                stroke: None,
                glyphs,
            })],
            links: Vec::new(),
        }],
        fonts: vec![Font::Program {
            program: Arc::new(data.to_vec()),
            face,
        }],
        ..Composition::default()
    };
    // A program the subsetter cannot cut is left out, as one that cannot be read.
    let composed = compose(&composition, &Limits::default()).expect("a page composes");
    let saved = composed
        .document
        .save(&SaveOptions::default())
        .expect("a page composed saves");
    let reopened = Document::open(saved, &OpenOptions::default()).expect("and reads back");
    assert_eq!(reopened.page_count(), 1);
});
