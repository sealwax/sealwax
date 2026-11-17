//! Form fields created on a page: placed where the page shows them, filled and read back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use sealwax_annots::{
    Error, FieldKind, FillValue, NewField, NewFieldKind, add_field, field_values, fields,
    fill_fields,
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
