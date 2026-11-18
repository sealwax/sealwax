//! What a page's links lead to, and the labels pages are shown with.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use sealwax_core::{Document, Error, Link, LinkTarget, OpenOptions, Rect};

fn open(data: Vec<u8>) -> Document {
    Document::open(data, &OpenOptions::default()).unwrap()
}

/// Seven pages (objects 3 to 9), the first holding links of every kind, and a Text annotation;
/// a named destination "Chap" to the second page; labels i, ii, A-8, A-9, Cover, A, B.
fn linked() -> Vec<u8> {
    let page =
        |annots: &str| format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] {annots} >>");
    let first = page(
        "/Annots [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 16 0 R 17 0 R 18 0 R 19 0 R 20 0 R]",
    );
    let other = page("");
    let link =
        |rest: &str| format!("<< /Type /Annot /Subtype /Link /Rect [10 20 110 40] {rest} >>");
    let objects = [
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R \
             /Names << /Dests << /Names [(Chap) [4 0 R /XYZ 0 800 0]] >> >> \
             /PageLabels << /Nums [0 << /S /r >> 2 << /S /D /P (A-) /St 8 >> \
             4 << /P (Cover) >> 5 << /S /A >>] >> >>"
                .to_owned(),
        ),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R 8 0 R 9 0 R] /Count 7 >>"
                .to_owned(),
        ),
        (3, first),
        (4, other.clone()),
        (5, other.clone()),
        (6, other.clone()),
        (7, other.clone()),
        (8, other.clone()),
        (9, other),
        (10, link("/Dest [5 0 R /Fit]")),
        (
            11,
            "<< /Type /Annot /Subtype /Link /Rect [110 40 10 20] /A << /S /GoTo /D (Chap) >> >>"
                .to_owned(),
        ),
        (
            12,
            link("/A << /S /URI /URI (https://example.org/a?b=1) >>"),
        ),
        (
            13,
            link(
                "/A << /S /GoToR /F << /Type /Filespec /F (other.pdf) \
                 /UF <FEFF00E9002E007000640066> >> /D [3 /Fit] >>",
            ),
        ),
        (14, link("/A << /S /Launch /F (run.exe) >>")),
        (15, link("/A << /S /Named /N /LastPage >>")),
        (16, link("/A << /S /Named /N /PrevPage >>")),
        (17, link("/A << /S /JavaScript /JS (app.alert(1)) >>")),
        (18, link("/Dest (Missing)")),
        (
            19,
            "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /Contents (note) >>".to_owned(),
        ),
        (
            20,
            "<< /Type /Annot /Subtype /Link /Dest [3 0 R /Fit] >>".to_owned(),
        ),
    ];
    let objects: Vec<(u32, &str)> = objects.iter().map(|(n, b)| (*n, b.as_str())).collect();
    common::pdf(&objects, false)
}

#[test]
fn each_link_says_where_it_leads_never_a_guess() {
    let doc = open(linked());
    let rect = Rect::from_corners(10.0, 20.0, 110.0, 40.0);
    let targets: Vec<LinkTarget> = doc
        .links(0)
        .unwrap()
        .into_iter()
        .map(|Link { rect: at, target }| {
            assert_eq!(at, rect, "{target:?}");
            target
        })
        .collect();
    assert_eq!(
        targets,
        [
            LinkTarget::Page(2),
            LinkTarget::Page(1),
            LinkTarget::Uri("https://example.org/a?b=1".to_owned()),
            LinkTarget::File {
                name: "é.pdf".to_owned(),
                page_index: Some(3)
            },
            LinkTarget::File {
                name: "run.exe".to_owned(),
                page_index: None
            },
            LinkTarget::Page(6),
            LinkTarget::Named("PrevPage".to_owned()),
            LinkTarget::Unknown {
                action: Some("JavaScript".to_owned())
            },
            LinkTarget::Unknown { action: None },
        ]
    );
    assert!(doc.links(1).unwrap().is_empty());
    assert!(matches!(
        doc.links(7),
        Err(Error::InvalidArgument { what: "page index" })
    ));
}

#[test]
fn pages_are_labelled_as_their_ranges_say() {
    let doc = open(linked());
    assert_eq!(
        doc.page_labels().unwrap().unwrap(),
        ["i", "ii", "A-8", "A-9", "Cover", "A", "B"]
    );
    let plain = common::pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>"),
        ],
        false,
    );
    assert_eq!(open(plain).page_labels().unwrap(), None);
}
