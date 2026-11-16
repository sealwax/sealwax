//! Conversion of PDF documents to PDF/A-3 (ISO 19005-3), the archival format.
//!
//! [`convert`] changes a [`Document`] in place; saving it in full ([`Document::save`]) then
//! writes the PDF/A file. The conversion intervenes as little as it can: what is already
//! compliant is left as it
//! is, and what it cannot repair yet is counted in the [`Report`] rather than hidden.
//!
//! Done so far:
//! - the output intent (sRGB), and default colour spaces where the content uses a device space
//!   the intent does not cover (CMYK);
//! - the XMP metadata, with the PDF/A identification;
//! - entries PDF/A forbids or requires: annotation flags and appearances, actions, JavaScript,
//!   optional content configurations, `CIDSet` and `CharSet`, `CIDToGIDMap`, image
//!   interpolation, object-level XMP, overprint mode, DeviceN process spaces;
//! - the programs of simple fonts that have none: the standard font that stands in for each,
//!   with the dictionary's widths and a `ToUnicode` CMap;
//! - the widths of programs their producer embedded, where they disagree with the dictionary:
//!   TrueType programs of composite fonts, CFF and Type 1 programs of simple fonts; a simple
//!   font without widths gets those of its program, and a Type 1 program in the PFB format is
//!   written without its segment headers;
//! - from level u on, a Unicode value for every glyph shown: from the font's
//!   glyph names and program, the character collection, the drawings of the other fonts of the
//!   document, the code as PDFium reads it, U+FFFD last;
//! - at level a, no Unicode value in the Private Use Area (the characters of Symbol, Webdings
//!   and Wingdings instead), the document's language, and a structure tree: for a
//!   document without one, every piece of content tied to a paragraph or a figure, in reading
//!   order, or marked as an artifact (level N1, `tagging`); a tree the document has is kept,
//!   its role map repaired (level N0), and what is drawn outside it tied to it.
//!
//! Not yet: the program of a composite font that has none (the report counts it as
//! `font_not_embedded`).
//!
//! ```no_run
//! use sealwax_core::{Document, OpenOptions, SaveOptions};
//! use sealwax_pdfa::{Level, Options, convert};
//!
//! let mut doc = Document::open(std::fs::read("in.pdf")?, &OpenOptions::default())?;
//! let report = convert(&mut doc, &Options::new(Level::A).language("fr-CA"))?;
//! std::fs::write("out.pdf", doc.save(&SaveOptions::default())?)?;
//! eprintln!("{:?}", report.residual());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod attachments;
mod color;
mod edit;
mod fonts;
mod inline;
mod language;
mod metadata;
mod repair;
mod scan;
mod structure;
mod sweep;
mod tagging;

use std::collections::BTreeMap;

use sealwax_core::{Document, Error, Result};

/// The PDF/A-3 conformance level the document declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Level {
    /// Level b: the visual appearance is preserved.
    B,
    /// Level u: level b, and every glyph maps to Unicode.
    U,
    /// Level a: level u, and the document is tagged.
    A,
}

impl Level {
    /// The value of `pdfaid:conformance`.
    pub fn conformance(self) -> &'static str {
        match self {
            Self::B => "B",
            Self::U => "U",
            Self::A => "A",
        }
    }
}

/// How to convert.
#[derive(Debug, Clone)]
pub struct Options {
    level: Level,
    language: Option<String>,
}

impl Options {
    /// Convert to `level`.
    pub fn new(level: Level) -> Self {
        Self {
            level,
            language: None,
        }
    }

    /// The document's language, as a language tag (`fr-CA`, `en`): at level a, the catalog's
    /// `/Lang` when the document declares none. Without it, the language is told
    /// from the document's words when they leave no doubt.
    pub fn language(mut self, tag: impl Into<String>) -> Self {
        self.language = Some(tag.into());
        self
    }
}

/// What a conversion changed, and what it left that keeps the document from conforming.
///
/// Counts only, by kind: never content of the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Report {
    changes: BTreeMap<String, usize>,
    residual: BTreeMap<String, usize>,
}

impl Report {
    /// The changes made, counted by kind.
    pub fn changes(&self) -> &BTreeMap<String, usize> {
        &self.changes
    }

    /// What is left that the conversion could not repair, counted by kind. Empty when it
    /// expects the output to conform.
    pub fn residual(&self) -> &BTreeMap<String, usize> {
        &self.residual
    }

    pub(crate) fn change(&mut self, what: &str, count: usize) {
        if count > 0 {
            *self.changes.entry(what.to_owned()).or_default() += count;
        }
    }

    pub(crate) fn residual_add(&mut self, what: &str, count: usize) {
        if count > 0 {
            *self.residual.entry(what.to_owned()).or_default() += count;
        }
    }
}

/// Convert `doc` to PDF/A-3 as `options` say, in place.
///
/// # Errors
/// - [`Error::InvalidArgument`] when the language given is no language tag; the document is
///   left as it was.
/// - [`Error::Unsupported`] when the document was changed since it was opened
///   ([`Document::is_modified`]): the conversion reads its pages as opened, and would miss what
///   was drawn since. Save it and open it again first.
/// - [`Error::LimitExceeded`] when an object nests deeper than sealwax accepts, or when
///   interpreting a page would pass the document's [`sealwax_core::Limits`]
///   ([`Document::check_pages`]); [`Error::Timeout`] past their deadline.
pub fn convert(doc: &mut Document, options: &Options) -> Result<Report> {
    if doc.is_modified() {
        return Err(Error::Unsupported {
            what: "converting a document changed since it was opened (save and open it again)",
        });
    }
    let level = options.level;
    if let Some(tag) = &options.language
        && (tag.is_empty() || !language::valid(tag))
    {
        return Err(Error::InvalidArgument {
            what: "language tag",
        });
    }
    let mut report = Report::default();
    // The conversion reads every content stream, and tagging interprets every page.
    doc.check_pages()?;
    let scan = scan::scan(doc);
    doc.check_time()?;
    repair::materialize_resources(doc, &scan.inherited_resources)?;
    let mut profiles = color::Profiles::default();
    let intent = color::apply(doc, &scan, &mut profiles, &mut report)?;
    doc.check_time()?;
    fonts::apply(doc, &scan, &mut report)?;
    doc.check_time()?;
    if level != Level::B {
        let meanings = fonts::unicode::apply(doc, &scan, level, &mut report)?;
        doc.check_time()?;
        if level == Level::A {
            let languages = structure::Languages {
                given: options.language.as_deref(),
                detect: || language::detect(&language::sample(&scan, &meanings)),
            };
            tagging::apply(doc, &mut report)?;
            structure::apply(doc, languages, &mut report)?;
        }
    }
    doc.check_time()?;
    repair::apply(doc, &scan, &mut report)?;
    attachments::apply(doc, &scan, &mut report)?;
    metadata::apply(doc, level, &mut report)?;
    sweep::apply(doc, intent, &mut profiles, &mut report)?;
    report.residual_add("content_undecodable", scan.undecodable);
    report.residual_add("content_too_deep", scan.too_deep);
    Ok(report)
}
