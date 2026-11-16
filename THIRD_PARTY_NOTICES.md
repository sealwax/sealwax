# Third-party notices

This file lists the third-party code and data **copied or adapted into this repository**, with
their licences. Dependencies pulled through Cargo or PyPI are not listed here: `cargo-deny` checks
their licences on every build (see `deny.toml`), and each Python wheel carries the licences of
the crates compiled into it (`licenses/DEPENDENCIES.md`, written by cargo-about at release).

Rule: only code and data under licences compatible with MIT OR Apache-2.0 (permissive, no
copyleft) are copied in, each listed below with its licence and what was changed.

Format of an entry:

| Project | URL | Licence | Version or commit | Files in this repository | Modified |
|---|---|---|---|---|---|

## Entries

| Project | URL | Licence | Version or commit | Files in this repository | Modified |
|---|---|---|---|---|---|
| Compact ICC Profiles | https://github.com/saucecontrol/Compact-ICC-Profiles | CC0-1.0 | `bdd84663061bc4ae95ca70decff54f581e27f702` | `crates/sealwax-pdfa/assets/icc/sRGB-v2-magic.icc`, `crates/sealwax-pdfa/assets/icc/CGATS001Compat-v2-micro.icc` (licence: `crates/sealwax-pdfa/assets/icc/LICENSE-CC0`) | No |
| Foxit fonts of PDFium (as shipped by hayro-font 0.5.0) | https://pdfium.googlesource.com/pdfium/ | BSD-3-Clause | hayro-font 0.5.0, `assets/` | `crates/sealwax-pdfa/assets/fonts/*.cff` (licence: `crates/sealwax-pdfa/assets/fonts/LICENSE-FOXIT`) | Renamed from `.pfb` to `.cff`: they are bare CFF |
| hayro-font | https://github.com/LaurenzV/hayro | MIT OR Apache-2.0 | 0.5.0 | `crates/sealwax-pdfa/src/fonts/encodings.rs`, generated from its `src/generated/` encoding tables (themselves after Apache PDFBox and ISO 32000-1, annex D) | Rewritten as arrays of 256 names |
| dingbat-to-unicode | https://github.com/mwilliamson/dingbat-to-unicode | BSD-2-Clause | 1.0.2 (npm) | `crates/sealwax-pdfa/src/fonts/dingbats.rs`, generated from its `dist/dingbats.js` tables (licence: `crates/sealwax-pdfa/src/fonts/LICENSE-dingbat-to-unicode`) | Rewritten as arrays of 224 values; the one Private Use value of the Symbol table left out |
| PDFium, appearance generation (`core/fpdfdoc/cpdf_generateap.cpp`) | https://pdfium.googlesource.com/pdfium/ | BSD-3-Clause | `b81e7ceb324fad3a7649b64bd54d67fe8eca6da3` | `crates/sealwax-annots/src/generate.rs` (licence: `crates/sealwax-annots/src/LICENSE-PDFIUM`) | Translated to Rust: circles, squares, ink, text markup and notes only |
