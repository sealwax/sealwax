"""DOCX converted to PDF: laid out by docxide-pdf, written by sealwax."""

from pathlib import Path

import pytest

import sealwax

CRATES = Path(__file__).parents[2]
# Written by crates/sealwax-convert/tests/data/make_docx.py: a heading, a link and a list on the
# first page, a paragraph on the second; and "Hi!" in the font Boxes, which no system has.
TWO_PAGES = (CRATES / "sealwax-convert/tests/data/two-pages.docx").read_bytes()
BOXES = (CRATES / "sealwax-convert/tests/data/boxes.docx").read_bytes()
# Written by crates/sealwax-compose/tests/data/make_fonts.py: two faces of the family Boxes.
BOXES_FONT = (CRATES / "sealwax-compose/tests/data/boxes.ttc").read_bytes()


def text(doc: sealwax.Document, page_index: int) -> str:
    return "".join(g["text"] or "" for g in doc.glyphs(page_index)).replace(" ", "")


def test_a_docx_becomes_a_pdf_with_its_pages_text_and_bookmarks():
    pdf, report = sealwax.docx_to_pdf(TWO_PAGES)
    # Which fonts are missing depends on the system's.
    assert report == {"skipped": {}, "not_described": {}, "missing_fonts": report["missing_fonts"]}
    doc = sealwax.Document.from_bytes(pdf)
    assert doc.page_count == 2
    assert text(doc, 0).startswith("AheadingHellofromasyntheticdocument.")
    assert text(doc, 1) == "Secondpage."
    assert [(e["title"], e["page_number"]) for e in doc.outline()] == [("A heading", 1)]


def test_what_is_not_a_docx_is_a_value_error():
    with pytest.raises(ValueError):
        sealwax.docx_to_pdf(b"not a docx")


def test_a_document_that_unpacks_past_the_bound_is_refused():
    with pytest.raises(sealwax.PdfLimitError) as raised:
        sealwax.docx_to_pdf(TWO_PAGES, limits=sealwax.Limits(max_unpacked_bytes=1_000))
    assert raised.value.limit == "max_unpacked_bytes"
    assert sealwax.Limits().max_unpacked_bytes == 512 << 20


def test_font_dirs_are_searched_and_must_be_a_list():
    pdf, _ = sealwax.docx_to_pdf(TWO_PAGES, font_dirs=[Path(__file__).parent])
    assert pdf.startswith(b"%PDF")
    with pytest.raises(TypeError):
        sealwax.docx_to_pdf(TWO_PAGES, font_dirs="/usr/share/fonts")  # type: ignore[arg-type]


def test_a_font_no_face_provides_is_reported_until_supplied():
    boxes = {"name": "Boxes", "bold": False, "italic": False, "replaces": None}
    _, report = sealwax.docx_to_pdf(BOXES)
    assert boxes in report["missing_fonts"]
    # Data without a usable face is ignored.
    _, report = sealwax.docx_to_pdf(BOXES, fonts=[b"not a font"])
    assert boxes in report["missing_fonts"]
    pdf, report = sealwax.docx_to_pdf(BOXES, fonts=[b"not a font", BOXES_FONT])
    assert boxes not in report["missing_fonts"]
    # Embedded as a subset: its name past the subset tag.
    glyphs = sealwax.Document.from_bytes(pdf).glyphs(0)
    assert [(g["text"], (g["font"] or "").split("+")[-1]) for g in glyphs] == [
        ("H", "BoxesTT-Regular"),
        ("i", "BoxesTT-Regular"),
        ("!", "BoxesTT-Regular"),
    ]
    with pytest.raises(TypeError):
        sealwax.docx_to_pdf(BOXES, fonts=BOXES_FONT)  # type: ignore[arg-type]


def test_an_untrusted_document_converts_in_a_process_of_its_own():
    pdf, report = sealwax.isolated(sealwax.docx_to_pdf, TWO_PAGES, timeout=60)
    assert sealwax.Document.from_bytes(pdf).page_count == 2
    assert report["skipped"] == {}
