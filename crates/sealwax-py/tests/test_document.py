import pytest

import sealwax


def make_pdf(page_entries: str, pages_entries: str = "", trailer: str = "") -> bytes:
    """A one-page PDF with an exact xref table."""
    objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        f"<< /Type /Pages /Kids [3 0 R] /Count 1 {pages_entries} >>",
        f"<< /Type /Page /Parent 2 0 R {page_entries} >>",
    ]
    out = bytearray(b"%PDF-1.7\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += f"{number} 0 obj\n{body}\nendobj\n".encode()
    xref_at = len(out)
    out += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    for offset in offsets:
        out += f"{offset:010} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R {trailer} >>\nstartxref\n{xref_at}\n%%EOF\n".encode()
    return bytes(out)


def test_geometry_matches_the_core_pdf_contract():
    doc = sealwax.Document.from_bytes(
        make_pdf("/CropBox [-10 -10 700 900] /Rotate -90 /UserUnit 2", "/MediaBox [0 0 612 792]"),
        page_count=(1, 1000),
    )
    assert doc.page_count == 1
    [page] = doc.geometry()
    assert page.page_number == 1
    assert page.media_box == (0.0, 0.0, 612.0, 792.0)
    assert page.crop_box == (0.0, 0.0, 612.0, 792.0)
    assert page.rotation == 270
    assert page.user_unit == 2.0


def test_page_count_outside_the_range_raises_pdf_read_error():
    with pytest.raises(sealwax.PdfReadError, match="PDF has 1 pages, accepted: 2 to 1000"):
        sealwax.Document.from_bytes(make_pdf("/MediaBox [0 0 1 1]"), page_count=(2, 1000))


def test_garbage_raises_pdf_read_error_which_is_a_pdf_error():
    with pytest.raises(sealwax.PdfError):
        sealwax.Document.from_bytes(b"not a pdf")
    assert issubclass(sealwax.PdfReadError, sealwax.PdfError)


def test_a_password_protected_pdf_raises_its_own_error():
    # RC4 40-bit: no password matches this /U, the empty one included. The API refuses such a
    # file at upload and tells it apart by its class, not by its message.
    encrypt = (
        f"/Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 /P -4 /O <{'AB' * 32}> /U <{'AB' * 32}> >> "
        f"/ID [<{'00' * 16}> <{'00' * 16}>]"
    )
    with pytest.raises(sealwax.PdfPasswordError):
        sealwax.read_geometry(make_pdf("/MediaBox [0 0 1 1]", trailer=encrypt), max_pages=1)
    assert issubclass(sealwax.PdfPasswordError, sealwax.PdfReadError)


@pytest.mark.parametrize("xref", ["stream", "table"])
def test_save_rewrites_a_document_that_reads_back_the_same(xref):
    original = make_pdf("/MediaBox [0 0 595 842] /Rotate 90")
    saved = sealwax.Document.from_bytes(original).save(xref=xref)
    assert isinstance(saved, bytes)
    assert saved.startswith(b"%PDF-1.7\n")
    assert (b"/Type /XRef" in saved) == (xref == "stream")
    [page] = sealwax.read_geometry(saved, max_pages=1)
    assert (page.media_box, page.rotation) == ((0.0, 0.0, 595.0, 842.0), 90)


def test_save_refuses_an_unknown_xref_format():
    with pytest.raises(ValueError, match="xref must be"):
        sealwax.Document.from_bytes(make_pdf("")).save(xref="linearized")


def test_open_reads_a_path(tmp_path):
    path = tmp_path / "in.pdf"
    path.write_bytes(make_pdf("/MediaBox [0 0 100 200]"))
    assert sealwax.Document.open(path).geometry()[0].media_box == (0.0, 0.0, 100.0, 200.0)


# The module-level functions take bytes or a path and give what a caller usually needs.


def test_count_takes_a_path_or_a_string(tmp_path):
    path = tmp_path / "in.pdf"
    path.write_bytes(make_pdf("/MediaBox [0 0 100 200]"))
    assert sealwax.count(path) == 1
    assert sealwax.count(str(path)) == 1


def test_read_geometry_takes_bytes_and_a_page_ceiling():
    [page] = sealwax.read_geometry(make_pdf("/MediaBox [0 0 595 842] /Rotate 90"), max_pages=1000)
    assert (page.page_number, page.media_box, page.crop_box, page.rotation, page.user_unit) == (
        1,
        (0.0, 0.0, 595.0, 842.0),
        (0.0, 0.0, 595.0, 842.0),
        90,
        1.0,
    )


def test_read_geometry_refuses_more_pages_than_the_ceiling():
    with pytest.raises(sealwax.PdfReadError, match="accepted: 1 to 0"):
        sealwax.read_geometry(make_pdf("/MediaBox [0 0 1 1]"), max_pages=0)


def test_a_page_without_media_box_is_letter():
    assert sealwax.read_geometry(make_pdf(""), max_pages=1)[0].media_box == (0.0, 0.0, 612.0, 792.0)
