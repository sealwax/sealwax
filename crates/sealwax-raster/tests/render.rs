//! One page rendered to a colour PNG: its size, what shows on it, and its bounds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use sealwax_annots::{
    AnnotationKind, FillValue, NewAnnotation, NewField, NewFieldKind, add_annotation, add_field,
    fill_fields, flatten, photos_document,
};
use sealwax_core::{Document, Error as PdfError, Limits, OpenOptions};
use sealwax_raster::{Clip, Error, RenderOptions, render, render_png};

const SCAN: &[u8] = include_bytes!("data/scan-16x16.jpg");

/// A PDF of `objects`, numbered from 1, the first the catalog.
fn build(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
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
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// One page drawing `content`, `page` added to its dictionary, `catalog` to the catalog's, and
/// `extra` objects from number 5 on.
fn document(content: &[u8], page: &str, catalog: &str, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut objects = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {page} >>").into_bytes(),
        stream("", content),
    ];
    objects.extend_from_slice(extra);
    build(&objects)
}

struct Image {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Image {
    fn decode(png_bytes: &[u8]) -> Self {
        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut rgb = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut rgb).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        rgb.truncate(info.buffer_size());
        Self {
            width: info.width,
            height: info.height,
            rgb,
        }
    }

    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let at = usize::try_from((y * self.width + x) * 3).unwrap();
        [self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]]
    }

    /// Pixels darker than light gray within `[x1, x2) x [y1, y2)`.
    fn dark(&self, x1: u32, y1: u32, x2: u32, y2: u32) -> usize {
        (y1..y2)
            .flat_map(|y| (x1..x2).map(move |x| (x, y)))
            .filter(|&(x, y)| self.at(x, y).iter().any(|c| *c < 128))
            .count()
    }
}

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).unwrap()
}

#[test]
fn a_rotated_page_with_an_offset_crop_box_has_the_size_it_shows() {
    // Crop box 500 x 700 at (50, 70), turned a quarter: shown 700 x 500. Red over the crop box's
    // left half, blue outside the crop box.
    let doc = open(document(
        b"1 0 0 rg 50 70 250 700 re f 0 0 1 rg 0 0 50 800 re f",
        "/MediaBox [0 0 600 800] /CropBox [50 70 550 770] /Rotate 90",
        "",
        &[],
    ));
    let scale = 150.0 / 72.0;
    let image = Image::decode(&render_png(&doc, 0, scale).unwrap());
    // 700 x 150 / 72 = 1458.33 and 500 x 150 / 72 = 1041.67, rounded up as MuPDF and PDFium do.
    assert_eq!((image.width, image.height), (1459, 1042));
    // Turned clockwise, the crop box's left edge is on top.
    assert_eq!(image.at(729, 10), [255, 0, 0]);
    assert_eq!(image.at(729, 1030), [255, 255, 255]);
    // A side a hair over a whole number of pixels takes no more.
    let doc = open(document(b"", "/MediaBox [0 0 100.0004 50.7]", "", &[]));
    let image = Image::decode(&render_png(&doc, 0, 1.0).unwrap());
    assert_eq!((image.width, image.height), (100, 51));
    assert!(
        image.rgb.chunks(3).all(|p| p != [0, 0, 255]),
        "nothing outside the crop box shows"
    );
}

#[test]
fn a_scanned_page_shows_its_jpeg() {
    let doc = open(document(
        b"q 200 0 0 300 0 0 cm /Scan Do Q",
        "/MediaBox [0 0 200 300] /Resources << /XObject << /Scan 5 0 R >> >>",
        "",
        &[stream(
            "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /DCTDecode",
            SCAN,
        )],
    ));
    let image = Image::decode(&render_png(&doc, 0, 1.0).unwrap());
    assert_eq!((image.width, image.height), (200, 300));
    // The scan's blue, away from its light diagonal.
    let [r, g, b] = image.at(180, 20);
    assert!(r < 80 && g < 100 && b > 120, "{:?}", [r, g, b]);
}

#[test]
fn a_filled_field_shows_its_value_and_the_document_is_left_as_it_is() {
    let form = document(
        b"",
        "/MediaBox [0 0 300 200] /Annots [5 0 R]",
        "/AcroForm << /Fields [5 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 6 0 R >> >> >>",
        &[
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [20 80 280 120] /F 4 >>"
                .to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        ],
    );
    let mut doc = open(form);
    let empty = Image::decode(&render_png(&doc, 0, 2.0).unwrap());
    assert_eq!(empty.dark(40, 160, 560, 240), 0);
    let values = HashMap::from([("name".to_owned(), FillValue::Text("Alice".to_owned()))]);
    fill_fields(&mut doc, &values).unwrap();
    // The field's rectangle, y from the top at 2 pixels a point: rows 160 to 240.
    let filled = Image::decode(&render_png(&doc, 0, 2.0).unwrap());
    assert!(filled.dark(40, 160, 560, 240) > 100);
    assert_eq!(filled.dark(0, 0, 600, 150), 0, "nothing else drawn");
    // Rendering changed nothing: the value is still the only change.
    assert!(doc.is_modified());
}

#[test]
fn past_the_pixel_bound_a_scale_or_a_page_is_refused_by_name() {
    let data = document(b"", "/MediaBox [0 0 600 800]", "", &[]);
    let mut limits = Limits::default();
    limits.max_render_pixels = 1_000_000;
    let doc = Document::open(data, &OpenOptions::default().limits(limits)).unwrap();
    // 600 x 800 at scale 1: 480,000 pixels; at 2: 1,920,000.
    assert!(render_png(&doc, 0, 1.0).is_ok());
    assert!(matches!(
        render_png(&doc, 0, 2.0),
        Err(Error::Pdf(PdfError::LimitExceeded {
            what: "max_render_pixels"
        }))
    ));
    // A side past what an image holds, whatever the bound.
    let doc = open(document(b"", "/MediaBox [0 0 600 800]", "", &[]));
    assert!(matches!(
        render_png(&doc, 0, 100.0),
        Err(Error::Pdf(PdfError::LimitExceeded {
            what: "max_render_pixels"
        }))
    ));
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            render_png(&doc, 0, scale),
            Err(Error::Pdf(PdfError::InvalidArgument { what: "scale" }))
        ));
    }
    assert!(matches!(
        render_png(&doc, 1, 1.0),
        Err(Error::Pdf(PdfError::InvalidArgument { what: "page index" }))
    ));
}

/// A page drawing an asymmetric shape: what turns shows.
fn arrow(rotate: u16) -> Document {
    open(document(
        b"1 0 0 rg 50 70 250 100 re f 0 0 1 rg 400 600 100 150 re f",
        &format!("/MediaBox [0 0 600 800] /CropBox [50 70 550 770] /Rotate {rotate}"),
        "",
        &[],
    ))
}

fn raster(doc: &Document, turns: i32) -> sealwax_raster::Raster {
    render(
        doc,
        0,
        &RenderOptions::new(0.5).quarter_turns(turns),
        &|| false,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn pixels_come_as_opaque_rgba_and_the_png_says_the_same() {
    let doc = arrow(0);
    let image = raster(&doc, 0);
    assert_eq!((image.width, image.height), (250, 350));
    assert_eq!(image.pixels.len(), 250 * 350 * 4);
    assert!(image.pixels.chunks(4).all(|p| p[3] == 255));
    let png = Image::decode(&render_png(&doc, 0, 0.5).unwrap());
    let rgb: Vec<u8> = image
        .pixels
        .chunks(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    assert_eq!(png.rgb, rgb);
}

#[test]
fn quarter_turns_show_the_page_as_its_rotation_would() {
    // Turning by quarter turns gives the very image of the page carrying that rotation.
    for (rotate, turns, same_as) in [
        (0, 1, 90),
        (0, 2, 180),
        (0, -1, 270),
        (90, 1, 180),
        (270, 2, 90),
    ] {
        let turned = raster(&arrow(rotate), turns);
        let rotated = raster(&arrow(same_as), 0);
        assert_eq!(
            (turned.width, turned.height),
            (rotated.width, rotated.height),
            "{rotate} + {turns}"
        );
        assert!(turned.pixels == rotated.pixels, "{rotate} + {turns}");
    }
}

/// A page of shapes, curves, a stroke and text over its crop box, turned `rotate`.
fn drawing(rotate: u16) -> Document {
    open(document(
        b"1 0 0 rg 50 70 250 100 re f 0 0 1 rg 400 600 100 150 re f \
          0 0.5 0 RG 3 w 60 80 m 300 700 400 100 540 760 c S \
          0 g BT /F1 31 Tf 1 0 0 1 90.3 401.7 Tm (Tiles agree) Tj ET",
        &format!(
            "/MediaBox [0 0 600 800] /CropBox [50 70 550 770] /Rotate {rotate} \
             /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>"
        ),
        "",
        &[],
    ))
}

#[test]
fn a_clipped_part_is_that_part_of_the_whole_page_to_rounding() {
    for rotate in [0, 90, 180, 270] {
        let doc = drawing(rotate);
        let options = RenderOptions::new(1.37);
        let whole = render(&doc, 0, &options, &|| false).unwrap().unwrap();
        let (w, h) = (whole.width, whole.height);
        for (x, y, width, height) in [
            (0, 0, 128, 128),
            (w - 128, h - 128, 128, 128),
            (101, 233, 200, 97),
            (0, 0, w, h),
            (w / 2, 0, 1, h),
        ] {
            let clip = Clip {
                x,
                y,
                width,
                height,
            };
            let part = render(&doc, 0, &options.clip(clip), &|| false)
                .unwrap()
                .unwrap();
            assert_eq!((part.width, part.height), (width, height));
            let mut largest = 0;
            for row in 0..height {
                let start = (((y + row) * w + x) * 4) as usize;
                let expected = &whole.pixels[start..start + (width * 4) as usize];
                let got =
                    &part.pixels[(row * width * 4) as usize..((row + 1) * width * 4) as usize];
                for (a, b) in expected.iter().zip(got) {
                    largest = largest.max(a.abs_diff(*b));
                }
            }
            // Coverage is computed from the coordinates moved by whole pixels: at most a level
            // of rounding apart.
            assert!(largest <= 2, "rotate {rotate}, clip {clip:?}: {largest}");
            if (x, y, width, height) == (0, 0, w, h) {
                assert!(
                    part.pixels == whole.pixels,
                    "the whole page as a clip is the page"
                );
            }
        }
    }
}

#[test]
fn a_clip_is_within_the_page_and_its_own_pixels_are_bounded() {
    let doc = drawing(90);
    let whole = render(&doc, 0, &RenderOptions::new(1.0), &|| false)
        .unwrap()
        .unwrap();
    let (w, h) = (whole.width, whole.height);
    for clip in [
        Clip {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        },
        Clip {
            x: 0,
            y: 0,
            width: 10,
            height: 0,
        },
        Clip {
            x: w - 9,
            y: 0,
            width: 10,
            height: 10,
        },
        Clip {
            x: 0,
            y: h,
            width: 10,
            height: 1,
        },
        Clip {
            x: u32::MAX,
            y: 0,
            width: 1,
            height: 1,
        },
    ] {
        assert!(
            matches!(
                render(&doc, 0, &RenderOptions::new(1.0).clip(clip), &|| false),
                Err(Error::Pdf(PdfError::InvalidArgument { what: "clip" }))
            ),
            "{clip:?}"
        );
    }
    // At 40 pixels per point the page has 560 million pixels, far past the bound; a tile of it
    // is rendered, and the bound counts the tile's.
    let tile = Clip {
        x: 9_000,
        y: 15_000,
        width: 512,
        height: 512,
    };
    let part = render(&doc, 0, &RenderOptions::new(40.0).clip(tile), &|| false)
        .unwrap()
        .unwrap();
    assert_eq!((part.width, part.height), (512, 512));
    assert!(matches!(
        render(&doc, 0, &RenderOptions::new(40.0), &|| false),
        Err(Error::Pdf(PdfError::LimitExceeded {
            what: "max_render_pixels"
        }))
    ));
    let mut limits = Limits::default();
    limits.max_render_pixels = 512 * 511;
    let bounded = Document::open(
        document(b"", "/MediaBox [0 0 600 800]", "", &[]),
        &OpenOptions::default().limits(limits),
    )
    .unwrap();
    assert!(matches!(
        render(
            &bounded,
            0,
            &RenderOptions::new(1.0).clip(Clip {
                x: 0,
                y: 0,
                width: 512,
                height: 512
            }),
            &|| false
        ),
        Err(Error::Pdf(PdfError::LimitExceeded {
            what: "max_render_pixels"
        }))
    ));
    // A clipped rendering is cancelled as a whole one is.
    assert!(
        render(
            &doc,
            0,
            &RenderOptions::new(1.0).clip(Clip {
                x: 0,
                y: 0,
                width: 64,
                height: 64
            }),
            &|| true
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn a_cancelled_rendering_stops_without_error_and_draws_nothing() {
    let doc = arrow(0);
    for stop_at in 1..=3 {
        let asked = std::cell::Cell::new(0);
        let cancelled = || {
            asked.set(asked.get() + 1);
            asked.get() >= stop_at
        };
        let rendered = render(&doc, 0, &RenderOptions::new(0.5), &cancelled).unwrap();
        assert!(rendered.is_none(), "stopped at the {stop_at}th question");
    }
    assert!(
        render(&doc, 0, &RenderOptions::new(0.5), &|| false)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_modified_document_is_read_again_once_until_it_changes() {
    let form = document(
        b"",
        "/MediaBox [0 0 300 200] /Annots [5 0 R]",
        "/AcroForm << /Fields [5 0 R] /DA (/Helv 0 Tf 0 g) >>",
        &[
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [20 80 280 120] /F 4 >>"
                .to_vec(),
        ],
    );
    let mut doc = open(form);
    assert!(doc.reopened_if_modified().unwrap().is_none());
    let values = HashMap::from([("name".to_owned(), FillValue::Text("Alice".to_owned()))]);
    fill_fields(&mut doc, &values).unwrap();
    let first = doc.reopened_if_modified().unwrap().unwrap();
    render_png(&doc, 0, 1.0).unwrap();
    let again = doc.reopened_if_modified().unwrap().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &again), "read again once");
    let values = HashMap::from([("name".to_owned(), FillValue::Text("Bob".to_owned()))]);
    fill_fields(&mut doc, &values).unwrap();
    let changed = doc.reopened_if_modified().unwrap().unwrap();
    assert!(
        !std::sync::Arc::ptr_eq(&first, &changed),
        "read again after a change"
    );
}

#[test]
fn a_field_created_on_a_turned_page_shows_where_it_was_placed_and_reads_upright() {
    for rotate in [0, 90, 180, 270] {
        let mut doc = open(document(
            b"",
            &format!("/MediaBox [0 0 600 800] /Rotate {rotate}"),
            "",
            &[],
        ));
        // Placed on the page as it shows, from its top-left corner.
        let (x1, y1, x2, y2) = (100, 50, 400, 90);
        let rect = [f64::from(x1), f64::from(y1), f64::from(x2), f64::from(y2)];
        add_field(
            &mut doc,
            &NewField::new("name", NewFieldKind::Text, 0, rect),
        )
        .unwrap();
        let values = HashMap::from([("name".to_owned(), FillValue::Text("WWWWWWWW".to_owned()))]);
        fill_fields(&mut doc, &values).unwrap();
        let image = Image::decode(&render_png(&doc, 0, 1.0).unwrap());
        let inside = image.dark(x1, y1, x2, y2);
        assert!(inside > 50, "{rotate}: the field shows where it was placed");
        let all = image.dark(0, 0, image.width, image.height);
        assert_eq!(all, inside, "{rotate}: nothing shows elsewhere");
        // The text runs along the field, upright: its ink is wider than high.
        let (mut left, mut right, mut top, mut bottom) = (u32::MAX, 0, u32::MAX, 0);
        for y in (y1 + 3)..(y2 - 3) {
            for x in (x1 + 3)..(x2 - 3) {
                if image.at(x, y).iter().any(|c| *c < 128) {
                    (left, right, top, bottom) =
                        (left.min(x), right.max(x), top.min(y), bottom.max(y));
                }
            }
        }
        assert!(
            right - left > 3 * (bottom - top),
            "{rotate}: the text reads along the field"
        );
    }
}

#[test]
fn a_photo_turned_by_its_exif_data_shows_upright() {
    // Stored 40 x 20, red on the left, blue on the right; its EXIF orientation turns it a quarter
    // clockwise: red on top, blue at the bottom.
    let photo = include_bytes!("../../sealwax-annots/tests/data/photo-40x20-exif6.jpg");
    let (doc, _) = photos_document(&[photo], Limits::default()).unwrap();
    let image = Image::decode(&render_png(&doc, 0, 0.5).unwrap());
    assert_eq!(
        (image.width, image.height),
        (306, 396),
        "a portrait Letter sheet"
    );
    let (middle, top, bottom) = (
        image.width / 2,
        image.height / 2 - 40,
        image.height / 2 + 40,
    );
    let red = image.at(middle, top);
    let blue = image.at(middle, bottom);
    assert!(red[0] > 200 && red[2] < 60, "{red:?} on top");
    assert!(blue[2] > 200 && blue[0] < 60, "{blue:?} at the bottom");
}

#[test]
fn annotations_made_show_as_drawn_and_stay_when_flattened() {
    // A black bar under the highlight, which must show through it.
    let mut doc = open(document(
        b"0 g 100 400 100 20 re f",
        "/MediaBox [0 0 600 800]",
        "",
        &[],
    ));
    let red = [1.0, 0.0, 0.0];
    let blue = [0.0, 0.0, 1.0];
    let green = [0.0, 0.6, 0.0];
    let made = [
        NewAnnotation::new(
            0,
            AnnotationKind::Square {
                rect: [100.0, 600.0, 200.0, 700.0],
            },
        )
        .color(red)
        .width(4.0),
        NewAnnotation::new(
            0,
            AnnotationKind::Circle {
                rect: [300.0, 600.0, 400.0, 700.0],
            },
        )
        .color(red)
        .fill(Some(blue)),
        NewAnnotation::new(
            0,
            AnnotationKind::Highlight {
                quads: vec![[90.0, 425.0, 210.0, 425.0, 90.0, 395.0, 210.0, 395.0]],
            },
        ),
        NewAnnotation::new(
            0,
            AnnotationKind::Ink {
                strokes: vec![vec![[100.0, 300.0], [200.0, 300.0]], vec![[300.0, 300.0]]],
            },
        )
        .color(green)
        .width(6.0),
        NewAnnotation::new(
            0,
            AnnotationKind::StrikeOut {
                quads: vec![[300.0, 420.0, 400.0, 420.0, 300.0, 400.0, 400.0, 400.0]],
            },
        )
        .color(red),
        NewAnnotation::new(
            0,
            AnnotationKind::Line {
                from: [100.0, 200.0],
                to: [200.0, 200.0],
            },
        )
        .color(blue)
        .width(3.0),
        NewAnnotation::new(
            0,
            AnnotationKind::Polygon {
                points: vec![[300.0, 150.0], [400.0, 150.0], [350.0, 250.0]],
            },
        )
        .fill(Some(green))
        .opacity(0.5),
        NewAnnotation::new(
            0,
            AnnotationKind::FreeText {
                rect: [400.0, 50.0, 580.0, 100.0],
                text: "WWWW".to_owned(),
                size: 20.0,
            },
        )
        .width(0.0),
    ];
    for annotation in &made {
        add_annotation(&mut doc, annotation).unwrap();
    }
    let check = |doc: &Document, when: &str| {
        let image = Image::decode(&render_png(doc, 0, 1.0).unwrap());
        // Image rows go down: y in the image is 800 minus y on the page.
        let at = |x: u32, y: u32| image.at(x, 800 - y);
        let near = |pixel: [u8; 3], expected: [u8; 3]| {
            pixel.iter().zip(expected).all(|(a, b)| a.abs_diff(b) < 40)
        };
        assert!(
            near(at(101, 650), [255, 0, 0]),
            "{when}: the square's border"
        );
        assert!(
            near(at(150, 650), [255, 255, 255]),
            "{when}: an empty square"
        );
        assert!(near(at(350, 650), [0, 0, 255]), "{when}: a filled circle");
        assert!(
            near(at(150, 410), [0, 0, 0]),
            "{when}: the text shows through the highlight"
        );
        assert!(near(at(95, 410), [255, 255, 0]), "{when}: the highlight");
        assert!(near(at(150, 300), [0, 153, 0]), "{when}: an ink stroke");
        assert!(near(at(300, 300), [0, 153, 0]), "{when}: a dot of ink");
        // The strike out is a line 1.4 point wide through the middle: red, blended at its edges.
        let strike = (408..=412)
            .map(|y| at(350, y))
            .min_by_key(|p| p[1])
            .unwrap();
        assert!(
            strike[0] > 200 && strike[1] < 120,
            "{when}: the strike out {strike:?}"
        );
        assert!(near(at(150, 200), [0, 0, 255]), "{when}: the line");
        assert!(
            near(at(350, 180), [128, 204, 128]),
            "{when}: a polygon half opaque"
        );
        assert!(image.dark(405, 700, 575, 750) > 50, "{when}: the free text");
    };
    check(&doc, "made");
    let reopened = open(doc.save(&sealwax_core::SaveOptions::default()).unwrap());
    check(&reopened, "saved");
    let mut flat = reopened;
    flatten(&mut flat).unwrap();
    check(&flat, "flattened");
}
