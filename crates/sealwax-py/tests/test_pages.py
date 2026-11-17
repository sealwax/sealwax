"""Pages turned, reordered, taken out, inserted and merged, the form kept."""

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
