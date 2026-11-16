#![no_main]
//! Whatever the text: reading an XFDF never panics, and what it refuses it refuses everywhere.
//!
//!     cargo +nightly fuzz run xfdf fuzz/corpus/xfdf -- -max_len=65536

use libfuzzer_sys::fuzz_target;
use sealwax_annots::xfdf;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let command = xfdf::read_command(text);
    let plain = xfdf::check_plain(text);
    let count = xfdf::added_annotation_count(text);
    let _ = xfdf::placeholder(text);
    // An XFDF read as a command is plain XML, and holds as many annotations as it adds.
    if let Ok(command) = command {
        assert!(plain.is_ok());
        assert_eq!(count.ok(), Some(command.marks.len()));
    }
});
