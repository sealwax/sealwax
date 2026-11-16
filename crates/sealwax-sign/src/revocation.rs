//! Revocation information, fetched and checked before it goes into the document's validation
//! data: an OCSP response (RFC 6960) or a CRL (RFC 5280) saying a certificate was not revoked,
//! signed by its issuer or by a responder its issuer delegated, and current.
//!
//! A response is checked on the bytes received: what its signature covers is never re-encoded.

use const_oid::ObjectIdentifier;
use der::{Decode, Encode};
use x509_cert::crl::CertificateList;
use x509_cert::ext::pkix::crl::dp::IssuingDistributionPoint;
use x509_cert::ext::pkix::name::DistributionPointName;
use x509_cert::spki::AlgorithmIdentifierOwned;

use crate::pki::{self, Cert, Hash, Usage, generalized_time};
use crate::tlv;

const ID_PKIX_OCSP_BASIC: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1.1");
const ID_KP_OCSP_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.9");
/// `id-pkix-ocsp-nocheck`: the responder's own certificate needs no revocation information.
pub(crate) const ID_PKIX_OCSP_NOCHECK: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1.5");
const SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");

/// The clocks of a responder and of this machine may disagree by this much (seconds).
const SKEW: u64 = 300;

/// Why revocation information was not accepted. Never document content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// It does not read, or not as what it should be.
    Malformed,
    /// The responder answered with an error status (`OCSPResponseStatus`).
    Status(u32),
    /// It says nothing of this certificate.
    OtherCertificate,
    /// Its signature does not verify, or uses an algorithm not supported.
    Signature,
    /// Its signer is neither the issuer nor a responder the issuer delegated.
    NotAuthorized,
    /// It is not current: issued in the future, or past its next update.
    Stale,
    /// The certificate's status is unknown to the responder.
    Unknown,
    /// The certificate is revoked.
    Revoked,
    /// A CRL that may leave the certificate out though it is revoked: a delta CRL, one meant for
    /// other certificates or reasons, an indirect one, or one with a critical extension this
    /// crate does not apply. Its silence proves nothing.
    Scope,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "unreadable",
            Self::Status(_) => "error status",
            Self::OtherCertificate => "about another certificate",
            Self::Signature => "signature does not verify",
            Self::NotAuthorized => "signer not authorized",
            Self::Stale => "not current",
            Self::Unknown => "certificate unknown",
            Self::Revoked => "certificate revoked",
            Self::Scope => "a CRL that does not cover the certificate",
        })
    }
}

/// CRL extensions (RFC 5280, 5.2) and entry extensions (5.3) this crate knows.
const AUTHORITY_KEY_IDENTIFIER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.35");
const ISSUER_ALTERNATIVE_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.18");
const CRL_NUMBER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.20");
const DELTA_CRL_INDICATOR: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.27");
const ISSUING_DISTRIBUTION_POINT: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.28");
const REASON_CODE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.21");
const HOLD_INSTRUCTION_CODE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.23");
const INVALIDITY_DATE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.24");
const CERTIFICATE_ISSUER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.29");

/// Whether `list` is a complete CRL for `cert`: one whose silence says the certificate is not
/// revoked (RFC 5280, 5.2 and 6.3). Not a delta CRL (its base would be needed); not an indirect
/// CRL; not one restricted to some reasons, or to other kinds of certificates, or to a
/// distribution point the certificate does not name; no critical extension, on the list or on
/// one of its entries, that this crate does not apply.
fn covers(list: &CertificateList, cert: &Cert) -> bool {
    for extension in list.tbs_cert_list.crl_extensions.iter().flatten() {
        let id = extension.extn_id;
        if id == DELTA_CRL_INDICATOR {
            return false;
        }
        if id == ISSUING_DISTRIBUTION_POINT {
            let Ok(point) = IssuingDistributionPoint::from_der(extension.extn_value.as_bytes())
            else {
                return false;
            };
            let authority = cert.is_authority();
            if point.indirect_crl
                || point.only_contains_attribute_certs
                || point.only_some_reasons.is_some()
                || (point.only_contains_user_certs && authority)
                || (point.only_contains_ca_certs && !authority)
            {
                return false;
            }
            match &point.distribution_point {
                None => {}
                Some(DistributionPointName::FullName(names)) => {
                    let listed: Vec<String> = names.iter().filter_map(pki::http_url).collect();
                    if !cert.crl_urls().iter().any(|url| listed.contains(url)) {
                        return false;
                    }
                }
                Some(DistributionPointName::NameRelativeToCRLIssuer(_)) => return false,
            }
            continue;
        }
        if extension.critical
            && ![
                CRL_NUMBER,
                AUTHORITY_KEY_IDENTIFIER,
                ISSUER_ALTERNATIVE_NAME,
            ]
            .contains(&id)
        {
            return false;
        }
    }
    for entry in list.tbs_cert_list.revoked_certificates.iter().flatten() {
        for extension in entry.crl_entry_extensions.iter().flatten() {
            let id = extension.extn_id;
            if id == CERTIFICATE_ISSUER
                || (extension.critical
                    && ![REASON_CODE, HOLD_INSTRUCTION_CODE, INVALIDITY_DATE].contains(&id))
            {
                return false;
            }
        }
    }
    true
}

/// The SHA-1 `CertID` of `cert` (RFC 6960, 4.1.1; the hash every responder answers to: it only
/// names the certificate).
fn cert_id(cert: &Cert, issuer: &Cert) -> Option<Vec<u8>> {
    let issuer_name = cert.parsed.tbs_certificate.issuer.to_der().ok()?;
    let serial = cert.parsed.tbs_certificate.serial_number.to_der().ok()?;
    let algorithm = tlv::encode(
        tlv::SEQUENCE,
        &[SHA1.to_der().ok()?, der::asn1::Null.to_der().ok()?].concat(),
    );
    Some(tlv::encode(
        tlv::SEQUENCE,
        &[
            algorithm,
            tlv::encode(tlv::OCTET_STRING, &Hash::Sha1.digest(&issuer_name)),
            tlv::encode(tlv::OCTET_STRING, &Hash::Sha1.digest(issuer.key_bits())),
            serial,
        ]
        .concat(),
    ))
}

/// The OCSP request (DER) for `cert`, issued by `issuer`: one certificate, no nonce (responders
/// following RFC 5019 answer with responses made in advance).
pub(crate) fn ocsp_request(cert: &Cert, issuer: &Cert) -> Option<Vec<u8>> {
    let request = tlv::encode(tlv::SEQUENCE, &cert_id(cert, issuer)?);
    let list = tlv::encode(tlv::SEQUENCE, &request);
    let tbs = tlv::encode(tlv::SEQUENCE, &list);
    Some(tlv::encode(tlv::SEQUENCE, &tbs))
}

/// Whether a `CertID` element names `cert` the way `cert_id` does, whatever its hash algorithm.
fn names(id: &tlv::Element<'_>, cert: &Cert, issuer: &Cert) -> bool {
    let Some(fields) = tlv::children_of(id, tlv::SEQUENCE) else {
        return false;
    };
    let [algorithm, name_hash, key_hash, serial] = fields.as_slice() else {
        return false;
    };
    let Some(hash) = AlgorithmIdentifierOwned::from_der(algorithm.whole)
        .ok()
        .and_then(|algorithm| Hash::from_oid(algorithm.oid))
    else {
        return false;
    };
    let Ok(issuer_name) = cert.parsed.tbs_certificate.issuer.to_der() else {
        return false;
    };
    let Ok(expected_serial) = cert.parsed.tbs_certificate.serial_number.to_der() else {
        return false;
    };
    name_hash.tag == tlv::OCTET_STRING
        && key_hash.tag == tlv::OCTET_STRING
        && name_hash.content == hash.digest(&issuer_name)
        && key_hash.content == hash.digest(issuer.key_bits())
        && serial.whole == expected_serial
}

/// Whether `now` (seconds since 1970) falls between `this` and `next`, give or take the skew. No
/// next update: current only if issued within the last week.
fn current(this: u64, next: Option<u64>, now: u64) -> bool {
    const WEEK: u64 = 7 * 86_400;
    this <= now + SKEW && next.map_or(now <= this + WEEK, |next| now <= next + SKEW)
}

/// The OCSP response `response` checked for `cert`, issued by `issuer`, at `now`: the delegated
/// responder's certificate when one signed it and it is not exempt from revocation checking
/// (`id-pkix-ocsp-nocheck`), as it then needs revocation information of its own.
pub(crate) fn check_ocsp(
    response: &[u8],
    cert: &Cert,
    issuer: &Cert,
    now: u64,
) -> Result<Option<Cert>, Refusal> {
    use Refusal::Malformed;

    let outer = tlv::single(response)
        .and_then(|element| tlv::children_of(&element, tlv::SEQUENCE))
        .ok_or(Malformed)?;
    let status = outer
        .first()
        .filter(|status| status.tag == 0x0a && status.content.len() == 1)
        .map(|status| u32::from(status.content[0]))
        .ok_or(Malformed)?;
    if status != 0 {
        return Err(Refusal::Status(status));
    }
    let bytes = outer
        .get(1)
        .and_then(|explicit| tlv::children_of(explicit, tlv::context(0)))
        .and_then(|inner| match inner.as_slice() {
            [bytes] => tlv::children_of(bytes, tlv::SEQUENCE),
            _ => None,
        })
        .ok_or(Malformed)?;
    let [kind, octets] = bytes.as_slice() else {
        return Err(Malformed);
    };
    if ObjectIdentifier::from_der(kind.whole).ok() != Some(ID_PKIX_OCSP_BASIC)
        || octets.tag != tlv::OCTET_STRING
    {
        return Err(Malformed);
    }
    let basic = tlv::single(octets.content)
        .and_then(|element| tlv::children_of(&element, tlv::SEQUENCE))
        .ok_or(Malformed)?;
    let [tbs, algorithm, signature, rest @ ..] = basic.as_slice() else {
        return Err(Malformed);
    };
    let algorithm = AlgorithmIdentifierOwned::from_der(algorithm.whole).map_err(|_| Malformed)?;
    // A BIT STRING: its first byte counts the unused bits, none for a signature.
    let signature = match signature.content.split_first() {
        Some((0, value)) if signature.tag == 0x03 => value,
        _ => return Err(Malformed),
    };
    let mut embedded = Vec::new();
    if let Some(certs) = rest.iter().find(|field| field.tag == tlv::context(0)) {
        let sequence = tlv::children(certs.content)
            .and_then(|inner| inner.first().copied())
            .and_then(|sequence| tlv::children_of(&sequence, tlv::SEQUENCE))
            .ok_or(Malformed)?;
        embedded.extend(
            sequence
                .iter()
                .filter_map(|certificate| Cert::from_der(certificate.whole)),
        );
    }

    // ResponseData: [0] version, responderID, producedAt, responses, [1] extensions.
    let data = tlv::children_of(tbs, tlv::SEQUENCE).ok_or(Malformed)?;
    let data = match data.first() {
        Some(version) if version.tag == tlv::context(0) => &data[1..],
        _ => &data[..],
    };
    let [responder, produced, responses, ..] = data else {
        return Err(Malformed);
    };
    if produced.tag != tlv::GENERALIZED_TIME {
        return Err(Malformed);
    }
    let single = tlv::children_of(responses, tlv::SEQUENCE)
        .ok_or(Malformed)?
        .into_iter()
        .filter_map(|single| tlv::children_of(&single, tlv::SEQUENCE))
        .find(|fields| fields.first().is_some_and(|id| names(id, cert, issuer)))
        .ok_or(Refusal::OtherCertificate)?;
    let [_, status, this_update, rest @ ..] = single.as_slice() else {
        return Err(Malformed);
    };
    if this_update.tag != tlv::GENERALIZED_TIME {
        return Err(Malformed);
    }
    let this_update = generalized_time(this_update.content).ok_or(Malformed)?;
    let next_update = match rest.first() {
        Some(next) if next.tag == tlv::context(0) => Some(
            tlv::children(next.content)
                .and_then(|inner| inner.first().copied())
                .filter(|time| time.tag == tlv::GENERALIZED_TIME)
                .and_then(|time| generalized_time(time.content))
                .ok_or(Malformed)?,
        ),
        _ => None,
    };

    // Who signed: the issuer itself, or a responder it delegated.
    let by_issuer = match responder.tag {
        // [1] Name
        0xa1 => tlv::children(responder.content)
            .and_then(|inner| inner.first().copied())
            .is_some_and(|name| {
                issuer
                    .parsed
                    .tbs_certificate
                    .subject
                    .to_der()
                    .is_ok_and(|subject| subject == name.whole)
            }),
        // [2] KeyHash: the SHA-1 of the key's bits.
        0xa2 => tlv::children(responder.content)
            .and_then(|inner| inner.first().copied())
            .is_some_and(|hash| hash.content == Hash::Sha1.digest(issuer.key_bits())),
        _ => return Err(Malformed),
    };
    let message = tbs.whole;
    let verifies =
        |signer: &Cert| pki::verify(signer, &algorithm, None, message, signature).is_ok();
    let delegated = if by_issuer && verifies(issuer) {
        None
    } else {
        let responder = embedded
            .into_iter()
            .find(|candidate| verifies(candidate))
            .ok_or(Refusal::Signature)?;
        if !issuer.issued(&responder)
            || !responder.has_purpose(ID_KP_OCSP_SIGNING)
            || !responder.allows(Usage::Signing)
            || !responder.valid_at(now)
        {
            return Err(Refusal::NotAuthorized);
        }
        Some(responder)
    };

    if !current(this_update, next_update, now) {
        return Err(Refusal::Stale);
    }
    match status.tag {
        // good [0] IMPLICIT NULL
        0x80 => {}
        // revoked [1] IMPLICIT RevokedInfo
        0xa1 => return Err(Refusal::Revoked),
        _ => return Err(Refusal::Unknown),
    }
    Ok(delegated.filter(|responder| !responder.has_extension(ID_PKIX_OCSP_NOCHECK)))
}

/// The CRL `crl` checked for `cert`, issued by `issuer`, at `now`: signed by the issuer, whose key
/// usage, when given, allows signing CRLs, current, and not listing the certificate.
pub(crate) fn check_crl(crl: &[u8], cert: &Cert, issuer: &Cert, now: u64) -> Result<(), Refusal> {
    use Refusal::Malformed;

    let parts = tlv::single(crl)
        .and_then(|element| tlv::children_of(&element, tlv::SEQUENCE))
        .ok_or(Malformed)?;
    let [tbs, _, _] = parts.as_slice() else {
        return Err(Malformed);
    };
    let list = CertificateList::from_der(crl).map_err(|_| Malformed)?;
    if list.tbs_cert_list.issuer != cert.parsed.tbs_certificate.issuer
        || list.tbs_cert_list.issuer != issuer.parsed.tbs_certificate.subject
    {
        return Err(Refusal::OtherCertificate);
    }
    let signature = list.signature.as_bytes().ok_or(Malformed)?;
    pki::verify(
        issuer,
        &list.signature_algorithm,
        None,
        tbs.whole,
        signature,
    )
    .map_err(|_| Refusal::Signature)?;
    if !issuer.allows(Usage::CrlSigning) {
        return Err(Refusal::NotAuthorized);
    }
    let this_update = list.tbs_cert_list.this_update.to_unix_duration().as_secs();
    let next_update = list
        .tbs_cert_list
        .next_update
        .map(|time| time.to_unix_duration().as_secs());
    if !current(this_update, next_update, now) {
        return Err(Refusal::Stale);
    }
    if !covers(&list, cert) {
        return Err(Refusal::Scope);
    }
    let serial = &cert.parsed.tbs_certificate.serial_number;
    if list
        .tbs_cert_list
        .revoked_certificates
        .iter()
        .flatten()
        .any(|revoked| revoked.serial_number == *serial)
    {
        return Err(Refusal::Revoked);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::current;

    #[test]
    fn current_within_the_window_and_the_skew() {
        assert!(current(1000, Some(2000), 1500));
        assert!(current(1000, Some(2000), 2000 + 300));
        assert!(!current(1000, Some(2000), 2000 + 301));
        assert!(current(1000 + 300, Some(2000), 1000));
        assert!(!current(1000 + 301, Some(2000), 1000));
        assert!(current(1000, None, 1000 + 7 * 86_400));
        assert!(!current(1000, None, 1001 + 7 * 86_400));
    }
}
