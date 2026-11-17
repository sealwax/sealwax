"""What goes with a flattening another engine makes: boxes on the pages, the form taken out after,
and a redacted copy rebuilt of its pages alone."""

from conftest import build, stream

import sealwax


def legal_tree() -> bytes:
    return build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 1008] /Rotate 90 >>",
        "<< /Type /Page /Parent 2 0 R >>",
    ])


def widget_page(flags: int, redact: bool = False) -> bytes:
    mark = " 7 0 R" if redact else ""
    return build([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 6 0 R /PieceInfo 5 0 R /Annots [4 0 R{mark}] >>",
        f"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (FIELDVALUE) /Rect [10 10 100 30] /F {flags} >>",
        stream("", b"OLDPAGE"),
        stream("", b"BT (PAGECONTENT) Tj ET"),
        "<< /Type /Annot /Subtype /Redact /Rect [0 0 10 10] >>",
    ])


def test_inherited_boxes_are_written_on_the_pages():
    document = sealwax.Document.from_bytes(legal_tree())
    assert document.own_page_boxes() == 1
    saved = sealwax.Document.from_bytes(document.save(xref="table"))
    assert saved.geometry()[0].media_box == (0, 0, 612, 1008)
    assert saved.geometry()[0].rotation == 90
    assert saved.own_page_boxes() == 0


def test_the_form_left_after_a_flattening_goes_and_a_visible_widget_is_refused():
    document = sealwax.Document.from_bytes(widget_page(2))
    assert document.clear_flattened_form() == 1
    assert b"FIELDVALUE" not in document.save(xref="table")
    visible = sealwax.Document.from_bytes(widget_page(4))
    try:
        visible.clear_flattened_form()
    except sealwax.PdfError as error:
        assert "page 1" in str(error)
    else:
        raise AssertionError("a visible widget was taken for flattened")
    assert not visible.is_modified


def test_a_redacted_copy_is_rebuilt_of_its_pages_alone():
    assert sealwax.has_redaction_marks(widget_page(4, redact=True))
    assert not sealwax.has_redaction_marks(widget_page(4))
    assert sealwax.has_redaction_marks(b"not a pdf"), "an unreadable PDF counts as marked"
    document = sealwax.Document.from_bytes(widget_page(4, redact=True))
    assert document.has_redaction_marks()
    rebuilt = sealwax.Document.from_bytes(document.rebuilt_for_redaction())
    saved = rebuilt.save(xref="table")
    assert b"FIELDVALUE" not in saved and b"OLDPAGE" not in saved
    assert rebuilt.fields() == []


def test_redaction_marks_applied_leave_nothing_under_them():
    document = sealwax.Document.from_bytes(build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R "
        "/Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>",
        stream("", b"BT /F1 12 Tf 72 700 Td (Hello secret world) Tj ET 0 0 1 rg 300 300 50 50 re f"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        "<< /Type /Annot /Subtype /Redact /Rect [104 690 134 715] /IC [0 0 0] >>",
    ]))
    applied = document.apply_redactions(paths="covered")
    assert (applied["marks"], applied["redacted"]["glyphs"]) == (1, 6)
    rebuilt = sealwax.Document.from_bytes(document.rebuilt_for_redaction())
    assert "secret" not in "".join(g["text"] or "" for g in rebuilt.glyphs(0))
    # Asked directly: the blue box goes.
    done = rebuilt.redact(0, [(290, 290, 360, 360)])
    assert done["paths"] == 1
    try:
        rebuilt.redact(0, [(0, 0, 1, 1)], images="blur")
    except ValueError:
        pass
    else:
        raise AssertionError("an unknown option was accepted")
