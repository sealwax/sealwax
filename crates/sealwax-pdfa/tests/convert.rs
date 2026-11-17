#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sealwax_core::{Document, Object, OpenOptions, SaveOptions};
use sealwax_pdfa::{Level, Options, Report, convert};

/// A PDF made of `objects` (`(number, body)`, object 1 the catalog), with `trailer` added to
/// the trailer dictionary.
fn build(objects: &[(u32, String)], trailer: &str) -> Vec<u8> {
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
        format!("trailer\n<< /Size {size} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// A one-page document: catalog 1, pages 2, page 3 drawing `content` (object 4), with `page`
/// added to the page dictionary and `more` objects after.
fn page_doc(content: &str, page: &str, more: &[(u32, String)]) -> Vec<u8> {
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R {page} >>"
            ),
        ),
        (4, stream("", content)),
    ];
    objects.extend(more.iter().cloned());
    build(&objects, "")
}

/// Convert, save, and read the output back.
fn convert_and_reopen(data: Vec<u8>, level: Level) -> (Document, Report) {
    let mut doc = Document::open(data, &OpenOptions::default()).expect("opens");
    let report = convert(&mut doc, &Options::new(level)).expect("converts");
    let saved = doc.save(&SaveOptions::default()).expect("saves");
    (
        Document::open(saved, &OpenOptions::default()).expect("reads back"),
        report,
    )
}

fn get(doc: &Document, object: &Object) -> Object {
    match object {
        Object::Ref(id) => doc.object(*id).unwrap().expect("resolves"),
        other => other.clone(),
    }
}

fn catalog(doc: &Document) -> Object {
    doc.object(doc.catalog_id()).unwrap().unwrap()
}

fn page(doc: &Document) -> Object {
    doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap()
}

fn key<'o>(object: &'o Object, key: &[u8]) -> &'o Object {
    object
        .as_dict()
        .and_then(|d| d.get(key))
        .unwrap_or_else(|| panic!("no /{}", String::from_utf8_lossy(key)))
}

/// The data of a stream, inflated when it is compressed.
fn inflated(doc: &Document, object: &Object) -> Vec<u8> {
    use std::io::Read as _;
    let Object::Stream(stream) = get(doc, object) else {
        panic!("not a stream");
    };
    if !stream.dict.contains_key(b"Filter") {
        return stream.data;
    }
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(stream.data.as_slice())
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn metadata(doc: &Document) -> String {
    let Object::Stream(stream) = get(doc, key(&catalog(doc), b"Metadata")) else {
        panic!("metadata is not a stream");
    };
    assert!(!stream.dict.contains_key(b"Filter"), "XMP stays readable");
    String::from_utf8(stream.data).unwrap()
}

#[test]
fn a_document_gets_an_srgb_output_intent_and_its_identification() {
    let (doc, report) = convert_and_reopen(page_doc("1 0 0 rg 0 0 10 10 re f", "", &[]), Level::B);
    let intents = get(&doc, key(&catalog(&doc), b"OutputIntents"));
    let intent = get(&doc, &intents.as_array().unwrap()[0]);
    assert_eq!(key(&intent, b"S").as_name(), Some(&b"GTS_PDFA1"[..]));
    let profile = get(&doc, key(&intent, b"DestOutputProfile"));
    assert_eq!(key(&profile, b"N").as_i64(), Some(3));
    let xmp = metadata(&doc);
    assert!(xmp.contains("<pdfaid:part>3</pdfaid:part>"));
    assert!(xmp.contains("<pdfaid:conformance>B</pdfaid:conformance>"));
    assert_eq!(report.changes().get("output_intent_added"), Some(&1));
    assert_eq!(report.changes().get("metadata_added"), Some(&1));
    assert!(report.residual().is_empty(), "{:?}", report.residual());
}

#[test]
fn cmyk_content_draws_through_a_default_color_space() {
    let (doc, report) = convert_and_reopen(
        page_doc("0 0 0 1 k 0 0 10 10 re f", "/Resources << >>", &[]),
        Level::B,
    );
    let resources = get(&doc, key(&page(&doc), b"Resources"));
    let spaces = get(&doc, key(&resources, b"ColorSpace"));
    let default = get(&doc, key(&spaces, b"DefaultCMYK"));
    let items = default.as_array().unwrap();
    assert_eq!(items[0].as_name(), Some(&b"ICCBased"[..]));
    assert_eq!(key(&get(&doc, &items[1]), b"N").as_i64(), Some(4));
    assert!(!spaces.as_dict().unwrap().contains_key(b"DefaultRGB"));
    assert_eq!(report.changes().get("default_color_space_added"), Some(&1));
}

#[test]
fn a_page_inheriting_direct_resources_gets_its_own_copy() {
    let data = build(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /Resources << /ProcSet [/PDF] >> >>"
                    .to_owned(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
                    .to_owned(),
            ),
            (4, stream("", "0 0 1 0 k 0 0 10 10 re f")),
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 6 0 R >>"
                    .to_owned(),
            ),
            (6, stream("", "0 0 10 10 re f")),
        ],
        "",
    );
    let (doc, _) = convert_and_reopen(data, Level::B);
    let first = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
    let resources = get(&doc, key(&first, b"Resources"));
    assert!(key(&resources, b"ProcSet").as_array().is_some());
    let spaces = get(&doc, key(&resources, b"ColorSpace"));
    assert!(spaces.as_dict().unwrap().contains_key(b"DefaultCMYK"));
    // The second page draws no CMYK: its resources stay as they were.
    let second = doc.object(doc.page_ids()[1].unwrap()).unwrap().unwrap();
    let resources = get(&doc, key(&second, b"Resources"));
    assert!(!resources.as_dict().unwrap().contains_key(b"ColorSpace"));
}

#[test]
fn cmyk_in_a_form_xobject_is_found_there() {
    let (doc, _) = convert_and_reopen(
        page_doc(
            "/X1 Do",
            "/Resources << /XObject << /X1 5 0 R >> >>",
            &[(
                5,
                stream(
                    "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << >>",
                    "0 1 0 0 k 0 0 10 10 re f",
                ),
            )],
        ),
        Level::B,
    );
    let form = doc
        .reachable_objects()
        .unwrap()
        .into_iter()
        .filter_map(|id| doc.object(id).unwrap())
        .find(|o| {
            o.as_dict()
                .and_then(|d| d.get(b"Subtype"))
                .and_then(Object::as_name)
                == Some(b"Form")
        })
        .expect("the form is kept");
    let spaces = get(&doc, key(key(&form, b"Resources"), b"ColorSpace"));
    assert!(spaces.as_dict().unwrap().contains_key(b"DefaultCMYK"));
}

#[test]
fn the_xmp_packet_says_what_the_information_dictionary_says() {
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_owned(),
        ),
    ];
    // "Été & <co>" in UTF-16BE, an author in PDFDocEncoding with a bullet (0x80), a subject in
    // UTF-8 without a byte order mark.
    objects.push((
        4,
        "<< /Title <FEFF00C9007400E9002000260020003C0063006F003E> /Author (A\\200B) \
         /Subject (Caf\\303\\251) /CreationDate (D:20231005142233+02'00') /ModDate (D:2024) >>"
            .to_owned(),
    ));
    let (doc, report) = convert_and_reopen(build(&objects, "/Info 4 0 R"), Level::U);
    let xmp = metadata(&doc);
    assert!(xmp.contains("<pdfaid:conformance>U</pdfaid:conformance>"));
    assert!(xmp.contains(">Été &amp; &lt;co&gt;</rdf:li>"), "{xmp}");
    assert!(xmp.contains("<rdf:li>A\u{2022}B</rdf:li>"), "{xmp}");
    assert!(
        xmp.contains(">Café</rdf:li></rdf:Alt></dc:description>"),
        "{xmp}"
    );
    // The subject is rewritten in UTF-16BE, which a reader of the standard reads as "Café" too;
    // the rest is left as written.
    let info = doc.object(doc.info_id().unwrap()).unwrap().unwrap();
    assert_eq!(
        key(&info, b"Subject").as_string(),
        Some(&b"\xFE\xFF\x00C\x00a\x00f\x00\xE9"[..])
    );
    assert_eq!(key(&info, b"Author").as_string(), Some(&b"A\x80B"[..]));
    assert_eq!(report.changes().get("info_reencoded"), Some(&1));
    assert!(xmp.contains("<xmp:CreateDate>2023-10-05T14:22:33+02:00</xmp:CreateDate>"));
    assert!(xmp.contains("<xmp:ModifyDate>2024</xmp:ModifyDate>"));
}

#[test]
fn actions_pdfa_forbids_are_removed_and_the_others_kept() {
    let (doc, report) = convert_and_reopen(
        page_doc(
            "",
            "/Annots [5 0 R 6 0 R 7 0 R] /AA << /O 8 0 R >>",
            &[
                (
                    5,
                    "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 8 0 R >>".to_owned(),
                ),
                (
                    6,
                    "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /F 4 \
                     /A << /S /URI /URI (https://example.org) >> >>"
                        .to_owned(),
                ),
                (
                    7,
                    "<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] /F 4 /FT /Btn \
                     /A << /S /URI /URI (https://example.org) >> /AA << /D 8 0 R >> \
                     /AP << /N << /Off 9 0 R >> /D << /Off 9 0 R >> >> >>"
                        .to_owned(),
                ),
                (8, "<< /S /JavaScript /JS (app.alert(1)) >>".to_owned()),
                (
                    9,
                    stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", ""),
                ),
            ],
        ),
        Level::B,
    );
    let page = page(&doc);
    assert!(!page.as_dict().unwrap().contains_key(b"AA"));
    let annots = get(&doc, key(&page, b"Annots"));
    let annots: Vec<Object> = annots
        .as_array()
        .unwrap()
        .iter()
        .map(|a| get(&doc, a))
        .collect();
    // The JavaScript link loses its action and gains the print flag.
    assert!(!annots[0].as_dict().unwrap().contains_key(b"A"));
    assert_eq!(key(&annots[0], b"F").as_i64(), Some(4));
    // A link to a web page keeps it.
    assert!(annots[1].as_dict().unwrap().contains_key(b"A"));
    // A widget keeps no action, and only its normal appearance.
    let widget = annots[2].as_dict().unwrap();
    assert!(!widget.contains_key(b"A") && !widget.contains_key(b"AA"));
    let appearance = get(&doc, key(&annots[2], b"AP"));
    assert!(!appearance.as_dict().unwrap().contains_key(b"D"));
    assert!(appearance.as_dict().unwrap().contains_key(b"N"));
    assert_eq!(report.changes().get("page_action_removed"), Some(&1));
}

#[test]
fn char_set_cid_set_and_cid_to_gid_map() {
    let (doc, report) = convert_and_reopen(
        page_doc(
            "BT /F1 12 Tf (a) Tj /F2 12 Tf <0001> Tj ET",
            "/Resources << /Font << /F1 5 0 R /F2 7 0 R >> >>",
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FontDescriptor 6 0 R >>"
                        .to_owned(),
                ),
                (
                    6,
                    "<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 /CharSet (/a) >>"
                        .to_owned(),
                ),
                (
                    7,
                    "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
                     /DescendantFonts [8 0 R] >>"
                        .to_owned(),
                ),
                (
                    8,
                    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X \
                     /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                     /FontDescriptor 9 0 R >>"
                        .to_owned(),
                ),
                (
                    9,
                    "<< /Type /FontDescriptor /FontName /X /Flags 4 /CIDSet 10 0 R >>".to_owned(),
                ),
                (10, stream("", "\u{1}")),
            ],
        ),
        Level::B,
    );
    for change in [
        "char_set_removed",
        "cid_set_removed",
        "cid_to_gid_map_added",
    ] {
        assert_eq!(report.changes().get(change), Some(&1), "{change}");
    }
    // Helvetica gets the program of its standard font; the composite font cannot, without a
    // `ToUnicode` CMap to tell what its CIDs mean.
    assert_eq!(report.changes().get("font_embedded"), Some(&1));
    assert_eq!(report.residual().get("font_not_embedded"), Some(&1));
    assert_eq!(
        report.residual().get("composite_font_without_to_unicode"),
        Some(&1)
    );
    let objects: Vec<Object> = doc
        .reachable_objects()
        .unwrap()
        .into_iter()
        .filter_map(|id| doc.object(id).unwrap())
        .collect();
    assert!(objects.iter().all(|o| {
        o.as_dict()
            .is_none_or(|d| !d.contains_key(b"CharSet") && !d.contains_key(b"CIDSet"))
    }));
    assert!(objects.iter().any(|o| {
        o.as_dict()
            .and_then(|d| d.get(b"CIDToGIDMap"))
            .and_then(Object::as_name)
            == Some(b"Identity")
    }));
}

#[test]
fn object_metadata_overprint_mode_and_process_spaces() {
    let (doc, report) = convert_and_reopen(
        page_doc(
            "/GS1 gs /CS1 cs 1 scn 0 0 10 10 re f",
            "/Metadata 5 0 R /Resources << /ExtGState << /GS1 << /OP true /OPM 1 >> >> \
             /ColorSpace << /CS1 [/DeviceN [/Spot] /DeviceCMYK 6 0 R \
             << /Subtype /NChannel /Process << /ColorSpace /DeviceCMYK /Components [/Cyan /Magenta /Yellow /Black] >> \
             /Colorants << >> >>] >> >>",
            &[
                (5, stream("/Type /Metadata /Subtype /XML", "<x:xmpmeta/>")),
                (
                    6,
                    "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0 0 0] /N 1 >>"
                        .to_owned(),
                ),
            ],
        ),
        Level::B,
    );
    let page = page(&doc);
    assert!(!page.as_dict().unwrap().contains_key(b"Metadata"));
    let resources = get(&doc, key(&page, b"Resources"));
    let state = get(&doc, key(&get(&doc, key(&resources, b"ExtGState")), b"GS1"));
    assert_eq!(key(&state, b"OPM").as_i64(), Some(0));
    let spaces = get(&doc, key(&resources, b"ColorSpace"));
    let device_n = get(&doc, key(&spaces, b"CS1"));
    let attributes = get(&doc, &device_n.as_array().unwrap()[4]);
    let process = get(&doc, key(&attributes, b"Process"));
    let space = key(&process, b"ColorSpace")
        .as_array()
        .expect("an ICC-based space");
    assert_eq!(space[0].as_name(), Some(&b"ICCBased"[..]));
    // The alternate is DeviceCMYK too, used by the page: a default space covers it.
    assert!(spaces.as_dict().unwrap().contains_key(b"DefaultCMYK"));
    assert_eq!(report.changes().get("object_metadata_removed"), Some(&1));
    assert_eq!(report.changes().get("overprint_mode_reset"), Some(&1));
    assert_eq!(
        report.changes().get("process_color_space_replaced"),
        Some(&1)
    );
}

#[test]
fn images_stop_asking_for_interpolation() {
    let (doc, report) = convert_and_reopen(
        page_doc(
            "q 10 0 0 10 0 0 cm /Im1 Do Q BI /W 1 /H 1 /CS /G /BPC 8 /I true ID \u{0} EI",
            "/Resources << /XObject << /Im1 5 0 R >> >>",
            &[(
                5,
                stream(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
                     /BitsPerComponent 8 /Interpolate true",
                    "\u{0}",
                ),
            )],
        ),
        Level::B,
    );
    assert_eq!(report.changes().get("interpolation_removed"), Some(&1));
    // An inline image would need its content rewritten: reported, not repaired.
    assert_eq!(report.residual().get("inline_image_interpolated"), Some(&1));
    let image = doc
        .reachable_objects()
        .unwrap()
        .into_iter()
        .filter_map(|id| doc.object(id).unwrap())
        .find(|o| {
            o.as_dict()
                .and_then(|d| d.get(b"Subtype"))
                .and_then(Object::as_name)
                == Some(b"Image")
        })
        .unwrap();
    assert_eq!(key(&image, b"Interpolate"), &Object::Bool(false));
}

#[test]
fn optional_content_configurations_get_a_name() {
    let data = build(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [4 0 R] \
                 /D << /ON [4 0 R] /AS [<< /Event /View /OCGs [4 0 R] /Category [/View] >>] >> \
                 /Configs [<< /Name (Print) >> << >>] >> >>"
                    .to_owned(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_owned(),
            ),
            (4, "<< /Type /OCG /Name (Layer) >>".to_owned()),
        ],
        "",
    );
    let (doc, report) = convert_and_reopen(data, Level::B);
    let properties = get(&doc, key(&catalog(&doc), b"OCProperties"));
    let default = get(&doc, key(&properties, b"D"));
    assert_eq!(key(&default, b"Name").as_string(), Some(&b"Default"[..]));
    assert!(!default.as_dict().unwrap().contains_key(b"AS"));
    let configs = get(&doc, key(&properties, b"Configs"));
    let names: Vec<Vec<u8>> = configs
        .as_array()
        .unwrap()
        .iter()
        .map(|c| key(&get(&doc, c), b"Name").as_string().unwrap().to_vec())
        .collect();
    assert_eq!(names[0], b"Print");
    assert!(!names[1].is_empty() && names[1] != b"Print" && names[1] != b"Default");
    assert_eq!(report.changes().get("optional_content_named"), Some(&2));
}

#[test]
fn an_existing_pdfa_output_intent_is_kept() {
    let (doc, report) = convert_and_reopen(
        build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OutputIntents [<< /Type /OutputIntent \
                     /S /GTS_PDFA1 /OutputConditionIdentifier (Custom) /DestOutputProfile 4 0 R >>] >>"
                        .to_owned(),
                ),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 5 0 R >>"
                        .to_owned(),
                ),
                (4, stream("/N 4", "")),
                (5, stream("", "1 0 0 rg 0 0 1 0 k 0 0 10 10 re f")),
            ],
            "",
        ),
        Level::B,
    );
    let intents = get(&doc, key(&catalog(&doc), b"OutputIntents"));
    let intent = get(&doc, &intents.as_array().unwrap()[0]);
    assert_eq!(
        key(&intent, b"OutputConditionIdentifier").as_string(),
        Some(&b"Custom"[..])
    );
    assert!(!report.changes().contains_key("output_intent_added"));
    // Under a CMYK intent, RGB content needs a default space; CMYK does not.
    let resources = get(&doc, key(&page(&doc), b"Resources"));
    let spaces = get(&doc, key(&resources, b"ColorSpace"));
    assert!(spaces.as_dict().unwrap().contains_key(b"DefaultRGB"));
    assert!(!spaces.as_dict().unwrap().contains_key(b"DefaultCMYK"));
}

#[test]
fn a_page_object_defined_twice_keeps_its_page_after_the_conversion() {
    // Object 3 is defined twice: the page, then a stream. The cross-reference sends object 5 to
    // the second definition, so reading the page's content makes hayro repair the table, after
    // which object 3 is the stream (hayro issue #1746). Found by fuzzing.
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    let objects: [&[u8]; 4] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 160] /Contents 4 0 R >>\nendobj\n",
        b"3 0 obj\n<< /Length 8 >>\nstream\n1 0 0 rg\nendstream\nendobj\n",
    ];
    for object in objects {
        offsets.push(out.len());
        out.extend_from_slice(object);
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    let before = Document::open(out.clone(), &OpenOptions::default())
        .unwrap()
        .geometry();
    let (doc, _) = convert_and_reopen(out, Level::B);
    assert_eq!(doc.geometry(), before);
    assert_eq!(before[0].media_box.x2, 200.0);
}

#[test]
fn objects_the_conversion_adds_never_take_the_number_of_a_page() {
    // No /Size in the trailer, and a page that reads as a dictionary but not as a whole object
    // (a stream keyword without a length): its number looked free. The added ICC profile then
    // took it, and the page tree pointed at the profile. Found by fuzzing.
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let objects: [&[u8]; 3] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 144] >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nstream\x02\x00\r\nxx\rendstream\nendobj\n",
    ];
    for object in objects {
        offsets.push(out.len());
        out.extend_from_slice(object);
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    let before = Document::open(out.clone(), &OpenOptions::default())
        .unwrap()
        .geometry();
    assert_eq!(before[0].media_box.x2, 300.0);
    let (doc, _) = convert_and_reopen(out, Level::B);
    assert_eq!(doc.geometry(), before);
    assert!(
        page(&doc)
            .as_dict()
            .unwrap()
            .get(b"Type")
            .and_then(Object::as_name)
            == Some(b"Page")
    );
}

#[test]
fn fonts_sharing_a_descriptor_each_get_their_program() {
    // Two ZapfDingbats dictionaries, one descriptor (form fields often share them). Embedding
    // the first must not make the second look embedded: it still needs its encoding spelled
    // out, or its "n" (a square) would draw nothing.
    let (doc, report) = convert_and_reopen(
        page_doc(
            "BT /F1 12 Tf (4) Tj /F2 12 Tf (n) Tj ET",
            "/Resources << /Font << /F1 5 0 R /F2 6 0 R >> >>",
            &[
                (
                    5,
                    "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats /FontDescriptor 7 0 R >>"
                        .to_owned(),
                ),
                (
                    6,
                    "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats /FontDescriptor 7 0 R >>"
                        .to_owned(),
                ),
                (
                    7,
                    "<< /Type /FontDescriptor /FontName /ZapfDingbats /Flags 4 \
                     /FontBBox [-1 -143 981 820] /ItalicAngle 0 /Ascent 0 /Descent 0 \
                     /CapHeight 0 /StemV 0 >>"
                        .to_owned(),
                ),
            ],
        ),
        Level::B,
    );
    assert_eq!(report.changes().get("font_embedded"), Some(&2));
    let resources = get(&doc, key(&page(&doc), b"Resources"));
    let fonts = get(&doc, key(&resources, b"Font"));
    for name in [&b"F1"[..], b"F2"] {
        let font = get(&doc, key(&fonts, name));
        assert!(
            font.as_dict().unwrap().contains_key(b"Encoding"),
            "{name:?}"
        );
        let descriptor = get(&doc, key(&font, b"FontDescriptor"));
        assert!(descriptor.as_dict().unwrap().contains_key(b"FontFile3"));
    }
}

/// A page showing `text` twice in Helvetica.
fn text_doc(text: &str) -> Vec<u8> {
    page_doc(
        &format!("BT /F1 12 Tf 10 100 Td ({text}) Tj 0 -14 Td ({text}) Tj ET"),
        "/Resources << /Font << /F1 5 0 R >> >>",
        &[(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        )],
    )
}

const FRENCH: &str = "Le contrat est conclu entre les parties pour une duree de un an, et il \
                      prend effet a la date de la signature par le client.";

#[test]
fn level_a_tags_the_document_and_tells_its_language() {
    let (doc, report) = convert_and_reopen(text_doc(FRENCH), Level::A);
    let root = catalog(&doc);
    let mark = get(&doc, key(&root, b"MarkInfo"));
    assert_eq!(key(&mark, b"Marked"), &Object::Bool(true));
    let tree = get(&doc, key(&root, b"StructTreeRoot"));
    assert_eq!(key(&tree, b"Type").as_name(), Some(&b"StructTreeRoot"[..]));
    let element = get(&doc, key(&tree, b"K"));
    assert_eq!(key(&element, b"S").as_name(), Some(&b"Document"[..]));
    // Level N1: the text is one paragraph, tied to the page's content by its identifier.
    let paragraphs = key(&element, b"K").as_array().unwrap();
    assert_eq!(paragraphs.len(), 1);
    let paragraph = get(&doc, &paragraphs[0]);
    assert_eq!(key(&paragraph, b"S").as_name(), Some(&b"P"[..]));
    assert_eq!(key(&paragraph, b"K"), &Object::Integer(0));
    let page = page(&doc);
    assert_eq!(key(&page, b"StructParents"), &Object::Integer(0));
    let parents = get(&doc, key(&tree, b"ParentTree"));
    let nums = key(&parents, b"Nums").as_array().unwrap();
    assert_eq!(nums[0], Object::Integer(0));
    assert_eq!(nums[1].as_array().unwrap(), [paragraphs[0].clone()]);
    let content = String::from_utf8(inflated(&doc, key(&page, b"Contents"))).unwrap();
    assert!(content.contains("/P <</MCID 0>> BDC"), "{content}");
    // French, without guessing the region.
    assert_eq!(key(&root, b"Lang").as_string(), Some(&b"fr"[..]));
    assert!(metadata(&doc).contains("<pdfaid:conformance>A</pdfaid:conformance>"));
    let changes = report.changes();
    assert_eq!(changes.get("marked"), Some(&1));
    assert_eq!(changes.get("structure_tree_added"), Some(&1));
    assert_eq!(changes.get("language_detected"), Some(&1));
}

#[test]
fn a_picture_described_in_the_content_gets_its_description_as_alternate_text() {
    let data = page_doc(
        "/Span <</Alt (Signature of A)>> BDC q 50 0 0 20 10 10 cm /Im1 Do Q EMC",
        "/Resources << /XObject << /Im1 5 0 R >> >>",
        &[(
            5,
            stream(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8",
                "A",
            ),
        )],
    );
    let (doc, _) = convert_and_reopen(data, Level::A);
    let tree = get(&doc, key(&catalog(&doc), b"StructTreeRoot"));
    let document = get(&doc, key(&tree, b"K"));
    let figure = get(&doc, &key(&document, b"K").as_array().unwrap()[0]);
    assert_eq!(key(&figure, b"S").as_name(), Some(&b"Figure"[..]));
    assert_eq!(
        key(&figure, b"Alt").as_string(),
        Some(&b"Signature of A"[..])
    );
    // The figure's sequence goes around the description's, not inside it.
    let content = String::from_utf8(inflated(&doc, key(&page(&doc), b"Contents"))).unwrap();
    let figure_at = content.find("/Figure <</MCID 0>> BDC").unwrap();
    assert!(figure_at < content.find("/Span").unwrap(), "{content}");
}

#[test]
fn content_drawn_after_a_document_was_tagged_is_tied_to_its_tree() {
    // A tagged page (its first line in a paragraph with identifier 0), and a second line drawn
    // after it was tagged, as a flattened stamp would be.
    let objects = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 6 0 R /MarkInfo << /Marked true >> >>"
                .to_owned(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_owned(),
        ),
        (
            4,
            stream(
                "",
                "BT /F1 12 Tf 10 150 Td /P <</MCID 0>> BDC (Tagged) Tj EMC ET \
                 BT /F1 12 Tf 10 50 Td (Stamped) Tj ET",
            ),
        ),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ),
        (
            6,
            "<< /Type /StructTreeRoot /K 7 0 R /ParentTree << /Nums [0 [8 0 R]] >> \
             /ParentTreeNextKey 1 >>"
                .to_owned(),
        ),
        (
            7,
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>".to_owned(),
        ),
        (
            8,
            "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K 0 >>".to_owned(),
        ),
    ];
    let (doc, report) = convert_and_reopen(build(&objects, ""), Level::A);
    assert_eq!(report.changes().get("structure_elements_added"), Some(&1));
    assert_eq!(report.changes().get("structure_tree_added"), None);
    let tree = get(&doc, key(&catalog(&doc), b"StructTreeRoot"));
    let document = get(&doc, key(&tree, b"K"));
    let kids = key(&document, b"K").as_array().unwrap();
    assert_eq!(kids.len(), 2);
    let added = get(&doc, &kids[1]);
    assert_eq!(key(&added, b"S").as_name(), Some(&b"P"[..]));
    assert_eq!(key(&added, b"K"), &Object::Integer(1));
    // The page's entry of the parent tree now leads identifier 1 to it.
    let parents = get(&doc, key(&tree, b"ParentTree"));
    let entry = key(&parents, b"Nums").as_array().unwrap()[1].clone();
    let entry = get(&doc, &entry);
    assert_eq!(entry.as_array().unwrap().len(), 2);
    assert_eq!(entry.as_array().unwrap()[1], kids[1]);
    let page = page(&doc);
    assert_eq!(key(&page, b"StructParents"), &Object::Integer(0));
    let content = String::from_utf8(inflated(&doc, key(&page, b"Contents"))).unwrap();
    assert!(
        content.contains("/P <</MCID 0>> BDC (Tagged) Tj EMC"),
        "{content}"
    );
    assert!(content.contains("/P <</MCID 1>> BDC"), "{content}");
}

#[test]
fn the_language_given_comes_after_the_document_own() {
    let mut doc = Document::open(text_doc("Signature"), &OpenOptions::default()).unwrap();
    let report = convert(&mut doc, &Options::new(Level::A).language("fr-CA")).unwrap();
    assert_eq!(
        key(&catalog(&doc), b"Lang").as_string(),
        Some(&b"fr-CA"[..])
    );
    assert_eq!(report.changes().get("language_given"), Some(&1));
    // A document that declares its language keeps it.
    let objects = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Lang (en-US) >>".to_owned(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_owned(),
        ),
    ];
    let mut doc = Document::open(build(&objects, ""), &OpenOptions::default()).unwrap();
    convert(&mut doc, &Options::new(Level::A).language("fr-CA")).unwrap();
    assert_eq!(
        key(&catalog(&doc), b"Lang").as_string(),
        Some(&b"en-US"[..])
    );
}

#[test]
fn a_language_tag_that_is_none_is_refused_before_anything_changes() {
    let data = text_doc(FRENCH);
    let mut doc = Document::open(data, &OpenOptions::default()).unwrap();
    let before = catalog(&doc);
    let refused = convert(&mut doc, &Options::new(Level::A).language("fr_CA"));
    assert!(matches!(
        refused,
        Err(sealwax_core::Error::InvalidArgument { .. })
    ));
    assert_eq!(catalog(&doc), before);
}

#[test]
fn a_tagged_document_keeps_its_tree_and_its_role_map_leads_to_standard_types() {
    // Custom1 leads to Custom2, which nothing maps; Loop1 and Loop2 map to each other; P, a
    // standard type, is mapped to Custom3; Orphan is not mapped at all, and its language is
    // no language tag.
    let objects =
        vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 5 0 R /MarkInfo << /Marked false >> \
             /Lang (en-US) >>"
                .to_owned(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_owned(),
        ),
        (4, stream("", "")),
        (
            5,
            "<< /Type /StructTreeRoot /K 6 0 R /RoleMap << /Custom1 /Custom2 /Loop1 /Loop2 \
             /Loop2 /Loop1 /P /Custom3 /Fine /Sect >> >>"
                .to_owned(),
        ),
        (
            6,
            "<< /Type /StructElem /S /Document /P 5 0 R /K [7 0 R 8 0 R \
             << /Type /StructElem /S /Orphan /P 6 0 R /Lang (fr_CA) >> \
             << /Type /StructElem /S /P /P 6 0 R >>] >>"
                .to_owned(),
        ),
        (7, "<< /Type /StructElem /S /Custom1 /P 6 0 R >>".to_owned()),
        (8, "<< /Type /StructElem /S /Loop1 /P 6 0 R /Lang (fr-CA) >>".to_owned()),
    ];
    let (doc, report) = convert_and_reopen(build(&objects, ""), Level::A);
    let root = catalog(&doc);
    assert_eq!(key(&root, b"Lang").as_string(), Some(&b"en-US"[..]));
    let mark = get(&doc, key(&root, b"MarkInfo"));
    assert_eq!(key(&mark, b"Marked"), &Object::Bool(true));
    let tree = get(&doc, key(&root, b"StructTreeRoot"));
    let roles = get(&doc, key(&tree, b"RoleMap"));
    let role = |name: &[u8]| roles.as_dict().unwrap().get(name).and_then(Object::as_name);
    assert_eq!(role(b"Custom1"), Some(&b"Custom2"[..]));
    assert_eq!(role(b"Custom2"), Some(&b"NonStruct"[..]));
    assert_eq!(role(b"Orphan"), Some(&b"NonStruct"[..]));
    assert_eq!(role(b"Fine"), Some(&b"Sect"[..]));
    assert_eq!(role(b"P"), None);
    // The loop is broken at one end.
    let ends = [role(b"Loop1"), role(b"Loop2")];
    assert!(ends.contains(&Some(&b"NonStruct"[..])), "{ends:?}");
    let changes = report.changes();
    assert_eq!(changes.get("role_map_standard_type_freed"), Some(&1));
    assert_eq!(changes.get("role_map_type_to_nonstruct"), Some(&3));
    assert_eq!(changes.get("language_malformed_removed"), Some(&1));
    assert!(!changes.contains_key("structure_tree_added"));
    // The element whose language was no tag lost it; the other kept its own.
    let document = get(&doc, key(&tree, b"K"));
    let kids = key(&document, b"K").as_array().unwrap().to_vec();
    assert!(!get(&doc, &kids[2]).as_dict().unwrap().contains_key(b"Lang"));
    assert_eq!(
        key(&get(&doc, &kids[1]), b"Lang").as_string(),
        Some(&b"fr-CA"[..])
    );
}

#[test]
fn widgets_get_the_appearances_pdfa_requires_where_nothing_is_drawn() {
    let widget = |more: &str| {
        format!("<< /Type /Annot /Subtype /Widget /Rect [10 10 110 30] /F 4 {more} >>")
    };
    let (doc, report) = convert_and_reopen(
        page_doc(
            "",
            "/Annots [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 16 0 R 17 0 R]",
            &[
                // An empty text field, its type and empty value on its parent.
                (5, widget("/Parent 13 0 R")),
                // A check box, off.
                (6, widget("/FT /Btn /V /Off")),
                // A text field with a value, one with a background: they need a drawing.
                (7, widget("/FT /Tx /V (x)")),
                (8, widget("/FT /Tx /MK << /BG [1 1 0] >>")),
                // A point needs no appearance.
                (
                    9,
                    "<< /Type /Annot /Subtype /Widget /Rect [5 5 5 5] /F 4 /FT /Tx >>".to_owned(),
                ),
                // Two push buttons sharing an appearance dictionary whose N is a stream.
                (10, widget("/FT /Btn /Ff 65536 /AP 14 0 R")),
                (11, widget("/FT /Btn /Ff 65536 /AP 14 0 R /AS /Down")),
                // A check box whose appearances are already states.
                (
                    12,
                    widget("/FT /Btn /AS /Off /AP << /N << /Off 15 0 R >> >>"),
                ),
                (13, "<< /FT /Tx /V () /Kids [5 0 R] >>".to_owned()),
                (14, "<< /N 15 0 R >>".to_owned()),
                (
                    15,
                    stream("/Type /XObject /Subtype /Form /BBox [0 0 100 20]", "0 g"),
                ),
                // A list shows its options; a push button its caption.
                (16, widget("/FT /Ch /Opt [(a) (b)]")),
                (17, widget("/FT /Btn /Ff 65536 /MK << /CA (OK) >>")),
            ],
        ),
        Level::B,
    );
    let annots = get(&doc, key(&page(&doc), b"Annots"));
    let annots: Vec<Object> = annots
        .as_array()
        .unwrap()
        .iter()
        .map(|a| get(&doc, a))
        .collect();
    let normal = |annot: &Object| get(&doc, key(&get(&doc, key(annot, b"AP")), b"N"));

    let Object::Stream(text) = normal(&annots[0]) else {
        panic!("a text field's appearance is a stream");
    };
    let bbox: Vec<f64> = text
        .dict
        .get(b"BBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(bbox, [0.0, 0.0, 100.0, 20.0]);
    let mut content = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::ZlibDecoder::new(text.data.as_slice()),
        &mut content,
    )
    .unwrap();
    assert_eq!(content, b"/Tx BMC\nEMC\n");

    let check = normal(&annots[1]);
    assert!(matches!(get(&doc, key(&check, b"Off")), Object::Stream(_)));
    assert_eq!(key(&annots[1], b"AS").as_name(), Some(&b"Off"[..]));

    for drawn in &annots[2..5] {
        assert!(!drawn.as_dict().unwrap().contains_key(b"AP"));
    }

    // Each push button gets its own dictionary, its stream as the state its /AS names.
    for (annot, state) in [(&annots[5], &b"On"[..]), (&annots[6], &b"Down"[..])] {
        assert_eq!(key(annot, b"AS").as_name(), Some(state));
        let states = normal(annot);
        assert!(matches!(get(&doc, key(&states, state)), Object::Stream(_)));
    }
    assert!(matches!(normal(&annots[7]), Object::Dict(_)));
    for drawn in &annots[8..] {
        assert!(!drawn.as_dict().unwrap().contains_key(b"AP"));
    }

    assert_eq!(report.changes().get("appearance_empty_added"), Some(&2));
    assert_eq!(report.changes().get("button_appearance_wrapped"), Some(&2));
    assert_eq!(
        report.residual().get("annotation_without_appearance"),
        Some(&4)
    );
}

#[test]
fn a_document_changed_since_it_was_opened_is_refused() {
    let mut doc = Document::open(
        page_doc("0 g 0 0 10 10 re f", "", &[]),
        &OpenOptions::default(),
    )
    .unwrap();
    assert!(!doc.is_modified());
    doc.add_object(Object::Null).unwrap();
    assert!(doc.is_modified());
    let error = convert(&mut doc, &Options::new(Level::B)).unwrap_err();
    assert!(
        matches!(error, sealwax_core::Error::Unsupported { .. }),
        "{error}"
    );
}

#[test]
fn a_state_name_too_long_is_shortened_wherever_it_names_the_state() {
    let long = "J".repeat(150);
    let (doc, report) = convert_and_reopen(
        page_doc(
            "",
            "/Annots [5 0 R]",
            &[
                (
                    5,
                    format!(
                        "<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] /F 4 /Parent 6 0 R \
                         /AS /{long} /AP << /N << /{long} 7 0 R /Off 7 0 R >> >> >>"
                    ),
                ),
                (
                    6,
                    format!("<< /FT /Btn /T (box) /V /{long} /Kids [5 0 R] >>"),
                ),
                (
                    7,
                    stream(
                        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                        "0 g 0 0 10 10 re f",
                    ),
                ),
            ],
        ),
        Level::B,
    );
    assert_eq!(report.changes().get("state_name_shortened"), Some(&1));
    assert_eq!(report.residual().get("name_too_long"), None);
    let annots = get(&doc, key(&page(&doc), b"Annots"));
    let widget = get(&doc, &annots.as_array().unwrap()[0]);
    let state = key(&widget, b"AS").as_name().unwrap().to_vec();
    assert!(state.len() <= 127 && state.starts_with(b"JJJJ"));
    let normal = get(&doc, key(&get(&doc, key(&widget, b"AP")), b"N"));
    assert!(normal.as_dict().unwrap().contains_key(&state));
    let field = get(&doc, key(&widget, b"Parent"));
    assert_eq!(key(&field, b"V").as_name(), Some(state.as_slice()));
}

/// An action whose `/Next` chain loops back on itself: the chain is walked once, without
/// recursion (an external review crashed the process with it), and judged by what it holds.
#[test]
fn a_looping_action_chain_is_walked_once() {
    let link = "<< /Type /Annot /Subtype /Link /Rect [0 0 20 20] /F 4 /A 6 0 R >>".to_owned();
    let looping = page_doc(
        "",
        "/Annots [5 0 R]",
        &[
            (5, link.clone()),
            (
                6,
                "<< /S /URI /URI (https://example.org) /Next 6 0 R >>".to_owned(),
            ),
        ],
    );
    let (doc, _) = convert_and_reopen(looping, Level::B);
    let annotation = get(&doc, &key(&page(&doc), b"Annots").as_array().unwrap()[0]);
    assert!(
        annotation.as_dict().unwrap().contains_key(b"A"),
        "an allowed chain stays"
    );

    let hiding = page_doc(
        "",
        "/Annots [5 0 R]",
        &[
            (5, link),
            (
                6,
                "<< /S /URI /URI (https://example.org) /Next [7 0 R 6 0 R] >>".to_owned(),
            ),
            (
                7,
                "<< /S /JavaScript /JS (app.alert(1)) /Next 6 0 R >>".to_owned(),
            ),
        ],
    );
    let (doc, _) = convert_and_reopen(hiding, Level::B);
    let annotation = get(&doc, &key(&page(&doc), b"Annots").as_array().unwrap()[0]);
    assert!(
        !annotation.as_dict().unwrap().contains_key(b"A"),
        "JavaScript further down the loop is found, and the action removed"
    );
}

/// Embedded files get what PDF/A-3 asks of them: `/F` and `/UF`, an `/AFRelationship`, an `/AF`
/// association, and their MIME type (external review: a file attached without them stayed so).
#[test]
fn embedded_files_are_described_and_associated() {
    let objects =
        vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(note.txt) 5 0 R \
             (invoice.xml) << /F (invoice.xml) /EF << /F 7 0 R >> >>] >> >> >>"
                .to_owned(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /Annots [8 0 R] >>"
                .to_owned(),
        ),
        (4, stream("", "")),
        (5, "<< /Type /Filespec /F (note.txt) /EF << /F 6 0 R >> >>".to_owned()),
        (6, stream("/Type /EmbeddedFile", "hello")),
        (7, stream("/Type /EmbeddedFile /Subtype /text", "<a/>")),
        (
            8,
            "<< /Type /Annot /Subtype /FileAttachment /Rect [10 10 30 30] /F 4 \
             /AP << /N 10 0 R >> /FS << /UF <FEFF00E90064> /EF << /F 9 0 R >> >> >>"
                .to_owned(),
        ),
        (9, stream("/Type /EmbeddedFile", "data")),
        (
            10,
            stream("/Type /XObject /Subtype /Form /BBox [0 0 20 20]", ""),
        ),
    ];
    let (doc, report) = convert_and_reopen(build(&objects, ""), Level::B);
    assert_eq!(report.changes().get("embedded_file_described"), Some(&3));
    assert_eq!(report.changes().get("embedded_file_associated"), Some(&3));
    let catalog = catalog(&doc);
    let associated = key(&catalog, b"AF").as_array().unwrap();
    assert_eq!(associated.len(), 2);
    let annotation = get(&doc, &key(&page(&doc), b"Annots").as_array().unwrap()[0]);
    let attached = key(&annotation, b"FS").clone();
    assert!(matches!(attached, Object::Ref(_)), "made indirect");
    assert_eq!(
        key(&annotation, b"AF").as_array().unwrap(),
        std::slice::from_ref(&attached)
    );
    let expected = [
        ("note.txt", &b"note.txt"[..], &b"text/plain"[..]),
        ("invoice.xml", b"invoice.xml", b"text/xml"),
        ("_d", b"\xFE\xFF\x00\xE9\x00d", b"application/octet-stream"),
    ];
    for (spec, (f, uf, mime)) in associated.iter().chain([&attached]).zip(expected) {
        let spec = get(&doc, spec);
        assert_eq!(key(&spec, b"F").as_string(), Some(f.as_bytes()), "{f}");
        assert_eq!(key(&spec, b"UF").as_string(), Some(uf), "{f}");
        assert_eq!(
            key(&spec, b"AFRelationship").as_name(),
            Some(&b"Unspecified"[..]),
            "{f}"
        );
        assert_eq!(key(&spec, b"Type").as_name(), Some(&b"Filespec"[..]), "{f}");
        let file = get(&doc, key(key(&spec, b"EF"), b"F"));
        assert_eq!(key(&file, b"Subtype").as_name(), Some(mime), "{f}");
    }
}

/// Files already associated through `/AF` get what PDF/A-3 asks of them too, and keep their
/// association; one written in place where nothing can refer to it is reported (second review:
/// a file associated with the catalog alone was left as it was, the report saying nothing).
#[test]
fn files_associated_through_af_are_described() {
    let objects =
        vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /AF [5 0 R] >>".to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /AF [<< /F (page.csv) /EF << /F 7 0 R >> >>] /Annots [9 0 R] >>"
                .to_owned(),
        ),
        (4, stream("", "")),
        (5, "<< /Type /Filespec /F (note.txt) /EF << /F 6 0 R >> >>".to_owned()),
        (6, stream("/Type /EmbeddedFile", "hello")),
        (7, stream("/Type /EmbeddedFile", "a,b")),
        (8, stream("/Type /EmbeddedFile", "%PDF")),
        (
            9,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 0 0] /F 4 /A << /S /GoToE /NewWindow false \
             /T << /R /C /N (x) >> /F << /F (lost.pdf) /EF << /F 8 0 R >> >> >> >>"
                .to_owned(),
        ),
    ];
    let (doc, report) = convert_and_reopen(build(&objects, ""), Level::B);
    assert_eq!(report.changes().get("embedded_file_described"), Some(&2));
    assert_eq!(
        report.changes().get("embedded_file_associated"),
        None,
        "already associated"
    );
    assert_eq!(
        report.residual().get("embedded_file_not_associated"),
        Some(&1)
    );
    let catalog = catalog(&doc);
    assert_eq!(key(&catalog, b"AF").as_array().unwrap().len(), 1);
    let page = page(&doc);
    let on_page = key(&page, b"AF").as_array().unwrap();
    assert!(matches!(on_page[0], Object::Ref(_)), "made indirect");
    for (spec, uf, mime) in [
        (
            &key(&catalog, b"AF").as_array().unwrap()[0],
            &b"note.txt"[..],
            &b"text/plain"[..],
        ),
        (&on_page[0], b"page.csv", b"text/csv"),
    ] {
        let spec = get(&doc, spec);
        assert_eq!(key(&spec, b"UF").as_string(), Some(uf));
        assert_eq!(
            key(&spec, b"AFRelationship").as_name(),
            Some(&b"Unspecified"[..])
        );
        let file = get(&doc, key(key(&spec, b"EF"), b"F"));
        assert_eq!(key(&file, b"Subtype").as_name(), Some(mime));
    }
}
