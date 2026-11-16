//! Certificates and signature values checked: a signature over bytes with a certificate's key
//! (RSA PKCS #1 v1.5 and PSS, ECDSA P-256 and P-384, SHA-2; SHA-1 refused), and a path from a
//! certificate up to a trust anchor, each link's signature checked and each certificate valid at
//! the time asked. Revocation is not looked at here: that is the validation data's business.

use const_oid::ObjectIdentifier;
use der::Decode;
use ring::digest;
use ring::signature::{self, UnparsedPublicKey, VerificationAlgorithm};
use x509_cert::Certificate;
use x509_cert::ext::pkix::crl::dp::DistributionPoint;
use x509_cert::ext::pkix::name::{DistributionPointName, GeneralName};
use x509_cert::ext::pkix::{
    AuthorityInfoAccessSyntax, BasicConstraints, CrlDistributionPoints, ExtendedKeyUsage, KeyUsage,
    SubjectKeyIdentifier,
};
use x509_cert::spki::AlgorithmIdentifierOwned;

use crate::tlv;

const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const RSASSA_PSS: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.10");
const SHA256_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
const SHA384_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.12");
const SHA512_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.13");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const ECDSA_WITH_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");
const ECDSA_WITH_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.4");
const P256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const P384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
const SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");
const SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2");
const SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.3");
const BASIC_CONSTRAINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.19");
const EXTENDED_KEY_USAGE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.37");
const SUBJECT_KEY_IDENTIFIER: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.14");
const AUTHORITY_INFO_ACCESS: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.1.1");
const AD_OCSP: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1");
const CRL_DISTRIBUTION_POINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.31");
const KEY_USAGE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.15");
const NAME_CONSTRAINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.30");
/// The extensions whose meaning a path check here takes into account, or that only inform: a
/// critical extension not among them refuses its certificate (RFC 5280, 4.2).
const UNDERSTOOD_EXTENSIONS: [ObjectIdentifier; 11] = [
    BASIC_CONSTRAINTS,
    KEY_USAGE,
    EXTENDED_KEY_USAGE,
    SUBJECT_KEY_IDENTIFIER,
    ObjectIdentifier::new_unwrap("2.5.29.35"), // authority key identifier
    ObjectIdentifier::new_unwrap("2.5.29.17"), // subject alternative name
    ObjectIdentifier::new_unwrap("2.5.29.18"), // issuer alternative name
    // Certificate policies: any policy is accepted (no explicit policy is required), which
    // processing them would conclude without policy mappings or constraints, refused below.
    ObjectIdentifier::new_unwrap("2.5.29.32"),
    CRL_DISTRIBUTION_POINTS,
    AUTHORITY_INFO_ACCESS,
    ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1.5"), // OCSP no check
];

/// The longest path tried, anchor included.
const MAX_DEPTH: usize = 8;

/// The most signatures checked while looking for a path: certificates sent by a party not yet
/// trusted (a token's) could otherwise make the search exponential.
const MAX_CHECKS: usize = 256;

/// A digest algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hash {
    /// To identify a certificate only (ESS `SigningCertificate`), never to sign.
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub(crate) fn from_oid(oid: ObjectIdentifier) -> Option<Self> {
        match oid {
            SHA1 => Some(Self::Sha1),
            SHA256 => Some(Self::Sha256),
            SHA384 => Some(Self::Sha384),
            SHA512 => Some(Self::Sha512),
            _ => None,
        }
    }

    pub(crate) fn digest(self, data: &[u8]) -> Vec<u8> {
        self.digest_parts(&[data])
    }

    /// The digest of `parts` one after the other.
    pub(crate) fn digest_parts(self, parts: &[&[u8]]) -> Vec<u8> {
        let algorithm = match self {
            Self::Sha1 => &digest::SHA1_FOR_LEGACY_USE_ONLY,
            Self::Sha256 => &digest::SHA256,
            Self::Sha384 => &digest::SHA384,
            Self::Sha512 => &digest::SHA512,
        };
        let mut context = digest::Context::new(algorithm);
        for part in parts {
            context.update(part);
        }
        context.finish().as_ref().to_vec()
    }
}

/// Why a signature value was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// An algorithm, key or curve this crate does not check (SHA-1 among them).
    Unsupported,
    /// The value does not verify.
    Invalid,
}

/// A certificate as received: its bytes, its signed part's bytes, and what they say.
#[derive(Debug, Clone)]
pub(crate) struct Cert {
    pub(crate) der: Vec<u8>,
    tbs: Vec<u8>,
    pub(crate) parsed: Certificate,
}

impl Cert {
    /// The certificate `der` encodes, `None` if it does not read.
    pub(crate) fn from_der(der: &[u8]) -> Option<Self> {
        let element = tlv::single(der)?;
        let tbs = tlv::children_of(&element, tlv::SEQUENCE)?
            .first()?
            .whole
            .to_vec();
        let parsed = Certificate::from_der(der).ok()?;
        Some(Self {
            der: der.to_vec(),
            tbs,
            parsed,
        })
    }

    /// Whether `self`'s key made `child`'s signature, `self` named as its issuer.
    pub(crate) fn issued(&self, child: &Self) -> bool {
        self.parsed.tbs_certificate.subject == child.parsed.tbs_certificate.issuer
            && verify(
                self,
                &child.parsed.signature_algorithm,
                None,
                &child.tbs,
                child.parsed.signature.raw_bytes(),
            )
            .is_ok()
    }

    /// Whether `at` (seconds since 1970) falls within the certificate's validity.
    pub(crate) fn valid_at(&self, at: u64) -> bool {
        let validity = &self.parsed.tbs_certificate.validity;
        validity.not_before.to_unix_duration().as_secs() <= at
            && at <= validity.not_after.to_unix_duration().as_secs()
    }

    /// The subject, RFC 4514.
    pub(crate) fn subject(&self) -> String {
        self.parsed.tbs_certificate.subject.to_string()
    }

    /// The public key's bits (the `subjectPublicKey` BIT STRING's content).
    pub(crate) fn key_bits(&self) -> &[u8] {
        self.parsed
            .tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .raw_bytes()
    }

    /// Whether the certificate carries the extension `oid`.
    pub(crate) fn has_extension(&self, oid: ObjectIdentifier) -> bool {
        self.extension(oid).is_some()
    }

    /// The HTTP addresses of its issuer's OCSP responder (authority information access).
    pub(crate) fn ocsp_urls(&self) -> Vec<String> {
        let Some(access) = self
            .extension(AUTHORITY_INFO_ACCESS)
            .and_then(|value| AuthorityInfoAccessSyntax::from_der(value).ok())
        else {
            return Vec::new();
        };
        access
            .0
            .iter()
            .filter(|description| description.access_method == AD_OCSP)
            .filter_map(|description| http_url(&description.access_location))
            .collect()
    }

    /// The HTTP addresses of the CRLs that would list it (CRL distribution points).
    pub(crate) fn crl_urls(&self) -> Vec<String> {
        let Some(points) = self
            .extension(CRL_DISTRIBUTION_POINTS)
            .and_then(|value| CrlDistributionPoints::from_der(value).ok())
        else {
            return Vec::new();
        };
        points
            .0
            .iter()
            .filter_map(
                |point: &DistributionPoint| match &point.distribution_point {
                    Some(DistributionPointName::FullName(names)) => Some(names),
                    _ => None,
                },
            )
            .flatten()
            .filter_map(http_url)
            .collect()
    }

    fn extension(&self, oid: ObjectIdentifier) -> Option<&[u8]> {
        self.parsed
            .tbs_certificate
            .extensions
            .as_ref()?
            .iter()
            .find(|extension| extension.extn_id == oid)
            .map(|extension| extension.extn_value.as_bytes())
    }

    /// Whether the certificate may issue others (basic constraints, `cA`).
    pub(crate) fn is_authority(&self) -> bool {
        self.extension(BASIC_CONSTRAINTS)
            .and_then(|value| BasicConstraints::from_der(value).ok())
            .is_some_and(|constraints| constraints.ca)
    }

    /// Whether the key usage allows `usage`: when the certificate gives one, it must name the
    /// usage (RFC 5280, 4.2.1.3); a key usage that does not read allows nothing.
    pub(crate) fn allows(&self, usage: Usage) -> bool {
        let Some(value) = self.extension(KEY_USAGE) else {
            return true;
        };
        let Ok(bits) = KeyUsage::from_der(value) else {
            return false;
        };
        match usage {
            Usage::Signing => bits.digital_signature() || bits.non_repudiation(),
            Usage::CertificateSigning => bits.key_cert_sign(),
            Usage::CrlSigning => bits.crl_sign(),
        }
    }

    /// Whether the extended key usage names `purpose`.
    pub(crate) fn has_purpose(&self, purpose: ObjectIdentifier) -> bool {
        self.extension(EXTENDED_KEY_USAGE)
            .and_then(|value| ExtendedKeyUsage::from_der(value).ok())
            .is_some_and(|usage| usage.0.contains(&purpose))
    }

    /// The subject key identifier, if the certificate gives one.
    pub(crate) fn key_identifier(&self) -> Option<Vec<u8>> {
        let value = self.extension(SUBJECT_KEY_IDENTIFIER)?;
        let identifier = SubjectKeyIdentifier::from_der(value).ok()?;
        Some(identifier.0.as_bytes().to_vec())
    }
}

/// A general name's address, if it is an HTTP one (never LDAP, never a file).
pub(crate) fn http_url(name: &GeneralName) -> Option<String> {
    let GeneralName::UniformResourceIdentifier(uri) = name else {
        return None;
    };
    let text = uri.as_str();
    let lower = text.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then(|| text.to_owned())
}

/// The hash a `RSASSA-PSS-params` names (SHA-1 when it names none, which is refused).
fn pss_hash(parameters: Option<&der::asn1::Any>) -> Option<Hash> {
    let encoded = der::Encode::to_der(parameters?).ok()?;
    let element = tlv::single(&encoded)?;
    let fields = tlv::children_of(&element, tlv::SEQUENCE)?;
    let hash = fields.iter().find(|field| field.tag == tlv::context(0))?;
    let algorithm = AlgorithmIdentifierOwned::from_der(hash.content).ok()?;
    Hash::from_oid(algorithm.oid)
}

/// The curve an `id-ecPublicKey` key is on.
fn curve(algorithm: &AlgorithmIdentifierOwned) -> Option<ObjectIdentifier> {
    algorithm.parameters.as_ref()?.decode_as().ok()
}

/// Whether `signature` is `signer`'s over `message`, made with `algorithm`. `digest` is the
/// digest algorithm a CMS signer names beside it, for the signature algorithms that name none
/// (`rsaEncryption`, `id-ecPublicKey`).
pub(crate) fn verify(
    signer: &Cert,
    algorithm: &AlgorithmIdentifierOwned,
    digest: Option<Hash>,
    message: &[u8],
    signature: &[u8],
) -> Result<(), Fault> {
    let key = &signer.parsed.tbs_certificate.subject_public_key_info;
    let chosen: &'static dyn VerificationAlgorithm = match key.algorithm.oid {
        RSA_ENCRYPTION => {
            let (pss, hash) = match algorithm.oid {
                SHA256_WITH_RSA => (false, Some(Hash::Sha256)),
                SHA384_WITH_RSA => (false, Some(Hash::Sha384)),
                SHA512_WITH_RSA => (false, Some(Hash::Sha512)),
                RSA_ENCRYPTION => (false, digest),
                RSASSA_PSS => (true, pss_hash(algorithm.parameters.as_ref())),
                _ => (false, None),
            };
            match (pss, hash) {
                (false, Some(Hash::Sha256)) => &signature::RSA_PKCS1_2048_8192_SHA256,
                (false, Some(Hash::Sha384)) => &signature::RSA_PKCS1_2048_8192_SHA384,
                (false, Some(Hash::Sha512)) => &signature::RSA_PKCS1_2048_8192_SHA512,
                (true, Some(Hash::Sha256)) => &signature::RSA_PSS_2048_8192_SHA256,
                (true, Some(Hash::Sha384)) => &signature::RSA_PSS_2048_8192_SHA384,
                (true, Some(Hash::Sha512)) => &signature::RSA_PSS_2048_8192_SHA512,
                _ => return Err(Fault::Unsupported),
            }
        }
        EC_PUBLIC_KEY => {
            let hash = match algorithm.oid {
                ECDSA_WITH_SHA256 => Some(Hash::Sha256),
                ECDSA_WITH_SHA384 => Some(Hash::Sha384),
                ECDSA_WITH_SHA512 => Some(Hash::Sha512),
                EC_PUBLIC_KEY => digest,
                _ => None,
            };
            match (curve(&key.algorithm), hash) {
                (Some(P256), Some(Hash::Sha256)) => &signature::ECDSA_P256_SHA256_ASN1,
                (Some(P256), Some(Hash::Sha384)) => &signature::ECDSA_P256_SHA384_ASN1,
                (Some(P384), Some(Hash::Sha256)) => &signature::ECDSA_P384_SHA256_ASN1,
                (Some(P384), Some(Hash::Sha384)) => &signature::ECDSA_P384_SHA384_ASN1,
                _ => return Err(Fault::Unsupported),
            }
        }
        _ => return Err(Fault::Unsupported),
    };
    UnparsedPublicKey::new(chosen, key.subject_public_key.raw_bytes())
        .verify(message, signature)
        .map_err(|_| Fault::Invalid)
}

/// Why no path was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathFault {
    /// No chain of signatures leads to an anchor.
    Untrusted,
    /// One does, but a certificate on it is not valid at the time asked.
    Expired,
}

/// What a key is used for, as a key usage extension names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Usage {
    /// Signing data other than certificates and CRLs (`digitalSignature` or
    /// `nonRepudiation`): documents, timestamp tokens, OCSP responses.
    Signing,
    /// Signing certificates (`keyCertSign`).
    CertificateSigning,
    /// Signing CRLs (`cRLSign`).
    CrlSigning,
}

/// A path from `leaf` to one of `anchors`, through certificates of `pool`: `leaf` first, the
/// anchor last. Every certificate but the anchor must be valid at `at`, every issuer below the
/// anchor an authority. An anchor is trusted as given: its validity is not looked at. The leaf
/// signs data ([`Usage::Signing`]): a signer's or a timestamp authority's certificate.
pub(crate) fn path<'a>(
    leaf: &'a Cert,
    pool: &'a [Cert],
    anchors: &'a [Cert],
    at: u64,
) -> Result<Vec<&'a Cert>, PathFault> {
    let mut search = Search {
        pool,
        anchors,
        at,
        walked: vec![leaf],
        found: Vec::new(),
        expired: false,
        checks_left: MAX_CHECKS,
    };
    if search.extend(leaf) {
        Ok(search.found)
    } else if search.expired {
        Err(PathFault::Expired)
    } else {
        Err(PathFault::Untrusted)
    }
}

/// A depth-first search for a path, its signature checks counted.
struct Search<'a> {
    pool: &'a [Cert],
    anchors: &'a [Cert],
    at: u64,
    walked: Vec<&'a Cert>,
    found: Vec<&'a Cert>,
    expired: bool,
    checks_left: usize,
}

impl<'a> Search<'a> {
    /// Whether `issuer` issued `child`, within the checks left.
    fn issued(&mut self, issuer: &Cert, child: &Cert) -> bool {
        if issuer.parsed.tbs_certificate.subject != child.parsed.tbs_certificate.issuer
            || self.checks_left == 0
        {
            return false;
        }
        self.checks_left -= 1;
        issuer.issued(child)
    }

    /// Take `path` (leaf first, anchor last) if its constraints hold.
    fn accept(&mut self, path: Vec<&'a Cert>) -> bool {
        if constraints_hold(&path) {
            self.found = path;
            true
        } else {
            false
        }
    }

    fn extend(&mut self, current: &'a Cert) -> bool {
        if self.anchors.iter().any(|anchor| anchor.der == current.der) {
            return self.accept(self.walked.clone());
        }
        if !current.valid_at(self.at) {
            self.expired = true;
            return false;
        }
        for anchor in self.anchors {
            if self.issued(anchor, current) {
                let mut path = self.walked.clone();
                path.push(anchor);
                if self.accept(path) {
                    return true;
                }
            }
        }
        if self.walked.len() >= MAX_DEPTH {
            return false;
        }
        for issuer in self.pool {
            if self.walked.iter().any(|known| known.der == issuer.der)
                || !issuer.is_authority()
                || !self.issued(issuer, current)
            {
                continue;
            }
            self.walked.push(issuer);
            if self.extend(issuer) {
                return true;
            }
            self.walked.pop();
        }
        false
    }
}

/// Whether the constraints of a path (leaf first, anchor last) hold, as RFC 5280 (6.1) has them,
/// within what this crate checks. The anchor is trusted as given; for every other certificate:
/// - no critical extension outside [`UNDERSTOOD_EXTENSIONS`]: name constraints (in any form),
///   policy mappings and policy constraints are not applied here, so a path holding them is
///   refused rather than taken without them;
/// - the leaf's key usage, when given, allows signing data ([`Usage::Signing`]), an issuer's
///   allows signing certificates (`keyCertSign`);
/// - an issuer's `pathLenConstraint` bounds the issuers below it that are not self-issued.
fn constraints_hold(path: &[&Cert]) -> bool {
    let Some((_, below_anchor)) = path.split_last() else {
        return false;
    };
    for (index, cert) in below_anchor.iter().enumerate() {
        let extensions = cert
            .parsed
            .tbs_certificate
            .extensions
            .as_deref()
            .unwrap_or_default();
        for extension in extensions {
            if extension.extn_id == NAME_CONSTRAINTS
                || (extension.critical && !UNDERSTOOD_EXTENSIONS.contains(&extension.extn_id))
            {
                return false;
            }
        }
        if index == 0 {
            if !cert.allows(Usage::Signing) {
                return false;
            }
            continue;
        }
        // An issuer below the anchor.
        if !cert.allows(Usage::CertificateSigning) {
            return false;
        }
        let constraints = cert
            .extension(BASIC_CONSTRAINTS)
            .and_then(|value| BasicConstraints::from_der(value).ok());
        if let Some(limit) = constraints.and_then(|c| c.path_len_constraint) {
            let issuers_below = below_anchor[1..index]
                .iter()
                .filter(|issuer| {
                    issuer.parsed.tbs_certificate.subject != issuer.parsed.tbs_certificate.issuer
                })
                .count();
            if issuers_below > usize::from(limit) {
                return false;
            }
        }
    }
    true
}

/// A `GeneralizedTime` in UTC (`YYYYMMDDHHMMSS[.f]Z`), in seconds since 1970.
pub(crate) fn generalized_time(content: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(content).ok()?.strip_suffix('Z')?;
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    if whole.len() != 14
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.is_empty()
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let number = |from: usize, to: usize| whole.get(from..to)?.parse::<u8>().ok();
    let time = der::DateTime::new(
        whole.get(..4)?.parse().ok()?,
        number(4, 6)?,
        number(6, 8)?,
        number(8, 10)?,
        number(10, 12)?,
        number(12, 14)?,
    )
    .ok()?;
    Some(time.unix_duration().as_secs())
}
