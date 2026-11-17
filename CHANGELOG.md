# Changelog

All notable changes to sealwax are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- PDF/A: a composite font without a program gets one when it is encoded `Identity-H` or
  `Identity-V` and its `ToUnicode` CMap says what its CIDs mean: the standard font that stands in
  for it (Arial's is Helvetica's), its glyphs put in the order of the CIDs (a CFF program read
  CID for glyph), each with the width `/W` gives it, so that no glyph moves. A font stays
  `font_not_embedded` when it has no `ToUnicode` (`composite_font_without_to_unicode`) or shows a
  CID the standard font has no glyph for (`composite_font_glyph_missing`): drawn blank, that text
  would be lost.
- Photos made pages (`sealwax_annots::photos_document`, `add_photo_pages`; `photos_to_pdf` and
  `Document.add_photo_pages` in Python, `photosToPdf` and `addPhotoPages` in WebAssembly): a
  US Letter sheet for each JPEG or PNG, portrait or landscape as the photo shows, half an inch of
  margin, the photo fitted without being stretched. A JPEG is embedded byte for byte
  (`DCTDecode`, its colour space read from its header, Adobe's inverted CMYK decoded); a PNG keeps
  its transparency; a JPEG's EXIF orientation becomes the page's rotation. The limits'
  `max_image_pixels` apply before anything is decoded; a photo that cannot be read gets no page
  and is reported.
- A WebAssembly build for browsers, web views and workers (`crates/sealwax-wasm`, built by its
  `build.sh` into the npm package `sealwax`): a `Document` class (open, geometry, fields read and
  filled, flattening, PNG rendering, metadata, outline, pages taken out, saving) and functions on
  bytes, with the package's own error classes. The limits' deadline reads its clock through
  `web-time`, which is `std`'s on every other target.
- Pages arranged and documents put together with their forms: `Document::compose` (pages of
  several documents, in any order, each turned), `Document::arrange` (turning, deleting, moving,
  reordering and taking out pages in one call) and `Document::insert`; in Python
  `Document.arrange`, `Document.insert_pages` and `merge_bytes(flatten=False)`, in WebAssembly
  `arrange`, `insertPages` and `mergeBytes({ flatten: false })`. Each field keeps its widgets on
  the pages copied, and only those; a top field an earlier document already names becomes
  `name (2)`.
- What goes with a flattening another engine makes: `own_page_boxes` writes on each page the
  boxes and rotation it inherits (an engine that does not look for them there flattens a Legal
  page to Letter), `clear_flattened_form` takes out the hidden widgets and the form left behind
  (and refuses when a visible widget remains), `has_redaction_marks` finds `/Redact`
  annotations (an unreadable PDF counting as marked, given as bytes), and
  `rebuilt_for_redaction` makes a document of the pages alone, which keep only what draws them,
  without widgets or hidden annotations, their optional content kept. In Python and in
  WebAssembly too.
- Rendering for a canvas: `sealwax_raster::render` gives RGBA pixels opaque over white (what
  `ImageData` takes), turns the page by quarter turns added to its rotation (the very image of
  the page carrying that rotation), and stops between its steps once `cancelled` says so,
  drawing nothing. Python: `render_rgba` and `quarter_turns`; WebAssembly: `renderRgba`
  (a `Uint8ClampedArray`; cancelled through an `Int32Array` over a `SharedArrayBuffer`) and
  `quarterTurns`.
- Fields created on a page (`sealwax_annots::add_field`): a text field, a date field (with
  Acrobat's `yyyy-mm-dd` date actions) or a check box, named, required or not, placed at a
  rectangle given on the page as it shows (rotation applied, from the crop box's top-left
  corner), with a gray border and no background, turned with the page so that its text reads
  upright. A name the form already has is refused (`Error::FieldNameTaken`,
  `FieldNameTakenError` in Python and WebAssembly).
- `fill_fields` says what it did with each name: refused, the field left as it was
  (`Filled::refused`: no field of that name on the pages, a signature or push button, a check box
  or radio button given a state none of its widgets has, a state given as text while the document
  spells it otherwise included), or written, with the value the field held before
  (`Filled::written`). `fill_fields_with` takes `FillOptions`: `keep_filled` leaves a field
  holding another value as it is (`Filled::kept`), `strict` refuses what a reader would not let a
  person type (a read-only field, a text longer than its maximum length or with characters its
  font cannot draw, a choice the field does not offer). `unfill_fields` gives each field written
  its previous value back, but those changed since. `Field::editable` tells a combo box that
  takes typed text. In Python (`keep_filled`, `strict`, `unfill_fields`) and WebAssembly
  (`keepFilled`, `strict`, `unfillFields`) too.
- A filled form made a template again (`clear_template`, `template_content`; `clear_template`
  and `template_content` in Python, `clearTemplate` and `templateContent` in WebAssembly): every
  field cleared with its empty appearance drawn, the XFA form taken out, the annotations a person
  added over the pages taken out (markup and their pop-ups; links and widgets stay) unless asked
  to keep them, the information's title set and its author, subject and keywords taken out with
  the XMP metadata. A full save leaves the values and appearances replaced out of the file.

### Changed

- `fill_fields` leaves a check box or radio button given a state none of its widgets has as it
  was, and reports it, where it turned it `Off`; signature fields and push buttons named are
  reported too.
- `fill_fields` draws a text field's appearance turned as its widget is (`/MK /R`): on a turned
  page, the value reads upright where it shows.
- `Document::push_page` keeps the `/Rotate` the page it adds holds in its geometry, and
  `Document::empty_with` starts an empty document held to given limits.
- Glyphs, drawings and PDF/A tagging keep the fonts they parsed from one call to the next: each
  thread keeps them for the document it read last (`release_cache()` drops them). On a document
  embedding ten whole TrueType fonts, reading a page's glyphs went from 23 ms to 1.3 ms.
- Checking a page against the limits decodes a stream other pages share (a font program) once
  per document, and opening a document finds its structure streams with a vectorised search.
- A modified document is saved and read again once for what reads its pages (rendering), until
  it changes again: `Document::reopened_if_modified` keeps that copy, and returns it shared.
- Drawings count as MuPDF's in two more cases: a box filled with its fourth side drawn back to
  its start, without `h`, is four lines (stroked only, it stays a rectangle); a curve whose
  control points lie on its ends is a line.

### Fixed

- Pages taken out (`extract_pages`, `append_page_range`) no longer carry the widgets of the pages
  left out, with their appearances, that a field on a page kept led to.

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
