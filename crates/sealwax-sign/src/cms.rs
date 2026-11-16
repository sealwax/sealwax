//! The CMS signature a PAdES signature dictionary holds (RFC 5652, the CAdES baseline profile of
//! ETSI EN 319 122-1): a detached `SignedData` over the signed bytes, its signed attributes
//! `contentType`, `messageDigest` and `signingCertificateV2` (ESS, RFC 5035), and, once
//! timestamped, the unsigned attribute `signatureTimeStampToken`. No `signingTime`: in PAdES the
//! time declared goes in the dictionary's `/M`.

use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{
    CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo,
};
use const_oid::ObjectIdentifier;
use const_oid::db::rfc5911::{
    ID_AA_SIGNING_CERTIFICATE_V_2, ID_CONTENT_TYPE, ID_DATA, ID_MESSAGE_DIGEST, ID_SIGNED_DATA,
};
use der::asn1::{Any, OctetString, SetOfVec, UintRef};
use der::{Decode, Encode, Sequence};
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::attr::{Attribute, AttributeValue};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::AlgorithmIdentifierOwned;

use crate::{Algorithm, DigestAlgorithm, Error, Result, TimestampError};

/// `id-aa-signatureTimeStampToken` (RFC 3161, appendix A).
const ID_AA_TIME_STAMP_TOKEN: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.14");
const SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2");
const SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.3");
const SHA256_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
const SHA384_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.12");
const SHA512_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.13");
const RSASSA_PSS: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.10");
const MGF1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.8");
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const ECDSA_WITH_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");
const ECDSA_WITH_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.4");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const P256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const P384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
const P521: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.35");

/// `RSASSA-PSS-params` (RFC 4055): the digest, MGF1 with the same digest, and the salt length;
/// the trailer field is the default.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
struct PssParameters {
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT")]
    hash: AlgorithmIdentifierOwned,
    #[asn1(context_specific = "1", tag_mode = "EXPLICIT")]
    mask_generation: AlgorithmIdentifierOwned,
    #[asn1(context_specific = "2", tag_mode = "EXPLICIT")]
    salt_length: u32,
}

/// `Ecdsa-Sig-Value` (RFC 3279).
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
struct EcdsaSignature<'a> {
    r: UintRef<'a>,
    s: UintRef<'a>,
}

/// `IssuerSerial` (RFC 5035): the issuer as general names, and the serial number.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
struct IssuerSerial {
    issuer: Vec<GeneralName>,
    serial_number: SerialNumber,
}

/// `ESSCertIDv2` (RFC 5035), its hash algorithm SHA-256, the default, left out as DER wants.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
struct EssCertIdV2 {
    cert_hash: OctetString,
    issuer_serial: IssuerSerial,
}

/// `SigningCertificateV2` (RFC 5035), without policies.
#[derive(Clone, Debug, Eq, PartialEq, Sequence)]
struct SigningCertificateV2 {
    certs: Vec<EssCertIdV2>,
}

/// A certificate read from its DER encoding.
pub(crate) fn certificate(der: &[u8]) -> Result<Certificate> {
    Certificate::from_der(der).map_err(|_| Error::Certificate)
}

fn attribute(oid: ObjectIdentifier, value: &impl Encode) -> Result<Attribute> {
    let value = AttributeValue::from_der(&value.to_der()?)?;
    let mut values = SetOfVec::new();
    values.insert(value)?;
    Ok(Attribute { oid, values })
}

/// The signed attributes, for the bytes whose digest is `message_digest`, signed by `signer`.
pub(crate) fn signed_attributes(
    message_digest: &[u8],
    signer: &Certificate,
    signer_der: &[u8],
) -> Result<SetOfVec<Attribute>> {
    let essential = EssCertIdV2 {
        cert_hash: OctetString::new(Sha256::digest(signer_der).to_vec())?,
        issuer_serial: IssuerSerial {
            issuer: vec![GeneralName::DirectoryName(
                signer.tbs_certificate.issuer.clone(),
            )],
            serial_number: signer.tbs_certificate.serial_number.clone(),
        },
    };
    let mut attributes = SetOfVec::new();
    attributes.insert(attribute(ID_CONTENT_TYPE, &ID_DATA)?)?;
    attributes.insert(attribute(
        ID_MESSAGE_DIGEST,
        &OctetString::new(message_digest.to_vec())?,
    )?)?;
    attributes.insert(attribute(
        ID_AA_SIGNING_CERTIFICATE_V_2,
        &SigningCertificateV2 {
            certs: vec![essential],
        },
    )?)?;
    Ok(attributes)
}

/// What the signer signs: the digest of the signed attributes' DER encoding, as a `SET OF`.
pub(crate) fn digest_to_sign(
    attributes: &SetOfVec<Attribute>,
    digest: DigestAlgorithm,
) -> Result<Vec<u8>> {
    Ok(digest.digest(&attributes.to_der()?))
}

fn digest_identifier(digest: DigestAlgorithm) -> AlgorithmIdentifierOwned {
    let oid = match digest {
        DigestAlgorithm::Sha256 => SHA256,
        DigestAlgorithm::Sha384 => SHA384,
        DigestAlgorithm::Sha512 => SHA512,
    };
    algorithm_identifier(oid, false)
}

/// The `signatureAlgorithm` of the signer info.
fn signature_identifier(algorithm: Algorithm) -> Result<AlgorithmIdentifierOwned> {
    let pss = |digest: DigestAlgorithm| -> Result<AlgorithmIdentifierOwned> {
        // The digest written with NULL parameters inside the PSS parameters, as OpenSSL does.
        let hash = AlgorithmIdentifierOwned {
            parameters: Some(Any::null()),
            ..digest_identifier(digest)
        };
        let parameters = PssParameters {
            mask_generation: AlgorithmIdentifierOwned {
                oid: MGF1,
                parameters: Some(Any::from_der(&hash.to_der()?)?),
            },
            hash,
            salt_length: u32::try_from(digest.output_len()).unwrap_or(32),
        };
        Ok(AlgorithmIdentifierOwned {
            oid: RSASSA_PSS,
            parameters: Some(Any::from_der(&parameters.to_der()?)?),
        })
    };
    Ok(match algorithm {
        Algorithm::RsaPkcs1Sha256 => algorithm_identifier(SHA256_WITH_RSA, true),
        Algorithm::RsaPkcs1Sha384 => algorithm_identifier(SHA384_WITH_RSA, true),
        Algorithm::RsaPkcs1Sha512 => algorithm_identifier(SHA512_WITH_RSA, true),
        Algorithm::RsaPssSha256 | Algorithm::RsaPssSha384 | Algorithm::RsaPssSha512 => {
            pss(algorithm.digest_algorithm())?
        }
        Algorithm::EcdsaSha256 => algorithm_identifier(ECDSA_WITH_SHA256, false),
        Algorithm::EcdsaSha384 => algorithm_identifier(ECDSA_WITH_SHA384, false),
        Algorithm::EcdsaSha512 => algorithm_identifier(ECDSA_WITH_SHA512, false),
    })
}

/// An ECDSA signature value as DER (`Ecdsa-Sig-Value`): kept when it is DER already, encoded when
/// it is the two integers side by side (`r || s`, each as long as the curve's order: JOSE, and the
/// key vaults that answer in that form).
pub(crate) fn ecdsa_der(signature: &[u8], signer: &Certificate) -> Result<Vec<u8>> {
    if let Ok(parsed) = EcdsaSignature::from_der(signature)
        && parsed.to_der().is_ok_and(|again| again == signature)
    {
        return Ok(signature.to_vec());
    }
    let key = &signer.tbs_certificate.subject_public_key_info.algorithm;
    let curve: Option<ObjectIdentifier> = (key.oid == EC_PUBLIC_KEY)
        .then(|| key.parameters.as_ref().and_then(|p| p.decode_as().ok()))
        .flatten();
    let half = match curve {
        Some(P256) => 32,
        Some(P384) => 48,
        Some(P521) => 66,
        _ => 0,
    };
    let refused = || Error::Signer("an ECDSA value that is neither DER nor r || s".to_owned());
    if half == 0 || signature.len() != 2 * half {
        return Err(refused());
    }
    let (r, s) = signature.split_at(half);
    // `UintRef` takes the integers without their leading zeros.
    let trim = |n: &[u8]| -> Vec<u8> {
        let start = n
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(n.len().saturating_sub(1));
        n[start..].to_vec()
    };
    let (r, s) = (trim(r), trim(s));
    let value = EcdsaSignature {
        r: UintRef::new(&r).map_err(|_| refused())?,
        s: UintRef::new(&s).map_err(|_| refused())?,
    };
    Ok(value.to_der()?)
}

fn algorithm_identifier(oid: ObjectIdentifier, null_parameters: bool) -> AlgorithmIdentifierOwned {
    AlgorithmIdentifierOwned {
        oid,
        parameters: null_parameters.then(Any::null),
    }
}

/// The `ContentInfo` holding the `SignedData`, DER-encoded: the signer's certificate then the
/// chain, the signature value, and the timestamp token if there is one.
pub(crate) fn signed_data(
    attributes: SetOfVec<Attribute>,
    algorithm: Algorithm,
    signature: &[u8],
    signer: &Certificate,
    certificates: &[Certificate],
    timestamp_token: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let unsigned = match timestamp_token {
        Some(token) => {
            let token = Any::from_der(token).map_err(|_| TimestampError::Malformed("token"))?;
            let mut values = SetOfVec::new();
            values.insert(token)?;
            let mut attributes = SetOfVec::new();
            attributes.insert(Attribute {
                oid: ID_AA_TIME_STAMP_TOKEN,
                values,
            })?;
            Some(attributes)
        }
        None => None,
    };
    let signature_algorithm = signature_identifier(algorithm)?;
    let signer_info = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
            issuer: signer.tbs_certificate.issuer.clone(),
            serial_number: signer.tbs_certificate.serial_number.clone(),
        }),
        digest_alg: digest_identifier(algorithm.digest_algorithm()),
        signed_attrs: Some(attributes),
        signature_algorithm,
        signature: OctetString::new(signature.to_vec())?,
        unsigned_attrs: unsigned,
    };
    let mut set = SetOfVec::new();
    for certificate in certificates {
        // A certificate given twice is kept once.
        let choice = CertificateChoices::Certificate(certificate.clone());
        if !set.iter().any(|known| *known == choice) {
            set.insert(choice)?;
        }
    }
    let mut digests = SetOfVec::new();
    digests.insert(digest_identifier(algorithm.digest_algorithm()))?;
    let mut signer_infos = SetOfVec::new();
    signer_infos.insert(signer_info)?;
    let signed = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: digests,
        encap_content_info: EncapsulatedContentInfo {
            econtent_type: ID_DATA,
            econtent: None,
        },
        certificates: Some(CertificateSet(set)),
        crls: None,
        signer_infos: cms::signed_data::SignerInfos(signer_infos),
    };
    let info = ContentInfo {
        content_type: ID_SIGNED_DATA,
        content: Any::from_der(&signed.to_der()?)?,
    };
    Ok(info.to_der()?)
}
