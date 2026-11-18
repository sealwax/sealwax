//! Other formats converted to PDF from Python: DOCX, laid out by docxide-pdf
//! (`sealwax-convert`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use crate::{Failure, Limits, PdfError, pdf_error, run};

/// `docx_content` (a Word document) converted to PDF, and what the conversion left out, counts
/// by kind: `skipped`, what the PDF could not hold, and `not_described`, what the layout drew
/// that sealwax does not take yet. Fonts are looked for in `font_dirs` first, then in the
/// system's. Raises ValueError when the document cannot be laid out; PdfLimitError past a bound
/// of `limits`, `max_unpacked_bytes` (what the document unpacks to) checked before the layout;
/// PdfTimeoutError past its timeout. The layout's own time is not bounded: convert documents you
/// do not trust with `sealwax.isolated`.
#[pyfunction]
#[pyo3(signature = (docx_content, *, font_dirs=None, limits=None))]
fn docx_to_pdf<'py>(
    py: Python<'py>,
    docx_content: Vec<u8>,
    font_dirs: Option<Vec<PathBuf>>,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<(Bound<'py, PyBytes>, Bound<'py, PyDict>)> {
    let bounds = limits
        .as_deref()
        .map_or_else(pdf::Limits::default, Limits::counted_from_now);
    let mut options = sealwax_convert::DocxOptions::default();
    options.font_dirs = font_dirs.unwrap_or_default();
    let (data, skipped, not_described) = run(py, || {
        let converted = sealwax_convert::docx_to_pdf(&docx_content, &options, &bounds)?;
        let data = converted
            .composed
            .document
            .save(&pdf::SaveOptions::default())?;
        let skipped: BTreeMap<&'static str, usize> = converted.composed.skipped.iter().collect();
        Ok::<_, Failure>((data, skipped, converted.not_described))
    })?;
    let report = PyDict::new(py);
    report.set_item("skipped", skipped)?;
    report.set_item("not_described", not_described)?;
    Ok((PyBytes::new(py, &data), report))
}

/// The exception of a conversion error: ValueError for a document that cannot be laid out, and
/// the core's exceptions for a bound passed or the deadline.
pub(crate) fn convert_error(py: Python<'_>, error: &sealwax_convert::Error) -> PyErr {
    match error {
        sealwax_convert::Error::Layout => {
            pyo3::exceptions::PyValueError::new_err(error.to_string())
        }
        sealwax_convert::Error::Compose(pdf::compose::Error::Pdf(error)) => pdf_error(py, error),
        _ => PdfError::new_err(error.to_string()),
    }
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(docx_to_pdf, m)?)?;
    Ok(())
}
