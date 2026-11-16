#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Validation data (B-LT): a test PKI (root, CA, signer, delegated responders) whose OCSP
//! responses and CRL are made here, well or in each of the ways they must be refused.

mod common;

use std::time::Duration;

use const_oid::ObjectIdentifier;
use der::asn1::{Any, BitString, GeneralizedTime, OctetString, UtcTime};
use der::{Decode, Encode, Sequence, Tag, TagNumber};
use ring::digest;
use sealwax_core::{Document, Object, OpenOptions};
use sealwax_sign::{
    Error, Kind, Options, RevocationFetcher, Trust, Validation, ValidationError,
    add_validation_data, sign,
};
use x509_cert::Certificate;
use x509_cert::crl::{CertificateList, RevokedCert, TbsCertList};
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::AlgorithmIdentifierOwned;
use x509_cert::time::Time;

use common::{ECDSA_WITH_SHA256, Key, OCSP_SIGNING, Profile, TestSigner, certificate, document};

const ROOT: &str = "CN=Test Root,O=sealwax";
const CA: &str = "CN=Test CA,O=sealwax";
const CA_OCSP: &str = "http://ocsp.test/ca";
const CA_CRL: &str = "http://crl.test/ca.crl";
const ROOT_OCSP: &str = "http://ocsp.test/root";
const SIGNER_SERIAL: u32 = 100;
const ID_PKIX_OCSP_BASIC: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1.1");

#[derive(Sequence)]
struct CertId {
    hash_algorithm: AlgorithmIdentifierOwned,
    issuer_name_hash: OctetString,
    issuer_key_hash: OctetString,
    serial_number: SerialNumber,
}

#[derive(Sequence)]
struct Request {
    req_cert: CertId,
}

#[derive(Sequence)]
struct TbsRequest {
    request_list: Vec<Request>,
}

#[derive(Sequence)]
struct OcspRequest {
    tbs_request: TbsRequest,
}

#[derive(Sequence)]
struct SingleResponse {
    cert_id: CertId,
    cert_status: Any,
    this_update: GeneralizedTime,
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT", optional = "true")]
    next_update: Option<GeneralizedTime>,
}

#[derive(Sequence)]
struct ResponseData {
    #[asn1(context_specific = "2", tag_mode = "EXPLICIT")]
    responder_key_hash: OctetString,
    produced_at: GeneralizedTime,
    responses: Vec<SingleResponse>,
}

#[derive(Sequence)]
struct BasicOcspResponse {
    tbs_response_data: ResponseData,
    signature_algorithm: AlgorithmIdentifierOwned,
    signature: BitString,
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT", optional = "true")]
    certs: Option<Vec<Certificate>>,
}

#[derive(Sequence)]
struct ResponseBytes {
    response_type: ObjectIdentifier,
    response: OctetString,
}

#[derive(Sequence)]
struct OcspResponse {
    response_status: Any,
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT", optional = "true")]
    response_bytes: Option<ResponseBytes>,
}

/// Who signs the CA's OCSP responses.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Responder {
    /// The CA itself.
    #[default]
    Issuer,
    /// A responder the CA delegated, exempt from revocation checking.
    Nocheck,
    /// A responder the CA delegated, whose own revocation the CA's CRL tells.
    Checked,
}

/// How the test PKI's services answer.
#[derive(Clone, Copy, Default)]
struct Behaviour {
    responder: Responder,
    /// The CA's responder says the signer is revoked.
    revoked: bool,
    /// The CA's responder answers with responses past their next update.
    stale: bool,
    /// The CA's responder does not answer.
    ocsp_down: bool,
    /// The CA's CRL is not published.
    crl_down: bool,
    /// The CA's CRL lists the signer.
    crl_revoked: bool,
}

struct Pki {
    root_key: Key,
    root: Vec<u8>,
    ca_key: Key,
    ca: Vec<u8>,
    nocheck_key: Key,
    nocheck: Vec<u8>,
    checked_key: Key,
    checked: Vec<u8>,
    signer: TestSigner,
}

impl Pki {
    fn new() -> Self {
        let root_key = Key::new();
        let mut profile = Profile::new(ROOT, 1);
        profile.authority = true;
        let root = certificate(&root_key, &profile);
        let ca_key = Key::new();
        let mut profile = Profile::new(CA, 2);
        profile.authority = true;
        profile.issuer = Some((ROOT, &root_key));
        profile.ocsp = Some(ROOT_OCSP);
        let ca = certificate(&ca_key, &profile);
        let delegated = |subject, serial, nocheck| {
            let key = Key::new();
            let mut profile = Profile::new(subject, serial);
            profile.issuer = Some((CA, &ca_key));
            profile.purpose = Some(OCSP_SIGNING);
            profile.nocheck = nocheck;
            if !nocheck {
                profile.crl = Some(CA_CRL);
            }
            let cert = certificate(&key, &profile);
            (key, cert)
        };
        let (nocheck_key, nocheck) = delegated("CN=Test OCSP nocheck,O=sealwax", 3, true);
        let (checked_key, checked) = delegated("CN=Test OCSP checked,O=sealwax", 4, false);
        let key = Key::new();
        let mut profile = Profile::new("CN=Test Signer,O=sealwax", SIGNER_SERIAL);
        profile.issuer = Some((CA, &ca_key));
        profile.ocsp = Some(CA_OCSP);
        profile.crl = Some(CA_CRL);
        let signer_certificate = certificate(&key, &profile);
        Self {
            root_key,
            root,
            ca_key,
            ca: ca.clone(),
            nocheck_key,
            nocheck,
            checked_key,
            checked,
            signer: TestSigner {
                key,
                certificate: signer_certificate,
                chain: vec![ca],
            },
        }
    }

    fn trust(&self) -> Trust {
        Trust::new(std::slice::from_ref(&self.root), &[]).unwrap()
    }
}

fn time(at: u64) -> GeneralizedTime {
    GeneralizedTime::from_unix_duration(Duration::from_secs(at)).unwrap()
}

fn signature_algorithm() -> AlgorithmIdentifierOwned {
    AlgorithmIdentifierOwned {
        oid: ECDSA_WITH_SHA256,
        parameters: None,
    }
}

/// The services of the PKI, as a fetcher sees them.
struct Services<'a> {
    pki: &'a Pki,
    behaviour: Behaviour,
}

impl Services<'_> {
    fn respond(
        &self,
        request: &[u8],
        issuer_key: &Key,
        signer: (&Key, Option<&[u8]>),
        stale: bool,
    ) -> Vec<u8> {
        let request = OcspRequest::from_der(request).unwrap();
        let cert_id = request
            .tbs_request
            .request_list
            .into_iter()
            .next()
            .unwrap()
            .req_cert;
        // The responder knows its certificates by the issuer's key hash it is asked about.
        assert_eq!(
            cert_id.issuer_key_hash.as_bytes(),
            digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, issuer_key.public()).as_ref()
        );
        let now = common::now();
        let revoked =
            self.behaviour.revoked && cert_id.serial_number == SerialNumber::from(SIGNER_SERIAL);
        let status = if revoked {
            Any::new(
                Tag::ContextSpecific {
                    constructed: true,
                    number: TagNumber::N1,
                },
                time(now - 3600).to_der().unwrap(),
            )
            .unwrap()
        } else {
            Any::new(
                Tag::ContextSpecific {
                    constructed: false,
                    number: TagNumber::N0,
                },
                Vec::new(),
            )
            .unwrap()
        };
        let (this_update, next_update) = if stale {
            (now - 10 * 86_400, now - 9 * 86_400)
        } else {
            (now - 60, now + 86_400)
        };
        let (key, embedded) = signer;
        let data = ResponseData {
            responder_key_hash: OctetString::new(
                digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, key.public()).as_ref(),
            )
            .unwrap(),
            produced_at: time(this_update),
            responses: vec![SingleResponse {
                cert_id,
                cert_status: status,
                this_update: time(this_update),
                next_update: Some(time(next_update)),
            }],
        };
        let signature = key.sign(&data.to_der().unwrap());
        let basic = BasicOcspResponse {
            tbs_response_data: data,
            signature_algorithm: signature_algorithm(),
            signature: BitString::from_bytes(&signature).unwrap(),
            certs: embedded.map(|der| vec![Certificate::from_der(der).unwrap()]),
        };
        OcspResponse {
            response_status: Any::new(Tag::Enumerated, vec![0]).unwrap(),
            response_bytes: Some(ResponseBytes {
                response_type: ID_PKIX_OCSP_BASIC,
                response: OctetString::new(basic.to_der().unwrap()).unwrap(),
            }),
        }
        .to_der()
        .unwrap()
    }

    fn ca_crl(&self) -> Vec<u8> {
        let now = common::now();
        let utc =
            |at: u64| Time::UtcTime(UtcTime::from_unix_duration(Duration::from_secs(at)).unwrap());
        let revoked = self.behaviour.crl_revoked.then(|| {
            vec![RevokedCert {
                serial_number: SerialNumber::from(SIGNER_SERIAL),
                revocation_date: utc(now - 3600),
                crl_entry_extensions: None,
            }]
        });
        let tbs = TbsCertList {
            version: x509_cert::Version::V2,
            signature: signature_algorithm(),
            issuer: Certificate::from_der(&self.pki.ca)
                .unwrap()
                .tbs_certificate
                .subject,
            this_update: utc(now - 60),
            next_update: Some(utc(now + 86_400)),
            revoked_certificates: revoked,
            crl_extensions: None,
        };
        let signature = self.pki.ca_key.sign(&tbs.to_der().unwrap());
        CertificateList {
            tbs_cert_list: tbs,
            signature_algorithm: signature_algorithm(),
            signature: BitString::from_bytes(&signature).unwrap(),
        }
        .to_der()
        .unwrap()
    }
}

impl RevocationFetcher for Services<'_> {
    fn ocsp(&self, url: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        let pki = self.pki;
        match url {
            ROOT_OCSP => Ok(self.respond(request, &pki.root_key, (&pki.root_key, None), false)),
            CA_OCSP if self.behaviour.ocsp_down => Err("connection refused".to_owned()),
            CA_OCSP => {
                let signer = match self.behaviour.responder {
                    Responder::Issuer => (&pki.ca_key, None),
                    Responder::Nocheck => (&pki.nocheck_key, Some(pki.nocheck.as_slice())),
                    Responder::Checked => (&pki.checked_key, Some(pki.checked.as_slice())),
                };
                Ok(self.respond(request, &pki.ca_key, signer, self.behaviour.stale))
            }
            _ => Err(format!("unknown address {url}")),
        }
    }

    fn crl(&self, url: &str) -> Result<Vec<u8>, String> {
        match url {
            CA_CRL if !self.behaviour.crl_down => Ok(self.ca_crl()),
            _ => Err("not found".to_owned()),
        }
    }
}

/// The store of `data`: its certificates, OCSP responses and CRLs, and its VRI keys.
fn store(data: &[u8]) -> (usize, usize, usize, Vec<String>) {
    let doc = Document::open(data.to_vec(), &OpenOptions::default()).unwrap();
    let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
    let dss = match catalog.as_dict().unwrap().get(b"DSS").unwrap() {
        Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
        other => other.clone(),
    };
    let dss = dss.as_dict().unwrap();
    let count = |key: &[u8]| {
        dss.get(key)
            .map_or(0, |array| array.as_array().unwrap().len())
    };
    let vri = dss.get(b"VRI").unwrap().as_dict().unwrap();
    let mut keys: Vec<String> = vri
        .iter()
        .map(|(key, _)| String::from_utf8(key.as_bytes().to_vec()).unwrap())
        .collect();
    keys.sort();
    (count(b"Certs"), count(b"OCSPs"), count(b"CRLs"), keys)
}

/// The VRI key of the signature in field `name`: the SHA-1 of its /Contents, upper-case hex.
fn vri_key(data: &[u8], name: &str) -> String {
    let doc = Document::open(data.to_vec(), &OpenOptions::default()).unwrap();
    let catalog = doc.object(doc.catalog_id()).unwrap().unwrap();
    let form = match catalog.as_dict().unwrap().get(b"AcroForm").unwrap() {
        Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
        other => other.clone(),
    };
    for field in form
        .as_dict()
        .unwrap()
        .get(b"Fields")
        .unwrap()
        .as_array()
        .unwrap()
    {
        let field = doc.object(field.as_ref_id().unwrap()).unwrap().unwrap();
        let field = field.as_dict().unwrap();
        if field.get(b"T").unwrap().as_string() != Some(name.as_bytes()) {
            continue;
        }
        let signature = doc
            .object(field.get(b"V").unwrap().as_ref_id().unwrap())
            .unwrap()
            .unwrap();
        let contents = signature.as_dict().unwrap().get(b"Contents").unwrap();
        return digest::digest(
            &digest::SHA1_FOR_LEGACY_USE_ONLY,
            contents.as_string().unwrap(),
        )
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    }
    panic!("no field {name}");
}

fn certified(pki: &Pki) -> Vec<u8> {
    sign(
        &document(),
        &pki.signer,
        &Options::new("Signature1", Kind::certification()),
        None,
    )
    .unwrap()
    .pdf
}

fn with_data(
    pdf: &[u8],
    field: &str,
    pki: &Pki,
    behaviour: Behaviour,
) -> Result<(Vec<u8>, sealwax_sign::Coverage), Error> {
    let services = Services { pki, behaviour };
    let trust = pki.trust();
    add_validation_data(
        pdf,
        field,
        &Validation {
            fetcher: &services,
            trust: &trust,
            time: None,
        },
    )
}

#[test]
fn ocsp_from_the_issuers_covers_the_path_and_a_second_signature_shares_the_store() {
    let pki = Pki::new();
    let signed = certified(&pki);
    let (data, coverage) = with_data(&signed, "Signature1", &pki, Behaviour::default()).unwrap();
    assert!(data.starts_with(&signed), "an incremental revision");
    assert_eq!(
        (
            coverage.certificates,
            coverage.ocsp_responses,
            coverage.crls
        ),
        (3, 2, 0),
        "signer, CA, root; the signer's and the CA's responses"
    );
    let first_key = vri_key(&data, "Signature1");
    assert_eq!(store(&data), (3, 2, 0, vec![first_key.clone()]));
    assert_eq!(
        common::verify(&data, pki.signer.public_key()),
        [("Signature1".to_owned(), true)]
    );

    let approved = sign(
        &data,
        &pki.signer,
        &Options::new("Signature2", Kind::Approval),
        None,
    )
    .unwrap()
    .pdf;
    let (data, _) = with_data(&approved, "Signature2", &pki, Behaviour::default()).unwrap();
    let mut keys = vec![first_key, vri_key(&data, "Signature2")];
    keys.sort();
    // The certificates are stored once; the new responses are added.
    assert_eq!(store(&data), (3, 4, 0, keys));
}

#[test]
fn a_delegated_responder_needs_a_proof_unless_exempt() {
    let pki = Pki::new();
    let signed = certified(&pki);
    let nocheck = Behaviour {
        responder: Responder::Nocheck,
        ..Behaviour::default()
    };
    let (_, coverage) = with_data(&signed, "Signature1", &pki, nocheck).unwrap();
    assert_eq!(
        (
            coverage.certificates,
            coverage.ocsp_responses,
            coverage.crls
        ),
        (3, 2, 0)
    );
    let checked = Behaviour {
        responder: Responder::Checked,
        ..Behaviour::default()
    };
    let (_, coverage) = with_data(&signed, "Signature1", &pki, checked).unwrap();
    assert_eq!(
        (
            coverage.certificates,
            coverage.ocsp_responses,
            coverage.crls
        ),
        (4, 2, 1),
        "the responder's certificate, and the CA's CRL for it"
    );
}

#[test]
fn a_stale_response_gives_way_to_the_crl() {
    let pki = Pki::new();
    let signed = certified(&pki);
    let stale = Behaviour {
        stale: true,
        ..Behaviour::default()
    };
    // The root's responder is not stale: the CA keeps its OCSP response.
    let (_, coverage) = with_data(&signed, "Signature1", &pki, stale).unwrap();
    assert_eq!(
        (
            coverage.certificates,
            coverage.ocsp_responses,
            coverage.crls
        ),
        (3, 1, 1)
    );
}

#[test]
fn revoked_unavailable_and_untrusted_are_refused() {
    let pki = Pki::new();
    let signed = certified(&pki);
    for behaviour in [
        Behaviour {
            revoked: true,
            ..Behaviour::default()
        },
        Behaviour {
            ocsp_down: true,
            crl_revoked: true,
            ..Behaviour::default()
        },
    ] {
        match with_data(&signed, "Signature1", &pki, behaviour) {
            Err(Error::Validation(ValidationError::Revoked { certificate })) => {
                assert_eq!(certificate, "CN=Test Signer,O=sealwax");
            }
            other => panic!("{other:?}"),
        }
    }
    let down = Behaviour {
        ocsp_down: true,
        crl_down: true,
        ..Behaviour::default()
    };
    match with_data(&signed, "Signature1", &pki, down) {
        Err(Error::Validation(ValidationError::Unavailable {
            certificate,
            reasons,
        })) => {
            assert_eq!(certificate, "CN=Test Signer,O=sealwax");
            assert!(
                reasons.contains(CA_OCSP) && reasons.contains(CA_CRL),
                "{reasons}"
            );
        }
        other => panic!("{other:?}"),
    }
    let stranger = Pki::new();
    let services = Services {
        pki: &pki,
        behaviour: Behaviour::default(),
    };
    let trust = stranger.trust();
    let validation = Validation {
        fetcher: &services,
        trust: &trust,
        time: None,
    };
    assert!(matches!(
        add_validation_data(&signed, "Signature1", &validation),
        Err(Error::Validation(ValidationError::Untrusted))
    ));
    assert!(matches!(
        add_validation_data(&signed, "Nothing", &validation),
        Err(Error::Validation(ValidationError::NotSigned))
    ));
}

/// The CA's CRL, given a critical CRL extension: a delta CRL (`deltaCRLIndicator`) or one
/// issued for CA certificates only (`issuingDistributionPoint`), its OCSP responder down.
struct ScopedCrl<'a> {
    base: Services<'a>,
    oid: &'static str,
    content: Vec<u8>,
}

impl RevocationFetcher for ScopedCrl<'_> {
    fn ocsp(&self, url: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        if url == CA_OCSP {
            Err("down".to_owned())
        } else {
            self.base.ocsp(url, request)
        }
    }

    fn crl(&self, _url: &str) -> Result<Vec<u8>, String> {
        let mut list = CertificateList::from_der(&self.base.ca_crl()).unwrap();
        list.tbs_cert_list.crl_extensions = Some(vec![x509_cert::ext::Extension {
            extn_id: self.oid.parse().unwrap(),
            critical: true,
            extn_value: OctetString::new(self.content.clone()).unwrap(),
        }]);
        list.signature = BitString::from_bytes(
            &self
                .base
                .pki
                .ca_key
                .sign(&list.tbs_cert_list.to_der().unwrap()),
        )
        .unwrap();
        Ok(list.to_der().unwrap())
    }
}

/// A CRL that may leave the signer out (a delta CRL without its base, one for CA certificates
/// only) proves nothing by its silence (external review: both were taken as proofs).
#[test]
fn a_crl_that_does_not_cover_the_certificate_is_no_proof() {
    let pki = Pki::new();
    let signed = sign(
        &document(),
        &pki.signer,
        &Options::new("S", Kind::Approval),
        None,
    )
    .unwrap();
    for (oid, content) in [
        ("2.5.29.27", 7u32.to_der().unwrap()),
        ("2.5.29.28", vec![0x30, 3, 0x82, 1, 0xff]),
    ] {
        let fetcher = ScopedCrl {
            base: Services {
                pki: &pki,
                behaviour: Behaviour::default(),
            },
            oid,
            content,
        };
        let trust = pki.trust();
        let validation = Validation {
            fetcher: &fetcher,
            trust: &trust,
            time: None,
        };
        match add_validation_data(&signed.pdf, "S", &validation) {
            Err(Error::Validation(ValidationError::Unavailable { reasons, .. })) => {
                assert!(reasons.contains("does not cover"), "{oid}: {reasons}");
            }
            other => panic!(
                "{oid}: not refused: {:?}",
                other.map(|(_, coverage)| coverage)
            ),
        }
    }
}

/// OCSP answers from a new delegated responder each time, none with `ocsp-nocheck`: each needs a
/// proof of its own, past what sealwax gathers. The data is refused, never returned with a
/// responder left uncovered (external review: it was returned).
struct ChainedResponders<'a> {
    base: Services<'a>,
    responders: Vec<(Key, Vec<u8>)>,
    calls: std::cell::Cell<usize>,
}

impl RevocationFetcher for ChainedResponders<'_> {
    fn ocsp(&self, url: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        if url == ROOT_OCSP {
            return self.base.ocsp(url, request);
        }
        let index = self.calls.get();
        self.calls.set(index + 1);
        let (key, cert) = self.responders.get(index).ok_or("too many".to_owned())?;
        Ok(self
            .base
            .respond(request, &self.base.pki.ca_key, (key, Some(cert)), false))
    }

    fn crl(&self, _url: &str) -> Result<Vec<u8>, String> {
        Err("unavailable".to_owned())
    }
}

#[test]
fn coverage_past_its_limit_is_an_error() {
    let pki = Pki::new();
    let signed = sign(
        &document(),
        &pki.signer,
        &Options::new("S", Kind::Approval),
        None,
    )
    .unwrap();
    let responders = (0..40)
        .map(|i| {
            let key = Key::new();
            let name = format!("CN=Responder {i}");
            let mut profile = Profile::new(&name, 200 + i);
            profile.issuer = Some((CA, &pki.ca_key));
            profile.purpose = Some(OCSP_SIGNING);
            profile.ocsp = Some(CA_OCSP);
            let cert = certificate(&key, &profile);
            (key, cert)
        })
        .collect();
    let fetcher = ChainedResponders {
        base: Services {
            pki: &pki,
            behaviour: Behaviour::default(),
        },
        responders,
        calls: std::cell::Cell::new(0),
    };
    let trust = pki.trust();
    let validation = Validation {
        fetcher: &fetcher,
        trust: &trust,
        time: None,
    };
    assert!(matches!(
        add_validation_data(&signed.pdf, "S", &validation),
        Err(Error::Validation(ValidationError::Incomplete {
            limit: 32,
            ..
        }))
    ));
}

/// A field name outside ASCII is written in UTF-16BE, and found again by the same text
/// (external review: `add_validation_data("Signé")` did not find the field "Signé").
#[test]
fn a_signature_named_outside_ascii_is_found_by_its_name() {
    let pki = Pki::new();
    let signed = sign(
        &document(),
        &pki.signer,
        &Options::new("Signé", Kind::Approval),
        None,
    )
    .unwrap()
    .pdf;
    let (_, coverage) = with_data(&signed, "Signé", &pki, Behaviour::default()).unwrap();
    assert_eq!(coverage.certificates, 3);
}

/// A CRL proves something only when its issuer's key usage, if given, allows signing CRLs
/// (RFC 5280, 6.3.3).
#[test]
fn a_crl_from_an_issuer_not_allowed_to_sign_crls_is_no_proof() {
    // keyCertSign only (bit 5), then keyCertSign and cRLSign (bits 5 and 6).
    for (usage, proved) in [
        (vec![0x03, 0x02, 0x02, 0x04], false),
        (vec![0x03, 0x02, 0x01, 0x06], true),
    ] {
        let mut pki = Pki::new();
        pki.ca = common::with_extension(&pki.ca, &pki.root_key, "2.5.29.15", usage, true);
        pki.signer.chain = vec![pki.ca.clone()];
        let signed = certified(&pki);
        // A stale OCSP response sends the signer's proof to the CA's CRL.
        let stale = Behaviour {
            stale: true,
            ..Behaviour::default()
        };
        let result = with_data(&signed, "Signature1", &pki, stale);
        if proved {
            assert_eq!(result.unwrap().1.crls, 1);
        } else {
            match result {
                Err(Error::Validation(ValidationError::Unavailable { reasons, .. })) => {
                    assert!(reasons.contains("not authorized"), "{reasons}");
                }
                other => panic!("not refused: {:?}", other.map(|(_, coverage)| coverage)),
            }
        }
    }
}
