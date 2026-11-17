//! Photos made pages: one per JPEG or PNG, turned as their EXIF data says, the JPEG embedded as
//! it is.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_annots::{add_photo_pages, photos_document};
use sealwax_core::{Document, Limits, Object, OpenOptions, SaveOptions};

const LANDSCAPE_EXIF6: &[u8] = include_bytes!("data/photo-40x20-exif6.jpg");
const CMYK: &[u8] = include_bytes!("data/photo-8x8-cmyk.jpg");
const ALPHA: &[u8] = include_bytes!("data/photo-12x10-alpha.png");

fn reopened(doc: &Document) -> Document {
    Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap()
}

/// The image XObject the page `index` draws.
fn photo(doc: &Document, index: usize) -> sealwax_core::Stream {
    let page = doc.object(doc.page_ids()[index].unwrap()).unwrap().unwrap();
    let resources = page.as_dict().unwrap().get(b"Resources").unwrap().clone();
    let image = resources
        .as_dict()
        .unwrap()
        .get(b"XObject")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Photo")
        .unwrap()
        .as_ref_id()
        .unwrap();
    match doc.object(image).unwrap() {
        Some(Object::Stream(stream)) => stream,
        other => panic!("not an image: {other:?}"),
    }
}

#[test]
fn each_photo_read_is_a_page_turned_as_it_shows() {
    let (doc, report) =
        photos_document(&[LANDSCAPE_EXIF6, b"not a photo", ALPHA], Limits::default()).unwrap();
    assert_eq!((report.added, report.unreadable.clone()), (2, vec![1]));
    let doc = reopened(&doc);
    let geometry = doc.geometry();
    // Stored 40 x 20, shown a quarter turn clockwise: a portrait Letter sheet, held landscape
    // under a rotation of 90.
    assert_eq!(geometry[0].rotation.degrees(), 90);
    assert_eq!(
        (geometry[0].media_box.x2, geometry[0].media_box.y2),
        (792.0, 612.0)
    );
    // The PNG, 12 x 10, landscape, keeps its transparency.
    assert_eq!(geometry[1].rotation.degrees(), 0);
    assert_eq!(
        (geometry[1].media_box.x2, geometry[1].media_box.y2),
        (792.0, 612.0)
    );
    assert!(photo(&doc, 1).dict.contains_key(b"SMask"));
    // The JPEG is the file itself.
    let jpeg = photo(&doc, 0);
    assert_eq!(jpeg.data, LANDSCAPE_EXIF6);
    assert_eq!(
        jpeg.dict.get(b"Filter").and_then(Object::as_name),
        Some(&b"DCTDecode"[..])
    );
}

#[test]
fn a_cmyk_jpeg_from_photoshop_is_read_inverted() {
    let (doc, _) = photos_document(&[CMYK], Limits::default()).unwrap();
    let image = photo(&reopened(&doc), 0);
    assert_eq!(
        image.dict.get(b"ColorSpace").and_then(Object::as_name),
        Some(&b"DeviceCMYK"[..])
    );
    assert!(image.dict.contains_key(b"Decode"));
}

#[test]
fn the_limits_apply_and_no_photo_read_is_no_document() {
    let mut limits = Limits::default();
    limits.max_image_pixels = 100;
    let (doc, report) = photos_document(&[LANDSCAPE_EXIF6, CMYK], limits).unwrap();
    assert_eq!((report.added, report.unreadable), (1, vec![0]));
    assert_eq!(doc.page_count(), 1);
    assert!(photos_document(&[b"nothing"], Limits::default()).is_err());
    // Pages added to a document go after its own.
    let mut doc = reopened(&doc);
    let report = add_photo_pages(&mut doc, &[ALPHA, ALPHA]).unwrap();
    assert_eq!(report.added, 2);
    assert_eq!(reopened(&doc).page_count(), 3);
}
