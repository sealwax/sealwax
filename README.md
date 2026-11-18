# sealwax

[![CI](https://github.com/sealwax/sealwax/actions/workflows/ci.yml/badge.svg)](https://github.com/sealwax/sealwax/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**sealwax** is a PDF library written in Rust, with Python bindings, for the documents people sign:
read them, fill and flatten their annotations, archive them as PDF/A-3, and sign them with PAdES
signatures up to B-LTA: timestamped, carrying their validation data, and kept provable by document
timestamps.

It is built for servers that process documents they did not produce. Operations hold to limits:
past a bound or a deadline they fail with an explicit error, rather than exhaust the worker.

> **Status: 0.1.** The library works and is tested; its API may still change before 1.0, and
> [the changelog](CHANGELOG.md) says how.

## What it does

| Area | Capabilities |
|---|---|
| **Documents** | Open PDF files, including damaged ones and encrypted ones (with their password; none is needed when the user password is empty; a full save writes them decrypted). Page count and page geometry (media and crop boxes, rotation, user unit). What a document says about itself (version, encryption, linearization, form and signature fields, PDF/A claim, document information), never failing. The outline, flat, with named destinations resolved. Pages taken out into a new document, links kept. Full saves, with cross-reference streams or tables, and incremental saves that keep existing signatures valid |
| **PDF/A-3** | Conversion to levels **b**, **u** and **a** (ISO 19005-3), checked with [veraPDF](https://verapdf.org): output intent, fonts embedded, every glyph mapped to Unicode, metadata. Level a tags untagged documents, with paragraphs, figures and reading order |
| **Forms and annotations** | Read the form's fields as their widgets place them (name, type, value, flags, options, button states as the document spells them), fill them (appearances drawn, button names kept to the byte) and read them back. Flatten annotations and form fields into page content, or the fields alone, drawing appearances that are missing. Apply the XFDF a web signing page exports (PNG stamps, free text). Merge documents |
| **Signatures** | PAdES baseline signatures: a certification (DocMDP), then approvals, in incremental revisions. The private key never comes to the library: an HSM or a cloud key vault signs a digest, with RSA (PKCS #1 v1.5 or PSS) or ECDSA and SHA-256, -384 or -512. RFC 3161 timestamps (B-T), long-term validation data (B-LT: OCSP responses, CRLs, DSS and VRI) and document timestamps (B-LTA), each checked before it is embedded: B-LTA keeps the validation data provable once the certificates it relies on have expired. The integrity of the signatures a document holds is checked (byte range, digest, value, timestamp, revisions since), and nothing is added over one that is not intact |
| **Text and drawings** | The glyphs a page draws, in the order of its content (forms, Type 3 fonts, and on request annotations): Unicode text, origin and advance, box and size (as MuPDF gives them), direction, font as written, invisible text (a scan's OCR layer) marked. Also what a reader reads instead: `/ActualText` from marked content or the structure tree, bold faked by drawing twice. Words, lines and reading order are left to the caller. The straight lines and rectangles a page paints (and on request its annotations), after its transformations: what tables and check boxes are drawn with |
| **Rendering** | One page to a colour PNG at any scale, as a reader shows it (annotations and field values included; JPEG, JPEG 2000, JBIG2 and CCITT images decoded). Every page to a grayscale PDF, as a fax sends them |
| **Optimization** | Lossless size reduction: unfiltered streams compressed, unreachable objects dropped |
| **Building** | A new document from nothing, as test fixtures are: pages of a given size and crop box, text in the standard fonts, lines, rectangles, links, fields, outline, information and XMP |
| **Composition** | A new document written from pages another engine laid out: glyphs placed one by one, paths, clips, gradients, pictures, links; fonts (TrueType or CFF, from a file or a collection) embedded as subsets whose text reads back |
| **Python** | Bindings for all of the above (Python 3.12+, `abi3`). The interpreter lock is released while sealwax works. An `isolated` helper runs an operation in a process of its own |

## Design

- **Untrusted input first.** `Limits` bounds what an operation may spend:
  - operators, page drawing cost and image pixels;
  - decoded stream sizes, at every stage of a filter chain, so that decompression bombs are
    refused before they expand;
  - annotations on a page, and the pictures of an XFDF;
  - pixels of a rendered page;
  - and a deadline, which every operation checks as it goes.

  Pages are checked before they are interpreted (rendered, tagged, their text counted). The bounds
  are calibrated so that real documents never reach them. They are a first line of defence, not
  a sandbox: for documents from anyone, also run each operation in a process of its own with a
  memory cap, as `sealwax.isolated` does. A synthetic bench of hostile inputs
  (`benchmark/robustness/`) and `cargo-fuzz` targets keep them honest.
- **No `unsafe`** in sealwax's crates (`unsafe_code = "forbid"` for the whole workspace). The rule
  is no panic on any input: fuzzing enforces it, and a panic in a dependency becomes an error.
- **No network.** sealwax never reaches the network by itself. Timestamp authorities and
  revocation sources are reached through clients the caller provides; the Python package ships
  HTTP clients with timeouts, size bounds and no redirects.
- **Errors never quote document content**, so logs stay free of what users upload.
- **Permissive dependencies only**, checked by `cargo-deny`. Parsing builds on
  [hayro](https://github.com/LaurenzV/hayro).

## Getting started

### Install

```sh
pip install sealwax        # or: uv add sealwax
cargo add sealwax          # the Rust library
cargo install sealwax-cli  # the `sealwax` command
```

Python wheels are built for Linux (x86_64 and aarch64, glibc 2.28 or later: Debian 10, Ubuntu
18.10, RHEL 8 and their successors) and macOS on Apple silicon, for CPython 3.12 and later
(`abi3`): no Rust toolchain is needed there. Elsewhere, pip builds the source distribution, which
needs a Rust toolchain (1.92 or later).

### Rust

```rust
use sealwax::{Document, OpenOptions, SaveOptions};

let data = std::fs::read("in.pdf")?;
let mut doc = Document::open(data, &OpenOptions::default().page_count(1..=1000))?;
for page in doc.geometry() {
    println!("page {}: {:?}", page.page_number, page.crop_box);
}
sealwax::annots::flatten(&mut doc)?;
std::fs::write("flattened.pdf", doc.save(&SaveOptions::default())?)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Python

To build from a clone instead, with a Rust toolchain and [maturin](https://www.maturin.rs):

```sh
pip install maturin
maturin develop --release -m crates/sealwax-py/Cargo.toml
```

```python
import sealwax

data = open("in.pdf", "rb").read()
limits = sealwax.Limits(timeout=60)

pages = sealwax.read_geometry(data, max_pages=1000, limits=limits)
archived, report = sealwax.pdfa_convert(data, level="3a", limits=limits)
flattened = sealwax.flatten_bytes(data, limits=limits)

document = sealwax.Document.from_bytes(data, limits=limits)
image = document.render_png(0, scale=150 / 72)  # PNG bytes, 150 dpi
glyphs = document.glyphs(0)  # text, origin, advance, size, font of each glyph

# Signing: any object with certificate, chain and sign_digest(digest) -> bytes
signed = sealwax.sign(archived, my_hsm_signer, field="Signature1", certify=True)
```

Errors are exceptions under `sealwax.PdfError`. `PdfLimitError` names the bound a document
passed.

Threads: every call releases the interpreter lock while sealwax works, and documents share
nothing, so distinct `Document`s are processed in parallel from several threads. One `Document`
may be read from several threads at once (rendering, glyphs, fields); a call that modifies it
(`fill_fields`, `flatten`) needs it to itself, and raises `RuntimeError` when another call holds
it. Functions given bytes are independent of one another. Each thread keeps the fonts it parsed
for the document it read last (glyphs, drawings), and what it drew for the document it rendered
last, so that the next page, or the same page at another scale, does not parse or draw them
again; reading or rendering another document replaces them, and `sealwax.release_cache()` drops
them, with the documents they hold.

### Command line

```sh
sealwax info in.pdf
sealwax pdfa in.pdf --level a --out-dir out --out report.jsonl
sealwax render in.pdf --page 1 --scale 2 --out page1.png
sealwax glyphs in.pdf --out glyphs.jsonl
```

## Repository

| Path | Contents |
|---|---|
| `crates/sealwax` | The facade crate: the document model, with `annots`, `compose`, `pdfa`, `raster`, `sign` and `text` modules |
| `crates/sealwax-*` | One crate per area. `sealwax-cli` is the command line, `sealwax-py` the Python bindings |
| `fuzz/` | `cargo-fuzz` targets, one per entry point |
| `benchmark/` | Hostile-input bench, and judges that check outputs with veraPDF, pyHanko, PDFium and qpdf |

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first. Report security issues
privately, as [SECURITY.md](SECURITY.md) explains. Everyone taking part follows the
[Code of Conduct](CODE_OF_CONDUCT.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.

Code and data adapted from other projects are listed, with their licences, in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
