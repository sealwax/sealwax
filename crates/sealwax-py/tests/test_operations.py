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
