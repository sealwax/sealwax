"""Pages turned, reordered, taken out, inserted and merged, the form kept."""

from pathlib import Path

from conftest import build, stream

import sealwax


def form(marker: str = "x") -> bytes:
    """Three pages; field `name` has a widget on pages 1 and 2, field `city` one on page 3."""
    widget = "<< /Type /Annot /Subtype /Widget /Parent {} 0 R /P {} 0 R /Rect [10 10 110 30] /F 4 >>"
    page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents {} 0 R /Annots [{} 0 R] >>"
    return build([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [9 0 R 12 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
        page.format(6, 10),
        page.format(7, 11),
        page.format(8, 13),
        stream("", f"BT ({marker}0) Tj ET".encode()),
        stream("", f"BT ({marker}1) Tj ET".encode()),
        stream("", f"BT ({marker}2) Tj ET".encode()),
        "<< /FT /Tx /T (name) /Kids [10 0 R 11 0 R] >>",
        widget.format(9, 3),
        widget.format(9, 4),
        "<< /FT /Tx /T (city) /Kids [13 0 R] >>",
        widget.format(12, 5),
    ])


def test_pages_are_arranged_turned_and_taken_out_with_their_fields():
    document = sealwax.Document.from_bytes(form())
    arranged = sealwax.Document.from_bytes(document.arrange([(2, 1), 0]))
    assert arranged.page_count == 2
    assert [g.rotation for g in arranged.geometry()] == [90, 0]
    assert [(f["name"], f["page_number"]) for f in arranged.fields()] == [("city", 1), ("name", 2)]
    assert arranged.fill_fields({"name": "Zoé"})["occurrences"] == 1
    for pages in ([], [5], [0, 0]):
        try:
            document.arrange(pages)
        except ValueError:
            pass
        else:
            raise AssertionError(f"{pages} accepted")


def test_documents_are_inserted_and_merged_with_their_forms_renamed():
    document = sealwax.Document.from_bytes(form("a"))
    inserted = sealwax.Document.from_bytes(document.insert_pages(1, [form("b")]))
    assert inserted.page_count == 6
    assert [f["name"] for f in inserted.fields()] == ["name", "name (2)", "name (2)", "city (2)", "name", "city"]
    merged = sealwax.Document.from_bytes(sealwax.merge_bytes([form(), form()], flatten=False))
    assert sorted({f["name"] for f in merged.fields()}) == ["city", "city (2)", "name", "name (2)"]
    flattened = sealwax.Document.from_bytes(sealwax.merge_bytes([form(), form()]))
    assert flattened.fields() == []


PHOTOS = Path(__file__).resolve().parents[2] / "sealwax-annots" / "tests" / "data"


def test_photos_become_pages_turned_as_they_show():
    jpeg = (PHOTOS / "photo-40x20-exif6.jpg").read_bytes()
    png = (PHOTOS / "photo-12x10-alpha.png").read_bytes()
    data, unreadable = sealwax.photos_to_pdf([jpeg, b"not a photo", png])
    assert unreadable == [1]
    document = sealwax.Document.from_bytes(data)
    assert [page.rotation for page in document.geometry()] == [90, 0]
    report = document.add_photo_pages([png, b""])
    assert report == {"added": 1, "unreadable": [1]}
    assert sealwax.Document.from_bytes(document.save()).page_count == 3
    try:
        sealwax.photos_to_pdf([b"nothing"])
    except ValueError:
        pass
    else:
        raise AssertionError("no photo read made a document")


def test_pages_turn_in_place_and_encrypted_documents_are_refused_when_asked():
    document = sealwax.Document.from_bytes(build([
        "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 600 800] >>",
        "<< /Type /Page /Parent 2 0 R >>",
        "<< /Type /Page /Parent 2 0 R >>",
        "<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>",
        "<< /Title (Chapter) /Parent 5 0 R /Dest [3 0 R /Fit] >>",
    ]))
    document.rotate_pages([(0, 1), (1, -1)])
    saved = sealwax.Document.from_bytes(document.save())
    assert [page.rotation for page in saved.geometry()] == [90, 270]
    assert saved.outline()[0]["title"] == "Chapter"
    try:
        document.rotate_pages([(5, 1)])
    except ValueError:
        pass
    else:
        raise AssertionError("a page the document does not have was turned")
    protected = (PHOTOS.parents[2] / "sealwax-core" / "tests" / "data" / "owner-protected.pdf").read_bytes()
    assert sealwax.Document.from_bytes(protected).page_count == 1
    for refused in (
        lambda: sealwax.Document.from_bytes(protected, refuse_encrypted=True),
        lambda: sealwax.merge_bytes([protected], refuse_encrypted=True),
        lambda: saved.insert_pages(0, [protected], refuse_encrypted=True),
    ):
        try:
            refused()
        except sealwax.PdfEncryptedError as error:
            assert isinstance(error, sealwax.PdfReadError)
        else:
            raise AssertionError("an encrypted document was accepted")
