"""pyHanko as the judge of a full save as a base for signatures: on each output, a certification
(DocMDP P=3, invisible field) then an approval signature are appended, and both must
validate: intact, valid, DocMDP respected.

    uv run python -I sign.py run DIR... --out JUDGE.jsonl [--signed-dir DIR]
    uv run python -I sign.py validate DIR... --cert CERT.pem --out JUDGE.jsonl
    uv run python -I sign.py sealwax DIR... --sealwax BIN --out JUDGE.jsonl [--signed-dir DIR] [--timestamp|--ltv|--lta]
    uv run python -I sign.py report JUDGE.jsonl     # Markdown on stdout

`sealwax` judges sealwax's own signatures: on each file, `sealwax sign` appends the certification
then the approval, its signature values made here with an RSA key (PKCS #1 v1.5 over the digest
sealwax gives, as an HSM makes them), and pyHanko validates both as above. With
`--timestamp`, each signature value is timestamped by a test authority of the judge (pyHanko's
`DummyTimeStamper`, its own root, a certificate for timestamping only) that sealwax reaches
through its TSA command and must trust; pyHanko then also requires each timestamp intact, valid
and trusted, at the time sealwax reported. With `--ltv` (timestamps included), the judge's PKI
is a chain (root, CA, signer) with an OCSP responder the CA delegated (`ocsp-nocheck`), the root
answering for the CA, and the authority's root publishing a CRL for it; after each signature
`sealwax ltv` adds the validation data, fetched through the judge's fetch command, and pyHanko
then validates each signature and its timestamp again from the document security store alone:
no fetching, revocation information required (`hard-fail`). With `--lta` (`--ltv` included),
`sealwax timestamp` then adds a document timestamp and `sealwax ltv` its authority's validation
data; pyHanko validates each signature as PAdES B-LTA, offline, and the document timestamp too.

`validate` judges files signed by `run` (its certificate is kept as `judge-cert.pem` in the
signed directory) after another tool appended a revision, for instance `sealwax save --mode
incremental`: every signature must stay intact, valid, and within its DocMDP permissions.

The certificate is self-signed, generated for the run and trusted explicitly: what is judged is
the file structure, not the trust chain. pyHanko reads strictly (`strict=True`), so a base it has
to repair counts as a failure. Statuses: `ok`, `sign_failed`, `invalid`, `timeout`, `crash`.
"""

from __future__ import annotations

import argparse
import datetime
import json
import subprocess
import sys
import tempfile
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

TIMEOUT_SECONDS = 120


JUDGE_URL = "http://judge.test"


def make_authority(directory: Path, crl: bool = False) -> tuple[Path, Path, Path]:
    """A test timestamp authority: its root, its certificate (timestamping only) and key; with
    `crl`, the root publishes a CRL for it (`tsa-root-key.pem` kept for the fetch command)."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

    now = datetime.datetime.now(datetime.UTC)
    root_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    root_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "sealwax judge TSA root")])
    root = (
        x509.CertificateBuilder()
        .subject_name(root_name)
        .issuer_name(root_name)
        .public_key(root_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        # A root outlives what it certifies, as real ones do (B-LTA is judged decades on).
        .not_valid_after(now + datetime.timedelta(days=30 * 365))
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .sign(root_key, hashes.SHA256())
    )
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    builder = (
        x509.CertificateBuilder()
        .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "sealwax judge TSA")]))
        .issuer_name(root_name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=7))
        .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.TIME_STAMPING]), critical=True)
    )
    if crl:
        point = x509.DistributionPoint([x509.UniformResourceIdentifier(f"{JUDGE_URL}/crl/tsa-root")], None, None, None)
        builder = builder.add_extension(x509.CRLDistributionPoints([point]), critical=False)
        (directory / "tsa-root-key.pem").write_bytes(_pem_key(root_key))
    certificate = builder.sign(root_key, hashes.SHA256())
    root_file, cert_file, key_file = directory / "tsa-root.pem", directory / "tsa.pem", directory / "tsa-key.pem"
    root_file.write_bytes(root.public_bytes(serialization.Encoding.PEM))
    cert_file.write_bytes(certificate.public_bytes(serialization.Encoding.PEM))
    key_file.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()
        )
    )
    return root_file, cert_file, key_file


def _pem_key(key) -> bytes:
    from cryptography.hazmat.primitives import serialization

    return key.private_bytes(
        serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()
    )


def make_pki(directory: Path) -> tuple[Path, Path]:
    """The signer's chain for `--ltv`: root (`root.pem`), CA (`ca.pem`, the root answering OCSP
    for it), signer (`cert.pem`, `key.pem`, a responder the CA delegated answering for it); and
    the timestamp authority, its root publishing a CRL for it. The keys stay in `directory` for
    the fetch command. Returns the signer's key and certificate."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import AuthorityInformationAccessOID, ExtendedKeyUsageOID, NameOID

    now = datetime.datetime.now(datetime.UTC)

    def name(text):
        return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, text)])

    def ocsp(url):
        return x509.AuthorityInformationAccess(
            [x509.AccessDescription(AuthorityInformationAccessOID.OCSP, x509.UniformResourceIdentifier(url))]
        )

    def issue(subject, key, issuer, issuer_key, *extensions, days=7):
        builder = (
            x509.CertificateBuilder()
            .subject_name(subject)
            .issuer_name(issuer)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(days=1))
            .not_valid_after(now + datetime.timedelta(days=days))
        )
        for extension, critical in extensions:
            builder = builder.add_extension(extension, critical=critical)
        return builder.sign(issuer_key, hashes.SHA256())

    keys = {n: rsa.generate_private_key(public_exponent=65537, key_size=2048)
            for n in ("root", "root-responder", "ca", "responder", "signer")}
    ca_constraints = (x509.BasicConstraints(ca=True, path_length=None), True)
    # Authorities outlive what they certify, as real ones do (B-LTA is judged decades on).
    root = issue(name("sealwax judge root"), keys["root"], name("sealwax judge root"), keys["root"], ca_constraints,
                 days=30 * 365)
    ca = issue(name("sealwax judge CA"), keys["ca"], root.subject, keys["root"], ca_constraints,
               (ocsp(f"{JUDGE_URL}/ocsp/root"), False), days=30 * 365)
    responder = issue(name("sealwax judge OCSP"), keys["responder"], ca.subject, keys["ca"],
                      (x509.ExtendedKeyUsage([ExtendedKeyUsageOID.OCSP_SIGNING]), True),
                      (x509.OCSPNoCheck(), False))
    # The root answers through a responder of its own, as real roots do.
    root_responder = issue(name("sealwax judge root OCSP"), keys["root-responder"], root.subject, keys["root"],
                           (x509.ExtendedKeyUsage([ExtendedKeyUsageOID.OCSP_SIGNING]), True),
                           (x509.OCSPNoCheck(), False))
    usage = x509.KeyUsage(digital_signature=True, content_commitment=True, key_encipherment=False,
                          data_encipherment=False, key_agreement=False, key_cert_sign=False, crl_sign=False,
                          encipher_only=False, decipher_only=False)
    signer = issue(name("sealwax judge signer"), keys["signer"], ca.subject, keys["ca"], (usage, True),
                   (ocsp(f"{JUDGE_URL}/ocsp/ca"), False))
    for label, certificate in (("root", root), ("root-responder", root_responder), ("ca", ca),
                               ("responder", responder), ("cert", signer)):
        (directory / f"{label}.pem").write_bytes(certificate.public_bytes(serialization.Encoding.PEM))
    for label in ("root", "root-responder", "ca", "responder"):
        (directory / f"{label}-key.pem").write_bytes(_pem_key(keys[label]))
    (directory / "key.pem").write_bytes(_pem_key(keys["signer"]))
    make_authority(directory, crl=True)
    return directory / "key.pem", directory / "cert.pem"


def fetch(directory: str, kind: str, url: str) -> None:
    """The fetch command of `sealwax ltv`: an OCSP response to the request on standard input,
    or a CRL, made by the judge's PKI for the address asked."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.x509 import ocsp

    def certificate(label):
        return x509.load_pem_x509_certificate(Path(directory, f"{label}.pem").read_bytes())

    def key(label):
        return serialization.load_pem_private_key(Path(directory, f"{label}-key.pem").read_bytes(), None)

    from urllib.parse import urlparse

    now = datetime.datetime.now(datetime.UTC).replace(microsecond=0)
    path = urlparse(url).path
    if kind == "crl" and path == "/crl/tsa-root":
        crl = (
            x509.CertificateRevocationListBuilder()
            .issuer_name(certificate("tsa-root").subject)
            .last_update(now - datetime.timedelta(minutes=1))
            .next_update(now + datetime.timedelta(days=1))
            .sign(key("tsa-root"), hashes.SHA256())
        )
        sys.stdout.buffer.write(crl.public_bytes(serialization.Encoding.DER))
        return
    # The CA's certificates answered by its delegated responder; the CA's own, by the root.
    answers = {"/ocsp/ca": ("ca", ["cert"], "responder"), "/ocsp/root": ("root", ["ca"], "root-responder")}
    if kind != "ocsp" or path not in answers:
        sys.exit(f"unknown {kind} address {url}")
    issuer_label, subjects, responder_label = answers[path]
    request = ocsp.load_der_ocsp_request(sys.stdin.buffer.read())
    target = next(c for c in map(certificate, subjects) if c.serial_number == request.serial_number)
    responder = certificate(responder_label)
    builder = (
        ocsp.OCSPResponseBuilder()
        .add_response(cert=target, issuer=certificate(issuer_label), algorithm=hashes.SHA1(),
                      cert_status=ocsp.OCSPCertStatus.GOOD, this_update=now - datetime.timedelta(minutes=1),
                      next_update=now + datetime.timedelta(days=1), revocation_time=None, revocation_reason=None)
        .responder_id(ocsp.OCSPResponderEncoding.HASH, responder)
    )
    if responder_label != issuer_label:
        builder = builder.certificates([responder])
    response = builder.sign(key(responder_label), hashes.SHA256())
    sys.stdout.buffer.write(response.public_bytes(serialization.Encoding.DER))


def tsa(directory: str) -> None:
    """The TSA command of `sealwax sign`: a TimeStampReq (DER) in, a TimeStampResp (DER) out."""
    from asn1crypto import tsp
    from pyhanko.keys import load_cert_from_pemder, load_private_key_from_pemder
    from pyhanko.sign.timestamps.dummy_client import DummyTimeStamper

    stamper = DummyTimeStamper(
        tsa_cert=load_cert_from_pemder(str(Path(directory, "tsa.pem"))),
        tsa_key=load_private_key_from_pemder(str(Path(directory, "tsa-key.pem")), None),
    )
    request = tsp.TimeStampReq.load(sys.stdin.buffer.read())
    sys.stdout.buffer.write(stamper.request_tsa_response(request).dump())


def make_certificate(directory: Path) -> tuple[Path, Path]:
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "sealwax judge")])
    now = datetime.datetime.now(datetime.UTC)
    certificate = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=7))
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=True,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=True,
                crl_sign=False,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .sign(key, hashes.SHA256())
    )
    key_file, cert_file = directory / "key.pem", directory / "cert.pem"
    key_file.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()
        )
    )
    cert_file.write_bytes(certificate.public_bytes(serialization.Encoding.PEM))
    return key_file, cert_file


def judge(path: str, key_file: str, cert_file: str, signed_dir: str) -> dict:
    from io import BytesIO

    from pyhanko.pdf_utils.incremental_writer import IncrementalPdfFileWriter
    from pyhanko.sign import fields, signers

    signer = signers.SimpleSigner.load(key_file, cert_file)
    data = Path(path).read_bytes()
    try:
        for name, certify in (("Certification", True), ("Approval_1", False)):
            meta = signers.PdfSignatureMetadata(
                field_name=name,
                certify=certify,
                docmdp_permissions=fields.MDPPerm.ANNOTATE,
            )
            writer = IncrementalPdfFileWriter(BytesIO(data), strict=True)
            data = signers.sign_pdf(
                writer, meta, signer=signer, new_field_spec=fields.SigFieldSpec(name)
            ).getvalue()
    except Exception as error:  # noqa: BLE001 - the judge records what pyHanko says
        return {"status": "sign_failed", "detail": f"{type(error).__name__}: {error}"[:200]}
    if signed_dir:
        Path(signed_dir, Path(path).name).write_bytes(data)

    return validate(data, cert_file)


def lta_problems(signature, cert_file: str, tsa_root: str) -> list[str]:
    """PAdES B-LTA validation of `signature` (ETSI EN 319 102-1, 5.6.3), offline, revocation
    information required: now, and simulated twenty years on, when every certificate has expired
    and only the document timestamps and the store prove the signature."""
    import asyncio
    from datetime import datetime, timedelta, timezone

    from pyhanko.keys import load_cert_from_pemder
    from pyhanko.sign.ades.report import AdESStatus
    from pyhanko.sign.validation.ades import ades_lta_validation, simulate_future_ades_lta_validation
    from pyhanko.sign.validation.policy_decl import (
        PdfSignatureValidationSpec,
        RevinfoOnlineFetchingRule,
        RevocationInfoGatheringSpec,
        SignatureValidationSpec,
    )
    from pyhanko_certvalidator.context import CertValidationPolicySpec
    from pyhanko_certvalidator.policy_decl import CertRevTrustPolicy, RevocationCheckingPolicy, RevocationCheckingRule
    from pyhanko_certvalidator.registry import SimpleTrustManager

    required = RevocationCheckingRule.CRL_OR_OCSP_REQUIRED
    policy = CertValidationPolicySpec(
        trust_manager=SimpleTrustManager.build(
            trust_roots=[load_cert_from_pemder(cert_file), load_cert_from_pemder(tsa_root)]),
        revinfo_policy=CertRevTrustPolicy(RevocationCheckingPolicy(required, required)),
    )
    spec = PdfSignatureValidationSpec(SignatureValidationSpec(
        cert_validation_policy=policy,
        revinfo_gathering_policy=RevocationInfoGatheringSpec(
            online_fetching_rule=RevinfoOnlineFetchingRule.LOCAL_ONLY),
    ))
    problems = []
    now = asyncio.run(ades_lta_validation(signature, spec))
    if now.ades_subindic.status != AdESStatus.PASSED:
        problems.append(f"{signature.field_name}: B-LTA now {now.ades_subindic}")
    later = datetime.now(timezone.utc) + timedelta(days=20 * 365)
    future = asyncio.run(simulate_future_ades_lta_validation(signature, spec, later))
    if future.ades_subindic.status != AdESStatus.PASSED:
        problems.append(f"{signature.field_name}: B-LTA in 20 years {future.ades_subindic}")
    return problems


def validate(data: bytes, cert_file: str, tsa_root: str = "", times: dict | None = None, ltv: bool = False,
             lta: bool = False) -> dict:
    """Both signatures of the judge must be intact, valid, and within DocMDP; with `tsa_root`,
    timestamped by an authority it roots, at the times `times` gives per field; with `ltv`,
    valid again from the document security store alone, revocation information required."""
    from io import BytesIO

    from pyhanko.sign.validation.dss import DocumentSecurityStore

    from pyhanko.keys import load_cert_from_pemder
    from pyhanko.pdf_utils.reader import PdfFileReader
    from pyhanko.sign.validation import validate_pdf_signature
    from pyhanko_certvalidator import ValidationContext

    context = ValidationContext(trust_roots=[load_cert_from_pemder(cert_file)])
    ts_context = ValidationContext(trust_roots=[load_cert_from_pemder(tsa_root)]) if tsa_root else None
    try:
        reader = PdfFileReader(BytesIO(data), strict=True)
        ours = [s for s in reader.embedded_signatures if s.field_name in ("Certification", "Approval_1")]
        problems, levels = [], []
        for signature in ours:
            status = validate_pdf_signature(signature, context, ts_validation_context=ts_context)
            levels.append(f"{signature.field_name}: {status.modification_level}")
            if not (status.intact and status.valid and status.docmdp_ok is not False):
                problems.append(
                    f"{signature.field_name}: intact={status.intact} valid={status.valid} "
                    f"docmdp_ok={status.docmdp_ok} level={status.modification_level}"
                )
            if tsa_root:
                stamp = status.timestamp_validity
                if stamp is None or not (stamp.intact and stamp.valid and stamp.trusted):
                    problems.append(f"{signature.field_name}: timestamp {stamp and stamp.summary()}")
                elif times and int(stamp.timestamp.timestamp()) != times.get(signature.field_name):
                    problems.append(f"{signature.field_name}: timestamp at {stamp.timestamp}, "
                                    f"sealwax said {times.get(signature.field_name)}")
            if ltv:
                store = DocumentSecurityStore.read_dss(reader)
                offline = {"allow_fetching": False, "revocation_mode": "hard-fail"}
                status = validate_pdf_signature(
                    signature,
                    store.as_validation_context(offline | {"trust_roots": [load_cert_from_pemder(cert_file)]}),
                    ts_validation_context=store.as_validation_context(
                        offline | {"trust_roots": [load_cert_from_pemder(tsa_root)]}),
                )
                stamp = status.timestamp_validity
                if not (status.bottom_line and stamp is not None and stamp.trusted):
                    problems.append(f"{signature.field_name}: offline {status.summary()}, "
                                    f"timestamp {stamp and stamp.summary()}")
            if lta:
                problems.extend(lta_problems(signature, cert_file, tsa_root))
        if lta:
            from pyhanko.sign.validation import validate_pdf_timestamp

            stamps = reader.embedded_timestamp_signatures
            if len(stamps) != 1:
                problems.append(f"{len(stamps)} document timestamps of 1")
            for stamp in stamps:
                status = validate_pdf_timestamp(
                    stamp, ValidationContext(trust_roots=[load_cert_from_pemder(tsa_root)]))
                if not (status.intact and status.valid and status.trusted):
                    problems.append(f"document timestamp: {status.summary()}")
    except Exception as error:  # noqa: BLE001 - the judge records what pyHanko says
        return {"status": "invalid", "detail": f"{type(error).__name__}: {error}"[:200]}
    if len(ours) != 2:
        problems.append(f"{len(ours)} of 2 signatures found")
    if problems:
        return {"status": "invalid", "detail": problems}
    return {"status": "ok", "levels": levels}


def run(args: argparse.Namespace) -> None:
    files = sorted({p for root in args.paths for p in Path(root).rglob("*.pdf")})
    if args.signed_dir:
        Path(args.signed_dir).mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as directory:
        key_file, cert_file = make_certificate(Path(directory))
        if args.signed_dir:
            Path(args.signed_dir, "judge-cert.pem").write_bytes(cert_file.read_bytes())

        def one(path: Path) -> str:
            command = [sys.executable, "-I", __file__, "judge", str(path), str(key_file), str(cert_file), args.signed_dir or ""]
            try:
                done = subprocess.run(command, capture_output=True, text=True, timeout=TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                return json.dumps({"file": path.name, "status": "timeout"})
            lines = done.stdout.strip().splitlines()
            if done.returncode == 0 and lines:
                return json.dumps({"file": path.name} | json.loads(lines[-1]))
            return json.dumps({"file": path.name, "status": "crash", "code": done.returncode, "stderr": done.stderr[-300:]})

        with ThreadPoolExecutor(args.jobs) as pool, open(args.out, "w") as sink:
            for line in pool.map(one, files):
                sink.write(line + "\n")


def rsa_sign(key_file: str) -> None:
    """The signer command of `sealwax sign`: a digest in hexadecimal in, a signature out."""
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import padding, utils

    key = serialization.load_pem_private_key(Path(key_file).read_bytes(), password=None)
    digest = bytes.fromhex(sys.stdin.read().strip())
    signature = key.sign(digest, padding.PKCS1v15(), utils.Prehashed(hashes.SHA256()))
    print(signature.hex())


def sealwax_judge(path: str, key_file: str, cert_file: str, signed_dir: str, sealwax: str, tsa_dir: str,
              pki_dir: str = "", lta: str = "") -> dict:
    signer = [sys.executable, "-I", __file__, "rsa-sign", key_file]
    chain = ["--chain", str(Path(pki_dir, "ca.pem"))] if pki_dir else []
    timestamp = []
    if tsa_dir:
        timestamp = ["--tsa", sys.executable, "--tsa-arg", "-I", "--tsa-arg", __file__, "--tsa-arg", "tsa",
                     "--tsa-arg", tsa_dir, "--tsa-root", str(Path(tsa_dir, "tsa-root.pem"))]
    times = {}
    coverage = {}
    with tempfile.TemporaryDirectory() as directory:
        current = Path(path)
        for name, certify in (("Certification", True), ("Approval_1", False)):
            target = Path(directory, f"{name}.pdf")
            command = [sealwax, "sign", str(current), "--out", str(target), "--cert", cert_file,
                       "--field", name, "--reason", "Document approval", "--location", "sealwax judge"]
            if certify:
                command.append("--certify")
            done = subprocess.run(command + chain + timestamp + ["--", *signer], capture_output=True, text=True)
            if done.returncode != 0:
                return {"status": "sign_failed", "detail": done.stderr.strip()[-200:]}
            if tsa_dir:
                times[name] = json.loads(done.stdout)["timestamp"]["time"]
            current = target
            if pki_dir:
                target = Path(directory, f"{name}-ltv.pdf")
                done = subprocess.run(
                    [sealwax, "ltv", str(current), "--out", str(target), "--field", name,
                     "--root", str(Path(pki_dir, "root.pem")), "--root", str(Path(pki_dir, "tsa-root.pem")),
                     "--fetch", sys.executable, "--fetch-arg", "-I", "--fetch-arg", __file__,
                     "--fetch-arg", "fetch", "--fetch-arg", pki_dir],
                    capture_output=True, text=True)
                if done.returncode != 0:
                    return {"status": "ltv_failed", "detail": done.stderr.strip()[-300:]}
                coverage[name] = json.loads(done.stdout)
                current = target
        if lta:
            target = Path(directory, "timestamped.pdf")
            done = subprocess.run(
                [sealwax, "timestamp", str(current), "--out", str(target), "--field", "DocTimeStamp1",
                 "--tsa", sys.executable, "--tsa-arg", "-I", "--tsa-arg", __file__, "--tsa-arg", "tsa",
                 "--tsa-arg", tsa_dir, "--tsa-root", str(Path(tsa_dir, "tsa-root.pem"))],
                capture_output=True, text=True)
            if done.returncode != 0:
                return {"status": "timestamp_failed", "detail": done.stderr.strip()[-300:]}
            current = target
            target = Path(directory, "timestamped-ltv.pdf")
            done = subprocess.run(
                [sealwax, "ltv", str(current), "--out", str(target), "--field", "DocTimeStamp1",
                 "--root", str(Path(pki_dir, "tsa-root.pem")),
                 "--fetch", sys.executable, "--fetch-arg", "-I", "--fetch-arg", __file__,
                 "--fetch-arg", "fetch", "--fetch-arg", pki_dir],
                capture_output=True, text=True)
            if done.returncode != 0:
                return {"status": "ltv_failed", "detail": done.stderr.strip()[-300:]}
            coverage["DocTimeStamp1"] = json.loads(done.stdout)
            current = target
        data = current.read_bytes()
    if signed_dir:
        Path(signed_dir, Path(path).name).write_bytes(data)
    tsa_root = str(Path(tsa_dir, "tsa-root.pem")) if tsa_dir else ""
    if pki_dir:
        verdict = validate(data, str(Path(pki_dir, "root.pem")), tsa_root, times, ltv=True, lta=bool(lta))
        return verdict | {"coverage": coverage}
    return validate(data, cert_file, tsa_root, times)


def run_sealwax(args: argparse.Namespace) -> None:
    files = sorted({p for root in args.paths for p in Path(root).rglob("*.pdf")})
    if args.signed_dir:
        Path(args.signed_dir).mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as directory:
        tsa_dir = pki_dir = ""
        if args.ltv or args.lta:
            key_file, cert_file = make_pki(Path(directory))
            tsa_dir = pki_dir = directory
        else:
            key_file, cert_file = make_certificate(Path(directory))
        if args.timestamp and not args.ltv:
            make_authority(Path(directory))
            tsa_dir = directory
        if args.signed_dir:
            Path(args.signed_dir, "judge-cert.pem").write_bytes(cert_file.read_bytes())
            if tsa_dir:
                Path(args.signed_dir, "judge-tsa-root.pem").write_bytes(Path(tsa_dir, "tsa-root.pem").read_bytes())

        def one(path: Path) -> str:
            command = [sys.executable, "-I", __file__, "sealwax-judge", str(path), str(key_file), str(cert_file),
                       args.signed_dir or "", args.sealwax, tsa_dir, pki_dir, "lta" if args.lta else ""]
            try:
                done = subprocess.run(command, capture_output=True, text=True, timeout=TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                return json.dumps({"file": path.name, "status": "timeout"})
            lines = done.stdout.strip().splitlines()
            if done.returncode == 0 and lines:
                return json.dumps({"file": path.name} | json.loads(lines[-1]))
            return json.dumps({"file": path.name, "status": "crash", "code": done.returncode, "stderr": done.stderr[-300:]})

        with ThreadPoolExecutor(args.jobs) as pool, open(args.out, "w") as sink:
            for line in pool.map(one, files):
                sink.write(line + "\n")


def run_validate(args: argparse.Namespace) -> None:
    files = sorted({p for root in args.paths for p in Path(root).rglob("*.pdf")})

    def one(path: Path) -> str:
        command = [sys.executable, "-I", __file__, "validate-one", str(path), args.cert]
        try:
            done = subprocess.run(command, capture_output=True, text=True, timeout=TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            return json.dumps({"file": path.name, "status": "timeout"})
        lines = done.stdout.strip().splitlines()
        if done.returncode == 0 and lines:
            return json.dumps({"file": path.name} | json.loads(lines[-1]))
        return json.dumps({"file": path.name, "status": "crash", "code": done.returncode, "stderr": done.stderr[-300:]})

    with ThreadPoolExecutor(args.jobs) as pool, open(args.out, "w") as sink:
        for line in pool.map(one, files):
            sink.write(line + "\n")


def report(path: str) -> None:
    rows = [json.loads(line) for line in Path(path).read_text().splitlines()]
    print("| Status | Documents |\n|---|---|")
    for status, count in Counter(row["status"] for row in rows).most_common():
        print(f"| {status} | {count} |")
    print()
    for row in rows:
        if row["status"] != "ok":
            print(f"- `{row['file'][:16]}` {row['status']}: {row.get('detail') or row.get('stderr')}")


def main() -> None:
    if sys.argv[1:2] == ["judge"]:
        print(json.dumps(judge(*sys.argv[2:6])))
        return
    if sys.argv[1:2] == ["rsa-sign"]:
        rsa_sign(sys.argv[2])
        return
    if sys.argv[1:2] == ["sealwax-judge"]:
        print(json.dumps(sealwax_judge(*sys.argv[2:10])))
        return
    if sys.argv[1:2] == ["tsa"]:
        tsa(sys.argv[2])
        return
    if sys.argv[1:2] == ["fetch"]:
        fetch(*sys.argv[2:5])
        return
    if sys.argv[1:2] == ["validate-one"]:
        print(json.dumps(validate(Path(sys.argv[2]).read_bytes(), sys.argv[3])))
        return
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    run_parser = commands.add_parser("run")
    run_parser.add_argument("paths", nargs="+")
    run_parser.add_argument("--out", required=True)
    run_parser.add_argument("--signed-dir", default="")
    run_parser.add_argument("--jobs", type=int, default=8)
    validate_parser = commands.add_parser("validate")
    validate_parser.add_argument("paths", nargs="+")
    validate_parser.add_argument("--cert", required=True)
    validate_parser.add_argument("--out", required=True)
    validate_parser.add_argument("--jobs", type=int, default=8)
    sealwax_parser = commands.add_parser("sealwax")
    sealwax_parser.add_argument("paths", nargs="+")
    sealwax_parser.add_argument("--sealwax", required=True)
    sealwax_parser.add_argument("--out", required=True)
    sealwax_parser.add_argument("--signed-dir", default="")
    sealwax_parser.add_argument("--timestamp", action="store_true")
    sealwax_parser.add_argument("--ltv", action="store_true")
    sealwax_parser.add_argument("--lta", action="store_true")
    sealwax_parser.add_argument("--jobs", type=int, default=8)
    report_parser = commands.add_parser("report")
    report_parser.add_argument("results")
    args = parser.parse_args()
    if args.command == "run":
        run(args)
    elif args.command == "validate":
        run_validate(args)
    elif args.command == "sealwax":
        run_sealwax(args)
    else:
        report(args.results)


if __name__ == "__main__":
    main()
