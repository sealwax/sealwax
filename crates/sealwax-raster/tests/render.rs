//! One page rendered to a colour PNG: its size, what shows on it, and its bounds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use sealwax_annots::{FillValue, fill_fields};
use sealwax_core::{Document, Error as PdfError, Limits, OpenOptions};
use sealwax_raster::{Error, render_png};

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
