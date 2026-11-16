#![no_main]
//! Whatever the bytes: rendering a page to PNG and a document to grayscale (at a low resolution,
//! to keep runs fast) never panics; a PNG rendered is one, and a grayscale output opens again
//! with as many pages as the input, each the size of one gray image drawn over it.
//!
//!     cargo +nightly fuzz run raster fuzz/corpus/raster corpus/public -- -max_len=1048576

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, Object, OpenOptions};

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions::default().max_file_bytes(4 << 20);
    let Ok(input) = Document::open(data.to_vec(), &options) else {
        return;
    };
    let pages = input.page_count();
    if pages > 0
        && let Ok(png) = sealwax_raster::render_png(&input, 0, 0.1)
    {
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "a PNG");
    }
    let Ok(gray) = sealwax_raster::grayscale(data, 6.0, &options) else {
        return;
    };
    let output = Document::open(gray, &OpenOptions::default())
        .unwrap_or_else(|error| panic!("grayscale output does not reopen: {error}"));
    assert_eq!(output.page_count(), pages);
    for id in output.page_ids() {
        let id = id.expect("each output page is an object");
        let Ok(Some(Object::Dict(page))) = output.object(id) else {
            panic!("each output page is a dictionary");
        };
        let images = page
            .get(b"Resources")
            .and_then(Object::as_dict)
            .and_then(|resources| resources.get(b"XObject"))
            .and_then(Object::as_dict)
            .map_or(0, |xobjects| xobjects.len());
        assert_eq!(images, 1, "one image per output page");
    }
});
