//! Python bindings of sealwax: the native module `sealwax._native`, which the package `sealwax`
//! re-exports (`python/sealwax/__init__.py`).
//!
//! Heavy work runs with the interpreter detached, so several threads (task-queue workers) can work in
//! parallel. A panic that escapes the Rust code becomes a `PdfError`, never PyO3's
//! `PanicException`, which `except Exception` does not catch.

mod signing;

use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::Duration;

use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyList, PyString};

create_exception!(
    sealwax,
    PdfError,
    PyException,
    "A PDF operation failed; the message names the cause, never the document's content."
);
create_exception!(
    sealwax,
    PdfReadError,
    PdfError,
    "The PDF cannot be read: malformed, encrypted, pages missing, outside the accepted page \
     count, or past the limits."
);
create_exception!(
    sealwax,
    PdfPasswordError,
    PdfReadError,
    "The PDF is protected by a password that was not given, or not the right one."
);
create_exception!(
    sealwax,
    PdfLimitError,
    PdfReadError,
    "Processing the PDF would pass a bound of its limits. The attribute `limit` \
     names the bound (`max_stream_bytes`, `max_page_cost`...)."
);
create_exception!(
    sealwax,
    PdfTimeoutError,
    PdfLimitError,
    "The deadline of the limits (`timeout`) passed before the operation ended."
);
create_exception!(
    sealwax,
    XfdfError,
    PdfError,
    "The XFDF is not what a signing page writes, or an annotation or picture cannot be placed."
);
create_exception!(
    sealwax,
    FieldNameTakenError,
    PdfError,
    "A field to create is named as one the form already has."
);
create_exception!(
    sealwax,
    PdfAError,
    PdfError,
    "The conversion to PDF/A cannot be made."
);

/// What went wrong in the Rust code, turned into an exception once the interpreter is back.
#[derive(Debug)]
pub(crate) enum Failure {
    Pdf(pdf::Error),
    Annots(pdf::annots::Error),
    Raster(pdf::raster::Error),
    Pdfa(pdf::Error),
    Sign(pdf::sign::Error),
    Value(String),
    /// An exception a Python object raised (a signer, a client), kept as is.
    Python(PyErr),
}

impl From<pdf::Error> for Failure {
    fn from(error: pdf::Error) -> Self {
        Self::Pdf(error)
    }
}

impl From<pdf::annots::Error> for Failure {
    fn from(error: pdf::annots::Error) -> Self {
        Self::Annots(error)
    }
}

impl From<pdf::raster::Error> for Failure {
    fn from(error: pdf::raster::Error) -> Self {
        Self::Raster(error)
    }
}

impl From<pdf::sign::Error> for Failure {
    fn from(error: pdf::sign::Error) -> Self {
        Self::Sign(error)
    }
}

/// The exception of a core error.
fn pdf_error(py: Python<'_>, error: &pdf::Error) -> PyErr {
    let message = error.to_string();
    match error {
        pdf::Error::PasswordRequired => PdfPasswordError::new_err(message),
        pdf::Error::Malformed
        | pdf::Error::PagesMissing { .. }
        | pdf::Error::PageCountOutOfRange { .. }
        | pdf::Error::Io(_) => PdfReadError::new_err(message),
        pdf::Error::LimitExceeded { what } => {
            let exception = PdfLimitError::new_err(message);
            let _ = exception.value(py).setattr("limit", *what);
            exception
        }
        pdf::Error::Timeout => {
            let exception = PdfTimeoutError::new_err(message);
            let _ = exception.value(py).setattr("limit", "deadline");
            exception
        }
        pdf::Error::InvalidArgument { .. } => PyValueError::new_err(message),
        pdf::Error::FieldNameTaken => FieldNameTakenError::new_err(message),
        _ => PdfError::new_err(message),
    }
}

fn annots_error(py: Python<'_>, error: &pdf::annots::Error) -> PyErr {
    match error {
        pdf::annots::Error::Pdf(inner) => pdf_error(py, inner),
        pdf::annots::Error::Source { position, error } => {
            // The class of what went wrong with that source, its position in the message.
            let inner = annots_error(py, error);
            let message = format!("source {position}: {error}");
            let class = inner.get_type(py);
            class
                .call1((message,))
                .map(|value| {
                    if let Ok(limit) = inner.value(py).getattr("limit") {
                        let _ = value.setattr("limit", limit);
                    }
                    PyErr::from_value(value)
                })
                .unwrap_or(inner)
        }
        _ => XfdfError::new_err(error.to_string()),
    }
}

impl Failure {
    pub(crate) fn into_py_err(self, py: Python<'_>) -> PyErr {
        match self {
            Self::Pdf(error) => pdf_error(py, &error),
            Self::Annots(error) => annots_error(py, &error),
            Self::Raster(pdf::raster::Error::Pdf(error)) => pdf_error(py, &error),
            Self::Raster(pdf::raster::Error::Annots(error)) => annots_error(py, &error),
            Self::Raster(error) => PdfError::new_err(error.to_string()),
            Self::Pdfa(error) => match error {
                pdf::Error::Unsupported { .. } | pdf::Error::SaveCheck { .. } => {
                    PdfAError::new_err(error.to_string())
                }
                other => pdf_error(py, &other),
            },
            Self::Sign(error) => signing::sign_error(py, error),
            Self::Value(message) => PyValueError::new_err(message),
            Self::Python(error) => error,
        }
    }
}

/// Run `work` with the interpreter detached; a panic is a `PdfError`.
pub(crate) fn run<T: Send>(
    py: Python<'_>,
    work: impl FnOnce() -> Result<T, Failure> + Send,
) -> PyResult<T> {
    match py.detach(|| catch_unwind(AssertUnwindSafe(work))) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(failure)) => Err(failure.into_py_err(py)),
        Err(_) => Err(PdfError::new_err("internal error")),
    }
}

/// What processing a document may spend, and for how long. Every bound is optional:
/// left out, it keeps its default, calibrated on real documents. `timeout` counts in seconds from
/// the call that opens the document, or from the call of a function given bytes.
#[pyclass(frozen, module = "sealwax")]
#[derive(Debug, Clone)]
struct Limits {
    bounds: pdf::Limits,
    timeout: Option<f64>,
}

#[pymethods]
impl Limits {
    #[new]
    #[pyo3(signature = (
        *,
        timeout=None,
        max_operators=None,
        max_shown_bytes=None,
        max_page_cost=None,
        max_image_pixels=None,
        max_mesh_bytes=None,
        max_code_range=None,
        max_stream_bytes=None,
        min_dash_period=None,
        max_annotations=None,
        max_render_pixels=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        timeout: Option<f64>,
        max_operators: Option<u64>,
        max_shown_bytes: Option<u64>,
        max_page_cost: Option<u64>,
        max_image_pixels: Option<u64>,
        max_mesh_bytes: Option<u64>,
        max_code_range: Option<u64>,
        max_stream_bytes: Option<u64>,
        min_dash_period: Option<f64>,
        max_annotations: Option<u64>,
        max_render_pixels: Option<u64>,
    ) -> PyResult<Self> {
        let mut bounds = pdf::Limits::default();
        let set = |field: &mut u64, value: Option<u64>| {
            if let Some(value) = value {
                *field = value;
            }
        };
        set(&mut bounds.max_operators, max_operators);
        set(&mut bounds.max_shown_bytes, max_shown_bytes);
        set(&mut bounds.max_page_cost, max_page_cost);
        set(&mut bounds.max_image_pixels, max_image_pixels);
        set(&mut bounds.max_mesh_bytes, max_mesh_bytes);
        set(&mut bounds.max_code_range, max_code_range);
        set(&mut bounds.max_stream_bytes, max_stream_bytes);
        set(&mut bounds.max_annotations, max_annotations);
        set(&mut bounds.max_render_pixels, max_render_pixels);
        if let Some(period) = min_dash_period {
            if !(period.is_finite() && period >= 0.0) {
                return Err(PyValueError::new_err(
                    "min_dash_period must be a finite number >= 0",
                ));
            }
            bounds.min_dash_period = period;
        }
        Ok(Self {
            bounds,
            timeout: checked_timeout(timeout)?,
        })
    }

    /// No bound at all but the timeout, if given: for documents the caller produced itself.
    #[staticmethod]
    #[pyo3(signature = (*, timeout=None))]
    fn unbounded(timeout: Option<f64>) -> PyResult<Self> {
        Ok(Self {
            bounds: pdf::Limits::unbounded(),
            timeout: checked_timeout(timeout)?,
        })
    }

    #[getter]
    fn timeout(&self) -> Option<f64> {
        self.timeout
    }

    #[getter]
    fn max_operators(&self) -> u64 {
        self.bounds.max_operators
    }

    #[getter]
    fn max_shown_bytes(&self) -> u64 {
        self.bounds.max_shown_bytes
    }

    #[getter]
    fn max_page_cost(&self) -> u64 {
        self.bounds.max_page_cost
    }

    #[getter]
    fn max_image_pixels(&self) -> u64 {
        self.bounds.max_image_pixels
    }

    #[getter]
    fn max_mesh_bytes(&self) -> u64 {
        self.bounds.max_mesh_bytes
    }

    #[getter]
    fn max_code_range(&self) -> u64 {
        self.bounds.max_code_range
    }

    #[getter]
    fn max_stream_bytes(&self) -> u64 {
        self.bounds.max_stream_bytes
    }

    #[getter]
    fn min_dash_period(&self) -> f64 {
        self.bounds.min_dash_period
    }

    #[getter]
    fn max_annotations(&self) -> u64 {
        self.bounds.max_annotations
    }

    #[getter]
    fn max_render_pixels(&self) -> u64 {
        self.bounds.max_render_pixels
    }

    /// What pickling needs to make the same limits again (`sealwax.isolated` hands them to a
    /// child process).
    fn __getnewargs_ex__<'py>(&self, py: Python<'py>) -> PyResult<((), Bound<'py, PyDict>)> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout", self.timeout)?;
        kwargs.set_item("max_operators", self.bounds.max_operators)?;
        kwargs.set_item("max_shown_bytes", self.bounds.max_shown_bytes)?;
        kwargs.set_item("max_page_cost", self.bounds.max_page_cost)?;
        kwargs.set_item("max_image_pixels", self.bounds.max_image_pixels)?;
        kwargs.set_item("max_mesh_bytes", self.bounds.max_mesh_bytes)?;
        kwargs.set_item("max_code_range", self.bounds.max_code_range)?;
        kwargs.set_item("max_stream_bytes", self.bounds.max_stream_bytes)?;
        kwargs.set_item("min_dash_period", self.bounds.min_dash_period)?;
        kwargs.set_item("max_annotations", self.bounds.max_annotations)?;
        kwargs.set_item("max_render_pixels", self.bounds.max_render_pixels)?;
        Ok(((), kwargs))
    }

    fn __eq__(&self, other: &Self) -> bool {
        self.bounds == other.bounds && self.timeout == other.timeout
    }

    fn __repr__(&self) -> String {
        format!(
            "Limits(timeout={:?}, max_operators={}, max_shown_bytes={}, max_page_cost={}, \
             max_image_pixels={}, max_mesh_bytes={}, max_code_range={}, max_stream_bytes={}, \
             min_dash_period={}, max_annotations={}, max_render_pixels={})",
            self.timeout,
            self.bounds.max_operators,
            self.bounds.max_shown_bytes,
            self.bounds.max_page_cost,
            self.bounds.max_image_pixels,
            self.bounds.max_mesh_bytes,
            self.bounds.max_code_range,
            self.bounds.max_stream_bytes,
            self.bounds.min_dash_period,
            self.bounds.max_annotations,
            self.bounds.max_render_pixels,
        )
    }
}

fn checked_timeout(timeout: Option<f64>) -> PyResult<Option<f64>> {
    match timeout {
        Some(seconds) if !(seconds.is_finite() && seconds > 0.0) => Err(PyValueError::new_err(
            "timeout must be a finite number of seconds > 0",
        )),
        other => Ok(other),
    }
}

impl Limits {
    /// The bounds, the deadline counted from now.
    fn counted_from_now(&self) -> pdf::Limits {
        match self.timeout {
            Some(seconds) => self.bounds.timeout(Duration::from_secs_f64(seconds)),
            None => self.bounds,
        }
    }
}

fn options(
    password: Option<String>,
    page_count: Option<(usize, usize)>,
    limits: Option<&Limits>,
) -> pdf::OpenOptions {
    let mut options = pdf::OpenOptions::default();
    if let Some(password) = password {
        options = options.password(password);
    }
    if let Some((min, max)) = page_count {
        options = options.page_count(min..=max);
    }
    if let Some(limits) = limits {
        options = options.limits(limits.counted_from_now());
    }
    options
}

/// The geometry a page declares: user-space points, bottom-left origin.
#[pyclass(frozen, get_all, module = "sealwax")]
#[derive(Debug, Clone)]
struct PageGeometry {
    /// 1-based page number.
    page_number: usize,
    /// (x1, y1, x2, y2), clipped to the media box.
    crop_box: (f64, f64, f64, f64),
    /// (x1, y1, x2, y2).
    media_box: (f64, f64, f64, f64),
    /// 0, 90, 180 or 270.
    rotation: u16,
    /// 1.0 when the page declares none.
    user_unit: f64,
}

#[pymethods]
impl PageGeometry {
    fn __repr__(&self) -> String {
        format!(
            "PageGeometry(page_number={}, crop_box={:?}, media_box={:?}, rotation={}, user_unit={})",
            self.page_number, self.crop_box, self.media_box, self.rotation, self.user_unit
        )
    }
}

impl From<pdf::PageGeometry> for PageGeometry {
    fn from(page: pdf::PageGeometry) -> Self {
        Self {
            page_number: page.page_number,
            crop_box: page.crop_box.to_tuple(),
            media_box: page.media_box.to_tuple(),
            rotation: page.rotation.degrees(),
            user_unit: page.user_unit,
        }
    }
}

fn counts<'py>(py: Python<'py>, map: &BTreeMap<String, usize>) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (key, value) in map {
        dict.set_item(key, value)?;
    }
    Ok(dict)
}

fn flattened_dict<'py>(
    py: Python<'py>,
    flattened: &pdf::annots::Flattened,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("painted", flattened.painted)?;
    dict.set_item("drawn", flattened.drawn)?;
    dict.set_item("links_kept", flattened.links_kept)?;
    dict.set_item("dropped", counts(py, &flattened.dropped)?)?;
    dict.set_item("characters_replaced", flattened.characters_replaced)?;
    Ok(dict)
}

fn applied_dict<'py>(
    py: Python<'py>,
    applied: &pdf::annots::Applied,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("pictures", applied.pictures)?;
    dict.set_item("texts", applied.texts)?;
    dict.set_item("characters_replaced", applied.characters_replaced)?;
    dict.set_item("deletions_ignored", applied.deletions_ignored)?;
    match &applied.flattened {
        Some(flattened) => dict.set_item("flattened", flattened_dict(py, flattened)?)?,
        None => dict.set_item("flattened", py.None())?,
    }
    Ok(dict)
}

fn pdfa_dict<'py>(py: Python<'py>, report: &pdf::pdfa::Report) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("changes", counts(py, report.changes())?)?;
    dict.set_item("residual", counts(py, report.residual())?)?;
    Ok(dict)
}

fn pdfa_options(level: &str, language: Option<String>) -> Result<pdf::pdfa::Options, Failure> {
    let level = match level {
        "3a" | "a" => pdf::pdfa::Level::A,
        "3u" | "u" => pdf::pdfa::Level::U,
        "3b" | "b" => pdf::pdfa::Level::B,
        _ => {
            return Err(Failure::Value(
                "level must be \"3a\", \"3u\" or \"3b\"".to_owned(),
            ));
        }
    };
    let mut options = pdf::pdfa::Options::new(level);
    if let Some(tag) = language {
        options = options.language(tag);
    }
    Ok(options)
}

/// Convert `doc` to PDF/A and save it; `doc` must not have been changed since it was opened.
fn convert_and_save(
    doc: &mut pdf::Document,
    options: &pdf::pdfa::Options,
) -> Result<(Vec<u8>, pdf::pdfa::Report), Failure> {
    let report = pdf::pdfa::convert(doc, options).map_err(Failure::Pdfa)?;
    Ok((doc.save(&pdf::SaveOptions::default())?, report))
}

/// An opened PDF document. What changes it (flattening, XFDF, conversion) changes it in memory;
/// `save` gives the bytes.
#[pyclass(module = "sealwax")]
#[derive(Debug)]
struct Document {
    inner: pdf::Document,
}

#[pymethods]
impl Document {
    /// Open a document from its bytes.
    ///
    /// `page_count` is an inclusive (min, max) range: outside it, PdfReadError is raised.
    /// `limits` bounds what processing it may spend (default: the calibrated bounds, no timeout).
    #[staticmethod]
    #[pyo3(signature = (data, *, password=None, page_count=None, limits=None))]
    fn from_bytes(
        py: Python<'_>,
        data: Vec<u8>,
        password: Option<String>,
        page_count: Option<(usize, usize)>,
        limits: Option<PyRef<'_, Limits>>,
    ) -> PyResult<Self> {
        let options = options(password, page_count, limits.as_deref());
        run(py, || Ok(pdf::Document::open(data, &options)?)).map(|inner| Self { inner })
    }

    /// Open the document stored at `path`.
    #[staticmethod]
    #[pyo3(signature = (path, *, password=None, page_count=None, limits=None))]
    fn open(
        py: Python<'_>,
        path: PathBuf,
        password: Option<String>,
        page_count: Option<(usize, usize)>,
        limits: Option<PyRef<'_, Limits>>,
    ) -> PyResult<Self> {
        let options = options(password, page_count, limits.as_deref());
        run(py, || Ok(pdf::Document::open_path(&path, &options)?)).map(|inner| Self { inner })
    }

    /// Number of pages.
    #[getter]
    fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    /// Whether the document was changed since it was opened.
    #[getter]
    fn is_modified(&self) -> bool {
        self.inner.is_modified()
    }

    /// The geometry of every page, in page order.
    fn geometry(&self, py: Python<'_>) -> PyResult<Vec<PageGeometry>> {
        let inner = &self.inner;
        let pages = run(py, || Ok(inner.geometry()))?;
        Ok(pages.into_iter().map(PageGeometry::from).collect())
    }

    /// The document saved in full: every reachable object rewritten, decrypted and renumbered.
    ///
    /// `xref` is "stream" (cross-reference stream and object streams, the default) or "table".
    #[pyo3(signature = (*, xref="stream"))]
    fn save<'py>(&self, py: Python<'py>, xref: &str) -> PyResult<Bound<'py, PyBytes>> {
        let format = match xref {
            "stream" => pdf::XrefFormat::Stream,
            "table" => pdf::XrefFormat::Table,
            _ => {
                return Err(PyValueError::new_err(
                    "xref must be \"stream\" or \"table\"",
                ));
            }
        };
        let options = pdf::SaveOptions::default().xref(format);
        let inner = &self.inner;
        let saved = run(py, || Ok(inner.save(&options)?))?;
        Ok(PyBytes::new(py, &saved))
    }

    /// The document with what changed appended as an incremental update: the input's bytes
    /// first, unchanged (signatures stay valid).
    fn save_incremental<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let saved = run(py, || Ok(inner.save_incremental()?))?;
        Ok(PyBytes::new(py, &saved))
    }

    /// Flatten every annotation but links into its page, and take the form out; with
    /// `fields_only`, the form's widgets alone, every other annotation (comments, highlights,
    /// links) kept as it is. Returns what was done: `painted`, `drawn`, `links_kept`, `dropped`
    /// (by subtype), `characters_replaced`.
    #[pyo3(signature = (*, fields_only=false))]
    fn flatten<'py>(&mut self, py: Python<'py>, fields_only: bool) -> PyResult<Bound<'py, PyDict>> {
        let inner = &mut self.inner;
        let flattened = run(py, || {
            Ok(if fields_only {
                pdf::annots::flatten_fields(inner)?
            } else {
                pdf::annots::flatten(inner)?
            })
        })?;
        flattened_dict(py, &flattened)
    }

    /// The occurrences of the form's fields on page `page_index` (from 0), every page's when
    /// None: one per widget, a name repeated on several widgets never merged. Each is a dict:
    /// `name` (full name), `label` (`/TU`), `kind` ("text", "checkbox", "radio",
    /// "push_button", "combo_box", "list_box", "signature", "other"), `page_number` (from 1),
    /// `rect` and `crop_box` (x1, y1, x2, y2 in the page's default user space, y up), `value`
    /// (text, a list for several selections, or None), `state` (a button's `/AS`, bytes),
    /// `required`, `read_only`, `max_length`, `choices` (text, or (export, display) pairs),
    /// `button_states` (the names of its appearances, bytes as the document spells them),
    /// `button_caption`.
    #[pyo3(signature = (page_index=None))]
    fn fields<'py>(
        &self,
        py: Python<'py>,
        page_index: Option<usize>,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let inner = &self.inner;
        let found = run(py, || Ok(pdf::annots::fields(inner, page_index)?))?;
        found.iter().map(|field| field_dict(py, field)).collect()
    }

    /// Fill the fields `values` names, by full name, every occurrence of each: a text or choice
    /// field gets the text, and an appearance drawn for it (an empty text clears it). A button
    /// gets the state named, given as bytes exactly as `button_states` spells it or as text; or
    /// the state of its widget whose export value (`/Opt`) it is; `False`, an empty value, "off",
    /// "no", "false", "0" and the like turn it `Off`; `True`, "yes", "on", "true", "1", "x" and
    /// the like check a button that has one state besides `Off` (a check box).
    ///
    /// Returns `occurrences` (written), `characters_replaced` (characters the appearance's font
    /// does not have, drawn as `?`; the value keeps them), `refused` (the names not written, each
    /// field left as it was: no field of that name on the pages, a signature or push button, a
    /// button given a state none of its widgets has, and with `strict` a value the field does not
    /// allow), `kept` (with `keep_filled`, the fields left as they were because they held another
    /// value) and `written` (each field written: `value`, and `previous`, the value it held; a
    /// button's state, `Off` when it has none). `strict` refuses what a reader would not let a
    /// person type: a read-only field, a text longer than its maximum length or with characters
    /// its font cannot draw, a choice the field does not offer.
    #[pyo3(signature = (values, *, keep_filled = false, strict = false))]
    fn fill_fields<'py>(
        &mut self,
        py: Python<'py>,
        values: &Bound<'py, PyDict>,
        keep_filled: bool,
        strict: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let mut wanted = std::collections::HashMap::new();
        for (key, value) in values.iter() {
            let name: String = key.extract()?;
            let value = if let Ok(bytes) = value.cast::<PyBytes>() {
                pdf::annots::FillValue::Bytes(bytes.as_bytes().to_vec())
            } else if let Ok(flag) = value.cast::<PyBool>() {
                // A check box checked or not; a text field gets the word.
                pdf::annots::FillValue::Text(if flag.is_true() { "true" } else { "false" }.into())
            } else if let Ok(text) = value.extract::<String>() {
                pdf::annots::FillValue::Text(text)
            } else {
                return Err(PyValueError::new_err(
                    "each value must be str, bytes or bool",
                ));
            };
            wanted.insert(name, value);
        }
        let options = pdf::annots::FillOptions::default()
            .keep_filled(keep_filled)
            .strict(strict);
        let inner = &mut self.inner;
        let filled = run(py, || {
            Ok(pdf::annots::fill_fields_with(inner, &wanted, &options)?)
        })?;
        let dict = PyDict::new(py);
        dict.set_item("occurrences", filled.occurrences)?;
        dict.set_item("characters_replaced", filled.characters_replaced)?;
        dict.set_item("refused", filled.refused)?;
        dict.set_item("kept", filled.kept)?;
        let written = PyDict::new(py);
        for (name, field) in filled.written {
            let entry = PyDict::new(py);
            entry.set_item("value", field.value)?;
            entry.set_item("previous", field.previous)?;
            written.set_item(name, entry)?;
        }
        dict.set_item("written", written)?;
        Ok(dict)
    }

    /// Undo a filling: each field `written` names (as `fill_fields` returned it) gets back the
    /// value it held before, unless it was changed since or refuses it. Returns those, left as
    /// they are; a name the form no longer has is passed over.
    fn unfill_fields(
        &mut self,
        py: Python<'_>,
        written: std::collections::BTreeMap<String, std::collections::HashMap<String, String>>,
    ) -> PyResult<Vec<String>> {
        let mut fields = std::collections::BTreeMap::new();
        for (name, field) in written {
            let (Some(value), Some(previous)) = (field.get("value"), field.get("previous")) else {
                return Err(PyValueError::new_err(
                    "each field written needs its value and previous",
                ));
            };
            fields.insert(name, pdf::annots::Written::new(value, previous));
        }
        let inner = &mut self.inner;
        run(py, || Ok(pdf::annots::unfill_fields(inner, &fields)?))
    }

    /// Make the document a template: every field cleared with its empty appearance drawn, the
    /// XFA form taken out, the annotations a person added over the pages taken out (markup and
    /// their pop-ups; links and widgets stay) unless `keep_annotations`, the information's title
    /// set to `title`, its author, subject and keywords and the XMP metadata taken out. What was
    /// replaced is left out of a full `save()`. Returns `fields` (cleared) and
    /// `annotations_removed`. What the pages' content draws is left as it is.
    #[pyo3(signature = (title, *, keep_annotations = false))]
    fn clear_template<'py>(
        &mut self,
        py: Python<'py>,
        title: &str,
        keep_annotations: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let options = pdf::annots::TemplateOptions::new(title).keep_annotations(keep_annotations);
        let inner = &mut self.inner;
        let cleared = run(py, || Ok(pdf::annots::clear_template(inner, &options)?))?;
        let dict = PyDict::new(py);
        dict.set_item("fields", cleared.fields)?;
        dict.set_item("annotations_removed", cleared.annotations_removed)?;
        Ok(dict)
    }

    /// What `clear_template` would clear or take out: `pages`, `fields` (each name once) and
    /// `annotations` (added by a person over the pages).
    fn template_content<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let inner = &self.inner;
        let content = run(py, || Ok(pdf::annots::template_content(inner)?))?;
        let dict = PyDict::new(py);
        dict.set_item("pages", content.pages)?;
        dict.set_item("fields", content.fields)?;
        dict.set_item("annotations", content.annotations)?;
        Ok(dict)
    }

    /// Add an annotation to page `page_index` (from 0), its appearance drawn, in the page's
    /// default user space (unrotated, y up): `kind` "ink" (`strokes`, lists of points),
    /// "highlight", "underline", "strikeout" or "squiggly" (`quads`: per line the corners
    /// top-left, top-right, bottom-left, bottom-right, eight numbers), "square" or "circle"
    /// (`rect`), "line" (`points`, its two ends), "polygon" or "polyline" (`points`), "freetext"
    /// (`rect`, `text`, `size`). `color` (RGB from 0 to 1) draws the strokes, markup, border and
    /// text, black by default and yellow for a highlight; `fill` the inside of a square,
    /// circle or polygon, or a free text's background; `width` the strokes and borders (0: no
    /// border); `opacity` from 0 to 1. `ValueError` for a missing or bad argument.
    #[pyo3(signature = (
        page_index, kind, *, quads = None, strokes = None, points = None, rect = None,
        text = None, size = 12.0, color = None, fill = None, width = 1.0, opacity = 1.0,
        author = None, contents = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn add_annotation(
        &mut self,
        py: Python<'_>,
        page_index: usize,
        kind: &str,
        quads: Option<Vec<[f64; 8]>>,
        strokes: Option<Vec<Vec<(f64, f64)>>>,
        points: Option<Vec<(f64, f64)>>,
        rect: Option<(f64, f64, f64, f64)>,
        text: Option<String>,
        size: f64,
        color: Option<(f64, f64, f64)>,
        fill: Option<(f64, f64, f64)>,
        width: f64,
        opacity: f64,
        author: Option<String>,
        contents: Option<String>,
    ) -> PyResult<()> {
        use pdf::annots::AnnotationKind as Kind;
        let missing = |what: &str| PyValueError::new_err(format!("a {kind} needs {what}"));
        let pairs = |list: Vec<(f64, f64)>| -> Vec<[f64; 2]> {
            list.into_iter().map(|(x, y)| [x, y]).collect()
        };
        let rect = rect.map(|(x1, y1, x2, y2)| [x1, y1, x2, y2]);
        let kind = match kind {
            "ink" => Kind::Ink {
                strokes: strokes
                    .ok_or_else(|| missing("strokes"))?
                    .into_iter()
                    .map(pairs)
                    .collect(),
            },
            "highlight" => Kind::Highlight {
                quads: quads.ok_or_else(|| missing("quads"))?,
            },
            "underline" => Kind::Underline {
                quads: quads.ok_or_else(|| missing("quads"))?,
            },
            "strikeout" => Kind::StrikeOut {
                quads: quads.ok_or_else(|| missing("quads"))?,
            },
            "squiggly" => Kind::Squiggly {
                quads: quads.ok_or_else(|| missing("quads"))?,
            },
            "square" => Kind::Square {
                rect: rect.ok_or_else(|| missing("a rect"))?,
            },
            "circle" => Kind::Circle {
                rect: rect.ok_or_else(|| missing("a rect"))?,
            },
            "line" => match pairs(points.ok_or_else(|| missing("points"))?).as_slice() {
                [from, to] => Kind::Line {
                    from: *from,
                    to: *to,
                },
                _ => return Err(PyValueError::new_err("a line needs two points")),
            },
            "polygon" => Kind::Polygon {
                points: pairs(points.ok_or_else(|| missing("points"))?),
            },
            "polyline" => Kind::PolyLine {
                points: pairs(points.ok_or_else(|| missing("points"))?),
            },
            "freetext" => Kind::FreeText {
                rect: rect.ok_or_else(|| missing("a rect"))?,
                text: text.ok_or_else(|| missing("text"))?,
                size,
            },
            _ => return Err(PyValueError::new_err("unknown annotation kind")),
        };
        let mut annotation = pdf::annots::NewAnnotation::new(page_index, kind)
            .fill(fill.map(|(r, g, b)| [r, g, b]))
            .width(width)
            .opacity(opacity)
            .author(author)
            .contents(contents);
        if let Some((r, g, b)) = color {
            annotation = annotation.color([r, g, b]);
        }
        let inner = &mut self.inner;
        run(py, || {
            Ok(pdf::annots::add_annotation(inner, &annotation).map(|_| ())?)
        })
    }

    /// Create a field on page `page_index` (from 0): `kind` "text", "date" (a text field
    /// carrying Acrobat's `yyyy-mm-dd` date actions) or "checkbox" (states `Yes` and `Off`),
    /// named `name` (not empty, no period), at `rect` (x1, y1, x2, y2) on the page as it shows:
    /// turned by its rotation, from the top-left corner of its crop box, y down, in points.
    /// It gets a gray border one point wide and no background, and turns with the page so that
    /// its text reads upright. The form is created if the document has none; the fields it has
    /// are left as they are. `FieldNameTakenError` when the form already has a field of that
    /// name; `ValueError` for a bad name, page or rectangle.
    #[pyo3(signature = (name, kind, page_index, rect, *, required = false))]
    fn add_field(
        &mut self,
        py: Python<'_>,
        name: &str,
        kind: &str,
        page_index: usize,
        rect: (f64, f64, f64, f64),
        required: bool,
    ) -> PyResult<()> {
        let kind = match kind {
            "text" => pdf::annots::NewFieldKind::Text,
            "date" => pdf::annots::NewFieldKind::Date,
            "checkbox" => pdf::annots::NewFieldKind::CheckBox,
            _ => {
                return Err(PyValueError::new_err(
                    "kind must be \"text\", \"date\" or \"checkbox\"",
                ));
            }
        };
        let field =
            pdf::annots::NewField::new(name, kind, page_index, [rect.0, rect.1, rect.2, rect.3])
                .required(required);
        let inner = &mut self.inner;
        run(
            py,
            || Ok(pdf::annots::add_field(inner, &field).map(|_| ())?),
        )
    }

    /// Add a page for each photo of `photos` (JPEG or PNG bytes), in order, after the document's
    /// pages: a US Letter sheet, portrait or landscape as the photo shows, half an inch of margin,
    /// the photo centered and fitted without being stretched. A JPEG is embedded as it is, never
    /// encoded again; a PNG keeps its transparency; the orientation a phone writes in a JPEG's
    /// EXIF data becomes the page's rotation. Returns `added` (pages) and `unreadable` (the
    /// photos, from 0, that could not be read: not a JPEG or a PNG, damaged, or past the limits'
    /// `max_image_pixels`).
    fn add_photo_pages<'py>(
        &mut self,
        py: Python<'py>,
        photos: Vec<Vec<u8>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let inner = &mut self.inner;
        let report = run(py, || {
            let borrowed: Vec<&[u8]> = photos.iter().map(Vec::as_slice).collect();
            Ok(pdf::annots::add_photo_pages(inner, &borrowed)?)
        })?;
        let dict = PyDict::new(py);
        dict.set_item("added", report.added)?;
        dict.set_item("unreadable", report.unreadable)?;
        Ok(dict)
    }

    /// Page `page_index` (from 0) rendered as a reader shows it (annotations and field values
    /// included), as PNG bytes: 8-bit RGB without alpha, over white. The image covers the crop
    /// box as shown (rotated, then turned `quarter_turns` more quarter turns clockwise), at
    /// `scale` pixels per point (150 / 72 for 150 dpi): its sides are the shown crop box's times
    /// `scale`, rounded up as PyMuPDF and PDFium size their images. A document modified since it
    /// was opened (a field filled) is rendered as it stands, saved and read again once until it
    /// changes again. Past `Limits.max_render_pixels` (or a side of 65,535 pixels),
    /// `PdfLimitError` with `limit` "max_render_pixels"; past the deadline, `PdfTimeoutError`;
    /// `ValueError` for a scale that is not a positive number or a page the document does not
    /// have.
    #[pyo3(signature = (page_index, scale, quarter_turns = 0))]
    fn render_png<'py>(
        &self,
        py: Python<'py>,
        page_index: usize,
        scale: f64,
        quarter_turns: i32,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let png = run(py, || {
            let options = pdf::raster::RenderOptions::new(scale).quarter_turns(quarter_turns);
            match pdf::raster::render(inner, page_index, &options, &|| false)? {
                Some(raster) => Ok(raster.png()?),
                None => Err(Failure::Value("rendering cancelled".to_owned())),
            }
        })?;
        Ok(PyBytes::new(py, &png))
    }

    /// Page `page_index` rendered as `render_png` renders it, as raw pixels: `(width, height,
    /// pixels)`, `pixels` holding 4 bytes per pixel (red, green, blue, alpha), row by row from
    /// the top, opaque over white.
    #[pyo3(signature = (page_index, scale, quarter_turns = 0))]
    fn render_rgba<'py>(
        &self,
        py: Python<'py>,
        page_index: usize,
        scale: f64,
        quarter_turns: i32,
    ) -> PyResult<(u32, u32, Bound<'py, PyBytes>)> {
        let inner = &self.inner;
        let raster = run(py, || {
            let options = pdf::raster::RenderOptions::new(scale).quarter_turns(quarter_turns);
            pdf::raster::render(inner, page_index, &options, &|| false)?
                .ok_or_else(|| Failure::Value("rendering cancelled".to_owned()))
        })?;
        Ok((
            raster.width,
            raster.height,
            PyBytes::new(py, &raster.pixels),
        ))
    }

    /// The straight lines and rectangles page `page_index` (from 0) paints (filled or stroked;
    /// forms included), in its default user space (unrotated, y up): `lines`, each segment of a
    /// path as `(x0, y0, x1, y1)`, the side that closes a subpath included; `rectangles`, each
    /// rectangle whose sides stay along the axes as `(x0, y0, x1, y1)`; `quads`, each rectangle
    /// the transformation turns off the axes, as its four corners. With `annotations`, what
    /// annotations' appearances paint follows (field borders and check boxes among them), as
    /// in MuPDF's drawings.
    #[pyo3(signature = (page_index, annotations = false))]
    fn drawings<'py>(
        &self,
        py: Python<'py>,
        page_index: usize,
        annotations: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let inner = &self.inner;
        let found = run(py, || {
            Ok(pdf::text::drawings(inner, page_index, annotations)?)
        })?;
        let dict = PyDict::new(py);
        let lines: Vec<(f64, f64, f64, f64)> = found
            .lines
            .iter()
            .map(|[a, b, c, d]| (*a, *b, *c, *d))
            .collect();
        let rectangles: Vec<(f64, f64, f64, f64)> = found
            .rectangles
            .iter()
            .map(|[a, b, c, d]| (*a, *b, *c, *d))
            .collect();
        let quads: Vec<[(f64, f64); 4]> = found
            .quads
            .iter()
            .map(|corners| corners.map(|[x, y]| (x, y)))
            .collect();
        dict.set_item("lines", lines)?;
        dict.set_item("rectangles", rectangles)?;
        dict.set_item("quads", quads)?;
        Ok(dict)
    }

    /// The outline (bookmarks), flat and depth first: each entry a dict with `level` (from 1),
    /// `title` (decoded) and `page_number` (from 1), None when the entry leads to no page of this
    /// document (another kind of action, a destination missing or broken): never a guess.
    /// Named destinations are resolved; a cyclic outline ends where it loops.
    fn outline<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let inner = &self.inner;
        let entries = run(py, || Ok(inner.outline()?))?;
        entries
            .into_iter()
            .map(|entry| {
                let dict = PyDict::new(py);
                dict.set_item("level", entry.level)?;
                dict.set_item("title", entry.title)?;
                dict.set_item("page_number", entry.page_index.map(|index| index + 1))?;
                Ok(dict)
            })
            .collect()
    }

    /// What the document says about itself, never failing (an entry that cannot be read is left
    /// out): `version` ("1.7"), `encrypted` (also when no password is needed to open it),
    /// `linearized`, `has_forms` (a field at least), `has_signatures` (a signature field, signed
    /// or not), `pdfa` (the part and conformance its XMP claims, "3A", or None), and `info`, the
    /// document information's text entries (`Title`, `Author`, `CreationDate` as written...).
    fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let inner = &self.inner;
        let metadata = py.detach(|| inner.metadata());
        let dict = PyDict::new(py);
        dict.set_item("version", metadata.version)?;
        dict.set_item("encrypted", metadata.encrypted)?;
        dict.set_item("linearized", metadata.linearized)?;
        dict.set_item("has_forms", metadata.has_forms)?;
        dict.set_item("has_signatures", metadata.has_signatures)?;
        dict.set_item("pdfa", metadata.pdfa)?;
        dict.set_item("info", metadata.info)?;
        Ok(dict)
    }

    /// Pages `[start, stop)` (from 0; at least one) in a new PDF, saved compressed: each page with
    /// its contents, resources and annotations (links kept; one to a page left out leads
    /// nowhere). The outline, the form and the structure tree are left behind. `ValueError` for
    /// a range the document does not have.
    fn extract_pages<'py>(
        &self,
        py: Python<'py>,
        start: usize,
        stop: usize,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let data = run(py, || {
            let extracted = inner.extract_pages(start..stop)?;
            let saved = extracted.save(&pdf::SaveOptions::default())?;
            let options = pdf::OpenOptions::default()
                .limits(*inner.limits())
                .max_file_bytes(usize::MAX);
            Ok(pdf::optimize(&saved, &options)?.data)
        })?;
        Ok(PyBytes::new(py, &data))
    }

    /// A new PDF of this document's pages `pages`, in that order, saved compressed: each entry a
    /// page index (from 0) or a (page index, quarter turns) pair, the quarter turns (clockwise;
    /// counterclockwise when negative) added to the page's rotation. Turning, deleting, moving,
    /// reordering and taking out pages are each one arrangement. The form comes along for the
    /// pages kept, each field with its widgets on them; nothing of the pages left out is copied.
    /// `ValueError` for an empty list, a page the document does not have, or a page twice.
    fn arrange<'py>(
        &self,
        py: Python<'py>,
        pages: Vec<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let mut wanted = Vec::with_capacity(pages.len());
        for page in &pages {
            wanted.push(if let Ok(index) = page.extract::<usize>() {
                (index, 0)
            } else if let Ok(pair) = page.extract::<(usize, i32)>() {
                pair
            } else {
                return Err(PyValueError::new_err(
                    "each page is an index or an (index, quarter turns) pair",
                ));
            });
        }
        let inner = &self.inner;
        let data = run(py, || compact(&inner.arrange(&wanted)?, inner.limits()))?;
        Ok(PyBytes::new(py, &data))
    }

    /// A new PDF, saved compressed, of this document's pages with every page of the PDFs
    /// `pdf_contents` inserted before page `at` (from 0; the page count to append), one document
    /// after the other, opened with this document's limits. The forms come along; a field an
    /// inserted document names as an earlier one does is renamed `name (2)`, `name (3)`...
    /// `ValueError` when `at` is past the last page; an error reading an inserted PDF names it
    /// (1-based).
    fn insert_pages<'py>(
        &self,
        py: Python<'py>,
        at: usize,
        pdf_contents: Vec<Vec<u8>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let data = run(py, || {
            let others = open_sources(pdf_contents, inner.limits())?;
            let borrowed: Vec<&pdf::Document> = others.iter().collect();
            compact(&inner.insert(at, &borrowed)?, inner.limits())
        })?;
        Ok(PyBytes::new(py, &data))
    }

    /// Write onto each page the media box, crop box and rotation it inherits from its page tree
    /// and does not hold itself, for an engine that does not look for them there (PDFium's
    /// flattening writes US Letter where a page holds no box). A document whose pages inherit
    /// none is left as it is. Returns the pages changed.
    fn own_page_boxes(&mut self, py: Python<'_>) -> PyResult<usize> {
        let inner = &mut self.inner;
        run(py, || Ok(inner.own_page_boxes()?))
    }

    /// After another engine flattened the form: take out the widgets it left because they are
    /// hidden (`Hidden` or `NoView`), then the form itself, which still holds the values (a full
    /// save leaves out what nothing reaches any more). Returns the widgets taken out. `PdfError`
    /// when a page still holds a visible widget (the flattening did not happen): nothing is
    /// changed then.
    fn clear_flattened_form(&mut self, py: Python<'_>) -> PyResult<usize> {
        let inner = &mut self.inner;
        run(py, || Ok(inner.clear_flattened_form()?))
    }

    /// Take out of page `page_index` (from 0) what it draws under `areas`, rectangles
    /// (x1, y1, x2, y2) in its default user space (unrotated, y up): the glyphs (a glyph whose box
    /// an area overlaps goes whole, nothing after it moves), the images' pixels (`images`
    /// "pixels", or "remove" whole, or "keep"), the paths painted there (`paths` "touched": even
    /// in part, "covered": whole, or "keep"). Form XObjects losing content are copied for the
    /// drawing that loses it. Annotations stay: flatten first. Returns the counts: `glyphs`,
    /// `operators`, `forms`, `strings_unread`, `images`, `images_removed`, `paths`. Follow with
    /// `rebuilt_for_redaction()`: what the pages no longer reach stays in this document.
    #[pyo3(signature = (page_index, areas, *, images = "pixels", paths = "touched"))]
    fn redact<'py>(
        &mut self,
        py: Python<'py>,
        page_index: usize,
        areas: Vec<(f64, f64, f64, f64)>,
        images: &str,
        paths: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let options = redact_options(images, paths)?;
        let areas: Vec<pdf::Rect> = areas
            .into_iter()
            .map(|(x1, y1, x2, y2)| pdf::Rect::from_corners(x1, y1, x2, y2))
            .collect();
        let inner = &mut self.inner;
        let done = run(py, || {
            Ok(pdf::text::redact(inner, page_index, &areas, &options)?)
        })?;
        redacted_dict(py, &done)
    }

    /// Apply every redaction mark (`/Redact`): the other annotations flattened first (links
    /// stay), what the pages draw under the marks taken out as `redact` does, each mark's area
    /// painted with its interior colour and its overlay text written, the marks and the links
    /// under them taken out. Returns `marks`, `pages`, `links_removed` and `redacted` (the
    /// counts of `redact`, all pages together). Follow with `rebuilt_for_redaction()`.
    #[pyo3(signature = (*, images = "pixels", paths = "touched"))]
    fn apply_redactions<'py>(
        &mut self,
        py: Python<'py>,
        images: &str,
        paths: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let options = redact_options(images, paths)?;
        let inner = &mut self.inner;
        let applied = run(py, || Ok(pdf::annots::apply_redactions(inner, &options)?))?;
        let dict = PyDict::new(py);
        dict.set_item("marks", applied.marks)?;
        dict.set_item("pages", applied.pages)?;
        dict.set_item("links_removed", applied.links_removed)?;
        dict.set_item("redacted", redacted_dict(py, &applied.redacted)?)?;
        Ok(dict)
    }

    /// Whether a page holds a redaction mark (a `/Redact` annotation).
    fn has_redaction_marks(&self, py: Python<'_>) -> PyResult<bool> {
        let inner = &self.inner;
        run(py, || Ok(inner.has_redaction_marks()?))
    }

    /// A new PDF, saved compressed, of this document's pages alone, for a copy whose redactions
    /// another engine applied: each page keeps only `/Type`, its boxes, `/Rotate`, `/UserUnit`,
    /// `/Resources`, `/Contents`, `/Annots` and `/Group` (not `/PieceInfo`, `/Thumb`,
    /// `/Metadata`, `/AA`...); the widgets and hidden annotations are taken out; the optional
    /// content is kept, so that a layer turned off stays off; the form, the document
    /// information, the XMP metadata, the outline and the embedded files stay behind.
    fn rebuilt_for_redaction<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let data = run(py, || {
            compact(&inner.rebuilt_for_redaction()?, inner.limits())
        })?;
        Ok(PyBytes::new(py, &data))
    }

    /// The glyphs page `page_index` (from 0) draws, in the order of its content (forms and
    /// Type 3 glyphs included), then with `annotations` those its annotations' appearances draw
    /// (as MuPDF's text includes them), each a dict in the page's default user space
    /// (unrotated, y up; read the crop box from `geometry`): `text` (None when the font does not
    /// say), `origin` and `end` (where its advance ends: the gap to the next glyph's origin is
    /// the space a word space or a `TJ` adjustment leaves), `bbox` (from the usual descent to
    /// the usual ascent), `ink` (its outline's box, or None), `size` (after the text and current
    /// transformation matrices), `direction` (unit vector of the baseline), `font` (`/BaseFont`
    /// as written, or None), `invisible` (the text layer of a scan, usually), `overdrawn` (repeats
    /// the glyph shown before it, less than a tenth of an em away: bold faked by drawing twice,
    /// leaders drawn over one another; leave those out to read the page's text), `actual_text`
    /// (what the document says the glyph reads as instead, through an `/ActualText` in a
    /// marked-content sequence or on the structure element: the whole text on the first glyph it
    /// covers, "" on the others; None when nothing says). A page's text, as a reader reads it:
    /// `actual_text` when not None, else `text` unless `overdrawn`. `annotation` tells the
    /// glyphs an annotation's appearance draws.
    #[pyo3(signature = (page_index, annotations = false))]
    fn glyphs<'py>(
        &self,
        py: Python<'py>,
        page_index: usize,
        annotations: bool,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let inner = &self.inner;
        let found = run(py, || {
            Ok(pdf::text::glyphs(inner, page_index, annotations)?)
        })?;
        // Keys interned, and each font's name made once: a page has thousands of glyphs.
        let mut fonts: HashMap<String, Bound<'py, PyString>> = HashMap::new();
        found
            .into_iter()
            .map(|glyph| {
                let dict = PyDict::new(py);
                dict.set_item(intern!(py, "text"), glyph.text)?;
                dict.set_item(intern!(py, "origin"), (glyph.origin[0], glyph.origin[1]))?;
                dict.set_item(intern!(py, "end"), (glyph.end[0], glyph.end[1]))?;
                let [x1, y1, x2, y2] = glyph.bbox;
                dict.set_item(intern!(py, "bbox"), (x1, y1, x2, y2))?;
                dict.set_item(
                    intern!(py, "ink"),
                    glyph.ink.map(|[a, b, c, d]| (a, b, c, d)),
                )?;
                dict.set_item(intern!(py, "size"), glyph.size)?;
                dict.set_item(
                    intern!(py, "direction"),
                    (glyph.direction[0], glyph.direction[1]),
                )?;
                let font = glyph.font.map(|name| {
                    fonts
                        .entry(name)
                        .or_insert_with_key(|name| PyString::new(py, name))
                        .clone()
                });
                dict.set_item(intern!(py, "font"), font)?;
                dict.set_item(intern!(py, "invisible"), glyph.invisible)?;
                dict.set_item(intern!(py, "overdrawn"), glyph.overdrawn)?;
                dict.set_item(intern!(py, "actual_text"), glyph.actual_text)?;
                dict.set_item(intern!(py, "annotation"), glyph.annotation)?;
                Ok(dict)
            })
            .collect()
    }

    /// Every occurrence's value, by full name: a text or choice field's text, a button's state
    /// (`/AS`; "Off" when it has none) read as UTF-8, else Latin-1.
    fn field_values<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let inner = &self.inner;
        let values = run(py, || Ok(pdf::annots::field_values(inner)?))?;
        let dict = PyDict::new(py);
        for (name, list) in values {
            dict.set_item(name, list)?;
        }
        Ok(dict)
    }

    /// Apply the XFDF a signing page exported: flatten what the document has first when
    /// `flatten` is set, then draw the XFDF's pictures and texts. Raises XfdfError for an XFDF
    /// outside a signing page's subset, or a mark that cannot be placed.
    #[pyo3(signature = (xfdf, *, flatten=true))]
    fn apply_xfdf<'py>(
        &mut self,
        py: Python<'py>,
        xfdf: String,
        flatten: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let inner = &mut self.inner;
        let applied = run(py, || Ok(pdf::annots::apply_xfdf(inner, &xfdf, flatten)?))?;
        applied_dict(py, &applied)
    }

    /// Convert the document to PDF/A-3 in place, at `level` "3a" (default), "3u" or "3b".
    /// `language` is the language of a document that declares none (`fr-CA`). The document
    /// must not have been changed since it was opened: save it and open it again first.
    /// Returns `changes` and `residual`, counts by kind (an empty `residual`: expected to
    /// conform).
    #[pyo3(signature = (*, level="3a", language=None))]
    fn convert_to_pdfa<'py>(
        &mut self,
        py: Python<'py>,
        level: &str,
        language: Option<String>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let options = pdfa_options(level, language).map_err(|f| f.into_py_err(py))?;
        let inner = &mut self.inner;
        let report = run(py, || {
            pdf::pdfa::convert(inner, &options).map_err(Failure::Pdfa)
        })?;
        pdfa_dict(py, &report)
    }

    /// Check that interpreting the pages (rendering, tagging) stays within the limits, without
    /// interpreting them; raises PdfLimitError otherwise. Returns the most a page spent on each
    /// bound.
    fn check_pages<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let inner = &self.inner;
        let peaks = run(py, || Ok(inner.check_pages()?))?;
        let dict = PyDict::new(py);
        dict.set_item("operators", peaks.operators)?;
        dict.set_item("shown_bytes", peaks.shown_bytes)?;
        dict.set_item("page_cost", peaks.page_cost)?;
        dict.set_item("image_pixels", peaks.image_pixels)?;
        dict.set_item("mesh_bytes", peaks.mesh_bytes)?;
        dict.set_item("code_range", peaks.code_range)?;
        dict.set_item("stream_bytes", peaks.stream_bytes)?;
        dict.set_item("dash_period", peaks.dash_period)?;
        dict.set_item("annotations", peaks.annotations)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!("Document(page_count={})", self.inner.page_count())
    }
}

/// `doc` saved in full, then rewritten smaller (as `optimize`), read again with `limits`.
fn compact(doc: &pdf::Document, limits: &pdf::Limits) -> Result<Vec<u8>, Failure> {
    let saved = doc.save(&pdf::SaveOptions::default())?;
    let options = pdf::OpenOptions::default()
        .limits(*limits)
        .max_file_bytes(usize::MAX);
    Ok(pdf::optimize(&saved, &options)?.data)
}

/// The PDFs `pdf_contents` opened with `limits`; an error names the source (1-based).
fn open_sources(
    pdf_contents: Vec<Vec<u8>>,
    limits: &pdf::Limits,
) -> Result<Vec<pdf::Document>, Failure> {
    let options = pdf::OpenOptions::default().limits(*limits);
    let mut sources = Vec::with_capacity(pdf_contents.len());
    for (index, data) in pdf_contents.into_iter().enumerate() {
        let doc =
            pdf::Document::open(data, &options).map_err(|error| pdf::annots::Error::Source {
                position: index + 1,
                error: Box::new(error.into()),
            })?;
        sources.push(doc);
    }
    Ok(sources)
}

fn field_dict<'py>(py: Python<'py>, field: &pdf::annots::Field) -> PyResult<Bound<'py, PyDict>> {
    use pdf::annots::{FieldChoice, FieldKind, FieldValue};
    let dict = PyDict::new(py);
    dict.set_item("name", &field.name)?;
    dict.set_item("label", &field.label)?;
    let kind = match field.kind {
        FieldKind::Text => "text",
        FieldKind::CheckBox => "checkbox",
        FieldKind::Radio => "radio",
        FieldKind::PushButton => "push_button",
        FieldKind::ComboBox => "combo_box",
        FieldKind::ListBox => "list_box",
        FieldKind::Signature => "signature",
        _ => "other",
    };
    dict.set_item("kind", kind)?;
    dict.set_item("page_number", field.page_index + 1)?;
    let rect = |r: pdf::Rect| (r.x1, r.y1, r.x2, r.y2);
    dict.set_item("rect", rect(field.rect))?;
    dict.set_item("crop_box", rect(field.crop_box))?;
    match &field.value {
        Some(FieldValue::Text(text)) => dict.set_item("value", text)?,
        Some(FieldValue::Name(name)) => {
            dict.set_item("value", pdf::annots::name_text(name))?;
        }
        Some(FieldValue::List(list)) => dict.set_item("value", list)?,
        None => dict.set_item("value", py.None())?,
    }
    dict.set_item("state", field.state.as_ref().map(|s| PyBytes::new(py, s)))?;
    dict.set_item("required", field.required)?;
    dict.set_item("read_only", field.read_only)?;
    dict.set_item("max_length", field.max_length)?;
    dict.set_item("editable", field.editable)?;
    let choices = PyList::empty(py);
    for choice in &field.choices {
        match choice {
            FieldChoice::Single(text) => choices.append(text)?,
            FieldChoice::Pair { export, display } => choices.append((export, display))?,
        }
    }
    dict.set_item("choices", choices)?;
    let states: Vec<Bound<'py, PyBytes>> = field
        .button_states
        .iter()
        .map(|state| PyBytes::new(py, state))
        .collect();
    dict.set_item("button_states", states)?;
    dict.set_item("button_caption", &field.button_caption)?;
    Ok(dict)
}

/// The number of pages of the PDF at `input_path`.
///
/// Raises PdfReadError when the PDF is malformed or encrypted with a password.
#[pyfunction]
#[pyo3(signature = (input_path, *, limits=None))]
fn count(
    py: Python<'_>,
    input_path: PathBuf,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<usize> {
    let options = options(None, None, limits.as_deref());
    run(py, || {
        Ok(pdf::Document::open_path(&input_path, &options)?.page_count())
    })
}

/// The geometry of every page of a PDF given as bytes, in page order.
///
/// Raises PdfReadError when the PDF is malformed, encrypted with a password, or has no page or
/// more than `max_pages`; the page count is checked before any page is read.
#[pyfunction]
#[pyo3(signature = (pdf_content, max_pages, *, limits=None))]
fn read_geometry(
    py: Python<'_>,
    pdf_content: Vec<u8>,
    max_pages: usize,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<Vec<PageGeometry>> {
    let options = options(None, Some((1, max_pages)), limits.as_deref());
    let pages = run(py, || {
        Ok(pdf::Document::open(pdf_content, &options)?.geometry())
    })?;
    Ok(pages.into_iter().map(PageGeometry::from).collect())
}

/// `pdf_content` with every annotation but links flattened into its page and the form taken
/// out.
#[pyfunction]
#[pyo3(signature = (pdf_content, *, limits=None))]
fn flatten_bytes<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<Bound<'py, PyBytes>> {
    let options = options(None, None, limits.as_deref());
    let saved = run(py, || {
        let mut doc = pdf::Document::open(pdf_content, &options)?;
        pdf::annots::flatten(&mut doc)?;
        Ok(doc.save(&pdf::SaveOptions::default())?)
    })?;
    Ok(PyBytes::new(py, &saved))
}

/// `pdf_content` with a signing page's XFDF applied (what it has flattened first when
/// `flatten` is set).
#[pyfunction]
#[pyo3(signature = (pdf_content, xfdf_data, *, flatten=true, limits=None))]
fn apply_xfdf_bytes<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
    xfdf_data: String,
    flatten: bool,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<Bound<'py, PyBytes>> {
    let options = options(None, None, limits.as_deref());
    let saved = run(py, || {
        let mut doc = pdf::Document::open(pdf_content, &options)?;
        pdf::annots::apply_xfdf(&mut doc, &xfdf_data, flatten)?;
        Ok(doc.save(&pdf::SaveOptions::default())?)
    })?;
    Ok(PyBytes::new(py, &saved))
}

/// Whether the PDF `pdf_content` holds a redaction mark (a `/Redact` annotation). A PDF that
/// cannot be read counts as marked: a copy of it must not skip what redacting asks for.
#[pyfunction]
#[pyo3(signature = (pdf_content, *, limits=None))]
fn has_redaction_marks(
    py: Python<'_>,
    pdf_content: Vec<u8>,
    limits: Option<PyRef<'_, Limits>>,
) -> bool {
    let options = options(None, None, limits.as_deref());
    py.detach(|| {
        catch_unwind(AssertUnwindSafe(|| {
            pdf::Document::open(pdf_content, &options)
                .and_then(|doc| doc.has_redaction_marks())
                .unwrap_or(true)
        }))
        .unwrap_or(true)
    })
}

/// A new PDF of a page for each photo of `photos` (JPEG or PNG bytes), as
/// `Document.add_photo_pages` adds them, and the photos (from 0) that could not be read.
/// `ValueError` when none can be.
#[pyfunction]
#[pyo3(signature = (photos, *, limits=None))]
fn photos_to_pdf<'py>(
    py: Python<'py>,
    photos: Vec<Vec<u8>>,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<(Bound<'py, PyBytes>, Vec<usize>)> {
    let bounds = limits
        .as_deref()
        .map_or_else(pdf::Limits::default, Limits::counted_from_now);
    let (data, unreadable) = run(py, || {
        let borrowed: Vec<&[u8]> = photos.iter().map(Vec::as_slice).collect();
        let (doc, report) = pdf::annots::photos_document(&borrowed, bounds)?;
        Ok((doc.save(&pdf::SaveOptions::default())?, report.unreadable))
    })?;
    Ok((PyBytes::new(py, &data), unreadable))
}

fn redact_options(images: &str, paths: &str) -> PyResult<pdf::text::RedactOptions> {
    let images = match images {
        "pixels" => pdf::text::ImageRedaction::Pixels,
        "remove" => pdf::text::ImageRedaction::Remove,
        "keep" => pdf::text::ImageRedaction::Keep,
        _ => {
            return Err(PyValueError::new_err(
                "images must be \"pixels\", \"remove\" or \"keep\"",
            ));
        }
    };
    let paths = match paths {
        "touched" => pdf::text::PathRedaction::Touched,
        "covered" => pdf::text::PathRedaction::Covered,
        "keep" => pdf::text::PathRedaction::Keep,
        _ => {
            return Err(PyValueError::new_err(
                "paths must be \"touched\", \"covered\" or \"keep\"",
            ));
        }
    };
    Ok(pdf::text::RedactOptions::default()
        .images(images)
        .paths(paths))
}

fn redacted_dict<'py>(py: Python<'py>, done: &pdf::text::Redacted) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("glyphs", done.glyphs)?;
    dict.set_item("operators", done.operators)?;
    dict.set_item("forms", done.forms)?;
    dict.set_item("strings_unread", done.strings_unread)?;
    dict.set_item("images", done.images)?;
    dict.set_item("images_removed", done.images_removed)?;
    dict.set_item("paths", done.paths)?;
    Ok(dict)
}

/// The documents merged into one, in order, each flattened first. With `flatten=False`, their
/// forms are kept instead, each document's fields renamed `name (2)`, `name (3)`... when an
/// earlier document has a field of that name. An error names the source (1-based) it came from.
#[pyfunction]
#[pyo3(signature = (pdf_contents, *, flatten=true, limits=None))]
fn merge_bytes<'py>(
    py: Python<'py>,
    pdf_contents: Vec<Vec<u8>>,
    flatten: bool,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<Bound<'py, PyBytes>> {
    let bounds = limits
        .as_deref()
        .map_or_else(pdf::Limits::default, Limits::counted_from_now);
    let saved = run(py, || {
        let sources = open_sources(pdf_contents, &bounds)?;
        if flatten {
            let (merged, _) = pdf::annots::merge(sources)?;
            return Ok(merged.save(&pdf::SaveOptions::default())?);
        }
        let borrowed: Vec<&pdf::Document> = sources.iter().collect();
        let picks: Vec<pdf::PagePick> = sources
            .iter()
            .enumerate()
            .flat_map(|(index, doc)| {
                (0..doc.page_count()).map(move |page| pdf::PagePick::new(index, page, 0))
            })
            .collect();
        let merged = pdf::Document::compose(&borrowed, &picks, true)?;
        Ok(merged.save(&pdf::SaveOptions::default())?)
    })?;
    Ok(PyBytes::new(py, &saved))
}

/// Every page rendered to one gray image at `dpi` (lower for a page longer than 3,400 pixels),
/// in a new PDF of as many pages.
#[pyfunction]
#[pyo3(signature = (pdf_content, *, dpi=200.0, limits=None))]
fn grayscale<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
    dpi: f32,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<Bound<'py, PyBytes>> {
    if !(dpi.is_finite() && dpi > 0.0) {
        return Err(PyValueError::new_err("dpi must be a finite number > 0"));
    }
    let options = options(None, None, limits.as_deref());
    let gray = run(py, || {
        Ok(pdf::raster::grayscale(&pdf_content, dpi, &options)?)
    })?;
    Ok(PyBytes::new(py, &gray))
}

/// `pdf_content` rewritten smaller, losslessly, or as it was when it is encrypted, signed, or
/// not made smaller.
#[pyfunction]
#[pyo3(signature = (pdf_content, *, limits=None))]
fn optimize<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<Bound<'py, PyBytes>> {
    let options = options(None, None, limits.as_deref());
    let optimized = run(py, || Ok(pdf::optimize(&pdf_content, &options)?))?;
    Ok(PyBytes::new(py, &optimized.data))
}

/// `pdf_content` converted to PDF/A-3 at `level` ("3a", "3u" or "3b"), and what the
/// conversion did: `changes` and `residual`,
/// counts by kind. Raises PdfAError when the conversion cannot be made; never returns the input
/// unconverted.
#[pyfunction]
#[pyo3(signature = (pdf_content, *, level="3a", language=None, limits=None))]
fn pdfa_convert<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
    level: &str,
    language: Option<String>,
    limits: Option<PyRef<'_, Limits>>,
) -> PyResult<(Bound<'py, PyBytes>, Bound<'py, PyDict>)> {
    let pdfa = pdfa_options(level, language).map_err(|f| f.into_py_err(py))?;
    let options = options(None, None, limits.as_deref());
    let (saved, report) = run(py, || {
        let mut doc = pdf::Document::open(pdf_content, &options)?;
        convert_and_save(&mut doc, &pdfa)
    })?;
    Ok((PyBytes::new(py, &saved), pdfa_dict(py, &report)?))
}

/// Drop what this thread keeps of the document it read last: the fonts its glyphs, drawings and
/// tagging parsed, and the document with them. Reading another document replaces it anyway; call
/// this when a thread is done with a large document it will not read again.
#[pyfunction]
fn release_cache() {
    pdf::text::release_cache();
}

#[pymodule]
#[pyo3(name = "_native")]
fn native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("PdfError", py.get_type::<PdfError>())?;
    m.add("PdfReadError", py.get_type::<PdfReadError>())?;
    m.add("PdfPasswordError", py.get_type::<PdfPasswordError>())?;
    m.add("PdfLimitError", py.get_type::<PdfLimitError>())?;
    m.add("PdfTimeoutError", py.get_type::<PdfTimeoutError>())?;
    m.add("XfdfError", py.get_type::<XfdfError>())?;
    m.add("PdfAError", py.get_type::<PdfAError>())?;
    m.add("FieldNameTakenError", py.get_type::<FieldNameTakenError>())?;
    m.add_class::<Document>()?;
    m.add_class::<PageGeometry>()?;
    m.add_class::<Limits>()?;
    m.add_function(wrap_pyfunction!(count, m)?)?;
    m.add_function(wrap_pyfunction!(read_geometry, m)?)?;
    m.add_function(wrap_pyfunction!(flatten_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(apply_xfdf_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(merge_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(grayscale, m)?)?;
    m.add_function(wrap_pyfunction!(optimize, m)?)?;
    m.add_function(wrap_pyfunction!(pdfa_convert, m)?)?;
    m.add_function(wrap_pyfunction!(release_cache, m)?)?;
    m.add_function(wrap_pyfunction!(has_redaction_marks, m)?)?;
    m.add_function(wrap_pyfunction!(photos_to_pdf, m)?)?;
    signing::register(m)?;
    Ok(())
}
