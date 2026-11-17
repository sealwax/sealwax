//! Text drawn by the content of PDF pages, glyph by glyph.
//!
//! PDF/A levels a and u require that every glyph drawn maps to Unicode (ISO 19005-3, clause
//! 6.2.11.7.2), and that a glyph mapped to the Private Use Area is covered by an `/ActualText`
//! (6.2.11.7.3). Those two rules are what most often keeps a converted document from PDF/A-3a.
//! The census
//! here measures them on any document, without a validator, by interpreting every page with
//! hayro-interpret: fonts, encodings, `ToUnicode` and predefined CMaps, Type 3 glyphs, annotation
//! appearances, invisible text.
//!
//! No text from the document is kept: only counts, and font names.

mod actual;
mod cache;
mod drawings;
mod drawn;
mod glyphs;
mod redact;
mod scan;

pub use cache::release_cache;
pub use drawings::{Drawings, drawings};
pub use drawn::{Bounds, Drawn, DrawnGlyph, drawn};
pub use glyphs::{TextGlyph, glyphs};
pub use redact::{ImageRedaction, PathRedaction, RedactOptions, Redacted, redact};

use std::collections::{BTreeSet, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hayro_interpret::font::{Glyph, GlyphRun};
use hayro_interpret::hayro_cmap::BfString;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::hayro_syntax::object::{Array, Dict, MaybeRef};
use hayro_interpret::kurbo::{Affine, BezPath, Rect};
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, InterpreterWarning, RenderContent, SoftMask,
    interpret_page,
};
use sealwax_core::Document;

/// What the text of a document maps to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TextCensus {
    /// Pages interpreted.
    pub pages: usize,
    /// Pages that draw at least one glyph.
    pub pages_with_text: usize,
    /// Pages whose glyphs are all invisible (render mode 3): an OCR layer over a scan, usually.
    pub pages_only_invisible: usize,
    /// Glyphs drawn, page content and annotation appearances.
    pub glyphs: u64,
    /// Of those, invisible.
    pub invisible: u64,
    /// Glyphs no Unicode value is found for (clause 6.2.11.7.2).
    pub without_unicode: u64,
    /// Glyphs mapped to U+0000, U+FEFF or U+FFFE, which the same clause forbids.
    pub invalid_unicode: u64,
    /// Glyphs mapped to the Private Use Area.
    pub private_use: u64,
    /// Of those, covered by an `/ActualText` (clause 6.2.11.7.3 is then met).
    pub private_use_with_actual_text: u64,
    /// Glyphs inside an `/ActualText` marked-content sequence.
    pub in_actual_text: u64,
    /// Glyphs drawn by annotation appearances (form fields, stamps, free text), included above.
    pub annotation_glyphs: u64,
    /// Of those, without a valid Unicode value.
    pub annotation_without_unicode: u64,
    /// Of those, in the Private Use Area without `/ActualText`.
    pub annotation_private_use_uncovered: u64,
    /// Fonts hayro could not load: their glyphs are not counted.
    pub unsupported_fonts: usize,
    /// Pages whose interpretation stopped on an internal error: their glyphs are partly counted.
    pub failed_pages: usize,
    /// Per font, those with a problem first.
    pub fonts: Vec<FontText>,
}

impl TextCensus {
    /// Some glyph has no valid Unicode value: clause 6.2.11.7.2 fails.
    pub fn fails_unicode_mapping(&self) -> bool {
        self.without_unicode + self.invalid_unicode > 0
    }

    /// Some Private Use Area glyph has no `/ActualText`: clause 6.2.11.7.3 fails.
    pub fn fails_private_use(&self) -> bool {
        self.private_use > self.private_use_with_actual_text
    }
}

/// The glyphs drawn with one font.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FontText {
    /// PostScript name of the font program, `Type3`, or `unknown`.
    pub name: String,
    /// Glyphs drawn.
    pub glyphs: u64,
    /// Without a Unicode value.
    pub without_unicode: u64,
    /// Mapped to U+0000, U+FEFF or U+FFFE.
    pub invalid_unicode: u64,
    /// Mapped to the Private Use Area.
    pub private_use: u64,
    /// Mapped to the Private Use Area and covered by an `/ActualText`.
    pub private_use_with_actual_text: u64,
    /// The first character codes without a valid Unicode value or in the Private Use Area
    /// (codes, not text: enough to tell a bullet from a checkmark).
    pub failing_codes: BTreeSet<u32>,
}

/// How many failing character codes a font keeps.
const MAX_FAILING_CODES: usize = 16;

impl FontText {
    fn problems(&self) -> u64 {
        self.without_unicode
            + self.invalid_unicode
            + (self.private_use - self.private_use_with_actual_text)
    }
}

/// Interpret every page of `doc` and count what its glyphs map to.
///
/// # Errors
///
/// When the pages pass [`Document::limits`] ([`Document::check_pages`], checked first), or the
/// deadline passes while they are interpreted.
pub fn text_census(doc: &Document) -> sealwax_core::Result<TextCensus> {
    doc.check_pages()?;
    let pdf = doc.hayro();
    let unsupported = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&unsupported);
    let settings = InterpreterSettings {
        warning_sink: Arc::new(move |warning| {
            if matches!(warning, InterpreterWarning::UnsupportedFont) {
                sink.fetch_add(1, Ordering::Relaxed);
            }
        }),
        ..InterpreterSettings::default()
    };
    let cache = InterpreterCache::new();
    let mut census = TextCensus::default();
    let mut fonts: HashMap<u128, FontText> = HashMap::new();
    // Nothing is culled: a validator checks every glyph, drawn on the page or not.
    let everywhere = Rect::new(-1e9, -1e9, 1e9, 1e9);
    for page in pdf.pages().iter() {
        doc.check_time()?;
        census.pages += 1;
        let mut counter = Counter {
            covered_mcids: covered_mcids(pdf, page.raw()),
            ..Counter::default()
        };
        // The page content, then the annotations, to tell apart what the document's author drew
        // from what forms and stamps added.
        let mut failed = false;
        for (content, in_annotation) in [
            (RenderContent::PAGE, false),
            (RenderContent::ANNOTATIONS, true),
        ] {
            counter.in_annotation = in_annotation;
            counter.actual_text.clear();
            let settings = InterpreterSettings {
                render_content: content,
                ..settings.clone()
            };
            let interpreted = catch_unwind(AssertUnwindSafe(|| {
                let mut context =
                    Context::new(Affine::IDENTITY, everywhere, &cache, pdf.xref(), settings);
                interpret_page(page, &mut context, &mut counter);
            }));
            failed |= interpreted.is_err();
        }
        if failed {
            census.failed_pages += 1;
        }
        census.annotation_glyphs += counter.annotation_glyphs;
        census.annotation_without_unicode += counter.annotation_without_unicode;
        census.annotation_private_use_uncovered += counter.annotation_private_use_uncovered;
        let page_glyphs = counter.visible + counter.invisible;
        if page_glyphs > 0 {
            census.pages_with_text += 1;
            if counter.visible == 0 {
                census.pages_only_invisible += 1;
            }
        }
        census.glyphs += page_glyphs;
        census.invisible += counter.invisible;
        census.in_actual_text += counter.in_actual_text;
        for (key, font) in counter.fonts {
            let total = fonts.entry(key).or_insert_with(|| FontText {
                name: font.name.clone(),
                ..FontText::default()
            });
            total.glyphs += font.glyphs;
            total.without_unicode += font.without_unicode;
            total.invalid_unicode += font.invalid_unicode;
            total.private_use += font.private_use;
            total.private_use_with_actual_text += font.private_use_with_actual_text;
            for code in font.failing_codes {
                if total.failing_codes.len() < MAX_FAILING_CODES {
                    total.failing_codes.insert(code);
                }
            }
        }
    }
    let mut fonts: Vec<FontText> = fonts.into_values().collect();
    for font in &fonts {
        census.without_unicode += font.without_unicode;
        census.invalid_unicode += font.invalid_unicode;
        census.private_use += font.private_use;
        census.private_use_with_actual_text += font.private_use_with_actual_text;
    }
    fonts.sort_by(|a, b| {
        b.problems()
            .cmp(&a.problems())
            .then(b.glyphs.cmp(&a.glyphs))
            .then(a.name.cmp(&b.name))
    });
    census.fonts = fonts;
    census.unsupported_fonts = unsupported.load(Ordering::Relaxed);
    Ok(census)
}

/// How deep the structure tree and its parent tree are walked.
const MAX_TREE_DEPTH: usize = 64;

/// The marked-content identifiers of `page` covered by an `/ActualText` of the structure tree:
/// clause 6.2.11.7.3 accepts it there as well as on the marked content itself. The page's
/// `/StructParents` leads, through the parent tree, to the structure element of each MCID.
fn covered_mcids(pdf: &Pdf, page: &Dict<'_>) -> HashSet<i64> {
    let mut covered = HashSet::new();
    let Some(key) = page.get::<i64>(b"StructParents") else {
        return covered;
    };
    let Some(parent_tree) = pdf
        .xref()
        .get::<Dict<'_>>(pdf.xref().root_id())
        .and_then(|catalog| catalog.get::<Dict<'_>>(b"StructTreeRoot"))
        .and_then(|root| root.get::<Dict<'_>>(b"ParentTree"))
    else {
        return covered;
    };
    let Some(elements) = number_tree_get(&parent_tree, key, 0) else {
        return covered;
    };
    // Entries can be null (an MCID without element), so each one is resolved on its own.
    for (mcid, entry) in (0_i64..).zip(elements.raw_iter()) {
        let element = match entry {
            MaybeRef::Ref(reference) => pdf.xref().get::<Dict<'_>>(reference.into()),
            MaybeRef::NotRef(object) => object.into_dict(),
        };
        if element.is_some_and(|element| has_actual_text(&element)) {
            covered.insert(mcid);
        }
    }
    covered
}

/// The structure element or one of its ancestors carries an `/ActualText`.
fn has_actual_text(element: &Dict<'_>) -> bool {
    let mut current = Some(element.clone());
    for _ in 0..MAX_TREE_DEPTH {
        let Some(dict) = current else {
            return false;
        };
        if dict.contains_key(b"ActualText") {
            return true;
        }
        current = dict.get::<Dict<'_>>(b"P");
    }
    false
}

/// The array stored under `key` in a number tree (ISO 32000-1, 7.9.7).
fn number_tree_get<'a>(node: &Dict<'a>, key: i64, depth: usize) -> Option<Array<'a>> {
    if depth > MAX_TREE_DEPTH {
        return None;
    }
    if let Some(nums) = node.get::<Array<'a>>(b"Nums") {
        let mut items = nums.flex_iter();
        while let Some(found) = items.next::<i64>() {
            match items.next::<Array<'a>>() {
                Some(value) if found == key => return Some(value),
                _ => {}
            }
        }
    }
    let kids = node.get::<Array<'a>>(b"Kids")?;
    kids.iter::<Dict<'a>>().find_map(|kid| {
        let in_range = kid
            .get::<Array<'_>>(b"Limits")
            .map(|limits| {
                let mut bounds = limits.iter::<i64>();
                match (bounds.next(), bounds.next()) {
                    (Some(low), Some(high)) => (low..=high).contains(&key),
                    _ => true,
                }
            })
            .unwrap_or(true);
        if in_range {
            number_tree_get(&kid, key, depth + 1)
        } else {
            None
        }
    })
}

/// What one Unicode value means for PDF/A.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mapping {
    Missing,
    Invalid,
    PrivateUse,
    Fine,
}

fn mapping(unicode: Option<BfString>) -> Mapping {
    let check = |c: char| match c {
        '\u{0}' | '\u{FEFF}' | '\u{FFFE}' => Mapping::Invalid,
        '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}' => {
            Mapping::PrivateUse
        }
        _ => Mapping::Fine,
    };
    match unicode {
        None => Mapping::Missing,
        Some(BfString::Char(c)) => check(c),
        Some(BfString::String(text)) if text.is_empty() => Mapping::Missing,
        Some(BfString::String(text)) => text
            .chars()
            .map(check)
            .find(|m| *m != Mapping::Fine)
            .unwrap_or(Mapping::Fine),
    }
}

/// Counts the glyphs of one page.
#[derive(Default)]
struct Counter {
    visible: u64,
    invisible: u64,
    in_actual_text: u64,
    /// One entry per open marked-content sequence: whether it carries an `/ActualText`.
    actual_text: Vec<bool>,
    /// Glyphs come from annotation appearances.
    in_annotation: bool,
    /// Marked-content identifiers of the page whose structure element, or an ancestor of it,
    /// carries an `/ActualText`.
    covered_mcids: HashSet<i64>,
    annotation_glyphs: u64,
    annotation_without_unicode: u64,
    annotation_private_use_uncovered: u64,
    fonts: HashMap<u128, FontText>,
}

/// Keys of the glyphs that do not come from an outline font program.
const TYPE3_KEY: u128 = 1;
const UNKNOWN_KEY: u128 = 0;

impl<'a> Device<'a> for Counter {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, _: DrawProps<'a>, mode: &DrawMode) {
        let glyphs = run.glyphs();
        let Some(first) = glyphs.first() else {
            return;
        };
        // A run is drawn with one font: identify it once.
        let (key, name) = match &**first {
            Glyph::Outline(outline) => match outline.font_data() {
                Some(data) => (
                    data.cache_key.max(2),
                    data.postscript_name.unwrap_or_else(|| "unnamed".to_owned()),
                ),
                None => (UNKNOWN_KEY, "unknown".to_owned()),
            },
            Glyph::Type3(_) => (TYPE3_KEY, "Type3".to_owned()),
        };
        let covered = self.actual_text.iter().any(|has| *has);
        let count = glyphs.len() as u64;
        if matches!(mode, DrawMode::Invisible) {
            self.invisible += count;
        } else {
            self.visible += count;
        }
        if covered {
            self.in_actual_text += count;
        }
        let font = self.fonts.entry(key).or_insert_with(|| FontText {
            name,
            ..FontText::default()
        });
        font.glyphs += count;
        if self.in_annotation {
            self.annotation_glyphs += count;
        }
        for glyph in glyphs {
            let found = mapping(glyph.as_unicode());
            match found {
                Mapping::Missing => font.without_unicode += 1,
                Mapping::Invalid => font.invalid_unicode += 1,
                Mapping::PrivateUse => {
                    font.private_use += 1;
                    if covered {
                        font.private_use_with_actual_text += 1;
                    }
                }
                Mapping::Fine => {}
            }
            let fails = matches!(found, Mapping::Missing | Mapping::Invalid)
                || (found == Mapping::PrivateUse && !covered);
            if fails && font.failing_codes.len() < MAX_FAILING_CODES {
                font.failing_codes.insert(glyph.char_code());
            }
            if self.in_annotation {
                match found {
                    Mapping::Missing | Mapping::Invalid => self.annotation_without_unicode += 1,
                    Mapping::PrivateUse if !covered => self.annotation_private_use_uncovered += 1,
                    _ => {}
                }
            }
        }
    }

    fn begin_marked_content(&mut self, _tag: &[u8], properties: Option<&Dict<'_>>) {
        let covered = properties.is_some_and(|p| {
            p.contains_key(b"ActualText")
                || p.get::<i64>(b"MCID")
                    .is_some_and(|mcid| self.covered_mcids.contains(&mcid))
        });
        self.actual_text.push(covered);
    }

    fn end_marked_content(&mut self) {
        self.actual_text.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_values_are_classified_as_pdf_a_reads_them() {
        assert_eq!(mapping(None), Mapping::Missing);
        assert_eq!(
            mapping(Some(BfString::String(String::new()))),
            Mapping::Missing
        );
        assert_eq!(mapping(Some(BfString::Char('A'))), Mapping::Fine);
        assert_eq!(mapping(Some(BfString::Char('\u{FEFF}'))), Mapping::Invalid);
        assert_eq!(
            mapping(Some(BfString::Char('\u{F0B7}'))),
            Mapping::PrivateUse
        );
        assert_eq!(
            mapping(Some(BfString::String("ﬁ".to_owned()))),
            Mapping::Fine
        );
        assert_eq!(
            mapping(Some(BfString::String("a\u{E001}".to_owned()))),
            Mapping::PrivateUse
        );
    }
}
