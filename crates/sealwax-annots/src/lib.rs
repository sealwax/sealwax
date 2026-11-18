//! XFDF applied to PDF documents, and annotations flattened into their pages.
//!
//! - [`apply_xfdf`] draws the pictures, texts and drawings of the XFDF a signing page exports
//!   into the pages, after flattening what the document had. Fails closed: an annotation it
//!   cannot draw refuses the whole XFDF, before any page changes. `<delete>` is read but never applied: a
//!   signing page only deletes its own placeholders, and a signer must not take an annotation of
//!   the original PDF out of the signed one.
//!   [`apply_xfdf_described`] gives each picture an alternate description (whose signature it
//!   is: the XFDF names no one, its caller knows), which PDF/A level a makes the alternate text
//!   of the picture's figure. [`apply_xfdf_with`] takes [`ApplyOptions`]: the document's own
//!   annotations flattened, kept as they are, or taken out, and says which pages changed.
//! - [`stamp_images`] draws pictures at rectangles of the signing page's frame (`position_metadata`),
//!   each with its description if it has one.
//! - [`flatten`] paints every annotation into its page; links stay.

mod annotate;
mod create;
mod draw;
mod fields;
mod flatten;
mod font;
mod generate;
mod image;
mod jpeg;
mod page;
mod pdf;
mod photos;
mod redactions;
mod template;
mod text;
pub mod xfdf;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use sealwax_core::{Dict, Document, ObjId, Object, PageGeometry, Rect};

pub use annotate::{AnnotationKind, NewAnnotation, add_annotation};
pub use create::{FIELD_FONT_SIZES, NewField, NewFieldKind, add_field};
pub use fields::{
    Field, FieldChoice, FieldKind, FieldValue, FillOptions, FillValue, Filled, Written,
    field_values, fields, fill_fields, fill_fields_with, name_text, unfill_fields,
};
pub use flatten::Flattened;
pub use font::TextFonts;
pub use image::{MAX_PIXELS, PictureError};
pub use photos::{PhotoPages, add_photo_pages, photos_document};
pub use redactions::{Redactions, apply_redactions};
pub use template::{Cleared, TemplateContent, TemplateOptions, clear_template, template_content};

use page::Placed;

/// Everything that can go wrong. Messages never hold a value of the document or of the XFDF.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The XFDF is not what a signing page writes.
    #[error(transparent)]
    Xfdf(#[from] xfdf::Refused),
    /// An annotation or a picture cannot be placed (1-based position in the XFDF or the list).
    #[error("annotation {position} {reason}")]
    Placement { position: usize, reason: String },
    /// A picture cannot be read.
    #[error("picture {position} {source}")]
    Picture {
        position: usize,
        source: PictureError,
    },
    /// Reading or changing the document failed.
    #[error(transparent)]
    Pdf(#[from] sealwax_core::Error),
    /// A source of [`merge`] could not be flattened or appended (1-based position).
    #[error("source {position}: {error}")]
    Source { position: usize, error: Box<Error> },
}

/// Result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A signer's picture and where it goes, in the frame of `position_metadata` (see [`xfdf`]): the
/// page unrotated, origin at the top-left corner of its crop box, y downward, in points.
#[derive(Clone, PartialEq)]
pub struct Stamp {
    /// 1-based.
    pub page_number: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub png: Vec<u8>,
    /// What the picture shows, said in words (`Signature of …`): its alternate description,
    /// which tagging (PDF/A level a) makes the alternate text of its figure.
    pub alt: Option<String>,
}

impl fmt::Debug for Stamp {
    // The picture is a signature: kept out of debug output and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stamp")
            .field("page_number", &self.page_number)
            .field("x", &self.x)
            .field("y", &self.y)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("png_bytes", &self.png.len())
            .field("alt", &self.alt.as_ref().map(|_| "…"))
            .finish()
    }
}

/// What [`apply_xfdf`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Applied {
    /// Pictures drawn.
    pub pictures: usize,
    /// Free texts drawn.
    pub texts: usize,
    /// Drawings (`<ink>`: signatures drawn by hand) drawn.
    pub inks: usize,
    /// Characters of the texts WinAnsiEncoding does not have, drawn as `?`.
    pub characters_replaced: usize,
    /// Deletions read and, by design, not applied.
    pub deletions_ignored: usize,
    /// What flattening did, when asked.
    pub flattened: Option<Flattened>,
    /// The document's own annotations taken out, when [`ApplyOptions::remove_annotations`] asked
    /// it (links stay; popups are not counted).
    pub annotations_removed: usize,
    /// The pages that changed (from 0, in order): those the XFDF draws on, and those whose
    /// annotations were flattened or taken out. A viewer renders these again, and only these.
    pub pages: Vec<usize>,
}

/// How [`apply_xfdf_with`] treats what the document already has, and the fonts it draws texts
/// with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ApplyOptions {
    /// Flatten the document's annotations (links but) and its form into the pages first: the
    /// XFDF draws over what the signer saw. Off, they stay annotations.
    pub flatten: bool,
    /// Take the document's annotations (links but) and its form out first, nothing of them
    /// painted: the pages show their content and the XFDF alone. Flattening then has nothing
    /// left to do.
    pub remove_annotations: bool,
    /// TrueType or OpenType programs for the free texts, by the font the XFDF names, and for
    /// every other:
    /// embedded (the glyphs used), each character the font has drawn as it is. Without them,
    /// Helvetica, the characters WinAnsiEncoding lacks drawn as `?`.
    pub fonts: TextFonts,
}

impl ApplyOptions {
    /// Nothing flattened, nothing taken out.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// See [`ApplyOptions::flatten`].
    #[must_use]
    pub fn flatten(mut self, flatten: bool) -> Self {
        self.flatten = flatten;
        self
    }

    /// See [`ApplyOptions::remove_annotations`].
    #[must_use]
    pub fn remove_annotations(mut self, remove: bool) -> Self {
        self.remove_annotations = remove;
        self
    }

    /// See [`ApplyOptions::fonts`].
    #[must_use]
    pub fn fonts(mut self, fonts: TextFonts) -> Self {
        self.fonts = fonts;
        self
    }
}

/// What is drawn where, checked before any page changes.
enum Prepared {
    /// A PNG checked whole, decoded again when drawn: one picture's pixels at a time.
    Picture {
        png: Vec<u8>,
        rect: Rect,
        rotation: u16,
        alt: Option<String>,
    },
    Text {
        text: xfdf::Text,
    },
    Ink {
        ink: xfdf::Ink,
    },
}

/// What the pictures of one XFDF or one list of stamps may spend, checked before any page
/// changes: as many marks as the document's limits allow annotations on a page
/// (`max_annotations`), each picture and all of them together within `max_image_pixels`, each
/// picture read whole once (and its pixels dropped) so that one that does not read refuses them
/// all.
struct Budget {
    pixels_left: u64,
}

impl Budget {
    fn new(doc: &Document, marks: usize) -> Result<Self> {
        let limits = doc.limits();
        if u64::try_from(marks).unwrap_or(u64::MAX) > limits.max_annotations {
            return Err(sealwax_core::Error::LimitExceeded {
                what: "max_annotations",
            }
            .into());
        }
        Ok(Self {
            pixels_left: limits.max_image_pixels,
        })
    }

    fn picture(&mut self, png: &[u8], position: usize) -> Result<()> {
        let pixels = image::pixels(png).map_err(|source| Error::Picture { position, source })?;
        self.pixels_left =
            self.pixels_left
                .checked_sub(pixels)
                .ok_or(sealwax_core::Error::LimitExceeded {
                    what: "max_image_pixels",
                })?;
        image::decode(png).map_err(|source| Error::Picture { position, source })?;
        Ok(())
    }
}

/// Apply the XFDF a signing page exported: flatten what the document has when `flatten` is
/// set, then draw the XFDF's pictures, texts and drawings on top, in its order.
///
/// # Errors
///
/// [`Error::Xfdf`] for an XFDF outside the signing page's subset; [`Error::Placement`] or
/// [`Error::Picture`] for an annotation on a page the document does not have, with an empty
/// rectangle or one whose centre is off the page's crop box, or a picture that is not a readable
/// PNG of at most [`MAX_PIXELS`]. Nothing is changed then.
pub fn apply_xfdf(doc: &mut Document, xfdf_text: &str, flatten: bool) -> Result<Applied> {
    apply_xfdf_with(
        doc,
        xfdf_text,
        &ApplyOptions::new().flatten(flatten),
        |_| None,
    )
}

/// [`apply_xfdf`], each picture given the alternate description `describe` says for it (from
/// its field, [`xfdf::Picture::field_id`], the caller knows whose signature it is): PDF/A level
/// a makes it the alternate text of the picture's figure. The XFDF names no one.
///
/// # Errors
///
/// As [`apply_xfdf`].
pub fn apply_xfdf_described(
    doc: &mut Document,
    xfdf_text: &str,
    flatten: bool,
    describe: impl Fn(&xfdf::Picture) -> Option<String>,
) -> Result<Applied> {
    apply_xfdf_with(
        doc,
        xfdf_text,
        &ApplyOptions::new().flatten(flatten),
        describe,
    )
}

/// [`apply_xfdf_described`], what the document already has treated as `options` says: its
/// annotations flattened, kept, or taken out. Several XFDF can be applied one after the other
/// (one per signed field), each drawn over the ones before.
///
/// # Errors
///
/// As [`apply_xfdf`]; nothing is changed then.
pub fn apply_xfdf_with(
    doc: &mut Document,
    xfdf_text: &str,
    options: &ApplyOptions,
    describe: impl Fn(&xfdf::Picture) -> Option<String>,
) -> Result<Applied> {
    let command = xfdf::read_command(xfdf_text)?;
    let programs = options.fonts.read()?;
    let mut budget = Budget::new(doc, command.marks.len())?;
    let pages = doc.page_ids();
    let geometry = doc.geometry();
    let mut prepared = Vec::with_capacity(command.marks.len());
    let mut changed = BTreeSet::new();
    for (index, mark) in command.marks.into_iter().enumerate() {
        doc.check_time()?;
        let position = index + 1;
        let (page_number, rect) = match &mark {
            xfdf::Mark::Picture(picture) => (picture.page_number, picture.rect),
            xfdf::Mark::Text(text) => (text.page_number, text.rect),
            xfdf::Mark::Ink(ink) => (ink.page_number, ink.rect),
        };
        let page = checked_page(&pages, &geometry, page_number, rect, position)?;
        changed.insert(page_number - 1);
        let item = match mark {
            xfdf::Mark::Picture(picture) => {
                budget.picture(&picture.png, position)?;
                let alt = describe(&picture);
                Prepared::Picture {
                    png: picture.png,
                    rect,
                    rotation: picture.rotation,
                    alt,
                }
            }
            xfdf::Mark::Text(text) => Prepared::Text { text },
            xfdf::Mark::Ink(ink) => Prepared::Ink { ink },
        };
        prepared.push((page, item));
    }
    let mut applied = Applied {
        deletions_ignored: command.deleted.len(),
        ..Applied::default()
    };
    if options.flatten || options.remove_annotations {
        changed.extend(flatten::annotated_pages(doc)?);
    }
    if options.remove_annotations {
        applied.annotations_removed = flatten::remove_annotations(doc)?;
    }
    if options.flatten {
        applied.flattened = Some(flatten::flatten(doc)?);
    }
    applied.pages = changed.into_iter().collect();
    let mut by_page: BTreeMap<ObjId, Vec<Placed>> = BTreeMap::new();
    let mut helvetica: Option<ObjId> = None;
    let mut helvetica_bold: Option<ObjId> = None;
    // The programs embedded, by the program: one font per program for this XFDF's texts.
    let mut embedded: Vec<(Arc<sealwax_compose::font::FontProgram>, font::Embedded)> = Vec::new();
    for (page, item) in prepared {
        let placed = match item {
            Prepared::Picture {
                png,
                rect,
                rotation,
                alt,
            } => {
                doc.check_time()?;
                applied.pictures += 1;
                let decoded = image::decode(&png).map_err(|source| Error::Picture {
                    position: applied.pictures,
                    source,
                })?;
                let image = image::add(doc, &decoded)?;
                let (turn, width, height) = turned(rect, rotation);
                let shown = Rect::from_corners(0.0, 0.0, width, height);
                Placed {
                    xobject: image.id,
                    matrix: multiply(contained(shown, image.width, image.height), turn),
                    alt,
                }
            }
            Prepared::Ink { ink } => {
                applied.inks += 1;
                // The strokes in a form of their extent, their width around them.
                let points = || ink.strokes.iter().flatten();
                let half = ink.width / 2.0 + 1.0;
                let left = points().map(|p| p[0]).fold(f64::MAX, f64::min) - half;
                let bottom = points().map(|p| p[1]).fold(f64::MAX, f64::min) - half;
                let width = points().map(|p| p[0]).fold(f64::MIN, f64::max) + half - left;
                let height = points().map(|p| p[1]).fold(f64::MIN, f64::max) + half - bottom;
                let drawing = draw::ink(&ink.strokes, &ink.color, ink.width, [left, bottom]);
                let form =
                    doc.add_object(flatten::form_stream(&drawing, width, height, Dict::new()))?;
                Placed {
                    xobject: form,
                    matrix: [1.0, 0.0, 0.0, 1.0, left, bottom],
                    alt: None,
                }
            }
            Prepared::Text { text } => {
                applied.texts += 1;
                let (turn, width, height) = turned(text.rect, text.rotation);
                let mut fonts = Dict::new();
                let drawing = match programs.for_name(text.font.as_deref()) {
                    Some(program) => {
                        let at = match embedded.iter().position(|(p, _)| Arc::ptr_eq(p, program)) {
                            Some(at) => at,
                            None => {
                                let id = doc.add_object(Object::Null)?;
                                let resource = format!("SwF{}", embedded.len() + 1);
                                embedded.push((
                                    Arc::clone(program),
                                    font::Embedded::new(Arc::clone(program), id, resource),
                                ));
                                embedded.len() - 1
                            }
                        };
                        let font = &embedded[at].1;
                        let (drawing, replaced) = draw::free_text_with(&text, width, height, font);
                        applied.characters_replaced += replaced;
                        fonts.insert(font::TextFont::resource(font), Object::Ref(font.id()));
                        drawing
                    }
                    None if font::HelveticaBold::named(text.font.as_deref()) => {
                        let (drawing, replaced) =
                            draw::free_text_with(&text, width, height, &font::HelveticaBold);
                        applied.characters_replaced += replaced;
                        let font = match helvetica_bold {
                            Some(font) => font,
                            None => *helvetica_bold.insert(
                                doc.add_object(flatten::font_dict(flatten::Font::HelveticaBold))?,
                            ),
                        };
                        fonts.insert("HeBo", Object::Ref(font));
                        drawing
                    }
                    None => {
                        let (drawing, replaced) = draw::free_text(&text, width, height);
                        applied.characters_replaced += replaced;
                        let font = match helvetica {
                            Some(font) => font,
                            None => *helvetica.insert(doc.add_object(flatten::helvetica())?),
                        };
                        fonts.insert("Helv", Object::Ref(font));
                        drawing
                    }
                };
                let mut resources = Dict::new();
                resources.insert("Font", fonts);
                let form =
                    doc.add_object(flatten::form_stream(&drawing, width, height, resources))?;
                Placed {
                    xobject: form,
                    matrix: turn,
                    alt: None,
                }
            }
        };
        by_page.entry(page).or_default().push(placed);
    }
    for (_, font) in &embedded {
        font.write(doc)?;
    }
    let mut handed_down = pdf::HandedDown::default();
    for (page, placed) in &by_page {
        page::draw(doc, *page, placed, &mut handed_down)?;
    }
    Ok(applied)
}

/// Draw each stamp's picture at its rectangle (see [`Stamp`]); returns the pictures drawn. Each
/// picture is contained in its rectangle and centred, never stretched, its transparency kept, and
/// drawn after the page's content, unrotated like its rectangle: the page's `/Rotate` turns it
/// with the page, as the signing page showed it.
///
/// # Errors
///
/// As [`apply_xfdf`] for a stamp it cannot place or a picture it cannot read; nothing is changed
/// then.
pub fn stamp_images(doc: &mut Document, stamps: &[Stamp]) -> Result<usize> {
    let mut budget = Budget::new(doc, stamps.len())?;
    let pages = doc.page_ids();
    let geometry = doc.geometry();
    let mut prepared = Vec::with_capacity(stamps.len());
    for (index, stamp) in stamps.iter().enumerate() {
        let position = index + 1;
        let crop_box = stamp
            .page_number
            .checked_sub(1)
            .and_then(|i| geometry.get(i))
            .map(|page| page.crop_box)
            .ok_or_else(|| Error::Placement {
                position,
                reason: format!(
                    "is on page {}, the PDF has {}",
                    stamp.page_number,
                    geometry.len()
                ),
            })?;
        let values = [stamp.x, stamp.y, stamp.width, stamp.height];
        if !values.iter().all(|v| v.is_finite()) {
            return Err(Error::Placement {
                position,
                reason: "has a value that is not a number".to_owned(),
            });
        }
        let rect = xfdf::to_user_space(stamp.x, stamp.y, stamp.width, stamp.height, crop_box);
        let page = checked_page(&pages, &geometry, stamp.page_number, rect, position)?;
        doc.check_time()?;
        budget.picture(&stamp.png, position)?;
        prepared.push((page, &stamp.png, rect, stamp.alt.clone()));
    }
    let mut by_page: BTreeMap<ObjId, Vec<Placed>> = BTreeMap::new();
    for (index, (page, png, rect, alt)) in prepared.iter().enumerate() {
        doc.check_time()?;
        let decoded = image::decode(png).map_err(|source| Error::Picture {
            position: index + 1,
            source,
        })?;
        let image = image::add(doc, &decoded)?;
        by_page.entry(*page).or_default().push(Placed {
            xobject: image.id,
            matrix: contained(*rect, image.width, image.height),
            alt: alt.clone(),
        });
    }
    let mut handed_down = pdf::HandedDown::default();
    for (page, placed) in &by_page {
        page::draw(doc, *page, placed, &mut handed_down)?;
    }
    Ok(prepared.len())
}

/// Merge documents into a new one, in the given order: each is flattened first, so that two
/// sources' fields of one name never meet and nothing stays editable, then its pages are
/// appended. The new document has their pages only: no outlines, form or catalog of theirs,
/// as a merge that inserts pages into a new document and then flattens them.
///
/// # Errors
///
/// [`Error::Source`] with the position of a source that cannot be flattened or appended.
pub fn merge(sources: Vec<Document>) -> Result<(Document, Vec<Flattened>)> {
    let mut merged = Document::empty()?;
    let mut reports = Vec::with_capacity(sources.len());
    for (index, mut source) in sources.into_iter().enumerate() {
        let failed = |error: Error| Error::Source {
            position: index + 1,
            error: Box::new(error),
        };
        source.check_time().map_err(|e| failed(e.into()))?;
        reports.push(flatten::flatten(&mut source).map_err(|e| failed(e.into()))?);
        merged.append_pages(&source).map_err(|e| failed(e.into()))?;
    }
    Ok((merged, reports))
}

/// Flatten every annotation of the document into its page, but links, and take its form out.
///
/// # Errors
///
/// [`Error::Pdf`] when the document cannot be read.
pub fn flatten(doc: &mut Document) -> Result<Flattened> {
    Ok(flatten::flatten(doc)?)
}

/// Flatten the form fields only: each widget painted into its page with its current appearance
/// (drawn where it has none), the form taken out; every other annotation (comments, highlights,
/// links) stays as it is.
///
/// # Errors
///
/// [`Error::Pdf`] when the document cannot be read.
pub fn flatten_fields(doc: &mut Document) -> Result<Flattened> {
    Ok(flatten::flatten_scope(doc, flatten::Scope::Fields)?)
}

/// The page an annotation goes on, once its rectangle is known to be drawable there: not empty,
/// its centre on the page's crop box (at least half of it shows; a check box the signing page grows
/// a little over the edge is drawn, one off the page is not).
/// The page `page_number` of `pages` (the document's page objects, `geometry` their geometry), if
/// `rect` is on it.
fn checked_page(
    pages: &[Option<ObjId>],
    geometry: &[PageGeometry],
    page_number: usize,
    rect: Rect,
    position: usize,
) -> Result<ObjId> {
    let index = page_number.checked_sub(1).filter(|i| *i < pages.len());
    let (Some(Some(page)), Some(crop)) = (
        index.and_then(|i| pages.get(i)).copied(),
        index.and_then(|i| geometry.get(i)).map(|g| g.crop_box),
    ) else {
        return Err(Error::Placement {
            position,
            reason: format!("is on page {page_number}, the PDF has {}", pages.len()),
        });
    };
    if !(rect.x2 > rect.x1 && rect.y2 > rect.y1) {
        return Err(Error::Placement {
            position,
            reason: "has an empty rectangle".to_owned(),
        });
    }
    let (centre_x, centre_y) = ((rect.x1 + rect.x2) / 2.0, (rect.y1 + rect.y2) / 2.0);
    if !(crop.x1 <= centre_x && centre_x <= crop.x2 && crop.y1 <= centre_y && centre_y <= crop.y2) {
        return Err(Error::Placement {
            position,
            reason: "is outside its page".to_owned(),
        });
    }
    Ok(page)
}

/// The matrix that draws a `width` x `height` picture contained in `rect` and centred, never
/// stretched.
/// What draws a mark turned `rotation` degrees counterclockwise in `rect`: the matrix from the
/// mark's own space (origin at its bottom-left corner as shown) to user space, and the mark's
/// width and height as shown (`rect`'s sides swapped by a quarter turn).
fn turned(rect: Rect, rotation: u16) -> ([f64; 6], f64, f64) {
    let (width, height) = (rect.x2 - rect.x1, rect.y2 - rect.y1);
    match rotation {
        90 => ([0.0, 1.0, -1.0, 0.0, rect.x2, rect.y1], height, width),
        180 => ([-1.0, 0.0, 0.0, -1.0, rect.x2, rect.y2], width, height),
        270 => ([0.0, -1.0, 1.0, 0.0, rect.x1, rect.y2], height, width),
        _ => ([1.0, 0.0, 0.0, 1.0, rect.x1, rect.y1], width, height),
    }
}

/// `first` then `then`, as `cm` operators in that order compose.
fn multiply(first: [f64; 6], then: [f64; 6]) -> [f64; 6] {
    let [a, b, c, d, e, f] = first;
    let [a2, b2, c2, d2, e2, f2] = then;
    [
        a * a2 + b * c2,
        a * b2 + b * d2,
        c * a2 + d * c2,
        c * b2 + d * d2,
        e * a2 + f * c2 + e2,
        e * b2 + f * d2 + f2,
    ]
}

fn contained(rect: Rect, width: u32, height: u32) -> [f64; 6] {
    let (w, h) = (f64::from(width), f64::from(height));
    let scale = ((rect.x2 - rect.x1) / w).min((rect.y2 - rect.y1) / h);
    let (drawn_w, drawn_h) = (w * scale, h * scale);
    let left = rect.x1 + (rect.x2 - rect.x1 - drawn_w) / 2.0;
    let bottom = rect.y1 + (rect.y2 - rect.y1 - drawn_h) / 2.0;
    [drawn_w, 0.0, 0.0, drawn_h, left, bottom]
}
