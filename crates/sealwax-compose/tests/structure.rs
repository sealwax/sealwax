//! Tagged compositions: marked content, the structure tree that ties it, links in the tree, and
//! the outline, read back from the saved document.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read as _;

use sealwax_compose::{
    ArtifactKind, Color, Composition, Element, FillRule, HeaderScope, Info, Item, Kid, Link,
    LinkTarget, ListNumbering, Mark, OutlineEntry, Page, Paint, Path, Segment, Structure,
    TableCell, compose,
};
use sealwax_core::{Dict, Document, Limits, Name, ObjId, Object, OpenOptions, SaveOptions};

fn reopened(composition: &Composition) -> (Document, Vec<(&'static str, usize)>) {
    let composed = compose(composition, &Limits::default()).unwrap();
    let bytes = composed.document.save(&SaveOptions::default()).unwrap();
    let skipped = composed.skipped.iter().collect();
    (
        Document::open(bytes, &OpenOptions::default()).unwrap(),
        skipped,
    )
}

fn square() -> Item {
    Item::Fill {
        path: Path(vec![Segment::Rect(10.0, 10.0, 20.0, 20.0)]),
        fill_rule: FillRule::NonZero,
        paint: Paint::opaque(Color::Gray(0.0)),
    }
}

fn tagged(tag: &str, mcid: u32) -> Item {
    Item::BeginMarked(Mark::Element {
        tag: tag.to_owned(),
        mcid,
    })
}

fn page(items: Vec<Item>) -> Page {
    Page {
        width: 200.0,
        height: 300.0,
        items,
        links: Vec::new(),
    }
}

fn element(kind: &str, kids: Vec<Kid>) -> Element {
    Element {
        kind: kind.to_owned(),
        kids,
        ..Element::default()
    }
}

fn dict(doc: &Document, object: Option<&Object>) -> Dict {
    match object {
        Some(Object::Ref(id)) => match doc.object(*id).unwrap() {
            Some(Object::Dict(dict)) => dict,
            other => panic!("not a dictionary: {other:?}"),
        },
        Some(Object::Dict(dict)) => dict.clone(),
        other => panic!("not a dictionary: {other:?}"),
    }
}

fn reference(object: Option<&Object>) -> ObjId {
    match object {
        Some(Object::Ref(id)) => *id,
        other => panic!("not a reference: {other:?}"),
    }
}

fn catalog(doc: &Document) -> Dict {
    dict(doc, Some(&Object::Ref(doc.catalog_id())))
}

fn page_dict(doc: &Document, index: usize) -> Dict {
    dict(doc, Some(&Object::Ref(doc.page_ids()[index].unwrap())))
}

/// The decoded content stream of page `index`.
fn content(doc: &Document, index: usize) -> String {
    let id = reference(page_dict(doc, index).get(b"Contents"));
    let Some(Object::Stream(stream)) = doc.object(id).unwrap() else {
        panic!("no content stream");
    };
    let mut text = String::new();
    flate2::read::ZlibDecoder::new(stream.data.as_slice())
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn kids(element: &Dict) -> Vec<Object> {
    match element.get(b"K") {
        Some(Object::Array(kids)) => kids.clone(),
        other => panic!("no kids: {other:?}"),
    }
}

fn name(text: &str) -> Object {
    Object::Name(Name::from(text))
}

/// Two pages: an artifact and a paragraph on the first, a heading and its link on the second.
fn two_pages() -> Composition {
    let mut second = page(vec![tagged("H1", 3), square(), Item::EndMarked]);
    second.links = vec![Link {
        rect: [10.0, 10.0, 30.0, 30.0],
        target: LinkTarget::Uri("https://example.org/".to_owned()),
        description: "Example".to_owned(),
    }];
    let mut heading = element(
        "H1",
        vec![
            Kid::Content { page: 1, mcid: 3 },
            Kid::Link { page: 1, link: 0 },
        ],
    );
    heading.alt = Some("Heading".to_owned());
    Composition {
        pages: vec![
            page(vec![
                Item::BeginMarked(Mark::Artifact(Some(ArtifactKind::Pagination))),
                square(),
                Item::EndMarked,
                tagged("P", 7),
                square(),
                Item::EndMarked,
            ]),
            second,
        ],
        structure: Some(Structure {
            elements: vec![
                element("Document", vec![Kid::Element(1), Kid::Element(2)]),
                element("P", vec![Kid::Content { page: 0, mcid: 7 }]),
                heading,
            ],
        }),
        ..Composition::default()
    }
}

#[test]
fn the_tree_ties_each_element_to_its_content() {
    let (doc, skipped) = reopened(&two_pages());
    assert!(skipped.is_empty(), "{skipped:?}");
    let catalog = catalog(&doc);
    assert_eq!(
        dict(&doc, catalog.get(b"MarkInfo")).get(b"Marked"),
        Some(&Object::Bool(true))
    );
    let root = dict(&doc, catalog.get(b"StructTreeRoot"));
    let top = dict(&doc, root.get(b"K"));
    assert_eq!(top.get(b"S"), Some(&name("Document")));
    let [p, h1_ref] = kids(&top).try_into().unwrap();
    let (p, h1) = (dict(&doc, Some(&p)), dict(&doc, Some(&h1_ref)));
    // Ids are written from 0 on each page, in the order drawn.
    assert_eq!(kids(&p), [Object::Integer(0)]);
    assert_eq!(p.get(b"Pg"), Some(&Object::Ref(doc.page_ids()[0].unwrap())));
    assert!(content(&doc, 0).contains("/Artifact <</Type /Pagination>> BDC"));
    assert!(content(&doc, 0).contains("/P <</MCID 0>> BDC"));
    assert!(content(&doc, 1).contains("/H1 <</MCID 0>> BDC"));
    assert_eq!(h1.get(b"S"), Some(&name("H1")));
    assert!(h1.get(b"Alt").is_some());
    let [content_kid, link_kid] = kids(&h1).try_into().unwrap();
    assert_eq!(content_kid, Object::Integer(0));
    let objr = dict(&doc, Some(&link_kid));
    assert_eq!(objr.get(b"Type"), Some(&name("OBJR")));
    let annotation = dict(&doc, objr.get(b"Obj"));
    assert_eq!(annotation.get(b"StructParent"), Some(&Object::Integer(2)));
    // The parent tree: each page's ids, then the annotation.
    for index in 0..2 {
        let page = page_dict(&doc, index);
        assert_eq!(
            page.get(b"StructParents"),
            Some(&Object::Integer(index as i64))
        );
        assert_eq!(page.get(b"Tabs"), Some(&name("S")));
    }
    let nums = match dict(&doc, root.get(b"ParentTree")).get(b"Nums") {
        Some(Object::Array(nums)) => nums.clone(),
        other => panic!("no numbers: {other:?}"),
    };
    assert_eq!(nums.len(), 6);
    assert_eq!(nums[4], Object::Integer(2));
    assert_eq!(nums[5], h1_ref);
    assert_eq!(root.get(b"ParentTreeNextKey"), Some(&Object::Integer(3)));
}

#[test]
fn attributes_are_written_as_pdf_names_them() {
    let mut cell = element("TH", vec![Kid::Content { page: 0, mcid: 0 }]);
    cell.table_cell = Some(TableCell {
        header_scope: Some(HeaderScope::Column),
        col_span: 2,
    });
    let mut list = element("L", Vec::new());
    list.list_numbering = Some(ListNumbering::Decimal);
    list.lang = Some("fr-CA".to_owned());
    let composition = Composition {
        pages: vec![page(vec![tagged("TH", 0), square(), Item::EndMarked])],
        structure: Some(Structure {
            elements: vec![
                element("Document", vec![Kid::Element(1), Kid::Element(2)]),
                cell,
                list,
            ],
        }),
        ..Composition::default()
    };
    let (doc, _) = reopened(&composition);
    let root = dict(&doc, catalog(&doc).get(b"StructTreeRoot"));
    let [th, l] = kids(&dict(&doc, root.get(b"K"))).try_into().unwrap();
    let attributes = |element: Object| match dict(&doc, Some(&element)).get(b"A") {
        Some(Object::Array(items)) => dict(&doc, items.first()),
        other => panic!("no attributes: {other:?}"),
    };
    let table = attributes(th);
    assert_eq!(table.get(b"O"), Some(&name("Table")));
    assert_eq!(table.get(b"Scope"), Some(&name("Column")));
    assert_eq!(table.get(b"ColSpan"), Some(&Object::Integer(2)));
    let numbering = attributes(l.clone());
    assert_eq!(numbering.get(b"ListNumbering"), Some(&name("Decimal")));
    assert!(dict(&doc, Some(&l)).get(b"Lang").is_some());
}

#[test]
fn marked_sequences_are_balanced_whatever_the_items() {
    let composition = Composition {
        pages: vec![page(vec![
            Item::EndMarked,
            tagged("P", 0),
            Item::Group(vec![Item::BeginMarked(Mark::Artifact(None)), square()]),
            square(),
        ])],
        ..Composition::default()
    };
    let (doc, _) = reopened(&composition);
    let text = content(&doc, 0);
    let begins = text.matches(" BDC").count() + text.matches(" BMC").count();
    assert_eq!(begins, 2);
    assert_eq!(text.matches("EMC").count(), 2);
    // The stray end is left out: the stream starts with the paragraph.
    assert!(text.starts_with("/P <</MCID 0>> BDC"));
}

#[test]
fn what_does_not_tie_to_the_pages_is_counted() {
    let composition = Composition {
        pages: vec![page(vec![
            tagged("P", 1),
            square(),
            Item::EndMarked,
            tagged("P", 1),
            square(),
            Item::EndMarked,
        ])],
        structure: Some(Structure {
            elements: vec![
                element(
                    "Document",
                    vec![
                        Kid::Element(1),
                        Kid::Element(1),
                        Kid::Element(0),
                        Kid::Element(99),
                    ],
                ),
                element(
                    "P",
                    vec![
                        Kid::Content { page: 0, mcid: 1 },
                        Kid::Content { page: 0, mcid: 1 },
                        Kid::Content { page: 0, mcid: 42 },
                        Kid::Content { page: 9, mcid: 1 },
                        Kid::Link { page: 0, link: 5 },
                    ],
                ),
                element("Orphan", Vec::new()),
            ],
        }),
        ..Composition::default()
    };
    let (_, skipped) = reopened(&composition);
    assert_eq!(
        skipped,
        [
            ("marked content ids repeated on a page", 1),
            ("structure content not drawn", 3),
            ("structure elements listed twice or out of range", 3),
            ("structure elements outside the tree", 1),
            ("structure links not written", 1),
        ]
    );
}

#[test]
fn the_outline_nests_by_level_and_leads_to_its_headings() {
    let entry = |level, title: &str, page, top| OutlineEntry {
        level,
        title: title.to_owned(),
        page,
        top,
    };
    let composition = Composition {
        pages: vec![page(Vec::new()), page(Vec::new())],
        outline: vec![
            entry(2, "Starts at 2", 0, Some(250.5)),
            entry(4, "Skips 3", 0, None),
            entry(3, "Back to 3", 1, None),
            entry(1, "Top", 1, None),
            entry(1, "Nowhere", 7, None),
        ],
        info: Info {
            title: Some("Report".to_owned()),
            ..Info::default()
        },
        ..Composition::default()
    };
    let (doc, skipped) = reopened(&composition);
    assert_eq!(skipped, [("outline entries to pages out of range", 1)]);
    let levels: Vec<usize> = doc.outline().unwrap().iter().map(|e| e.level).collect();
    assert_eq!(levels, [1, 2, 2, 1, 1]);
    let catalog = catalog(&doc);
    assert_eq!(catalog.get(b"PageMode"), Some(&name("UseOutlines")));
    assert_eq!(
        dict(&doc, catalog.get(b"ViewerPreferences")).get(b"DisplayDocTitle"),
        Some(&Object::Bool(true))
    );
    let first = dict(&doc, dict(&doc, catalog.get(b"Outlines")).get(b"First"));
    let Some(Object::Array(destination)) = first.get(b"Dest") else {
        panic!("no destination");
    };
    assert_eq!(destination[1], name("XYZ"));
    assert_eq!(destination[3], Object::Real(250.5));
}

/// The XMP packet of a composed document, as written.
fn packet(composition: &Composition) -> String {
    let composed = compose(composition, &Limits::default()).unwrap();
    let doc = composed.document;
    let id = reference(catalog(&doc).get(b"Metadata"));
    let Some(Object::Stream(stream)) = doc.object(id).unwrap() else {
        panic!("no metadata stream");
    };
    String::from_utf8(stream.data).unwrap()
}

#[test]
fn pdf_ua_is_claimed_only_for_a_whole_document() {
    let mut composition = two_pages();
    composition.info.pdf_ua = true;
    assert!(packet(&composition).contains("<pdfuaid:part>1</pdfuaid:part>"));
    // A picture that is not there: the document is no longer what its layout checked.
    composition.pages[0].items.push(Item::Image(3));
    assert!(!packet(&composition).contains("pdfuaid:part"));
}
