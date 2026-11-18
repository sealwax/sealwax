# Changelog

All notable changes to sealwax are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The files a document embeds (`Document::attachments`; `attachments` in Python and
  WebAssembly): those of its `/EmbeddedFiles`, in the order of their keys, then those of its
  pages' file attachment annotations, each with its name, key, description, MIME type, size,
  dates, relationship to the document, and page and rectangle for an annotation's. One read
  (`attachment_data`; `attachmentData`), bounded by `max_stream_bytes`. A file embedded
  (`Document::add_attachment`, `NewAttachment`; `add_attachment`, `addAttachment`): compressed,
  its MIME type (guessed from its extension when not given), size, MD5 checksum and date in
  `/Params`, named in `/F` and `/UF`, with its description and `/AFRelationship`, added to
  `/EmbeddedFiles` and to the catalog's `/AF`: a document with a file added converts to
  PDF/A-3b that veraPDF passes. Refused for a document claiming PDF/A-1 or PDF/A-2, and for an
  `/EmbeddedFiles` that cannot be read whole, which rewriting would lose. `mime_type_for` and `is_mime_type`, which PDF/A conversion
  used on its own.
- The images a page draws (`sealwax_text::images`; `images` in Python and WebAssembly), in the
  order of its content, forms and inline images included: the box and matrix they are drawn
  with, their size in pixels, object, colour space family, bits per component, filters, and
  whether they are a stencil mask or masked. One extracted (`extract_image`; `extract_image` in
  Python, `extractImage` in WebAssembly): the JPEG or JPEG 2000 file it is stored as when it has
  no mask, decode array or other filter, else a PNG of its pixels with its soft mask as alpha.
  On a corpus of 98 documents, boxes are PyMuPDF's on every image compared; PyMuPDF also counts
  shadings and the images of soft masks, which are not images a page draws.
- A page's user unit set (`Document::set_user_unit`; `set_user_unit` in Python), on a document
  built or opened: `/UserUnit` written on the page, 1 taking it away; `geometry` reports it at
  once.
- A page's links and where they lead (`Document::links`; `links` in Python and WebAssembly):
  each link's rectangle, and a page of the document (destinations, named ones resolved, `GoTo`,
  `FirstPage` and `LastPage`), a URI, another file and its page, a named action, or what cannot
  be followed, with its action's type. Never a guess.
- The labels pages are shown with (`Document::page_labels`; `page_labels` in Python,
  `pageLabels` in WebAssembly): prefixes and numbers in decimal, roman numerals or letters, as
  `/PageLabels` gives them; `None` when the document gives none. Refused past 16 MiB of labels
  together.
- `sealwax-compose`: tagged documents. A composition may mark what its pages draw
  (`Item::BeginMarked`, `Item::EndMarked`: a structure element's content, by marked-content
  id, or an artifact, of pagination or another kind) and give the logical structure that ties
  it together (`Composition::structure`): elements of any standard type, each holding elements,
  marked content by page and id, and link areas, with alternate and actual text, language, ID,
  and table cell and list attributes. It is written as PDF tags it: the structure tree and its
  parent tree, `/StructParents` and `/Tabs /S` on the pages, `/StructParent` on the links,
  `/MarkInfo`. Marked sequences are balanced on each page and their ids renumbered from 0; what
  does not tie to the pages (an element listed twice or outside the tree, content not drawn, a
  link not written) is left out and counted. PDF/A level a keeps the tree.
- `sealwax-compose`: bookmarks (`Composition::outline`), each nested under the nearest one before
  it of a smaller level and leading to a height of its page; the document opens with them shown.
- `sealwax-compose`: an XMP packet with what the document information says, and
  `/DisplayDocTitle` in the viewer preferences, as PDF/UA asks. A layout that knows its document
  meets PDF/UA-1 claims it (`Info::pdf_ua`); the claim is written only when nothing was left out.
- `Limits::max_unpacked_bytes` (512 MiB by default): what a packaged document (a DOCX) may
  unpack to, all its parts together, as its archive declares them. A DOCX converted is checked
  against it before its layout, so a ZIP that expands without end is refused rather than
  exhausting the memory.
- In Python, `docx_to_pdf(docx_content, font_dirs=None, limits=None)`: a Word document converted
  to PDF, laid out by docxide-pdf, tagged with its structure and bookmarks, and what the
  conversion left out (`skipped`, `not_described`). ValueError when it cannot be laid out;
  `Limits(max_unpacked_bytes=...)` bounds what it unpacks to. The layout's time is not bounded:
  convert documents you do not trust with `sealwax.isolated`. Building the Python package now
  needs Rust 1.97.

### Changed

- `NewOutlineEntry` has a `top`: the height an entry leads to (`/XYZ`), or none for the whole
  page (`/Fit`, as before); it is no longer `Eq`. `Document::set_outline` takes time in
  proportion to the entries, not to their square.

### Fixed

- Redacting a second page of an encrypted document, or of one whose cross-reference was
  repaired, failed with `Unsupported`, about an incremental save the caller never asked for. The
  document is now read again with its objects written anew under their numbers.

### Security

- Redaction left tiling patterns in the file. A shape painted with one under an area went, or
  stayed with `paths` covered or kept, while the pattern and the images and text it draws stayed
  in the page's resources, after `rebuilt_for_redaction` too. Such a shape now goes whole,
  whatever `paths` asks, and a pattern nothing left paints with leaves the resources. That
  holds for a form painting with the pattern its content inherits, which is looked up where the
  colour was set (`scn`).
- The `/Alt`, `/ActualText` and `/E` of an image or a form tied to the structure by an object
  reference (`/StructParent`) now go when it loses content.

## [0.6.1] - 2026-11-18

### Security

- Redaction (`redact`, `apply_redactions`) left what it took out in the file, from 0.2.0 to
  0.6.0:
  - The form XObjects it copied, and the images it wrote anew or took out, stayed named in the
    page's resources: the original form, with the text taken out, and the original image, with
    the pixels blanked, were still saved, after `rebuilt_for_redaction` too. What the page no
    longer draws now leaves its resources.
  - The `/ActualText`, `/Alt` and `/E` standing for the content taken out stayed, and text
    extraction reads them in its place: in the properties of the marked-content sequences it was
    in, written in the content or named in the resources, and on the structure elements tied to
    those. They now go, from the elements' ancestors too; `Redacted::replacement_texts` counts
    the dictionaries that lost them.
- A redacted document refuses an incremental save, which kept the bytes it was opened from and
  what was taken out with them (`Document::mark_redacted`, `Document::is_redacted`).

## [0.6.0] - 2026-11-18

### Added

- In WebAssembly, `drawings(pageIndex, annotations)` and `glyphs(pageIndex, annotations)`, as in
  Python: the straight lines, rectangles and quads a page paints, and the glyphs it draws, in its
  default user space. Each line, rectangle and quad says whether it is filled, stroked or both
  (`filled`, `stroked`; a shape filled then stroked counts once, as both), and each line the
  subpath it belongs to (`subpath`), for a caller that reads a subpath whole: a box open on one
  side is three lines of one subpath. In Rust, `Drawings` gains `lines_painted`,
  `rectangles_painted` and `quads_painted` (`Painted`), and `lines_subpath`. Python's `drawings`
  is unchanged.
- `add_widget` gives a field a form has another widget (`addWidget` in WebAssembly,
  `sealwax_annots::add_widget` in Rust): the same field shown on several pages, or twice on one,
  one value for all, as real forms repeat a name or a date. A text field, combo or list box, or
  check box; the field keeps what it is (its kind, flags, value, maximum length, label, options),
  the widget takes the look of its first one, turned with its own page, and shows the field's
  value. A field that is one dictionary with its widget becomes that widget's parent, where it
  was in the form.

## [0.5.0] - 2026-11-18

### Added

- Documents built from nothing, as test fixtures are (`Document.new()` in Python): blank pages
  of a given size (`add_page`, A4 by default) and their crop box (`set_crop_box`, given as
  PyMuPDF's `set_cropbox` takes it); text in a standard font, not embedded, in WinAnsiEncoding
  (`insert_text`, `\n` starting a line the font's height below, as PyMuPDF writes it), lines
  and rectangles (`draw_line`, `draw_rect`), links to addresses (`add_link`), all placed on the
  page as it shows, as fields are, text upright; an outline (`set_outline`, as `outline` reads
  it back), the document information (`set_info`) and an XMP packet (`set_xmp`). In Rust,
  `Document::add_page`, `set_crop_box`, `set_outline`, `set_info`, `set_xmp`, and
  `sealwax_annots::draw_shapes` and `add_link`.
- `add_field` creates combo boxes (`combobox`, its options exported values and labels) and
  signature fields, gives any field the name shown to users (`label`, `/TU`) and a text field
  its maximum length (`max_length`). Fields a form has can be retouched: the name shown
  changed (`set_field_label`), a check box's checked state renamed, to a name of any bytes
  (`rename_button_state`), the appearances of a field's widgets taken away
  (`remove_field_appearances`).
- Saving encrypted with the standard security handler: RC4 with a 128-bit key, AES-128 or
  AES-256, an owner password, a user password or none, and the permissions granted
  (`save(encryption="aes-256", owner_password=..., user_password=..., permissions=4 | 256)` in
  Python, `SaveOptions::encrypt` in Rust). Read back by qpdf and PyMuPDF with either password.
  The randomness keys and initialization vectors take comes from the caller in Rust
  (`Encrypt::seed`), from the system in Python. sealwax itself opens a document of RC4 or
  AES-128 with its user password only, as hayro does.
- `metadata()` describes how a document is encrypted (`encryption`): its security handler, `/V`
  and `/R`, the length of its key and how its streams are encrypted (RC4, AES, none), as a
  string as PyMuPDF writes it (`Standard V5 R6 256-bit AES`) in Python and WebAssembly. None
  for a document that is not encrypted.

### Fixed

- A full save of a document whose catalog is no dictionary (a stream a later revision gave the
  catalog's number), its pages found by scanning the file, writes a catalog of its own over the
  page tree rebuilt. It wrote the object as found, which led to no page, and the save failed its
  read-back check. Found by fuzzing.

## [0.4.0] - 2026-11-17

### Added

- `sealwax-compose`: a new document written from laid-out pages (`compose` and `Composition`),
  for what another engine lays out (a word processor's document, HTML) to become a PDF that the
  rest of sealwax archives, fills and signs. Pages hold glyph runs, each glyph where the layout
  put it, paths filled and stroked, clips, transforms, linear gradients (in linear light, as Word
  draws them, or in sRGB), pictures already encoded (JPEG passed through) and link areas (to an
  address or a page). Fonts are embedded as subsets with their `ToUnicode` maps, TrueType or
  CFF outlines, from a file or a collection (`font`, cut by subsetter); a standard-14 font is
  written with the widths the layout measured. What cannot be written (a program that cannot
  be embedded or that the subsetter cannot cut, a glyph its font lacks, which PDF/A forbids
  showing as `.notdef`, a picture or its soft mask past `max_image_pixels`, a palette of no 1
  to 256 colours) is left out and counted (`Composed::skipped`); the deadline is checked as
  pages are written. A fuzz target covers the font programs.

### Changed

- A glyph's `size` and `bbox` (`glyphs`) are now those MuPDF gives. The size is the side of a
  square as large as an em square after the text and current transformation matrices, so an em
  stretched or squeezed (`Tz`, a text matrix `0.8 0 0 1`) is drawn at less than the font size.
  The box reaches from the font's descent to its ascent rather than from 0.2 to 0.8 em, at least
  an em tall: the font descriptor's, else the font's TrueType program's, else those of the font
  a standard font is drawn with, else 0.2 and 0.8 em, as measured against PyMuPDF. On a corpus
  of 98 documents (1.3 million glyphs), the box is PyMuPDF's to 1 point for all glyphs but 15,
  and the size to 0.5% for all but 20, against 25% and 96% of them before.
- A page rendered again (at another scale, in parts, after another page of the same document)
  reuses the fonts parsed and the glyphs drawn by the renders before it. Natively, pages of text
  render again 1.3 to 4 times faster; in Chrome, where filling the pixels weighs more, up to twice
  as fast as thumbnails and about a fifth faster at twice their size. Each thread keeps them for
  the document it rendered last, until it renders another, `release_cache()` in Python, or, in
  WebAssembly, a document freed. A scanned page's image is still decoded on every render.
- The fonts given for free texts (`TextFonts`; `fonts` and `default_font` in Python, `fonts` and
  `defaultFont` in WebAssembly) may be OpenType programs with CFF outlines, or collections (their
  first face), as well as TrueType programs: they are embedded as `sealwax-compose` embeds
  fonts. The subset now holds the glyphs used only, numbered from 1 (they kept their
  place among emptied ones): the PDF's bytes change, not what it draws or reads back. A program
  the subsetter cannot cut is refused, as one that cannot be read. In WebAssembly, where a panic
  cannot be caught, a program made to make the subsetter panic stops the module (see the
  WebAssembly README).

### Fixed

- `glyphs()`: a glyph of a font not embedded that is drawn with a font of other widths ends
  where the font's width puts it; it ended where the other font's glyph, stretched to that
  width, was scaled to end. A glyph an annotation draws names its font as written, as those
  of the page do, and a glyph drawn in a form that draws with several fonts gets the font it
  is drawn in.
- `drawings()` leaves out a line that goes nowhere (an `l`, or a curve made a line, ending where
  it starts, as written), as MuPDF does, unless it comes right after a move, where it stays a line
  of no length. Since 0.2.0 made degenerate curves lines, the outlines of letters drawn as paths
  added such lines: 6.7% more lines than PyMuPDF on a corpus of forms. A line of no length that
  is kept now comes as written, its ends equal.

## [0.3.0] - 2026-11-17

### Added

- A signing page's XFDF applied in WebAssembly (`doc.applyXfdf(xfdf, { flatten, removeAnnotations })`),
  with the code the server runs: the same document and XFDF give the same PDF, byte for byte, in
  the browser and in Python. Several XFDF apply one after the other, one per signed field.
- What applying an XFDF does to the document's own annotations, chosen
  (`sealwax_annots::apply_xfdf_with` and `ApplyOptions`; `remove_annotations` in Python,
  `removeAnnotations` in WebAssembly): flattened under the XFDF, kept as annotations, or taken out
  with the form, nothing of them painted: the pages show their content and the XFDF alone.
  Links stay.
- `Applied.pages` (`pages` in Python and WebAssembly): the pages an XFDF changed, from 0, those it
  draws on and those whose annotations were flattened or taken out, for a viewer to render again;
  `Applied.annotations_removed`.
- A part of a page rendered alone (`RenderOptions::clip` and `Clip`; `clip=(x, y, width, height)`
  in Python, `clip: { x, y, width, height }` in WebAssembly, for PNG and RGBA): the part of the
  whole page's image, in its pixels at the scale and turned as asked, its pixels those of the
  whole image there to rounding (the drawing moved by whole pixels: at most a level or two apart
  on edges, a sample of an image at a texel's border now and then). `max_render_pixels` counts
  the part's pixels, so that a page zoomed past the bound renders by parts. Each part interprets
  the page whole and decodes the images it shows: render the part a screen shows in one call
  rather than many small tiles.
- The XFDF of a signed field written (`xfdf::write_command`, `NewMark`, `NewPicture`, `NewText`;
  `xfdf_for` in Python, `xfdfFor` in WebAssembly): PNG stamps and free texts, in PDF user space,
  turned by quarter turns, with their name and custom data, in the command form a signing page
  sends (`<add>`), exactly what `apply_xfdf` reads and draws: read back before it is returned. The
  same marks give the same PDF as a web viewer's export of them.
- Free texts drawn with the TrueType fonts a caller gives (`ApplyOptions::fonts` and
  `TextFonts`; `fonts` and `default_font` in Python, `fonts` and `defaultFont` in WebAssembly):
  by the font the XFDF names (`Caveat`), and one for every other, embedded as a subset (a
  composite font, `Identity-H`, the glyphs used kept in their place, the program's widths, a
  `ToUnicode` map): each character the font has drawn as it is, and read back. A character it
  lacks is drawn as its `?` and counted; `.notdef` is never shown. The documents convert to PDF/A
  as before. Without fonts, Helvetica as before.
- Drawings (`<ink>`: signatures drawn by hand) in a signing page's XFDF drawn, where they were
  refused: each stroke a line through its points (round caps and joins, a single point a dot),
  in its colour and width, at most 500,000 points an XFDF; one turned, translucent or holding
  more than its strokes is refused. `Applied.inks` counts them; `NewInk` (`"ink"` marks in
  Python and WebAssembly) writes them.
- Free texts in Helvetica-Bold drawn in Helvetica-Bold, a standard font (its WinAnsiEncoding
  widths, Adobe's metrics), rather than in Helvetica; a program given for `Helvetica-Bold` takes
  its place.
- A font size for the text of a field created (`NewField::font_size` and `FIELD_FONT_SIZES`;
  `font_size` in Python, `fontSize` in WebAssembly), 1 to 1000 points: its `/DA` says it and its
  value is drawn at it. Without it the size is fitted to the field (`0 Tf`) as before, which some
  viewers (PDFium) do not keep a typed value in. A check box's mark stays fitted.

### Changed

- An XFDF mark turned by quarter turns (`rotation` 90, 180 or 270, as a signing page sets the
  page's own rotation on a turned page) is drawn turned counterclockwise within its rectangle, so
  that it shows upright on the page as shown; it was refused. Other angles are still refused.
- `Applied` is `#[non_exhaustive]`; `xfdf::Picture` and `xfdf::Text` have a `rotation`.
- A JPEG with two EXIF segments is turned as its first one says, as Chrome and Apple's ImageIO
  turn it, even when that one has no orientation; the last one decided. A phone writes one.

## [0.2.0] - 2026-11-17

### Added

- Pages turned in place (`Document::rotate_pages`; `rotate_pages` in Python, `rotatePages` in
  WebAssembly): `/Rotate` changes and the document keeps everything it has (outline, structure,
  information, metadata), unlike `arrange`, which makes a new one.
- Encrypted documents refused when asked (`OpenOptions::refuse_encrypted`; `refuse_encrypted` in
  Python, `refuseEncrypted` in WebAssembly, for opening, merging and inserting pages): an error of
  its own (`Error::Encrypted`, `PdfEncryptedError`), as sealwax saves documents unencrypted.
- The npm package under Node: its `node` export reads the module from the package and is ready
  once imported. After a panic, every call throws `SealwaxCrashedError` until `reset()` makes the
  module anew; a document opened before is refused rather than read in the new memory.
- Redaction (`sealwax_text::redact`; `Document.redact` in Python, `doc.redact` in WebAssembly):
  what a page draws under areas is taken out of its content, rather than covered. Glyphs: each
  string split into codes and each code placed as hayro reads it, a glyph whose box (its advance
  from 0.2 em below its baseline to 0.8 em above it, and its outline) an area overlaps taken out,
  an adjustment moving as far in its place so that nothing after it moves; text in a hidden layer
  too. Images: their pixels under the areas blanked (black, transparent in their soft mask), the
  image written anew in RGB or gray, or taken out whole (`images`: "pixels", "remove", "keep").
  Paths painted over an area: taken out when the area touches them, or covers them whole
  (`paths`: "touched", "covered", "keep"), a clipping path clipping still. A form XObject losing
  content is copied for the drawing that loses it, the others left whole.
- Redaction marks applied (`sealwax_annots::apply_redactions`; `Document.apply_redactions`,
  `doc.applyRedactions`): the other annotations flattened first, what the pages draw under the
  marks taken out, each mark's area painted with its interior colour (`/IC`) and its overlay
  text written, the marks and the links under them taken out. Follow with
  `rebuilt_for_redaction` and a full save.
- Annotations made with their appearance (`sealwax_annots::add_annotation`;
  `Document.add_annotation` in Python, `addAnnotation` in WebAssembly): ink, highlight (its
  colours multiplied, the text showing through), underline, strike out, squiggly, square, circle,
  line, polygon, polyline and free text, in the page's user space, with their colour, fill,
  width, opacity, author and contents. Every reader draws them alike, and flattening keeps them.
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

- The WebAssembly module is built with WebAssembly SIMD and optimized by Binaryen
  (`wasm-opt -Oz`): 7.7 MB to 4.4 MB, 2.32 MB to 1.98 MB gzipped. It needs an engine with SIMD
  (Chrome and Edge 91, Firefox 89, Safari 16.4, Node 16.4). Its optional parameters are declared optional (`options?:`), and its
  declarations are compiled by the package's tests.
- `fill_fields` gives a check box or radio button the state the value means: its name (bytes
  or text, `Français` finding `/Fran#E7ais`), the export value of a widget (`/Opt`), a word that
  unchecks (`off`, `no`, `false`, `0`, `non`…) or, for a button with one state besides `Off`, a
  word that checks (`yes`, `on`, `true`, `1`, `x`, `oui`…) whatever that state is named;
  `True` and `False` in Python and WebAssembly. A value that means no state, or could mean
  several, leaves the button as it was and is reported, where it turned it `Off`; signature
  fields and push buttons named are reported too.
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

- The package's TypeScript declarations compile again: `RenderedPage` was left open.
- A word inside an array of a content stream is read as an operand, as readers read it
  (`[(a) 0 Tc (b)] TJ` is one `TJ`): such text gets its font named in `glyphs`.
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

[Unreleased]: https://github.com/sealwax/sealwax/compare/v0.6.1...HEAD
[0.6.1]: https://github.com/sealwax/sealwax/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/sealwax/sealwax/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/sealwax/sealwax/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/sealwax/sealwax/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/sealwax/sealwax/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/sealwax/sealwax/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sealwax/sealwax/releases/tag/v0.1.0
