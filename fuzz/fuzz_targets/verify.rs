#![no_main]
//! Whatever the bytes of a document and of its signatures: checking their integrity never panics,
//! and a check is made of every signature the document's form holds.
//!
//!     cargo +nightly fuzz run verify fuzz/corpus/verify -- -max_len=65536
//!
//! Seeds: documents signed by the tests' signer (`sealwax-sign/tests`), so that mutations reach
//! the byte ranges, the CMS and the timestamps. Never a client's document.

use libfuzzer_sys::fuzz_target;
use sealwax_sign::verify_signatures;

fuzz_target!(|data: &[u8]| {
    let _ = verify_signatures(data);
});
