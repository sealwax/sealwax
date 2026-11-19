#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::pdf;
use sealwax_core::{Document, Error, Limits, OpenOptions, SaveOptions};

/// A one-page document drawing `content`, with `resources` and more objects from number 10.
fn page(content: &str, resources: &str, more: &[(u32, &str)]) -> Vec<u8> {
    let page = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
         /Resources << {resources} >> >>"
    );
    let stream = format!(
        "<< /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    );
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, page.as_str()),
        (4, stream.as_str()),
    ];
    objects.extend_from_slice(more);
    pdf(&objects, false)
}

fn form(content: &str, resources: &str) -> String {
    format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << {resources} >> \
         /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    )
}

fn check(data: Vec<u8>, limits: Limits) -> Result<sealwax_core::Peaks, Error> {
    Document::open(data, &OpenOptions::default().limits(limits))
        .expect("opens")
        .check_pages()
}

fn exceeded(result: Result<sealwax_core::Peaks, Error>) -> &'static str {
    match result {
        Err(Error::LimitExceeded { what }) => what,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn a_plain_page_is_measured_within_the_defaults() {
    let data = page(
        "BT /F1 12 Tf 10 10 Td (Hello) Tj ET /X0 Do",
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> \
         /XObject << /X0 10 0 R >>",
        &[(10, &form("0 0 5 5 re f", ""))],
    );
    let peaks = check(data, Limits::default()).unwrap();
    assert_eq!(peaks.shown_bytes, 5);
    // Six operators on the page, two in the form.
    assert_eq!(peaks.operators, 6);
    assert_eq!(peaks.page_cost, 6 + 5 + 2);
}

#[test]
fn each_bound_is_named_when_passed() {
    let mut limits = Limits::default();
    limits.max_operators = 3;
    assert_eq!(
        exceeded(check(page("q q q Q Q Q", "", &[]), limits)),
        "max_operators"
    );

    let mut limits = Limits::default();
    limits.max_shown_bytes = 4;
    let data = page("BT [(ab) 10 (cde)] TJ ET", "", &[]);
    assert_eq!(exceeded(check(data, limits)), "max_shown_bytes");

    let mut limits = Limits::default();
    limits.max_image_pixels = 99;
    let image = "<< /Type /XObject /Subtype /Image /Width 10 /Height 10 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8 /Length 1 >>\nstream\n\0\nendstream";
    let data = page("/Im0 Do", "/XObject << /Im0 10 0 R >>", &[(10, image)]);
    assert_eq!(exceeded(check(data, limits)), "max_image_pixels");
    let data = page("BI /W 10 /H 10 /CS /G /BPC 8 ID \0 EI", "", &[]);
    assert_eq!(exceeded(check(data, limits)), "max_image_pixels");

    let mut limits = Limits::default();
    limits.max_mesh_bytes = 8;
    let mesh = "<< /ShadingType 4 /ColorSpace /DeviceGray /BitsPerCoordinate 8 /BitsPerComponent 8 \
                /BitsPerFlag 8 /Decode [0 1 0 1 0 1] /Length 12 >>\nstream\n000000000000\nendstream";
    let data = page("/Sh0 sh", "/Shading << /Sh0 10 0 R >>", &[(10, mesh)]);
    assert_eq!(exceeded(check(data, limits)), "max_mesh_bytes");

    let font = "<< /Type /Font /Subtype /Type0 /BaseFont /F /Encoding /Identity-H \
                /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /F \
                /W [0 [500 600] 10 4294967295 500] >>] >>";
    let data = page(
        "BT /F1 12 Tf <0001> Tj ET",
        "/Font << /F1 10 0 R >>",
        &[(10, font)],
    );
    assert_eq!(exceeded(check(data, Limits::default())), "max_code_range");
}

/// Ten forms each drawing the next ten times: a billion draws from a few hundred bytes. The cost
/// is added up from each form's, walked once.
#[test]
fn forms_drawn_in_forms_are_counted_each_time_without_being_walked_again() {
    let mut forms = Vec::new();
    for level in 0..10u32 {
        let content = if level == 9 {
            "0 0 1 1 re f".to_owned()
        } else {
            "/X Do ".repeat(10)
        };
        forms.push((
            10 + level,
            form(
                &content,
                &format!("/XObject << /X {} 0 R >>", 11 + level.min(8)),
            ),
        ));
    }
    let borrowed: Vec<(u32, &str)> = forms.iter().map(|(n, b)| (*n, b.as_str())).collect();
    let data = page("/X Do", "/XObject << /X 10 0 R >>", &borrowed);
    let started = Instant::now();
    assert_eq!(
        exceeded(check(data.clone(), Limits::default())),
        "max_page_cost"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let peaks = check(data, Limits::unbounded()).unwrap();
    // 10^9 rectangles of 2 operators, 10^8 + ... + 10 Do of the forms above, 1 on the page.
    assert!(peaks.page_cost > 2_000_000_000, "{}", peaks.page_cost);
}

/// hayro does not resolve a reference met while resolving the same object: a form naming itself,
/// or two forms naming each other, draw each other once, for the interpreter as for the walk.
#[test]
fn forms_drawing_each_other_draw_once_as_hayro_reads_them() {
    let data = page(
        "/X Do",
        "/XObject << /X 10 0 R >>",
        &[(10, &form("0 0 1 1 re f /X Do", "/XObject << /X 10 0 R >>"))],
    );
    assert_eq!(check(data, Limits::default()).unwrap().page_cost, 1 + 3);
    let data = page(
        "/A Do",
        "/XObject << /A 10 0 R >>",
        &[
            (10, &form("/B Do", "/XObject << /B 11 0 R >>")),
            (11, &form("/A Do", "/XObject << /A 10 0 R >>")),
        ],
    );
    assert_eq!(check(data, Limits::default()).unwrap().page_cost, 1 + 1 + 1);
}

#[test]
fn type3_glyphs_cost_their_procedures_and_appearances_count() {
    let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] \
                /CharProcs << /a 11 0 R >> /Encoding << /Differences [97 /a] >> /FirstChar 97 \
                /LastChar 97 /Widths [1] >>";
    let procedure = form("1 0 d0 0 0 1 1 re f", "");
    let data = page(
        "BT /T 1 Tf (aaaa) Tj ET",
        "/Font << /T 10 0 R >>",
        &[(10, font), (11, &procedure)],
    );
    let peaks = check(data, Limits::default()).unwrap();
    // 4 operators; 4 glyphs, each 1 plus its procedure's 3.
    assert_eq!(peaks.page_cost, 4 + 4 * (1 + 3));
}

/// Every entry point checks the deadline, even on a page too small to reach a check while it is
/// walked (external review: open, check_pages and save went through a deadline passed).
#[test]
fn a_deadline_passed_is_a_timeout() {
    let mut limits = Limits::default();
    limits.deadline = Some(Instant::now());
    let opened = Document::open(page("q Q", "", &[]), &OpenOptions::default().limits(limits));
    assert!(matches!(opened, Err(Error::Timeout)));

    let deadline = Instant::now() + Duration::from_secs(1);
    let mut limits = Limits::default();
    limits.deadline = Some(deadline);
    let mut doc = Document::open(page("q Q", "", &[]), &OpenOptions::default().limits(limits))
        .expect("opens well before its deadline");
    while Instant::now() <= deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(doc.check_time(), Err(Error::Timeout)));
    assert!(matches!(doc.check_pages(), Err(Error::Timeout)));
    assert!(matches!(doc.measure_pages(), Err(Error::Timeout)));
    assert!(matches!(doc.check_content(0, b"q Q"), Err(Error::Timeout)));
    assert!(matches!(
        doc.save(&SaveOptions::default()),
        Err(Error::Timeout)
    ));
    assert!(matches!(doc.save_incremental(), Err(Error::Timeout)));
    assert!(matches!(doc.compress_streams(), Err(Error::Timeout)));
    let mut merged = Document::empty().unwrap();
    assert!(matches!(merged.append_pages(&doc), Err(Error::Timeout)));

    let doc_later = Document::open(
        page("q Q", "", &[]),
        &OpenOptions::default().limits(Limits::default().timeout(Duration::from_secs(3600))),
    )
    .unwrap();
    assert!(doc_later.check_time().is_ok());
}

/// A content stream of 4 KiB that decodes to 4 MiB, twice deflated: refused before anything
/// decodes it, as is a dash too short to stroke and a page holding too many annotations.
#[test]
fn bombs_dashes_and_annotations_are_refused_before_interpreting() {
    use std::io::Write as _;
    let deflate = |data: &[u8]| {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    };
    let bomb = deflate(&deflate(&vec![b' '; 4 << 20]));
    let mut data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>",
            ),
        ],
        false,
    );
    // The stream goes after the others, its data binary: offsets do not matter to hayro's repair.
    let at = data.windows(4).position(|w| w == b"xref").unwrap();
    let mut stream = format!(
        "4 0 obj\n<< /Filter [/FlateDecode /FlateDecode] /Length {} >>\nstream\n",
        bomb.len()
    )
    .into_bytes();
    stream.extend_from_slice(&bomb);
    stream.extend_from_slice(b"\nendstream\nendobj\n");
    data.splice(at..at, stream);
    let mut limits = Limits::default();
    limits.max_stream_bytes = 1 << 20;
    let doc = Document::open(data.clone(), &OpenOptions::default().limits(limits)).unwrap();
    assert_eq!(doc.page_count(), 1);
    assert_eq!(exceeded(doc.check_pages()), "max_stream_bytes");
    // Within the bound, the stream is counted, not refused.
    let doc = Document::open(data, &OpenOptions::default()).unwrap();
    assert_eq!(doc.check_pages().unwrap().stream_bytes, 4 << 20);

    let data = page("[0.00001 0.00001] 0 d 0 0 m 200 200 l S", "", &[]);
    assert_eq!(exceeded(check(data, Limits::default())), "min_dash_period");
    let data = page("[3 2] 0 d 0 0 m 200 200 l S", "", &[]);
    assert_eq!(
        check(data, Limits::default()).unwrap().dash_period,
        Some(5.0)
    );

    let annotations =
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [5 0 R 5 0 R 5 0 R] >>";
    let data = pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, annotations),
            (5, "<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] >>"),
        ],
        false,
    );
    let mut limits = Limits::default();
    limits.max_annotations = 2;
    assert_eq!(exceeded(check(data, limits)), "max_annotations");
}

/// An object stream that decodes to 4 MiB is refused before hayro opens the document: hayro
/// decodes object streams whole as it reads the objects they hold. A small one opens.
#[test]
fn object_streams_are_checked_before_the_document_is_read() {
    use std::io::Write as _;
    let object_stream = |padding: usize| {
        let page = b"3 0 << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>";
        let mut plain = page.to_vec();
        plain.extend(std::iter::repeat_n(b' ', padding));
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&plain).unwrap();
        let packed = encoder.finish().unwrap();
        let mut data = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"
            .to_vec();
        data.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {} >>\nstream\n",
                packed.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&packed);
        data.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
        data
    };
    let mut limits = Limits::default();
    limits.max_stream_bytes = 1 << 20;
    let options = OpenOptions::default().limits(limits);
    match Document::open(object_stream(4 << 20), &options) {
        Err(Error::LimitExceeded { what }) => assert_eq!(what, "max_stream_bytes"),
        other => panic!("not refused: {other:?}"),
    }
    assert!(Document::open(object_stream(1000), &options).is_ok());
}

/// A chain of filters is bounded at every stage, not only when it is Flate alone: 8 KiB of spaces
/// deflated then hex-encoded passed a bound of 1 KiB (external review), as did a stage that the
/// next one shrinks to nothing.
#[test]
fn chained_filters_are_bounded_at_every_stage() {
    use std::io::Write as _;
    let deflate = |data: &[u8]| {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    };
    let hex = |data: &[u8]| {
        let mut out: String = data.iter().map(|b| format!("{b:02x}")).collect();
        out.push('>');
        out
    };
    let content_stream = |filters: &str, data: &str| {
        let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>";
        let stream = format!(
            "<< /Filter [{filters}] /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        );
        pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, page),
                (4, stream.as_str()),
            ],
            false,
        )
    };
    let content_stream_bytes = |filters: &str, data: &[u8]| {
        let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>";
        let mut stream =
            format!("<< /Filter [{filters}] /Length {} >>\nstream\n", data.len()).into_bytes();
        stream.extend_from_slice(data);
        stream.extend_from_slice(b"\nendstream");
        common::pdf_bytes(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
                (3, page.as_bytes().to_vec()),
                (4, stream),
            ],
            false,
            "",
        )
    };
    let mut limits = Limits::default();
    limits.max_stream_bytes = 1024;
    let spaces = vec![b' '; 8192];
    let hexed = hex(&deflate(&spaces));
    let data = content_stream("/ASCIIHexDecode /FlateDecode", &hexed);
    assert_eq!(exceeded(check(data, limits)), "max_stream_bytes");
    // 8 KiB in the middle, nothing left at the end: the middle stage is what costs.
    let middle = hex(&deflate(&deflate(&hex(&spaces).into_bytes())));
    let data = content_stream(
        "/ASCIIHexDecode /FlateDecode /FlateDecode /ASCIIHexDecode",
        &middle,
    );
    assert_eq!(exceeded(check(data, limits)), "max_stream_bytes");
    // A last stage that ends at once (`>` first) leaves the stage before it decoded whole by a
    // reader (second review: 128 KiB went through a bound of 32 KiB).
    let mut early = b">".to_vec();
    early.extend_from_slice(&[b' '; 131_071]);
    let mut wide = Limits::default();
    wide.max_stream_bytes = 32 * 1024;
    let data = content_stream_bytes("/FlateDecode /ASCIIHexDecode", &deflate(&early));
    assert_eq!(exceeded(check(data, wide)), "max_stream_bytes");
    // Within the bound, the same chain draws.
    let text = hex(&deflate(b"0 0 m 10 10 l S"));
    let data = content_stream("/AHx /Fl", &text);
    assert!(check(data, limits).is_ok());
}

/// A deadline that passes while a document is saved fails the save, the reading back of the
/// output included (second review: an 8 MB page was saved past its deadline, the reading back
/// having no deadline and nothing checking the time after it).
#[test]
fn a_deadline_passing_during_a_save_fails_it() {
    let content = "0 g\n".repeat(2_000_000);
    let data = page(&content, "", &[]);
    // How long opening (warm) and saving take here, without a deadline.
    let mut opening = Duration::MAX;
    let mut doc = None;
    for _ in 0..2 {
        let started = Instant::now();
        doc = Some(Document::open(data.clone(), &OpenOptions::default()).unwrap());
        opening = opening.min(started.elapsed());
    }
    let started = Instant::now();
    doc.unwrap().save(&SaveOptions::default()).unwrap();
    let saving = started.elapsed();
    for _ in 0..3 {
        // The deadline passes a third of the way through the save.
        let deadline = Instant::now() + opening + saving * 3 / 10;
        let mut limits = Limits::default();
        limits.deadline = Some(deadline);
        let Ok(doc) = Document::open(data.clone(), &OpenOptions::default().limits(limits)) else {
            continue;
        };
        match doc.save(&SaveOptions::default()) {
            Err(Error::Timeout) => {}
            Ok(_) => {
                let past = Instant::now().saturating_duration_since(deadline);
                assert!(past < saving / 8, "saved {past:?} past its deadline");
            }
            Err(other) => panic!("{other}"),
        }
    }
}

#[test]
fn a_page_is_checked_once_when_within_the_limits_and_every_time_when_not() {
    let data = page("q q q Q Q Q", "", &[]);
    let open = |limits: Limits| {
        Document::open(data.clone(), &OpenOptions::default().limits(limits)).expect("opens")
    };
    let doc = open(Limits::default());
    assert!(doc.check_page(0).is_ok() && doc.check_page(0).is_ok());
    assert!(matches!(
        doc.check_page(1),
        Err(Error::InvalidArgument { what: "page index" })
    ));
    let mut limits = Limits::default();
    limits.max_operators = 3;
    let doc = open(limits);
    for _ in 0..2 {
        assert!(matches!(
            doc.check_page(0),
            Err(Error::LimitExceeded {
                what: "max_operators"
            })
        ));
    }
}

#[test]
fn shared_forms_are_measured_with_each_drawing_sites_resources() {
    let shared = form("/Shape Do", "").replace("/Resources <<  >>", "");
    let a = form("/Shared Do", "/XObject << /Shared 12 0 R /Shape 13 0 R >>");
    let b = form("/Shared Do", "/XObject << /Shared 12 0 R /Shape 14 0 R >>");
    let small = form("q Q", "");
    let large = form("q Q q Q", "");
    let data = page(
        "/A Do /B Do",
        "/XObject << /A 10 0 R /B 11 0 R >>",
        &[
            (10, &a),
            (11, &b),
            (12, &shared),
            (13, &small),
            (14, &large),
        ],
    );
    assert_eq!(check(data, Limits::default()).unwrap().page_cost, 12);
}
