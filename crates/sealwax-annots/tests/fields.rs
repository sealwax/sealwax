//! The fields of a form: read as their widgets place them, filled, read back, and flattened alone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use sealwax_annots::{
    FieldChoice, FieldKind, FieldValue, FillValue, field_values, fields, fill_fields,
    flatten_fields,
};
use sealwax_core::{Document, Object, OpenOptions, SaveOptions};

fn build(objects: &[(u32, String)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in objects {
        offsets.push((*number, out.len()));
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|(n, _)| *n).max().unwrap() + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for number in 1..size {
        match offsets.iter().find(|(n, _)| *n == number) {
            Some((_, offset)) => {
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
            }
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
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

/// Two pages (crop box of page 1 offset). Fields:
/// - `name`, a text field (required, `MaxLen` 12, `TU`), a widget on each page;
/// - `agree`, a check box with `/Yes` and `/Off`;
/// - `lang`, radio buttons whose states are `/en` and `/Fran#E7ais` (a Latin-1 byte);
/// - `country`, a combo box of export/display pairs;
/// - `bare`, a check box without appearance;
/// - `sign`, a signature field; `go`, a push button with a caption.
///
/// Page 1 also holds a link and a text annotation (a comment).
fn form() -> Document {
    let on = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        "0 0 10 10 re f",
    );
    let objects = vec![
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 20 0 R 30 0 R 40 0 R \
             50 0 R 60 0 R 70 0 R] /DA (/Helv 0 Tf 0 g) >> >>"
                .to_owned(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [50 70 550 770] \
             /Contents 5 0 R /Annots [11 0 R 20 0 R 31 0 R 32 0 R 40 0 R 50 0 R 60 0 R 70 0 R \
             80 0 R 81 0 R] >>"
                .to_owned(),
        ),
        (
            4,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 6 0 R /Annots [12 0 R] >>"
                .to_owned(),
        ),
        (5, stream("", "BT /F1 12 Tf 60 700 Td (Original) Tj ET")),
        (6, stream("", "")),
        // The text field and its two widgets.
        (
            10,
            "<< /FT /Tx /T (name) /TU (Your name) /Ff 2 /MaxLen 12 /Kids [11 0 R 12 0 R] >>"
                .to_owned(),
        ),
        (
            11,
            "<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [100 400 200 420] /F 4 /P 3 0 R >>"
                .to_owned(),
        ),
        (
            12,
            "<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [100 400 200 420] /F 4 /P 4 0 R >>"
                .to_owned(),
        ),
        // The check box.
        (
            20,
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [100 300 112 312] /F 4 \
             /AS /Off /AP << /N << /Yes 90 0 R /Off 91 0 R >> >> >>"
                .to_owned(),
        ),
        // The radio buttons.
        (
            30,
            "<< /FT /Btn /Ff 32768 /T (lang) /Kids [31 0 R 32 0 R] /V /Off >>".to_owned(),
        ),
        (
            31,
            "<< /Type /Annot /Subtype /Widget /Parent 30 0 R /Rect [100 250 112 262] /F 4 /AS /Off \
             /AP << /N << /en 90 0 R /Off 91 0 R >> >> /MK << /CA (l) >> >>"
                .to_owned(),
        ),
        (
            32,
            "<< /Type /Annot /Subtype /Widget /Parent 30 0 R /Rect [120 250 132 262] /F 4 /AS /Off \
             /AP << /N << /Fran#E7ais 90 0 R /Off 91 0 R >> >> >>"
                .to_owned(),
        ),
        // The combo box.
        (
            40,
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (country) \
             /Opt [[(CA) (Canada)] [(FR) (France)]] /Rect [100 200 250 220] /F 4 >>"
                .to_owned(),
        ),
        // A check box without appearance.
        (
            50,
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (bare) /Rect [300 300 312 312] /F 4 >>"
                .to_owned(),
        ),
        (
            60,
            "<< /Type /Annot /Subtype /Widget /FT /Sig /T (sign) /Rect [300 100 400 150] /F 4 >>"
                .to_owned(),
        ),
        (
            70,
            "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (go) /Rect [300 50 350 70] /F 4 \
             /MK << /CA (Send) >> >>"
                .to_owned(),
        ),
        (
            80,
            "<< /Type /Annot /Subtype /Link /Rect [60 600 200 620] /A << /S /URI /URI (https://example.org) >> >>"
                .to_owned(),
        ),
        (
            81,
            "<< /Type /Annot /Subtype /Text /Rect [400 600 420 620] /Contents (A comment) /F 4 >>"
                .to_owned(),
        ),
        (90, on.clone()),
        (91, stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", "")),
    ];
    Document::open(build(&objects), &OpenOptions::default()).unwrap()
}

fn reopened(doc: &Document) -> Document {
    Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap()
}

#[test]
fn every_widget_is_an_occurrence_with_what_its_field_says() {
    let doc = form();
    let all = fields(&doc, None).unwrap();
    let names: Vec<(&str, usize)> = all
        .iter()
        .map(|f| (f.name.as_str(), f.page_index))
        .collect();
    assert_eq!(
        names,
        [
            ("name", 0),
            ("agree", 0),
            ("lang", 0),
            ("lang", 0),
            ("country", 0),
            ("bare", 0),
            ("sign", 0),
            ("go", 0),
            ("name", 1)
        ]
    );
    let name = &all[0];
    assert_eq!(name.kind, FieldKind::Text);
    assert_eq!(name.label.as_deref(), Some("Your name"));
    assert!(name.required && !name.read_only);
    assert_eq!(name.max_length, Some(12));
    assert_eq!(
        (name.rect.x1, name.rect.y1, name.rect.x2, name.rect.y2),
        (100.0, 400.0, 200.0, 420.0)
    );
    assert_eq!((name.crop_box.x1, name.crop_box.y1), (50.0, 70.0));
    let agree = &all[1];
    assert_eq!(agree.kind, FieldKind::CheckBox);
    assert_eq!(agree.button_states, [b"Off".to_vec(), b"Yes".to_vec()]);
    assert_eq!(agree.state.as_deref(), Some(&b"Off"[..]));
    let french = &all[3];
    assert_eq!(french.kind, FieldKind::Radio);
    assert_eq!(french.button_states[0], b"Fran\xe7ais");
    assert_eq!(all[2].button_caption.as_deref(), Some("l"));
    let country = &all[4];
    assert_eq!(country.kind, FieldKind::ComboBox);
    assert_eq!(
        country.choices,
        [
            FieldChoice::Pair {
                export: "CA".into(),
                display: "Canada".into()
            },
            FieldChoice::Pair {
                export: "FR".into(),
                display: "France".into()
            },
        ]
    );
    assert!(all[5].button_states.is_empty(), "no appearance, no states");
    assert_eq!(all[6].kind, FieldKind::Signature);
    assert_eq!(
        (all[7].kind, all[7].button_caption.as_deref()),
        (FieldKind::PushButton, Some("Send"))
    );
    assert_eq!(fields(&doc, Some(1)).unwrap().len(), 1);
    assert!(fields(&doc, Some(2)).is_err());
}

#[test]
fn values_written_are_read_back_and_button_names_keep_their_bytes() {
    let mut doc = form();
    let values: HashMap<String, FillValue> = [
        ("name", FillValue::Text("Élodie".into())),
        ("agree", FillValue::Text("Yes".into())),
        ("lang", FillValue::Bytes(b"Fran\xe7ais".to_vec())),
        ("country", FillValue::Text("FR".into())),
        ("bare", FillValue::Text("Yes".into())),
        ("sign", FillValue::Text("ignored".into())),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let filled = fill_fields(&mut doc, &values).unwrap();
    // name twice, agree, lang twice, country, bare; never the signature.
    assert_eq!(filled.occurrences, 7);
    let plain = doc
        .save(&SaveOptions::default().xref(sealwax_core::XrefFormat::Table))
        .unwrap();
    assert!(
        plain
            .windows(b"/Fran#E7ais".len())
            .any(|w| w == b"/Fran#E7ais"),
        "the state's name is written with its bytes, escaped the same way"
    );
    let saved = doc.save(&SaveOptions::default()).unwrap();
    let doc = Document::open(saved, &OpenOptions::default()).unwrap();
    let values: HashMap<String, Vec<String>> = field_values(&doc).unwrap().into_iter().collect();
    assert_eq!(values["name"], ["Élodie", "Élodie"]);
    assert_eq!(values["agree"], ["Yes"]);
    assert_eq!(values["lang"], ["Off", "Français"]);
    assert_eq!(values["country"], ["FR"]);
    // No appearance, so no state of that name: the widget shows Off.
    assert_eq!(values["bare"], ["Off"]);
    let read = fields(&doc, None).unwrap();
    assert_eq!(read[0].value, Some(FieldValue::Text("Élodie".into())));
    assert_eq!(read[3].state.as_deref(), Some(&b"Fran\xe7ais"[..]));
    assert_eq!(
        read[2].value,
        Some(FieldValue::Name(b"Fran\xe7ais".to_vec()))
    );
    // The text field's widgets got an appearance showing the value.
    let widget = doc.object(sealwax_core::ObjId::new(11)).unwrap().unwrap();
    assert!(widget.as_dict().unwrap().contains_key(b"AP"));

    // An empty value clears the text.
    let mut doc = doc;
    let clear: HashMap<String, FillValue> =
        [("name".to_owned(), FillValue::Text(String::new()))].into();
    assert_eq!(fill_fields(&mut doc, &clear).unwrap().occurrences, 2);
    let values: HashMap<String, Vec<String>> =
        field_values(&reopened(&doc)).unwrap().into_iter().collect();
    assert_eq!(values["name"], ["", ""]);
}

#[test]
fn characters_winansi_does_not_have_are_counted_never_lost() {
    let mut doc = form();
    let values: HashMap<String, FillValue> =
        [("name".to_owned(), FillValue::Text("Ωmega".into()))].into();
    let filled = fill_fields(&mut doc, &values).unwrap();
    assert_eq!(filled.characters_replaced, 2, "one per widget");
    let values: HashMap<String, Vec<String>> =
        field_values(&reopened(&doc)).unwrap().into_iter().collect();
    assert_eq!(values["name"], ["Ωmega", "Ωmega"], "the value keeps them");
}

#[test]
fn flattening_the_fields_alone_keeps_links_and_comments() {
    let mut doc = form();
    let values: HashMap<String, FillValue> =
        [("name".to_owned(), FillValue::Text("Alice".into()))].into();
    fill_fields(&mut doc, &values).unwrap();
    let report = flatten_fields(&mut doc).unwrap();
    assert!(report.painted >= 2, "{report:?}");
    let doc = reopened(&doc);
    assert!(fields(&doc, None).unwrap().is_empty());
    let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
    assert!(!catalog.as_dict().unwrap().contains_key(b"AcroForm"));
    let page = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
    let annots = match page.as_dict().unwrap().get(b"Annots").unwrap() {
        Object::Array(items) => items.clone(),
        other => panic!("{other:?}"),
    };
    let subtypes: Vec<Vec<u8>> = annots
        .iter()
        .map(|a| {
            let a = match a {
                Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
                other => other.clone(),
            };
            a.as_dict()
                .unwrap()
                .get(b"Subtype")
                .unwrap()
                .as_name()
                .unwrap()
                .to_vec()
        })
        .collect();
    assert_eq!(subtypes, [b"Link".to_vec(), b"Text".to_vec()]);
    // The value is drawn by a form the page now paints.
    let resources = match page.as_dict().unwrap().get(b"Resources").unwrap() {
        Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
        other => other.clone(),
    };
    let xobjects = match resources.as_dict().unwrap().get(b"XObject").unwrap() {
        Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
        other => other.clone(),
    };
    let drawn = xobjects.as_dict().unwrap().iter().any(|(_, value)| {
        let form = doc.object(value.as_ref_id().unwrap()).unwrap().unwrap();
        decoded(&form).windows(7).any(|w| w == b"(Alice)")
    });
    assert!(drawn, "the filled value is painted into the page");
}

fn decoded(object: &Object) -> Vec<u8> {
    use std::io::Read as _;
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

/// Reading and filling stay within the document's limits.
#[test]
fn fields_hold_to_the_documents_limits() {
    let data = form().save(&SaveOptions::default()).unwrap();
    let mut limits = sealwax_core::Limits::default();
    limits.max_annotations = 3;
    let mut doc = Document::open(data.clone(), &OpenOptions::default().limits(limits)).unwrap();
    assert!(matches!(
        fields(&doc, None),
        Err(sealwax_core::Error::LimitExceeded {
            what: "max_annotations"
        })
    ));
    assert!(fill_fields(&mut doc, &HashMap::new()).is_err());
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
    let mut limits = sealwax_core::Limits::default();
    limits.deadline = Some(deadline);
    let doc = Document::open(data, &OpenOptions::default().limits(limits)).unwrap();
    while std::time::Instant::now() <= deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(matches!(
        fields(&doc, None),
        Err(sealwax_core::Error::Timeout)
    ));
}
