import datetime

import pytest
from conftest import text_page
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.hazmat.primitives.asymmetric.utils import Prehashed
from cryptography.x509.oid import NameOID

import sealwax


class KeySigner:
    """A key in memory signing digests, as an HSM does."""

    algorithm = "rsa-pkcs1v15-sha256"

    def __init__(self) -> None:
        self.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "sealwax test signer")])
        now = datetime.datetime.now(datetime.UTC)
        self.cert = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(self.key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(days=1))
            .not_valid_after(now + datetime.timedelta(days=1))
            .sign(self.key, hashes.SHA256())
        )
        self.digests: list[bytes] = []

    def certificate(self) -> bytes:
        return self.cert.public_bytes(serialization.Encoding.DER)

    chain: list[bytes] = []

    def sign_digest(self, digest: bytes) -> bytes:
        self.digests.append(digest)
        return self.key.sign(digest, padding.PKCS1v15(), Prehashed(hashes.SHA256()))


class Unreachable(Exception):
    pass


def test_a_certification_then_an_approval_append_revisions():
    signer = KeySigner()
    data = text_page()
    certified = sealwax.sign(data, signer, field="Certification", certify=True, reason="TEST")
    assert certified.pdf.startswith(data)
    assert b"/DocMDP" in certified.pdf and b"/ByteRange" in certified.pdf
    assert certified.timestamp_time is None and certified.timestamp_error is None
    assert len(signer.digests) == 1 and len(signer.digests[0]) == 32
    approved = sealwax.sign(certified.pdf, signer, field="Approval_1")
    assert approved.pdf.startswith(certified.pdf)
    with pytest.raises(sealwax.SignatureError, match="already has a field"):
        sealwax.sign(approved.pdf, signer, field="Approval_1")
    with pytest.raises(sealwax.SignatureError, match="first signature"):
        sealwax.sign(approved.pdf, signer, field="Again", certify=True)


def test_what_the_signer_raises_is_raised_as_is():
    class Failing(KeySigner):
        def sign_digest(self, digest: bytes) -> bytes:
            raise Unreachable("vault down")

    with pytest.raises(Unreachable, match="vault down"):
        sealwax.sign(text_page(), Failing(), field="Signature1")


def test_a_failed_timestamp_stops_the_signature_unless_not_required():
    signer = KeySigner()
    root = signer.certificate()

    def client(request: bytes) -> bytes:
        assert request.startswith(b"\x30")
        raise Unreachable("TSA down")

    required = sealwax.Timestamping(client, roots=[root])
    with pytest.raises(sealwax.TimestampError) as raised:
        sealwax.sign(text_page(), signer, field="Signature1", timestamp=required)
    assert isinstance(raised.value.__cause__, Unreachable)
    best_effort = sealwax.Timestamping(client, roots=[root], required=False)
    signed = sealwax.sign(text_page(), signer, field="Signature1", timestamp=best_effort)
    assert "TSA down" in signed.timestamp_error and signed.timestamp_time is None
    garbage = sealwax.Timestamping(lambda request: b"not DER", roots=[root])
    with pytest.raises(sealwax.TimestampError, match="cannot be read"):
        sealwax.sign(text_page(), signer, field="Signature1", timestamp=garbage)


def test_validation_data_needs_a_signature():
    class Nowhere:
        def ocsp(self, url: str, request: bytes) -> bytes:
            raise Unreachable(url)

        def crl(self, url: str) -> bytes:
            raise Unreachable(url)

    signer = KeySigner()
    signed = sealwax.sign(text_page(), signer, field="Signature1")
    with pytest.raises(sealwax.ValidationError, match="no signature"):
        sealwax.add_validation_data(signed.pdf, "Other", fetcher=Nowhere(), roots=[signer.certificate()])
    # The signer's own certificate is the anchor: nothing to fetch, the store holds it alone.
    pdf, coverage = sealwax.add_validation_data(
        signed.pdf, "Signature1", fetcher=Nowhere(), roots=[signer.certificate()]
    )
    assert pdf.startswith(signed.pdf)
    assert coverage["ocsp_responses"] == 0 and coverage["crls"] == 0


def test_the_network_clients_refuse_what_they_should():
    from sealwax.net import FetchError, HttpRevocationFetcher, HttpTimestampClient

    with pytest.raises(FetchError, match="http"):
        HttpRevocationFetcher().crl("file:///etc/passwd")
    with pytest.raises(FetchError, match="not allowed"):
        HttpRevocationFetcher(allowed_hosts=["ocsp.example"]).ocsp("http://169.254.169.254/", b"")
    with pytest.raises(FetchError, match="unreachable"):
        HttpTimestampClient("http://127.0.0.1:9/", timeout=1).timestamp(b"0")


def test_each_algorithm_gets_its_digest():
    """The signer gets the digest of its algorithm: 32, 48 or 64 bytes; an unknown name is
    refused."""

    class PssSigner(KeySigner):
        algorithm = "PS384"

        def sign_digest(self, digest: bytes) -> bytes:
            self.digests.append(digest)
            pss = padding.PSS(mgf=padding.MGF1(hashes.SHA384()), salt_length=48)
            return self.key.sign(digest, pss, Prehashed(hashes.SHA384()))

    class Rs512Signer(KeySigner):
        algorithm = "rsa-pkcs1v15-sha512"

        def sign_digest(self, digest: bytes) -> bytes:
            self.digests.append(digest)
            return self.key.sign(digest, padding.PKCS1v15(), Prehashed(hashes.SHA512()))

    for signer, length in [(PssSigner(), 48), (Rs512Signer(), 64)]:
        signed = sealwax.sign(text_page(), signer, field="S")
        assert signed.pdf.startswith(text_page())
        assert [len(digest) for digest in signer.digests] == [length]

    class Unknown(KeySigner):
        algorithm = "HS256"

    with pytest.raises(ValueError, match="PS256"):
        sealwax.sign(text_page(), Unknown(), field="S")


def test_signatures_are_verified_and_a_broken_one_refuses_the_next():
    signer = KeySigner()
    certified = sealwax.sign(text_page(), signer, field="Certification", certify=True).pdf
    approved = sealwax.sign(certified, signer, field="Approval_1").pdf
    checks = sealwax.verify_signatures(approved)
    assert [(c["field"], c["kind"], c["integrity"], c["later_revisions"]) for c in checks] == [
        ("Certification", "certification", "intact", 1),
        ("Approval_1", "approval", "intact", 0),
    ]
    assert all(c["signer"] and c["sub_filter"] == "ETSI.CAdES.detached" for c in checks)
    changed = bytearray(approved)
    changed[approved.index(b"%") + 10] ^= 1
    checks = sealwax.verify_signatures(bytes(changed))
    assert {c["integrity"] for c in checks} == {"broken"}
    assert checks[0]["detail"] == "the signed bytes were changed"
    with pytest.raises(sealwax.SignatureError, match="not intact"):
        sealwax.sign(bytes(changed), signer, field="Approval_2")
    sealwax.sign(bytes(changed), signer, field="Approval_2", allow_broken_signatures=True)


def test_a_document_timestamp_that_fails_is_a_timestamp_error():
    signer = KeySigner()
    signed = sealwax.sign(text_page(), signer, field="Certification", certify=True).pdf

    def down(request: bytes) -> bytes:
        raise Unreachable("TSA down")

    timestamping = sealwax.Timestamping(down, roots=[signer.certificate()])
    with pytest.raises(sealwax.TimestampError) as raised:
        sealwax.add_document_timestamp(signed, timestamping)
    assert isinstance(raised.value.__cause__, Unreachable)


def test_signing_limits_are_checked_before_external_calls():
    data = text_page()
    signer = KeySigner()
    limits = sealwax.Limits(max_shown_bytes=1)

    class Unused:
        def __getattr__(self, name):
            pytest.fail(f"unexpected external lookup {name}")

    stamping = sealwax.Timestamping(Unused(), roots=[signer.certificate()])
    operations = (
        lambda: sealwax.sign(data, Unused(), field="Approval", limits=limits),
        lambda: sealwax.verify_signatures(data, limits=limits),
        lambda: sealwax.add_document_timestamp(data, stamping, limits=limits),
        lambda: sealwax.add_validation_data(data, "Approval", fetcher=Unused(), roots=[], limits=limits),
    )
    for call in operations:
        with pytest.raises(sealwax.PdfLimitError) as raised:
            call()
        assert raised.value.limit == "max_shown_bytes"


def test_hsm_wait_is_excluded_from_the_signing_timeout():
    import time

    class SlowSigner(KeySigner):
        def sign_digest(self, digest):
            time.sleep(0.4)
            return super().sign_digest(digest)

    signed = sealwax.sign(text_page(), SlowSigner(), field="Approval", limits=sealwax.Limits(timeout=0.25))
    assert sealwax.verify_signatures(signed.pdf, limits=sealwax.Limits(timeout=3))[0]["integrity"] == "intact"
