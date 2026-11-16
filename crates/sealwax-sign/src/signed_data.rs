//! A CMS `SignedData` (RFC 5652) read as received, one signer: a timestamp token, or the
//! signature a PDF signature dictionary holds. What its signature covers is kept as bytes.

use const_oid::ObjectIdentifier;
use der::Decode;
use x509_cert::spki::AlgorithmIdentifierOwned;

use crate::pki::Cert;
use crate::tlv;

const ID_SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");

/// The parts of a `SignerInfo` read here, as received.
pub(crate) struct SignerInfo<'a> {
    pub(crate) sid: tlv::Element<'a>,
    pub(crate) digest_algorithm: ObjectIdentifier,
    /// The content of the signed attributes (`[0] IMPLICIT SET OF`); `None` when the signer
    /// signed the content's digest directly (PKCS #7, allowed by CMS for `id-data`).
    pub(crate) signed_attributes: Option<&'a [u8]>,
    pub(crate) signature_algorithm: AlgorithmIdentifierOwned,
    pub(crate) signature: &'a [u8],
    /// The content of the unsigned attributes (`[1] IMPLICIT SET OF`), if any.
    pub(crate) unsigned_attributes: Option<&'a [u8]>,
}

/// A `SignedData` read.
pub(crate) struct SignedData<'a> {
    pub(crate) content_type: ObjectIdentifier,
    /// The encapsulated content's octets; none when detached.
    pub(crate) content: Option<&'a [u8]>,
    /// The certificates it carries that read (attribute certificates and the like left aside).
    pub(crate) certificates: Vec<Cert>,
    pub(crate) signer: SignerInfo<'a>,
}

fn signer_info<'a>(element: &tlv::Element<'a>) -> Option<SignerInfo<'a>> {
    let fields = tlv::children_of(element, tlv::SEQUENCE)?;
    let [_version, sid, digest, after_digest @ ..] = fields.as_slice() else {
        return None;
    };
    let (attributes, after_attributes) = match after_digest {
        [first, rest @ ..] if first.tag == tlv::context(0) => (Some(first.content), rest),
        rest => (None, rest),
    };
    let [algorithm, signature, rest @ ..] = after_attributes else {
        return None;
    };
    if signature.tag != tlv::OCTET_STRING {
        return None;
    }
    Some(SignerInfo {
        sid: *sid,
        digest_algorithm: AlgorithmIdentifierOwned::from_der(digest.whole).ok()?.oid,
        signed_attributes: attributes,
        signature_algorithm: AlgorithmIdentifierOwned::from_der(algorithm.whole).ok()?,
        signature: signature.content,
        unsigned_attributes: rest
            .iter()
            .find(|field| field.tag == tlv::context(1))
            .map(|field| field.content),
    })
}

/// The `SignedData` a `ContentInfo` holds, the one element `bytes` starts with (what follows it,
/// the zeros padding a PDF's `/Contents`, is ignored when `padded`). The error names the part
/// that does not read.
pub(crate) fn read(bytes: &[u8], padded: bool) -> Result<SignedData<'_>, &'static str> {
    let element = if padded {
        tlv::element(bytes).map(|(element, _)| element)
    } else {
        tlv::single(bytes)
    };
    let info = element
        .and_then(|element| tlv::children_of(&element, tlv::SEQUENCE))
        .ok_or("content info")?;
    let [kind, content] = info.as_slice() else {
        return Err("content info");
    };
    if ObjectIdentifier::from_der(kind.whole).ok() != Some(ID_SIGNED_DATA) {
        return Err("content info");
    }
    let signed = tlv::children_of(content, tlv::context(0))
        .and_then(|inner| match inner.as_slice() {
            [signed] => tlv::children_of(signed, tlv::SEQUENCE),
            _ => None,
        })
        .ok_or("signed data")?;
    let [_version, _digests, encapsulated, rest @ ..] = signed.as_slice() else {
        return Err("signed data");
    };
    let encapsulated = tlv::children_of(encapsulated, tlv::SEQUENCE).ok_or("signed data")?;
    let (content_type, content) = match encapsulated.as_slice() {
        [content_type] => (content_type, None),
        [content_type, explicit] => (
            content_type,
            Some(
                tlv::children_of(explicit, tlv::context(0))
                    .and_then(|inner| match inner.as_slice() {
                        [octets] if octets.tag == tlv::OCTET_STRING => Some(octets.content),
                        _ => None,
                    })
                    .ok_or("signed data")?,
            ),
        ),
        _ => return Err("signed data"),
    };
    let content_type = ObjectIdentifier::from_der(content_type.whole).map_err(|_| "signed data")?;
    let mut certificates = Vec::new();
    let mut signer_infos = None;
    for field in rest {
        if field.tag == tlv::context(0) {
            for certificate in tlv::children(field.content).ok_or("certificates")? {
                if let Some(cert) = Cert::from_der(certificate.whole) {
                    certificates.push(cert);
                }
            }
        } else if field.tag == tlv::SET {
            signer_infos = tlv::children(field.content);
        }
    }
    let signer = signer_infos
        .as_deref()
        .and_then(|infos| match infos {
            [only] => signer_info(only),
            _ => None,
        })
        .ok_or("signer info")?;
    Ok(SignedData {
        content_type,
        content,
        certificates,
        signer,
    })
}

/// The first value of each attribute of a `SET OF Attribute`'s content, by type.
pub(crate) fn attributes(content: &[u8]) -> Option<Vec<(ObjectIdentifier, tlv::Element<'_>)>> {
    tlv::children(content)?
        .iter()
        .map(|attribute| {
            let parts = tlv::children_of(attribute, tlv::SEQUENCE)?;
            let [kind, values] = parts.as_slice() else {
                return None;
            };
            let value = *tlv::children_of(values, tlv::SET)?.first()?;
            Some((ObjectIdentifier::from_der(kind.whole).ok()?, value))
        })
        .collect()
}

/// Whether `cert` is the one a signer identifier names.
pub(crate) fn identifies(sid: &tlv::Element<'_>, cert: &Cert) -> bool {
    if sid.tag == tlv::SEQUENCE {
        cms::cert::IssuerAndSerialNumber::from_der(sid.whole).is_ok_and(|named| {
            named.issuer == cert.parsed.tbs_certificate.issuer
                && named.serial_number == cert.parsed.tbs_certificate.serial_number
        })
    } else if sid.tag == tlv::context_primitive(0) {
        cert.key_identifier().as_deref() == Some(sid.content)
    } else {
        false
    }
}
