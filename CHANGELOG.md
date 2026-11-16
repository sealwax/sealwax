# Changelog

All notable changes to sealwax are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `fill_fields` reports the check boxes and radio buttons given a state none of their widgets has
  (`Filled::unmatched`, `unmatched` in Python): they show `Off`, as before, but no longer
  silently. A state given as text while the document spells it otherwise lands there.

## [0.1.0] - 2026-11-16

The first release.

### Added

- Documents:
  - opening, including damaged files and encrypted ones (with their password);
  - page count and page geometry;
  - what a document says about itself (version, encryption, linearization, form and signature
    fields, PDF/A claim, document information), never failing;
  - the outline, flat, with named destinations resolved;
  - pages taken out into a new document;
  - full saves (cross-reference streams or tables) and incremental saves;
  - text strings (document information, outline titles, field names, labels and values,
    `/ActualText`) read as MuPDF reads them, UTF-8 without a byte order mark and UTF-16LE
    included; PDF/A conversion rewrites such document information in UTF-16BE, so that its XMP
    packet and a validator reading by the letter of the standard agree.
- PDF/A-3 conversion at levels b, u and a: output intent, embedded fonts, font programs made
  consistent with their dictionaries (TrueType, CFF and Type 1), Unicode mapping, embedded files
  described and associated, metadata, and tagging of untagged documents.
- Form fields: read as their widgets place them, filled (appearances drawn, button state names
  kept to the byte) and read back; flattening of the fields alone.
- Annotations: flattening, with appearances drawn where missing; XFDF from web signing pages;
  merging.
- PAdES signatures:
  - certification and approvals, signed outside the library with RSA (PKCS #1 v1.5 or PSS) or
    ECDSA, over SHA-256, -384 or -512;
  - RFC 3161 timestamps (B-T);
  - long-term validation data (B-LT);
  - document timestamps (B-LTA), and validation data for them;
  - an integrity check of the signatures a document holds (byte range, digest, value,
    timestamp, revisions since); nothing is signed or added over one that is not intact.
- Text: the glyphs a page draws, in the order of its content (forms, Type 3 fonts, annotations
  on request), with their text, origin, advance, box, size, direction, font, and whether they are
  invisible, drawn again over themselves, or replaced by an `/ActualText` (from marked content or
  the structure tree). The straight lines and rectangles a page paints (annotations on request),
  as MuPDF's drawings count them: subpaths closed with their last side, closed boxes as
  rectangles, a shape filled then stroked once, shadings left out.
- Rendering: one page to a colour PNG at any scale (annotations and field values included), and
  every page to a grayscale PDF. `Limits::max_render_pixels` bounds a rendered page.
- Lossless optimization.
- `Limits`: bounds and a deadline for every operation, checked before pages are interpreted;
  streams counted at every stage of their filters before they are decoded.
- Python bindings (Python 3.12+): every operation, signatures with signers written in Python,
  HTTP clients for timestamp and revocation, and `isolated` to run an operation in its own process.
- `sealwax` command line (`render` and `glyphs` among its commands), fuzz targets, and benches
  for hostile inputs and output validation, with comparisons against PyMuPDF's text, drawings
  and speed, and PDFium's rendering.

[Unreleased]: https://github.com/sealwax/sealwax/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sealwax/sealwax/releases/tag/v0.1.0
