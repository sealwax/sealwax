#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read as _;

use sealwax_core::{Document, Object, OpenOptions};
use sealwax_raster::{DEFAULT_DPI, MAX_SIDE_PX, grayscale};

/// Pages of the given sizes (points), each filled in red on its left half, the second rotated
/// a quarter, with a square annotation (blue) on the first.
fn pdf(sizes: &[(u32, u32)]) -> Vec<u8> {
    let count = u32::try_from(sizes.len()).unwrap();
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {count} >>",
            (0..count)
                .map(|i| format!("{} 0 R", 3 + 2 * i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ];
    for (i, (w, h)) in sizes.iter().enumerate() {
        let rotate = if i == 1 { " /Rotate 90" } else { "" };
        let annots = if i == 0 {
            " /Annots [<< /Type /Annot /Subtype /Square /Rect [10 10 50 50] /C [0 0 1] /IC [0 0 1] >>]"
        } else {
            ""
        };
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R{rotate}{annots} >>",
            4 + 2 * i
        ));
        let content = format!("1 0 0 rg 0 0 {} {h} re f", w / 2);
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
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
    out
}

/// A page's size in points and its image: width, height, samples.
type Page = ((f64, f64), (i64, i64, Vec<u8>));

/// Each page of `data`.
fn pages(data: Vec<u8>) -> Vec<Page> {
    let doc = Document::open(data, &OpenOptions::default()).unwrap();
    let geometry = doc.geometry();
    doc.page_ids()
        .into_iter()
        .zip(geometry)
        .map(|(id, page_geometry)| {
            let Some(Object::Dict(page)) = doc.object(id.unwrap()).unwrap() else {
                panic!("a page");
            };
            let resources = page.get(b"Resources").unwrap().as_dict().unwrap().clone();
            let xobjects = resources
                .get(b"XObject")
                .unwrap()
                .as_dict()
                .unwrap()
                .clone();
            let Some(Object::Stream(image)) = doc
                .object(xobjects.get(b"Im0").unwrap().as_ref_id().unwrap())
                .unwrap()
            else {
                panic!("an image");
            };
            assert_eq!(
                image.dict.get(b"ColorSpace").and_then(Object::as_name),
                Some(&b"DeviceGray"[..])
            );
            let mut samples = Vec::new();
            flate2::read::ZlibDecoder::new(image.data.as_slice())
                .read_to_end(&mut samples)
                .unwrap();
            let width = image.dict.get(b"Width").and_then(Object::as_i64).unwrap();
            let height = image.dict.get(b"Height").and_then(Object::as_i64).unwrap();
            (
                (page_geometry.media_box.x2, page_geometry.media_box.y2),
                (width, height, samples),
            )
        })
        .collect()
}

#[test]
fn each_page_becomes_one_gray_image_of_its_size() {
    let output = grayscale(
        &pdf(&[(612, 792), (612, 792)]),
        DEFAULT_DPI,
        &OpenOptions::default(),
    )
    .unwrap();
    let pages = pages(output);
    assert_eq!(pages.len(), 2);
    let ((w, h), (iw, ih, samples)) = &pages[0];
    assert!(
        (w - 612.0).abs() < 0.5 && (h - 792.0).abs() < 0.5,
        "{w} {h}"
    );
    assert_eq!((*iw, *ih), (1700, 2200));
    let at = |x: i64, y: i64| samples[usize::try_from(y * iw + x).unwrap()];
    // Red on the left half is mid-gray, the right half white, the annotation (blue) dark.
    assert!((60..=90).contains(&at(800, 1000)), "{}", at(800, 1000));
    assert_eq!(at(1600, 1000), 255);
    let blue = at(80, ih - 80);
    assert!(blue < 50, "{blue}");
    // The rotated page is drawn as shown: landscape.
    let ((w, h), (iw, ih, _)) = &pages[1];
    assert!(w > h, "{w} {h}");
    assert!(iw > ih);
}

#[test]
fn a_large_page_is_rendered_at_a_lower_resolution() {
    let output = grayscale(&pdf(&[(2448, 1584)]), DEFAULT_DPI, &OpenOptions::default()).unwrap();
    let pages = pages(output);
    let ((w, _), (iw, ih, _)) = &pages[0];
    assert!((w - 2448.0).abs() < 1.0);
    #[allow(clippy::cast_precision_loss)]
    let longest = (*iw).max(*ih) as f32;
    assert!(longest <= MAX_SIDE_PX + 1.0, "{longest}");
}
