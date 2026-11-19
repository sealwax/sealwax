//! Validation data (PAdES B-LT, ETSI EN 319 142-1; ISO 32000-2, 12.8.4.3): for a signature
//! already in the document, the certificates of its signer's path and of its timestamp
//! authority's, and for each of them but the anchors a proof it was not revoked, an OCSP response
//! (preferred) or a CRL, fetched and checked; all of it written into the document security store
//! (`/DSS`) in an incremental revision, with the signature's `/VRI` entry as Adobe reads it.
//!
//! Only certificates on a path checked up to an anchor are looked into: the addresses fetched
//! come from those, never from a certificate a document merely carries.

use std::collections::HashMap;

use sealwax_core::{Dict, Document, ObjId, Object, Stream};

use crate::pki::{self, Cert, Hash, PathFault};
use crate::revocation::{self, Refusal};
use crate::{Error, Result, TimestampError, Trust, date, pdf, signed_data};

const ID_AA_TIME_STAMP_TOKEN: const_oid::ObjectIdentifier =
    const_oid::ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.14");

/// The most certificates whose revocation is looked into for one signature (paths, and the
/// delegated responders that need it).
const MAX_COVERED: usize = 32;

/// What fetches revocation information: HTTP in production, from outside this crate, which
/// should only fetch the addresses of its own trust chains.
pub trait RevocationFetcher {
    /// The responder's answer (`OCSPResponse`, DER) to `request` (`OCSPRequest`, DER), posted to
    /// `url` (`application/ocsp-request`).
    ///
    /// # Errors
    /// Whatever keeps the response from coming back.
    fn ocsp(&self, url: &str, request: &[u8]) -> Result<Vec<u8>, String>;

    /// The CRL (DER) published at `url`.
    ///
    /// # Errors
    /// Whatever keeps the CRL from coming back.
    fn crl(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// How to gather validation data.
#[derive(Clone, Copy)]
pub struct Validation<'a> {
    pub fetcher: &'a dyn RevocationFetcher,
    /// The anchors of the signer and of its timestamp authority (and intermediates their
    /// signatures may leave out).
    pub trust: &'a Trust,
    /// The time the data must be current at, in seconds since 1970 (UTC); now when `None`.
    pub time: Option<i64>,
}

impl std::fmt::Debug for Validation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Validation")
            .field("trust", self.trust)
            .field("time", &self.time)
            .finish_non_exhaustive()
    }
}

/// Why validation data could not be gathered. Never document content: certificates are named by
/// their subject, sources by their address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ValidationError {
    /// The document has no signed field of that name.
    #[error("no signature in a field of that name")]
    NotSigned,
    /// The signature does not read; the part that does not is named.
    #[error("signature {0} cannot be read")]
    Malformed(&'static str),
    /// No path leads from the signer's certificate to an anchor.
    #[error("the signer's certificate is not trusted")]
    Untrusted,
    /// A certificate of the signer's path is not valid at the time asked.
    #[error("a certificate of the signer is not valid at the time asked")]
    Expired,
    /// The signature's timestamp does not check.
    #[error(transparent)]
    Timestamp(TimestampError),
    /// No proof could be had that this certificate was not revoked: each source tried, and why
    /// it failed.
    #[error("no revocation information for {certificate}: {reasons}")]
    Unavailable {
        certificate: String,
        reasons: String,
    },
    /// A responder or a CRL says this certificate is revoked.
    #[error("{certificate} is revoked")]
    Revoked { certificate: String },
    /// Covering the paths would take more proofs than sealwax gathers (delegated responders each
    /// needing one of their own): this certificate is left without one.
    #[error("more than {limit} certificates to cover; {certificate} left without a proof")]
    Incomplete { certificate: String, limit: usize },
}

/// What a signature's validation data holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Certificates of the signer's and the authority's paths, anchors included.
    pub certificates: usize,
    pub ocsp_responses: usize,
    pub crls: usize,
}

/// The validation data gathered for one signature.
struct Gathered {
    certificates: Vec<Vec<u8>>,
    ocsp_responses: Vec<Vec<u8>>,
    crls: Vec<Vec<u8>>,
}

/// The certificates to vouch for, each with its issuer: a path's links, its anchor left out.
fn links(path: &[&Cert], into: &mut Vec<(Cert, Cert)>) {
    for pair in path.windows(2) {
        if let [cert, issuer] = pair
            && !into.iter().any(|(known, _)| known.der == cert.der)
        {
            into.push(((*cert).clone(), (*issuer).clone()));
        }
    }
}

fn gather(
    contents: &[u8],
    validation: &Validation<'_>,
    now: u64,
    doc: &mut Document,
) -> Result<Gathered> {
    let signed = signed_data::read(contents, true).map_err(ValidationError::Malformed)?;
    let trust = validation.trust;
    let signer = signed
        .certificates
        .iter()
        .find(|cert| signed_data::identifies(&signed.signer.sid, cert))
        .ok_or(ValidationError::Malformed("signer certificate"))?;
    let pool: Vec<Cert> = signed
        .certificates
        .iter()
        .chain(trust.intermediates())
        .cloned()
        .collect();
    let path = pki::path(signer, &pool, trust.anchors(), now).map_err(|fault| match fault {
        PathFault::Untrusted => ValidationError::Untrusted,
        PathFault::Expired => ValidationError::Expired,
    })?;
    let mut certificates: Vec<Vec<u8>> = path.iter().map(|cert| cert.der.clone()).collect();
    let mut covered = Vec::new();
    links(&path, &mut covered);

    // The timestamp's authority: its path as the token's check found it.
    let token = signed
        .signer
        .unsigned_attributes
        .and_then(signed_data::attributes)
        .and_then(|attributes| {
            attributes
                .into_iter()
                .find(|(kind, _)| *kind == ID_AA_TIME_STAMP_TOKEN)
                .map(|(_, value)| value.whole)
        });
    if let Some(token) = token {
        let timestamp = crate::check_timestamp_token(token, signed.signer.signature, trust)
            .map_err(ValidationError::Timestamp)?;
        let chain: Vec<Cert> = timestamp
            .chain
            .iter()
            .filter_map(|der| Cert::from_der(der))
            .collect();
        for cert in &chain {
            if !certificates.contains(&cert.der) {
                certificates.push(cert.der.clone());
            }
        }
        links(&chain.iter().collect::<Vec<_>>(), &mut covered);
    }

    let mut ocsp_responses = Vec::new();
    let mut crls = Vec::new();
    let mut index = 0;
    while let Some((cert, issuer)) = covered.get(index).cloned() {
        index += 1;
        // Validation data that leaves a certificate uncovered is no proof: never returned.
        if index > MAX_COVERED {
            return Err(ValidationError::Incomplete {
                certificate: cert.subject(),
                limit: MAX_COVERED,
            }
            .into());
        }
        match vouch(&cert, &issuer, validation.fetcher, now, doc)? {
            Proof::Ocsp(response, responder) => {
                if !ocsp_responses.contains(&response) {
                    ocsp_responses.push(response);
                }
                // A delegated responder without `ocsp-nocheck` needs a proof of its own.
                if let Some(responder) = responder
                    && !covered.iter().any(|(known, _)| known.der == responder.der)
                {
                    certificates.push(responder.der.clone());
                    covered.push((*responder, issuer));
                }
            }
            Proof::Crl(crl) => {
                if !crls.contains(&crl) {
                    crls.push(crl);
                }
            }
        }
    }
    Ok(Gathered {
        certificates,
        ocsp_responses,
        crls,
    })
}

enum Proof {
    Ocsp(Vec<u8>, Option<Box<Cert>>),
    Crl(Vec<u8>),
}

/// A proof that `cert` was not revoked: OCSP first, each responder its issuer names, then each
/// CRL. A source that says it is revoked ends the search.
fn vouch(
    cert: &Cert,
    issuer: &Cert,
    fetcher: &dyn RevocationFetcher,
    now: u64,
    doc: &mut Document,
) -> Result<Proof> {
    let revoked = || ValidationError::Revoked {
        certificate: cert.subject(),
    };
    let mut reasons = Vec::new();
    if let Some(request) = revocation::ocsp_request(cert, issuer) {
        for url in cert.ocsp_urls() {
            doc.check_time()?;
            let response = match doc.external_call(|| fetcher.ocsp(&url, &request)) {
                Ok(response) => response,
                Err(error) => {
                    reasons.push(format!("OCSP {url}: {error}"));
                    continue;
                }
            };
            match revocation::check_ocsp(&response, cert, issuer, now) {
                Ok(responder) => return Ok(Proof::Ocsp(response, responder.map(Box::new))),
                Err(Refusal::Revoked) => return Err(revoked().into()),
                Err(refusal) => reasons.push(format!("OCSP {url}: {refusal}")),
            }
        }
    }
    for url in cert.crl_urls() {
        doc.check_time()?;
        let crl = match doc.external_call(|| fetcher.crl(&url)) {
            Ok(crl) => crl,
            Err(error) => {
                reasons.push(format!("CRL {url}: {error}"));
                continue;
            }
        };
        match revocation::check_crl(&crl, cert, issuer, now) {
            Ok(()) => return Ok(Proof::Crl(crl)),
            Err(Refusal::Revoked) => return Err(revoked().into()),
            Err(refusal) => reasons.push(format!("CRL {url}: {refusal}")),
        }
    }
    if reasons.is_empty() {
        reasons.push("no OCSP responder nor CRL named".to_owned());
    }
    Err(ValidationError::Unavailable {
        certificate: cert.subject(),
        reasons: reasons.join("; "),
    }
    .into())
}

/// A dictionary, direct or by reference, and its object number if indirect.
fn dict_at(doc: &Document, value: Option<&Object>) -> Result<(Option<ObjId>, Dict)> {
    Ok(match value {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Dict(dict)) => (Some(*id), dict),
            _ => (None, Dict::new()),
        },
        Some(Object::Dict(dict)) => (None, dict.clone()),
        _ => (None, Dict::new()),
    })
}

/// The streams an array of the store refers to, by the digest of their data (those whose data
/// is not filtered: the ones this crate writes).
fn known_streams(doc: &Document, array: &[Object]) -> Result<HashMap<Vec<u8>, ObjId>> {
    let mut known = HashMap::new();
    for item in array {
        doc.check_time()?;
        if let Object::Ref(id) = item
            && let Some(Object::Stream(stream)) = doc.object(*id)?
            && !stream.dict.contains_key(b"Filter")
        {
            known.insert(
                Hash::Sha256.digest_parts(&[&stream.data], doc.limits())?,
                *id,
            );
        }
    }
    Ok(known)
}

/// Each blob as a stream of the store's array `key`, reusing those already there: the
/// references, in order.
fn add_to_store(
    doc: &mut Document,
    store: &mut Dict,
    key: &str,
    blobs: &[Vec<u8>],
) -> Result<Vec<Object>> {
    let mut array = match store.get(key.as_bytes()) {
        Some(Object::Array(array)) => array.clone(),
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Array(array)) => array,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let mut known = known_streams(doc, &array)?;
    let mut references = Vec::new();
    for blob in blobs {
        let digest = Hash::Sha256.digest_parts(&[blob], doc.limits())?;
        let id = match known.get(&digest) {
            Some(id) => *id,
            None => {
                let id = doc.add_object(Object::Stream(Stream {
                    dict: Dict::new(),
                    data: blob.clone(),
                }))?;
                array.push(Object::Ref(id));
                known.insert(digest, id);
                id
            }
        };
        references.push(Object::Ref(id));
    }
    if !array.is_empty() {
        store.insert(key, array);
    }
    Ok(references)
}

/// Add the validation data of the signature in field `field_name` to `pdf`, in an incremental
/// revision: the store (`/DSS`) completed, never replaced, and the signature's `/VRI` entry. The
/// field may hold a document timestamp ([`crate::add_document_timestamp`]): its authority's path
/// is covered, what renews a B-LTA document.
///
/// # Errors
/// [`Error::Validation`] when the data cannot be gathered; [`Error::Pdf`] for a document that
/// takes no incremental revision.
pub fn add_validation_data(
    pdf: &[u8],
    field_name: &str,
    validation: &Validation<'_>,
) -> Result<(Vec<u8>, Coverage)> {
    crate::SigningInput::open(pdf.to_vec(), None)?.add_validation_data(field_name, validation)
}

impl crate::SigningInput {
    /// Gather validation data under the document limits, excluding OCSP and CRL waits.
    pub fn add_validation_data(
        self,
        field_name: &str,
        validation: &Validation<'_>,
    ) -> Result<(Vec<u8>, Coverage)> {
        let doc = &self.doc;
        let contents = pdf::signature_contents(doc, field_name)?
            .ok_or(Error::Validation(ValidationError::NotSigned))?;
        // Validation data proves nothing for a signature that is not intact.
        if let Some(check) = self
            .verify_signatures()?
            .into_iter()
            .find(|check| check.field == field_name)
            && let crate::Integrity::Broken(problem) = check.integrity
        {
            return Err(Error::BrokenSignature {
                field: check.field,
                problem,
            });
        }
        let time = validation.time.unwrap_or_else(date::now);
        let now = u64::try_from(time).unwrap_or(0);
        let mut doc = self.doc;
        let gathered = gather(&contents, validation, now, &mut doc)?;
        doc.check_time()?;

        let catalog_id = doc.catalog_id();
        let Some(Object::Dict(mut catalog)) = doc.object(catalog_id)? else {
            return Err(Error::Pdf(sealwax_core::Error::Malformed));
        };
        let (store_id, mut store) = dict_at(&doc, catalog.get(b"DSS"))?;
        let certificates = add_to_store(&mut doc, &mut store, "Certs", &gathered.certificates)?;
        let ocsp_responses = add_to_store(&mut doc, &mut store, "OCSPs", &gathered.ocsp_responses)?;
        let crls = add_to_store(&mut doc, &mut store, "CRLs", &gathered.crls)?;

        // The signature's entry, named by the SHA-1 of its /Contents as the file holds it.
        let mut entry = Dict::new();
        for (key, references) in [
            ("Cert", &certificates),
            ("OCSP", &ocsp_responses),
            ("CRL", &crls),
        ] {
            if !references.is_empty() {
                entry.insert(key, references.clone());
            }
        }
        entry.insert("TU", Object::String(date::pdf_date(time).into_bytes()));
        let key: String = Hash::Sha1
            .digest_parts(&[&contents], doc.limits())?
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect();
        let (vri_id, mut vri) = dict_at(&doc, store.get(b"VRI"))?;
        vri.insert(key.as_str(), entry);
        match vri_id {
            Some(id) => doc.set_object(id, Object::Dict(vri)),
            None => {
                store.insert("VRI", vri);
            }
        }
        match store_id {
            Some(id) => doc.set_object(id, Object::Dict(store)),
            None => {
                let id = doc.add_object(Object::Dict(store))?;
                catalog.insert("DSS", id);
                doc.set_object(catalog_id, Object::Dict(catalog));
            }
        }
        let data = doc.save_incremental()?;
        doc.check_time()?;
        Ok((
            data,
            Coverage {
                certificates: certificates.len(),
                ocsp_responses: ocsp_responses.len(),
                crls: crls.len(),
            },
        ))
    }
}
