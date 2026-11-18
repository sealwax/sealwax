//! Text, lines and rectangles drawn on a page, and links: written as asked, on the page as it
//! shows, and refused whole when a value cannot be taken.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_annots::{Shape, add_link, draw_shapes};
use sealwax_core::{Dict, Document, Object, OpenOptions, SaveOptions};

/// A document of one page of 600 by 800 points, opened again so that it is the input's.
fn page() -> Document {
    let mut doc = Document::empty().unwrap();
    doc.add_page(600.0, 800.0).unwrap();
    let saved = doc.save(&SaveOptions::default()).unwrap();
    Document::open(saved, &OpenOptions::default()).unwrap()
}

fn text(text: &str, font: &str) -> Shape {
    Shape::Text {
        at: [80.0, 100.0],
        text: text.to_owned(),
        font: font.to_owned(),
        size: 12.0,
        color: [0.0, 0.0, 0.0],
    }
}

fn page_dict(doc: &Document) -> Dict {
    let id = doc.page_ids()[0].unwrap();
    match doc.object(id).unwrap() {
        Some(Object::Dict(page)) => page,
        other => panic!("no page: {other:?}"),
    }
}

/// The streams of the forms the page draws.
fn forms(doc: &Document) -> Vec<Vec<u8>> {
    let resources = page_dict(doc).get(b"Resources").cloned().unwrap();
    let Object::Dict(resources) = resources else {
        panic!("resources");
    };
    let Some(Object::Dict(xobjects)) = resources.get(b"XObject").cloned() else {
        return Vec::new();
    };
    xobjects
        .iter()
        .filter_map(|(_, value)| match value {
            Object::Ref(id) => match doc.object(*id).unwrap() {
                Some(Object::Stream(stream)) => Some(stream.data),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn text_is_written_in_win_ansi_and_what_it_lacks_is_counted() {
    let mut doc = page();
    let replaced = draw_shapes(&mut doc, 0, &[text("Éloïse ✓\nnext", "hebo")]).unwrap();
    assert_eq!(replaced, 1);
    let forms = forms(&doc);
    assert_eq!(forms.len(), 1);
    let drawn = &forms[0];
    assert!(
        drawn.windows(10).any(|w| w == b"(\xC9lo\xEFse ?)"),
        "{}",
        String::from_utf8_lossy(drawn)
    );
    // The next line a font's height (1.377 em for Helvetica-Bold) below.
    assert!(String::from_utf8_lossy(drawn).contains("1 0 0 -1 80 116.524 Tm (next) Tj"));
}

#[test]
fn what_cannot_be_drawn_is_refused_and_nothing_is_drawn() {
    let mut doc = page();
    let line = |width: f64, color: [f64; 3]| Shape::Line {
        from: [0.0, 0.0],
        to: [10.0, 10.0],
        width,
        color,
    };
    for shapes in [
        vec![line(1.0, [0.0; 3]), text("x", "Symbol")],
        vec![Shape::Text {
            at: [0.0, 0.0],
            text: "x".to_owned(),
            font: "Helvetica".to_owned(),
            size: 0.0,
            color: [0.0; 3],
        }],
        vec![line(-1.0, [0.0; 3])],
        vec![line(1.0, [0.0, 2.0, 0.0])],
        vec![Shape::Rectangle {
            rect: [0.0, 0.0, 10.0, 10.0],
            width: 1.0,
            color: None,
            fill: None,
        }],
    ] {
        assert!(draw_shapes(&mut doc, 0, &shapes).is_err(), "{shapes:?}");
    }
    assert!(draw_shapes(&mut doc, 1, &[line(1.0, [0.0; 3])]).is_err());
    assert!(
        add_link(
            &mut doc,
            0,
            [10.0, 10.0, 10.0, 20.0],
            "https://example.org/"
        )
        .is_err()
    );
    assert!(add_link(&mut doc, 0, [10.0, 10.0, 20.0, 20.0], "").is_err());
    assert!(!doc.is_modified());
}

#[test]
fn a_link_covers_its_rectangle_on_the_page_as_it_shows() {
    let mut doc = page();
    doc.rotate_pages(&[(0, 1)]).unwrap();
    add_link(
        &mut doc,
        0,
        [80.0, 500.0, 200.0, 520.0],
        "https://example.org/",
    )
    .unwrap();
    let Some(Object::Array(annots)) = page_dict(&doc).get(b"Annots").cloned() else {
        panic!("no annotations");
    };
    let Object::Ref(id) = annots[0] else {
        panic!("not a reference");
    };
    let Some(Object::Dict(link)) = doc.object(id).unwrap() else {
        panic!("no link");
    };
    // Turned a quarter: shown x is user y, shown y user x.
    let rect: Vec<f64> = link
        .get(b"Rect")
        .and_then(Object::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(rect, [500.0, 80.0, 520.0, 200.0]);
    let Some(Object::Dict(action)) = link.get(b"A").cloned() else {
        panic!("no action");
    };
    assert_eq!(
        action.get(b"URI"),
        Some(&Object::String(b"https://example.org/".to_vec()))
    );
}
