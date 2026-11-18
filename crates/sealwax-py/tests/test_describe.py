"""What a document says about itself, its outline, the lines it paints, pages taken out of it,
and documents processed from several threads."""

from concurrent.futures import ThreadPoolExecutor

from conftest import build, stream
from test_fields import encrypted_form

import sealwax

XMP = (
    b'<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
    b'<rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/" '
    b'pdfaid:part="3" pdfaid:conformance="A"/></rdf:RDF></x:xmpmeta>'
)


def archived() -> bytes:
    """A document claiming PDF/A-3a, with a title."""
    return build([
        "<< /Type /Catalog /Pages 2 0 R /Metadata 4 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
        stream("/Type /Metadata /Subtype /XML", XMP),
        "<< /Title <FEFF00C90074006500> /CreationDate (D:20240131120000Z) >>",
    ], trailer="/Info 5 0 R")


def test_metadata_of_an_archive_and_of_a_document_encrypted_without_password():
    metadata = sealwax.Document.from_bytes(archived()).metadata()
    assert metadata == {
        "version": "1.7",
        "encrypted": False,
        "encryption": None,
        "linearized": False,
        "has_forms": False,
        "has_signatures": False,
        "pdfa": "3A",
        "info": {"Title": "Éte", "CreationDate": "D:20240131120000Z"},
    }
    locked = sealwax.Document.from_bytes(encrypted_form()).metadata()
    assert locked["encrypted"] and locked["has_forms"] and not locked["has_signatures"]
    assert locked["encryption"] == "Standard V1 R2 40-bit RC4"
    assert locked["pdfa"] is None and locked["info"] == {}


def outlined() -> bytes:
    """Three pages; outline: One (page 1) > Sub (named, page 3); Gone (a name nobody defines)."""
    return build([
        "<< /Type /Catalog /Pages 2 0 R /Outlines 6 0 R /Names << /Dests << /Names [(sub) [5 0 R /Fit]] >> >> >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
        "<< /Type /Outlines /First 7 0 R >>",
        "<< /Title (One) /Parent 6 0 R /Next 9 0 R /First 8 0 R /Dest [3 0 R /Fit] >>",
        "<< /Title (Sub) /Parent 7 0 R /A << /S /GoTo /D (sub) >> >>",
        "<< /Title (Gone) /Parent 6 0 R /Dest /nowhere >>",
    ])


def test_the_outline_resolves_named_destinations_and_never_guesses():
    assert sealwax.Document.from_bytes(outlined()).outline() == [
        {"level": 1, "title": "One", "page_number": 1},
        {"level": 2, "title": "Sub", "page_number": 3},
        {"level": 1, "title": "Gone", "page_number": None},
    ]


def test_text_in_utf8_without_its_mark_reads_as_utf8():
    """Many writers put UTF-8 in a text string without its byte order mark; PDFDocEncoding
    text that is not UTF-8 stays PDFDocEncoding."""
    document = sealwax.Document.from_bytes(build([
        "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /AcroForm << /Fields [7 0 R] >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [7 0 R] >>",
        b"<< /Title (Caf\xc3\xa9) /Author (l\xe2\x80\x99eau) /Subject (\xe9t\xe9) >>",
        "<< /Type /Outlines /First 6 0 R /Last 6 0 R >>",
        b"<< /Title (Caf\xc3\xa9) /Parent 5 0 R /Dest [3 0 R /Fit] >>",
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (Pr\xc3\xa9nom) /TU (Votre pr\xc3\xa9nom) "
        b"/Rect [0 0 50 20] /P 3 0 R >>",
    ], trailer="/Info 4 0 R"))
    assert document.metadata()["info"] == {"Title": "Café", "Author": "l\u2019eau", "Subject": "été"}
    assert document.outline()[0]["title"] == "Café"
    [field] = document.fields()
    assert (field["name"], field["label"]) == ("Prénom", "Votre prénom")
    assert document.fill_fields({"Prénom": "Zoé"})["occurrences"] == 1
    assert document.field_values()["Prénom"] == ["Zoé"]


def five_pages() -> bytes:
    """Five pages 100 to 104 points wide; page 2 links to a web page."""
    objects: list[str | bytes] = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [4 0 R 6 0 R 8 0 R 10 0 R 12 0 R] /Count 5 >>",
        "<< /Type /Annot /Subtype /Link /Rect [0 0 50 50] /A << /S /URI /URI (https://example.org) >> >>",
    ]
    for page in range(5):
        annots = " /Annots [3 0 R]" if page == 1 else ""
        objects.append(
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {100 + page} 200] /Contents {5 + 2 * page} 0 R{annots} >>"
        )
        objects.append(stream("", f"0 0 {10 * (page + 1)} 10 re f".encode()))
    return build(objects)


def test_pages_taken_out_keep_their_content_and_their_link():
    document = sealwax.Document.from_bytes(five_pages())
    extracted = sealwax.Document.from_bytes(document.extract_pages(1, 3))
    assert [g.media_box[2] for g in extracted.geometry()] == [101, 102]
    assert extracted.drawings(0)["rectangles"] == [(0.0, 0.0, 20.0, 10.0)]
    assert b"https://example.org" in extracted.save(xref="table")
    for start, stop in ((2, 2), (4, 6), (-1, 2)):
        try:
            document.extract_pages(start, stop)
        except (ValueError, OverflowError):
            pass
        else:
            raise AssertionError(f"[{start}, {stop}) accepted")


def test_drawings_follow_a_turned_matrix():
    data = build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R >>",
        stream("", b"q 0 1 -1 0 300 0 cm 10 20 m 110 20 l S 50 50 100 30 re f Q"),
    ])
    drawings = sealwax.Document.from_bytes(data).drawings(0)
    assert drawings == {
        "lines": [(280.0, 10.0, 280.0, 110.0)],
        "rectangles": [(220.0, 50.0, 250.0, 150.0)],
        "quads": [],
    }


def test_drawings_close_boxes_and_include_annotations_on_request():
    data = build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Annots [5 0 R] >>",
        # Two boxes in one path, one drawn with `re`, one closed with `h`; a triangle closed.
        stream("", b"10 10 40 30 re 100 100 m 120 100 l 120 130 l 100 130 l h f 0 200 m 40 200 l 20 230 l h S"),
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (c) /Rect [300 300 312 312] /F 4 /AP << /N 6 0 R >> >>",
        stream("/Type /XObject /Subtype /Form /BBox [0 0 12 12]", b"0 0 12 12 re S"),
    ])
    document = sealwax.Document.from_bytes(data)
    drawings = document.drawings(0)
    assert drawings["rectangles"] == [(10.0, 10.0, 50.0, 40.0), (100.0, 100.0, 120.0, 130.0)]
    assert drawings["lines"][-1] == (20.0, 230.0, 0.0, 200.0) and len(drawings["lines"]) == 3
    assert document.drawings(0, annotations=True)["rectangles"][-1] == (300.0, 300.0, 312.0, 312.0)


def test_documents_processed_in_parallel_threads_read_as_they_do_alone():
    sources = [five_pages(), outlined(), archived(), encrypted_form()]

    def work(data: bytes) -> tuple:
        document = sealwax.Document.from_bytes(data)
        return (
            document.render_png(0, 0.5),
            [g["text"] for g in document.glyphs(0, annotations=True)],
            document.drawings(0),
            document.outline(),
            document.metadata(),
        )

    alone = [work(data) for data in sources]
    with ThreadPoolExecutor(8) as pool:
        together = list(pool.map(work, sources * 4))
    assert together == alone * 4
    # One document read from several threads at once.
    shared = sealwax.Document.from_bytes(five_pages())
    with ThreadPoolExecutor(8) as pool:
        images = list(pool.map(lambda page: shared.render_png(page % 5, 0.5), range(20)))
    assert images == [shared.render_png(page % 5, 0.5) for page in range(20)]


def test_links_say_where_they_lead_and_pages_their_labels():
    document = sealwax.Document.from_bytes(build([
        "<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums [0 << /S /r >> 1 << /S /D /P (A-) >>] >> >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [5 0 R 6 0 R 7 0 R] >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>",
        "<< /Type /Annot /Subtype /Link /Rect [10 20 110 40] /Dest [4 0 R /Fit] >>",
        "<< /Type /Annot /Subtype /Link /Rect [10 50 110 70] /A << /S /URI /URI (https://example.org) >> >>",
        "<< /Type /Annot /Subtype /Link /Rect [10 80 110 99] /A << /S /JavaScript /JS (x) >> >>",
    ]))
    links = document.links(0)
    assert [(link["kind"], link["page_number"], link["uri"], link["name"]) for link in links] == [
        ("page", 2, None, None),
        ("uri", None, "https://example.org", None),
        ("unknown", None, None, "JavaScript"),
    ]
    assert links[0]["rect"] == (10, 20, 110, 40)
    assert document.links(1) == []
    assert document.page_labels() == ["i", "A-1"]
