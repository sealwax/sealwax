import time
import zlib

import pytest
from conftest import build, stream, text_page, xfdf

import sealwax


def test_flatten_bytes_paints_the_annotations_and_keeps_the_page():
    data = text_page(annots="[6 0 R]")
    flat = sealwax.flatten_bytes(data)
    doc = sealwax.Document.from_bytes(flat)
    assert doc.page_count == 1
    assert b"/Annots" not in sealwax.Document.from_bytes(flat).save(xref="table")


def test_a_document_reports_what_flattening_did():
    doc = sealwax.Document.from_bytes(text_page(annots="[6 0 R]"))
    report = doc.flatten()
    assert report["drawn"] + report["painted"] == 1
    assert report["links_kept"] == 0
    assert doc.is_modified
    assert sealwax.Document.from_bytes(doc.save()).page_count == 1


def test_xfdf_is_applied_and_refused_when_it_is_not_the_signer_pages():
    out = sealwax.apply_xfdf_bytes(text_page(), xfdf())
    assert sealwax.Document.from_bytes(out).page_count == 1
    doc = sealwax.Document.from_bytes(text_page())
    applied = doc.apply_xfdf(xfdf(), flatten=False)
    assert applied["pictures"] == 1 and applied["flattened"] is None
    with pytest.raises(sealwax.XfdfError, match="DTD"):
        sealwax.apply_xfdf_bytes(text_page(), '<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "b">]><xfdf/>')
    with pytest.raises(sealwax.XfdfError, match="page"):
        sealwax.apply_xfdf_bytes(text_page(), xfdf(page=5))
    assert issubclass(sealwax.XfdfError, sealwax.PdfError)


def test_xfdf_says_which_pages_changed_and_can_take_the_annotations_out():
    doc = sealwax.Document.from_bytes(text_page(annots="[6 0 R]"))
    applied = doc.apply_xfdf(xfdf(), flatten=False)
    assert applied["pages"] == [0] and applied["annotations_removed"] == 0
    applied = doc.apply_xfdf(xfdf(), remove_annotations=True)
    assert applied["annotations_removed"] == 1 and applied["pages"] == [0]
    assert b"/Annots" not in sealwax.Document.from_bytes(doc.save()).save(xref="table")
    out = sealwax.apply_xfdf_bytes(text_page(annots="[6 0 R]"), xfdf(), remove_annotations=True)
    assert b"/Annots" not in sealwax.Document.from_bytes(out).save(xref="table")


def quadrants_png(width: int = 40, height: int = 20) -> bytes:
    """Red at the top left, green at the top right, blue at the bottom left, yellow at the
    bottom right: a picture whose turn shows."""
    import struct

    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    colours = {(0, 0): b"\xff\x00\x00", (0, 1): b"\x00\xff\x00", (1, 0): b"\x00\x00\xff", (1, 1): b"\xff\xff\x00"}
    rows = b"".join(
        b"\x00" + b"".join(colours[(2 * y // height, 2 * x // width)] for x in range(width)) for y in range(height)
    )
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


@pytest.mark.parametrize("rotation", [0, 90, 180, 270])
def test_a_picture_turned_as_its_page_shows_upright(rotation):
    import base64

    page = build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Rotate {rotation} >>",
    ])
    # A box 100 x 50 as shown: its sides swapped in user space on a page turned a quarter turn.
    rect = "200,300,300,350" if rotation in (0, 180) else "200,300,250,400"
    url = "data:image/png;base64," + base64.b64encode(quadrants_png()).decode()
    xfdf = (
        '<?xml version="1.0" encoding="UTF-8" ?><xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">'
        f'<fields /><add><stamp page="0" rect="{rect}" rotation="{rotation}" flags="print,readonly,locked" '
        f'name="a1"><imagedata>{url}</imagedata></stamp></add><modify /></xfdf>'
    )
    doc = sealwax.Document.from_bytes(page)
    doc.apply_xfdf(xfdf)
    width, height, pixels = doc.render_rgba(0, 1.0)

    def at(x: int, y: int) -> tuple[int, int, int]:
        i = 4 * (y * width + x)
        return pixels[i], pixels[i + 1], pixels[i + 2]

    drawn = [(x, y) for y in range(height) for x in range(width) if at(x, y) != (255, 255, 255)]
    left, right = min(x for x, _ in drawn), max(x for x, _ in drawn)
    top, bottom = min(y for _, y in drawn), max(y for _, y in drawn)
    # Shown wider than high, as the box, and each colour in its corner.
    assert (right - left + 1, bottom - top + 1) == (100, 50)
    quarter_x, quarter_y = (right - left) // 4, (bottom - top) // 4
    assert at(left + quarter_x, top + quarter_y) == (255, 0, 0)
    assert at(right - quarter_x, top + quarter_y) == (0, 255, 0)
    assert at(left + quarter_x, bottom - quarter_y) == (0, 0, 255)
    assert at(right - quarter_x, bottom - quarter_y) == (255, 255, 0)


def test_a_clipped_render_is_that_part_of_the_page():
    doc = sealwax.Document.from_bytes(text_page())
    width, height, whole = doc.render_rgba(0, 2.0)
    w, h, part = doc.render_rgba(0, 2.0, clip=(100, 150, 64, 32))
    assert (w, h) == (64, 32)
    for row in range(32):
        start = ((150 + row) * width + 100) * 4
        expected, got = whole[start : start + 64 * 4], part[row * 64 * 4 : (row + 1) * 64 * 4]
        assert max(abs(a - b) for a, b in zip(expected, got)) <= 2
    assert doc.render_png(0, 2.0, clip=(0, 0, width, height)) == doc.render_png(0, 2.0)
    with pytest.raises(ValueError):
        doc.render_rgba(0, 2.0, clip=(width - 1, 0, 2, 2))


def test_the_xfdf_written_for_a_signed_field_is_what_apply_xfdf_draws():
    from conftest import png

    marks = [
        {"kind": "stamp", "page_index": 0, "rect": (100, 100, 200, 150), "name": "a1", "png": png(),
         "custom_data": {"fieldId": "f-1", "signed": "true"}},
        {"kind": "freetext", "page_index": 0, "rect": (300, 300, 420, 330), "name": "t1", "text": "Łukasz & co",
         "size": 14, "align": "center", "vertical_align": "center", "rotation": 0},
    ]
    xfdf = sealwax.xfdf_for(marks, deleted=[(0, "placeholder-1")])
    assert xfdf.count("<add>") == 1 and 'name="a1"' in xfdf and "fieldId" in xfdf
    doc = sealwax.Document.from_bytes(text_page())
    applied = doc.apply_xfdf(xfdf)
    assert (applied["pictures"], applied["texts"], applied["deletions_ignored"]) == (1, 1, 1)
    with pytest.raises(sealwax.XfdfError):
        sealwax.xfdf_for([{**marks[0], "rotation": 45}])
    with pytest.raises(sealwax.XfdfError):
        sealwax.xfdf_for([{**marks[0], "png": b"GIF89a"}])
    with pytest.raises(ValueError):
        sealwax.xfdf_for([{"kind": "stamp", "page_index": 0}])
    with pytest.raises(ValueError):
        sealwax.xfdf_for([{**marks[1], "align": "justify"}])


def test_a_font_that_is_no_truetype_program_is_refused_and_nothing_changes():
    doc = sealwax.Document.from_bytes(text_page())
    xfdf = sealwax.xfdf_for([
        {"kind": "freetext", "page_index": 0, "rect": (300, 300, 420, 330), "name": "t1", "text": "Łukasz",
         "font": "Caveat"},
    ])
    with pytest.raises(ValueError):
        doc.apply_xfdf(xfdf, fonts={"Caveat": b"not a font"})
    with pytest.raises(ValueError):
        sealwax.apply_xfdf_bytes(text_page(), xfdf, default_font=b"not a font")
    assert not doc.is_modified
    applied = doc.apply_xfdf(xfdf, fonts={}, default_font=None)
    assert applied["characters_replaced"] == 1


def test_a_drawn_signature_is_written_and_drawn():
    xfdf = sealwax.xfdf_for([
        {"kind": "ink", "page_index": 0, "name": "i1", "strokes": [[(100, 200), (120, 230), (150, 215)], [(160, 205)]],
         "width": 1.5, "custom_data": {"trn-signature-type": "ink"}},
    ])
    assert "<ink " in xfdf and "<gesture>" in xfdf
    doc = sealwax.Document.from_bytes(text_page())
    applied = doc.apply_xfdf(xfdf)
    assert (applied["inks"], applied["pictures"], applied["texts"]) == (1, 0, 0)
    with pytest.raises(ValueError):
        sealwax.xfdf_for([{"kind": "ink", "page_index": 0, "name": "i1", "strokes": [[(1, 2, 3)]]}])
    with pytest.raises(sealwax.XfdfError):
        sealwax.xfdf_for([{"kind": "ink", "page_index": 0, "name": "i1", "strokes": [[]]}])


def test_merge_names_the_source_that_failed():
    merged = sealwax.merge_bytes([text_page(), text_page(annots="[6 0 R]")])
    assert sealwax.Document.from_bytes(merged).page_count == 2
    with pytest.raises(sealwax.PdfReadError, match="source 2"):
        sealwax.merge_bytes([text_page(), b"not a pdf"])


def test_grayscale_and_optimize_give_pdfs_of_as_many_pages():
    gray = sealwax.grayscale(text_page(), dpi=20)
    assert sealwax.Document.from_bytes(gray).page_count == 1
    optimized = sealwax.optimize(text_page())
    assert sealwax.Document.from_bytes(optimized).page_count == 1
    with pytest.raises(ValueError):
        sealwax.grayscale(text_page(), dpi=0)


def test_pdfa_conversion_reports_its_changes_and_refuses_a_changed_document():
    converted, report = sealwax.pdfa_convert(text_page(), level="3a", language="fr-CA")
    assert report["changes"] and report["residual"] == {}
    assert b"pdfaid" in converted
    doc = sealwax.Document.from_bytes(text_page(annots="[6 0 R]"))
    doc.flatten()
    with pytest.raises(sealwax.PdfAError, match="save and open it again"):
        doc.convert_to_pdfa()
    with pytest.raises(ValueError, match="level"):
        sealwax.pdfa_convert(text_page(), level="2b")
    with pytest.raises(ValueError, match="language"):
        sealwax.pdfa_convert(text_page(), language="")


def test_limits_refuse_with_the_bound_named():
    limits = sealwax.Limits(max_operators=3)
    assert limits.max_operators == 3 and limits.max_page_cost == sealwax.Limits().max_page_cost
    with pytest.raises(sealwax.PdfLimitError) as raised:
        sealwax.pdfa_convert(text_page(b"q q q Q Q Q"), limits=limits)
    assert raised.value.limit == "max_operators"
    assert isinstance(raised.value, sealwax.PdfReadError)
    # A bomb: 64 MiB of spaces in 64 KiB, refused before anything decodes it.
    bomb = zlib.compress(b" " * (64 << 20), 9)
    data = build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
        stream("/Filter /FlateDecode", bomb),
    ])
    with pytest.raises(sealwax.PdfLimitError) as raised:
        sealwax.grayscale(data, limits=sealwax.Limits(max_stream_bytes=1 << 20))
    assert raised.value.limit == "max_stream_bytes"
    peaks = sealwax.Document.from_bytes(data).check_pages()
    assert peaks["stream_bytes"] == 64 << 20
    assert sealwax.Limits.unbounded(timeout=5).timeout == 5
    with pytest.raises(ValueError):
        sealwax.Limits(timeout=0)


def test_a_passed_deadline_is_a_timeout():
    pages = 300
    kids = " ".join(f"{3 + i} 0 R" for i in range(pages))
    objects = ["<< /Type /Catalog /Pages 2 0 R >>", f"<< /Type /Pages /Kids [{kids}] /Count {pages} >>"]
    objects += [f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {3 + pages} 0 R >>"] * pages
    objects.append(stream("", b"0 0 m 600 780 l S " * 2000))
    with pytest.raises(sealwax.PdfTimeoutError) as raised:
        sealwax.Document.from_bytes(build(objects), limits=sealwax.Limits(timeout=1e-9))
    assert raised.value.limit == "deadline"
    assert isinstance(raised.value, sealwax.PdfLimitError)
    # A deadline that passes once the document is open stops what comes next.
    doc = sealwax.Document.from_bytes(build(objects), limits=sealwax.Limits(timeout=0.5))
    time.sleep(0.6)
    with pytest.raises(sealwax.PdfTimeoutError):
        doc.check_pages()
