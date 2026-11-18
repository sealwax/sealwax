//! Form fields created on a page: placed where the page shows them, filled and read back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use sealwax_annots::{
    Error, FieldChoice, FieldKind, FillValue, NewField, NewFieldKind, add_field, field_values,
    fields, fill_fields, remove_field_appearances, rename_button_state, set_field_label,
};
use sealwax_core::{Document, Object, OpenOptions, SaveOptions};

fn build(objects: &[String]) -> Vec<u8> {
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

/// One page, 600 x 800, turned `rotate` degrees, with `catalog` in its catalog.
fn flat(rotate: u16, catalog: &str, extra: &[String]) -> Document {
    let mut objects = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Rotate {rotate} >>"),
    ];
    objects.extend_from_slice(extra);
    Document::open(build(&objects), &OpenOptions::default()).unwrap()
}

fn reopened(doc: &Document) -> Document {
    Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap()
}

fn values(doc: &Document) -> HashMap<String, Vec<String>> {
    field_values(&reopened(doc)).unwrap().into_iter().collect()
}

#[test]
fn a_text_field_is_created_where_the_page_shows_it_and_fills() {
    let mut doc = flat(0, "", &[]);
    let field =
        NewField::new("name", NewFieldKind::Text, 0, [100.0, 100.0, 280.0, 120.0]).required(true);
    add_field(&mut doc, &field).unwrap();
    let doc = reopened(&doc);
    let created = fields(&doc, None).unwrap();
    assert_eq!(created.len(), 1);
    let field = &created[0];
    assert_eq!((field.name.as_str(), field.kind), ("name", FieldKind::Text));
    assert!(field.required);
    // From the top-left corner, y down: 800 - 120 = 680 to 800 - 100 = 700.
    assert_eq!(
        (field.rect.x1, field.rect.y1, field.rect.x2, field.rect.y2),
        (100.0, 680.0, 280.0, 700.0)
    );
    let mut doc = doc;
    let filled = HashMap::from([("name".to_owned(), FillValue::Text("Zoé".into()))]);
    assert_eq!(fill_fields(&mut doc, &filled).unwrap().occurrences, 1);
    assert_eq!(values(&doc)["name"], ["Zoé"]);
}

#[test]
fn a_date_carries_its_actions_and_a_check_box_its_states() {
    let mut doc = flat(0, "", &[]);
    add_field(
        &mut doc,
        &NewField::new("when", NewFieldKind::Date, 0, [10.0, 10.0, 100.0, 30.0]),
    )
    .unwrap();
    let widget = add_field(
        &mut doc,
        &NewField::new("agree", NewFieldKind::CheckBox, 0, [10.0, 50.0, 24.0, 64.0]),
    )
    .unwrap();
    let saved = doc
        .save(&SaveOptions::default().xref(sealwax_core::XrefFormat::Table))
        .unwrap();
    assert!(
        saved
            .windows(b"AFDate_FormatEx".len())
            .any(|w| w == b"AFDate_FormatEx")
    );
    let found = fields(&reopened(&doc), None).unwrap();
    let agree = found.iter().find(|f| f.name == "agree").unwrap();
    assert_eq!(agree.kind, FieldKind::CheckBox);
    assert_eq!(agree.button_states, [b"Off".to_vec(), b"Yes".to_vec()]);
    let checked = HashMap::from([("agree".to_owned(), FillValue::Text("Yes".into()))]);
    assert!(fill_fields(&mut doc, &checked).unwrap().refused.is_empty());
    assert_eq!(values(&doc)["agree"], ["Yes"]);
    assert!(matches!(doc.object(widget).unwrap(), Some(Object::Dict(_))));
}

#[test]
fn a_name_taken_is_refused_and_the_form_left_as_it_was() {
    let mut doc = flat(
        0,
        "/AcroForm << /Fields [4 0 R] >>",
        &["<< /FT /Tx /T (client) /V (Jane) /Kids [5 0 R] >>".to_owned()],
    );
    // A field `client` with a child: `client` is taken, and so is a name that would sit over it.
    let refused = add_field(
        &mut doc,
        &NewField::new("client", NewFieldKind::Text, 0, [10.0, 10.0, 100.0, 30.0]),
    );
    assert!(matches!(
        refused,
        Err(Error::Pdf(sealwax_core::Error::FieldNameTaken))
    ));
    add_field(
        &mut doc,
        &NewField::new("spouse", NewFieldKind::Text, 0, [10.0, 40.0, 100.0, 60.0]),
    )
    .unwrap();
    let doc = reopened(&doc);
    let form = doc.object(doc.catalog_id()).unwrap().unwrap();
    let fields_list = form
        .as_dict()
        .unwrap()
        .get(b"AcroForm")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Fields")
        .unwrap()
        .as_array()
        .unwrap()
        .len();
    assert_eq!(
        fields_list, 2,
        "the field there stays, the new one joins it"
    );
    for (name, rect) in [
        ("", [10.0, 10.0, 20.0, 20.0]),
        ("a.b", [10.0, 10.0, 20.0, 20.0]),
        ("flat", [10.0, 10.0, 10.0, 20.0]),
    ] {
        let mut doc = flat(0, "", &[]);
        assert!(matches!(
            add_field(&mut doc, &NewField::new(name, NewFieldKind::Text, 0, rect)),
            Err(Error::Pdf(sealwax_core::Error::InvalidArgument { .. }))
        ));
    }
    let mut doc = flat(0, "", &[]);
    assert!(
        add_field(
            &mut doc,
            &NewField::new("x", NewFieldKind::Text, 1, [0.0, 0.0, 10.0, 10.0])
        )
        .is_err()
    );
}

#[test]
fn on_a_turned_page_the_rectangle_is_where_the_page_shows_it() {
    // Shown 800 wide and 600 high: (u, v) from the shown top-left corner is (v, u) in user space
    // for a quarter turn.
    for (rotate, user) in [
        (90, (50.0, 100.0, 70.0, 280.0)),
        (180, (320.0, 50.0, 500.0, 70.0)),
        (270, (530.0, 520.0, 550.0, 700.0)),
    ] {
        let mut doc = flat(rotate, "", &[]);
        add_field(
            &mut doc,
            &NewField::new("name", NewFieldKind::Text, 0, [100.0, 50.0, 280.0, 70.0]),
        )
        .unwrap();
        let field = &fields(&reopened(&doc), None).unwrap()[0];
        assert_eq!(
            (field.rect.x1, field.rect.y1, field.rect.x2, field.rect.y2),
            user,
            "{rotate}"
        );
    }
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

/// The widget's `/DA` and its normal appearance's drawing.
fn appearance(doc: &Document, widget: sealwax_core::ObjId) -> (Vec<u8>, String) {
    let Some(Object::Dict(dict)) = doc.object(widget).unwrap() else {
        panic!("no widget");
    };
    let Some(Object::String(da)) = dict.get(b"DA") else {
        panic!("no /DA");
    };
    let normal = dict
        .get(b"AP")
        .and_then(Object::as_dict)
        .and_then(|ap| ap.get(b"N"))
        .and_then(Object::as_ref_id)
        .unwrap();
    let drawing = decoded(&doc.object(normal).unwrap().unwrap());
    (da.clone(), String::from_utf8_lossy(&drawing).into_owned())
}

#[test]
fn a_font_size_given_is_the_fields_and_its_values_drawing() {
    let mut doc = flat(0, "", &[]);
    let fixed = add_field(
        &mut doc,
        &NewField::new("name", NewFieldKind::Text, 0, [10.0, 10.0, 200.0, 30.0])
            .font_size(Some(12.0)),
    )
    .unwrap();
    let date = add_field(
        &mut doc,
        &NewField::new("when", NewFieldKind::Date, 0, [10.0, 40.0, 200.0, 60.0])
            .font_size(Some(10.5)),
    )
    .unwrap();
    let fitted = add_field(
        &mut doc,
        &NewField::new("notes", NewFieldKind::Text, 0, [10.0, 70.0, 200.0, 90.0]),
    )
    .unwrap();
    let filled = HashMap::from([
        ("name".to_owned(), FillValue::Text("Zoé".into())),
        ("when".to_owned(), FillValue::Text("2026-11-17".into())),
        ("notes".to_owned(), FillValue::Text("Rien".into())),
    ]);
    assert_eq!(fill_fields(&mut doc, &filled).unwrap().occurrences, 3);
    let (da, drawing) = appearance(&doc, fixed);
    assert_eq!(da, b"/Helv 12 Tf 0 g");
    assert!(drawing.contains("/Helv 12 Tf"), "{drawing}");
    let (da, drawing) = appearance(&doc, date);
    assert_eq!(da, b"/Helv 10.5 Tf 0 g");
    assert!(drawing.contains("/Helv 10.5 Tf"), "{drawing}");
    // Without a size, it is fitted: 0.85 of the field's 20 points.
    let (da, drawing) = appearance(&doc, fitted);
    assert_eq!(da, b"/Helv 0 Tf 0 g");
    assert!(drawing.contains("/Helv 17 Tf"), "{drawing}");
    // A check box's mark stays fitted to its box.
    let mut doc = flat(0, "", &[]);
    let check = add_field(
        &mut doc,
        &NewField::new("agree", NewFieldKind::CheckBox, 0, [10.0, 50.0, 24.0, 64.0])
            .font_size(Some(12.0)),
    )
    .unwrap();
    let Some(Object::Dict(dict)) = doc.object(check).unwrap() else {
        panic!("no widget");
    };
    assert_eq!(
        dict.get(b"DA"),
        Some(&Object::String(b"/ZaDb 0 Tf 0 g".to_vec()))
    );
}

#[test]
fn a_font_size_out_of_range_is_refused_and_nothing_added() {
    for size in [0.0, -12.0, 0.5, 1000.5, f64::NAN, f64::INFINITY] {
        let mut doc = flat(0, "", &[]);
        let before = doc.save(&SaveOptions::default()).unwrap();
        assert!(
            matches!(
                add_field(
                    &mut doc,
                    &NewField::new("x", NewFieldKind::Text, 0, [10.0, 10.0, 100.0, 30.0])
                        .font_size(Some(size)),
                ),
                Err(Error::Pdf(sealwax_core::Error::InvalidArgument {
                    what: "font size"
                }))
            ),
            "{size}"
        );
        assert_eq!(doc.save(&SaveOptions::default()).unwrap(), before, "{size}");
    }
    for size in [
        *sealwax_annots::FIELD_FONT_SIZES.start(),
        *sealwax_annots::FIELD_FONT_SIZES.end(),
    ] {
        let mut doc = flat(0, "", &[]);
        add_field(
            &mut doc,
            &NewField::new("x", NewFieldKind::Text, 0, [10.0, 10.0, 100.0, 30.0])
                .font_size(Some(size)),
        )
        .unwrap();
    }
}

#[test]
fn combo_boxes_signatures_labels_and_lengths_read_back() {
    let mut doc = flat(0, "", &[]);
    let field = |name: &str, kind| NewField::new(name, kind, 0, [80.0, 100.0, 300.0, 120.0]);
    add_field(
        &mut doc,
        &field("country", NewFieldKind::ComboBox)
            .choices(vec![
                ("fr".to_owned(), "France".to_owned()),
                ("Other".to_owned(), "Other".to_owned()),
            ])
            .label(Some("Country".to_owned())),
    )
    .unwrap();
    add_field(&mut doc, &field("sign", NewFieldKind::Signature)).unwrap();
    add_field(
        &mut doc,
        &field("code", NewFieldKind::Text).max_length(Some(5)),
    )
    .unwrap();
    let doc = reopened(&doc);
    let read = fields(&doc, None).unwrap();
    let by_name = |name: &str| read.iter().find(|f| f.name == name).unwrap();
    let country = by_name("country");
    assert_eq!(country.kind, FieldKind::ComboBox);
    assert_eq!(country.label.as_deref(), Some("Country"));
    assert_eq!(
        country.choices,
        [
            FieldChoice::Pair {
                export: "fr".to_owned(),
                display: "France".to_owned()
            },
            FieldChoice::Single("Other".to_owned()),
        ]
    );
    // None chosen.
    assert_eq!(country.value, None);
    assert_eq!(by_name("sign").kind, FieldKind::Signature);
    assert_eq!(by_name("code").max_length, Some(5));
}

#[test]
fn choices_and_lengths_where_they_do_not_belong_are_refused() {
    let mut doc = flat(0, "", &[]);
    let field = |kind| NewField::new("f", kind, 0, [80.0, 100.0, 300.0, 120.0]);
    let choices = vec![("a".to_owned(), "a".to_owned())];
    for refused in [
        field(NewFieldKind::ComboBox),
        field(NewFieldKind::Text).choices(choices.clone()),
        field(NewFieldKind::Text).max_length(Some(0)),
        field(NewFieldKind::CheckBox).max_length(Some(3)),
        field(NewFieldKind::ComboBox)
            .choices(choices.clone())
            .max_length(Some(3)),
    ] {
        assert!(
            matches!(
                add_field(&mut doc, &refused),
                Err(Error::Pdf(sealwax_core::Error::InvalidArgument { .. }))
            ),
            "{refused:?}"
        );
    }
    assert!(!doc.is_modified());
}

#[test]
fn a_label_changes_a_check_box_state_is_renamed_and_appearances_go() {
    let mut doc = flat(0, "", &[]);
    let field = |name: &str, kind| NewField::new(name, kind, 0, [80.0, 100.0, 95.0, 115.0]);
    add_field(&mut doc, &field("agree", NewFieldKind::CheckBox)).unwrap();
    add_field(
        &mut doc,
        &field("name", NewFieldKind::Text).label(Some("Name".to_owned())),
    )
    .unwrap();
    fill_fields(
        &mut doc,
        &HashMap::from([("agree".to_owned(), FillValue::Text("Yes".to_owned()))]),
    )
    .unwrap();
    set_field_label(&mut doc, "name", Some("Full name")).unwrap();
    rename_button_state(&mut doc, "agree", b"Fran\xE7ais").unwrap();
    assert_eq!(remove_field_appearances(&mut doc, "name").unwrap(), 1);
    let doc = reopened(&doc);
    let read = fields(&doc, None).unwrap();
    let agree = read.iter().find(|f| f.name == "agree").unwrap();
    assert_eq!(
        agree.button_states,
        [b"Fran\xE7ais".to_vec(), b"Off".to_vec()]
    );
    // Checked, it stays checked under the state's new name.
    assert_eq!(agree.state.as_deref(), Some(b"Fran\xE7ais".as_slice()));
    assert_eq!(
        agree.value,
        Some(sealwax_annots::FieldValue::Name(b"Fran\xE7ais".to_vec()))
    );
    let name = read.iter().find(|f| f.name == "name").unwrap();
    assert_eq!(name.label.as_deref(), Some("Full name"));
    let saved = doc
        .save(&SaveOptions::default().xref(sealwax_core::XrefFormat::Table))
        .unwrap();
    assert!(saved.windows(11).any(|w| w == b"/Fran#E7ais"));
}

#[test]
fn retouches_of_a_field_the_form_lacks_are_refused() {
    let mut doc = flat(0, "", &[]);
    add_field(
        &mut doc,
        &NewField::new("name", NewFieldKind::Text, 0, [80.0, 100.0, 300.0, 120.0]),
    )
    .unwrap();
    assert!(set_field_label(&mut doc, "nobody", Some("x")).is_err());
    assert!(remove_field_appearances(&mut doc, "nobody").is_err());
    // A text field has no state to rename; Off is no name to give.
    assert!(rename_button_state(&mut doc, "name", b"On").is_err());
    add_field(
        &mut doc,
        &NewField::new("box", NewFieldKind::CheckBox, 0, [10.0, 10.0, 20.0, 20.0]),
    )
    .unwrap();
    assert!(rename_button_state(&mut doc, "box", b"Off").is_err());
    assert!(rename_button_state(&mut doc, "box", b"").is_err());
}
