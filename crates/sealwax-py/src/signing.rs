//! Signatures from Python: the signer, the timestamp authority and the revocation sources are
//! Python objects (an HSM stays with its caller, the network in `sealwax.net`); the
//! interpreter is taken back only to call them.

use std::sync::Mutex;

use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use crate::{Failure, PdfError, run};

create_exception!(
    sealwax,
    SignatureError,
    PdfError,
    "The signature cannot be made: a certificate that does not read, a field that exists, a \
     certification that is not the first signature, a signature too large."
);
create_exception!(
    sealwax,
    TimestampError,
    SignatureError,
    "No timestamp was obtained, or the one obtained was not accepted (under `required`)."
);
create_exception!(
    sealwax,
    ValidationError,
    SignatureError,
    "The validation data of a signature cannot be gathered: untrusted, revoked, unavailable."
);

/// The exception of a signing error. An exception raised by a Python object is raised as is,
/// before this is reached.
pub(crate) fn sign_error(py: Python<'_>, error: pdf::sign::Error) -> PyErr {
    match error {
        pdf::sign::Error::Pdf(error) => crate::Failure::Pdf(error).into_py_err(py),
        pdf::sign::Error::Timestamp(error) => TimestampError::new_err(error.to_string()),
        pdf::sign::Error::Validation(error) => ValidationError::new_err(error.to_string()),
        other => SignatureError::new_err(other.to_string()),
    }
}

/// The first exception a Python object raised while Rust code called it.
#[derive(Default)]
struct Raised(Mutex<Option<PyErr>>);

impl Raised {
    fn keep(&self, error: PyErr) -> String {
        let message = Python::attach(|py| error.value(py).to_string());
        if let Ok(mut slot) = self.0.lock()
            && slot.is_none()
        {
            *slot = Some(error);
        }
        message
    }

    fn take(&self) -> Option<PyErr> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// An attribute of `object`, called when it is a method.
fn read<'py>(object: &Bound<'py, PyAny>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    let value = object.getattr(name)?;
    if value.is_callable() {
        value.call0()
    } else {
        Ok(value)
    }
}

/// The names a Python signer may give its algorithm: JOSE's (RFC 7518), as cloud key vaults name
/// them, and longer ones.
const ALGORITHMS: [(&[&str], pdf::sign::Algorithm); 9] = [
    (
        &["RS256", "rsa-pkcs1v15-sha256", "rsa"],
        pdf::sign::Algorithm::RsaPkcs1Sha256,
    ),
    (
        &["RS384", "rsa-pkcs1v15-sha384"],
        pdf::sign::Algorithm::RsaPkcs1Sha384,
    ),
    (
        &["RS512", "rsa-pkcs1v15-sha512"],
        pdf::sign::Algorithm::RsaPkcs1Sha512,
    ),
    (
        &["PS256", "rsa-pss-sha256"],
        pdf::sign::Algorithm::RsaPssSha256,
    ),
    (
        &["PS384", "rsa-pss-sha384"],
        pdf::sign::Algorithm::RsaPssSha384,
    ),
    (
        &["PS512", "rsa-pss-sha512"],
        pdf::sign::Algorithm::RsaPssSha512,
    ),
    (
        &["ES256", "ecdsa-sha256", "ecdsa"],
        pdf::sign::Algorithm::EcdsaSha256,
    ),
    (
        &["ES384", "ecdsa-sha384"],
        pdf::sign::Algorithm::EcdsaSha384,
    ),
    (
        &["ES512", "ecdsa-sha512"],
        pdf::sign::Algorithm::EcdsaSha512,
    ),
];

/// A Python signer: `certificate` (DER), `chain` (DER, intermediates and the root if wanted),
/// `algorithm` (see [`ALGORITHMS`]; `RS256` when absent), and `sign_digest(digest)`, the
/// signature value over the algorithm's digest.
struct PySigner {
    object: Py<PyAny>,
    certificate: Vec<u8>,
    chain: Vec<Vec<u8>>,
    algorithm: pdf::sign::Algorithm,
    raised: Raised,
}

impl PySigner {
    fn new(object: &Bound<'_, PyAny>) -> PyResult<Self> {
        let certificate: Vec<u8> = read(object, "certificate")?.extract()?;
        let chain: Vec<Vec<u8>> = match object.hasattr("chain")? {
            true => read(object, "chain")?.extract()?,
            false => Vec::new(),
        };
        let algorithm = match object.hasattr("algorithm")? {
            true => read(object, "algorithm")?.extract::<String>()?,
            false => "rsa-pkcs1v15-sha256".to_owned(),
        };
        let Some(algorithm) = ALGORITHMS
            .iter()
            .find(|(names, _)| names.contains(&algorithm.as_str()))
            .map(|(_, algorithm)| *algorithm)
        else {
            return Err(PyValueError::new_err(
                "signer.algorithm must be one of RS256, RS384, RS512, PS256, PS384, PS512, \
                 ES256, ES384, ES512 (or their long names, such as \"rsa-pss-sha256\")",
            ));
        };
        Ok(Self {
            object: object.clone().unbind(),
            certificate,
            chain,
            algorithm,
            raised: Raised::default(),
        })
    }
}

impl pdf::sign::Signer for PySigner {
    fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    fn algorithm(&self) -> pdf::sign::Algorithm {
        self.algorithm
    }

    fn sign(&self, _message: &[u8], digest: &[u8]) -> Result<Vec<u8>, String> {
        Python::attach(|py| {
            self.object
                .bind(py)
                .call_method1("sign_digest", (PyBytes::new(py, digest),))
                .and_then(|value| value.extract::<Vec<u8>>())
                .map_err(|error| self.raised.keep(error))
        })
    }
}

/// A Python timestamp client: an object with `timestamp(request)`, or a callable, given the
/// request (`TimeStampReq`, DER) and returning the response (`TimeStampResp`, DER).
struct PyClient {
    object: Py<PyAny>,
    raised: Raised,
}

impl pdf::sign::TimestampClient for PyClient {
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        Python::attach(|py| {
            let object = self.object.bind(py);
            let request = PyBytes::new(py, request);
            let response = match object.hasattr("timestamp") {
                Ok(true) => object.call_method1("timestamp", (request,)),
                _ => object.call1((request,)),
            };
            response
                .and_then(|value| value.extract::<Vec<u8>>())
                .map_err(|error| self.raised.keep(error))
        })
    }
}

/// A Python revocation source: `ocsp(url, request)`, the responder's answer (DER) to the request
/// (DER) posted to `url`, and `crl(url)`, the CRL (DER) published there.
struct PyFetcher {
    object: Py<PyAny>,
}

impl pdf::sign::RevocationFetcher for PyFetcher {
    fn ocsp(&self, url: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        Python::attach(|py| {
            self.object
                .bind(py)
                .call_method1("ocsp", (url, PyBytes::new(py, request)))
                .and_then(|value| value.extract::<Vec<u8>>())
                .map_err(|error| error.value(py).to_string())
        })
    }

    fn crl(&self, url: &str) -> Result<Vec<u8>, String> {
        Python::attach(|py| {
            self.object
                .bind(py)
                .call_method1("crl", (url,))
                .and_then(|value| value.extract::<Vec<u8>>())
                .map_err(|error| error.value(py).to_string())
        })
    }
}

fn trust(roots: &[Vec<u8>], intermediates: &[Vec<u8>]) -> PyResult<pdf::sign::Trust> {
    pdf::sign::Trust::new(roots, intermediates)
        .map_err(|_| SignatureError::new_err("a trusted certificate cannot be read"))
}

/// How to timestamp a signature: the client that reaches the authority, the roots it must chain
/// to (DER) and the intermediates its tokens may leave out, and whether a failed timestamp stops
/// the signature (`required`, the default) or leaves it without one (`required=False`, reported
/// in `Signed.timestamp_error`).
#[pyclass(frozen, module = "sealwax")]
struct Timestamping {
    client: Py<PyAny>,
    roots: Vec<Vec<u8>>,
    intermediates: Vec<Vec<u8>>,
    required: bool,
}

#[pymethods]
impl Timestamping {
    #[new]
    #[pyo3(signature = (client, *, roots, intermediates=Vec::new(), required=true))]
    fn new(
        client: Py<PyAny>,
        roots: Vec<Vec<u8>>,
        intermediates: Vec<Vec<u8>>,
        required: bool,
    ) -> PyResult<Self> {
        if roots.is_empty() {
            return Err(PyValueError::new_err(
                "roots must name at least one certificate",
            ));
        }
        Ok(Self {
            client,
            roots,
            intermediates,
            required,
        })
    }

    #[getter]
    fn required(&self) -> bool {
        self.required
    }
}

/// A signed document, and what became of its timestamp.
#[pyclass(frozen, get_all, module = "sealwax")]
struct Signed {
    /// The signed PDF.
    pdf: Py<PyBytes>,
    /// The timestamp's time (seconds since 1970, UTC), if one was embedded.
    timestamp_time: Option<i64>,
    /// The authority's certificate subject, if a timestamp was embedded.
    timestamp_authority: Option<String>,
    /// Why no timestamp was embedded, when it was not required.
    timestamp_error: Option<String>,
}

#[pymethods]
impl Signed {
    fn __repr__(&self) -> String {
        format!(
            "Signed(timestamp_time={:?}, timestamp_authority={:?}, timestamp_error={:?})",
            self.timestamp_time, self.timestamp_authority, self.timestamp_error
        )
    }
}

/// Sign `pdf_content` in an incremental revision: an invisible field `field` on the first page,
/// a certification (DocMDP P=3, the document's first signature) when `certify` is set, else an
/// approval. `signer` makes the signature value (see `sealwax.Signer`); `timestamp` asks a
/// timestamp authority for a token over it (PAdES B-T).
///
/// A document with a signature that is not intact (see `verify_signatures`) is refused, unless
/// `allow_broken_signatures` is set.
///
/// Raises SignatureError (TimestampError when a required timestamp fails); an exception the
/// signer or the timestamp client raises is raised as is.
#[pyfunction]
#[pyo3(signature = (
    pdf_content,
    signer,
    *,
    field,
    certify=false,
    reason=None,
    location=None,
    contact_info=None,
    signing_time=None,
    timestamp=None,
    allow_broken_signatures=false,
))]
#[allow(clippy::too_many_arguments)]
fn sign<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
    signer: &Bound<'py, PyAny>,
    field: String,
    certify: bool,
    reason: Option<String>,
    location: Option<String>,
    contact_info: Option<String>,
    signing_time: Option<i64>,
    timestamp: Option<PyRef<'py, Timestamping>>,
    allow_broken_signatures: bool,
) -> PyResult<Signed> {
    let signer = PySigner::new(signer)?;
    let kind = if certify {
        pdf::sign::Kind::certification()
    } else {
        pdf::sign::Kind::Approval
    };
    let mut options = pdf::sign::Options::new(field, kind);
    options.reason = reason;
    options.location = location;
    options.contact_info = contact_info;
    options.signing_time = signing_time;
    options.allow_broken_signatures = allow_broken_signatures;
    let stamping = match &timestamp {
        Some(t) => Some((
            PyClient {
                object: t.client.clone_ref(py),
                raised: Raised::default(),
            },
            trust(&t.roots, &t.intermediates)?,
            if t.required {
                pdf::sign::Policy::Required
            } else {
                pdf::sign::Policy::BestEffort
            },
        )),
        None => None,
    };
    let outcome = run(py, || {
        let timestamping =
            stamping
                .as_ref()
                .map(|(client, trust, policy)| pdf::sign::Timestamping {
                    client,
                    trust,
                    policy: *policy,
                });
        let signed = pdf::sign::sign(&pdf_content, &signer, &options, timestamping.as_ref());
        // What a Python object raised goes out as it was raised.
        if let Some(error) = signer.raised.take() {
            return Err(Failure::Python(error));
        }
        match signed {
            Err(pdf::sign::Error::Timestamp(error)) => {
                let cause = stamping
                    .as_ref()
                    .and_then(|(client, ..)| client.raised.take());
                Err(Failure::Python(Python::attach(|py| {
                    let exception = TimestampError::new_err(error.to_string());
                    exception.set_cause(py, cause);
                    exception
                })))
            }
            other => Ok(other?),
        }
    })?;
    Ok(Signed {
        pdf: PyBytes::new(py, &outcome.pdf).unbind(),
        timestamp_time: outcome.timestamp.as_ref().map(|t| t.time),
        timestamp_authority: outcome.timestamp.as_ref().map(|t| t.authority.clone()),
        timestamp_error: outcome.timestamp_error.map(|e| e.to_string()),
    })
}

/// Add the validation data of the signature in field `field` (PAdES B-LT), in an incremental
/// revision: a proof that each certificate of the signer's and of its timestamp authority's
/// paths was not revoked, an OCSP response or a CRL fetched through `fetcher` (see
/// `sealwax.RevocationFetcher`) and checked, then the signature's VRI entry. `roots` are the
/// anchors (DER) of the signer and of the authority; `time`, seconds since 1970, the time the
/// data must be current at (now by default).
///
/// Returns the PDF and what was added: `certificates`, `ocsp_responses`, `crls`. Raises
/// ValidationError when a certificate is revoked, untrusted, or nothing tells whether it is.
#[pyfunction]
#[pyo3(signature = (pdf_content, field, *, fetcher, roots, intermediates=Vec::new(), time=None))]
fn add_validation_data<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
    field: String,
    fetcher: Py<PyAny>,
    roots: Vec<Vec<u8>>,
    intermediates: Vec<Vec<u8>>,
    time: Option<i64>,
) -> PyResult<(Bound<'py, PyBytes>, Bound<'py, PyDict>)> {
    let trust = trust(&roots, &intermediates)?;
    let fetcher = PyFetcher { object: fetcher };
    let (data, coverage) = run(py, || {
        let validation = pdf::sign::Validation {
            fetcher: &fetcher,
            trust: &trust,
            time,
        };
        Ok(pdf::sign::add_validation_data(
            &pdf_content,
            &field,
            &validation,
        )?)
    })?;
    let dict = PyDict::new(py);
    dict.set_item("certificates", coverage.certificates)?;
    dict.set_item("ocsp_responses", coverage.ocsp_responses)?;
    dict.set_item("crls", coverage.crls)?;
    Ok((PyBytes::new(py, &data), dict))
}

/// Add a document timestamp (PAdES B-LTA) to `pdf_content`, in an incremental revision: an RFC
/// 3161 token over the whole document, in an invisible field `field`. `timestamp` names the
/// authority and the roots it must chain to (its `required` is not looked at: a document
/// timestamp is the operation itself). Given after the signatures' validation data, it keeps that
/// data provable once their certificates expire; `add_validation_data` on its field, then
/// another timestamp, renews it. A document with a signature that is not intact is refused.
///
/// Returns a `Signed`: the PDF, the timestamp's time and authority. Raises TimestampError when no
/// timestamp is had or the one had is not accepted, SignatureError otherwise; an exception the
/// client raises is raised as is.
#[pyfunction]
#[pyo3(signature = (pdf_content, timestamp, *, field="DocTimeStamp1".to_owned()))]
fn add_document_timestamp(
    py: Python<'_>,
    pdf_content: Vec<u8>,
    timestamp: PyRef<'_, Timestamping>,
    field: String,
) -> PyResult<Signed> {
    let client = PyClient {
        object: timestamp.client.clone_ref(py),
        raised: Raised::default(),
    };
    let trust = trust(&timestamp.roots, &timestamp.intermediates)?;
    let (data, stamp) = run(py, || {
        match pdf::sign::add_document_timestamp(&pdf_content, &field, &client, &trust) {
            Err(pdf::sign::Error::Timestamp(error)) => {
                let cause = client.raised.take();
                Err(Failure::Python(Python::attach(|py| {
                    let exception = TimestampError::new_err(error.to_string());
                    exception.set_cause(py, cause);
                    exception
                })))
            }
            other => Ok(other?),
        }
    })?;
    Ok(Signed {
        pdf: PyBytes::new(py, &data).unbind(),
        timestamp_time: Some(stamp.time),
        timestamp_authority: Some(stamp.authority),
        timestamp_error: None,
    })
}

/// The signatures of `pdf_content`, each checked for integrity: that its byte range covers its
/// revision but its own value, that the bytes it covers have the digest it signed, that its
/// value verifies with its signer's key, and its timestamp if it has one. Whom the signer is,
/// and whether to trust them, is not looked at.
///
/// Returns one dict per signature: `field`, `kind` ("certification", "approval",
/// "document_timestamp"), `sub_filter`, `signer` (the certificate subject, or None),
/// `timestamp_time` (seconds since 1970, or None), `later_revisions` (the revisions appended
/// after the one it signed), `integrity` ("intact", "broken", "unchecked") and `detail` (why it
/// is not intact, or None).
#[pyfunction]
fn verify_signatures<'py>(
    py: Python<'py>,
    pdf_content: Vec<u8>,
) -> PyResult<Vec<Bound<'py, PyDict>>> {
    let checks = run(py, || Ok(pdf::sign::verify_signatures(&pdf_content)?))?;
    checks
        .into_iter()
        .map(|check| {
            let dict = PyDict::new(py);
            let kind = match check.kind {
                pdf::sign::SignatureKind::Certification => "certification",
                pdf::sign::SignatureKind::DocumentTimestamp => "document_timestamp",
                _ => "approval",
            };
            let (integrity, detail) = match &check.integrity {
                pdf::sign::Integrity::Intact => ("intact", None),
                pdf::sign::Integrity::Broken(problem) => ("broken", Some(problem.to_string())),
                pdf::sign::Integrity::Unchecked(why) => ("unchecked", Some((*why).to_owned())),
                _ => ("unchecked", None),
            };
            dict.set_item("field", &check.field)?;
            dict.set_item("kind", kind)?;
            dict.set_item("sub_filter", &check.sub_filter)?;
            dict.set_item("signer", &check.signer)?;
            dict.set_item("timestamp_time", check.timestamp_time)?;
            dict.set_item("later_revisions", check.later_revisions)?;
            dict.set_item("integrity", integrity)?;
            dict.set_item("detail", detail)?;
            Ok(dict)
        })
        .collect()
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("SignatureError", py.get_type::<SignatureError>())?;
    m.add("TimestampError", py.get_type::<TimestampError>())?;
    m.add("ValidationError", py.get_type::<ValidationError>())?;
    m.add_class::<Timestamping>()?;
    m.add_class::<Signed>()?;
    m.add_function(wrap_pyfunction!(sign, m)?)?;
    m.add_function(wrap_pyfunction!(add_validation_data, m)?)?;
    m.add_function(wrap_pyfunction!(add_document_timestamp, m)?)?;
    m.add_function(wrap_pyfunction!(verify_signatures, m)?)?;
    Ok(())
}
