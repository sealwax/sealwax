#![no_main]
//! Whatever the bytes a timestamp authority sends back: checking them never panics, and with no
//! anchor trusted no response, no token, is ever accepted.
//!
//!     cargo +nightly fuzz run timestamp fuzz/corpus/timestamp -- -max_len=65536
//!
//! Seeds: responses of a test authority (pyHanko's `DummyTimeStamper`, as
//! `benchmark/judges/sign.py tsa` runs it) to `TimestampRequest::with_nonce(b"value", 1)`, so
//! that mutations reach the signature and the path checks. Never a client's document.

use libfuzzer_sys::fuzz_target;
use sealwax_sign::{TimestampRequest, Trust, check_timestamp_token};

fuzz_target!(|data: &[u8]| {
    let trust = Trust::default();
    let request = TimestampRequest::with_nonce(b"value", 1).expect("a request encodes");
    assert!(request.check_response(data, &trust).is_err());
    assert!(check_timestamp_token(data, b"value", &trust).is_err());
});
