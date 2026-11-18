#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{pdf, pdf_with_trailer};
use sealwax_core::{
    Dict, Document, Error, Name, ObjId, Object, OpenOptions, SaveOptions, Stream, XrefFormat,
};

const FORMATS: [XrefFormat; 2] = [XrefFormat::Stream, XrefFormat::Table];

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).expect("opens")
}

fn save(doc: &Document, format: XrefFormat) -> Vec<u8> {
    doc.save(&SaveOptions::default().xref(format))
        .expect("saves")
}

/// Catalog, page tree, two pages sharing a content stream, an info dictionary, an orphan and
/// a reference to a missing object.
fn sample() -> Vec<u8> {
    pdf_with_trailer(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Missing 99 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>",
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Contents 5 0 R /Rotate 90 >>",
            ),
            (5, "<< /Length 7 >>\nstream\n0 0 m S\nendstream"),
            (6, "<< /Title (R\\351sum\\351 \\(v2\\)) /Producer (test) >>"),
            (7, "(nobody points here)"),
        ],
        false,
        "/Info 6 0 R /ID [<00112233445566778899AABBCCDDEEFF> <FFEEDDCCBBAA99887766554433221100>]",
    )
}

#[test]
fn a_full_save_reads_back_with_the_same_pages() {
    let original = open(sample());
    for format in FORMATS {
        let saved = open(save(&original, format));
        assert_eq!(saved.page_count(), 2, "{format:?}");
        assert_eq!(saved.geometry(), original.geometry(), "{format:?}");
    }
}

#[test]
fn a_full_save_renumbers_from_the_catalog_and_drops_what_is_unreachable() {
    for format in FORMATS {
        let saved = open(save(&open(sample()), format));
        assert_eq!(saved.catalog_id(), ObjId::new(1), "{format:?}");
        let catalog = saved.object(ObjId::new(1)).unwrap().unwrap();
        let catalog = catalog.as_dict().unwrap();
        // The missing object reads as null; the orphan string is gone.
        assert_eq!(catalog.get(b"Missing"), Some(&Object::Null), "{format:?}");
        // Catalog, pages, 2 pages, content stream, info: 6 objects, then only machinery.
        let strings = (1..20)
            .filter_map(|n| saved.object(ObjId::new(n)).unwrap())
            .filter(|object| matches!(object, Object::String(_)))
            .count();
        assert_eq!(strings, 0, "{format:?}");
    }
}

#[test]
fn a_full_save_shares_what_was_shared() {
    let saved = open(save(&open(sample()), XrefFormat::Table));
    let dict = |id: ObjId| {
        saved
            .object(id)
            .unwrap()
            .unwrap()
            .as_dict()
            .unwrap()
            .clone()
    };
    let pages = dict(saved.catalog_id())
        .get(b"Pages")
        .unwrap()
        .as_ref_id()
        .unwrap();
    let kids: Vec<ObjId> = dict(pages)
        .get(b"Kids")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|kid| kid.as_ref_id().unwrap())
        .collect();
    let contents: Vec<ObjId> = kids
        .iter()
        .map(|kid| dict(*kid).get(b"Contents").unwrap().as_ref_id().unwrap())
        .collect();
    assert_eq!(contents[0], contents[1]);
    let Some(Object::Stream(stream)) = saved.object(contents[0]).unwrap() else {
        panic!("the content stream reads back");
    };
    assert_eq!(stream.data, b"0 0 m S");
}

#[test]
fn a_full_save_keeps_info_and_the_permanent_half_of_the_id() {
    for format in FORMATS {
        let data = save(&open(sample()), format);
        let saved = open(data.clone());
        let info = saved.object(saved.info_id().unwrap()).unwrap().unwrap();
        assert_eq!(
            info.as_dict().unwrap().get(b"Title"),
            Some(&Object::String(b"R\xe9sum\xe9 (v2)".to_vec())),
            "{format:?}"
        );
        let text = String::from_utf8_lossy(&data);
        assert!(
            text.contains("/ID [<00112233445566778899AABBCCDDEEFF> <"),
            "{format:?}"
        );
        assert!(
            !text.contains("<FFEEDDCCBBAA99887766554433221100>"),
            "{format:?}"
        );
    }
}

#[test]
fn a_full_save_follows_the_pdf_a_syntax_rules() {
    for format in FORMATS {
        let data = save(&open(sample()), format);
        assert!(
            data.starts_with(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n"),
            "{format:?}"
        );
        assert!(data.ends_with(b"%%EOF\n"), "{format:?}");
        let text = String::from_utf8_lossy(&data);
        assert!(
            text.contains(">>\nstream\n0 0 m S\nendstream\nendobj\n"),
            "{format:?}"
        );
        assert!(!text.contains("/Encrypt"), "{format:?}");
    }
}

#[test]
fn a_full_save_with_streams_packs_objects_and_is_deterministic() {
    let doc = open(sample());
    let first = save(&doc, XrefFormat::Stream);
    assert_eq!(first, save(&doc, XrefFormat::Stream));
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("/Type /ObjStm"));
    assert!(text.contains("/Type /XRef"));
    assert!(!text.contains("\nxref\n"));
    let table = save(&doc, XrefFormat::Table);
    let text = String::from_utf8_lossy(&table);
    assert!(!text.contains("/ObjStm"));
    assert!(text.contains("\nxref\n0 7\n0000000000 65535 f\r\n"));
}

#[test]
fn many_objects_fill_several_object_streams() {
    let pages = 250;
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (
            2,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {pages} >>",
                (0..pages)
                    .map(|i| format!("{} 0 R", i + 3))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ),
    ];
    for i in 0..pages {
        objects.push((
            i + 3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 100] >>",
                100 + i
            ),
        ));
    }
    let objects: Vec<(u32, &str)> = objects.iter().map(|(n, b)| (*n, b.as_str())).collect();
    let original = open(pdf(&objects, false));
    let data = save(&original, XrefFormat::Stream);
    assert_eq!(
        String::from_utf8_lossy(&data)
            .matches("/Type /ObjStm")
            .count(),
        3
    );
    let saved = open(data);
    assert_eq!(saved.page_count(), 250);
    assert_eq!(saved.geometry(), original.geometry());
}

#[test]
fn modified_and_added_objects_are_saved_in_full() {
    for format in FORMATS {
        let mut doc = open(sample());
        let mut page = doc.object(ObjId::new(3)).unwrap().unwrap();
        page.as_dict_mut()
            .unwrap()
            .insert("MediaBox", vec![0.into(), 0.into(), 300.into(), 400.into()]);
        doc.set_object(ObjId::new(3), page);
        let info = doc
            .add_object(Object::Dict(
                [("Producer", Object::String(b"sealwax".to_vec()))]
                    .into_iter()
                    .collect(),
            ))
            .unwrap();
        doc.set_info_id(Some(info));

        let saved = open(save(&doc, format));
        assert_eq!(
            saved.geometry()[0].media_box.to_tuple(),
            (0.0, 0.0, 300.0, 400.0),
            "{format:?}"
        );
        let info = saved.object(saved.info_id().unwrap()).unwrap().unwrap();
        assert_eq!(
            info.as_dict().unwrap().get(b"Producer"),
            Some(&Object::String(b"sealwax".to_vec())),
            "{format:?}"
        );
    }
}

#[test]
fn an_incremental_save_appends_to_the_input_unchanged() {
    for base in [
        sample(),
        save(&open(sample()), XrefFormat::Stream),
        save(&open(sample()), XrefFormat::Table),
    ] {
        let mut doc = open(base.clone());
        let page_id = doc
            .object(doc.catalog_id())
            .unwrap()
            .and_then(|catalog| {
                let pages = catalog.as_dict()?.get(b"Pages")?.as_ref_id()?;
                let pages = doc.object(pages).ok()??;
                pages
                    .as_dict()?
                    .get(b"Kids")?
                    .as_array()?
                    .first()?
                    .as_ref_id()
            })
            .unwrap();
        let mut page = doc.object(page_id).unwrap().unwrap();
        page.as_dict_mut().unwrap().insert("Rotate", 180);
        doc.set_object(page_id, page);
        let added = doc
            .add_object(Object::Stream(Stream {
                dict: Dict::new(),
                data: b"q Q".to_vec(),
            }))
            .unwrap();

        let updated = doc.save_incremental().unwrap();
        assert!(updated.starts_with(&base));
        assert!(updated.len() > base.len());

        let reopened = open(updated.clone());
        assert_eq!(reopened.geometry()[0].rotation.degrees(), 180);
        assert_eq!(reopened.geometry()[1], doc.geometry()[1]);
        let Some(Object::Stream(stream)) = reopened.object(added).unwrap() else {
            panic!("the added stream reads back");
        };
        assert_eq!(stream.data, b"q Q");

        // The update is written in the kind of the section it follows, and points to it.
        let tail = String::from_utf8_lossy(&updated[base.len()..]).into_owned();
        let base_is_stream = String::from_utf8_lossy(&base).contains("/Type /XRef");
        assert_eq!(tail.contains("/Type /XRef"), base_is_stream);
        assert_eq!(tail.contains("\nxref\n"), !base_is_stream);
        let base_startxref = String::from_utf8_lossy(&base)
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(tail.contains(&format!("/Prev {base_startxref}")), "{tail}");
    }
}

#[test]
fn an_incremental_save_without_change_is_the_input() {
    let base = sample();
    assert_eq!(open(base.clone()).save_incremental().unwrap(), base);
}

#[test]
fn an_incremental_save_refuses_a_repaired_input() {
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
        ],
        true,
    );
    let mut doc = open(data);
    doc.add_object(Object::Null).unwrap();
    let error = doc.save_incremental().unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }), "{error:?}");
    // A full save repairs it, and the result takes an update.
    let mut repaired = open(save(&doc, XrefFormat::Stream));
    repaired.add_object(Object::Name(Name::from("Ok"))).unwrap();
    assert!(repaired.save_incremental().is_ok());
}

#[test]
fn new_objects_never_take_the_number_of_an_existing_one() {
    // /Size claims 2 while objects go up to 3.
    let mut data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
        ],
        false,
    );
    let at = data.windows(7).position(|w| w == b"/Size 4").unwrap();
    data[at + 6] = b'2';
    let mut doc = open(data);
    assert_eq!(doc.add_object(Object::Null).unwrap(), ObjId::new(4));
}

#[test]
fn a_full_save_rebuilds_a_page_tree_that_does_not_read() {
    // /Pages points nowhere: hayro finds the two pages by scanning the file for dictionaries
    // with /Contents. Written as is, the output would have no page (found by fuzzing, on
    // poppler-395-0-fuzzed.pdf of pdf.js).
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 9 0 R >>"),
            (
                3,
                "<< /Type /Page /Parent 9 0 R /MediaBox [0 0 300 400] /Rotate 90 /Contents 5 0 R >>",
            ),
            (4, "<< /Type /Page /Parent 9 0 R /Contents 5 0 R >>"),
            (5, "<< /Length 7 >>\nstream\n0 0 m S\nendstream"),
        ],
        false,
    );
    let original = open(data);
    assert_eq!(original.page_count(), 2);
    for format in FORMATS {
        let saved = open(save(&original, format));
        assert_eq!(saved.page_count(), 2, "{format:?}");
        assert_eq!(saved.geometry(), original.geometry(), "{format:?}");
    }
}

#[test]
fn a_full_save_writes_a_catalog_of_its_own_when_the_catalog_is_no_dictionary() {
    // The catalog read as a stream (an object stream that a later revision gave the catalog's
    // number, in a fuzzed file), its pages found all the same: written as is, it would lead to
    // no page. The catalog written is a new one, over the page tree rebuilt.
    let mut doc = open(sample());
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("ObjStm"));
    dict.insert("N", 0);
    dict.insert("First", 0);
    doc.set_object(
        doc.catalog_id(),
        Object::Stream(Stream {
            dict,
            data: Vec::new(),
        }),
    );
    for format in FORMATS {
        let saved = open(save(&doc, format));
        assert_eq!(saved.page_count(), 2, "{format:?}");
        assert_eq!(saved.geometry(), doc.geometry(), "{format:?}");
        let Some(Object::Dict(catalog)) = saved.object(saved.catalog_id()).unwrap() else {
            panic!("the catalog is no dictionary");
        };
        assert_eq!(
            catalog.get(b"Type"),
            Some(&Object::Name(Name::from("Catalog")))
        );
    }
}

#[test]
fn a_full_save_rebuilds_a_page_tree_that_leads_elsewhere() {
    // /Pages is a dictionary, but not a page tree (a damaged object stream did that in a fuzzed
    // file): hayro finds the page by scanning, and the tree must be rebuilt for it.
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Namespace /NS (http://iso.org/pdf/ssn) >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R >>",
            ),
            (4, "<< /Length 7 >>\nstream\n0 0 m S\nendstream"),
        ],
        false,
    );
    let original = open(data);
    assert_eq!(original.page_count(), 1);
    for format in FORMATS {
        let saved = open(save(&original, format));
        assert_eq!(saved.page_count(), 1, "{format:?}");
        assert_eq!(saved.geometry(), original.geometry(), "{format:?}");
    }
}

#[test]
fn a_full_save_writes_on_each_page_what_it_inherited() {
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] /Rotate 90 \
                 /Resources << /Font << /F1 4 0 R >> >> >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R >>"),
            (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        false,
    );
    let original = open(data);
    let saved = open(save(&original, XrefFormat::Stream));
    let page_id = saved.page_ids()[0].unwrap();
    let page = saved.object(page_id).unwrap().unwrap();
    let page = page.as_dict().unwrap();
    let media_box: Vec<f64> = page
        .get(b"MediaBox")
        .and_then(Object::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(media_box, [0.0, 0.0, 200.0, 300.0]);
    assert_eq!(page.get(b"Rotate").and_then(Object::as_i64), Some(90));
    let font = page
        .get(b"Resources")
        .and_then(Object::as_dict)
        .and_then(|r| r.get(b"Font"))
        .and_then(Object::as_dict)
        .and_then(|f| f.get(b"F1"))
        .and_then(Object::as_ref_id)
        .unwrap();
    let font = saved.object(font).unwrap().unwrap();
    assert_eq!(
        font.as_dict()
            .unwrap()
            .get(b"BaseFont")
            .and_then(Object::as_name),
        Some(&b"Helvetica"[..])
    );
    assert_eq!(saved.geometry(), original.geometry());
}

#[test]
fn a_page_the_caller_changed_keeps_what_it_inherited_when_the_tree_is_rebuilt() {
    // /Pages is not a page tree: hayro finds the page by scanning, the save rebuilds a tree. The
    // page inherits its box and rotation from its /Parent, which the catalog does not reach. A
    // page the caller changed used to be written as it stood: still pointing at that parent,
    // which is not its parent in the new tree, and without what it inherited.
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Namespace /NS (http://iso.org/pdf/ssn) >>"),
            (3, "<< /Type /Page /Parent 6 0 R /Contents 4 0 R >>"),
            (4, "<< /Length 7 >>\nstream\n0 0 m S\nendstream"),
            (
                6,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 144] /Rotate 90 >>",
            ),
        ],
        false,
    );
    let mut doc = open(data);
    assert_eq!(doc.page_count(), 1);
    let before = doc.geometry();
    assert_eq!(before[0].media_box.x2, 300.0);
    let page_id = doc.page_ids()[0].unwrap();
    let mut page = doc.object(page_id).unwrap().unwrap();
    page.as_dict_mut()
        .unwrap()
        .insert("Annots", Vec::<Object>::new());
    doc.set_object(page_id, page);
    for format in FORMATS {
        let saved = open(save(&doc, format));
        assert_eq!(saved.geometry(), before, "{format:?}");
        let page = saved.object(saved.page_ids()[0].unwrap()).unwrap().unwrap();
        let page = page.as_dict().unwrap();
        assert!(page.contains_key(b"Annots"), "{format:?}");
        assert!(page.contains_key(b"MediaBox"), "{format:?}");
        let catalog = saved.object(saved.catalog_id()).unwrap().unwrap();
        assert_eq!(
            page.get(b"Parent"),
            catalog.as_dict().unwrap().get(b"Pages"),
            "{format:?}"
        );
    }
}

#[test]
fn a_document_without_pages_is_not_saved() {
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
        ],
        false,
    );
    let Ok(doc) = Document::open(data, &OpenOptions::default()) else {
        return; // hayro may already refuse it
    };
    let error = doc.save(&SaveOptions::default()).unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }), "{error:?}");
}

#[test]
fn an_incremental_save_refuses_an_input_whose_earlier_section_is_damaged() {
    // A sound update appended to a table whose offsets are wrong: the last section reads well,
    // but an update would chain to the damaged one.
    let mut data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>"),
        ],
        true,
    );
    let text = String::from_utf8_lossy(&data).into_owned();
    let prev: usize = text
        .rsplit("startxref\n")
        .next()
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let object_at = data.len();
    data.extend_from_slice(b"4 0 obj\n(update)\nendobj\n");
    let xref_at = data.len();
    data.extend_from_slice(
        format!(
            "xref\n4 1\n{object_at:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Prev {prev} >>\nstartxref\n{xref_at}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let mut doc = open(data);
    doc.add_object(Object::Null).unwrap();
    let error = doc.save_incremental().unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }), "{error:?}");
}

/// Object numbers stay below `MAX_OBJECT_NUMBER` and their search ends: an `N G obj` header in a
/// comment of a content stream (`% 4294967295 0 obj`) once made the number saturate and the
/// search loop for ever (external review), and a `/Size` past the ceiling sends new objects to
/// free numbers below it.
#[test]
fn new_object_numbers_stay_below_the_ceiling_and_their_search_ends() {
    use sealwax_core::MAX_OBJECT_NUMBER;
    let page = |content: &str, trailer: &str| {
        common::pdf_with_trailer(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
                ),
                (
                    4,
                    &format!(
                        "<< /Length {} >>\nstream\n{content}\nendstream",
                        content.len()
                    ),
                ),
            ],
            false,
            trailer,
        )
    };
    for (content, trailer) in [
        ("% 4294967295 0 obj\n", ""),
        ("% 999 0 obj\n", ""),
        ("0 0 m", "/Size 4294967295"),
    ] {
        let mut doc = Document::open(page(content, trailer), &OpenOptions::default()).unwrap();
        let first = doc.add_object(Object::Null).unwrap();
        let second = doc.add_object(Object::Null).unwrap();
        assert!(first.number <= MAX_OBJECT_NUMBER && second.number <= MAX_OBJECT_NUMBER);
        assert_ne!(first, second);
        // A header in a comment defines nothing: numbers follow the file's own.
        assert!(first.number < 100, "{content:?} {trailer:?}: {first}");
        doc.save(&SaveOptions::default()).unwrap();
        doc.save_incremental().unwrap();
    }
}
