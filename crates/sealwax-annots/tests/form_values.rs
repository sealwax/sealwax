//! Values written into a form, what each field held before, and the fields refused or kept: what
//! an editor needs to fill a form and to undo it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashMap};

use sealwax_annots::{
    FillOptions, FillValue, Written, field_values, fill_fields_with, unfill_fields,
};
use sealwax_core::{Document, OpenOptions, SaveOptions};

fn build(objects: &[&str]) -> Vec<u8> {
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

/// Two text fields (`PRENOM` holding `Ancien`, at most `max` characters), a check box, a radio
/// group of two, a combo box offering two options.
fn form(max: Option<u32>) -> Document {
    let max = max.map(|max| format!("/MaxLen {max}")).unwrap_or_default();
    let prenom = format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (PRENOM) /V (Ancien) {max} /Rect [50 660 250 680] /F 4 /P 3 0 R >>"
    );
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 6 0 R 7 0 R 10 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [4 0 R 5 0 R 6 0 R 8 0 R 9 0 R 10 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (NOM) /Rect [50 700 250 720] /F 4 /P 3 0 R >>",
        &prenom,
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (ATTESTATION) /Rect [50 620 64 634] /F 4 /AS /Off /AP << /N << /Yes 11 0 R /Off 11 0 R >> >> >>",
        "<< /FT /Btn /Ff 49152 /T (TYPE_COMPTE) /Kids [8 0 R 9 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /Parent 7 0 R /Rect [50 580 64 594] /F 4 /AS /Off /AP << /N << /REER 11 0 R /Off 11 0 R >> >> >>",
        "<< /Type /Annot /Subtype /Widget /Parent 7 0 R /Rect [90 580 104 594] /F 4 /AS /Off /AP << /N << /CELI 11 0 R /Off 11 0 R >> >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (PAYS) /Opt [(Canada) (France)] /Rect [50 540 250 560] /F 4 /P 3 0 R >>",
        "<< /Type /XObject /Subtype /Form /BBox [0 0 14 14] /Length 0 >>\nstream\n\nendstream",
    ];
    Document::open(build(&objects), &OpenOptions::default()).unwrap()
}

fn values(doc: &Document) -> HashMap<String, String> {
    let saved = doc.save(&SaveOptions::default()).unwrap();
    let doc = Document::open(saved, &OpenOptions::default()).unwrap();
    field_values(&doc)
        .unwrap()
        .into_iter()
        .map(|(name, values)| {
            // A radio group's value: the state of the widget that is on.
            let value = values
                .iter()
                .find(|v| *v != "Off")
                .or(values.first())
                .cloned()
                .unwrap_or_default();
            (name, value)
        })
        .collect()
}

fn text(pairs: &[(&str, &str)]) -> HashMap<String, FillValue> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), FillValue::Text((*value).to_owned())))
        .collect()
}

#[test]
fn each_field_written_says_what_it_held_and_a_name_absent_is_refused() {
    let mut doc = form(None);
    let filled = fill_fields_with(
        &mut doc,
        &text(&[
            ("NOM", "Côté"),
            ("PRENOM", "Lise"),
            ("TYPE_COMPTE", "REER"),
            ("ABSENT", "x"),
        ]),
        &FillOptions::default(),
    )
    .unwrap();
    assert_eq!(filled.refused, ["ABSENT"]);
    assert_eq!(filled.written["PRENOM"], Written::new("Lise", "Ancien"));
    assert_eq!(filled.written["NOM"].previous, "");
    assert_eq!(filled.written["TYPE_COMPTE"], Written::new("REER", "Off"));
    let read = values(&doc);
    assert_eq!(
        (
            read["NOM"].as_str(),
            read["PRENOM"].as_str(),
            read["TYPE_COMPTE"].as_str()
        ),
        ("Côté", "Lise", "REER")
    );
}

#[test]
fn a_choice_not_offered_is_refused_and_the_field_left_as_it_was() {
    let mut doc = form(None);
    let filled = fill_fields_with(
        &mut doc,
        &text(&[("TYPE_COMPTE", "FERR")]),
        &FillOptions::default(),
    )
    .unwrap();
    assert_eq!(filled.refused, ["TYPE_COMPTE"]);
    assert!(filled.written.is_empty());
    assert_eq!(values(&doc)["TYPE_COMPTE"], "Off");
}

#[test]
fn strictly_a_value_the_field_does_not_allow_is_refused_alone() {
    let options = FillOptions::default().strict(true);
    let mut doc = form(Some(8));
    let filled = fill_fields_with(
        &mut doc,
        &text(&[
            ("NOM", "Nguyễn"),
            ("PRENOM", "Marie-Élisabeth"),
            ("TYPE_COMPTE", "CELI"),
            ("PAYS", "Mexique"),
        ]),
        &options,
    )
    .unwrap();
    assert_eq!(filled.refused, ["NOM", "PAYS", "PRENOM"]);
    let read = values(&doc);
    assert_eq!(
        (
            read["NOM"].as_str(),
            read["PRENOM"].as_str(),
            read["TYPE_COMPTE"].as_str()
        ),
        ("", "Ancien", "CELI")
    );
    // Without strictness, they are written: the text drawn with `?` for what its font lacks.
    let mut doc = form(Some(8));
    let filled = fill_fields_with(
        &mut doc,
        &text(&[("NOM", "Nguyễn"), ("PAYS", "Mexique")]),
        &FillOptions::default(),
    )
    .unwrap();
    assert!(filled.refused.is_empty());
    assert_eq!(filled.characters_replaced, 1);
    assert_eq!(values(&doc)["NOM"], "Nguyễn");
}

#[test]
fn a_field_filled_by_hand_is_kept_when_asked_and_replaced_otherwise() {
    let mut doc = form(None);
    let options = FillOptions::default().keep_filled(true);
    let filled = fill_fields_with(
        &mut doc,
        &text(&[("PRENOM", "Lise"), ("NOM", "Côté"), ("ATTESTATION", "Yes")]),
        &options,
    )
    .unwrap();
    assert_eq!(filled.kept, ["PRENOM"]);
    let read = values(&doc);
    assert_eq!(
        (
            read["NOM"].as_str(),
            read["PRENOM"].as_str(),
            read["ATTESTATION"].as_str()
        ),
        ("Côté", "Ancien", "Yes")
    );
    // The same value is no reason to keep it.
    let filled = fill_fields_with(&mut doc, &text(&[("PRENOM", "Ancien")]), &options).unwrap();
    assert!(filled.kept.is_empty());
    let filled = fill_fields_with(
        &mut doc,
        &text(&[("PRENOM", "Lise")]),
        &FillOptions::default(),
    )
    .unwrap();
    assert!(filled.kept.is_empty());
    assert_eq!(values(&doc)["PRENOM"], "Lise");
}

#[test]
fn undoing_restores_each_field_but_one_changed_since() {
    let mut doc = form(None);
    let filled = fill_fields_with(
        &mut doc,
        &text(&[("NOM", "Côté"), ("PRENOM", "Lise"), ("TYPE_COMPTE", "CELI")]),
        &FillOptions::default(),
    )
    .unwrap();
    // PRENOM retouched by hand.
    fill_fields_with(
        &mut doc,
        &text(&[("PRENOM", "Lisette")]),
        &FillOptions::default(),
    )
    .unwrap();
    let mut written: BTreeMap<String, Written> = filled.written;
    written.insert("GONE".into(), Written::new("x", ""));
    let kept = unfill_fields(&mut doc, &written).unwrap();
    assert_eq!(kept, ["PRENOM"]);
    let read = values(&doc);
    assert_eq!(
        (
            read["NOM"].as_str(),
            read["PRENOM"].as_str(),
            read["TYPE_COMPTE"].as_str()
        ),
        ("", "Lisette", "Off")
    );
}
