"""The glyphs a page draws, read in Python: text, place, advance, size, font, and what the
document says a reader reads instead."""

import pytest
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


def test_word_indices_blocks_and_search_highlights():
    doc = sealwax.Document.from_bytes(page(
        b"BT /F1 20 Tf 40 500 Td (a multi-) Tj 0 -24 Td (line test) Tj ET",
        annots="[10 0 R]",
    ))
    words = doc.words(0)
    assert [(w["text"], w["block"], w["line"], w["word"]) for w in words] == [
        ("a", 0, 0, 0), ("multi-", 0, 0, 1), ("line", 0, 1, 0), ("test", 0, 1, 1)
    ]
    block, = doc.text_blocks(0)
    first, second = block["lines"]
    assert first["direction"] == (1, 0)
    assert first["words"] + second["words"] == words
    assert block["bbox"][3] == first["bbox"][3]
    hit, = doc.search(0, "MULTILINE")
    assert len(hit) == 2
    assert hit[0][0][1] > hit[1][0][1]
    assert hit[0][0][0] == words[1]["bbox"][0]
    assert hit[1][3] == (words[2]["bbox"][2], words[2]["bbox"][1])
    assert doc.search(0, "Alice") == []
    assert doc.words(0, annotations=True)[-1]["text"] == "Alice"
    assert doc.search(0, "Alice", annotations=True)
    for method, args in [(doc.words, (1,)), (doc.text_blocks, (1,)), (doc.search, (1, "x"))]:
        with pytest.raises(ValueError):
            method(*args)


def one_font(base_font: str, text: bytes) -> bytes:
    """One page showing `text` in a font that is object 5, whatever the document."""
    return build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R "
        "/Resources << /Font << /F 5 0 R >> >> >>",
        stream("", b"BT /F 12 Tf 10 10 Td (" + text + b") Tj ET"),
        f"<< /Type /Font /Subtype /Type1 /BaseFont /{base_font} /Encoding /WinAnsiEncoding >>",
    ])


def test_documents_read_in_turn_never_mix_their_fonts():
    """Each thread keeps the fonts of the document it read last; another document, whose font is
    the same object number, gets its own."""
    first = sealwax.Document.from_bytes(one_font("Helvetica", b"AB"))
    second = sealwax.Document.from_bytes(one_font("Courier", b"xyz"))
    for _ in range(2):
        assert [(g["text"], g["font"]) for g in first.glyphs(0)] == [("A", "Helvetica"), ("B", "Helvetica")]
        assert [g["font"] for g in second.glyphs(0)] == ["Courier"] * 3
        assert second.drawings(0) == {"lines": [], "rectangles": [], "quads": []}
    # Read again after the thread let go of what it kept: the same.
    sealwax.release_cache()
    assert [g["text"] for g in first.glyphs(0)] == ["A", "B"]


@pytest.mark.parametrize(("setup", "extra"), [
    (b"0 0 100 100 re W n", ""),
    (b"2 0 0 2 0 0 cm 0 0 100 100 re W n", ""),
    (b"0 0 100 100 re W n", "/Rotate 90"),
    (b"BT /F1 50 Tf 7 Tr 10 10 Td (Window) Tj ET", ""),
])
def test_layout_respects_graphics_clips_and_their_scope(setup, extra):
    data = page(b"q " + setup + b" BT /F1 12 Tf 210 110 Td (Outside) Tj ET Q "
                b"BT /F1 12 Tf 210 110 Td (After) Tj ET", extra=extra)
    doc = sealwax.Document.from_bytes(data)
    assert "Outside" not in [w["text"] for w in doc.words(0)]
    assert "After" in [w["text"] for w in doc.words(0)]
    assert not doc.search(0, "Outside")
    assert len(doc.search(0, "After")) == 1
    assert all(w["text"] != "Outside" for b in doc.text_blocks(0) for line in b["lines"] for w in line["words"])


@pytest.mark.parametrize("annotation", [False, True])
def test_layout_respects_form_and_annotation_appearance_boxes(annotation):
    data = build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Contents 4 0 R "
        "/Resources << /XObject << /Form 5 0 R >> >> /Annots [6 0 R] >>",
        stream("", b"" if annotation else b"/Form Do"),
        stream("/Type /XObject /Subtype /Form /BBox [0 0 80 40] "
               "/Resources << /Font << /F << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
               b"BT /F 12 Tf 5 15 Td (Inside) Tj 150 0 Td (Outside) Tj ET"),
        "<< /Type /Annot /Subtype /Stamp /Rect [50 100 130 140] /F 4 /AP << /N 5 0 R >> >>",
    ])
    doc = sealwax.Document.from_bytes(data)
    assert [w["text"] for w in doc.words(0, annotation)] == ["Inside"]
    assert not doc.search(0, "Outside", annotation)


def test_actual_text_intersects_the_clip_for_each_character():
    doc = sealwax.Document.from_bytes(page(
        b"0 0 100 100 re W n /Span << /ActualText (AB) >> BDC "
        b"BT /F1 12 Tf 10 15 Td (x) Tj 190 0 Td (y) Tj ET EMC"
    ))
    assert [w["text"] for w in doc.words(0)] == ["A"]
    assert not doc.search(0, "B")


def test_text_reads_the_current_revision():
    doc = sealwax.Document.new()
    doc.add_page(150, 150)
    doc.insert_text(0, (10, 25), "Alpha")
    assert [w["text"] for w in doc.words(0)] == ["Alpha"]
    doc = sealwax.Document.from_bytes(doc.save())
    doc.insert_text(0, (10, 50), "Beta")
    assert len(doc.search(0, "Beta")) == 1
    assert [w["text"] for w in doc.words(0)] == ["Alpha", "Beta"]
