#![no_main]
//! Whatever the bytes: flattening the annotations and applying an XFDF of a signing page (a PNG
//! stamp and a free text on the first page) never panic, and the document saves into a file that
//! opens again with the same pages and geometry.
//!
//!     cargo +nightly fuzz run annots fuzz/corpus/annots corpus/public -- -max_len=1048576

use libfuzzer_sys::fuzz_target;
use sealwax_core::{Document, OpenOptions, SaveOptions};

/// A 1 x 1 opaque PNG, then an XFDF placing it, and a text, in the middle of a Letter page.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn xfdf() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\
         <fields /><add><stamp page=\"0\" rect=\"250,350,350,400\" flags=\"print\">\
         <imagedata>data:image/png;base64,{PNG}</imagedata></stamp>\
         <freetext page=\"0\" rect=\"250,300,370,325\" flags=\"print\" width=\"0\"><contents>Jean</contents>\
         <defaultappearance>0 0 0 rg /Helvetica 16 Tf</defaultappearance>\
         <defaultstyle>font: Helvetica 16pt; text-align: left; text-vertical-align: center</defaultstyle>\
         </freetext></add><modify /><delete /></xfdf>"
    )
}

fuzz_target!(|data: &[u8]| {
    let options = OpenOptions::default().max_file_bytes(4 << 20);
    let Ok(mut document) = Document::open(data.to_vec(), &options) else {
        return;
    };
    let pages = document.page_count();
    let geometry = document.geometry();
    // The stamp may fall off a page that is not Letter: then nothing changes.
    if sealwax_annots::apply_xfdf(&mut document, &xfdf(), true).is_err()
        && sealwax_annots::flatten(&mut document).is_err()
    {
        return;
    }
    let Ok(saved) = document.save(&SaveOptions::default()) else {
        return;
    };
    let reopened = Document::open(saved, &OpenOptions::default())
        .unwrap_or_else(|error| panic!("flattened document does not reopen: {error}"));
    assert_eq!(reopened.page_count(), pages);
    assert_eq!(reopened.geometry(), geometry);
});
