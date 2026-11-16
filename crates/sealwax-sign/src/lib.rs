//! PAdES signatures (ETSI EN 319 142-1, baseline B-B, then B-T with a timestamp, B-LT with
//! validation data, B-LTA with document timestamps) of PDF documents: an invisible field on the first page, a certification (DocMDP, annotations, form
//! filling and signatures allowed: P=3) by the first signer, then approvals, each in an
//! incremental revision.
//!
//! The key never comes here: the signature value is made outside (an HSM, a cloud key vault),
//! from the digest this crate gives. Hence two phases, [`prepare`] then
//! [`Prepared::finalize`], and [`sign`] for a [`Signer`] that answers at once.
//!
//! A timestamp (RFC 3161) over the signature value makes it B-T: [`TimestampRequest`] asks for
//! one, its response checked before the token goes in (`tsp`). The network stays outside too: a
//! [`TimestampClient`] carries the request.
//!
//! Validation data (B-LT) comes after the signature, in a revision of its own:
//! [`add_validation_data`] fetches through a [`RevocationFetcher`] a proof for each certificate
//! of the signer's and the timestamp authority's paths, checks it, and completes the document
//! security store.
//!
//! A document timestamp (B-LTA) then covers the whole document, the store included:
//! [`add_document_timestamp`]. It keeps the validation data provable once the certificates it
//! relies on have expired; validation data for its own authority, then another timestamp, renew
//! it.
//!
//! [`verify_signatures`] checks the integrity of the signatures a document holds: byte range,
//! digest, value, timestamp, and the revisions since. Nothing is signed, timestamped or
//! validated over a signature that is not intact ([`Error::BrokenSignature`]).
//!
//! ```no_run
//! use sealwax_sign::{Algorithm, Kind, Options, prepare};
//!
//! let pdf = std::fs::read("in.pdf")?;
//! let certificate = std::fs::read("signer.der")?;
//! let options = Options::new("Signature1", Kind::certification());
//! let prepared = prepare(&pdf, &certificate, &[], Algorithm::RsaPkcs1Sha256, &options)?;
//! let signature: Vec<u8> = todo!("the HSM signs prepared.digest()");
//! std::fs::write("out.pdf", prepared.finalize(&signature, None)?)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod cms;
mod date;
mod ltv;
mod pdf;
mod pki;
mod revocation;
mod signed_data;
mod tlv;
mod tsp;
mod verify;

use der::asn1::SetOfVec;
use sha2::Digest;
use x509_cert::attr::Attribute;

pub use date::pdf_date;
pub use ltv::{Coverage, RevocationFetcher, Validation, ValidationError, add_validation_data};
pub use tsp::{Timestamp, TimestampError, TimestampRequest, Trust, check_timestamp_token};
pub use verify::{Broken, Integrity, SignatureCheck, SignatureKind, verify_signatures};

/// Everything that can go wrong. Messages never hold document content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Reading or writing the document failed (an encrypted document, or one whose
    /// cross-reference was repaired, takes no incremental revision).
    #[error(transparent)]
    Pdf(#[from] sealwax_core::Error),
    /// A certificate is not a DER-encoded X.509 certificate.
    #[error("certificate cannot be read")]
    Certificate,
    /// The document already has a field of that name.
    #[error("the document already has a field named {0:?}")]
    FieldExists(String),
    /// A field name must be one partial name: not empty, without a period, which separates the
    /// partial names of a full name.
    #[error("{0:?} is not a field name: it must not be empty nor hold a period")]
    FieldName(String),
    /// A certification must be the document's first signature, and its only one.
    #[error("a certification must be the first signature of the document")]
    NotFirstSignature,
    /// The document has no page to hold the field.
    #[error("the document has no page")]
    NoPage,
    /// The signature does not fit in the room reserved for it: never cut short.
    #[error("signature of {needed} bytes does not fit the {reserved} reserved")]
    ContentsTooLarge { needed: usize, reserved: usize },
    /// No timestamp was obtained, or the one obtained was not accepted.
    #[error(transparent)]
    Timestamp(#[from] TimestampError),
    /// The validation data of a signature could not be gathered.
    #[error(transparent)]
    Validation(ValidationError),
    /// A signature of the document is not intact ([`verify_signatures`]): nothing is added over
    /// it.
    #[error("the signature in field {field:?} is not intact: {problem}")]
    BrokenSignature { field: String, problem: Broken },
    /// The revision written could not be laid out as expected (a bug).
    #[error("signature dictionary not found where it was written")]
    Layout,
    /// Encoding the CMS signature failed.
    #[error("CMS encoding failed: {0}")]
    Der(#[from] der::Error),
    /// The signer failed.
    #[error("signer failed: {0}")]
    Signer(String),
}

/// Result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The digest a signature is made over: of the signed bytes, then of the signed attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DigestAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl DigestAlgorithm {
    /// The digest's length in bytes: 32, 48 or 64.
    #[must_use]
    pub fn output_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    pub(crate) fn hasher(self) -> Hasher {
        match self {
            Self::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
            Self::Sha384 => Hasher::Sha384(sha2::Sha384::new()),
            Self::Sha512 => Hasher::Sha512(sha2::Sha512::new()),
        }
    }

    pub(crate) fn digest(self, data: &[u8]) -> Vec<u8> {
        let mut hasher = self.hasher();
        hasher.update(data);
        hasher.finish()
    }
}

/// A digest being computed.
pub(crate) enum Hasher {
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
}

impl Hasher {
    pub(crate) fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(data),
            Self::Sha384(hasher) => hasher.update(data),
            Self::Sha512(hasher) => hasher.update(data),
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        match self {
            Self::Sha256(hasher) => hasher.finalize().to_vec(),
            Self::Sha384(hasher) => hasher.finalize().to_vec(),
            Self::Sha512(hasher) => hasher.finalize().to_vec(),
        }
    }
}

/// How the signature value is made: the key's algorithm and the digest it signs. The names in
/// parentheses are those of JOSE (RFC 7518), which cloud key vaults use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Algorithm {
    /// RSA, PKCS #1 v1.5 (`RS256`).
    RsaPkcs1Sha256,
    /// RSA, PKCS #1 v1.5 (`RS384`).
    RsaPkcs1Sha384,
    /// RSA, PKCS #1 v1.5 (`RS512`).
    RsaPkcs1Sha512,
    /// RSA-PSS, MGF1 with the same digest, a salt as long as the digest (`PS256`).
    RsaPssSha256,
    /// RSA-PSS (`PS384`).
    RsaPssSha384,
    /// RSA-PSS (`PS512`).
    RsaPssSha512,
    /// ECDSA (`ES256` with a P-256 key; also with a P-384 key).
    EcdsaSha256,
    /// ECDSA (`ES384` with a P-384 key).
    EcdsaSha384,
    /// ECDSA (`ES512` with a P-521 key).
    EcdsaSha512,
}

impl Algorithm {
    /// The digest this algorithm signs.
    #[must_use]
    pub fn digest_algorithm(self) -> DigestAlgorithm {
        match self {
            Self::RsaPkcs1Sha256 | Self::RsaPssSha256 | Self::EcdsaSha256 => {
                DigestAlgorithm::Sha256
            }
            Self::RsaPkcs1Sha384 | Self::RsaPssSha384 | Self::EcdsaSha384 => {
                DigestAlgorithm::Sha384
            }
            Self::RsaPkcs1Sha512 | Self::RsaPssSha512 | Self::EcdsaSha512 => {
                DigestAlgorithm::Sha512
            }
        }
    }

    /// Whether the key is an elliptic curve key (ECDSA).
    #[must_use]
    pub fn is_ecdsa(self) -> bool {
        matches!(
            self,
            Self::EcdsaSha256 | Self::EcdsaSha384 | Self::EcdsaSha512
        )
    }
}

/// Which signature: the first, certifying the document, or one approving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// DocMDP with these permissions: 1 no change, 2 form filling and signatures, 3 also
    /// annotations.
    Certification {
        permissions: u8,
    },
    Approval,
}

impl Kind {
    /// A certification allowing annotations, form filling and signatures (P=3).
    pub fn certification() -> Self {
        Self::Certification { permissions: 3 }
    }
}

/// How to sign.
#[derive(Debug, Clone)]
pub struct Options {
    pub field_name: String,
    pub kind: Kind,
    pub reason: Option<String>,
    pub location: Option<String>,
    pub contact_info: Option<String>,
    /// The signing time `/M` declares, in seconds since 1970 (UTC); now when `None`.
    pub signing_time: Option<i64>,
    /// Bytes reserved for the CMS signature: 32,768 by default.
    pub reserved_contents_bytes: usize,
    /// Sign even when a signature already in the document is not intact. By default such a
    /// document is refused ([`Error::BrokenSignature`]).
    pub allow_broken_signatures: bool,
}

impl Options {
    pub fn new(field_name: impl Into<String>, kind: Kind) -> Self {
        Self {
            field_name: field_name.into(),
            kind,
            reason: None,
            location: None,
            contact_info: None,
            signing_time: None,
            reserved_contents_bytes: 32_768,
            allow_broken_signatures: false,
        }
    }
}

/// A document with its signature's revision laid out, waiting for the signature value.
#[derive(Debug, Clone)]
pub struct Prepared {
    laid: pdf::Laid,
    attributes: SetOfVec<Attribute>,
    digest: Vec<u8>,
    algorithm: Algorithm,
    signer: x509_cert::Certificate,
    certificates: Vec<x509_cert::Certificate>,
}

/// Lay out the signature's revision of `pdf` for the signer whose certificate (DER) is
/// `certificate`, with the rest of its chain `chain` (intermediates, the root if wanted).
///
/// # Errors
/// [`Error::Certificate`] for a certificate that does not read; [`Error::FieldName`],
/// [`Error::FieldExists`], [`Error::NotFirstSignature`], [`Error::NoPage`]; [`Error::Pdf`] for a document that takes no
/// incremental revision.
pub fn prepare(
    pdf: &[u8],
    certificate: &[u8],
    chain: &[Vec<u8>],
    algorithm: Algorithm,
    options: &Options,
) -> Result<Prepared> {
    if !options.allow_broken_signatures {
        refuse_broken(pdf)?;
    }
    let signer = cms::certificate(certificate)?;
    let mut certificates = vec![signer.clone()];
    for der in chain {
        certificates.push(cms::certificate(der)?);
    }
    let time = options.signing_time.unwrap_or_else(date::now);
    let laid = pdf::lay_out(pdf, options, &pdf_date(time))?;
    let digest_algorithm = algorithm.digest_algorithm();
    let mut hasher = digest_algorithm.hasher();
    for part in laid.signed_parts() {
        hasher.update(part);
    }
    let message_digest = hasher.finish();
    let attributes = cms::signed_attributes(&message_digest, &signer, certificate)?;
    let digest = cms::digest_to_sign(&attributes, digest_algorithm)?;
    Ok(Prepared {
        laid,
        attributes,
        digest,
        algorithm,
        signer,
        certificates,
    })
}

impl Prepared {
    /// What the signer signs: the digest of the signed attributes, by the algorithm's digest
    /// ([`Algorithm::digest_algorithm`]: 32, 48 or 64 bytes).
    pub fn digest(&self) -> &[u8] {
        &self.digest
    }

    /// The algorithm the signature value is made with.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// The signed attributes, DER-encoded: the message whose digest is [`Self::digest`], for a
    /// signer that hashes what it signs itself.
    ///
    /// # Errors
    /// [`Error::Der`] should the encoding fail.
    pub fn message(&self) -> Result<Vec<u8>> {
        Ok(der::Encode::to_der(&self.attributes)?)
    }

    /// The document, signed with `signature` (the value the signer made over [`Self::digest`]),
    /// and the timestamp token (`TimeStampToken`, DER) over that value if there is one. An ECDSA
    /// value may be DER (`Ecdsa-Sig-Value`) or the two integers side by side, as JOSE and some
    /// key vaults give them (`r || s`): it is written as DER.
    ///
    /// # Errors
    /// [`Error::ContentsTooLarge`] when the signature does not fit; [`Error::Timestamp`] for a
    /// token that is not DER; [`Error::Signer`] for an ECDSA value that is neither form.
    pub fn finalize(mut self, signature: &[u8], timestamp_token: Option<&[u8]>) -> Result<Vec<u8>> {
        let signature = if self.algorithm.is_ecdsa() {
            cms::ecdsa_der(signature, &self.signer)?
        } else {
            signature.to_vec()
        };
        let cms = cms::signed_data(
            self.attributes,
            self.algorithm,
            &signature,
            &self.signer,
            &self.certificates,
            timestamp_token,
        )?;
        pdf::fill(&mut self.laid, &cms)?;
        Ok(self.laid.data)
    }
}

/// What makes signature values: an HSM, a key in a file.
pub trait Signer {
    /// The signer's certificate, DER.
    fn certificate(&self) -> &[u8];
    /// The rest of its chain, DER: intermediates, the root if wanted.
    fn chain(&self) -> &[Vec<u8>];
    fn algorithm(&self) -> Algorithm;
    /// The signature value over `message`, the DER encoding of the signed attributes, whose
    /// digest by the algorithm's digest ([`Algorithm::digest_algorithm`]) is `digest`: an HSM
    /// signs the digest, a key in memory may sign the message. Called once per signature.
    ///
    /// # Errors
    /// Whatever keeps the signer from signing.
    fn sign(&self, message: &[u8], digest: &[u8]) -> Result<Vec<u8>, String>;
}

/// What carries a timestamp request to the authority and brings its response back: HTTP
/// (`application/timestamp-query`) in production, from outside this crate.
pub trait TimestampClient {
    /// The authority's response (`TimeStampResp`, DER) to `request` (`TimeStampReq`, DER).
    ///
    /// # Errors
    /// Whatever keeps the response from coming back.
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>, String>;
}

/// What to do when a step that adds to the signature fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// The signature is not made.
    Required,
    /// The signature is made without it, and the failure reported beside it.
    BestEffort,
}

/// How to timestamp: who asks, whom to trust, what to do on failure.
#[derive(Clone, Copy)]
pub struct Timestamping<'a> {
    pub client: &'a dyn TimestampClient,
    pub trust: &'a Trust,
    pub policy: Policy,
}

impl std::fmt::Debug for Timestamping<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timestamping")
            .field("trust", self.trust)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// A signed document, and what became of its timestamp.
#[derive(Debug, Clone)]
pub struct Signed {
    pub pdf: Vec<u8>,
    /// The timestamp embedded, if one was asked for and accepted.
    pub timestamp: Option<Timestamp>,
    /// Why none was, under [`Policy::BestEffort`].
    pub timestamp_error: Option<TimestampError>,
}

impl Prepared {
    /// The timestamp request over `signature`, the value the signer made: its response, checked
    /// by [`TimestampRequest::check_response`], gives the token [`Self::finalize`] embeds.
    ///
    /// # Errors
    /// As [`TimestampRequest::new`].
    pub fn timestamp_request(&self, signature: &[u8]) -> Result<TimestampRequest> {
        TimestampRequest::new(signature)
    }
}

fn timestamp(signature: &[u8], timestamping: &Timestamping<'_>) -> Result<Timestamp> {
    let request = TimestampRequest::new(signature)?;
    let response = timestamping
        .client
        .timestamp(request.to_der())
        .map_err(TimestampError::Client)?;
    Ok(request.check_response(&response, timestamping.trust)?)
}

/// Sign `pdf` in one step with `signer`, timestamped if `timestamping` says so.
///
/// # Errors
/// As [`prepare`] and [`Prepared::finalize`]; [`Error::Signer`] when the signer fails;
/// [`Error::Timestamp`] when the timestamp fails under [`Policy::Required`].
pub fn sign(
    pdf: &[u8],
    signer: &dyn Signer,
    options: &Options,
    timestamping: Option<&Timestamping<'_>>,
) -> Result<Signed> {
    let prepared = prepare(
        pdf,
        signer.certificate(),
        signer.chain(),
        signer.algorithm(),
        options,
    )?;
    let value = signer
        .sign(&prepared.message()?, prepared.digest())
        .map_err(Error::Signer)?;
    let (timestamp, timestamp_error) = match timestamping {
        None => (None, None),
        Some(timestamping) => match (timestamp(&value, timestamping), timestamping.policy) {
            (Ok(timestamp), _) => (Some(timestamp), None),
            (Err(Error::Timestamp(error)), Policy::BestEffort) => (None, Some(error)),
            (Err(error), _) => return Err(error),
        },
    };
    let pdf = prepared.finalize(&value, timestamp.as_ref().map(|t| t.token.as_slice()))?;
    Ok(Signed {
        pdf,
        timestamp,
        timestamp_error,
    })
}

/// [`Error::BrokenSignature`] for the first signature of `pdf` that is not intact.
fn refuse_broken(pdf: &[u8]) -> Result<()> {
    for check in verify_signatures(pdf)? {
        if let Integrity::Broken(problem) = check.integrity {
            return Err(Error::BrokenSignature {
                field: check.field,
                problem,
            });
        }
    }
    Ok(())
}

/// The room a document timestamp's token gets at first: a token with its authority's
/// certificates is a few kilobytes (7,649 bytes for GlobalSign's). A larger one is asked for
/// again, with the room it needs.
const TIMESTAMP_RESERVED: usize = 16_384;

/// Add a document timestamp (PAdES B-LTA; ISO 32000-2, 12.8.5) to `pdf`, in an incremental
/// revision: an invisible field `field_name` whose value, a `/DocTimeStamp`, holds an RFC 3161
/// token over the whole document but that token. Given after the validation data of the
/// signatures ([`add_validation_data`]), it makes that data provable after the certificates it
/// relies on expire; [`add_validation_data`] on its own field, then another timestamp, renews it.
///
/// The response is checked as a signature's timestamp is, against `trust`. A document with a
/// signature that is not intact is refused.
///
/// # Errors
/// [`Error::BrokenSignature`]; [`Error::Timestamp`] when no timestamp is had or the one had is
/// not accepted; [`Error::FieldName`], [`Error::FieldExists`]; [`Error::Pdf`] for a document
/// that takes no incremental revision.
pub fn add_document_timestamp(
    pdf: &[u8],
    field_name: &str,
    client: &dyn TimestampClient,
    trust: &Trust,
) -> Result<(Vec<u8>, Timestamp)> {
    refuse_broken(pdf)?;
    let mut reserved = TIMESTAMP_RESERVED;
    for _ in 0..2 {
        let mut laid = pdf::lay_out_timestamp(pdf, field_name, reserved)?;
        let request = TimestampRequest::over_parts(&laid.signed_parts())?;
        let response = client
            .timestamp(request.to_der())
            .map_err(TimestampError::Client)?;
        let timestamp = request.check_response(&response, trust)?;
        match pdf::fill(&mut laid, &timestamp.token) {
            Ok(()) => return Ok((laid.data, timestamp)),
            Err(Error::ContentsTooLarge { needed, .. }) => reserved = needed + 1024,
            Err(error) => return Err(error),
        }
    }
    Err(Error::ContentsTooLarge {
        needed: reserved,
        reserved,
    })
}

/// For fuzzing only: `data` read as a certificate, its issuer's certificate, then an OCSP
/// response or a CRL about it, all checked. Never panics, whatever the bytes.
#[doc(hidden)]
pub fn fuzz_revocation(data: &[u8]) {
    let Some((first, rest)) = tlv::element(data) else {
        return;
    };
    let Some((second, response)) = tlv::element(rest) else {
        return;
    };
    let (Some(cert), Some(issuer)) = (
        pki::Cert::from_der(first.whole),
        pki::Cert::from_der(second.whole),
    ) else {
        return;
    };
    let now = 1_800_000_000;
    let _ = revocation::check_ocsp(response, &cert, &issuer, now);
    let _ = revocation::check_crl(response, &cert, &issuer, now);
    let _ = revocation::ocsp_request(&cert, &issuer);
}
