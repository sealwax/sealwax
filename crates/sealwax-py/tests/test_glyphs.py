"""The glyphs a page draws, read in Python: text, place, advance, size, font, and what the
document says a reader reads instead."""

from conftest import build, stream

import sealwax

TO_UNICODE = stream(
    "",
    b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n"
    b"1 begincodespacerange <00> <FF> endcodespacerange\n"
    b"2 beginbfchar <01> <00660069> <02> <0066006C> endbfchar\n"
    b"endcmap CMapName currentdict /CMap defineresource pop end end",
)


def page(content: bytes, extra: str = "", annots: str = "") -> bytes:
    """One page: /F1 Helvetica (WinAnsi), /F2 a subset whose codes 1 and 2 are the fi and fl
    ligatures, /T3 a Type 3 font drawing a square for `A`."""
    return build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R "
        "/Resources << /Font << /F1 5 0 R /F2 6 0 R /T3 8 0 R >> >> "
        f"{extra} {'/Annots ' + annots if annots else ''} >>",
        stream("", content),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Garamond /ToUnicode 7 0 R >>",
        TO_UNICODE,
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] "
        "/CharProcs << /A 9 0 R >> /Encoding << /Differences [65 /A] >> /FirstChar 65 /LastChar 65 "
        "/Widths [1000] >>",
        stream("", b"1000 0 d0 0 0 1000 1000 re f"),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [100 400 200 420] /F 4 /AP << /N 11 0 R >> >>",
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /H 5 0 R >> >>",
            b"BT /H 10 Tf 2 5 Td (Alice) Tj ET",
        ),
    ])


def text(glyphs: list[dict]) -> str:
    return "".join(glyph["text"] or "" for glyph in glyphs)


def read(glyphs: list[dict]) -> str:
    return "".join(
        glyph["actual_text"] if glyph["actual_text"] is not None else ("" if glyph["overdrawn"] else glyph["text"] or "")
        for glyph in glyphs
    )


def test_accents_and_ligatures_are_decoded_with_place_advance_size_and_font():
    document = sealwax.Document.from_bytes(page(b"BT /F1 20 Tf 72 700 Td (\xe9t\xe9) Tj /F2 20 Tf <0102> Tj ET"))
    glyphs = document.glyphs(0)
    assert text(glyphs) == "étéfifl"
    first = glyphs[0]
    assert first["origin"] == (72.0, 700.0)
    assert abs(first["end"][0] - (72 + 0.556 * 20)) < 1e-6 and first["end"][1] == 700.0
    assert abs(first["size"] - 20) < 1e-9 and first["direction"] == (1.0, 0.0)
    assert first["font"] == "Helvetica" and glyphs[3]["font"] == "ABCDEF+Garamond"
    assert not first["invisible"] and not first["overdrawn"] and first["actual_text"] is None
    x1, y1, x2, y2 = first["bbox"]
    assert x1 == 72.0 and y1 < 700 < y2 and abs(x2 - first["end"][0]) < 1e-9


def test_rotated_page_keeps_default_user_space_and_type3_and_invisible_text_are_there():
    document = sealwax.Document.from_bytes(page(
        b"BT /T3 12 Tf 10 10 Td (A) Tj ET BT 3 Tr /F1 12 Tf 100 200 Td (ocr) Tj ET",
        extra="/Rotate 90 /CropBox [50 70 550 770]",
    ))
    glyphs = document.glyphs(0)
    assert text(glyphs) == "Aocr"
    assert glyphs[0]["ink"] is None and abs(glyphs[0]["end"][0] - glyphs[0]["origin"][0] - 12) < 1e-9
    assert [g["invisible"] for g in glyphs] == [False, True, True, True]
    assert glyphs[1]["origin"] == (100.0, 200.0)
    assert document.geometry()[0].crop_box == (50, 70, 550, 770)


def test_faked_bold_and_actual_text_read_as_a_reader_reads_them():
    document = sealwax.Document.from_bytes(page(
        b"BT /F1 10 Tf 50 700 Td (B) Tj ET BT /F1 10 Tf 50.3 700 Td (B) Tj ET "
        b"/Span <</ActualText <FEFF0009>>> BDC BT /F1 10 Tf 60 700 Td (....) Tj ET EMC"
    ))
    glyphs = document.glyphs(0)
    assert text(glyphs) == "BB...."
    assert [g["overdrawn"] for g in glyphs[:2]] == [False, True]
    assert [g["actual_text"] for g in glyphs[2:]] == ["\t", "", "", ""]
    assert read(glyphs) == "B\t"


def test_annotations_are_read_only_when_asked_and_an_empty_page_has_no_glyph():
    document = sealwax.Document.from_bytes(page(b"0 0 10 10 re f", annots="[10 0 R]"))
    assert document.glyphs(0) == []
    glyphs = document.glyphs(0, annotations=True)
    assert text(glyphs) == "Alice" and all(g["annotation"] for g in glyphs)
    try:
        document.glyphs(1)
    except ValueError:
        pass
    else:
        raise AssertionError("a missing page is refused")
