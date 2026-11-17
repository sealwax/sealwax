"""sealwax: reading, flattening, PDF/A-3 conversion and PAdES signatures, in Rust.

The work happens in the native module (`sealwax._native`) with the interpreter released; this
package adds the protocols the objects handed to it follow, HTTP clients for the timestamp
authority and the revocation sources (`sealwax.net`), and `isolated`, which runs an operation in a
process of its own.
"""

from typing import Protocol

from ._native import (
    Document,
    Limits,
    PageGeometry,
    PdfAError,
    PdfError,
    PdfLimitError,
    PdfPasswordError,
    PdfReadError,
    PdfTimeoutError,
    SignatureError,
    Signed,
    TimestampError,
    Timestamping,
    ValidationError,
    XfdfError,
    __version__,
    add_document_timestamp,
    add_validation_data,
    apply_xfdf_bytes,
    count,
    flatten_bytes,
    grayscale,
    merge_bytes,
    optimize,
    pdfa_convert,
    read_geometry,
    release_cache,
    sign,
    verify_signatures,
)
from .isolation import isolated

__all__ = [
    "Document",
    "Limits",
    "PageGeometry",
    "PdfAError",
    "PdfError",
    "PdfLimitError",
    "PdfPasswordError",
    "PdfReadError",
    "PdfTimeoutError",
    "RevocationFetcher",
    "SignatureError",
    "Signed",
    "Signer",
    "TimestampClient",
    "TimestampError",
    "Timestamping",
    "ValidationError",
    "XfdfError",
    "__version__",
    "add_document_timestamp",
    "add_validation_data",
    "apply_xfdf_bytes",
    "count",
    "flatten_bytes",
    "grayscale",
    "isolated",
    "merge_bytes",
    "optimize",
    "pdfa_convert",
    "read_geometry",
    "release_cache",
    "sign",
    "verify_signatures",
]


class Signer(Protocol):
    """What makes signature values: an HSM, a key in a cloud vault, a key in memory.

    `certificate` and `chain` may be attributes or methods. `algorithm` names how the key signs,
    as cloud key vaults name it: "RS256" (the default when absent), "RS384", "RS512" (RSA,
    PKCS #1 v1.5), "PS256", "PS384", "PS512" (RSA-PSS, MGF1 with the same digest, a salt as long
    as the digest), "ES256", "ES384", "ES512" (ECDSA). Longer names are accepted too:
    "rsa-pkcs1v15-sha256", "rsa-pss-sha384", "ecdsa-sha512"...
    """

    certificate: bytes
    """The signer's certificate, DER."""
    chain: list[bytes]
    """The rest of its chain, DER: intermediates, the root if wanted."""
    algorithm: str

    def sign_digest(self, digest: bytes) -> bytes:
        """The signature value over `digest`, the digest of the algorithm (SHA-256, SHA-384 or
        SHA-512: 32, 48 or 64 bytes). An ECDSA value may be DER or `r || s`, as key vaults give
        it. What it raises is raised by `sign` as is."""
        ...


class TimestampClient(Protocol):
    """What carries a timestamp request to the authority: `sealwax.net.HttpTimestampClient`, or
    any callable taking and returning bytes."""

    def timestamp(self, request: bytes) -> bytes:
        """The response (`TimeStampResp`, DER) to `request` (`TimeStampReq`, DER)."""
        ...


class RevocationFetcher(Protocol):
    """Where validation data comes from: `sealwax.net.HttpRevocationFetcher`."""

    def ocsp(self, url: str, request: bytes) -> bytes:
        """The responder's answer (`OCSPResponse`, DER) to `request` posted to `url`."""
        ...

    def crl(self, url: str) -> bytes:
        """The CRL (DER) published at `url`."""
        ...
