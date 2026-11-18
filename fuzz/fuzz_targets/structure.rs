#![no_main]
//! Whatever marked content, structure tree and outline a composition gives (indices out of range,
//! elements listed twice or in a cycle, sequences that never end): it composes without a panic,
//! and the document saves and reads back with its pages.
//!
//!     cargo +nightly fuzz run structure fuzz/corpus/structure -- -max_len=4096

use libfuzzer_sys::fuzz_target;
use sealwax_compose::{
    ArtifactKind, Color, Composition, Element, FillRule, HeaderScope, Item, Kid, Link, LinkTarget,
    ListNumbering, Mark, OutlineEntry, Page, Paint, Path, Segment, Structure, TableCell, compose,
};
use sealwax_core::{Document, Limits, OpenOptions, SaveOptions};

/// The bytes, one at a time, then zeros.
struct Bytes<'a>(std::slice::Iter<'a, u8>);

impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        self.0.next().copied().unwrap_or(0)
    }

    fn index(&mut self, bound: usize) -> usize {
        usize::from(self.next()) % bound.max(1)
    }
}

const TAGS: [&str; 6] = ["P", "H1", "Span", "", "A B/(é)", "Figure"];

fn item(bytes: &mut Bytes<'_>) -> Item {
    let square = Item::Fill {
        path: Path(vec![Segment::Rect(1.0, 1.0, 5.0, 5.0)]),
        fill_rule: FillRule::NonZero,
        paint: Paint::opaque(Color::Gray(0.0)),
    };
    match bytes.next() % 5 {
        0 => Item::BeginMarked(Mark::Element {
            tag: TAGS[bytes.index(TAGS.len())].to_owned(),
            mcid: u32::from(bytes.next() % 8) * if bytes.next() == 255 { u32::MAX / 8 } else { 1 },
        }),
        1 => Item::BeginMarked(Mark::Artifact(match bytes.next() % 5 {
            0 => None,
            1 => Some(ArtifactKind::Pagination),
            2 => Some(ArtifactKind::Layout),
            3 => Some(ArtifactKind::Page),
            _ => Some(ArtifactKind::Background),
        })),
        2 => Item::EndMarked,
        3 => Item::Group(vec![
            Item::BeginMarked(Mark::Artifact(None)),
            square,
            Item::EndMarked,
            Item::EndMarked,
        ]),
        _ => square,
    }
}

fn kid(bytes: &mut Bytes<'_>, elements: usize, pages: usize) -> Kid {
    match bytes.next() % 3 {
        0 => Kid::Element(bytes.index(elements + 2)),
        1 => Kid::Content {
            page: bytes.index(pages + 1),
            mcid: u32::from(bytes.next() % 8),
        },
        _ => Kid::Link {
            page: bytes.index(pages + 1),
            link: bytes.index(3),
        },
    }
}

fn element(bytes: &mut Bytes<'_>, elements: usize, pages: usize) -> Element {
    let kids = (0..bytes.index(6))
        .map(|_| kid(bytes, elements, pages))
        .collect();
    Element {
        kind: TAGS[bytes.index(TAGS.len())].to_owned(),
        kids,
        alt: (bytes.next() % 2 == 0).then(|| "alt".to_owned()),
        actual_text: None,
        lang: (bytes.next() % 3 == 0).then(|| "fr".to_owned()),
        id: (bytes.next() % 4 == 0).then(|| "Note 1".to_owned()),
        table_cell: (bytes.next() % 4 == 0).then_some(TableCell {
            header_scope: Some(HeaderScope::Both),
            col_span: u32::from(bytes.next()),
        }),
        list_numbering: (bytes.next() % 4 == 0).then_some(ListNumbering::Decimal),
    }
}

fn composition(data: &[u8]) -> Composition {
    let mut bytes = Bytes(data.iter());
    let page_count = 1 + bytes.index(3);
    let pages = (0..page_count)
        .map(|_| {
            let items = (0..bytes.index(24)).map(|_| item(&mut bytes)).collect();
            let links = (0..bytes.index(3))
                .map(|_| Link {
                    rect: [0.0, 0.0, 10.0, 10.0],
                    target: LinkTarget::Page {
                        index: bytes.index(page_count + 1),
                        top: 50.0,
                    },
                    description: String::new(),
                })
                .collect();
            Page {
                width: 100.0,
                height: 100.0,
                items,
                links,
            }
        })
        .collect();
    let element_count = bytes.index(12);
    let elements = (0..element_count)
        .map(|_| element(&mut bytes, element_count, page_count))
        .collect();
    let outline = (0..bytes.index(6))
        .map(|_| OutlineEntry {
            level: u32::from(bytes.next() % 5),
            title: "Entry".to_owned(),
            page: bytes.index(page_count + 1),
            top: (bytes.next() % 2 == 0).then_some(f64::from(bytes.next())),
        })
        .collect();
    Composition {
        pages,
        structure: (bytes.next() % 4 != 0).then_some(Structure { elements }),
        outline,
        ..Composition::default()
    }
}

fuzz_target!(|data: &[u8]| {
    let composition = composition(data);
    let composed = compose(&composition, &Limits::default()).expect("a composition composes");
    let saved = composed
        .document
        .save(&SaveOptions::default())
        .expect("a composed document saves");
    let reopened = Document::open(saved, &OpenOptions::default()).expect("and reads back");
    assert_eq!(reopened.page_count(), composition.pages.len());
    reopened.outline().expect("its outline reads back");
});
