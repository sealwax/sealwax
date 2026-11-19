//! Photos made pages: one per JPEG or PNG, turned as their EXIF data says, the JPEG embedded as
//! it is.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_annots::{add_photo_pages, insert_image, photos_document};
use sealwax_core::{Document, Error, Limits, Object, OpenOptions, Rect, SaveOptions};

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

#[test]
fn inserting_images_keeps_the_existing_page_and_its_images() {
    let (original, _) = photos_document(&[CMYK], Limits::default()).unwrap();
    let mut doc = reopened(&original);
    let geometry = doc.geometry();
    let before = sealwax_text::images(&doc, 0).unwrap();
    let rect = Rect::from_corners(10.0, 20.0, 130.0, 100.0);
    insert_image(&mut doc, 0, rect, LANDSCAPE_EXIF6, true).unwrap();
    insert_image(&mut doc, 0, rect, ALPHA, false).unwrap();
    let doc = Document::open(doc.save_incremental().unwrap(), &OpenOptions::default()).unwrap();
    assert_eq!(doc.geometry(), geometry);
    let images = sealwax_text::images(&doc, 0).unwrap();
    assert_eq!(&images[..1], before.as_slice());
    assert_eq!(images.len(), 3);
    assert_eq!(images[1].bbox, [50.0, 20.0, 90.0, 100.0]);
    assert_eq!(images[1].filters, ["DCTDecode"]);
    assert_eq!(
        sealwax_text::extract_image(&doc, 0, 1).unwrap().data,
        LANDSCAPE_EXIF6
    );
    assert_eq!(images[2].bbox, [10.0, 20.0, 130.0, 100.0]);
    assert!(images[2].masked);
}

#[test]
fn a_refused_image_leaves_the_page_unchanged_and_the_pixel_limit_is_inclusive() {
    for (data, pixels) in [(LANDSCAPE_EXIF6, 800), (ALPHA, 120)] {
        let mut limits = Limits::default();
        limits.max_image_pixels = pixels - 1;
        let mut doc = Document::empty_with(limits).unwrap();
        doc.add_page(200.0, 200.0).unwrap();
        let before = doc.save(&SaveOptions::default()).unwrap();
        let rect = Rect::from_corners(2.0, 3.0, 15.0, 17.0);
        assert!(matches!(
            insert_image(&mut doc, 0, rect, data, true),
            Err(sealwax_annots::Error::Pdf(Error::LimitExceeded {
                what: "max_image_pixels"
            }))
        ));
        assert!(insert_image(&mut doc, 0, rect, b"", true).is_err());
        assert!(insert_image(&mut doc, 1, rect, data, true).is_err());
        assert_eq!(doc.save(&SaveOptions::default()).unwrap(), before);
        limits.max_image_pixels = pixels;
        let mut doc = Document::open(before, &OpenOptions::default().limits(limits)).unwrap();
        insert_image(&mut doc, 0, rect, data, true).unwrap();
    }
}

#[test]
fn a_single_palette_pixel_keeps_its_transparency_at_the_limit() {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(vec![127; 256 * 3]);
        encoder.set_trns(vec![128; 256]);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0])
            .unwrap();
    }
    let mut limits = Limits::default();
    limits.max_image_pixels = 1;
    let mut doc = Document::empty_with(limits).unwrap();
    doc.add_page(10.0, 10.0).unwrap();
    insert_image(
        &mut doc,
        0,
        Rect::from_corners(0.0, 0.0, 5.0, 5.0),
        &png,
        true,
    )
    .unwrap();
    assert!(sealwax_text::images(&doc, 0).unwrap()[0].masked);
}
