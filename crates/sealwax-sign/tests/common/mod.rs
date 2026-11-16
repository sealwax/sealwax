//! What the tests share: a document, P-256 keys (ring), certificates made with them, a signer,
//! and a reading of the signatures written.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cms::content_info::ContentInfo;
use cms::signed_data::{SignedData, SignerInfo};
use const_oid::ObjectIdentifier;
use der::asn1::{Any, BitString, Ia5String, Null, OctetString, UtcTime};
use der::{Decode, Encode};
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair,
    UnparsedPublicKey,
};
use sealwax_core::{Document, Object, OpenOptions, SaveOptions};
use sealwax_sign::{Algorithm, Signer};
use sha2::{Digest, Sha256};
use x509_cert::certificate::{Certificate, TbsCertificate, Version};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::crl::dp::DistributionPoint;
use x509_cert::ext::pkix::name::{DistributionPointName, GeneralName};
use x509_cert::ext::pkix::{
    AccessDescription, AuthorityInfoAccessSyntax, BasicConstraints, CrlDistributionPoints,
    ExtendedKeyUsage,
};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
use x509_cert::time::{Time, Validity};

pub const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const PRIME256V1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const BASIC_CONSTRAINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.19");
const EXTENDED_KEY_USAGE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.37");
pub const TIME_STAMPING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.8");
pub const OCSP_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.9");
const AUTHORITY_INFO_ACCESS: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.1.1");
const AD_OCSP: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1");
const CRL_DISTRIBUTION_POINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.31");
const OCSP_NOCHECK: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1.5");

/// A one-page document, saved in full as sealwax writes what it signs.
pub fn document() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    let doc = Document::open(out, &OpenOptions::default()).unwrap();
    doc.save(&SaveOptions::default()).unwrap()
}

/// Seconds since 1970, now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A P-256 key.
pub struct Key(EcdsaKeyPair);

impl Key {
    pub fn new() -> Self {
        let random = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random).unwrap();
        Self(
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &random)
                .unwrap(),
        )
    }

    pub fn public(&self) -> &[u8] {
        self.0.public_key().as_ref()
    }

    /// ECDSA with SHA-256 over `message`, DER.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.0
            .sign(&SystemRandom::new(), message)
            .unwrap()
            .as_ref()
            .to_vec()
    }
}

/// What a test certificate is.
pub struct Profile<'a> {
    pub subject: &'a str,
    pub serial: u32,
    /// The issuer's name and key; self-signed when `None`.
    pub issuer: Option<(&'a str, &'a Key)>,
    pub not_before: u64,
    pub not_after: u64,
    pub authority: bool,
    pub purpose: Option<ObjectIdentifier>,
    /// Where its issuer's OCSP responder answers.
    pub ocsp: Option<&'a str>,
    /// Where the CRL that would list it is published.
    pub crl: Option<&'a str>,
    /// `id-pkix-ocsp-nocheck`, for a delegated responder.
    pub nocheck: bool,
}

impl<'a> Profile<'a> {
    /// An end-entity certificate valid from an hour ago to a day from now.
    pub fn new(subject: &'a str, serial: u32) -> Self {
        let now = now();
        Self {
            subject,
            serial,
            issuer: None,
            not_before: now - 3600,
            not_after: now + 86_400,
            authority: false,
            purpose: None,
            ocsp: None,
            crl: None,
            nocheck: false,
        }
    }
}

/// The certificate of `key` for `profile`, DER.
pub fn certificate(key: &Key, profile: &Profile<'_>) -> Vec<u8> {
    let algorithm = AlgorithmIdentifierOwned {
        oid: ECDSA_WITH_SHA256,
        parameters: None,
    };
    let subject = Name::from_str(profile.subject).unwrap();
    let (issuer, issuer_key) = match profile.issuer {
        Some((name, key)) => (Name::from_str(name).unwrap(), key),
        None => (subject.clone(), key),
    };
    let time =
        |at: u64| Time::UtcTime(UtcTime::from_unix_duration(Duration::from_secs(at)).unwrap());
    let mut extensions = Vec::new();
    if profile.authority {
        extensions.push(Extension {
            extn_id: BASIC_CONSTRAINTS,
            critical: true,
            extn_value: OctetString::new(
                BasicConstraints {
                    ca: true,
                    path_len_constraint: None,
                }
                .to_der()
                .unwrap(),
            )
            .unwrap(),
        });
    }
    if let Some(purpose) = profile.purpose {
        extensions.push(Extension {
            extn_id: EXTENDED_KEY_USAGE,
            critical: true,
            extn_value: OctetString::new(ExtendedKeyUsage(vec![purpose]).to_der().unwrap())
                .unwrap(),
        });
    }
    let uri = |url: &str| GeneralName::UniformResourceIdentifier(Ia5String::new(url).unwrap());
    let extension = |extn_id, value: Vec<u8>| Extension {
        extn_id,
        critical: false,
        extn_value: OctetString::new(value).unwrap(),
    };
    if let Some(url) = profile.ocsp {
        let access = AuthorityInfoAccessSyntax(vec![AccessDescription {
            access_method: AD_OCSP,
            access_location: uri(url),
        }]);
        extensions.push(extension(AUTHORITY_INFO_ACCESS, access.to_der().unwrap()));
    }
    if let Some(url) = profile.crl {
        let points = CrlDistributionPoints(vec![DistributionPoint {
            distribution_point: Some(DistributionPointName::FullName(vec![uri(url)])),
            reasons: None,
            crl_issuer: None,
        }]);
        extensions.push(extension(CRL_DISTRIBUTION_POINTS, points.to_der().unwrap()));
    }
    if profile.nocheck {
        extensions.push(extension(OCSP_NOCHECK, Null.to_der().unwrap()));
    }
    let tbs = TbsCertificate {
        version: Version::V3,
        serial_number: SerialNumber::from(profile.serial),
        signature: algorithm.clone(),
        issuer,
        validity: Validity {
            not_before: time(profile.not_before),
            not_after: time(profile.not_after),
        },
        subject,
        subject_public_key_info: SubjectPublicKeyInfoOwned {
            algorithm: AlgorithmIdentifierOwned {
                oid: EC_PUBLIC_KEY,
                parameters: Some(Any::from(&PRIME256V1)),
            },
            subject_public_key: BitString::from_bytes(key.public()).unwrap(),
        },
        issuer_unique_id: None,
        subject_unique_id: None,
        extensions: (!extensions.is_empty()).then_some(extensions),
    };
    let signature = issuer_key.sign(&tbs.to_der().unwrap());
    Certificate {
        tbs_certificate: tbs,
        signature_algorithm: algorithm,
        signature: BitString::from_bytes(&signature).unwrap(),
    }
    .to_der()
    .unwrap()
}

/// A signer with a self-signed P-256 certificate.
pub struct TestSigner {
    pub key: Key,
    pub certificate: Vec<u8>,
    pub chain: Vec<Vec<u8>>,
}

impl TestSigner {
    pub fn new() -> Self {
        let key = Key::new();
        let certificate = certificate(&key, &Profile::new("CN=sealwax test,O=sealwax", 42));
        Self {
            key,
            certificate,
            chain: Vec::new(),
        }
    }

    pub fn public_key(&self) -> &[u8] {
        self.key.public()
    }
}

impl Signer for TestSigner {
    fn certificate(&self) -> &[u8] {
        &self.certificate
    }
    fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }
    fn algorithm(&self) -> Algorithm {
        Algorithm::EcdsaSha256
    }
    fn sign(&self, message: &[u8], _digest: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.key.sign(message))
    }
}

/// A signature read back.
pub struct Read {
    pub name: String,
    /// The byte range matches the file, the message digest the range, the value the key.
    pub verified: bool,
    pub signer_info: SignerInfo,
}

/// Each signature of `data`: its byte range checked against the file, its CMS read back, its
/// message digest and signature value verified.
pub fn signatures(data: &[u8], public_key: &[u8]) -> Vec<Read> {
    let doc = Document::open(data.to_vec(), &OpenOptions::default()).unwrap();
    let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
    let form = match catalog.as_dict().unwrap().get(b"AcroForm").unwrap() {
        Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
        other => other.clone(),
    };
    let form = form.as_dict().unwrap();
    assert_eq!(form.get(b"SigFlags").and_then(Object::as_i64), Some(3));
    let fields = form.get(b"Fields").unwrap().as_array().unwrap().to_vec();
    let mut found = Vec::new();
    for field in fields {
        let field = doc.object(field.as_ref_id().unwrap()).unwrap().unwrap();
        let field = field.as_dict().unwrap();
        let name =
            String::from_utf8(field.get(b"T").unwrap().as_string().unwrap().to_vec()).unwrap();
        let signature = doc
            .object(field.get(b"V").unwrap().as_ref_id().unwrap())
            .unwrap()
            .unwrap();
        let signature = signature.as_dict().unwrap();
        let range: Vec<usize> = signature
            .get(b"ByteRange")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| usize::try_from(v.as_i64().unwrap()).unwrap())
            .collect();
        let [0, a, b, c] = range.as_slice() else {
            panic!("byte range {range:?}");
        };
        // The range covers the revision that holds the signature, `/Contents` left out.
        assert_eq!(data[*a], b'<');
        assert_eq!(data[*b - 1], b'>');
        assert!(b + c <= data.len());
        let mut hasher = Sha256::new();
        hasher.update(&data[..*a]);
        hasher.update(&data[*b..b + c]);
        let digest = hasher.finalize();
        let contents = signature.get(b"Contents").unwrap().as_string().unwrap();
        let info = ContentInfo::from_der(&contents[..der_length(contents)]).unwrap();
        let signed = SignedData::from_der(&info.content.to_der().unwrap()).unwrap();
        let signer = signed.signer_infos.0.get(0).unwrap().clone();
        let attributes = signer.signed_attrs.clone().unwrap();
        let message_digest = attributes
            .iter()
            .find(|a| a.oid == const_oid::db::rfc5911::ID_MESSAGE_DIGEST)
            .unwrap()
            .values
            .get(0)
            .unwrap()
            .value()
            .to_vec();
        let digest_ok = message_digest[..] == digest[..];
        let message = attributes.to_der().unwrap();
        let signature_ok = UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, public_key)
            .verify(&message, signer.signature.as_bytes())
            .is_ok();
        found.push(Read {
            name,
            verified: digest_ok && signature_ok,
            signer_info: signer,
        });
    }
    found
}

/// Each signature's name, and whether it verified.
pub fn verify(data: &[u8], public_key: &[u8]) -> Vec<(String, bool)> {
    signatures(data, public_key)
        .into_iter()
        .map(|read| (read.name, read.verified))
        .collect()
}

/// The length of the DER element at the start of `bytes` (the zeros after it are padding).
pub fn der_length(bytes: &[u8]) -> usize {
    let first = usize::from(bytes[1]);
    if first < 0x80 {
        return 2 + first;
    }
    let count = first & 0x7f;
    let length = bytes[2..2 + count]
        .iter()
        .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
    2 + count + length
}

/// `cert` re-signed by `issuer` with the extension `oid` set to `content`, critical or not.
pub fn with_extension(
    cert: &[u8],
    issuer: &Key,
    oid: &str,
    content: Vec<u8>,
    critical: bool,
) -> Vec<u8> {
    let mut cert = Certificate::from_der(cert).unwrap();
    let oid = oid.parse().unwrap();
    let extensions = cert.tbs_certificate.extensions.get_or_insert_with(Vec::new);
    extensions.retain(|e| e.extn_id != oid);
    extensions.push(x509_cert::ext::Extension {
        extn_id: oid,
        critical,
        extn_value: OctetString::new(content).unwrap(),
    });
    cert.signature =
        BitString::from_bytes(&issuer.sign(&cert.tbs_certificate.to_der().unwrap())).unwrap();
    cert.to_der().unwrap()
}
