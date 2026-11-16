//! RFC 3161 timestamps over a signature value (PAdES B-T): the request, and the response checked
//! before its token goes into the signature. The authority answers what it is asked, so nothing
//! of its answer is taken on trust: the status, the imprint (the token is over this signature
//! value), the nonce (this request's), the token's signature and the authority's certificate
//! (named by the token's signed attributes, meant for timestamping, valid at the token's time),
//! then a path from it to one of the anchors given. The authority's root is often not the
//! signer's.

use const_oid::ObjectIdentifier;
use der::{Decode, Encode};
use ring::rand::{SecureRandom, SystemRandom};
use x509_cert::spki::AlgorithmIdentifierOwned;

use crate::pki::{self, Cert, Fault, Hash, PathFault, generalized_time};
use crate::{Error, signed_data, tlv};

const ID_CT_TST_INFO: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.1.4");
const ID_CONTENT_TYPE: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.3");
const ID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const ID_SIGNING_CERTIFICATE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.12");
const ID_SIGNING_CERTIFICATE_V2: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.47");
const ID_KP_TIME_STAMPING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.8");
const SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");

/// Why a timestamp was not obtained, or not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TimestampError {
    /// The client could not get an answer from the authority.
    #[error("timestamp authority unreachable: {0}")]
    Client(String),
    /// The authority refused (`PKIStatus` 2 and above).
    #[error("timestamp refused by the authority (status {status})")]
    Refused { status: u32 },
    /// The response, or the token in it, does not read; the part that does not is named.
    #[error("timestamp {0} cannot be read")]
    Malformed(&'static str),
    /// The token is not over the signature value.
    #[error("the timestamp is not over the signature value")]
    Imprint,
    /// The token's nonce is not the request's: an answer to another request.
    #[error("the timestamp answers another request (nonce)")]
    Nonce,
    /// The token's signature does not verify.
    #[error("the timestamp's signature does not verify")]
    Signature,
    /// An algorithm, key or curve this crate does not check (SHA-1 among them).
    #[error("the timestamp uses an algorithm not supported")]
    Unsupported,
    /// The authority's certificate is not in the token, or not the one its signed attributes
    /// name, or not meant for timestamping.
    #[error("the timestamp authority's certificate is missing or not meant for timestamping")]
    Certificate,
    /// No path leads from the authority's certificate to an anchor.
    #[error("the timestamp authority is not trusted")]
    Untrusted,
    /// A certificate of the path is not valid at the token's time.
    #[error("a certificate of the timestamp authority is not valid at the timestamp's time")]
    Expired,
}

/// What a timestamp authority is trusted with: its anchors (roots), and intermediates the
/// token may leave out.
#[derive(Debug, Clone, Default)]
pub struct Trust {
    anchors: Vec<Cert>,
    intermediates: Vec<Cert>,
}

impl Trust {
    /// Anchors and intermediates, DER.
    ///
    /// # Errors
    /// [`Error::Certificate`] for one that does not read.
    pub fn new(anchors: &[Vec<u8>], intermediates: &[Vec<u8>]) -> Result<Self, Error> {
        let read = |list: &[Vec<u8>]| -> Result<Vec<Cert>, Error> {
            list.iter()
                .map(|der| Cert::from_der(der).ok_or(Error::Certificate))
                .collect()
        };
        Ok(Self {
            anchors: read(anchors)?,
            intermediates: read(intermediates)?,
        })
    }

    pub(crate) fn anchors(&self) -> &[Cert] {
        &self.anchors
    }

    pub(crate) fn intermediates(&self) -> &[Cert] {
        &self.intermediates
    }
}

/// A timestamp checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timestamp {
    /// The token (`TimeStampToken`, a CMS `SignedData`), DER, as the signature embeds it.
    pub token: Vec<u8>,
    /// The token's time (`genTime`), in seconds since 1970 (UTC), any fraction dropped.
    pub time: i64,
    /// The authority's certificate subject (RFC 4514).
    pub authority: String,
    /// The authority's certificate, then its issuers up to the anchor, DER: what validation data
    /// will have to cover.
    pub chain: Vec<Vec<u8>>,
}

/// A request for a timestamp over some bytes (a signature value), SHA-256, its nonce random,
/// the authority's certificate asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampRequest {
    der: Vec<u8>,
    data_digest: Vec<u8>,
    nonce: Vec<u8>,
}

impl TimestampRequest {
    /// A request over `data`, with a random nonce.
    ///
    /// # Errors
    /// [`Error::Timestamp`] when no random nonce can be drawn.
    pub fn new(data: &[u8]) -> Result<Self, Error> {
        let mut random = [0u8; 8];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| TimestampError::Client("no randomness for the nonce".to_owned()))?;
        Self::with_nonce(data, u64::from_be_bytes(random))
    }

    /// A request over `data`, with `nonce` (for tests).
    ///
    /// # Errors
    /// [`Error::Der`] should the encoding fail.
    pub fn with_nonce(data: &[u8], nonce: u64) -> Result<Self, Error> {
        Self::over_digest(Hash::Sha256.digest(data), nonce)
    }

    /// A request over `parts`, one after the other (the bytes a document timestamp covers),
    /// with a random nonce.
    pub(crate) fn over_parts(parts: &[&[u8]]) -> Result<Self, Error> {
        let mut random = [0u8; 8];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| TimestampError::Client("no randomness for the nonce".to_owned()))?;
        Self::over_digest(Hash::Sha256.digest_parts(parts), u64::from_be_bytes(random))
    }

    fn over_digest(data_digest: Vec<u8>, nonce: u64) -> Result<Self, Error> {
        let nonce = nonce.to_der()?;
        let imprint = tlv::encode(
            tlv::SEQUENCE,
            &[
                tlv::encode(tlv::SEQUENCE, &SHA256.to_der()?),
                tlv::encode(tlv::OCTET_STRING, &data_digest),
            ]
            .concat(),
        );
        let der = tlv::encode(
            tlv::SEQUENCE,
            &[
                1u8.to_der()?,
                imprint,
                nonce.clone(),
                // certReq: the authority's certificate in the token.
                true.to_der()?,
            ]
            .concat(),
        );
        Ok(Self {
            der,
            data_digest,
            nonce,
        })
    }

    /// The request, DER (`TimeStampReq`), as sent to the authority
    /// (`application/timestamp-query`).
    pub fn to_der(&self) -> &[u8] {
        &self.der
    }

    /// The authority's response (`TimeStampResp`, DER, `application/timestamp-reply`) checked
    /// against this request and `trust`: the token it holds, if it holds a good one.
    ///
    /// # Errors
    /// [`TimestampError`] for each check that fails.
    pub fn check_response(
        &self,
        response: &[u8],
        trust: &Trust,
    ) -> Result<Timestamp, TimestampError> {
        let response = tlv::single(response)
            .and_then(|element| tlv::children_of(&element, tlv::SEQUENCE))
            .ok_or(TimestampError::Malformed("response"))?;
        let status = response
            .first()
            .and_then(|info| tlv::children_of(info, tlv::SEQUENCE))
            .and_then(|info| info.first().and_then(tlv::small_integer))
            .ok_or(TimestampError::Malformed("response status"))?;
        // 0 granted, 1 granted with modifications.
        if status > 1 {
            return Err(TimestampError::Refused { status });
        }
        let token = response
            .get(1)
            .ok_or(TimestampError::Malformed("response token"))?;
        check_token(
            token.whole,
            Imprint::Digest(&self.data_digest),
            Some(&self.nonce),
            Some(trust),
        )
    }
}

/// What the token's imprint must be: the digest of the data (in parts, one after the other)
/// under the token's own hash algorithm, or this SHA-256 digest (the request's).
#[derive(Clone, Copy)]
pub(crate) enum Imprint<'a> {
    Data(&'a [&'a [u8]]),
    Digest(&'a [u8]),
}

/// A token already embedded (a signature's `signatureTimeStampToken`) checked over `data`, the
/// signature value it covers, against `trust`. Its nonce is not checked: the request is gone.
///
/// # Errors
/// [`TimestampError`] for each check that fails.
pub fn check_timestamp_token(
    token: &[u8],
    data: &[u8],
    trust: &Trust,
) -> Result<Timestamp, TimestampError> {
    check_token(token, Imprint::Data(&[data]), None, Some(trust))
}

/// The fields of a `TSTInfo` checked here.
struct TstInfo<'a> {
    imprint_algorithm: ObjectIdentifier,
    imprint: &'a [u8],
    time: u64,
    nonce: Option<&'a [u8]>,
}

fn tst_info(bytes: &[u8]) -> Option<TstInfo<'_>> {
    let fields = tlv::children_of(&tlv::single(bytes)?, tlv::SEQUENCE)?;
    let [version, _policy, imprint, _serial, time, rest @ ..] = fields.as_slice() else {
        return None;
    };
    if tlv::small_integer(version)? != 1 {
        return None;
    }
    let imprint = tlv::children_of(imprint, tlv::SEQUENCE)?;
    let [algorithm, hashed] = imprint.as_slice() else {
        return None;
    };
    if hashed.tag != tlv::OCTET_STRING || time.tag != tlv::GENERALIZED_TIME {
        return None;
    }
    Some(TstInfo {
        imprint_algorithm: AlgorithmIdentifierOwned::from_der(algorithm.whole)
            .ok()?
            .oid,
        imprint: hashed.content,
        time: generalized_time(time.content)?,
        nonce: rest
            .iter()
            .find(|field| field.tag == tlv::INTEGER)
            .map(|field| field.whole),
    })
}

/// The hash algorithm and digest of the first certificate an ESS `SigningCertificate` (v1:
/// SHA-1) or `SigningCertificateV2` (SHA-256 unless named) identifies.
fn essential_certificate<'a>(value: &tlv::Element<'a>, v2: bool) -> Option<(Hash, &'a [u8])> {
    let certs = tlv::children_of(value, tlv::SEQUENCE)?;
    let ids = tlv::children_of(certs.first()?, tlv::SEQUENCE)?;
    let id = tlv::children_of(ids.first()?, tlv::SEQUENCE)?;
    let first = id.first()?;
    if !v2 {
        return (first.tag == tlv::OCTET_STRING).then_some((Hash::Sha1, first.content));
    }
    if first.tag == tlv::OCTET_STRING {
        return Some((Hash::Sha256, first.content));
    }
    let algorithm = AlgorithmIdentifierOwned::from_der(first.whole).ok()?;
    let hash = id.get(1).filter(|hash| hash.tag == tlv::OCTET_STRING)?;
    Some((Hash::from_oid(algorithm.oid)?, hash.content))
}

/// A token checked over `imprint`: what it says, that its authority signed it, and, with
/// `trust`, that the authority is trusted. Without, the authority's certificate is the token's
/// own and the chain returned holds it alone: the token is intact, nothing more is said.
pub(crate) fn check_token(
    token: &[u8],
    imprint: Imprint<'_>,
    nonce: Option<&[u8]>,
    trust: Option<&Trust>,
) -> Result<Timestamp, TimestampError> {
    use TimestampError::Malformed;

    let signed = signed_data::read(token, false).map_err(Malformed)?;
    if signed.content_type != ID_CT_TST_INFO {
        return Err(Malformed("signed data"));
    }
    let tst_bytes = signed.content.ok_or(Malformed("signed data"))?;
    let embedded = signed.certificates;
    let signer = signed.signer;

    // What the token says: over these bytes, at this time, for this request.
    let tst = tst_info(tst_bytes).ok_or(Malformed("TSTInfo"))?;
    let imprint_ok = match imprint {
        Imprint::Digest(digest) => tst.imprint_algorithm == SHA256 && tst.imprint == digest,
        Imprint::Data(parts) => match Hash::from_oid(tst.imprint_algorithm) {
            // An imprint this crate does not check is no proof either way.
            None | Some(Hash::Sha1) => return Err(TimestampError::Unsupported),
            Some(hash) => hash.digest_parts(parts) == tst.imprint,
        },
    };
    if !imprint_ok {
        return Err(TimestampError::Imprint);
    }
    if let Some(expected) = nonce
        && tst.nonce != Some(expected)
    {
        return Err(TimestampError::Nonce);
    }

    // That the authority said it: the signed attributes bind the TSTInfo, its key signed them.
    let digest = Hash::from_oid(signer.digest_algorithm)
        .filter(|hash| *hash != Hash::Sha1)
        .ok_or(TimestampError::Unsupported)?;
    // A token's signer has signed attributes (RFC 3161, 2.4.2: ESS signing certificate).
    let signed_attributes = signer
        .signed_attributes
        .ok_or(Malformed("signed attributes"))?;
    let attributes =
        signed_data::attributes(signed_attributes).ok_or(Malformed("signed attributes"))?;
    let value = |oid| {
        attributes
            .iter()
            .find(|(kind, _)| *kind == oid)
            .map(|(_, value)| *value)
    };
    let content_type_ok = value(ID_CONTENT_TYPE)
        .and_then(|value| ObjectIdentifier::from_der(value.whole).ok())
        == Some(ID_CT_TST_INFO);
    let digest_ok = value(ID_MESSAGE_DIGEST)
        .filter(|value| value.tag == tlv::OCTET_STRING)
        .is_some_and(|value| value.content == digest.digest(tst_bytes));
    if !content_type_ok || !digest_ok {
        return Err(TimestampError::Signature);
    }
    let empty = Trust::default();
    let given = trust.unwrap_or(&empty);
    let tsa = embedded
        .iter()
        .chain(&given.intermediates)
        .chain(&given.anchors)
        .find(|cert| signed_data::identifies(&signer.sid, cert))
        .ok_or(TimestampError::Certificate)?;
    let message = tlv::encode(tlv::SET, signed_attributes);
    pki::verify(
        tsa,
        &signer.signature_algorithm,
        Some(digest),
        &message,
        signer.signature,
    )
    .map_err(|fault| match fault {
        Fault::Unsupported => TimestampError::Unsupported,
        Fault::Invalid => TimestampError::Signature,
    })?;
    // The certificate the signed attributes name is the one that verified (RFC 3161, 2.4.1).
    let named = value(ID_SIGNING_CERTIFICATE_V2)
        .map(|value| essential_certificate(&value, true))
        .or_else(|| {
            value(ID_SIGNING_CERTIFICATE).map(|value| essential_certificate(&value, false))
        });
    match named {
        Some(Some((hash, digest))) if hash.digest(&tsa.der) == digest => {}
        _ => return Err(TimestampError::Certificate),
    }
    if !tsa.has_purpose(ID_KP_TIME_STAMPING) {
        return Err(TimestampError::Certificate);
    }

    // That the authority is one trusted, its certificates valid at the token's time.
    let pool: Vec<Cert> = embedded
        .iter()
        .chain(&given.intermediates)
        .cloned()
        .collect();
    let chain = match trust {
        Some(trust) => {
            pki::path(tsa, &pool, &trust.anchors, tst.time).map_err(|fault| match fault {
                PathFault::Untrusted => TimestampError::Untrusted,
                PathFault::Expired => TimestampError::Expired,
            })?
        }
        None => vec![tsa],
    };
    Ok(Timestamp {
        token: token.to_vec(),
        time: i64::try_from(tst.time).map_err(|_| Malformed("TSTInfo"))?,
        authority: tsa.parsed.tbs_certificate.subject.to_string(),
        chain: chain.iter().map(|cert| cert.der.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::TimestampRequest;
    use crate::pki::generalized_time;
    use crate::tlv;

    #[test]
    fn times_with_and_without_fractions() {
        assert_eq!(generalized_time(b"19700101000000Z"), Some(0));
        assert_eq!(
            generalized_time(b"20261115123456.789Z"),
            Some(1_794_746_096)
        );
        assert_eq!(generalized_time(b"20261115123456Z"), Some(1_794_746_096));
        assert_eq!(generalized_time(b"20261115123456"), None, "local time");
        assert_eq!(generalized_time(b"20261115123456.Z"), None);
    }

    #[test]
    fn a_request_asks_sha256_a_nonce_and_the_certificate() {
        let request = TimestampRequest::with_nonce(b"value", 0x8000_0000_0000_0001).unwrap();
        let element = tlv::single(request.to_der()).unwrap();
        let fields = tlv::children_of(&element, tlv::SEQUENCE).unwrap();
        assert_eq!(fields.len(), 4);
        assert_eq!(tlv::small_integer(&fields[0]), Some(1));
        // A positive nonce, its high bit set: a leading zero.
        assert_eq!(fields[2].content, [0, 0x80, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(fields[3].whole, [0x01, 0x01, 0xff]);
    }
}
