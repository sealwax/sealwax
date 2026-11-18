//! The glyphs a page draws, in the order of its content: what each means, where it is, how large,
//! in which font. [`crate::words`], [`crate::text_blocks`] and [`crate::search`] consume the same
//! stream with its transformed metric corners.
//!
//! Interpreting the content with hayro gives each glyph's position, size and meaning, not the font
//! it was drawn with. So the content is first read operator by operator, following `q`/`Q` and
//! `Tf`, and each text-showing operator is enclosed in a marked-content sequence naming its font:
//! the interpreter hands the sequence to the device with the glyphs. A form XObject is enclosed in
//! one too, naming its font when it draws with one only (or inherits the one in effect), and its
//! fonts when it draws with several: a glyph drawn there gets the one whose program draws it. The
//! sequence around a form also carries its `/StructParents`, so that the `/MCID`s inside are
//! looked up where the form's structure is (see [`crate::actual`]). Annotations' appearances are
//! read the same way, each placed on the page as hayro places it.
//!
//! The font gives the box of its glyphs (see [`crate::metrics`]), and the size of a glyph is
//! MuPDF's: that of its em, stretched or squeezed, as a square. hayro stretches the glyphs of the
//! font it draws a font not embedded with to that font's widths: the em is taken before.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_font::{FontCache, FontGlyph, GlyphIdentifier};
use hayro_interpret::font::{Glyph as HayroGlyph, GlyphRun};
use hayro_interpret::hayro_cmap::{BfString, WritingMode};
use hayro_interpret::hayro_syntax::content::TypedIter;
use hayro_interpret::hayro_syntax::object::{
    Array, Dict, Name as HayroName, ObjectIdentifier, Rect as PdfRect, Stream,
};
use hayro_interpret::hayro_syntax::page::Resources;
use hayro_interpret::hayro_syntax::xref::XRef;
use hayro_interpret::kurbo::{Affine, BezPath, Point, Rect, Shape as _, Vec2};
use hayro_interpret::{
    BlendMode, CacheKey as _, ClipPath, Context, Device, DrawMode, DrawProps, Image,
    ImageDrawProps, InterpreterCache, InterpreterSettings, SoftMask, interpret,
};
use sealwax_core::Document;

use crate::actual::{Key, Replacement, Structure};
use crate::cache::with_cache;
use crate::drawn::Bounds;
use crate::metrics::{self, Metrics};
use crate::scan::{name_operand, operators};

/// The units per em of glyph space in hayro's glyph transforms.
const UNITS_PER_EM: f64 = 1000.0;
/// The tag of the sequences naming fonts.
const TAG: &[u8] = b"SwFont";
/// Annotation flags: hidden, not to be viewed.
const HIDDEN: u32 = 2;
const NO_VIEW: u32 = 32;
/// How close to the glyph shown before it, in em, a glyph with the same text repeats it.
const OVERDRAWN: f64 = 0.1;
/// How deep forms inside forms are looked into for their fonts.
const MAX_FORM_DEPTH: usize = 8;

/// A glyph a page draws, in the page's default user space (unrotated, y up).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TextGlyph {
    /// What it means (`ToUnicode`, its encoding or its name), a ligature's letters included.
    pub text: Option<String>,
    /// Its writing position. For vertical CID fonts this is the vertical pen position before
    /// the font displaces the outline to its horizontal glyph origin.
    pub origin: [f64; 2],
    /// Where the next glyph's origin would land without spacing (its advance): between two
    /// glyphs, the gap a word space or a `TJ` adjustment leaves.
    pub end: [f64; 2],
    /// For horizontal writing, the box from its origin to its end, its font's descent to its ascent, at least an
    /// em tall, as MuPDF reads them: the font descriptor's, else the font's TrueType program's,
    /// else those of the font a standard font is drawn with, else 0.2 and 0.8 em (see
    /// `metrics.rs` for the whole of it). Vertical boxes span the writing advance and the glyph cell's width.
    pub bbox: Bounds,
    /// The box of its outline, when it has one (a space has none, nor a Type 3 glyph here).
    pub ink: Option<Bounds>,
    /// The size it is drawn at, as MuPDF gives it: the side of a square as large as an em
    /// square after the text and the current transformation matrices (zero when they flatten
    /// it). An em stretched or squeezed along one axis (`Tz`, a text matrix `0.8 0 0 1`) is
    /// drawn at less than the font size.
    pub size: f64,
    /// The direction of writing as a unit vector, including top-to-bottom vertical CID text.
    pub direction: [f64; 2],
    /// The font's `/BaseFont` as written (subset prefix included), when known.
    pub font: Option<String>,
    /// Drawn in an invisible render mode: the text layer over a scan, usually.
    pub invisible: bool,
    /// Repeats the glyph shown before it: the same text, its origin less than a tenth of an em
    /// away. Text drawn twice to look bold, leaders drawn over one another: a reader shows it
    /// once, and the text of a page reads as it does without the glyphs so marked.
    pub overdrawn: bool,
    /// What the document says it reads as instead, through an `/ActualText` around it (in a
    /// marked-content sequence, or on the structure element the content is tied to): the whole
    /// text on the first glyph it covers, an empty string on the others. Leaders drawn for a tab,
    /// letters drawn as shapes: a reader reads this rather than `text`.
    pub actual_text: Option<String>,
    /// Drawn by an annotation's appearance (a filled field's value, a stamp, a comment's text
    /// box) rather than by the page's content: only when annotations are asked for.
    pub annotation: bool,
}

/// The glyphs page `page_index` (from 0) draws, in the order of its content, forms and Type 3
/// glyphs included; then, with `annotations`, those its annotations' appearances draw (shown
/// ones, in the order of `/Annots`), as a viewer shows them and as MuPDF's text includes them.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have; what
/// [`Document::check_page`] refuses (the limits, the deadline); [`sealwax_core::Error::Timeout`]
/// when the deadline passes while the page is read.
pub fn glyphs(
    doc: &Document,
    page_index: usize,
    annotations: bool,
) -> sealwax_core::Result<Vec<TextGlyph>> {
    let mut found = Vec::new();
    visit_glyphs(doc, page_index, annotations, true, &mut |glyph, _, _| {
        found.push(glyph);
        Ok(())
    })?;
    Ok(found)
}

type Visitor<'a> = dyn FnMut(TextGlyph, [Point; 4], Option<&Key>) -> sealwax_core::Result<()> + 'a;

/// Visit each glyph with its transformed metric corners and the identity of its replacement.
/// Layout only needs the metrics, so it skips loading outlines and collecting font names.
pub(crate) fn visit_glyphs(
    doc: &Document,
    page_index: usize,
    annotations: bool,
    details: bool,
    visitor: &mut Visitor<'_>,
) -> sealwax_core::Result<()> {
    if page_index >= doc.hayro().pages().len() {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
    }
    doc.check_page(page_index)?;
    with_cache(doc, |pdf, cache| {
        let pages = pdf.pages();
        let Some(page) = pages.get(page_index) else {
            return Ok(());
        };
        let content = page.page_stream().unwrap_or_default();
        let resources = page.resources();
        let reader = FontReader::new();
        let (instrumented, fonts) = instrument(content, resources, &reader);
        let mut collector = Collector {
            fonts,
            open: Vec::new(),
            visitor,
            details,
            error: None,
            last_run: None,
            shown: None,
            structure: Structure::new(pdf.xref()),
            page_parents: page.raw().get::<i64>(b"StructParents"),
            replaced: HashSet::new(),
            annotation: false,
            programs: HashMap::new(),
            found_in: HashMap::new(),
        };
        // What was read before the interpreter failed, if it did, is kept, as a reader keeps it.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut context = Context::new(
                Affine::IDENTITY,
                EVERYWHERE,
                cache,
                pdf.xref(),
                InterpreterSettings::default(),
            );
            interpret(
                TypedIter::new(&instrumented),
                resources,
                &mut context,
                &mut collector,
            );
            if annotations && let Some(list) = page.raw().get::<Array<'_>>(b"Annots") {
                collector.annotation = true;
                let off = groups_off(pdf.xref());
                for annotation in list.iter::<Dict<'_>>() {
                    draw_annotation(
                        &annotation,
                        resources,
                        cache,
                        pdf.xref(),
                        &off,
                        &reader,
                        &mut collector,
                    );
                }
            }
        }));
        collector.error.map_or(Ok(()), Err)
    })?;
    doc.check_time()
}

/// Where the glyphs of a page are looked for: anywhere.
const EVERYWHERE: Rect = Rect::new(-1e9, -1e9, 1e9, 1e9);

/// The appearance annotation `annotation` shows, as hayro picks it: its normal appearance, in
/// its state (`/AS`) when it has several, else `/Off`.
fn appearance<'a>(annotation: &Dict<'a>) -> Option<Stream<'a>> {
    let normal = annotation.get::<Dict<'_>>(b"AP")?;
    if let Some(stream) = normal.get::<Stream<'_>>(b"N") {
        return Some(stream);
    }
    let states = normal.get::<Dict<'_>>(b"N")?;
    annotation
        .get::<HayroName<'_>>(b"AS")
        .and_then(|state| states.get::<Stream<'_>>(state.as_ref()))
        .or_else(|| states.get::<Stream<'_>>(b"Off"))
}

/// The optional content groups off in the document's default configuration, as hayro reads
/// them.
fn groups_off(xref: &XRef) -> HashSet<ObjectIdentifier> {
    let mut off = HashSet::new();
    let Some(properties) = xref
        .get::<Dict<'_>>(xref.root_id())
        .and_then(|catalog| catalog.get::<Dict<'_>>(b"OCProperties"))
    else {
        return off;
    };
    let Some(config) = properties.get::<Dict<'_>>(b"D") else {
        return off;
    };
    let ids = |list: Option<Array<'_>>| -> Vec<ObjectIdentifier> {
        list.map(|list| {
            list.raw_iter()
                .filter_map(|item| item.as_obj_ref())
                .map(Into::into)
                .collect()
        })
        .unwrap_or_default()
    };
    if config.get::<HayroName<'_>>(b"BaseState").as_deref() == Some(&b"OFF"[..]) {
        off.extend(ids(properties.get::<Array<'_>>(b"OCGs")));
    }
    for id in ids(config.get::<Array<'_>>(b"ON")) {
        off.remove(&id);
    }
    off.extend(ids(config.get::<Array<'_>>(b"OFF")));
    off
}

/// Whether the optional content an appearance is tied to (its `/OC`) shows it, `off` being the
/// groups off, as hayro decides it for a form.
fn shown_by_optional_content(dict: &Dict<'_>, off: &HashSet<ObjectIdentifier>) -> bool {
    let Some(content) = dict.get::<Dict<'_>>(b"OC") else {
        return true;
    };
    let reference = dict.get_ref(b"OC");
    let membership = content.get::<HayroName<'_>>(b"Type").as_deref() == Some(&b"OCMD"[..]);
    if let (Some(reference), false) = (reference, membership) {
        return !off.contains(&reference.into());
    }
    let groups: Vec<ObjectIdentifier> = match content.get::<Array<'_>>(b"OCGs") {
        Some(list) => list
            .raw_iter()
            .filter_map(|item| item.as_obj_ref())
            .map(Into::into)
            .collect(),
        None => content
            .get_ref(b"OCGs")
            .map(Into::into)
            .into_iter()
            .collect(),
    };
    if groups.is_empty() {
        return true;
    }
    let on = |id: &ObjectIdentifier| !off.contains(id);
    match content.get::<HayroName<'_>>(b"P").as_deref() {
        Some(b"AllOn") => groups.iter().all(on),
        Some(b"AnyOff") => !groups.iter().all(on),
        Some(b"AllOff") => !groups.iter().any(on),
        _ => groups.iter().any(on),
    }
}

/// The glyphs `annotation`'s appearance draws, placed as a viewer places them (ISO 32000-1,
/// 12.5.5) and as hayro does, its text-showing operators marked with their fonts as the page's
/// are.
fn draw_annotation<'a>(
    annotation: &Dict<'a>,
    page_resources: &Resources<'a>,
    cache: &InterpreterCache<'a>,
    xref: &'a XRef,
    off: &HashSet<ObjectIdentifier>,
    reader: &FontReader<'a>,
    collector: &mut Collector<'a, '_>,
) {
    if annotation.get::<u32>(b"F").unwrap_or(0) & (HIDDEN | NO_VIEW) != 0 {
        return;
    }
    let Some(appearance) = appearance(annotation) else {
        return;
    };
    let dict = appearance.dict();
    let (Some(rect), Some(bbox), Ok(data)) = (
        annotation.get::<PdfRect>(b"Rect"),
        dict.get::<[f32; 4]>(b"BBox"),
        appearance.decoded_data(),
    ) else {
        return;
    };
    if !shown_by_optional_content(dict, off) {
        return;
    }
    let matrix = Affine::new(
        dict.get::<[f64; 6]>(b"Matrix")
            .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
    );
    // The appearance's box through its matrix, fitted to the annotation's rectangle.
    let boxed = (matrix
        * Rect::new(
            f64::from(bbox[0]),
            f64::from(bbox[1]),
            f64::from(bbox[2]),
            f64::from(bbox[3]),
        )
        .to_path(0.1))
    .bounding_box();
    let target = Rect::new(rect.x0, rect.y0, rect.x1, rect.y1);
    let (sx, sy) = (
        target.width() / boxed.width(),
        target.height() / boxed.height(),
    );
    let fit = Affine::new([
        sx,
        0.0,
        0.0,
        sy,
        target.x0 - sx * boxed.x0,
        target.y0 - sy * boxed.y0,
    ]);
    let own = dict.get::<Dict<'_>>(b"Resources").map(Resources::new);
    let resources = own.as_ref().unwrap_or(page_resources);
    let (instrumented, fonts) = instrument(&data, resources, reader);
    // The indexes the page's content gave its fonts mean nothing in the appearance.
    collector.fonts = fonts;
    collector.shown = None;
    collector.last_run = None;
    collector.found_in.clear();
    collector.open.clear();
    let mut context = Context::new(
        fit * matrix,
        EVERYWHERE,
        cache,
        xref,
        InterpreterSettings::default(),
    );
    interpret(
        TypedIter::new(&instrumented),
        resources,
        &mut context,
        collector,
    );
}

/// A font text is shown in.
#[derive(Debug, Clone)]
struct Font<'a> {
    /// What tells its dictionary from another.
    key: u128,
    /// Its `/BaseFont` as written.
    name: Option<String>,
    /// How far its glyphs' boxes reach.
    metrics: Metrics,
    /// hayro's reading of it, for a font not embedded (hayro stretches each glyph of the font
    /// it draws it with to the width the font gives it) and for one of several a form draws
    /// with (to tell which a glyph is drawn in), or for a vertical font's writing metrics.
    hayro: Option<hayro_font::Font<'a>>,
}

/// Reads the fonts text is shown in.
struct FontReader<'a> {
    cache: FontCache<'a>,
    settings: InterpreterSettings,
}

impl<'a> FontReader<'a> {
    fn new() -> Self {
        Self {
            cache: FontCache::new(),
            settings: InterpreterSettings::default(),
        }
    }

    fn read(&self, dict: &Dict<'a>) -> Font<'a> {
        Font {
            key: dict.cache_key(),
            name: base_font(dict),
            metrics: metrics::of_font(dict),
            hayro: (metrics::is_substituted(dict)
                || dict
                    .get::<HayroName<'_>>(b"Encoding")
                    .is_some_and(|n| n.ends_with(b"-V"))
                || dict.get::<Stream<'_>>(b"Encoding").is_some())
            .then(|| self.hayro(dict))
            .flatten(),
        }
    }

    fn hayro(&self, dict: &Dict<'a>) -> Option<hayro_font::Font<'a>> {
        self.cache.font(
            dict,
            &self.settings.font_resolver,
            &self.settings.cmap_resolver,
        )
    }
}

fn base_font(font: &Dict<'_>) -> Option<String> {
    let name = font.get::<HayroName<'_>>(b"BaseFont")?;
    let bytes: &[u8] = name.as_ref();
    Some(match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|b| char::from(*b)).collect(),
    })
}

/// The fonts a form draws with, itself and the forms it draws, as `Tf` names them (None for a
/// name its resources lack).
fn form_fonts<'a>(
    form: &Stream<'a>,
    parent: &Resources<'a>,
    depth: usize,
    found: &mut Vec<Option<Dict<'a>>>,
) {
    if depth > MAX_FORM_DEPTH || found.len() > 16 {
        return;
    }
    let Ok(data) = form.decoded() else {
        return;
    };
    let own = form
        .dict()
        .get::<Dict<'_>>(b"Resources")
        .map(Resources::new);
    let resources = own.as_ref().unwrap_or(parent);
    for op in operators(&data) {
        match &data[op.operator.clone()] {
            b"Tf" => {
                let font =
                    name_operand(&data, &op).and_then(|name| resources.fonts.get::<Dict<'_>>(name));
                let key = font.as_ref().map(Dict::cache_key);
                if !found
                    .iter()
                    .any(|known| known.as_ref().map(Dict::cache_key) == key)
                {
                    found.push(font);
                }
            }
            b"Do" => {
                if let Some(inner) = name_operand(&data, &op)
                    .and_then(|name| resources.x_objects.get::<Stream<'_>>(name))
                    .filter(|xobject| {
                        xobject.dict().get::<HayroName<'_>>(b"Subtype").as_deref()
                            == Some(&b"Form"[..])
                    })
                {
                    form_fonts(&inner, resources, depth + 1, found);
                }
            }
            _ => {}
        }
    }
}

/// `content` with each text-showing operator and each `Do` of a form enclosed in a sequence naming
/// its font (`/F`: an index into the fonts returned; -1 for none known, then `/FS`, the fonts of
/// a form drawing with several), with the form's `/StructParents` (`/SP`) or `/StructParent`
/// (`/SE`); and those fonts, read by `reader`.
fn instrument<'a>(
    content: &[u8],
    resources: &Resources<'a>,
    reader: &FontReader<'a>,
) -> (Vec<u8>, Vec<Font<'a>>) {
    let mut fonts: Vec<Font<'a>> = Vec::new();
    let mut index_of = |dict: &Dict<'a>, read_by_hayro: bool| -> usize {
        let key = dict.cache_key();
        let index = match fonts.iter().position(|known| known.key == key) {
            Some(index) => index,
            None => {
                fonts.push(reader.read(dict));
                fonts.len() - 1
            }
        };
        if read_by_hayro
            && let Some(font) = fonts.get_mut(index)
            && font.hayro.is_none()
        {
            font.hayro = reader.hayro(dict);
        }
        index
    };
    let mut out = Vec::with_capacity(content.len() + content.len() / 4);
    let mut copied = 0;
    // The font in effect, by index, saved and restored with the graphics state.
    let mut stack: Vec<Option<usize>> = vec![None];
    for op in operators(content) {
        let operator = &content[op.operator.clone()];
        let mut structure = String::new();
        let mut candidates = String::new();
        let marked = match operator {
            b"q" => {
                let top = stack.last().copied().flatten();
                stack.push(top);
                None
            }
            b"Q" => {
                if stack.len() > 1 {
                    stack.pop();
                }
                None
            }
            b"Tf" => {
                let font = name_operand(content, &op)
                    .and_then(|name| resources.fonts.get::<Dict<'_>>(name))
                    .map(|font| index_of(&font, false));
                if let Some(top) = stack.last_mut() {
                    *top = font;
                }
                None
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => Some(stack.last().copied().flatten()),
            b"Do" => {
                let form = name_operand(content, &op)
                    .and_then(|name| resources.x_objects.get::<Stream<'_>>(name))
                    .filter(|xobject| {
                        xobject.dict().get::<HayroName<'_>>(b"Subtype").as_deref()
                            == Some(&b"Form"[..])
                    });
                form.map(|form| {
                    if let Some(parents) = form.dict().get::<i64>(b"StructParents") {
                        structure = format!(" /SP {parents}");
                    } else if let Some(item) = form.dict().get::<i64>(b"StructParent") {
                        structure = format!(" /SE {item}");
                    }
                    let mut found = Vec::new();
                    form_fonts(&form, resources, 0, &mut found);
                    match found.as_slice() {
                        [] => stack.last().copied().flatten(),
                        [Some(only)] => Some(index_of(only, false)),
                        several => {
                            let indexes: Vec<String> = several
                                .iter()
                                .flatten()
                                .map(|font| index_of(font, true).to_string())
                                .collect();
                            candidates = format!(" /FS [{}]", indexes.join(" "));
                            None
                        }
                    }
                })
            }
            _ => None,
        };
        let Some(font) = marked else {
            continue;
        };
        let index = font
            .and_then(|index| i64::try_from(index).ok())
            .unwrap_or(-1);
        out.extend_from_slice(&content[copied..op.start]);
        out.extend_from_slice(
            format!(
                " /{} <</F {index}{candidates}{structure}>> BDC ",
                String::from_utf8_lossy(TAG)
            )
            .as_bytes(),
        );
        out.extend_from_slice(&content[op.start..op.end]);
        out.extend_from_slice(b" EMC ");
        copied = op.end;
    }
    out.extend_from_slice(&content[copied..]);
    (out, fonts)
}

/// A marked-content sequence open.
struct Open {
    /// The font it names (by index, None when not known), if it is one of ours.
    font: Option<Option<usize>>,
    /// When its font is not known, the fonts of the form it encloses, which draws with several.
    candidates: Vec<usize>,
    /// The `/StructParents` of the form it encloses, if it is one of ours around one.
    parents: Option<i64>,
    /// What the content inside reads as, when the document says.
    replacement: Option<Replacement>,
}

struct Collector<'a, 'v> {
    /// The fonts the sequences of the content read name.
    fonts: Vec<Font<'a>>,
    open: Vec<Open>,
    visitor: &'v mut Visitor<'v>,
    details: bool,
    error: Option<sealwax_core::Error>,
    /// The preceding fill run in this marked sequence, to skip its matching stroke.
    last_run: Option<usize>,
    /// Text and placement of the last glyph not overdrawn.
    shown: Option<(String, Point, f64)>,
    structure: Structure<'a>,
    /// The page's `/StructParents`.
    page_parents: Option<i64>,
    /// The replacements already read, on an earlier glyph.
    replaced: HashSet<Key>,
    /// Reading the annotations' appearances.
    annotation: bool,
    /// The metrics of the programs glyphs were drawn from without a known font, by their key.
    programs: HashMap<u128, Metrics>,
    /// The font among a form's several each glyph drawn there was found in.
    found_in: HashMap<GlyphIdentifier, Option<usize>>,
}

impl<'f> Collector<'f, '_> {
    /// The font the innermost of our sequences names, when it is known; else the fonts of the
    /// form it encloses, when that draws with several.
    fn font(&self) -> (Option<Font<'f>>, Vec<usize>) {
        let Some(open) = self.open.iter().rev().find(|open| open.font.is_some()) else {
            return (None, Vec::new());
        };
        match open.font.flatten() {
            Some(index) => (self.fonts.get(index).cloned(), Vec::new()),
            None => (None, open.candidates.clone()),
        }
    }

    /// Which of `candidates` the glyph `outline` is drawn in: the one whose program it is.
    fn drawn_in(
        &mut self,
        candidates: &[usize],
        outline: &hayro_font::OutlineGlyph,
    ) -> Option<Font<'f>> {
        let identifier = outline.identifier();
        let fonts = &self.fonts;
        let index = *self.found_in.entry(identifier.clone()).or_insert_with(|| {
            candidates.iter().copied().find(|index| {
                fonts
                    .get(*index)
                    .and_then(|font| font.hayro.as_ref())
                    .is_some_and(|font| {
                        matches!(
                            font.glyph(outline.glyph_id(), outline.char_code()),
                            FontGlyph::Outline(own) if own.identifier() == identifier
                        )
                    })
            })
        });
        self.fonts.get(index?).cloned()
    }

    /// What the next glyph reads as, when an `/ActualText` covers it: the outermost one.
    fn actual_text(&mut self) -> Option<Replacement> {
        let (key, text) = self
            .open
            .iter()
            .find_map(|open| open.replacement.as_ref())?;
        let value = if self.replaced.insert(key.clone()) {
            text.clone()
        } else {
            String::new()
        };
        Some((key.clone(), value))
    }
}

fn bounds(points: &[Point]) -> Option<Bounds> {
    let (mut low, mut high) = (points[0], points[0]);
    for point in &points[1..] {
        low = Point::new(low.x.min(point.x), low.y.min(point.y));
        high = Point::new(high.x.max(point.x), high.y.max(point.y));
    }
    let found = [low.x, low.y, high.x, high.y];
    found.iter().all(|v| v.is_finite()).then_some(found)
}

fn outline_bounds(outline: &hayro_font::OutlineGlyph, drawn: Affine) -> Option<Bounds> {
    let path = outline.outline();
    if path.elements().is_empty() {
        return None;
    }
    let rect = path.bounding_box();
    bounds(&[
        drawn * Point::new(rect.x0, rect.y0),
        drawn * Point::new(rect.x1, rect.y0),
        drawn * Point::new(rect.x0, rect.y1),
        drawn * Point::new(rect.x1, rect.y1),
    ])
}

fn text(unicode: BfString) -> String {
    match unicode {
        BfString::Char(c) => c.to_string(),
        BfString::String(s) => s,
    }
}

impl<'a> Device<'a> for Collector<'_, '_> {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        if self.error.is_some() {
            return;
        }
        // A text filled then stroked comes as two runs of the same glyphs.
        let identity = run.glyphs().as_ptr() as usize;
        if matches!(mode, DrawMode::Stroke(_)) && self.last_run == Some(identity) {
            return;
        }
        self.last_run = matches!(mode, DrawMode::Fill(_)).then_some(identity);
        let invisible = matches!(mode, DrawMode::Invisible);
        let (named_font, candidates) = self.font();
        let vertical = matches!(run.writing_mode(), WritingMode::Vertical);
        for glyph in run.glyphs() {
            let in_form = match (&named_font, &**glyph) {
                (None, HayroGlyph::Outline(outline)) if !candidates.is_empty() => {
                    self.drawn_in(&candidates, outline)
                }
                _ => None,
            };
            let marked = named_font.as_ref().or(in_form.as_ref());
            let drawn = props.transform * glyph.transform();
            // hayro stretches the glyph a font not embedded is drawn with to the width the font
            // gives it: its outline, not its em.
            let stretch = match (&**glyph, marked.and_then(|font| font.hayro.as_ref())) {
                (HayroGlyph::Outline(outline), Some(font)) => {
                    font.glyph_stretch(outline.glyph_id(), outline.char_code())
                }
                _ => 1.0,
            };
            let transform = if stretch.is_finite() && stretch > 0.0 {
                drawn * Affine::scale_non_uniform(1.0 / stretch, 1.0)
            } else {
                drawn
            };
            let origin = transform * Point::ZERO;
            let up = transform * Point::new(0.0, UNITS_PER_EM) - origin;
            let mut program = None;
            let (advance, ink) = match &**glyph {
                HayroGlyph::Outline(outline) => {
                    let ink = if self.details {
                        outline_bounds(outline, drawn)
                    } else {
                        None
                    };
                    if marked.is_none()
                        || (self.details && marked.is_some_and(|f| f.name.is_none()))
                    {
                        program = outline.font_data();
                    }
                    (outline.advance_width().map(f64::from), ink)
                }
                HayroGlyph::Type3(type3) => (type3.advance_width().map(f64::from), None),
            };
            let advance = advance.unwrap_or(0.0);
            let right = transform * Point::new(UNITS_PER_EM, 0.0) - origin;
            let baseline = if vertical { -up } else { right };
            let length = baseline.hypot();
            let [a, b, c, d, _, _] = transform.as_coeffs();
            let size = (a * d - b * c).abs().sqrt() * UNITS_PER_EM;
            if !(origin.x.is_finite()
                && origin.y.is_finite()
                && size.is_finite()
                && length.is_finite()
                && length > 0.0)
            {
                continue;
            }
            let direction = [baseline.x / length, baseline.y / length];
            // The font's metrics, else those of the program the glyph was drawn from.
            let metrics = match (&marked, &program) {
                (Some(font), _) => font.metrics,
                (None, Some(program)) => *self
                    .programs
                    .entry(program.cache_key)
                    .or_insert_with(|| metrics::of_program(program.data.as_ref().as_ref())),
                (None, None) => Metrics::USUAL,
            };
            let (below, above) = metrics.span();
            let (origin, end, corners) = if vertical {
                // The interpreter's transform places the horizontal glyph origin. Undo
                // the vertical font's origin displacement to recover the writing position.
                let font = marked.and_then(|f| f.hayro.as_ref());
                let code = glyph.char_code();
                let displacement = font.map_or(Vec2::new(-UNITS_PER_EM / 2.0, -880.0), |f| {
                    f.origin_displacement(code)
                });
                let step = font.map_or(-UNITS_PER_EM, |f| f.code_advance(code).y);
                let pen = transform * Point::new(-displacement.x, -displacement.y);
                let step = up * (step / UNITS_PER_EM);
                let left = transform * Point::new(0.0, -displacement.y);
                // The usual vertical origin is centred in its glyph cell. For a
                // custom origin on the edge, use one em for the missing cross-size.
                let width = if displacement.x < 0.0 {
                    -2.0 * displacement.x
                } else {
                    UNITS_PER_EM
                };
                let right = transform * Point::new(width, -displacement.y);
                (pen, pen + step, [right, right + step, left, left + step])
            } else {
                let end = transform * Point::new(advance, 0.0);
                let (down, top) = (up * below, up * above);
                (
                    origin,
                    end,
                    [origin + top, end + top, origin + down, end + down],
                )
            };
            let Some(bbox) = bounds(&corners) else {
                continue;
            };
            let font = if self.details {
                marked
                    .and_then(|f| f.name.clone())
                    .or_else(|| program.and_then(|p| p.postscript_name))
            } else {
                None
            };
            let text = glyph.as_unicode().map(text);
            let overdrawn = text.as_ref().is_some_and(|text| {
                self.shown.as_ref().is_some_and(|(last, place, em)| {
                    last == text && origin.distance(*place) < em * OVERDRAWN
                })
            });
            if !overdrawn {
                match &text {
                    Some(text) => {
                        let (last, place, em) = self.shown.get_or_insert_with(Default::default);
                        last.clone_from(text);
                        *place = origin;
                        *em = size;
                    }
                    None => self.shown = None,
                }
            }
            let (key, actual_text) = self.actual_text().unzip();
            let glyph = TextGlyph {
                text,
                origin: [origin.x, origin.y],
                end: [end.x, end.y],
                bbox,
                ink,
                size,
                direction,
                font,
                invisible,
                overdrawn,
                actual_text,
                annotation: self.annotation,
            };
            if let Err(error) = (self.visitor)(glyph, corners, key.as_ref()) {
                self.error = Some(error);
                return;
            }
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn begin_marked_content(&mut self, tag: &[u8], properties: Option<&Dict<'_>>) {
        self.last_run = None;
        let mut open = Open {
            font: None,
            candidates: Vec::new(),
            parents: None,
            replacement: None,
        };
        if tag == TAG {
            open.font = properties
                .and_then(|p| p.get::<i64>(b"F"))
                .map(|index| usize::try_from(index).ok());
            open.candidates = properties
                .and_then(|p| p.get::<Array<'_>>(b"FS"))
                .map(|list| {
                    list.iter::<i64>()
                        .filter_map(|index| usize::try_from(index).ok())
                        .collect()
                })
                .unwrap_or_default();
            open.parents = properties.and_then(|p| p.get::<i64>(b"SP"));
            open.replacement = properties
                .and_then(|p| p.get::<i64>(b"SE"))
                .and_then(|item| self.structure.of_object(item));
        } else if let Some(properties) = properties {
            open.replacement = self.structure.in_properties(properties).or_else(|| {
                let mcid = properties.get::<i64>(b"MCID")?;
                let parents = self
                    .open
                    .iter()
                    .rev()
                    .find_map(|open| open.parents)
                    .or(self.page_parents)?;
                self.structure.of_item(parents, mcid)
            });
        }
        self.open.push(open);
    }

    fn end_marked_content(&mut self) {
        self.last_run = None;
        self.open.pop();
    }
}
