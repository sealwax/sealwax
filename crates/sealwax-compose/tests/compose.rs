//! Documents composed from laid-out pages: text reads back where it was placed, paths and
//! pictures render, links lead where they should, and what cannot be written is counted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use sealwax_compose::{
    Color, Composition, FillRule, Font, Glyph, Image, ImageColorSpace, ImageFilter, Item, Link,
    LinkTarget, Page, Paint, Path, Segment, TextMode, TextRun, compose,
};
use sealwax_core::{Document, Limits, Object, OpenOptions, SaveOptions};

fn reopened(doc: &Document) -> Document {
    let bytes = doc.save(&SaveOptions::default()).unwrap();
    Document::open(bytes, &OpenOptions::default()).unwrap()
}

fn page(items: Vec<Item>) -> Page {
    Page {
        width: 200.0,
        height: 100.0,
        items,
        links: Vec::new(),
    }
}

fn run(font: usize, glyphs: Vec<Glyph>) -> TextRun {
    TextRun {
        font,
        size: 10.0,
        matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 50.0],
        horizontal_scaling: 1.0,
        mode: TextMode::Fill,
        fill: Paint::opaque(Color::Gray(0.0)),
        stroke: None,
        glyphs,
    }
}

fn glyph(id: u16, x: f64, text: &str) -> Glyph {
    Glyph {
        id,
        x,
        advance: 500.0,
        text: text.to_owned(),
    }
}

#[test]
fn text_reads_back_where_it_was_placed() {
    // Glyphs 1, 2, 3 are 'H', 'i', '!' (advances 500, 510, 520): placed at 0, 7 and 20, not
    // where those advances would put them.
    let program = common::program("Test", &['H', 'i', '!']);
    let composition = Composition {
        pages: vec![page(vec![Item::Text(run(
            0,
            vec![glyph(1, 0.0, "H"), glyph(2, 7.0, "i"), glyph(3, 20.0, "!")],
        ))])],
        fonts: vec![Font::Program {
            program: Arc::new(program),
            face: 0,
        }],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    assert!(composed.skipped.is_empty());
    let doc = reopened(&composed.document);
    let glyphs = sealwax_text::glyphs(&doc, 0, false).unwrap();
    let read: Vec<(String, f64, f64)> = glyphs
        .iter()
        .map(|g| (g.text.clone().unwrap_or_default(), g.origin[0], g.origin[1]))
        .collect();
    assert_eq!(read.len(), 3);
    for ((text, x, y), (want_text, want_x)) in
        read.iter().zip([("H", 10.0), ("i", 17.0), ("!", 30.0)])
    {
        assert_eq!(text, want_text);
        assert!((x - want_x).abs() < 0.01, "{text} at {x}, not {want_x}");
        assert!((y - 50.0).abs() < 0.01);
    }
}

#[test]
fn notdef_glyphs_land_where_they_are_placed() {
    // Glyph 0 (.notdef, as a layout draws a character no font has) and a glyph past the
    // program's are drawn as code 0, whose width is 0: each stays at its x.
    let program = common::program("Test", &['H']);
    let composition = Composition {
        pages: vec![page(vec![Item::Text(run(
            0,
            vec![
                glyph(0, 0.0, ""),
                glyph(0, 0.0, ""),
                glyph(999, 0.0, ""),
                glyph(1, 4.0, "H"),
            ],
        ))])],
        fonts: vec![Font::Program {
            program: Arc::new(program),
            face: 0,
        }],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    let doc = reopened(&composed.document);
    let glyphs = sealwax_text::glyphs(&doc, 0, false).unwrap();
    let xs: Vec<f64> = glyphs
        .iter()
        .map(|g| (g.origin[0] * 100.0).round() / 100.0)
        .collect();
    assert_eq!(xs, [10.0, 10.0, 10.0, 14.0]);
}

#[test]
fn fonts_with_a_garbled_table_directory_are_embedded() {
    let program = common::program_with_bad_record("Test", &['H']);
    let composition = Composition {
        pages: vec![page(vec![Item::Text(run(0, vec![glyph(1, 0.0, "H")]))])],
        fonts: vec![Font::Program {
            program: Arc::new(program),
            face: 0,
        }],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    assert!(composed.skipped.is_empty());
    let doc = reopened(&composed.document);
    let glyphs = sealwax_text::glyphs(&doc, 0, false).unwrap();
    assert_eq!(glyphs[0].text.as_deref(), Some("H"));
}

/// The subtype of the composite font's descendant, and the key of its program.
fn descendant(doc: &Document) -> (Vec<u8>, Vec<u8>) {
    for number in 1..100 {
        let id = sealwax_core::ObjId {
            number,
            generation: 0,
        };
        let Ok(Some(Object::Dict(dict))) = doc.object(id) else {
            continue;
        };
        let Some(subtype) = dict.get(b"Subtype").and_then(Object::as_name) else {
            continue;
        };
        if !subtype.starts_with(b"CIDFontType") {
            continue;
        }
        let descriptor = dict
            .get(b"FontDescriptor")
            .and_then(Object::as_ref_id)
            .unwrap();
        let Some(Object::Dict(descriptor)) = doc.object(descriptor).unwrap() else {
            panic!("a descriptor");
        };
        let file = [b"FontFile2".as_slice(), b"FontFile3"]
            .into_iter()
            .find(|key| descriptor.get(key).is_some())
            .unwrap();
        return (subtype.to_vec(), file.to_vec());
    }
    panic!("no CID font");
}

/// 'H', 'i', '!' of the generated boxes fonts (`tests/data/make_fonts.py`), glyphs 1 to 3,
/// placed at 0, 7 and 20 from x = 10, read back and drawn.
fn boxes_read_back_and_draw(program: &[u8], face: u32) -> (Vec<u8>, Vec<u8>) {
    let composition = Composition {
        pages: vec![page(vec![Item::Text(run(
            0,
            vec![glyph(1, 0.0, "H"), glyph(2, 7.0, "i"), glyph(3, 20.0, "!")],
        ))])],
        fonts: vec![Font::Program {
            program: Arc::new(program.to_vec()),
            face,
        }],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    assert!(composed.skipped.is_empty(), "{:?}", composed.skipped);
    let doc = reopened(&composed.document);
    let glyphs = sealwax_text::glyphs(&doc, 0, false).unwrap();
    let read: Vec<(String, f64)> = glyphs
        .iter()
        .map(|g| (g.text.clone().unwrap_or_default(), g.origin[0]))
        .collect();
    assert_eq!(
        read,
        [("H".into(), 10.0), ("i".into(), 17.0), ("!".into(), 30.0)]
    );
    // H is a box from x = 10.5 to 15.5 (50 to 550 units at 10pt), 0 to 7 high.
    let options = sealwax_raster::RenderOptions::new(4.0);
    let raster = sealwax_raster::render(&doc, 0, &options, &|| false)
        .unwrap()
        .unwrap();
    let at = |x: f64, y: f64| {
        let (px, py) = ((x * 4.0) as u32, raster.height - 1 - (y * 4.0) as u32);
        raster.pixels[usize::try_from(4 * (py * raster.width + px)).unwrap()]
    };
    assert!(at(13.0, 53.0) < 50, "H is not drawn");
    assert!(at(13.0, 60.0) > 200, "H is drawn too high");
    descendant(&doc)
}

#[test]
fn cff_programs_are_embedded_read_back_and_drawn() {
    let program = include_bytes!("data/boxes-cff.otf");
    let (subtype, file) = boxes_read_back_and_draw(program, 0);
    assert_eq!(subtype, b"CIDFontType0");
    assert_eq!(file, b"FontFile3");
}

#[test]
fn faces_of_a_collection_are_embedded() {
    let collection = include_bytes!("data/boxes.ttc");
    let (subtype, file) = boxes_read_back_and_draw(collection, 0);
    assert_eq!(
        (subtype.as_slice(), file.as_slice()),
        (b"CIDFontType2".as_slice(), b"FontFile2".as_slice())
    );
    let (subtype, file) = boxes_read_back_and_draw(collection, 1);
    assert_eq!(
        (subtype.as_slice(), file.as_slice()),
        (b"CIDFontType0".as_slice(), b"FontFile3".as_slice())
    );
}

#[test]
fn standard_fonts_draw_with_the_widths_given() {
    let composition = Composition {
        pages: vec![page(vec![Item::Text(run(
            0,
            vec![
                glyph(u16::from(b'A'), 0.0, "A"),
                glyph(u16::from(b'B'), 5.0, "B"),
            ],
        ))])],
        fonts: vec![Font::Standard("Helvetica".to_owned())],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    let doc = reopened(&composed.document);
    let glyphs = sealwax_text::glyphs(&doc, 0, false).unwrap();
    let text: String = glyphs.iter().filter_map(|g| g.text.clone()).collect();
    assert_eq!(text, "AB");
    assert!((glyphs[1].origin[0] - 15.0).abs() < 0.01);
}

#[test]
fn paths_and_pictures_render() {
    // A red square on the left; a blue 1×1 picture scaled to a square on the right.
    let composition = Composition {
        pages: vec![page(vec![
            Item::Fill {
                path: Path(vec![Segment::Rect(10.0, 10.0, 40.0, 40.0)]),
                fill_rule: FillRule::NonZero,
                paint: Paint::opaque(Color::Rgb(1.0, 0.0, 0.0)),
            },
            Item::Group(vec![
                Item::Transform([40.0, 0.0, 0.0, 40.0, 100.0, 10.0]),
                Item::Image(0),
            ]),
        ])],
        images: vec![Image {
            width: 1,
            height: 1,
            color_space: ImageColorSpace::Rgb,
            bits_per_component: 8,
            filter: ImageFilter::None,
            data: vec![0, 0, 255],
            soft_mask: None,
            interpolate: false,
        }],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    let doc = reopened(&composed.document);
    let options = sealwax_raster::RenderOptions::new(1.0);
    let raster = sealwax_raster::render(&doc, 0, &options, &|| false)
        .unwrap()
        .unwrap();
    let pixel = |x: u32, y_up: u32| {
        let y = raster.height - 1 - y_up;
        let at = usize::try_from(4 * (y * raster.width + x)).unwrap();
        [
            raster.pixels[at],
            raster.pixels[at + 1],
            raster.pixels[at + 2],
        ]
    };
    assert_eq!(pixel(30, 30), [255, 0, 0]);
    assert_eq!(pixel(120, 30), [0, 0, 255]);
    assert_eq!(pixel(75, 30), [255, 255, 255]);
}

#[test]
fn gradients_fill_in_page_space() {
    // Red at x = 0, blue at x = 200, filled by a square moved there by a transform: the
    // gradient stays where the page puts it.
    let gradient = sealwax_compose::LinearGradient {
        matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        start: [0.0, 0.0],
        end: [200.0, 0.0],
        stops: vec![
            sealwax_compose::GradientStop {
                offset: 0.0,
                color: [1.0, 0.0, 0.0],
            },
            sealwax_compose::GradientStop {
                offset: 1.0,
                color: [0.0, 0.0, 1.0],
            },
        ],
        extend: [true, true],
        linear_light: false,
    };
    let composition = Composition {
        pages: vec![page(vec![Item::Group(vec![
            Item::Transform([1.0, 0.0, 0.0, 1.0, 100.0, 0.0]),
            Item::FillGradient {
                path: Path(vec![Segment::Rect(-100.0, 0.0, 200.0, 100.0)]),
                fill_rule: FillRule::NonZero,
                gradient,
            },
        ])])],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    let doc = reopened(&composed.document);
    let options = sealwax_raster::RenderOptions::new(1.0);
    let raster = sealwax_raster::render(&doc, 0, &options, &|| false)
        .unwrap()
        .unwrap();
    let pixel = |x: u32| {
        let at = usize::try_from(4 * (50 * raster.width + x)).unwrap();
        [
            raster.pixels[at],
            raster.pixels[at + 1],
            raster.pixels[at + 2],
        ]
    };
    let [r, _, b] = pixel(2);
    assert!(r > 240 && b < 15, "left is not red: {:?}", pixel(2));
    let [r, _, b] = pixel(197);
    assert!(r < 15 && b > 240, "right is not blue: {:?}", pixel(197));
    let [r, _, b] = pixel(100);
    assert!(
        (100..156).contains(&r) && (100..156).contains(&b),
        "middle: {:?}",
        pixel(100)
    );
}

#[test]
fn links_lead_to_pages_and_addresses() {
    let mut first = page(Vec::new());
    first.links = vec![
        Link {
            rect: [0.0, 0.0, 10.0, 10.0],
            target: LinkTarget::Page {
                index: 1,
                top: 80.0,
            },
            description: "Next".to_owned(),
        },
        Link {
            rect: [20.0, 0.0, 30.0, 10.0],
            target: LinkTarget::Uri("https://example.org/".to_owned()),
            description: String::new(),
        },
        Link {
            rect: [40.0, 0.0, 50.0, 10.0],
            target: LinkTarget::Page { index: 9, top: 0.0 },
            description: String::new(),
        },
    ];
    let composition = Composition {
        pages: vec![first, page(Vec::new())],
        ..Composition::default()
    };
    let composed = compose(&composition, &Limits::default()).unwrap();
    assert_eq!(
        composed.skipped.iter().collect::<Vec<_>>(),
        [("links to pages out of range", 1)]
    );
    let doc = reopened(&composed.document);
    let page_ids = doc.page_ids();
    let Some(Object::Dict(page)) = doc.object(page_ids[0].unwrap()).unwrap() else {
        panic!("no page");
    };
    let Some(Object::Array(annotations)) = page.get(b"Annots") else {
        panic!("no annotations");
    };
    assert_eq!(annotations.len(), 2);
    let annotation = |i: usize| match &annotations[i] {
        Object::Ref(id) => match doc.object(*id).unwrap() {
            Some(Object::Dict(dict)) => dict,
            other => panic!("not a dictionary: {other:?}"),
        },
        other => panic!("not a reference: {other:?}"),
    };
    let Some(Object::Array(dest)) = annotation(0).get(b"Dest").cloned() else {
        panic!("no destination");
    };
    assert_eq!(dest[0], Object::Ref(page_ids[1].unwrap()));
    assert!(annotation(1).get(b"A").is_some());
}

#[test]
fn what_cannot_be_written_is_counted() {
    let mut limits = Limits::default();
    limits.max_image_pixels = 4;
    let composition = Composition {
        pages: vec![page(vec![
            Item::Text(run(0, vec![glyph(1, 0.0, "x")])),
            Item::Image(0),
            Item::Image(7),
        ])],
        fonts: vec![Font::Program {
            program: Arc::new(b"not a font".to_vec()),
            face: 0,
        }],
        images: vec![Image {
            width: 3,
            height: 3,
            color_space: ImageColorSpace::Gray,
            bits_per_component: 8,
            filter: ImageFilter::None,
            data: vec![0; 9],
            soft_mask: None,
            interpolate: false,
        }],
        ..Composition::default()
    };
    let composed = compose(&composition, &limits).unwrap();
    let skipped: Vec<_> = composed.skipped.iter().collect();
    assert_eq!(
        skipped,
        [
            ("glyphs in an unembeddable font", 1),
            ("picture indexes out of range", 1),
            ("pictures past max_image_pixels", 1),
        ]
    );
    reopened(&composed.document);
}

#[test]
fn compositions_are_held_to_their_limits() {
    let composition = Composition {
        pages: vec![page(Vec::new())],
        ..Composition::default()
    };
    let past = Limits::default().timeout(Duration::ZERO);
    std::thread::sleep(Duration::from_millis(2));
    assert!(matches!(
        compose(&composition, &past),
        Err(sealwax_compose::Error::Pdf(sealwax_core::Error::Timeout))
    ));
    assert!(matches!(
        compose(&Composition::default(), &Limits::default()),
        Err(sealwax_compose::Error::Pdf(
            sealwax_core::Error::InvalidArgument { what: "pages" }
        ))
    ));
    let mut nested = Item::Group(Vec::new());
    for _ in 0..100 {
        nested = Item::Group(vec![nested]);
    }
    assert!(matches!(
        compose(
            &Composition {
                pages: vec![page(vec![nested])],
                ..Composition::default()
            },
            &Limits::default()
        ),
        Err(sealwax_compose::Error::Pdf(
            sealwax_core::Error::LimitExceeded { .. }
        ))
    ));
}
