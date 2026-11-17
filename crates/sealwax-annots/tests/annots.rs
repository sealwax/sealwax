//! XFDF, stamps and flattening: these read the page's content and the placement of what it draws.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read as _;

use base64::Engine as _;
use sealwax_annots::xfdf::{Align, NewMark, NewPicture, NewText, VerticalAlign, write_command};
use sealwax_annots::{
    ApplyOptions, Error, Stamp, apply_xfdf, apply_xfdf_with, flatten, stamp_images,
};
use sealwax_core::Rect;
use sealwax_core::{Dict, Document, Object, OpenOptions, SaveOptions};

// --- building PDFs ------------------------------------------------------------------------------

/// A PDF of `objects` (`(number, body)`, object 1 the catalog).
fn build(objects: &[(u32, String)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in objects {
        offsets.push((*number, out.len()));
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|(n, _)| *n).max().unwrap() + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for number in 1..size {
        let offset = offsets
            .iter()
            .find(|(n, _)| *n == number)
            .map_or(0, |(_, o)| *o);
        let kind = if offset == 0 { "f" } else { "n" };
        out.extend_from_slice(format!("{offset:010} 00000 {kind} \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// Pages 600 x 800, each with `page` added to its dictionary and drawing `content`; `more`
/// objects from number 10 on. Object 1 the catalog (with `catalog` added), 2 the page tree, pages
/// from 3, their contents from 3 + pages.
fn doc_with(
    pages: usize,
    page: &str,
    content: &str,
    catalog: &str,
    more: &[(u32, String)],
) -> Document {
    let pages_u32 = u32::try_from(pages).unwrap();
    let kids: Vec<String> = (0..pages_u32).map(|i| format!("{} 0 R", 3 + i)).collect();
    let mut objects = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>")),
        (
            2,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {pages} >>",
                kids.join(" ")
            ),
        ),
    ];
    for i in 0..pages_u32 {
        objects.push((
            3 + i,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents {} 0 R {page} >>",
                3 + pages_u32 + i
            ),
        ));
        objects.push((3 + pages_u32 + i, stream("", content)));
    }
    objects.extend(more.iter().cloned());
    Document::open(build(&objects), &OpenOptions::default()).unwrap()
}

fn png(width: u32, height: u32, color: png::ColorType, pixel: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let data: Vec<u8> = (0..width * height)
            .flat_map(|_| pixel.iter().copied())
            .collect();
        writer.write_image_data(&data).unwrap();
    }
    out
}

fn black(width: u32, height: u32) -> Vec<u8> {
    png(width, height, png::ColorType::Rgba, &[0, 0, 0, 255])
}

fn data_url(content: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(content)
    )
}

fn xfdf_stamp(rect: &str, page: usize, image: &str) -> String {
    format!(
        "<stamp page=\"{page}\" rect=\"{rect}\" flags=\"print,readonly,locked\" name=\"a1b2c3\" \
         subject=\"Draft\" icon=\"Draft\"><trn-custom-data bytes=\"{{&quot;signed&quot;:&quot;true&quot;}}\"/>\
         <imagedata>{image}</imagedata></stamp>"
    )
}

fn exported(annotations: &[&str], delete: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\
         <pdf-info xmlns=\"http://www.pdftron.com/pdfinfo\" version=\"2\" import-version=\"4\" /><fields />\
         \n<add>{}</add>\n<modify />\n{delete}<pages><defmtx matrix=\"1,0,0,-1,0,800\" /></pages></xfdf>",
        annotations.concat()
    )
}

/// A filled square note with its appearance (object 11), named like an annotation the XFDF may
/// delete (object 10).
fn square_note() -> Vec<(u32, String)> {
    vec![
        (
            10,
            "<< /Type /Annot /Subtype /Square /Rect [300 300 350 350] /NM (placeholder-1) /F 4 \
             /AP << /N 11 0 R >> >>"
                .to_owned(),
        ),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 50 50]",
                "1 0 0 rg 0 0 50 50 re f",
            ),
        ),
    ]
}

// --- reading the output -------------------------------------------------------------------------

fn saved(doc: &Document) -> Document {
    Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap()
}

fn resolve(doc: &Document, object: &Object) -> Object {
    match object {
        Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
        other => other.clone(),
    }
}

fn page(doc: &Document, index: usize) -> Dict {
    let Object::Dict(page) = doc.object(doc.page_ids()[index].unwrap()).unwrap().unwrap() else {
        panic!("a page is a dictionary");
    };
    page
}

fn decoded(object: &Object) -> Vec<u8> {
    let Object::Stream(stream) = object else {
        panic!("not a stream");
    };
    if stream.dict.get(b"Filter").and_then(Object::as_name) == Some(b"FlateDecode") {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(stream.data.as_slice())
            .read_to_end(&mut out)
            .unwrap();
        out
    } else {
        stream.data.clone()
    }
}

/// The page's content, its streams joined.
fn content(doc: &Document, page: &Dict) -> String {
    let contents = resolve(doc, page.get(b"Contents").unwrap());
    let streams: Vec<Object> = match contents {
        Object::Array(items) => items.iter().map(|i| resolve(doc, i)).collect(),
        other => vec![other],
    };
    streams
        .iter()
        .map(|s| String::from_utf8_lossy(&decoded(s)).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn xobjects(doc: &Document, page: &Dict) -> Dict {
    let resources = resolve(doc, page.get(b"Resources").unwrap());
    let xobjects = resolve(doc, resources.as_dict().unwrap().get(b"XObject").unwrap());
    xobjects.as_dict().unwrap().clone()
}

/// What the page draws after its content: each XObject by name, its dictionary and data, and the
/// box its `/BBox` (or unit square, for an image) lands on.
fn painted(doc: &Document, page: &Dict) -> Vec<(Dict, Vec<u8>, [f64; 4])> {
    let content = content(doc, page);
    let xobjects = xobjects(doc, page);
    let mut found = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("q ") else {
            continue;
        };
        let Some((matrix, name)) = rest.split_once(" cm /") else {
            continue;
        };
        let name = name.trim_end_matches(" Do Q");
        let m: Vec<f64> = matrix.split(' ').map(|v| v.parse().unwrap()).collect();
        let object = resolve(doc, xobjects.get(name.as_bytes()).unwrap());
        let Object::Stream(stream) = &object else {
            panic!("an XObject is a stream");
        };
        let form = stream.dict.get(b"Subtype").and_then(Object::as_name) == Some(b"Form");
        let (bbox, inner) = if form {
            let bbox: Vec<f64> = stream
                .dict
                .get(b"BBox")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .collect();
            let inner: Vec<f64> =
                stream
                    .dict
                    .get(b"Matrix")
                    .map_or(vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0], |v| {
                        v.as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_f64().unwrap())
                            .collect()
                    });
            (bbox, inner)
        } else {
            (vec![0.0, 0.0, 1.0, 1.0], vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
        };
        // The form's matrix, then the placement.
        let (a1, b1, c1, d1, e1, f1) = (inner[0], inner[1], inner[2], inner[3], inner[4], inner[5]);
        let (a2, b2, c2, d2, e2, f2) = (m[0], m[1], m[2], m[3], m[4], m[5]);
        let (a, b, c, d) = (
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
        );
        let (e, f) = (e1 * a2 + f1 * c2 + e2, e1 * b2 + f1 * d2 + f2);
        let corners = [
            (bbox[0], bbox[1]),
            (bbox[0], bbox[3]),
            (bbox[2], bbox[1]),
            (bbox[2], bbox[3]),
        ]
        .map(|(x, y)| (x * a + y * c + e, x * b + y * d + f));
        let min = |i: usize| {
            corners
                .iter()
                .map(|p| if i == 0 { p.0 } else { p.1 })
                .fold(f64::INFINITY, f64::min)
        };
        let max = |i: usize| {
            corners
                .iter()
                .map(|p| if i == 0 { p.0 } else { p.1 })
                .fold(f64::NEG_INFINITY, f64::max)
        };
        found.push((
            stream.dict.clone(),
            decoded(&object),
            [min(0), min(1), max(0), max(1)],
        ));
    }
    found
}

fn close(actual: [f64; 4], expected: [f64; 4]) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(a, e)| (a - e).abs() < 1e-3)
}

fn annotation_names(doc: &Document, page: &Dict) -> Vec<String> {
    let Some(annots) = page.get(b"Annots") else {
        return Vec::new();
    };
    resolve(doc, annots)
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let a = resolve(doc, a);
            String::from_utf8_lossy(
                a.as_dict()
                    .unwrap()
                    .get(b"NM")
                    .and_then(Object::as_string)
                    .unwrap_or_default(),
            )
            .into_owned()
        })
        .collect()
}

// --- the XFDF applied ---------------------------------------------------------------------------

#[test]
fn the_merged_xfdf_of_two_signed_fields_draws_both_pictures() {
    let mut doc = doc_with(1, "", "", "", &[]);
    let picture = data_url(&black(60, 20));
    let xfdf = exported(
        &[
            &xfdf_stamp("100,600,160,620", 0, &picture),
            &xfdf_stamp("300,100,360,120", 0, &picture),
        ],
        "<delete />",
    );
    let applied = apply_xfdf(&mut doc, &xfdf, true).unwrap();
    assert_eq!(applied.pictures, 2);
    let doc = saved(&doc);
    let boxes: Vec<[f64; 4]> = painted(&doc, &page(&doc, 0))
        .into_iter()
        .map(|p| p.2)
        .collect();
    assert_eq!(boxes.len(), 2);
    assert!(close(boxes[0], [100.0, 600.0, 160.0, 620.0]), "{boxes:?}");
    assert!(close(boxes[1], [300.0, 100.0, 360.0, 120.0]), "{boxes:?}");
}

#[test]
fn flatten_paints_the_existing_annotations_under_the_pictures() {
    let mut doc = doc_with(1, "/Annots [10 0 R]", "", "", &square_note());
    let xfdf = exported(
        &[&xfdf_stamp("100,600,160,620", 0, &data_url(&black(60, 20)))],
        "<delete />",
    );
    let applied = apply_xfdf(&mut doc, &xfdf, true).unwrap();
    assert_eq!(applied.flattened.as_ref().unwrap().painted, 1);
    let doc = saved(&doc);
    let page = page(&doc, 0);
    assert!(!page.contains_key(b"Annots"));
    let painted = painted(&doc, &page);
    assert_eq!(painted.len(), 2);
    // The square first, under the picture.
    assert_eq!(
        painted[0].0.get(b"Subtype").and_then(Object::as_name),
        Some(&b"Form"[..])
    );
    assert!(close(painted[0].2, [300.0, 300.0, 350.0, 350.0]));
    assert_eq!(
        painted[1].0.get(b"Subtype").and_then(Object::as_name),
        Some(&b"Image"[..])
    );
}

#[test]
fn without_flatten_the_existing_annotations_stay() {
    let mut doc = doc_with(1, "/Annots [10 0 R]", "", "", &square_note());
    let xfdf = exported(
        &[&xfdf_stamp("100,600,160,620", 0, &data_url(&black(60, 20)))],
        "<delete />",
    );
    apply_xfdf(&mut doc, &xfdf, false).unwrap();
    let doc = saved(&doc);
    assert_eq!(annotation_names(&doc, &page(&doc, 0)), ["placeholder-1"]);
}

#[test]
fn a_deletion_never_takes_an_annotation_of_the_pdf_out() {
    let deletes = "<delete><id page=\"0\">placeholder-1</id><id page=\"0\">not-here</id><id page=\"7\">x</id></delete>";
    let mut doc = doc_with(1, "/Annots [10 0 R]", "", "", &square_note());
    let xfdf = exported(
        &[&xfdf_stamp("100,600,160,620", 0, &data_url(&black(60, 20)))],
        deletes,
    );
    let applied = apply_xfdf(&mut doc, &xfdf, false).unwrap();
    assert_eq!(applied.deletions_ignored, 3);
    let doc = saved(&doc);
    assert_eq!(annotation_names(&doc, &page(&doc, 0)), ["placeholder-1"]);
}

#[test]
fn a_deleted_annotation_is_still_flattened_into_the_page() {
    let mut doc = doc_with(1, "/Annots [10 0 R]", "", "", &square_note());
    let xfdf = exported(
        &[&xfdf_stamp("100,600,160,620", 0, &data_url(&black(60, 20)))],
        "<delete><id page=\"0\">placeholder-1</id></delete>",
    );
    apply_xfdf(&mut doc, &xfdf, true).unwrap();
    let doc = saved(&doc);
    let page = page(&doc, 0);
    assert!(!page.contains_key(b"Annots"));
    assert_eq!(painted(&doc, &page).len(), 2);
}

/// A square note (10, 11), a link (12) and a filled text field (13) on the page, the field in
/// the form.
fn page_with_a_note_a_link_and_a_field() -> Document {
    let mut more = square_note();
    more.push((
        12,
        "<< /Type /Annot /Subtype /Link /Rect [10 10 50 50] /NM (link-1) \
         /A << /S /URI /URI (https://example.com) >> >>"
            .to_owned(),
    ));
    more.push((
        13,
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Existing Value) \
         /Rect [100 100 250 120] /NM (field-1) /F 4 /DA (/Helv 12 Tf 0 g) >>"
            .to_owned(),
    ));
    doc_with(
        1,
        "/Annots [10 0 R 12 0 R 13 0 R]",
        "",
        "/AcroForm << /Fields [13 0 R] /NeedAppearances true >>",
        &more,
    )
}

#[test]
fn the_pages_drawn_on_are_listed_from_zero_in_order() {
    let mut doc = doc_with(3, "", "", "", &[]);
    let picture = data_url(&black(60, 20));
    let xfdf = exported(
        &[
            &xfdf_stamp("100,600,160,620", 2, &picture),
            &xfdf_stamp("100,600,160,620", 0, &picture),
            &xfdf_stamp("300,100,360,120", 2, &picture),
        ],
        "<delete />",
    );
    let applied = apply_xfdf(&mut doc, &xfdf, true).unwrap();
    assert_eq!(applied.pages, [0, 2]);
    assert_eq!(applied.annotations_removed, 0);
}

#[test]
fn the_pages_flattened_count_as_changed_and_kept_annotations_do_not() {
    let picture = data_url(&black(60, 20));
    let xfdf = exported(&[&xfdf_stamp("100,600,160,620", 0, &picture)], "<delete />");
    let mut kept = doc_with(2, "/Annots [10 0 R]", "", "", &square_note());
    assert_eq!(apply_xfdf(&mut kept, &xfdf, false).unwrap().pages, [0]);
    let mut flattened = doc_with(2, "/Annots [10 0 R]", "", "", &square_note());
    assert_eq!(
        apply_xfdf(&mut flattened, &xfdf, true).unwrap().pages,
        [0, 1]
    );
    // A page with links alone does not change: flattening keeps links.
    let link = vec![(
        10,
        "<< /Type /Annot /Subtype /Link /Rect [10 10 50 50] >>".to_owned(),
    )];
    let mut linked = doc_with(2, "/Annots [10 0 R]", "", "", &link);
    assert_eq!(apply_xfdf(&mut linked, &xfdf, true).unwrap().pages, [0]);
}

#[test]
fn removing_takes_the_document_annotations_and_form_out_links_but() {
    let mut doc = page_with_a_note_a_link_and_a_field();
    let xfdf = exported(
        &[&xfdf_stamp("300,600,360,620", 0, &data_url(&black(60, 20)))],
        "<delete />",
    );
    let options = ApplyOptions::new().flatten(true).remove_annotations(true);
    let applied = apply_xfdf_with(&mut doc, &xfdf, &options, |_| None).unwrap();
    assert_eq!(applied.annotations_removed, 2);
    assert_eq!(applied.pages, [0]);
    assert_eq!(applied.pictures, 1);
    let flattened = applied.flattened.unwrap();
    assert_eq!((flattened.painted, flattened.drawn), (0, 0));
    let doc = saved(&doc);
    let page = page(&doc, 0);
    assert_eq!(annotation_names(&doc, &page), ["link-1"]);
    // Only the picture is drawn: neither the square nor the field's value.
    let painted = painted(&doc, &page);
    assert_eq!(painted.len(), 1);
    assert_eq!(
        painted[0].0.get(b"Subtype").and_then(Object::as_name),
        Some(&b"Image"[..])
    );
    let catalog = resolve(&doc, &Object::Ref(doc.catalog_id()));
    assert!(!catalog.as_dict().unwrap().contains_key(b"AcroForm"));
}

#[test]
fn removing_without_flattening_leaves_no_form_either() {
    let mut doc = page_with_a_note_a_link_and_a_field();
    let options = ApplyOptions::new().remove_annotations(true);
    let applied =
        apply_xfdf_with(&mut doc, &exported(&[], "<delete />"), &options, |_| None).unwrap();
    assert_eq!(applied.annotations_removed, 2);
    assert!(applied.flattened.is_none());
    assert_eq!(applied.pages, [0]);
    let doc = saved(&doc);
    assert_eq!(annotation_names(&doc, &page(&doc, 0)), ["link-1"]);
    // Nothing painted: the page draws no XObject.
    assert!(!content(&doc, &page(&doc, 0)).contains("Do"));
}

#[test]
fn a_refused_xfdf_removes_nothing() {
    let mut doc = page_with_a_note_a_link_and_a_field();
    let xfdf = exported(&["<ink page=\"0\" rect=\"1,1,2,2\" />"], "<delete />");
    let options = ApplyOptions::new().flatten(true).remove_annotations(true);
    assert!(apply_xfdf_with(&mut doc, &xfdf, &options, |_| None).is_err());
    assert!(!doc.is_modified());
}

#[test]
fn the_xfdf_of_each_field_applies_one_after_the_other() {
    let mut doc = doc_with(2, "/Annots [10 0 R]", "", "", &square_note());
    let picture = data_url(&black(60, 20));
    let first = exported(&[&xfdf_stamp("100,600,160,620", 0, &picture)], "<delete />");
    let second = exported(&[&xfdf_stamp("300,100,360,120", 1, &picture)], "<delete />");
    assert_eq!(apply_xfdf(&mut doc, &first, false).unwrap().pages, [0]);
    assert_eq!(apply_xfdf(&mut doc, &second, false).unwrap().pages, [1]);
    let doc = saved(&doc);
    for index in 0..2 {
        let page = page(&doc, index);
        assert_eq!(annotation_names(&doc, &page), ["placeholder-1"]);
        let images = painted(&doc, &page)
            .into_iter()
            .filter(|p| p.0.get(b"Subtype").and_then(Object::as_name) == Some(&b"Image"[..]))
            .count();
        assert_eq!(images, 1, "page {index}");
    }
}

/// The matrix of the `cm` before each `Do` of the page's content.
fn placements(doc: &Document, page: &Dict) -> Vec<[f64; 6]> {
    let content = content(doc, page);
    let words: Vec<&str> = content.split_whitespace().collect();
    words
        .windows(9)
        .filter(|w| w[6] == "cm" && w[8] == "Do")
        .map(|w| core::array::from_fn(|i| w[i].parse().unwrap()))
        .collect()
}

#[test]
fn a_turned_mark_is_drawn_turned_counterclockwise_within_its_rectangle() {
    // Rectangles as a signing page writes them on a page turned a quarter turn: 25 wide and 100
    // high in user space, a box 100 x 25 as shown.
    let picture = data_url(&black(40, 10));
    for (rotation, expected) in [
        (90, [0.0, 1.0, -1.0, 0.0]),
        (180, [-1.0, 0.0, 0.0, -1.0]),
        (270, [0.0, -1.0, 1.0, 0.0]),
    ] {
        let mut doc = doc_with(1, &format!("/Rotate {rotation}"), "", "", &[]);
        let rect = if rotation == 180 {
            "100,500,200,525"
        } else {
            "100,500,125,600"
        };
        let stamp = xfdf_stamp(rect, 0, &picture)
            .replace("<stamp ", &format!("<stamp rotation=\"{rotation}\" "));
        let free = format!(
            "<freetext rotation=\"{rotation}\" page=\"0\" rect=\"{rect}\" flags=\"print\" width=\"0\" FontSize=\"10\">\
             <contents>Jean</contents><defaultappearance>0 0 0 rg /Helvetica 10 Tf</defaultappearance></freetext>"
        );
        let applied =
            apply_xfdf(&mut doc, &exported(&[&stamp, &free], "<delete />"), false).unwrap();
        assert_eq!((applied.pictures, applied.texts), (1, 1));
        let doc = saved(&doc);
        let page = page(&doc, 0);
        let painted = painted(&doc, &page);
        let [r1, r2] = [painted[0].2, painted[1].2];
        let target = if rotation == 180 {
            [100.0, 500.0, 200.0, 525.0]
        } else {
            [100.0, 500.0, 125.0, 600.0]
        };
        // The picture (4:1, contained) fills the box as shown; the text's form is the box.
        assert!(close(r1, target), "{rotation}: {r1:?}");
        assert!(close(r2, target), "{rotation}: {r2:?}");
        for matrix in placements(&doc, &page) {
            let unit: Vec<f64> = matrix[..4]
                .iter()
                .map(|v| v.signum() * f64::from(u8::from(v.abs() > 1e-9)))
                .collect();
            assert_eq!(unit, expected, "{rotation}: {matrix:?}");
        }
    }
}

#[test]
fn an_xfdf_written_draws_what_the_viewer_export_draws() {
    let png = black(60, 20);
    for rotation in [0_u16, 90, 180, 270] {
        let turned = if rotation == 0 {
            String::new()
        } else {
            format!(" rotation=\"{rotation}\"")
        };
        let (sig, txt) = if rotation % 180 == 0 {
            ("100,600,160,620", "100,500,220,525")
        } else {
            ("100,560,120,620", "300,400,325,520")
        };
        // As the viewer exports them: a signature stamp and a text field's free text.
        let viewer = exported(
            &[
                &xfdf_stamp(sig, 0, &data_url(&png))
                    .replace("<stamp ", &format!("<stamp{turned} ")),
                &format!(
                    "<freetext page=\"0\" rect=\"{txt}\" flags=\"print,readonly,locked\" name=\"t1\" \
                     subject=\"Free Text\" width=\"0\" TextColor=\"#000000\" FontSize=\"16\"{turned}>\
                     <trn-custom-data bytes=\"{{&quot;fieldId&quot;:&quot;f-2&quot;}}\"/>\
                     <contents>Jean Été</contents><contents-richtext><body><p><span>Jean Été</span></p></body></contents-richtext>\
                     <defaultappearance>0 0 0 rg /Helvetica 16 Tf</defaultappearance>\
                     <defaultstyle>font: Helvetica 16pt; text-align: center; text-vertical-align: center; color: #000000</defaultstyle>\
                     </freetext>"
                ),
            ],
            "<delete />",
        );
        let rect = |value: &str| {
            let v: Vec<f64> = value.split(',').map(|n| n.parse().unwrap()).collect();
            Rect::from_corners(v[0], v[1], v[2], v[3])
        };
        let written = write_command(
            &[
                NewMark::Picture(NewPicture {
                    page_index: 0,
                    rect: rect(sig),
                    rotation,
                    png: png.clone(),
                    name: "a1b2c3".to_owned(),
                    custom_data: vec![("signed".to_owned(), "true".to_owned())],
                }),
                NewMark::Text(NewText {
                    page_index: 0,
                    rect: rect(txt),
                    rotation,
                    contents: "Jean Été".to_owned(),
                    size: 16.0,
                    color: [0.0, 0.0, 0.0],
                    align: Align::Center,
                    vertical: VerticalAlign::Center,
                    font: "Helvetica".to_owned(),
                    name: "t1".to_owned(),
                    custom_data: vec![("fieldId".to_owned(), "f-2".to_owned())],
                }),
            ],
            &[],
        )
        .unwrap();
        let page = format!("/Rotate {rotation}");
        let mut from_viewer = doc_with(1, &page, "", "", &[]);
        let mut from_written = doc_with(1, &page, "", "", &[]);
        apply_xfdf(&mut from_viewer, &viewer, true).unwrap();
        apply_xfdf(&mut from_written, &written, true).unwrap();
        let save = |doc: &Document| doc.save(&SaveOptions::default()).unwrap();
        assert!(
            save(&from_viewer) == save(&from_written),
            "rotation {rotation}"
        );
    }
}

#[test]
fn a_drawn_signature_is_stroked_where_its_points_are() {
    let mut doc = doc_with(1, "", "", "", &[]);
    let ink = "<ink color=\"#000000\" flags=\"print,norotate,locked\" name=\"i-1\" page=\"0\" \
        rect=\"100,200,180,240\" subject=\"Signature\" title=\"Signer\">\
        <trn-custom-data bytes=\"{&quot;trn-signature-type&quot;:&quot;ink&quot;}\"/>\
        <inklist><gesture>100,210;120,230;150,215</gesture><gesture>160,205</gesture></inklist></ink>";
    let applied = apply_xfdf(&mut doc, &exported(&[ink], "<delete />"), true).unwrap();
    assert_eq!((applied.inks, applied.pictures, applied.texts), (1, 0, 0));
    assert_eq!(applied.pages, [0]);
    let doc = saved(&doc);
    let painted = painted(&doc, &page(&doc, 0));
    let [(form, drawing, bbox)] = painted.as_slice() else {
        panic!("one drawing");
    };
    assert_eq!(
        form.get(b"Subtype").and_then(Object::as_name),
        Some(&b"Form"[..])
    );
    // The strokes' extent, 1.5 around (half the width, and a point).
    assert!(close(*bbox, [98.5, 203.5, 161.5, 231.5]), "{bbox:?}");
    let drawing = String::from_utf8_lossy(drawing);
    assert!(drawing.contains("1 J 1 j 1 w 0 0 0 RG"), "{drawing}");
    assert!(
        drawing.contains("1.5 6.5 m 21.5 26.5 l 51.5 11.5 l S"),
        "{drawing}"
    );
    // A single point is a dot.
    assert!(drawing.contains("61.5 1.5 m 61.5 1.5 l S"), "{drawing}");
}

#[test]
fn an_xfdf_without_annotations_still_saves() {
    let mut doc = doc_with(1, "", "", "", &[]);
    apply_xfdf(&mut doc, &exported(&[], "<delete />"), true).unwrap();
    assert_eq!(saved(&doc).page_count(), 1);
}

#[test]
fn a_free_text_is_drawn_in_its_rectangle_with_helvetica() {
    let mut doc = doc_with(1, "", "", "", &[]);
    let free = "<freetext page=\"0\" rect=\"100,500,220,525\" flags=\"print,readonly,locked\" width=\"0\" \
        TextColor=\"#000000\" FontSize=\"16\"><contents>Jean Été</contents>\
        <defaultappearance>0 0 0 rg /Helvetica 16 Tf</defaultappearance>\
        <defaultstyle>font: Helvetica 16pt; text-align: left; text-vertical-align: center; color: #000000</defaultstyle>\
        </freetext>";
    let applied = apply_xfdf(&mut doc, &exported(&[free], "<delete />"), true).unwrap();
    assert_eq!((applied.texts, applied.characters_replaced), (1, 0));
    let doc = saved(&doc);
    let painted = painted(&doc, &page(&doc, 0));
    let [(form, drawing, bbox)] = painted.as_slice() else {
        panic!("one text");
    };
    assert!(close(*bbox, [100.0, 500.0, 220.0, 525.0]));
    let drawing = String::from_utf8_lossy(drawing);
    assert!(drawing.contains("BT /Helv 16 Tf 0 0 0 rg"), "{drawing}");
    let resources = resolve(&doc, form.get(b"Resources").unwrap());
    let fonts = resolve(&doc, resources.as_dict().unwrap().get(b"Font").unwrap());
    let font = resolve(&doc, fonts.as_dict().unwrap().get(b"Helv").unwrap());
    assert_eq!(
        font.as_dict()
            .unwrap()
            .get(b"BaseFont")
            .and_then(Object::as_name),
        Some(&b"Helvetica"[..])
    );
}

#[test]
fn what_cannot_be_drawn_refuses_the_xfdf_and_nothing_changes() {
    let picture = data_url(&black(4, 4));
    let cases = [
        exported(&[&xfdf_stamp("100,600,160,620", 1, &picture)], "<delete />"),
        exported(
            &[&xfdf_stamp("-9999,-9999,-9900,-9950", 0, &picture)],
            "<delete />",
        ),
        exported(&[&xfdf_stamp("100,600,100,620", 0, &picture)], "<delete />"),
        exported(
            &[&xfdf_stamp("100,600,160,620", 0, &data_url(b"GIF89a"))],
            "<delete />",
        ),
        exported(
            &[
                &xfdf_stamp("100,600,160,620", 0, &picture),
                "<ink page=\"0\" rect=\"10,10,90,30\" />",
            ],
            "<delete />",
        ),
    ];
    for (index, xfdf) in cases.iter().enumerate() {
        let mut doc = doc_with(1, "/Annots [10 0 R]", "", "", &square_note());
        let before = doc.save(&SaveOptions::default()).unwrap();
        assert!(apply_xfdf(&mut doc, xfdf, true).is_err(), "case {index}");
        assert_eq!(
            doc.save(&SaveOptions::default()).unwrap(),
            before,
            "case {index} changed the document"
        );
    }
}

// --- stamps -------------------------------------------------------------------------------------

#[test]
fn a_stamp_lands_in_its_rectangle_of_the_signer_frame() {
    // The frame: page unrotated, origin at the crop box's top-left corner, y down.
    for (page_entries, crop) in [
        ("", [0.0, 0.0, 600.0, 800.0]),
        (
            "/CropBox [50 70 550 760] /Rotate 90",
            [50.0, 70.0, 550.0, 760.0],
        ),
    ] {
        let mut doc = doc_with(1, page_entries, "", "", &[]);
        let stamp = Stamp {
            page_number: 1,
            x: 100.0,
            y: 150.0,
            width: 120.0,
            height: 40.0,
            png: black(240, 80),
            alt: None,
        };
        assert_eq!(stamp_images(&mut doc, &[stamp]).unwrap(), 1);
        let doc = saved(&doc);
        let [(_, _, bbox)] = painted(&doc, &page(&doc, 0)).try_into().unwrap();
        let expected = [
            crop[0] + 100.0,
            crop[3] - 150.0 - 40.0,
            crop[0] + 220.0,
            crop[3] - 150.0,
        ];
        assert!(close(bbox, expected), "{bbox:?} {expected:?}");
    }
}

#[test]
fn a_picture_is_contained_and_centred_never_stretched() {
    let mut doc = doc_with(1, "", "", "", &[]);
    // A square picture in a 200 x 40 rectangle: drawn 40 x 40, centred.
    let stamp = Stamp {
        page_number: 1,
        x: 100.0,
        y: 100.0,
        width: 200.0,
        height: 40.0,
        png: black(100, 100),
        alt: None,
    };
    stamp_images(&mut doc, &[stamp]).unwrap();
    let doc = saved(&doc);
    let [(_, _, bbox)] = painted(&doc, &page(&doc, 0)).try_into().unwrap();
    assert!(close(bbox, [180.0, 660.0, 220.0, 700.0]), "{bbox:?}");
}

#[test]
fn a_described_picture_carries_its_description_around_its_drawing() {
    let mut doc = doc_with(1, "", "", "", &[]);
    let stamp = Stamp {
        page_number: 1,
        x: 100.0,
        y: 100.0,
        width: 200.0,
        height: 40.0,
        png: black(100, 100),
        alt: Some("Signature \u{e9}".to_owned()),
    };
    stamp_images(&mut doc, &[stamp]).unwrap();
    let doc = saved(&doc);
    let drawn = content(&doc, &page(&doc, 0));
    // UTF-16BE after a byte order mark: S, i, ..., e acute.
    assert!(
        drawn.contains("/Span <</Alt <FEFF005300690067006E00610074007500720065002000E9>>> BDC q"),
        "{drawn}"
    );
    assert!(drawn.contains("Do Q EMC"), "{drawn}");
}

#[test]
fn transparency_becomes_a_soft_mask_and_an_opaque_picture_has_none() {
    let transparent = png(8, 8, png::ColorType::Rgba, &[0, 0, 0, 0]);
    let opaque = png(8, 8, png::ColorType::Rgb, &[9, 9, 9]);
    let gray_alpha = png(8, 8, png::ColorType::GrayscaleAlpha, &[0, 128]);
    for (picture, masked) in [(transparent, true), (opaque, false), (gray_alpha, true)] {
        let mut doc = doc_with(1, "", "", "", &[]);
        let stamp = Stamp {
            page_number: 1,
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: picture,
            alt: None,
        };
        stamp_images(&mut doc, &[stamp]).unwrap();
        let doc = saved(&doc);
        let [(image, samples, _)] = painted(&doc, &page(&doc, 0)).try_into().unwrap();
        assert_eq!(
            image.get(b"Subtype").and_then(Object::as_name),
            Some(&b"Image"[..])
        );
        assert_eq!(
            image.get(b"ColorSpace").and_then(Object::as_name),
            Some(&b"DeviceRGB"[..])
        );
        assert_eq!(samples.len(), 8 * 8 * 3);
        assert_eq!(image.contains_key(b"SMask"), masked);
    }
}

#[test]
fn each_stamp_goes_to_its_page_and_the_page_content_stays() {
    let mut doc = doc_with(2, "", "0 g 10 10 5 5 re f", "", &[]);
    let stamp = Stamp {
        page_number: 2,
        x: 100.0,
        y: 100.0,
        width: 50.0,
        height: 50.0,
        png: black(50, 50),
        alt: None,
    };
    stamp_images(&mut doc, &[stamp]).unwrap();
    let doc = saved(&doc);
    assert!(page(&doc, 0).get(b"Resources").is_none());
    let second = page(&doc, 1);
    let content = content(&doc, &second);
    assert!(content.contains("10 10 5 5 re f"));
    // The page's own content runs between q and Q, then the picture is drawn.
    let original = content.find("10 10 5 5 re f").unwrap();
    assert!(content.trim_start().starts_with('q'));
    assert!(content[original..].find("\nQ").unwrap() < content[original..].find(" Do Q").unwrap());
    assert_eq!(xobjects(&doc, &second).len(), 1);
}

#[test]
fn a_stamp_it_cannot_place_is_refused_and_nothing_changes() {
    let small = black(4, 4);
    let cases = [
        Stamp {
            page_number: 0,
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: small.clone(),
            alt: None,
        },
        Stamp {
            page_number: 2,
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: small.clone(),
            alt: None,
        },
        Stamp {
            page_number: 1,
            x: 5000.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: small.clone(),
            alt: None,
        },
        Stamp {
            page_number: 1,
            x: 10.0,
            y: -30.0,
            width: 20.0,
            height: 20.0,
            png: small.clone(),
            alt: None,
        },
        Stamp {
            page_number: 1,
            x: 10.0,
            y: 10.0,
            width: 0.0,
            height: 20.0,
            png: small.clone(),
            alt: None,
        },
        Stamp {
            page_number: 1,
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: b"\xff\xd8\xff\xe0 jpeg".to_vec(),
            alt: None,
        },
        Stamp {
            page_number: 1,
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: black(40, 40)[..60].to_vec(),
            alt: None,
        },
        Stamp {
            page_number: 1,
            x: f64::NAN,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: small,
            alt: None,
        },
    ];
    for (index, stamp) in cases.into_iter().enumerate() {
        let mut doc = doc_with(1, "", "", "", &[]);
        let before = doc.save(&SaveOptions::default()).unwrap();
        assert!(stamp_images(&mut doc, &[stamp]).is_err(), "case {index}");
        assert_eq!(
            doc.save(&SaveOptions::default()).unwrap(),
            before,
            "case {index}"
        );
    }
}

#[test]
fn an_oversized_picture_is_refused_before_it_is_inflated() {
    // 4001 x 4001 is over the cap of 16 million pixels; its PNG is a few kilobytes.
    let huge = png(4001, 4001, png::ColorType::Grayscale, &[0]);
    assert!(huge.len() < 100_000);
    let mut doc = doc_with(1, "", "", "", &[]);
    let stamp = Stamp {
        page_number: 1,
        x: 10.0,
        y: 10.0,
        width: 20.0,
        height: 20.0,
        png: huge,
        alt: None,
    };
    let error = stamp_images(&mut doc, &[stamp]).unwrap_err();
    assert!(matches!(error, Error::Picture { .. }), "{error}");
    assert!(error.to_string().contains("pixels"));
}

#[test]
fn nothing_personal_shows_in_debug_output() {
    let stamp = Stamp {
        page_number: 1,
        x: 1.0,
        y: 1.0,
        width: 1.0,
        height: 1.0,
        png: b"secret".to_vec(),
        alt: None,
    };
    assert!(!format!("{stamp:?}").contains("secret"));
}

// --- flattening ---------------------------------------------------------------------------------

/// Where each appearance must land: its marker, then its annotation's rectangle. A box or a radio
/// button that is off shows its `/Off` appearance, as in a viewer.
const PLACED: [(&str, [f64; 4]); 7] = [
    ("TEXT-FIELD", [60.0, 700.0, 260.0, 720.0]),
    ("CHECKED-ON", [60.0, 660.0, 72.0, 672.0]),
    ("UNCHECKED-OFF", [80.0, 660.0, 92.0, 672.0]),
    ("RADIO-A", [60.0, 620.0, 72.0, 632.0]),
    ("RADIO-B-OFF", [80.0, 620.0, 92.0, 632.0]),
    ("FREETEXT", [300.0, 300.0, 330.0, 400.0]),
    ("STAMP", [400.0, 100.0, 500.0, 200.0]),
];
const NOT_PAINTED: [&str; 5] = [
    "CHECKED-OFF",
    "UNCHECKED-ON",
    "RADIO-B-ON",
    "RADIO-A-OFF",
    "HIDDEN",
];

fn appearance(marker: &str, drawing: &str, bbox: &str, matrix: &str) -> String {
    stream(
        &format!("/Type /XObject /Subtype /Form /BBox [{bbox}] {matrix}"),
        &format!("% {marker}\n{drawing}"),
    )
}

/// One page with its own text, then annotations and fields written as a form tool would.
fn everything(page_entries: &str) -> Document {
    let objects = vec![
        // Annotations.
        (10, "<< /Type /Annot /Subtype /Widget /Rect [60 700 260 720] /T (nom) /FT /Tx /V (Jean) /AP << /N 30 0 R >> >>".to_owned()),
        (11, "<< /Type /Annot /Subtype /Widget /Rect [60 660 72 672] /T (checked) /FT /Btn /V /Yes /AS /Yes /AP << /N << /Yes 31 0 R /Off 32 0 R >> >> >>".to_owned()),
        (12, "<< /Type /Annot /Subtype /Widget /Rect [80 660 92 672] /T (unchecked) /FT /Btn /V /Off /AS /Off /AP << /N << /Yes 33 0 R /Off 34 0 R >> >> >>".to_owned()),
        (13, "<< /T (province) /FT /Btn /Ff 32768 /V /A /Kids [14 0 R 15 0 R] >>".to_owned()),
        (14, "<< /Type /Annot /Subtype /Widget /Rect [60 620 72 632] /Parent 13 0 R /AS /A /AP << /N << /A 35 0 R /Off 36 0 R >> >> >>".to_owned()),
        (15, "<< /Type /Annot /Subtype /Widget /Rect [80 620 92 632] /Parent 13 0 R /AS /Off /AP << /N << /B 37 0 R /Off 38 0 R >> >> >>".to_owned()),
        // A free text whose form is turned a quarter by its /Matrix, from a box not at the origin.
        (16, "<< /Type /Annot /Subtype /FreeText /Rect [300 300 330 400] /Contents (Note) /AP << /N 39 0 R >> >>".to_owned()),
        (17, "<< /Type /Annot /Subtype /Stamp /Rect [400 100 500 200] /AP << /N 40 0 R >> >>".to_owned()),
        (18, "<< /Type /Annot /Subtype /Square /Rect [100 100 200 200] /F 2 /AP << /N 41 0 R >> >>".to_owned()),
        (19, "<< /Type /Annot /Subtype /Link /Rect [50 50 150 70] /A << /S /URI /URI (https://example.com) >> >>".to_owned()),
        (20, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_owned()),
        // Appearances.
        (30, appearance("TEXT-FIELD", "BT /Helv 10 Tf 2 5 Td (Jean) Tj ET", "0 0 200 20", "/Resources << /Font << /Helv 20 0 R >> >>")),
        (31, appearance("CHECKED-ON", "0 g 0 0 12 12 re f", "0 0 12 12", "")),
        (32, appearance("CHECKED-OFF", "0 G 0 0 12 12 re S", "0 0 12 12", "")),
        (33, appearance("UNCHECKED-ON", "0 g 0 0 12 12 re f", "0 0 12 12", "")),
        (34, appearance("UNCHECKED-OFF", "0 G 0 0 12 12 re S", "0 0 12 12", "")),
        (35, appearance("RADIO-A", "0 g 6 6 4 4 re f", "0 0 12 12", "")),
        (36, appearance("RADIO-A-OFF", "", "0 0 12 12", "")),
        (37, appearance("RADIO-B-ON", "0 g 6 6 4 4 re f", "0 0 12 12", "")),
        (38, appearance("RADIO-B-OFF", "", "0 0 12 12", "")),
        (39, appearance("FREETEXT", "BT /Helv 10 Tf 2 5 Td (Note) Tj ET", "10 10 110 40", "/Matrix [0 1 -1 0 0 0] /Resources << /Font << /Helv 20 0 R >> >>")),
        (40, appearance("STAMP", "1 0 0 rg 0 0 50 50 re f", "0 0 50 50", "/Matrix [2 0 0 2 5 5]")),
        (41, appearance("HIDDEN", "0 g 0 0 100 100 re f", "0 0 100 100", "")),
    ];
    doc_with(
        1,
        &format!(
            "/Annots [10 0 R 11 0 R 12 0 R 14 0 R 15 0 R 16 0 R 17 0 R 18 0 R 19 0 R] /Resources << /Font << /F1 20 0 R >> >> {page_entries}"
        ),
        "BT /F1 12 Tf 50 750 Td (Original page text) Tj ET",
        "/AcroForm << /Fields [10 0 R 11 0 R 12 0 R 13 0 R] >> /Perms << /DocMDP 10 0 R >>",
        &objects,
    )
}

fn marker(data: &[u8]) -> String {
    String::from_utf8_lossy(data)
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("% ")
        .to_owned()
}

#[test]
fn every_annotation_becomes_page_content() {
    for page_entries in ["", "/Rotate 90 /CropBox [36 48 576 756]"] {
        let mut doc = everything(page_entries);
        let report = flatten(&mut doc).unwrap();
        assert_eq!((report.painted, report.drawn, report.links_kept), (7, 0, 1));
        assert!(report.dropped.is_empty());
        let doc = saved(&doc);
        let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
        // Nothing editable is left: no widget, no field, no markup; the link stays.
        assert!(!catalog.as_dict().unwrap().contains_key(b"AcroForm"));
        assert!(!catalog.as_dict().unwrap().contains_key(b"Perms"));
        let page = page(&doc, 0);
        let annots = resolve(&doc, page.get(b"Annots").unwrap());
        let subtypes: Vec<Vec<u8>> = annots
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                resolve(&doc, a)
                    .as_dict()
                    .unwrap()
                    .get(b"Subtype")
                    .unwrap()
                    .as_name()
                    .unwrap()
                    .to_vec()
            })
            .collect();
        assert_eq!(subtypes, [b"Link".to_vec()]);
        // Each appearance sits exactly on its annotation's rectangle.
        let painted = painted(&doc, &page);
        let found: Vec<(String, [f64; 4])> = painted
            .iter()
            .map(|(_, data, bbox)| (marker(data), *bbox))
            .collect();
        assert_eq!(found.len(), PLACED.len(), "{found:?}");
        for (name, rect) in PLACED {
            let (_, bbox) = found
                .iter()
                .find(|(m, _)| m == name)
                .unwrap_or_else(|| panic!("{name} not painted"));
            assert!(close(*bbox, rect), "{name}: {bbox:?}");
        }
        for name in NOT_PAINTED {
            assert!(found.iter().all(|(m, _)| m != name), "{name} painted");
        }
        // The page keeps its text and its geometry.
        assert!(content(&doc, &page).contains("(Original page text) Tj"));
        assert_eq!(page.contains_key(b"Rotate"), !page_entries.is_empty());
        assert_eq!(doc.geometry(), everything(page_entries).geometry());
    }
}

#[test]
fn the_page_own_drawing_state_cannot_shift_the_appearances() {
    let mut doc = everything("");
    flatten(&mut doc).unwrap();
    let doc = saved(&doc);
    let content = content(&doc, &page(&doc, 0));
    let original = content.find("Original page text").unwrap();
    let first = content.find(" Do Q").unwrap();
    assert!(content.trim_start().starts_with('q'));
    assert!(content[original..first].contains("\nQ"));
}

fn needs_appearances(widget: &str) -> Document {
    doc_with(
        1,
        "/Annots [10 0 R]",
        "",
        "/AcroForm << /Fields [10 0 R] /NeedAppearances true /DA (/Helv 0 Tf 0 g) >>",
        &[(
            10,
            format!(
                "<< /Type /Annot /Subtype /Widget /Rect [60 600 300 624] /T (ville) /FT /Tx /V (Montreal Quebec) {widget} >>"
            ),
        )],
    )
}

#[test]
fn a_value_without_appearance_is_drawn_from_its_default_appearance() {
    for widget in [
        "",
        "/DA (/Helv 11 Tf 0 0 1 rg) /Q 1",
        "/DA (/Helv 9 Tf 0 g) /Q 2 /Ff 4096",
    ] {
        let mut doc = needs_appearances(widget);
        let report = flatten(&mut doc).unwrap();
        assert_eq!(report.drawn, 1, "{widget}");
        let doc = saved(&doc);
        let page = page(&doc, 0);
        assert!(!page.contains_key(b"Annots"));
        let [(form, drawing, bbox)] = painted(&doc, &page).try_into().unwrap();
        assert!(close(bbox, [60.0, 600.0, 300.0, 624.0]));
        assert!(
            String::from_utf8_lossy(&drawing).contains("(Montreal Quebec) Tj"),
            "{widget}"
        );
        let resources = resolve(&doc, form.get(b"Resources").unwrap());
        assert!(resources.as_dict().unwrap().contains_key(b"Font"));
    }
}

#[test]
fn a_saved_appearance_is_kept_when_the_form_says_it_needs_none() {
    let mut doc = doc_with(
        1,
        "/Annots [10 0 R]",
        "",
        "/AcroForm << /Fields [10 0 R] /NeedAppearances false >>",
        &[
            (10, "<< /Type /Annot /Subtype /Widget /Rect [60 600 300 624] /FT /Tx /V (x) /AP << /N 11 0 R >> >>".to_owned()),
            (11, appearance("SAVED", "", "0 0 240 24", "")),
        ],
    );
    let report = flatten(&mut doc).unwrap();
    assert_eq!((report.painted, report.drawn), (1, 0));
    let doc = saved(&doc);
    let painted = painted(&doc, &page(&doc, 0));
    assert_eq!(
        painted.iter().map(|p| marker(&p.1)).collect::<Vec<_>>(),
        ["SAVED"]
    );
}

#[test]
fn a_box_drawn_without_appearance_still_covers_what_is_under_it() {
    let mut doc = doc_with(
        1,
        "/Annots [10 0 R]",
        "",
        "",
        &[(
            10,
            "<< /Type /Annot /Subtype /Square /Rect [40 740 200 770] /IC [0 0 0] /C [0 0 0] >>"
                .to_owned(),
        )],
    );
    let report = flatten(&mut doc).unwrap();
    assert_eq!(report.drawn, 1);
    let doc = saved(&doc);
    let page = page(&doc, 0);
    assert!(!page.contains_key(b"Annots"));
    let [(_, drawing, bbox)] = painted(&doc, &page).try_into().unwrap();
    assert!(close(bbox, [40.0, 740.0, 200.0, 770.0]));
    // As PDFium draws it: filled with /IC, stroked with /C, inside the rectangle.
    let drawing = String::from_utf8_lossy(&drawing);
    assert_eq!(
        drawing,
        "/GS gs 0 0 0 rg\n0 0 0 RG\n1 w 40.5 740.5 159 29 re b\n"
    );
}

#[test]
fn annotations_without_appearance_are_drawn_as_pdfium_draws_them() {
    let mut doc = doc_with(
        1,
        "/Annots [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R]",
        "",
        "",
        &[
            // A highlight whose /Rect is wider than its words: drawn on the words.
            (
                10,
                "<< /Type /Annot /Subtype /Highlight /Rect [30 730 300 780] /CA 0.5 \
                  /QuadPoints [40 770 200 770 40 740 200 740] >>"
                    .to_owned(),
            ),
            (
                11,
                "<< /Type /Annot /Subtype /Text /Rect [400 400 450 460] >>".to_owned(),
            ),
            (
                12,
                "<< /Type /Annot /Subtype /Ink /Rect [100 100 200 200] /BS << /W 4 >> \
                  /InkList [[110 110 190 190]] /C [1 0 0] >>"
                    .to_owned(),
            ),
            (
                13,
                "<< /Type /Annot /Subtype /Circle /Rect [300 100 400 150] /IC [0 0 1] >>"
                    .to_owned(),
            ),
            (
                14,
                "<< /Type /Annot /Subtype /Squiggly /Rect [40 600 200 620] \
                  /QuadPoints [40 620 47 620 40 600 47 600] >>"
                    .to_owned(),
            ),
        ],
    );
    let report = flatten(&mut doc).unwrap();
    assert_eq!(report.drawn, 5);
    assert!(report.dropped.is_empty());
    let doc = saved(&doc);
    let painted = painted(&doc, &page(&doc, 0));
    let boxes: Vec<[f64; 4]> = painted.iter().map(|p| p.2).collect();
    assert!(close(boxes[0], [40.0, 740.0, 200.0, 770.0]), "{boxes:?}");
    assert!(close(boxes[1], [400.0, 400.0, 420.0, 420.0]), "{boxes:?}");
    assert!(close(boxes[2], [98.0, 98.0, 202.0, 202.0]), "{boxes:?}");
    assert!(close(boxes[3], [300.0, 100.0, 400.0, 150.0]), "{boxes:?}");
    let highlight = String::from_utf8_lossy(&painted[0].1);
    assert_eq!(
        highlight,
        "/GS gs 1 1 0 rg\n40 770 m 200 770 l 200 740 l 40 740 l h f\n"
    );
    let resources = resolve(&doc, painted[0].0.get(b"Resources").unwrap());
    let states = resolve(
        &doc,
        resources.as_dict().unwrap().get(b"ExtGState").unwrap(),
    );
    let state = resolve(&doc, states.as_dict().unwrap().get(b"GS").unwrap());
    let state = state.as_dict().unwrap();
    assert_eq!(
        state.get(b"BM").and_then(Object::as_name),
        Some(&b"Multiply"[..])
    );
    assert_eq!(state.get(b"CA").and_then(Object::as_f64), Some(0.5));
    assert!(String::from_utf8_lossy(&painted[1].1).ends_with("B*\n"));
    assert_eq!(
        String::from_utf8_lossy(&painted[2].1),
        "/GS gs 1 0 0 RG\n4 w 110 110 m 110 110 l 190 190 l S\n"
    );
    assert!(String::from_utf8_lossy(&painted[3].1).ends_with(" c\nb\n"));
    assert_eq!(
        String::from_utf8_lossy(&painted[4].1),
        "/GS gs 0 0 0 RG\n1 w 40 602 m 42 600 l 44 602 l 46 600 l 47 601 l S\n"
    );
}

#[test]
fn an_annotation_nothing_draws_is_counted_and_a_pdf_with_nothing_to_flatten_stays_valid() {
    let mut doc = doc_with(
        1,
        "/Annots [10 0 R]",
        "",
        "",
        &[(
            10,
            "<< /Type /Annot /Subtype /Line /Rect [40 740 200 770] /L [40 740 200 770] >>"
                .to_owned(),
        )],
    );
    let report = flatten(&mut doc).unwrap();
    assert_eq!(report.dropped.get("Line"), Some(&1));
    let mut empty = doc_with(1, "", "", "", &[]);
    let report = flatten(&mut empty).unwrap();
    assert_eq!(report, sealwax_annots::Flattened::default());
    assert_eq!(saved(&empty).page_count(), 1);
}

#[test]
fn a_free_text_without_appearance_keeps_its_text() {
    let mut doc = doc_with(
        1,
        "/Annots [10 0 R]",
        "",
        "/AcroForm << /Fields [] /DA (/Helv 0 Tf 0 g) >>",
        &[(
            10,
            "<< /Type /Annot /Subtype /FreeText /Rect [100 600 300 640] /DA (1 0 0 rg /Helv 14 Tf) \
             /Q 1 /Contents (Note au dossier) /C [1 1 0.8] >>"
                .to_owned(),
        )],
    );
    let report = flatten(&mut doc).unwrap();
    assert_eq!(report.drawn, 1);
    assert!(report.dropped.is_empty());
    let doc = saved(&doc);
    let [(_, drawing, bbox)] = painted(&doc, &page(&doc, 0)).try_into().unwrap();
    assert!(close(bbox, [100.0, 600.0, 300.0, 640.0]));
    let drawing = String::from_utf8_lossy(&drawing);
    assert!(drawing.contains("1 1 0.8 rg 0 0 200 40 re f"), "{drawing}");
    assert!(drawing.contains("1 0 0 RG 1 w"), "{drawing}");
    assert!(drawing.contains("BT /Helv 14 Tf 1 0 0 rg"), "{drawing}");
    assert!(drawing.contains("(Note au dossier) Tj"), "{drawing}");
}

#[test]
fn documents_merge_flattened_in_order() {
    let first = everything("");
    let second = doc_with(
        2,
        "/Annots [10 0 R]",
        "0 g 10 10 5 5 re f",
        "",
        &square_note(),
    );
    let (merged, reports) = sealwax_annots::merge(vec![first, second]).unwrap();
    assert_eq!(merged.page_count(), 3);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[1].painted, 2);
    let merged = saved(&merged);
    assert_eq!(merged.page_count(), 3);
    let catalog = merged.object(merged.catalog_id()).unwrap().unwrap();
    assert!(!catalog.as_dict().unwrap().contains_key(b"AcroForm"));
    // The first source's page: its appearances painted, its link kept.
    let page0 = page(&merged, 0);
    assert_eq!(painted(&merged, &page0).len(), 7);
    assert_eq!(annotation_names(&merged, &page0).len(), 1);
    // The second source's pages: the square painted on each, no annotation left.
    for index in [1, 2] {
        let page = page(&merged, index);
        assert!(!page.contains_key(b"Annots"));
        assert!(content(&merged, &page).contains("10 10 5 5 re f"));
        assert_eq!(painted(&merged, &page).len(), 1);
    }
}

/// The document's limits hold for XFDF and stamps: marks count against `max_annotations`, and
/// pictures, each and together, against `max_image_pixels`, before any page changes (external
/// review: a 2 x 2 stamp went through both at zero).
#[test]
fn xfdf_and_stamps_stay_within_the_documents_limits() {
    let picture = data_url(&black(2, 2));
    let xfdf = exported(&[&xfdf_stamp("10,10,40,40", 0, &picture)], "");
    let open = |limits: sealwax_core::Limits| {
        let data = saved(&doc_with(1, "", "", "", &[]))
            .save(&SaveOptions::default())
            .unwrap();
        Document::open(data, &OpenOptions::default().limits(limits)).unwrap()
    };
    let mut no_marks = sealwax_core::Limits::default();
    no_marks.max_annotations = 0;
    let mut one_pixel = sealwax_core::Limits::default();
    one_pixel.max_image_pixels = 1;
    for (limits, bound) in [
        (no_marks, "max_annotations"),
        (one_pixel, "max_image_pixels"),
    ] {
        let mut doc = open(limits);
        match apply_xfdf(&mut doc, &xfdf, true) {
            Err(Error::Pdf(sealwax_core::Error::LimitExceeded { what })) => assert_eq!(what, bound),
            other => panic!("{bound}: not refused: {other:?}"),
        }
        assert!(!doc.is_modified(), "{bound}: nothing changes");
        let stamp = Stamp {
            page_number: 1,
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            png: black(2, 2),
            alt: None,
        };
        match stamp_images(&mut doc, &[stamp]) {
            Err(Error::Pdf(sealwax_core::Error::LimitExceeded { what })) => assert_eq!(what, bound),
            other => panic!("{bound}: stamp not refused: {other:?}"),
        }
    }
    let mut doc = open(sealwax_core::Limits::default());
    assert_eq!(apply_xfdf(&mut doc, &xfdf, true).unwrap().pictures, 1);
}
