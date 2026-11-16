#![no_main]
//! Whatever the bytes a responder or a CRL server sends back: checking them never panics.
//!
//!     cargo +nightly fuzz run revocation fuzz/corpus/revocation -- -max_len=65536
//!
//! An input is a certificate, its issuer's, then an OCSP response or a CRL about it. Seeds: those
//! of a test PKI (`benchmark/judges/sign.py`'s, as `make_pki` and `fetch` make them), so that
//! mutations reach the signature checks. Never a client's document.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    sealwax_sign::fuzz_revocation(data);
});
