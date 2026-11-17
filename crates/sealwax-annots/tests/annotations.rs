//! Annotations made: what their dictionaries say, and what is refused.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_annots::{AnnotationKind, Error, NewAnnotation, add_annotation};
use sealwax_core::{Dict, Document, Object, OpenOptions, SaveOptions};

fn page() -> Document {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots 4 0 R >>",
        "[]",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Document::open(out, &OpenOptions::default()).unwrap()
}

fn annotations(doc: &Document) -> Vec<Dict> {
    let doc = Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap();
    let page = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
    let list = match page.as_dict().unwrap().get(b"Annots").unwrap() {
        Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
        other => other.clone(),
    };
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| match item {
            Object::Ref(id) => doc.object(*id).unwrap().unwrap().as_dict().unwrap().clone(),
            other => panic!("not a reference: {other:?}"),
        })
        .collect()
}

fn name(dict: &Dict, key: &[u8]) -> Vec<u8> {
    dict.get(key)
        .and_then(Object::as_name)
        .unwrap_or_default()
        .to_vec()
}

#[test]
fn an_annotation_made_says_what_it_is_and_who_made_it() {
    let mut doc = page();
    let quad = [100.0, 420.0, 200.0, 420.0, 100.0, 400.0, 200.0, 400.0];
    add_annotation(
        &mut doc,
        &NewAnnotation::new(0, AnnotationKind::Highlight { quads: vec![quad] })
            .opacity(0.4)
            .author(Some("Zoé".to_owned()))
            .contents(Some("à revoir".to_owned())),
    )
    .unwrap();
    add_annotation(
        &mut doc,
        &NewAnnotation::new(
            0,
            AnnotationKind::Ink {
                strokes: vec![vec![[10.0, 10.0], [20.0, 30.0]]],
            },
        )
        .width(3.0),
    )
    .unwrap();
    add_annotation(
        &mut doc,
        &NewAnnotation::new(
            0,
            AnnotationKind::Square {
                rect: [300.0, 300.0, 200.0, 250.0],
            },
        )
        .fill(Some([0.0, 0.0, 1.0])),
    )
    .unwrap();
    // Added to the page's own array of annotations, which is an object of its own.
    let found = annotations(&doc);
    assert_eq!(found.len(), 3);
    let highlight = &found[0];
    assert_eq!(name(highlight, b"Subtype"), b"Highlight");
    assert_eq!(highlight.get(b"F").and_then(Object::as_i64), Some(4));
    assert_eq!(highlight.get(b"CA").and_then(Object::as_f64), Some(0.4));
    assert_eq!(
        highlight.get(b"T"),
        Some(&Object::String(sealwax_core::encode_text_string("Zoé")))
    );
    assert_eq!(
        highlight
            .get(b"QuadPoints")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert!(highlight.contains_key(b"AP") && highlight.contains_key(b"NM"));
    let ink = &found[1];
    assert_eq!(name(ink, b"Subtype"), b"Ink");
    let rect = ink.get(b"Rect").unwrap().as_array().unwrap();
    let rect: Vec<f64> = rect.iter().filter_map(Object::as_f64).collect();
    // The points, and half the width around them.
    assert_eq!(rect, [8.5, 8.5, 21.5, 31.5]);
    let square = &found[2];
    assert_eq!(name(square, b"Subtype"), b"Square");
    assert!(square.contains_key(b"IC"));
    let rect: Vec<f64> = square
        .get(b"Rect")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Object::as_f64)
        .collect();
    assert_eq!(rect, [200.0, 250.0, 300.0, 300.0], "normalized");
}

#[test]
fn what_cannot_be_drawn_is_refused() {
    let quad = [0.0, 10.0, 10.0, 10.0, 0.0, 0.0, 10.0, 0.0];
    let refused = [
        NewAnnotation::new(1, AnnotationKind::Highlight { quads: vec![quad] }),
        NewAnnotation::new(0, AnnotationKind::Highlight { quads: vec![] }),
        NewAnnotation::new(
            0,
            AnnotationKind::Ink {
                strokes: vec![vec![]],
            },
        ),
        NewAnnotation::new(
            0,
            AnnotationKind::Line {
                from: [1.0, 1.0],
                to: [1.0, 1.0],
            },
        ),
        NewAnnotation::new(
            0,
            AnnotationKind::Polygon {
                points: vec![[0.0, 0.0]],
            },
        ),
        NewAnnotation::new(
            0,
            AnnotationKind::Square {
                rect: [0.0, 0.0, 0.0, 10.0],
            },
        ),
        NewAnnotation::new(0, AnnotationKind::Highlight { quads: vec![quad] })
            .color([2.0, 0.0, 0.0]),
        NewAnnotation::new(0, AnnotationKind::Highlight { quads: vec![quad] }).opacity(f64::NAN),
        NewAnnotation::new(0, AnnotationKind::Highlight { quads: vec![quad] }).width(-1.0),
        NewAnnotation::new(
            0,
            AnnotationKind::Highlight {
                quads: vec![[0.0, f64::INFINITY, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0]],
            },
        ),
        NewAnnotation::new(
            0,
            AnnotationKind::FreeText {
                rect: [0.0, 0.0, 100.0, 20.0],
                text: "x".to_owned(),
                size: 0.0,
            },
        ),
    ];
    for annotation in &refused {
        let mut doc = page();
        assert!(
            matches!(
                add_annotation(&mut doc, annotation),
                Err(Error::Pdf(sealwax_core::Error::InvalidArgument { .. }))
            ),
            "{annotation:?}"
        );
        assert!(annotations(&doc).is_empty(), "nothing added");
    }
}
