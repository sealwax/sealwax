//! The integrity of the signatures a document holds: that each covers its whole revision, but its
//! own value, that the bytes it covers have the digest it signed, and that its value verifies
//! with its signer's key; then how many revisions followed. Whom the signer is, and whether to
//! trust them, is not looked at: that needs a trust store and revocation data
//! ([`crate::add_validation_data`] gathers them).

use const_oid::ObjectIdentifier;
use sealwax_core::{Document, Object, OpenOptions};

use crate::pki::{self, Fault, Hash};
use crate::tsp::{self, Imprint};
use crate::{Result, TimestampError, pdf, signed_data};

const ID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const ID_CONTENT_TYPE: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.3");
const ID_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.1");
const ID_AA_TIME_STAMP_TOKEN: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.14");
/// Why a timestamp whose signature is not in question is not checked: its authority's certificate
/// is not in the token, not the one it names, or not meant for timestamping.
const TIMESTAMP_CERTIFICATE: &str =
    "a timestamp whose authority's certificate is missing or not meant for timestamping";

/// What a signature is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignatureKind {
    /// The certification (DocMDP).
    Certification,
    /// An approval.
    Approval,
    /// A document timestamp (`/DocTimeStamp`, PAdES B-LTA).
    DocumentTimestamp,
}

/// What became of a signature's check.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Integrity {
    /// It covers its revision, the digest matches, the value verifies.
    Intact,
    /// It does not: the bytes it covers are not those it signed, or not those it says.
    Broken(Broken),
    /// It could not be checked: a format or an algorithm this crate does not check (SHA-1).
    Unchecked(&'static str),
}

/// Why a signature is not intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Broken {
    /// The byte range is not the whole revision but the signature's own value: it leaves bytes
    /// out, points outside the file, or does not end where a revision does.
    #[error("the byte range does not cover the revision")]
    ByteRange,
    /// The signature does not read.
    #[error("the signature does not read")]
    Malformed,
    /// The bytes covered do not have the digest that was signed: changed since.
    #[error("the signed bytes were changed")]
    Digest,
    /// The value does not verify with the signer's key, or the signer's certificate is missing.
    #[error("the signature value does not verify")]
    Signature,
    /// The signature's timestamp is not over its value, or its own signature does not verify.
    #[error("the signature's timestamp does not verify")]
    Timestamp,
}

/// A signature of the document, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureCheck {
    /// The field's full name.
    pub field: String,
    pub kind: SignatureKind,
    /// The signature's `/SubFilter` (`ETSI.CAdES.detached`, `ETSI.RFC3161`...).
    pub sub_filter: String,
    /// The signer's certificate subject (the authority's, for a document timestamp), when found.
    pub signer: Option<String>,
    /// The time of its timestamp (`genTime`, seconds since 1970, UTC), when it has one.
    pub timestamp_time: Option<i64>,
    /// The revisions appended after the one it signed.
    pub later_revisions: usize,
    pub integrity: Integrity,
}

impl SignatureCheck {
    /// Whether the check found it intact.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.integrity == Integrity::Intact
    }
}

/// The signatures of `pdf`, each checked, in the order they were made: by where the revision
/// each signed ends (one whose byte range does not read comes last).
///
/// # Errors
/// [`crate::Error::Pdf`] when the document does not open.
pub fn verify_signatures(pdf: &[u8]) -> Result<Vec<SignatureCheck>> {
    let doc = Document::open(pdf.to_vec(), &OpenOptions::default())?;
    let certification = pdf::certification(&doc)?;
    let encrypted = doc.is_encrypted();
    let mut checks: Vec<(usize, SignatureCheck)> = Vec::new();
    for (field, id, value) in pdf::signatures(&doc)? {
        doc.check_time()?;
        let sub_filter = value
            .get(b"SubFilter")
            .and_then(Object::as_name)
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default();
        let timestamp = value.get(b"Type").and_then(Object::as_name) == Some(b"DocTimeStamp")
            || sub_filter == "ETSI.RFC3161";
        let kind = if timestamp {
            SignatureKind::DocumentTimestamp
        } else if id.is_some() && id == certification {
            SignatureKind::Certification
        } else {
            SignatureKind::Approval
        };
        let mut check = SignatureCheck {
            field,
            kind,
            sub_filter: sub_filter.clone(),
            signer: None,
            timestamp_time: None,
            later_revisions: 0,
            integrity: Integrity::Intact,
        };
        let range = value
            .get(b"ByteRange")
            .and_then(Object::as_array)
            .and_then(|range| {
                range
                    .iter()
                    .map(|n| n.as_i64().and_then(|n| usize::try_from(n).ok()))
                    .collect::<Option<Vec<usize>>>()
            });
        let contents = match value.get(b"Contents") {
            Some(Object::String(contents)) => Some(contents.as_slice()),
            _ => None,
        };
        let (Some(range), Some(contents)) = (range, contents) else {
            check.integrity = Integrity::Broken(Broken::Malformed);
            checks.push((usize::MAX, check));
            continue;
        };
        let order = match range.as_slice() {
            [_, _, start, length] => start.saturating_add(*length),
            _ => usize::MAX,
        };
        let (parts, contents) = match covered(pdf, &range, contents, encrypted) {
            Some(covered) => {
                check.later_revisions = covered.later;
                (covered.parts, covered.value)
            }
            None => {
                check.integrity = Integrity::Broken(Broken::ByteRange);
                checks.push((order, check));
                continue;
            }
        };
        check.integrity = match sub_filter.as_str() {
            "ETSI.RFC3161" => document_timestamp(&contents, &parts, &mut check),
            "ETSI.CAdES.detached" | "adbe.pkcs7.detached" if !timestamp => {
                detached(&contents, &parts, &mut check)
            }
            _ => Integrity::Unchecked("a signature format this crate does not check"),
        };
        checks.push((order, check));
    }
    checks.sort_by_key(|(order, _)| *order);
    Ok(checks.into_iter().map(|(_, check)| check).collect())
}

/// What a byte range covers.
struct Covered<'a> {
    /// The two parts of the file signed.
    parts: [&'a [u8]; 2],
    /// The revisions appended after.
    later: usize,
    /// The signature's value, as the gap between the parts holds it.
    value: Vec<u8>,
}

/// The two parts of `pdf` that `range` covers, when it covers a whole revision but `/Contents`,
/// the revisions after it, and the signature's value as the gap between them holds it. The gap
/// must be `/Contents` (`contents`, its value as read), with or without its brackets, and
/// nothing else. In
/// an encrypted document the value is the gap's alone: signature values are not encrypted, but
/// readers decrypt every string they read.
fn covered<'a>(
    pdf: &'a [u8],
    range: &[usize],
    contents: &[u8],
    encrypted: bool,
) -> Option<Covered<'a>> {
    let [0, first_length, second_start, second_length] = *range else {
        return None;
    };
    let end = second_start.checked_add(second_length)?;
    if first_length >= second_start || end > pdf.len() {
        return None;
    }
    let gap = pdf.get(first_length..second_start)?;
    let digits = match gap {
        [b'<', digits @ .., b'>'] => digits,
        // Some signers leave the brackets in the signed bytes: the gap is the digits alone.
        digits
            if pdf.get(first_length.checked_sub(1)?) == Some(&b'<')
                && pdf.get(second_start) == Some(&b'>') =>
        {
            digits
        }
        _ => return None,
    };
    let digits: Vec<u8> = digits
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if !digits.len().is_multiple_of(2) || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let value: Vec<u8> = digits
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap_or("00");
            u8::from_str_radix(text, 16).unwrap_or(0)
        })
        .collect();
    if !encrypted && value != contents {
        return None;
    }
    // The range ends where a revision does: `%%EOF` and its line break.
    let signed = pdf.get(..end)?;
    let trimmed = signed.trim_ascii_end();
    if !trimmed.ends_with(b"%%EOF") || signed.len() - trimmed.len() > 2 {
        return None;
    }
    let rest = pdf.get(end..)?;
    let later = rest.windows(5).filter(|window| *window == b"%%EOF").count();
    Some(Covered {
        parts: [pdf.get(..first_length)?, pdf.get(second_start..end)?],
        later,
        value,
    })
}

/// A CMS signature (`ETSI.CAdES.detached`, `adbe.pkcs7.detached`) over `parts`.
fn detached(contents: &[u8], parts: &[&[u8]; 2], check: &mut SignatureCheck) -> Integrity {
    let Ok(signed) = signed_data::read(contents, true) else {
        return Integrity::Broken(Broken::Malformed);
    };
    let signer = &signed.signer;
    let Some(digest) = Hash::from_oid(signer.digest_algorithm) else {
        return Integrity::Unchecked("a digest algorithm this crate does not check");
    };
    if digest == Hash::Sha1 {
        return Integrity::Unchecked("SHA-1");
    }
    // What the value signs: the signed attributes, whose `messageDigest` is that of the signed
    // bytes; or, without them (PKCS #7), the signed bytes themselves.
    let message = match signer.signed_attributes {
        Some(signed_attributes) => {
            let Some(attributes) = signed_data::attributes(signed_attributes) else {
                return Integrity::Broken(Broken::Malformed);
            };
            let value = |oid| {
                attributes
                    .iter()
                    .find(|(kind, _)| *kind == oid)
                    .map(|(_, value)| *value)
            };
            let content_type = value(ID_CONTENT_TYPE)
                .and_then(|value| der::Decode::from_der(value.whole).ok())
                .map(|oid: ObjectIdentifier| oid == ID_DATA);
            if content_type == Some(false) {
                return Integrity::Broken(Broken::Malformed);
            }
            let signed_digest =
                value(ID_MESSAGE_DIGEST).filter(|value| value.tag == crate::tlv::OCTET_STRING);
            match signed_digest {
                Some(signed_digest) if signed_digest.content == digest.digest_parts(parts) => {}
                Some(_) => return Integrity::Broken(Broken::Digest),
                None => return Integrity::Broken(Broken::Malformed),
            }
            crate::tlv::encode(crate::tlv::SET, signed_attributes)
        }
        None => parts.concat(),
    };
    let Some(certificate) = signed
        .certificates
        .iter()
        .find(|cert| signed_data::identifies(&signer.sid, cert))
    else {
        return Integrity::Broken(Broken::Signature);
    };
    check.signer = Some(certificate.subject());
    match pki::verify(
        certificate,
        &signer.signature_algorithm,
        Some(digest),
        &message,
        signer.signature,
    ) {
        Ok(()) => {}
        Err(Fault::Unsupported) => {
            return Integrity::Unchecked("a signature algorithm this crate does not check");
        }
        Err(Fault::Invalid) => return Integrity::Broken(Broken::Signature),
    }
    // Its timestamp, when it has one: over the signature value, its own signature intact.
    let token = signer
        .unsigned_attributes
        .and_then(signed_data::attributes)
        .and_then(|attributes| {
            attributes
                .into_iter()
                .find(|(kind, _)| *kind == ID_AA_TIME_STAMP_TOKEN)
                .map(|(_, value)| value.whole)
        });
    if let Some(token) = token {
        match tsp::check_token(token, Imprint::Data(&[signer.signature]), None, None) {
            Ok(timestamp) => check.timestamp_time = Some(timestamp.time),
            Err(TimestampError::Unsupported) => {
                return Integrity::Unchecked("a timestamp algorithm this crate does not check");
            }
            Err(TimestampError::Certificate) => {
                return Integrity::Unchecked(TIMESTAMP_CERTIFICATE);
            }
            Err(_) => return Integrity::Broken(Broken::Timestamp),
        }
    }
    Integrity::Intact
}

/// A document timestamp (`ETSI.RFC3161`): a token whose imprint is the digest of `parts`.
fn document_timestamp(
    contents: &[u8],
    parts: &[&[u8]; 2],
    check: &mut SignatureCheck,
) -> Integrity {
    // The token is padded with zeros: the one element it starts with.
    let Some((element, _)) = crate::tlv::element(contents) else {
        return Integrity::Broken(Broken::Malformed);
    };
    match tsp::check_token(element.whole, Imprint::Data(parts), None, None) {
        Ok(timestamp) => {
            check.signer = Some(timestamp.authority);
            check.timestamp_time = Some(timestamp.time);
            Integrity::Intact
        }
        Err(TimestampError::Imprint) => Integrity::Broken(Broken::Digest),
        Err(TimestampError::Signature) => Integrity::Broken(Broken::Signature),
        Err(TimestampError::Certificate) => Integrity::Unchecked(TIMESTAMP_CERTIFICATE),
        Err(TimestampError::Unsupported) => {
            Integrity::Unchecked("a timestamp algorithm this crate does not check")
        }
        Err(_) => Integrity::Broken(Broken::Malformed),
    }
}
