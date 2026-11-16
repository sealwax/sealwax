//! Each signature algorithm: the digest the signer gets, the CMS that names it, and a signature
//! value that verifies, for RSA (PKCS #1 v1.5 and PSS) and ECDSA, with SHA-256, SHA-384 and
//! SHA-512.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::cell::Cell;

use cms::content_info::ContentInfo;
use cms::signed_data::SignedData;
use const_oid::ObjectIdentifier;
use der::asn1::{Any, BitString};
use der::{Decode, Encode};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair, UnparsedPublicKey};
use sealwax_core::{Document, Object, OpenOptions};
use sealwax_sign::{Algorithm, Kind, Options, Signer, sign};
use sha2::Digest as _;
use x509_cert::Certificate;
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};

use common::{Key, Profile, certificate, document};

/// A 2048-bit RSA key made for these tests only (`openssl genpkey`, PKCS #8 DER): ring makes no
/// RSA keys.
const RSA_KEY: &[u8] = include_bytes!("data/rsa-2048-test-only.pk8");
const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const SECP384R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
const RSASSA_PSS: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.10");

/// A signer whose key signs the message itself, the digest it was given kept.
struct Keyed<'a> {
    certificate: Vec<u8>,
    algorithm: Algorithm,
    sign: &'a dyn Fn(&[u8]) -> Vec<u8>,
    digest_length: Cell<usize>,
}

impl Signer for Keyed<'_> {
    fn certificate(&self) -> &[u8] {
        &self.certificate
    }
    fn chain(&self) -> &[Vec<u8>] {
        &[]
    }
    fn algorithm(&self) -> Algorithm {
        self.algorithm
    }
    fn sign(&self, message: &[u8], digest: &[u8]) -> Result<Vec<u8>, String> {
        self.digest_length.set(digest.len());
        Ok((self.sign)(message))
    }
}

/// `certificate`, its key replaced by `key`, re-signed by `issuer`.
fn with_key(certificate: &[u8], key: SubjectPublicKeyInfoOwned, issuer: &Key) -> Vec<u8> {
    let mut cert = Certificate::from_der(certificate).unwrap();
    cert.tbs_certificate.subject_public_key_info = key;
    cert.signature =
        BitString::from_bytes(&issuer.sign(&cert.tbs_certificate.to_der().unwrap())).unwrap();
    cert.to_der().unwrap()
}

/// What a signed document holds: the signed bytes, the CMS signer's digest and signature
/// algorithms, the `messageDigest`, the signed attributes and the signature value.
struct Signed {
    signed_bytes: Vec<u8>,
    digest_algorithm: ObjectIdentifier,
    signature_algorithm: AlgorithmIdentifierOwned,
    message_digest: Vec<u8>,
    attributes: Vec<u8>,
    value: Vec<u8>,
}

fn read(pdf: &[u8]) -> Signed {
    let doc = Document::open(pdf.to_vec(), &OpenOptions::default()).unwrap();
    let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
    let form = doc
        .object(
            catalog
                .as_dict()
                .unwrap()
                .get(b"AcroForm")
                .unwrap()
                .as_ref_id()
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    let fields = form
        .as_dict()
        .unwrap()
        .get(b"Fields")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    let field = doc.object(fields[0].as_ref_id().unwrap()).unwrap().unwrap();
    let value = field
        .as_dict()
        .unwrap()
        .get(b"V")
        .unwrap()
        .as_ref_id()
        .unwrap();
    let signature = doc.object(value).unwrap().unwrap();
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
    let mut signed_bytes = pdf[..*a].to_vec();
    signed_bytes.extend_from_slice(&pdf[*b..b + c]);
    let Some(Object::String(contents)) = signature.get(b"Contents") else {
        panic!("no contents");
    };
    // `/Contents` is padded with zeros after the DER element.
    let info = ContentInfo::from_der(&contents[..common::der_length(contents)]).unwrap();
    let signed = SignedData::from_der(&info.content.to_der().unwrap()).unwrap();
    let signer = signed.signer_infos.0.get(0).unwrap().clone();
    assert_eq!(
        signed.digest_algorithms.get(0).unwrap().oid,
        signer.digest_alg.oid,
        "SignedData and SignerInfo name the same digest"
    );
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
    Signed {
        signed_bytes,
        digest_algorithm: signer.digest_alg.oid,
        signature_algorithm: signer.signature_algorithm.clone(),
        message_digest,
        attributes: attributes.to_der().unwrap(),
        value: signer.signature.as_bytes().to_vec(),
    }
}

fn digest(oid: ObjectIdentifier, data: &[u8]) -> Vec<u8> {
    match oid.to_string().as_str() {
        "2.16.840.1.101.3.4.2.1" => sha2::Sha256::digest(data).to_vec(),
        "2.16.840.1.101.3.4.2.2" => sha2::Sha384::digest(data).to_vec(),
        "2.16.840.1.101.3.4.2.3" => sha2::Sha512::digest(data).to_vec(),
        other => panic!("digest {other}"),
    }
}

/// Sign with `algorithm`, the key signing as `sign` does, and check what was written; returns the
/// signature value and the signed attributes it covers.
fn signed_with(
    certificate: Vec<u8>,
    algorithm: Algorithm,
    sign_with: &dyn Fn(&[u8]) -> Vec<u8>,
) -> Signed {
    let signer = Keyed {
        certificate,
        algorithm,
        sign: sign_with,
        digest_length: Cell::new(0),
    };
    let pdf = sign(
        &document(),
        &signer,
        &Options::new("S", Kind::Approval),
        None,
    )
    .unwrap()
    .pdf;
    assert_eq!(
        signer.digest_length.get(),
        algorithm.digest_algorithm().output_len(),
        "{algorithm:?}: the digest the signer gets"
    );
    let signed = read(&pdf);
    assert_eq!(
        signed.message_digest,
        digest(signed.digest_algorithm, &signed.signed_bytes),
        "{algorithm:?}: messageDigest"
    );
    assert_eq!(
        signed.message_digest.len(),
        algorithm.digest_algorithm().output_len()
    );
    signed
}

#[test]
fn rsa_pkcs1_and_pss_with_each_digest_verify() {
    let random = SystemRandom::new();
    let key = RsaKeyPair::from_pkcs8(RSA_KEY).unwrap();
    let issuer = Key::new();
    let spki = SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: RSA_ENCRYPTION,
            parameters: Some(Any::null()),
        },
        subject_public_key: BitString::from_bytes(key.public_key().as_ref()).unwrap(),
    };
    let base = certificate(&issuer, &{
        let mut profile = Profile::new("CN=RSA signer,O=sealwax", 7);
        profile.issuer = Some(("CN=Issuer,O=sealwax", &issuer));
        profile
    });
    let cert = with_key(&base, spki, &issuer);
    let cases: [(
        Algorithm,
        &'static dyn signature::RsaEncoding,
        &'static dyn signature::VerificationAlgorithm,
        &str,
    ); 6] = [
        (
            Algorithm::RsaPkcs1Sha256,
            &signature::RSA_PKCS1_SHA256,
            &signature::RSA_PKCS1_2048_8192_SHA256,
            "1.2.840.113549.1.1.11",
        ),
        (
            Algorithm::RsaPkcs1Sha384,
            &signature::RSA_PKCS1_SHA384,
            &signature::RSA_PKCS1_2048_8192_SHA384,
            "1.2.840.113549.1.1.12",
        ),
        (
            Algorithm::RsaPkcs1Sha512,
            &signature::RSA_PKCS1_SHA512,
            &signature::RSA_PKCS1_2048_8192_SHA512,
            "1.2.840.113549.1.1.13",
        ),
        (
            Algorithm::RsaPssSha256,
            &signature::RSA_PSS_SHA256,
            &signature::RSA_PSS_2048_8192_SHA256,
            "1.2.840.113549.1.1.10",
        ),
        (
            Algorithm::RsaPssSha384,
            &signature::RSA_PSS_SHA384,
            &signature::RSA_PSS_2048_8192_SHA384,
            "1.2.840.113549.1.1.10",
        ),
        (
            Algorithm::RsaPssSha512,
            &signature::RSA_PSS_SHA512,
            &signature::RSA_PSS_2048_8192_SHA512,
            "1.2.840.113549.1.1.10",
        ),
    ];
    for (algorithm, padding, verification, oid) in cases {
        let sign_with = |message: &[u8]| {
            let mut value = vec![0; key.public().modulus_len()];
            key.sign(padding, &random, message, &mut value).unwrap();
            value
        };
        let signed = signed_with(cert.clone(), algorithm, &sign_with);
        assert_eq!(
            signed.signature_algorithm.oid.to_string(),
            oid,
            "{algorithm:?}"
        );
        if signed.signature_algorithm.oid == RSASSA_PSS {
            // RSASSA-PSS-params: the digest, MGF1 with the same digest, the salt length.
            let parameters = signed
                .signature_algorithm
                .parameters
                .as_ref()
                .unwrap()
                .to_der()
                .unwrap();
            let digest_oid = signed.digest_algorithm.to_der().unwrap();
            let occurrences = parameters
                .windows(digest_oid.len())
                .filter(|window| *window == digest_oid.as_slice())
                .count();
            assert_eq!(
                occurrences, 2,
                "{algorithm:?}: the digest, in the hash and in MGF1"
            );
            let salt = u8::try_from(algorithm.digest_algorithm().output_len()).unwrap();
            assert!(
                parameters.ends_with(&[0xa2, 3, 2, 1, salt]),
                "{algorithm:?}: salt"
            );
        } else {
            assert_eq!(
                signed.signature_algorithm.parameters,
                Some(Any::null()),
                "{algorithm:?}: NULL parameters"
            );
        }
        UnparsedPublicKey::new(verification, key.public_key().as_ref())
            .verify(&signed.attributes, &signed.value)
            .unwrap_or_else(|_| panic!("{algorithm:?}: the value verifies"));
    }
}

#[test]
fn ecdsa_p384_with_sha384_verifies() {
    let random = SystemRandom::new();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P384_SHA384_ASN1_SIGNING, &random).unwrap();
    let key = EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P384_SHA384_ASN1_SIGNING,
        pkcs8.as_ref(),
        &random,
    )
    .unwrap();
    let issuer = Key::new();
    let spki = SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: EC_PUBLIC_KEY,
            parameters: Some(Any::from(&SECP384R1)),
        },
        subject_public_key: BitString::from_bytes(key.public_key().as_ref()).unwrap(),
    };
    let base = certificate(&issuer, &{
        let mut profile = Profile::new("CN=P-384 signer,O=sealwax", 8);
        profile.issuer = Some(("CN=Issuer,O=sealwax", &issuer));
        profile
    });
    let cert = with_key(&base, spki, &issuer);
    let sign_with = |message: &[u8]| key.sign(&random, message).unwrap().as_ref().to_vec();
    let signed = signed_with(cert, Algorithm::EcdsaSha384, &sign_with);
    assert_eq!(
        signed.signature_algorithm.oid.to_string(),
        "1.2.840.10045.4.3.3"
    );
    assert_eq!(signed.signature_algorithm.parameters, None);
    UnparsedPublicKey::new(
        &signature::ECDSA_P384_SHA384_ASN1,
        key.public_key().as_ref(),
    )
    .verify(&signed.attributes, &signed.value)
    .unwrap();
}

/// Key vaults may answer ECDSA with the two integers side by side (`r || s`), as JOSE has them:
/// the value is written as DER, and verifies as such.
#[test]
fn an_ecdsa_value_as_r_and_s_is_written_as_der() {
    let random = SystemRandom::new();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
    let key = EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        pkcs8.as_ref(),
        &random,
    )
    .unwrap();
    let issuer = Key::new();
    let spki = SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: EC_PUBLIC_KEY,
            parameters: Some(Any::from(&ObjectIdentifier::new_unwrap(
                "1.2.840.10045.3.1.7",
            ))),
        },
        subject_public_key: BitString::from_bytes(key.public_key().as_ref()).unwrap(),
    };
    let base = certificate(&issuer, &Profile::new("CN=P-256 signer,O=sealwax", 9));
    let cert = with_key(&base, spki, &issuer);
    let sign_with = |message: &[u8]| {
        let value = key.sign(&random, message).unwrap().as_ref().to_vec();
        assert_eq!(value.len(), 64, "r || s");
        value
    };
    let signed = signed_with(cert, Algorithm::EcdsaSha256, &sign_with);
    assert_eq!(signed.value[0], 0x30, "DER");
    UnparsedPublicKey::new(
        &signature::ECDSA_P256_SHA256_ASN1,
        key.public_key().as_ref(),
    )
    .verify(&signed.attributes, &signed.value)
    .unwrap();
    // A value of any other length is refused, not written.
    let wrong = |_: &[u8]| vec![7; 63];
    let signer = Keyed {
        certificate: base,
        algorithm: Algorithm::EcdsaSha256,
        sign: &wrong,
        digest_length: Cell::new(0),
    };
    assert!(
        sign(
            &document(),
            &signer,
            &Options::new("S", Kind::Approval),
            None
        )
        .is_err()
    );
}

/// ECDSA with SHA-512 (`ES512`, P-521 keys, which ring does not sign with): the digest and the
/// algorithm named.
#[test]
fn ecdsa_with_sha512_names_its_digest() {
    let issuer = Key::new();
    let base = certificate(&issuer, &Profile::new("CN=P-256 signer,O=sealwax", 10));
    let sign_with = |message: &[u8]| issuer.sign(message);
    let signed = signed_with(base, Algorithm::EcdsaSha512, &sign_with);
    assert_eq!(
        signed.signature_algorithm.oid.to_string(),
        "1.2.840.10045.4.3.4"
    );
    assert_eq!(
        signed.digest_algorithm.to_string(),
        "2.16.840.1.101.3.4.2.3"
    );
}
