//! Other formats converted to PDF: laid out by an engine of their own, written by
//! [`sealwax_compose`]. DOCX first, laid out by [docxide-pdf](https://github.com/sverrejb/docxide-pdf).
//!
//! The engine hands over laid-out pages (docxide's [`display`] module), with the marked content
//! and structure that tag them and the bookmarks of the headings; this crate translates them
//! into a [`sealwax_compose::Composition`] and composes it. Font files the layout names on disk
//! are read here, at the boundary.
//!
//! What [`Limits`] bound: the deadline, checked before the layout and as pages are composed; what
//! the document unpacks to ([`Limits::max_unpacked_bytes`]), checked before the layout; and what
//! [`sealwax_compose::compose`] bounds. The layout itself is docxide-pdf's, and its time is not
//! bounded: convert documents you do not trust in a process you can stop. A panic in
//! docxide-pdf is caught as a document it cannot lay out, unless the build aborts on panic.
//!
//! Not published on crates.io yet.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use docxide_pdf::display;
use sealwax_compose::Composed;
use sealwax_core::Limits;

mod translate;
pub use translate::{Translated, composition};

pub use docxide_pdf::ConvertOptions as DocxOptions;

/// What a conversion can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The layout engine refused the document, or failed on it.
    #[error("the document could not be laid out")]
    Layout,
    /// The deadline passed, or a bound of [`Limits`]: what the document unpacks to before the
    /// layout, the others while composing.
    #[error(transparent)]
    Compose(#[from] sealwax_compose::Error),
}

/// A converted document and what the conversion left out.
#[derive(Debug)]
#[non_exhaustive]
pub struct Converted {
    /// The document, with what [`sealwax_compose::compose`] left out.
    pub composed: Composed,
    /// What the layout drew that the PDF lacks, by kind, with how many times: see
    /// [`Translated::not_described`].
    pub not_described: BTreeMap<&'static str, usize>,
}

/// A DOCX document as a PDF document.
///
/// # Errors
/// [`Error::Layout`] when docxide-pdf refuses the document or fails on it; [`Error::Compose`]
/// when the deadline passes or a bound of `limits` is passed.
pub fn docx_to_pdf(
    docx: &[u8],
    options: &DocxOptions,
    limits: &Limits,
) -> Result<Converted, Error> {
    let translated = composition(&layout_docx(docx, options, limits)?);
    let composed = sealwax_compose::compose(&translated.composition, limits)?;
    Ok(Converted {
        composed,
        not_described: translated.not_described,
    })
}

/// A DOCX document laid out by docxide-pdf, its pages not yet composed. Of `limits`, the
/// deadline and what the document unpacks to are checked, before the layout.
///
/// # Errors
/// [`Error::Layout`] when the document is not a ZIP archive, or docxide-pdf refuses it or panics
/// on it; [`Error::Compose`] when the deadline has passed or the document unpacks to more than
/// [`Limits::max_unpacked_bytes`].
pub fn layout_docx(
    docx: &[u8],
    options: &DocxOptions,
    limits: &Limits,
) -> Result<display::Document, Error> {
    limits.check_time().map_err(sealwax_compose::Error::from)?;
    check_unpacked(docx, limits)?;
    let options = options.clone().display_list(true);
    // docxide-pdf does not promise not to panic on a hostile document. What it records is kept
    // per thread and put back as a panic unwinds, so a layout that panicked leaves nothing.
    catch_unwind(AssertUnwindSafe(|| docxide_pdf::convert(docx, &options)))
        .map_err(|_| Error::Layout)?
        .map_err(|_| Error::Layout)?
        .display
        .ok_or(Error::Layout)
}

/// Check that `docx` unpacks to at most [`Limits::max_unpacked_bytes`], its parts at the sizes
/// its central directory declares. That is all the layout can unpack: the zip crate stops a
/// part at its declared size, without reading the archive's compressed data here.
fn check_unpacked(docx: &[u8], limits: &Limits) -> Result<(), Error> {
    let archive = zip::ZipArchive::new(std::io::Cursor::new(docx)).map_err(|_| Error::Layout)?;
    let parts = archive.metadata();
    let mut unpacked: u64 = 0;
    for index in 0..parts.len() {
        let part = parts.entry(index).map_err(|_| Error::Layout)?;
        unpacked = unpacked.saturating_add(part.size());
        if unpacked > limits.max_unpacked_bytes {
            return Err(
                sealwax_compose::Error::from(sealwax_core::Error::LimitExceeded {
                    what: "max_unpacked_bytes",
                })
                .into(),
            );
        }
    }
    Ok(())
}
