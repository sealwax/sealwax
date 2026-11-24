//! Other formats converted to PDF from Python: DOCX, laid out by docxide-pdf
//! (`sealwax-convert`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

use crate::{Failure, Limits, PdfError, pdf_error, run};

/// `docx_content` (a Word document) converted to PDF, and a report: `skipped`, what the PDF
/// could not hold, and `not_described`, what the layout drew that sealwax does not take yet,
/// counts by kind; `missing_fonts`, the fonts the document asks for that no face provides, drawn
/// with another. Fonts are looked for in `fonts` (font files' contents, all faces of a
/// collection) first, then in `font_dirs`, then in the system's; data without a usable face is
/// ignored. Raises ValueError when the document cannot be laid out; PdfLimitError past a bound
/// of `limits`, `max_unpacked_bytes` (what the document unpacks to) checked before the layout;
/// PdfTimeoutError past its timeout. The layout's own time is not bounded: convert documents you
/// do not trust with `sealwax.isolated`.
#[pyfunction]
#[pyo3(signature = (docx_content, *, font_dirs=None, fonts=None, limits=None))]
fn docx_to_pdf<'py>(
    py: Python<'py>,
    docx_content: Vec<u8>,
    font_dirs: Option<Vec<PathBuf>>,
    fonts: Option<Vec<Vec<u8>>>,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<(Bound<'py, PyBytes>, Bound<'py, PyDict>)> {
    let bounds = limits
        .as_deref()
        .map_or_else(pdf::Limits::default, Limits::counted_from_now);
    let (data, skipped, not_described, missing) = run(py, || {
        // The fonts are parsed here, with the interpreter lock released.
        let mut options =
            sealwax_convert::DocxOptions::default().font_dirs(font_dirs.unwrap_or_default());
        for font in fonts.unwrap_or_default() {
            options = options.font(font);
        }
        let converted = sealwax_convert::docx_to_pdf(&docx_content, &options, &bounds)?;
        let data = converted
            .composed
            .document
            .save(&pdf::SaveOptions::default())?;
        let skipped: BTreeMap<&'static str, usize> = converted.composed.skipped.iter().collect();
        Ok::<_, Failure>((
            data,
            skipped,
            converted.not_described,
            converted.missing_fonts,
        ))
    })?;
    let report = PyDict::new(py);
    report.set_item("skipped", skipped)?;
    report.set_item("not_described", not_described)?;
    report.set_item("missing_fonts", missing_fonts(py, &missing)?)?;
    Ok((PyBytes::new(py, &data), report))
}

/// Each missing font as a dict: its family, whether bold, whether italic, and the name the
/// document gives it when Word draws this family in its place (None otherwise).
fn missing_fonts<'py>(
    py: Python<'py>,
    fonts: &[sealwax_convert::MissingFont],
) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for font in fonts {
        let entry = PyDict::new(py);
        entry.set_item("name", &font.name)?;
        entry.set_item("bold", font.bold)?;
        entry.set_item("italic", font.italic)?;
        entry.set_item("replaces", font.replaces.as_deref())?;
        list.append(entry)?;
    }
    Ok(list)
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
