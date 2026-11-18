//! A filled form made a template again: nothing of the person who filled it is left in the file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::Read as _;

use sealwax_annots::{
    FillValue, TemplateOptions, clear_template, field_values, fill_fields, template_content,
};
use sealwax_core::{Document, Object, OpenOptions, SaveOptions, XrefFormat};

const CLIENT: &str = "Lise Exemple";
const AMOUNT: &str = "5 500 $";

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
            "trailer\n<< /Size {} /Root 1 0 R /Info 12 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
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

/// A form filled for a client (synthetic): a text field and a checked box filled by sealwax, a
/// field without widget holding the client too, an amount in a free text note with its pop-up,
/// a link, the client in the information, the XMP metadata and the XFA form.
fn filled_form() -> Document {
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 6 0 R] /DA (/Helv 0 Tf 0 g) /XFA 11 0 R >> /Metadata 10 0 R /NeedsRendering false >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots 13 0 R >>".to_owned(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (nom) /Rect [72 600 272 620] /F 4 /P 3 0 R >>".to_owned(),
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (reer) /Rect [72 560 84 572] /F 4 /P 3 0 R /AS /Off /AP << /N << /Yes 9 0 R /Off 9 0 R >> >> >>".to_owned(),
        format!("<< /FT /Tx /T (cache) /V ({CLIENT}) >>"),
        format!("<< /Type /Annot /Subtype /FreeText /Rect [300 600 380 620] /Contents ({AMOUNT}) /DA (/Helv 12 Tf 0 g) /Popup 8 0 R /AP << /N 14 0 R >> >>"),
        "<< /Type /Annot /Subtype /Popup /Rect [400 600 500 700] /Parent 7 0 R >>".to_owned(),
        stream("/Type /XObject /Subtype /Form /BBox [0 0 12 12]", "0 0 12 12 re f"),
        stream("/Type /Metadata /Subtype /XML", &format!("<x:xmpmeta><dc:creator>{CLIENT}</dc:creator></x:xmpmeta>")),
        stream("", &format!("<xdp:xdp><nom>{CLIENT}</nom></xdp:xdp>")),
        format!("<< /Author ({CLIENT}) /Subject (Ouverture) /Keywords (client) /Title (Dossier) /Producer (test) >>"),
        "[4 0 R 5 0 R 7 0 R 8 0 R 15 0 R]".to_owned(),
        stream("/Type /XObject /Subtype /Form /BBox [0 0 80 20]", &format!("BT /Helv 12 Tf 2 4 Td ({AMOUNT}) Tj ET")),
        "<< /Type /Annot /Subtype /Link /Rect [72 700 200 720] /A << /S /URI /URI (https://example.org) >> >>".to_owned(),
    ];
    let mut doc = Document::open(build(&objects), &OpenOptions::default()).unwrap();
    let values = HashMap::from([
        ("nom".to_owned(), FillValue::Text(CLIENT.to_owned())),
        ("reer".to_owned(), FillValue::Text("Yes".to_owned())),
    ]);
    fill_fields(&mut doc, &values).unwrap();
    reopened(&doc)
}

fn reopened(doc: &Document) -> Document {
    Document::open(
        doc.save(&SaveOptions::default()).unwrap(),
        &OpenOptions::default(),
    )
    .unwrap()
}

/// The file as a parser reads it: its bytes, and every stream inflated.
fn everything(doc: &Document) -> Vec<u8> {
    let saved = doc
        .save(&SaveOptions::default().xref(XrefFormat::Table))
        .unwrap();
    let mut all = saved.clone();
    let mut at = 0;
    while let Some(start) = find(&saved[at..], b"stream\n").map(|i| at + i + 7) {
        let end = find(&saved[start..], b"endstream").map_or(saved.len(), |i| start + i);
        let mut inflated = Vec::new();
        if flate2::read::ZlibDecoder::new(&saved[start..end])
            .read_to_end(&mut inflated)
            .is_ok()
        {
            all.extend_from_slice(&inflated);
        }
        // Past `endstream`, whose own "stream\n" would otherwise be taken for the next stream's.
        at = (end + b"endstream".len()).min(saved.len());
    }
    all
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// How a PDF string can spell `text`: as is, in hex, in UTF-16 (raw and in hex).
fn spellings(text: &str) -> Vec<Vec<u8>> {
    let hex: String = text.bytes().map(|b| format!("{b:02X}")).collect();
    let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let utf16_hex: String = utf16.iter().map(|b| format!("{b:02X}")).collect();
    vec![
        text.as_bytes().to_vec(),
        hex.into_bytes(),
        utf16,
        utf16_hex.into_bytes(),
    ]
}

fn catalog(doc: &Document) -> sealwax_core::Dict {
    match doc.object(doc.catalog_id()).unwrap() {
        Some(Object::Dict(catalog)) => catalog,
        other => panic!("no catalog: {other:?}"),
    }
}

#[test]
fn the_fields_stay_empty_and_the_client_is_nowhere_in_the_file() {
    let mut doc = filled_form();
    let before = everything(&doc).to_ascii_uppercase();
    assert!(find(&before, CLIENT.to_ascii_uppercase().as_bytes()).is_some());
    let content = template_content(&doc).unwrap();
    assert_eq!(
        (content.pages, content.fields, content.annotations),
        (1, 3, 2)
    );
    let cleared = clear_template(&mut doc, &TemplateOptions::new("Exemple · Ouverture")).unwrap();
    assert_eq!((cleared.fields, cleared.annotations_removed), (3, 2));
    let doc = reopened(&doc);
    let values: HashMap<String, Vec<String>> = field_values(&doc).unwrap().into_iter().collect();
    assert_eq!(values["nom"], [""]);
    assert_eq!(values["reer"], ["Off"]);
    let content = template_content(&doc).unwrap();
    assert_eq!((content.fields, content.annotations), (3, 0));
    let info = doc.metadata().info;
    assert_eq!(
        info.get("Title").map(String::as_str),
        Some("Exemple · Ouverture")
    );
    assert!(
        ["Author", "Subject", "Keywords"]
            .iter()
            .all(|key| !info.contains_key(*key))
    );
    assert_eq!(info.get("Producer").map(String::as_str), Some("test"));
    let catalog = catalog(&doc);
    assert!(!catalog.contains_key(b"Metadata") && !catalog.contains_key(b"NeedsRendering"));
    // Not on the page, and not left over in an object nothing reaches.
    let after = everything(&doc).to_ascii_uppercase();
    for text in [CLIENT, AMOUNT] {
        for spelling in spellings(text) {
            assert!(
                find(&after, &spelling.to_ascii_uppercase()).is_none(),
                "{text} left as {}",
                String::from_utf8_lossy(&spelling)
            );
        }
    }
    // The link stays.
    assert!(find(&after, b"/LINK").is_some());
}

#[test]
fn the_annotations_added_stay_when_asked() {
    let mut doc = filled_form();
    let options = TemplateOptions::new("x").keep_annotations(true);
    let cleared = clear_template(&mut doc, &options).unwrap();
    assert_eq!(cleared.annotations_removed, 0);
    let doc = reopened(&doc);
    assert_eq!(template_content(&doc).unwrap().annotations, 2);
    let values: HashMap<String, Vec<String>> = field_values(&doc).unwrap().into_iter().collect();
    assert_eq!(values["nom"], [""]);
}
