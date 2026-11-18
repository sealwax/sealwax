//! The images a page draws, listed and extracted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, Error, ObjId, OpenOptions};
use sealwax_text::{ImageFormat, extract_image, images};

const JPEG: &[u8] = include_bytes!("data/scan-16x16.jpg");

/// A document of `objects` (bodies as bytes), numbered from 1, the first the catalog.
fn build(objects: &[Vec<u8>]) -> Document {
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
    Document::open(out, &OpenOptions::default()).unwrap()
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A JPEG, an RGB image with a soft mask, the JPEG again through a form, a stencil mask and an
/// inline gray image.
fn pictured() -> Document {
    let content = b"q 160 0 0 160 100 500 cm /J Do Q q 20 0 0 10 300 300 cm /P Do Q /Fm Do \
        q 80 0 0 10 50 50 cm /St Do Q \
        q 30 0 0 30 400 400 cm BI /W 2 /H 2 /CS /G /BPC 8 /F /AHx ID 00FF80C0> EI Q";
    build(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
          /Resources << /XObject << /J 5 0 R /P 6 0 R /Fm 8 0 R /St 9 0 R >> >> >>"
            .to_vec(),
        stream("", content),
        stream(
            "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /DCTDecode",
            JPEG,
        ),
        stream(
            "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /SMask 7 0 R /Filter /ASCIIHexDecode",
            b"FF000000FF00>",
        ),
        stream(
            "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Filter /ASCIIHexDecode",
            b"FF80>",
        ),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Matrix [50 0 0 50 10 20] \
             /Resources << /XObject << /J 5 0 R >> >>",
            b"/J Do",
        ),
        stream(
            "/Type /XObject /Subtype /Image /Width 8 /Height 1 /ImageMask true \
             /Filter /ASCIIHexDecode",
            b"F0>",
        ),
    ])
}

fn decoded(png_data: &[u8]) -> (png::ColorType, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_data));
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    buffer.truncate(info.buffer_size());
    (info.color_type, buffer)
}

#[test]
fn a_page_lists_the_images_it_draws_where_it_draws_them() {
    let doc = pictured();
    let found = images(&doc, 0).unwrap();
    assert_eq!(found.len(), 5);
    let jpeg = &found[0];
    assert_eq!(jpeg.bbox, [100.0, 500.0, 260.0, 660.0]);
    assert_eq!(jpeg.transform, [160.0, 0.0, 0.0, 160.0, 100.0, 500.0]);
    assert_eq!((jpeg.width, jpeg.height), (16, 16));
    assert_eq!(jpeg.object, Some(ObjId::new(5)));
    assert_eq!(jpeg.color_space.as_deref(), Some("DeviceRGB"));
    assert_eq!(jpeg.filters, ["DCTDecode"]);
    assert!(!jpeg.masked && !jpeg.stencil);
    let masked = &found[1];
    assert!(masked.masked);
    assert_eq!(masked.bbox, [300.0, 300.0, 320.0, 310.0]);
    // Through the form: its matrix applied.
    assert_eq!(found[2].object, Some(ObjId::new(5)));
    assert_eq!(found[2].bbox, [10.0, 20.0, 60.0, 70.0]);
    let stencil = &found[3];
    assert!(stencil.stencil);
    assert_eq!(
        (stencil.color_space.as_deref(), stencil.bits_per_component),
        (None, Some(1))
    );
    let inline = &found[4];
    assert_eq!(inline.object, None);
    assert_eq!(inline.color_space.as_deref(), Some("DeviceGray"));
    assert_eq!(inline.filters, ["ASCIIHexDecode"]);
    assert_eq!((inline.width, inline.height), (2, 2));
}

#[test]
fn an_image_is_extracted_as_stored_or_as_a_png_of_its_pixels() {
    let doc = pictured();
    let jpeg = extract_image(&doc, 0, 0).unwrap();
    assert_eq!(jpeg.format, ImageFormat::Jpeg);
    assert_eq!(jpeg.data, JPEG);
    // With its soft mask: RGBA.
    let masked = extract_image(&doc, 0, 1).unwrap();
    assert_eq!(masked.format, ImageFormat::Png);
    assert_eq!(
        decoded(&masked.data),
        (png::ColorType::Rgba, vec![255, 0, 0, 255, 0, 255, 0, 128])
    );
    // A stencil: black where it paints, its samples at 0.
    let (color, samples) = decoded(&extract_image(&doc, 0, 3).unwrap().data);
    assert_eq!(color, png::ColorType::Grayscale);
    assert_eq!(samples, [255, 255, 255, 255, 0, 0, 0, 0]);
    let (color, samples) = decoded(&extract_image(&doc, 0, 4).unwrap().data);
    assert_eq!(
        (color, samples),
        (png::ColorType::Grayscale, vec![0, 255, 128, 192])
    );
    assert!(matches!(
        extract_image(&doc, 0, 5),
        Err(Error::InvalidArgument {
            what: "image index"
        })
    ));
    assert!(matches!(
        images(&doc, 1),
        Err(Error::InvalidArgument { what: "page index" })
    ));
}
