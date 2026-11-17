//! The glyphs a page draws, in the order of its content: what each means, where it is, how large,
//! in which font. The primitive of text extraction: words, lines and reading order are built from
//! it by the caller.
//!
//! Interpreting the content with hayro gives each glyph's position, size and meaning, not the font
//! it was drawn with. So the content is first read operator by operator, following `q`/`Q` and
//! `Tf`, and each text-showing operator is enclosed in a marked-content sequence naming its font:
//! the interpreter hands the sequence to the device with the glyphs. A form XObject is enclosed in
//! one too, naming its font when it draws with one only (or inherits the one in effect); a glyph
//! of a form drawing with several gets the PostScript name hayro reads from the font, when there
//! is one. The sequence around a form also carries its `/StructParents`, so that the `/MCID`s
//! inside are looked up where the form's structure is (see [`crate::actual`]).

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::font::{Glyph as HayroGlyph, GlyphRun};
use hayro_interpret::hayro_cmap::{BfString, WritingMode};
use hayro_interpret::hayro_syntax::content::TypedIter;
use hayro_interpret::hayro_syntax::object::{Dict, Name as HayroName, Stream};
use hayro_interpret::hayro_syntax::page::Resources;
use hayro_interpret::kurbo::{Affine, BezPath, Point, Rect, Shape as _};
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterSettings, RenderContent, SoftMask, interpret, interpret_page,
};
use sealwax_core::Document;

use crate::actual::{Key, Replacement, Structure};
use crate::cache::with_cache;
use crate::drawn::Bounds;

/// The units per em of glyph space in hayro's glyph transforms.
const UNITS_PER_EM: f64 = 1000.0;
/// The tag of the sequences naming fonts.
const TAG: &[u8] = b"SwFont";
/// How far below and above the baseline a glyph's box reaches, in em: the usual descent and
/// ascent, where the font's own are not read.
const DESCENT: f64 = 0.2;
const ASCENT: f64 = 0.8;
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
    /// Where its origin lands.
    pub origin: [f64; 2],
    /// Where the next glyph's origin would land without spacing (its advance): between two
    /// glyphs, the gap a word space or a `TJ` adjustment leaves.
    pub end: [f64; 2],
    /// The box from its origin to its end, from the descent to the ascent of a usual font.
    pub bbox: Bounds,
    /// The box of its outline, when it has one (a space has none, nor a Type 3 glyph here).
    pub ink: Option<Bounds>,
    /// The size it is drawn at: the length of one em upward, after the text and the current
    /// transformation matrices (zero when they flatten it to its baseline).
    pub size: f64,
    /// The direction of its baseline, a unit vector.
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
/// A glyph an annotation draws names its font only when the font names itself.
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
    if page_index >= doc.hayro().pages().len() {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
    }
    doc.check_page(page_index)?;
    let glyphs = with_cache(doc, |pdf, cache| {
        let pages = pdf.pages();
        let Some(page) = pages.get(page_index) else {
            return Vec::new();
        };
        let content = page.page_stream().unwrap_or_default();
        let resources = page.resources();
        let (instrumented, fonts) = instrument(content, resources);
        let mut collector = Collector {
            fonts: &fonts,
            open: Vec::new(),
            glyphs: Vec::new(),
            last_run: None,
            shown: None,
            structure: Structure::new(pdf.xref()),
            page_parents: page.raw().get::<i64>(b"StructParents"),
            replaced: HashSet::new(),
            annotation: false,
        };
        let everywhere = Rect::new(-1e9, -1e9, 1e9, 1e9);
        // What was read before the interpreter failed, if it did, is kept, as a reader keeps it.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut context = Context::new(
                Affine::IDENTITY,
                everywhere,
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
            if annotations {
                collector.annotation = true;
                collector.open.clear();
                let mut context = Context::new(
                    Affine::IDENTITY,
                    everywhere,
                    cache,
                    pdf.xref(),
                    InterpreterSettings {
                        render_content: RenderContent::ANNOTATIONS,
                        ..InterpreterSettings::default()
                    },
                );
                interpret_page(page, &mut context, &mut collector);
            }
        }));
        collector.glyphs
    });
    doc.check_time()?;
    Ok(glyphs)
}

/// An operator of a content stream: from its first operand to its end.
struct Op {
    start: usize,
    operator: std::ops::Range<usize>,
    end: usize,
}

fn is_white(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' | b'\0')
}

fn is_regular(b: u8) -> bool {
    !is_white(b) && !b"()<>[]{}/%".contains(&b)
}

fn skip_string(data: &[u8], mut at: usize) -> usize {
    let mut depth = 0usize;
    while at < data.len() {
        match data[at] {
            b'\\' => at += 1,
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return at + 1;
                }
            }
            _ => {}
        }
        at += 1;
    }
    data.len()
}

/// The end of an inline image whose `BI` ends at `at`: past `EI`, the first one standing alone
/// after the data.
fn inline_image_end(data: &[u8], at: usize) -> usize {
    let mut cursor = at;
    while cursor + 1 < data.len() {
        if data[cursor] == b'I'
            && data[cursor + 1] == b'D'
            && cursor > 0
            && !is_regular(data[cursor - 1])
            && data.get(cursor + 2).is_none_or(|b| !is_regular(*b))
        {
            break;
        }
        cursor += 1;
    }
    cursor += 3;
    while cursor + 1 < data.len() {
        if data[cursor] == b'E'
            && data[cursor + 1] == b'I'
            && is_white(data[cursor - 1])
            && data.get(cursor + 2).is_none_or(|b| is_white(*b))
        {
            return cursor + 2;
        }
        cursor += 1;
    }
    data.len()
}

/// The operators of `data`.
fn operators(data: &[u8]) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut operands: Option<usize> = None;
    let mut at = 0;
    loop {
        while at < data.len() && is_white(data[at]) {
            at += 1;
        }
        if data.get(at) == Some(&b'%') {
            while at < data.len() && !matches!(data[at], b'\r' | b'\n') {
                at += 1;
            }
            continue;
        }
        let Some(&b) = data.get(at) else {
            break;
        };
        let start = at;
        let mut operator = false;
        match b {
            b'(' => at = skip_string(data, at),
            b'<' if data.get(at + 1) == Some(&b'<') => at += 2,
            b'<' => {
                at = data[at..]
                    .iter()
                    .position(|c| *c == b'>')
                    .map_or(data.len(), |p| at + p + 1);
            }
            b'>' if data.get(at + 1) == Some(&b'>') => at += 2,
            b'/' => {
                at += 1;
                while at < data.len() && is_regular(data[at]) {
                    at += 1;
                }
            }
            _ if !is_regular(b) => at += 1,
            _ => {
                while at < data.len() && is_regular(data[at]) {
                    at += 1;
                }
                let word = &data[start..at];
                operator = !(b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'))
                    && !matches!(word, b"true" | b"false" | b"null");
            }
        }
        if !operator {
            operands.get_or_insert(start);
            continue;
        }
        let word = start..at;
        if &data[word.clone()] == b"BI" {
            at = inline_image_end(data, at);
        }
        ops.push(Op {
            start: operands.take().unwrap_or(start),
            operator: word,
            end: at,
        });
    }
    ops
}

/// The last name operand of `op`, without its `/`.
fn name_operand<'d>(data: &'d [u8], op: &Op) -> Option<&'d [u8]> {
    let operands = &data[op.start..op.operator.start];
    let slash = operands.iter().rposition(|b| *b == b'/')?;
    let name = &operands[slash + 1..];
    let end = name
        .iter()
        .position(|b| !is_regular(*b))
        .unwrap_or(name.len());
    Some(&name[..end])
}

fn base_font(font: &Dict<'_>) -> Option<String> {
    let name = font.get::<HayroName<'_>>(b"BaseFont")?;
    let bytes: &[u8] = name.as_ref();
    Some(match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|b| char::from(*b)).collect(),
    })
}

/// The fonts a form draws with, itself and the forms it draws, as `Tf` names them: their
/// `/BaseFont` (None for one that names none).
fn form_fonts(
    form: &Stream<'_>,
    parent: &Resources<'_>,
    depth: usize,
    found: &mut Vec<Option<String>>,
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
                let font = name_operand(&data, &op)
                    .and_then(|name| resources.fonts.get::<Dict<'_>>(name))
                    .and_then(|font| base_font(&font));
                if !found.contains(&font) {
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
/// its font (`/F`: an index into the fonts returned; -1 for none known), with the form's
/// `/StructParents` (`/SP`) or `/StructParent` (`/SE`); and those fonts.
fn instrument(content: &[u8], resources: &Resources<'_>) -> (Vec<u8>, Vec<Option<String>>) {
    let mut fonts: Vec<Option<String>> = Vec::new();
    let index_of = |font: Option<String>, fonts: &mut Vec<Option<String>>| -> i64 {
        match fonts.iter().position(|known| *known == font) {
            Some(index) => i64::try_from(index).unwrap_or(-1),
            None => {
                fonts.push(font);
                i64::try_from(fonts.len() - 1).unwrap_or(-1)
            }
        }
    };
    let mut out = Vec::with_capacity(content.len() + content.len() / 4);
    let mut copied = 0;
    // The font in effect, saved and restored with the graphics state.
    let mut stack: Vec<Option<String>> = vec![None];
    for op in operators(content) {
        let operator = &content[op.operator.clone()];
        let mut structure = String::new();
        let marked = match operator {
            b"q" => {
                let top = stack.last().cloned().flatten();
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
                    .and_then(|font| base_font(&font));
                if let Some(top) = stack.last_mut() {
                    *top = font;
                }
                None
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => Some(stack.last().cloned().flatten()),
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
                        [] => stack.last().cloned().flatten(),
                        [only] => only.clone(),
                        _ => None,
                    }
                })
            }
            _ => None,
        };
        let Some(font) = marked else {
            continue;
        };
        let index = if font.is_none() && operator == b"Do" {
            -1
        } else {
            index_of(font, &mut fonts)
        };
        out.extend_from_slice(&content[copied..op.start]);
        out.extend_from_slice(
            format!(
                " /{} <</F {index}{structure}>> BDC ",
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
    /// The font it names, if it is one of ours.
    font: Option<Option<String>>,
    /// The `/StructParents` of the form it encloses, if it is one of ours around one.
    parents: Option<i64>,
    /// What the content inside reads as, when the document says.
    replacement: Option<Replacement>,
}

struct Collector<'f, 's> {
    fonts: &'f [Option<String>],
    open: Vec<Open>,
    glyphs: Vec<TextGlyph>,
    /// The glyphs of the last run, to skip the stroke run of a text both filled and stroked.
    last_run: Option<usize>,
    /// The last glyph not overdrawn, by index.
    shown: Option<usize>,
    structure: Structure<'s>,
    /// The page's `/StructParents`.
    page_parents: Option<i64>,
    /// The replacements already read, on an earlier glyph.
    replaced: HashSet<Key>,
    /// Reading the annotations' appearances.
    annotation: bool,
}

impl Collector<'_, '_> {
    fn font(&self) -> Option<Option<String>> {
        self.open.iter().rev().find_map(|open| open.font.clone())
    }

    /// What the next glyph reads as, when an `/ActualText` covers it: the outermost one.
    fn actual_text(&mut self) -> Option<String> {
        let (key, text) = self
            .open
            .iter()
            .find_map(|open| open.replacement.as_ref())?;
        Some(if self.replaced.insert(key.clone()) {
            text.clone()
        } else {
            String::new()
        })
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
        // A text filled then stroked comes as two runs of the same glyphs.
        let identity = run.glyphs().as_ptr() as usize;
        if matches!(mode, DrawMode::Stroke(_)) && self.last_run == Some(identity) {
            return;
        }
        self.last_run = Some(identity);
        let invisible = matches!(mode, DrawMode::Invisible);
        let marked = self.font();
        let vertical = matches!(run.writing_mode(), WritingMode::Vertical);
        for glyph in run.glyphs() {
            let transform = props.transform * glyph.transform();
            let origin = transform * Point::ZERO;
            let up = transform * Point::new(0.0, UNITS_PER_EM) - origin;
            let (advance, ink, named) = match &**glyph {
                HayroGlyph::Outline(outline) => {
                    let path = outline.outline();
                    let ink = (!path.elements().is_empty())
                        .then(|| path.bounding_box())
                        .and_then(|rect| {
                            bounds(&[
                                transform * Point::new(rect.x0, rect.y0),
                                transform * Point::new(rect.x1, rect.y0),
                                transform * Point::new(rect.x0, rect.y1),
                                transform * Point::new(rect.x1, rect.y1),
                            ])
                        });
                    let named = outline.font_data().and_then(|data| data.postscript_name);
                    (outline.advance_width().map(f64::from), ink, named)
                }
                HayroGlyph::Type3(type3) => (type3.advance_width().map(f64::from), None, None),
            };
            let advance = advance.unwrap_or(0.0);
            let end = if vertical {
                transform * Point::new(0.0, -advance)
            } else {
                transform * Point::new(advance, 0.0)
            };
            let baseline = transform * Point::new(UNITS_PER_EM, 0.0) - origin;
            let length = baseline.hypot();
            let size = up.hypot();
            if !(origin.x.is_finite() && origin.y.is_finite() && size.is_finite() && length > 0.0) {
                continue;
            }
            let direction = [baseline.x / length, baseline.y / length];
            let (down, top) = (up * -DESCENT, up * ASCENT);
            let Some(bbox) = bounds(&[origin + down, origin + top, end + down, end + top]) else {
                continue;
            };
            let font = match &marked {
                Some(Some(name)) => Some(name.clone()),
                _ => named,
            };
            let text = glyph.as_unicode().map(text);
            let actual_text = self.actual_text();
            let overdrawn = self
                .shown
                .and_then(|index| self.glyphs.get(index))
                .is_some_and(|shown| {
                    text.is_some()
                        && shown.text == text
                        && (shown.origin[0] - origin.x).hypot(shown.origin[1] - origin.y)
                            < shown.size * OVERDRAWN
                });
            if !overdrawn {
                self.shown = Some(self.glyphs.len());
            }
            self.glyphs.push(TextGlyph {
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
            });
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn begin_marked_content(&mut self, tag: &[u8], properties: Option<&Dict<'_>>) {
        let mut open = Open {
            font: None,
            parents: None,
            replacement: None,
        };
        if tag == TAG {
            open.font = properties.and_then(|p| p.get::<i64>(b"F")).map(|index| {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| self.fonts.get(index).cloned().flatten())
            });
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
        self.open.pop();
    }
}
