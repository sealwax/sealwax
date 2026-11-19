#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! RFC 3161 timestamps: a test authority (P-256) answers, well or in each of the ways a
//! response must be refused.

mod common;

use std::time::Duration;

use cms::cert::IssuerAndSerialNumber;
use cms::cert::x509::attr::{Attribute, AttributeValue};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{
    CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo, SignerInfos,
};
use const_oid::ObjectIdentifier;
use der::asn1::{Any, GeneralizedTime, Int, OctetString, SetOfVec};
use der::{Decode, Encode, Sequence, Tag};
use ring::digest;
use sealwax_sign::{
    Error, Integrity, Kind, Options, Policy, SignatureKind, TimestampClient, TimestampError,
    TimestampRequest, Timestamping, Trust, add_document_timestamp, check_timestamp_token, sign,
    verify_signatures,
};
use x509_cert::Certificate;
use x509_cert::spki::AlgorithmIdentifierOwned;

use common::{
    ECDSA_WITH_SHA256, Key, Profile, TIME_STAMPING, TestSigner, certificate, document,
    with_extension,
};

const ID_SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");
const ID_CT_TST_INFO: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.1.4");
const ID_CONTENT_TYPE: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.3");
const ID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const ID_SIGNING_CERTIFICATE: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.12");
const ID_SIGNING_CERTIFICATE_V2: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.47");
const ID_TIME_STAMP_TOKEN: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.14");
const SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const POLICY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.99999.1");

const ROOT: &str = "CN=Test TSA Root,O=sealwax";
const CA: &str = "CN=Test TSA CA,O=sealwax";
const TSA: &str = "CN=Test TSA,O=sealwax";

#[derive(Sequence)]
struct MessageImprint {
    hash_algorithm: AlgorithmIdentifierOwned,
    hashed_message: OctetString,
}

#[derive(Sequence)]
struct TimeStampReq {
    version: u8,
    message_imprint: MessageImprint,
    nonce: Int,
    cert_req: bool,
}

#[derive(Sequence)]
struct TstInfo {
    version: u8,
    policy: ObjectIdentifier,
    message_imprint: MessageImprint,
    serial_number: u32,
    gen_time: GeneralizedTime,
    nonce: Int,
}

#[derive(Sequence)]
struct EssCertId {
    cert_hash: OctetString,
}

#[derive(Sequence)]
struct SigningCertificate {
    certs: Vec<EssCertId>,
}

#[derive(Sequence)]
struct PkiStatusInfo {
    status: u32,
}

#[derive(Sequence)]
struct TimeStampResp {
    status: PkiStatusInfo,
    #[asn1(optional = "true")]
    token: Option<ContentInfo>,
}

/// How the test authority answers.
#[derive(Clone, Copy, Default)]
struct Behaviour {
    status: u32,
    other_nonce: bool,
    other_imprint: bool,
    tampered: bool,
    /// The token's time; now when `None`.
    time: Option<u64>,
    /// `SigningCertificate` (v1, SHA-1) instead of v2.
    ess_v1: bool,
    no_ess: bool,
    /// The intermediate left out of the token.
    no_intermediate: bool,
}

/// A timestamp authority: its root, possibly an intermediate, its own certificate and key.
struct Authority {
    root: Vec<u8>,
    intermediate: Option<Vec<u8>>,
    certificate: Vec<u8>,
    key: Key,
}

impl Authority {
    fn new(purpose: Option<ObjectIdentifier>, intermediate: bool) -> Self {
        let root_key = Key::new();
        let mut root = Profile::new(ROOT, 1);
        root.authority = true;
        let root = certificate(&root_key, &root);
        let ca_key = Key::new();
        let ca = intermediate.then(|| {
            let mut profile = Profile::new(CA, 2);
            profile.authority = true;
            profile.issuer = Some((ROOT, &root_key));
            certificate(&ca_key, &profile)
        });
        let key = Key::new();
        let mut profile = Profile::new(TSA, 3);
        profile.purpose = purpose;
        profile.issuer = Some(if intermediate {
            (CA, &ca_key)
        } else {
            (ROOT, &root_key)
        });
        let certificate = certificate(&key, &profile);
        Self {
            root,
            intermediate: ca,
            certificate,
            key,
        }
    }

    fn trust(&self) -> Trust {
        Trust::new(std::slice::from_ref(&self.root), &[]).unwrap()
    }

    fn respond(&self, request: &[u8], behaviour: Behaviour) -> Vec<u8> {
        if behaviour.status > 1 {
            return TimeStampResp {
                status: PkiStatusInfo {
                    status: behaviour.status,
                },
                token: None,
            }
            .to_der()
            .unwrap();
        }
        let request = TimeStampReq::from_der(request).unwrap();
        assert!(request.cert_req);
        let mut imprint = request.message_imprint;
        if behaviour.other_imprint {
            imprint.hashed_message = OctetString::new(vec![0; 32]).unwrap();
        }
        let nonce = if behaviour.other_nonce {
            Int::new(&[1, 2, 3]).unwrap()
        } else {
            request.nonce
        };
        let time = behaviour.time.unwrap_or_else(common::now);
        let tst = TstInfo {
            version: 1,
            policy: POLICY,
            message_imprint: imprint,
            serial_number: 7,
            gen_time: GeneralizedTime::from_unix_duration(Duration::from_secs(time)).unwrap(),
            nonce,
        }
        .to_der()
        .unwrap();
        let mut attributes = SetOfVec::new();
        attributes
            .insert(attribute(ID_CONTENT_TYPE, &ID_CT_TST_INFO))
            .unwrap();
        attributes
            .insert(attribute(
                ID_MESSAGE_DIGEST,
                &OctetString::new(digest::digest(&digest::SHA256, &tst).as_ref()).unwrap(),
            ))
            .unwrap();
        if !behaviour.no_ess {
            let (oid, algorithm) = if behaviour.ess_v1 {
                (ID_SIGNING_CERTIFICATE, &digest::SHA1_FOR_LEGACY_USE_ONLY)
            } else {
                (ID_SIGNING_CERTIFICATE_V2, &digest::SHA256)
            };
            let hash = digest::digest(algorithm, &self.certificate);
            let value = SigningCertificate {
                certs: vec![EssCertId {
                    cert_hash: OctetString::new(hash.as_ref()).unwrap(),
                }],
            };
            attributes.insert(attribute(oid, &value)).unwrap();
        }
        let mut signature = self.key.sign(&attributes.to_der().unwrap());
        if behaviour.tampered {
            let last = signature.len() - 1;
            signature[last] ^= 1;
        }
        let tsa = Certificate::from_der(&self.certificate).unwrap();
        let signer = SignerInfo {
            version: CmsVersion::V1,
            sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
                issuer: tsa.tbs_certificate.issuer.clone(),
                serial_number: tsa.tbs_certificate.serial_number.clone(),
            }),
            digest_alg: AlgorithmIdentifierOwned {
                oid: SHA256,
                parameters: None,
            },
            signed_attrs: Some(attributes),
            signature_algorithm: AlgorithmIdentifierOwned {
                oid: ECDSA_WITH_SHA256,
                parameters: None,
            },
            signature: OctetString::new(signature).unwrap(),
            unsigned_attrs: None,
        };
        let mut certificates = SetOfVec::new();
        certificates
            .insert(cms::cert::CertificateChoices::Certificate(tsa))
            .unwrap();
        if let Some(intermediate) = &self.intermediate
            && !behaviour.no_intermediate
        {
            certificates
                .insert(cms::cert::CertificateChoices::Certificate(
                    Certificate::from_der(intermediate).unwrap(),
                ))
                .unwrap();
        }
        let mut digests = SetOfVec::new();
        digests
            .insert(AlgorithmIdentifierOwned {
                oid: SHA256,
                parameters: None,
            })
            .unwrap();
        let mut signers = SetOfVec::new();
        signers.insert(signer).unwrap();
        let signed = SignedData {
            version: CmsVersion::V3,
            digest_algorithms: digests,
            encap_content_info: EncapsulatedContentInfo {
                econtent_type: ID_CT_TST_INFO,
                econtent: Some(Any::new(Tag::OctetString, tst).unwrap()),
            },
            certificates: Some(CertificateSet(certificates)),
            crls: None,
            signer_infos: SignerInfos(signers),
        };
        TimeStampResp {
            status: PkiStatusInfo {
                status: behaviour.status,
            },
            token: Some(ContentInfo {
                content_type: ID_SIGNED_DATA,
                content: Any::from_der(&signed.to_der().unwrap()).unwrap(),
            }),
        }
        .to_der()
        .unwrap()
    }
}

fn attribute(oid: ObjectIdentifier, value: &impl Encode) -> Attribute {
    let mut values = SetOfVec::new();
    values
        .insert(AttributeValue::from_der(&value.to_der().unwrap()).unwrap())
        .unwrap();
    Attribute { oid, values }
}

/// The authority as a client sees it.
struct Client<'a> {
    authority: &'a Authority,
    behaviour: Behaviour,
}

impl TimestampClient for Client<'_> {
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.authority.respond(request, self.behaviour))
    }
}

/// An authority out of reach.
struct Unreachable;

impl TimestampClient for Unreachable {
    fn timestamp(&self, _request: &[u8]) -> Result<Vec<u8>, String> {
        Err("connection refused".to_owned())
    }
}

/// The response to a request over `value`, checked against `trust`.
fn checked(
    authority: &Authority,
    behaviour: Behaviour,
    trust: &Trust,
) -> Result<sealwax_sign::Timestamp, TimestampError> {
    let request = TimestampRequest::new(b"a signature value").unwrap();
    let response = authority.respond(request.to_der(), behaviour);
    request.check_response(&response, trust)
}

#[test]
fn a_timestamped_signature_carries_a_token_that_checks() {
    let authority = Authority::new(Some(TIME_STAMPING), false);
    let trust = authority.trust();
    let signer = TestSigner::new();
    let client = Client {
        authority: &authority,
        behaviour: Behaviour::default(),
    };
    let timestamping = Timestamping {
        client: &client,
        trust: &trust,
        policy: Policy::Required,
    };
    let before = common::now();
    let signed = sign(
        &document(),
        &signer,
        &Options::new("Signature1", Kind::certification()),
        Some(&timestamping),
    )
    .unwrap();
    let timestamp = signed.timestamp.unwrap();
    assert!(signed.timestamp_error.is_none());
    let time = u64::try_from(timestamp.time).unwrap();
    assert!(before <= time && time <= common::now());
    assert_eq!(timestamp.authority, "CN=Test TSA,O=sealwax");
    assert_eq!(
        timestamp.chain,
        [authority.certificate.clone(), authority.root.clone()]
    );

    // The token is the signature's unsigned attribute, over its value, and still checks there.
    let read = common::signatures(&signed.pdf, signer.public_key());
    assert_eq!(read.len(), 1);
    assert!(read[0].verified);
    let info = &read[0].signer_info;
    let unsigned = info.unsigned_attrs.as_ref().unwrap();
    let attribute = unsigned.iter().next().unwrap();
    assert_eq!(attribute.oid, ID_TIME_STAMP_TOKEN);
    let token = attribute.values.iter().next().unwrap().to_der().unwrap();
    assert_eq!(token, timestamp.token);
    let again = check_timestamp_token(&token, info.signature.as_bytes(), &trust).unwrap();
    assert_eq!(again, timestamp);
    assert_eq!(
        check_timestamp_token(&token, b"another value", &trust),
        Err(TimestampError::Imprint)
    );
}

#[test]
fn each_check_of_the_response_holds() {
    let authority = Authority::new(Some(TIME_STAMPING), false);
    let trust = authority.trust();
    let good = Behaviour::default();
    assert!(checked(&authority, good, &trust).is_ok());
    let cases = [
        (
            Behaviour { status: 2, ..good },
            TimestampError::Refused { status: 2 },
        ),
        (
            Behaviour {
                other_nonce: true,
                ..good
            },
            TimestampError::Nonce,
        ),
        (
            Behaviour {
                other_imprint: true,
                ..good
            },
            TimestampError::Imprint,
        ),
        (
            Behaviour {
                tampered: true,
                ..good
            },
            TimestampError::Signature,
        ),
        (
            Behaviour {
                no_ess: true,
                ..good
            },
            TimestampError::Certificate,
        ),
        (
            Behaviour {
                time: Some(common::now() + 2 * 86_400),
                ..good
            },
            TimestampError::Expired,
        ),
    ];
    for (behaviour, expected) in cases {
        assert_eq!(checked(&authority, behaviour, &trust), Err(expected));
    }
    // SigningCertificate v1 (SHA-1) names the certificate as well as v2.
    let v1 = Behaviour {
        ess_v1: true,
        ..good
    };
    assert!(checked(&authority, v1, &trust).is_ok());
    // Another root is not this authority's.
    let stranger = Authority::new(Some(TIME_STAMPING), false);
    assert_eq!(
        checked(&authority, good, &stranger.trust()),
        Err(TimestampError::Untrusted)
    );
    // A certificate not meant for timestamping.
    let unfit = Authority::new(None, false);
    assert_eq!(
        checked(&unfit, good, &unfit.trust()),
        Err(TimestampError::Certificate)
    );
    // Not a response at all.
    let request = TimestampRequest::new(b"a signature value").unwrap();
    assert_eq!(
        request.check_response(b"\x30\x03\x02\x01", &trust),
        Err(TimestampError::Malformed("response"))
    );
}

#[test]
fn an_intermediate_comes_from_the_token_or_from_the_trust() {
    let authority = Authority::new(Some(TIME_STAMPING), true);
    let trust = authority.trust();
    let chain = checked(&authority, Behaviour::default(), &trust)
        .unwrap()
        .chain;
    assert_eq!(chain.len(), 3);
    let left_out = Behaviour {
        no_intermediate: true,
        ..Behaviour::default()
    };
    assert_eq!(
        checked(&authority, left_out, &trust),
        Err(TimestampError::Untrusted)
    );
    let intermediate = authority.intermediate.clone().unwrap();
    let with = Trust::new(std::slice::from_ref(&authority.root), &[intermediate]).unwrap();
    assert_eq!(checked(&authority, left_out, &with).unwrap().chain.len(), 3);
}

#[test]
fn best_effort_signs_without_the_timestamp_required_does_not_sign() {
    let authority = Authority::new(Some(TIME_STAMPING), false);
    let trust = authority.trust();
    let signer = TestSigner::new();
    let options = Options::new("Signature1", Kind::certification());
    let mut timestamping = Timestamping {
        client: &Unreachable,
        trust: &trust,
        policy: Policy::BestEffort,
    };
    let signed = sign(&document(), &signer, &options, Some(&timestamping)).unwrap();
    assert!(signed.timestamp.is_none());
    assert_eq!(
        signed.timestamp_error,
        Some(TimestampError::Client("connection refused".to_owned()))
    );
    let read = common::signatures(&signed.pdf, signer.public_key());
    assert!(read[0].verified && read[0].signer_info.unsigned_attrs.is_none());

    timestamping.policy = Policy::Required;
    assert!(matches!(
        sign(&document(), &signer, &options, Some(&timestamping)),
        Err(Error::Timestamp(TimestampError::Client(_)))
    ));
    // A response refused is a failure as well.
    let tampered = Client {
        authority: &authority,
        behaviour: Behaviour {
            tampered: true,
            ..Behaviour::default()
        },
    };
    timestamping.client = &tampered;
    assert!(matches!(
        sign(&document(), &signer, &options, Some(&timestamping)),
        Err(Error::Timestamp(TimestampError::Signature))
    ));
}

/// An issuer under a `pathLenConstraint` of 0 cannot have an issuer below it, and a critical
/// extension this crate does not understand refuses its certificate (RFC 5280; external review:
/// both were accepted).
#[test]
fn path_length_and_unknown_critical_extensions_are_enforced() {
    use x509_cert::ext::pkix::BasicConstraints;
    let root_key = Key::new();
    let mut root = Profile::new(ROOT, 1);
    root.authority = true;
    let root = certificate(&root_key, &root);
    let ca_key = Key::new();
    let mut ca = Profile::new(CA, 2);
    ca.authority = true;
    ca.issuer = Some((ROOT, &root_key));
    let ca = certificate(&ca_key, &ca);
    let limited = BasicConstraints {
        ca: true,
        path_len_constraint: Some(0),
    };
    let ca = with_extension(&ca, &root_key, "2.5.29.19", limited.to_der().unwrap(), true);
    let sub_key = Key::new();
    let mut sub = Profile::new("CN=Sub CA below the limit", 3);
    sub.authority = true;
    sub.issuer = Some((CA, &ca_key));
    let sub = certificate(&sub_key, &sub);
    let key = Key::new();
    let mut tsa = Profile::new(TSA, 4);
    tsa.purpose = Some(TIME_STAMPING);
    tsa.issuer = Some(("CN=Sub CA below the limit", &sub_key));
    let tsa = certificate(&key, &tsa);
    let authority = Authority {
        root: root.clone(),
        intermediate: Some(sub),
        certificate: tsa,
        key,
    };
    let trust = Trust::new(&[root], &[ca]).unwrap();
    let request = TimestampRequest::new(b"value").unwrap();
    let response = authority.respond(request.to_der(), Behaviour::default());
    assert!(matches!(
        request.check_response(&response, &trust),
        Err(TimestampError::Untrusted)
    ));

    let root_key = Key::new();
    let mut root = Profile::new(ROOT, 1);
    root.authority = true;
    let root = certificate(&root_key, &root);
    let key = Key::new();
    let mut tsa = Profile::new(TSA, 2);
    tsa.purpose = Some(TIME_STAMPING);
    tsa.issuer = Some((ROOT, &root_key));
    let tsa = certificate(&key, &tsa);
    let unknown = with_extension(&tsa, &root_key, "1.2.3.4.5.6.7", vec![0x05, 0], true);
    let authority = Authority {
        root,
        intermediate: None,
        certificate: unknown,
        key,
    };
    let request = TimestampRequest::new(b"value").unwrap();
    let response = authority.respond(request.to_der(), Behaviour::default());
    assert!(matches!(
        request.check_response(&response, &authority.trust()),
        Err(TimestampError::Untrusted)
    ));
    // The same extension, not critical, is only information.
    let informative = with_extension(&tsa, &root_key, "1.2.3.4.5.6.7", vec![0x05, 0], false);
    let authority = Authority {
        certificate: informative,
        ..authority
    };
    let request = TimestampRequest::new(b"value").unwrap();
    let response = authority.respond(request.to_der(), Behaviour::default());
    assert!(
        request
            .check_response(&response, &authority.trust())
            .is_ok()
    );
}

/// A key usage, when a certificate gives one, must allow what the certificate is used for: a
/// timestamp authority's must allow signing (external review: one allowing only key encipherment,
/// critical, was accepted).
#[test]
fn a_timestamp_authority_whose_key_usage_forbids_signing_is_refused() {
    let root_key = Key::new();
    let mut root = Profile::new(ROOT, 1);
    root.authority = true;
    let root = certificate(&root_key, &root);
    // keyEncipherment only (bit 2), then digitalSignature only (bit 0).
    for (usage, accepted) in [
        (vec![0x03, 0x02, 0x05, 0x20], false),
        (vec![0x03, 0x02, 0x07, 0x80], true),
    ] {
        let key = Key::new();
        let mut tsa = Profile::new(TSA, 2);
        tsa.purpose = Some(TIME_STAMPING);
        tsa.issuer = Some((ROOT, &root_key));
        let tsa = certificate(&key, &tsa);
        let authority = Authority {
            root: root.clone(),
            intermediate: None,
            certificate: with_extension(&tsa, &root_key, "2.5.29.15", usage, true),
            key,
        };
        let request = TimestampRequest::new(b"value").unwrap();
        let response = authority.respond(request.to_der(), Behaviour::default());
        let checked = request.check_response(&response, &authority.trust());
        if accepted {
            assert!(checked.is_ok(), "{checked:?}");
        } else {
            assert!(
                matches!(checked, Err(TimestampError::Untrusted)),
                "{checked:?}"
            );
        }
    }
}

/// A document timestamp (B-LTA) covers the whole document, its token checked like a
/// signature's; the signature's timestamp is told by the integrity check too.
#[test]
fn a_document_timestamp_covers_the_document() {
    let authority = Authority::new(Some(TIME_STAMPING), true);
    let trust = authority.trust();
    let signer = TestSigner::new();
    let client = Client {
        authority: &authority,
        behaviour: Behaviour::default(),
    };
    let timestamping = Timestamping {
        client: &client,
        trust: &trust,
        policy: Policy::Required,
    };
    let signed = sign(
        &document(),
        &signer,
        &Options::new("Signature1", Kind::certification()),
        Some(&timestamping),
    )
    .unwrap();
    let (stamped, timestamp) =
        add_document_timestamp(&signed.pdf, "DocTimeStamp1", &client, &trust).unwrap();
    assert!(stamped.starts_with(&signed.pdf), "an incremental revision");
    assert_eq!(timestamp.authority, "CN=Test TSA,O=sealwax");
    assert!(String::from_utf8_lossy(&stamped).contains("/ETSI.RFC3161"));
    let checks = verify_signatures(&stamped).unwrap();
    assert_eq!(checks.len(), 2);
    let signature = checks.iter().find(|c| c.field == "Signature1").unwrap();
    let stamp = checks.iter().find(|c| c.field == "DocTimeStamp1").unwrap();
    assert_eq!(signature.integrity, Integrity::Intact);
    assert_eq!(signature.timestamp_time, signed.timestamp.map(|t| t.time));
    assert_eq!(signature.later_revisions, 1);
    assert_eq!(stamp.kind, SignatureKind::DocumentTimestamp);
    assert_eq!(stamp.integrity, Integrity::Intact);
    assert_eq!(stamp.timestamp_time, Some(timestamp.time));
    assert_eq!(stamp.signer.as_deref(), Some("CN=Test TSA,O=sealwax"));

    // An authority not trusted, or one answering for other bytes, adds nothing.
    let stranger = Authority::new(Some(TIME_STAMPING), false);
    assert!(matches!(
        add_document_timestamp(&signed.pdf, "DocTimeStamp1", &client, &stranger.trust()),
        Err(Error::Timestamp(TimestampError::Untrusted))
    ));
    let lying = Client {
        authority: &authority,
        behaviour: Behaviour {
            other_imprint: true,
            ..Behaviour::default()
        },
    };
    assert!(matches!(
        add_document_timestamp(&signed.pdf, "DocTimeStamp1", &lying, &trust),
        Err(Error::Timestamp(TimestampError::Imprint))
    ));
    // A byte changed after the timestamp breaks it.
    let mut changed = stamped.clone();
    let last = changed.len() - 1;
    let at = (0..last).rev().find(|i| changed[*i] == b'0').unwrap();
    changed[at] = b'1';
    let checks = verify_signatures(&changed).unwrap();
    assert!(checks.iter().any(|c| !c.is_intact()), "{checks:?}");
}

#[test]
fn a_key_vault_signature_has_the_same_timestamp_in_both_signing_apis() {
    use sealwax_sign::{Algorithm, Signer, prepare};

    struct Vault(TestSigner);
    impl Signer for Vault {
        fn certificate(&self) -> &[u8] {
            self.0.certificate()
        }
        fn chain(&self) -> &[Vec<u8>] {
            self.0.chain()
        }
        fn algorithm(&self) -> Algorithm {
            self.0.algorithm()
        }
        fn sign(&self, message: &[u8], digest: &[u8]) -> Result<Vec<u8>, String> {
            #[derive(Sequence)]
            struct Pair {
                r: der::asn1::Uint,
                s: der::asn1::Uint,
            }
            let pair = Pair::from_der(&self.0.sign(message, digest)?).unwrap();
            let mut raw = [0u8; 64];
            for (value, slot) in [pair.r, pair.s].iter().zip(raw.chunks_mut(32)) {
                let value = value.as_bytes();
                slot[32 - value.len()..].copy_from_slice(value);
            }
            Ok(raw.to_vec())
        }
    }
    let signer = Vault(TestSigner::new());
    let authority = Authority::new(Some(TIME_STAMPING), false);
    let trust = authority.trust();
    let client = Client {
        authority: &authority,
        behaviour: Behaviour::default(),
    };
    let stamping = Timestamping {
        client: &client,
        trust: &trust,
        policy: Policy::Required,
    };
    let pdf = document();
    let options = Options::new("Approval", Kind::Approval);
    let one = sign(&pdf, &signer, &options, Some(&stamping)).unwrap().pdf;
    let prepared = prepare(
        &pdf,
        signer.certificate(),
        signer.chain(),
        signer.algorithm(),
        &options,
    )
    .unwrap();
    let raw = signer
        .sign(&prepared.message().unwrap(), prepared.digest())
        .unwrap();
    let request = prepared.timestamp_request(&raw).unwrap();
    let token = request
        .check_response(&client.timestamp(request.to_der()).unwrap(), &trust)
        .unwrap()
        .token;
    let two = prepared.finalize(&raw, Some(&token)).unwrap();
    for signed in [one, two] {
        let check = verify_signatures(&signed).unwrap().remove(0);
        assert!(check.is_intact());
        assert!(check.timestamp_time.is_some());
    }
}
