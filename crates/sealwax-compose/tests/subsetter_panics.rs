//! The programs subsetter 0.2.6 panics on (typst/subsetter#34 and #35) are left out before it
//! runs, so that WebAssembly, where panics abort, keeps going. Once a subsetter release no longer
//! panics on them, `subsetter_panics_on_them` fails: `src/subset_guard.rs` can then go.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Once};

use sealwax_compose::{
    Color, Composition, Font, Glyph, Item, Page, Paint, TextMode, TextRun, compose,
};
use sealwax_core::Limits;

/// A CID-keyed CFF font whose glyphs 1 to 256 each have their own font dictionary
/// (`tests/data/make_fonts.py`).
const FONT_DICTS: &[u8] = include_bytes!("data/font-dicts.otf");

/// A program, and the glyphs whose subset subsetter 0.2.6 panics on.
fn programs() -> [(Vec<u8>, Vec<u16>); 2] {
    [
        (common::program_with_fan(&['H'], u16::MAX), vec![1]),
        (FONT_DICTS.to_vec(), (1..=256).collect()),
    ]
}

fn subset(program: &[u8], glyphs: &[u16]) -> std::thread::Result<()> {
    let mut remapper = subsetter::GlyphRemapper::new();
    for glyph in glyphs {
        remapper.remap(*glyph);
    }
    catch_unwind(AssertUnwindSafe(|| {
        let _ = subsetter::subset(program, 0, &remapper);
    }))
}

#[test]
fn subsetter_panics_on_them() {
    for (program, glyphs) in programs() {
        assert!(subset(&program, &glyphs).is_err());
    }
    // One component fewer, and the subset stays within 65,535 glyphs: refused, not a panic.
    assert!(subset(&common::program_with_fan(&['H'], u16::MAX - 1), &[1]).is_ok());
}

thread_local! {
    static PANICKED: Cell<bool> = const { Cell::new(false) };
}

#[test]
fn they_are_left_out_without_a_panic() {
    // Record a panic on this thread even if it is caught (a panic hook runs on the thread that
    // panics, so the other test's panics do not count).
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICKED.with(|panicked| panicked.set(true));
            previous(info);
        }));
    });
    for (program, glyphs) in programs() {
        let run = TextRun {
            font: 0,
            size: 10.0,
            matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 50.0],
            horizontal_scaling: 1.0,
            mode: TextMode::Fill,
            fill: Paint::opaque(Color::Gray(0.0)),
            stroke: None,
            glyphs: glyphs
                .iter()
                .map(|id| Glyph {
                    id: *id,
                    x: 0.0,
                    advance: 500.0,
                    text: "x".to_owned(),
                })
                .collect(),
        };
        let composition = Composition {
            pages: vec![Page {
                width: 200.0,
                height: 100.0,
                items: vec![Item::Text(run)],
                links: Vec::new(),
            }],
            fonts: vec![Font::Program {
                program: Arc::new(program),
                face: 0,
            }],
            ..Composition::default()
        };
        let composed = compose(&composition, &Limits::default()).unwrap();
        assert_eq!(
            composed.skipped.iter().collect::<Vec<_>>(),
            [("glyphs in an unembeddable font", glyphs.len())]
        );
        assert!(!PANICKED.with(Cell::get));
    }
}
