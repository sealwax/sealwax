"""HTTP for signatures: the timestamp authority (RFC 3161) and the revocation sources (OCSP,
RFC 6960; CRLs). sealwax itself never reaches the network: these are what a caller hands to
`sealwax.sign` and `sealwax.add_validation_data`.

Every request has a timeout and a size bound; redirects are refused. Only http and https URLs are
fetched, and with `allowed_hosts`, only those hosts. The revocation addresses come from
certificates that chain to the roots the caller trusts: sealwax looks into no other.
"""

import urllib.error
import urllib.request
from collections.abc import Collection
from urllib.parse import urlsplit

__all__ = ["FetchError", "HttpRevocationFetcher", "HttpTimestampClient"]


class FetchError(Exception):
    """An HTTP exchange failed: refused URL, network error, status, size."""


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(
        self, req: object, fp: object, code: int, msg: str, headers: object, newurl: str
    ) -> None:
        raise FetchError(f"redirect refused ({code})")


_OPENER = urllib.request.build_opener(_NoRedirect)


def _exchange(
    url: str,
    *,
    data: bytes | None,
    content_type: str | None,
    accept: str,
    timeout: float,
    max_bytes: int,
    allowed_hosts: Collection[str] | None,
) -> bytes:
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        raise FetchError("only http and https URLs are fetched")
    if allowed_hosts is not None and parts.hostname.lower() not in {h.lower() for h in allowed_hosts}:
        raise FetchError(f"host not allowed: {parts.hostname}")
    headers = {"Accept": accept, "User-Agent": "sealwax"}
    if content_type is not None:
        headers["Content-Type"] = content_type
    request = urllib.request.Request(url, data=data, headers=headers, method="POST" if data is not None else "GET")
    try:
        with _OPENER.open(request, timeout=timeout) as response:
            body: bytes = response.read(max_bytes + 1)
    except FetchError:
        raise
    except urllib.error.HTTPError as error:
        raise FetchError(f"HTTP status {error.code}") from error
    except (urllib.error.URLError, OSError) as error:
        raise FetchError(f"unreachable: {type(error).__name__}") from error
    if len(body) > max_bytes:
        raise FetchError(f"response larger than {max_bytes} bytes")
    return body


class HttpTimestampClient:
    """A timestamp authority reached over HTTP (`application/timestamp-query`): a URL, no
    authentication."""

    def __init__(self, url: str, *, timeout: float = 10.0, max_bytes: int = 1 << 20) -> None:
        self.url = url
        self.timeout = timeout
        self.max_bytes = max_bytes

    def timestamp(self, request: bytes) -> bytes:
        return _exchange(
            self.url,
            data=request,
            content_type="application/timestamp-query",
            accept="application/timestamp-reply",
            timeout=self.timeout,
            max_bytes=self.max_bytes,
            allowed_hosts=None,
        )


class HttpRevocationFetcher:
    """OCSP responses (POST `application/ocsp-request`) and CRLs (GET) over HTTP, from the
    addresses the certificates give."""

    def __init__(
        self,
        *,
        timeout: float = 10.0,
        max_bytes: int = 16 << 20,
        allowed_hosts: Collection[str] | None = None,
    ) -> None:
        self.timeout = timeout
        self.max_bytes = max_bytes
        self.allowed_hosts = allowed_hosts

    def ocsp(self, url: str, request: bytes) -> bytes:
        return _exchange(
            url,
            data=request,
            content_type="application/ocsp-request",
            accept="application/ocsp-response",
            timeout=self.timeout,
            max_bytes=self.max_bytes,
            allowed_hosts=self.allowed_hosts,
        )

    def crl(self, url: str) -> bytes:
        return _exchange(
            url,
            data=None,
            content_type=None,
            accept="application/pkix-crl",
            timeout=self.timeout,
            max_bytes=self.max_bytes,
            allowed_hosts=self.allowed_hosts,
        )
