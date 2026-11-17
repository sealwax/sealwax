//! Text redacted: the glyphs drawn under areas of a page taken out of its content, as text that
//! no reader can extract any longer, rather than covered.
//!
//! The page's content, and the content of every form XObject it draws, is read operator by
//! operator, its text state followed as hayro follows it (ISO 32000-1, 9.4): each string is split
//! into codes and each code placed with the font's own reading and advance (hayro-font), the
//! glyph boxed from its advance and from 0.2 em below its baseline to 0.8 em above it. A glyph
//! whose box overlaps an area by any surface is taken out. Its operator is written again as a
//! `TJ` whose kept codes are where they were: each run of codes taken out becomes the adjustment
//! that moves as far as they did, so that nothing after them moves.
//!
//! Text in a hidden optional content group is read like any other: hidden is not gone. A form
//! XObject that loses text is written anew for this page, the page's resources pointing to the
//! copy, so that the other pages drawing the form keep it whole.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::ops::Range;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use hayro_font::{Font, FontCache};
use hayro_interpret::InterpreterSettings;
use hayro_interpret::hayro_syntax::object::{
    Array, Dict as HayroDict, ObjectIdentifier, Stream as HayroStream,
};
use hayro_interpret::hayro_syntax::page::Resources;
use hayro_interpret::kurbo::{Affine, Point, Rect as KurboRect, Shape as _, Vec2};
use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Stream};

use crate::scan::{Op, Operand, operands, operators};

/// Glyph units per em (hayro-font's).
const UNITS: f64 = 1000.0;
/// How far below its baseline a glyph's box goes, and above it, in ems.
const DESCENT: f64 = 0.2;
const ASCENT: f64 = 0.8;
/// How deep form XObjects are followed.
const MAX_DEPTH: usize = 12;

/// What [`redact_text`] took out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct RedactedText {
    /// Glyphs taken out.
    pub glyphs: usize,
    /// Text-showing operators written again.
    pub operators: usize,
    /// Form XObjects written anew for the page.
    pub forms: usize,
    /// Strings taken out whole because their font could not be read: where each glyph lies is
    /// not known, only where the string starts.
    pub strings_unread: usize,
}

/// Take out of page `page_index` (from 0) the glyphs drawn under `areas`, rectangles in the
/// page's default user space (unrotated, y up), its form XObjects' included (see the module).
/// Annotations are left as they are: flatten them first to redact what they show. Only text is
/// taken out: images and paths under the areas stay.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have or an area
/// that is not finite; what [`Document::check_page`] refuses (the limits, the deadline);
/// [`sealwax_core::Error::Malformed`] for a content stream of the page that cannot be decoded
/// (it would be lost); as [`Document::save_incremental`] for a modified document read again;
/// [`sealwax_core::Error::LimitExceeded`] when a new object cannot be numbered.
pub fn redact_text(
    doc: &mut Document,
    page_index: usize,
    areas: &[Rect],
) -> sealwax_core::Result<RedactedText> {
    let Some(Some(page_id)) = doc.page_ids().get(page_index).copied() else {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
    };
    if !areas
        .iter()
        .all(|a| [a.x1, a.y1, a.x2, a.y2].iter().all(|v| v.is_finite()))
    {
        return Err(sealwax_core::Error::InvalidArgument {
            what: "redaction area",
        });
    }
    doc.check_page(page_index)?;
    let areas: Vec<KurboRect> = areas
        .iter()
        .map(|a| KurboRect::new(a.x1, a.y1, a.x2, a.y2).abs())
        .filter(|a| a.area() > 0.0)
        .collect();
    if areas.is_empty() {
        return Ok(RedactedText::default());
    }
    // hayro reads the document as it stands: a modified one is read again, its objects keeping
    // their numbers.
    let reread;
    let reader: &Document = if doc.is_modified() {
        reread = Document::open(
            doc.save_incremental()?,
            &sealwax_core::OpenOptions::default().limits(*doc.limits()),
        )?;
        &reread
    } else {
        doc
    };
    let content = page_content(reader, page_id)?;
    let plan = plan(reader, page_index, &content, &areas)?;
    write(doc, page_id, &content, plan)
}

/// The decoded data of stream `id`, as hayro reads it.
fn decoded(reader: &Document, id: ObjId) -> Option<Vec<u8>> {
    let identifier = ObjectIdentifier {
        obj_number: id.number,
        gen_number: i32::from(id.generation),
    };
    reader
        .hayro()
        .xref()
        .get::<HayroStream<'_>>(identifier)
        .and_then(|stream| stream.decoded().ok().map(|data| data.into_owned()))
}

/// The page's content: its streams decoded, joined by line feeds.
fn page_content(doc: &Document, page: ObjId) -> sealwax_core::Result<Vec<u8>> {
    let Some(Object::Dict(dict)) = doc.object(page)? else {
        return Ok(Vec::new());
    };
    let ids: Vec<ObjId> = match dict.get(b"Contents") {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Array(items)) => items.iter().filter_map(Object::as_ref_id).collect(),
            _ => vec![*id],
        },
        Some(Object::Array(items)) => items.iter().filter_map(Object::as_ref_id).collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for id in ids {
        // A stream that cannot be read would be lost from the content written anew.
        let data = decoded(doc, id).ok_or(sealwax_core::Error::Malformed)?;
        out.extend_from_slice(&data);
        out.push(b'\n');
    }
    Ok(out)
}

/// A content read: the page's, or that of a form XObject where one `Do` draws it (a form drawn
/// twice is read twice, and redacted where each drawing puts it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Source {
    Page,
    Draw(usize),
}

/// A form XObject drawn.
#[derive(Debug, Clone)]
struct Draw {
    form: ObjId,
    /// The content whose `Do` draws it.
    parent: Source,
    /// That `Do`, in the parent's content.
    op: Range<usize>,
    /// The form has resources of its own (else it uses its parent's).
    own_resources: bool,
}

/// A code of a string shown.
#[derive(Debug, Clone)]
struct Code {
    bytes: Vec<u8>,
    /// The `TJ` adjustment that moves as far as the code does; `None` when none can (a font
    /// size of 0).
    adjustment: Option<f64>,
}

/// An element of what an operator shows.
#[derive(Debug, Clone)]
enum Element {
    Codes(Vec<Code>),
    Adjust(f64),
}

/// A text-showing operator, and the codes of it to take out.
#[derive(Debug, Clone)]
struct Shown {
    /// What is written before the `TJ` that replaces it: `T*` for `'`, the spacing and `T*` for
    /// `"`.
    prefix: Vec<u8>,
    elements: Vec<Element>,
    /// The codes taken out, numbered across the elements.
    removed: BTreeSet<usize>,
    /// Its font could not be read: the strings are taken out whole when any is.
    unread: bool,
}

/// What redacting a page changes.
#[derive(Default)]
struct Plan {
    /// The operators to write again, by content and by their range in it.
    shown: HashMap<Source, Vec<(Range<usize>, Shown)>>,
    /// The forms drawn, in the order they are met: a form after the one drawing it.
    draws: Vec<Draw>,
    /// The forms read, and their content decoded.
    forms: HashMap<ObjId, Vec<u8>>,
}

/// The text state, as far as placing glyphs goes (ISO 32000-1, 9.3).
#[derive(Clone)]
struct State<'a> {
    ctm: Affine,
    font: Option<Font<'a>>,
    size: f64,
    char_space: f64,
    word_space: f64,
    scale: f64,
    leading: f64,
    rise: f64,
}

impl Default for State<'_> {
    fn default() -> Self {
        Self {
            ctm: Affine::IDENTITY,
            font: None,
            // hayro's, when no font size is set.
            size: 1.0,
            char_space: 0.0,
            word_space: 0.0,
            scale: 1.0,
            leading: 0.0,
            rise: 0.0,
        }
    }
}

struct Walker<'a, 'r> {
    areas: &'r [KurboRect],
    fonts: FontCache<'a>,
    settings: InterpreterSettings,
    plan: Plan,
    /// Shown operators of each stream, by operator index, while it is read.
    shown: HashMap<Source, HashMap<usize, (Range<usize>, Shown)>>,
}

fn object_id(id: ObjectIdentifier) -> ObjId {
    ObjId {
        number: id.obj_number,
        generation: u16::try_from(id.gen_number).unwrap_or(0),
    }
}

fn plan(
    reader: &Document,
    page_index: usize,
    content: &[u8],
    areas: &[KurboRect],
) -> sealwax_core::Result<Plan> {
    let pdf = reader.hayro();
    let pages = pdf.pages();
    let Some(page) = pages.get(page_index) else {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
    };
    let mut walker = Walker {
        areas,
        fonts: FontCache::new(),
        settings: InterpreterSettings::default(),
        plan: Plan::default(),
        shown: HashMap::new(),
    };
    walker.walk(Source::Page, content, page.resources(), State::default(), 0);
    let mut plan = walker.plan;
    for (source, ops) in walker.shown {
        let mut kept: Vec<(Range<usize>, Shown)> = ops
            .into_values()
            .filter(|(_, shown)| !shown.removed.is_empty())
            .collect();
        kept.sort_by_key(|(range, _)| range.start);
        if !kept.is_empty() {
            plan.shown.insert(source, kept);
        }
    }
    Ok(plan)
}

impl<'a> Walker<'a, '_> {
    fn font(&self, dict: &HayroDict<'a>) -> Option<Font<'a>> {
        self.fonts.font(
            dict,
            &self.settings.font_resolver,
            &self.settings.cmap_resolver,
        )
    }

    /// The font hayro draws with when none is set, or the one set cannot be read: Helvetica.
    fn fallback(&self) -> Option<Font<'a>> {
        self.fonts.standard_font(
            hayro_font::StandardFont::Helvetica,
            &self.settings.font_resolver,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn walk(
        &mut self,
        source: Source,
        content: &[u8],
        resources: &Resources<'a>,
        initial: State<'a>,
        depth: usize,
    ) {
        let mut state = initial;
        let mut stack: Vec<State<'a>> = Vec::new();
        let mut tm = Affine::IDENTITY;
        let mut tlm = Affine::IDENTITY;
        for (index, op) in operators(content).iter().enumerate() {
            let operator = &content[op.operator.clone()];
            let args = operands(&content[op.start..op.operator.start]);
            let number = |at: usize| -> Option<f64> {
                match args.get(at) {
                    Some(Operand::Number(n)) => Some(*n),
                    _ => None,
                }
            };
            match operator {
                b"q" => stack.push(state.clone()),
                b"Q" => {
                    if let Some(saved) = stack.pop() {
                        state = saved;
                    }
                }
                b"cm" => {
                    if let [Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)] =
                        [0, 1, 2, 3, 4, 5].map(number)
                    {
                        state.ctm *= Affine::new([a, b, c, d, e, f]);
                    }
                }
                b"BT" => {
                    tm = Affine::IDENTITY;
                    tlm = Affine::IDENTITY;
                }
                b"Td" | b"TD" => {
                    if let (Some(x), Some(y)) = (number(0), number(1)) {
                        if operator == b"TD" {
                            state.leading = -y;
                        }
                        tlm *= Affine::translate((x, y));
                        tm = tlm;
                    }
                }
                b"Tm" => {
                    if let [Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)] =
                        [0, 1, 2, 3, 4, 5].map(number)
                    {
                        tlm = Affine::new([a, b, c, d, e, f]);
                        tm = tlm;
                    }
                }
                b"T*" => {
                    tlm *= Affine::translate((0.0, -state.leading));
                    tm = tlm;
                }
                b"Tc" => state.char_space = number(0).unwrap_or(state.char_space),
                b"Tw" => state.word_space = number(0).unwrap_or(state.word_space),
                b"Tz" => state.scale = number(0).map_or(state.scale, |s| s / 100.0),
                b"TL" => state.leading = number(0).unwrap_or(state.leading),
                b"Ts" => state.rise = number(0).unwrap_or(state.rise),
                b"Tf" => {
                    if let (Some(Operand::Name(name)), Some(size)) = (args.first(), number(1)) {
                        state.size = size;
                        state.font = resources
                            .fonts
                            .get::<HayroDict<'a>>(name.as_slice())
                            .and_then(|dict| self.font(&dict));
                    }
                }
                // A graphics state may set the font (`/Font [font size]`).
                b"gs" => {
                    if let Some(Operand::Name(name)) = args.first()
                        && let Some(graphics) =
                            resources.ext_g_states.get::<HayroDict<'a>>(name.as_slice())
                        && let Some(font) = graphics.get::<Array<'a>>(b"Font")
                    {
                        let mut items =
                            font.iter::<hayro_interpret::hayro_syntax::object::Object<'a>>();
                        let dict = items.next().and_then(|o| o.into_dict());
                        let size = items.next().and_then(|o| o.into_f32()).map(f64::from);
                        if let (Some(dict), Some(size)) = (dict, size) {
                            state.size = size;
                            state.font = self.font(&dict);
                        }
                    }
                }
                b"Tj" | b"TJ" | b"'" | b"\"" => {
                    let mut prefix = Vec::new();
                    if operator == b"\""
                        && let (Some(aw), Some(ac)) = (number(0), number(1))
                    {
                        state.word_space = aw;
                        state.char_space = ac;
                        prefix.extend_from_slice(
                            format!("{} Tw {} Tc ", number_text(aw), number_text(ac)).as_bytes(),
                        );
                    }
                    if matches!(operator, b"'" | b"\"") {
                        tlm *= Affine::translate((0.0, -state.leading));
                        tm = tlm;
                        prefix.extend_from_slice(b"T* ");
                    }
                    let shown: Vec<Operand> = match (operator, args.last()) {
                        (b"TJ", Some(Operand::Array(items))) => items.clone(),
                        (_, Some(Operand::String(bytes))) => vec![Operand::String(bytes.clone())],
                        _ => Vec::new(),
                    };
                    self.show(source, index, op, prefix, &shown, &state, &mut tm);
                }
                b"Do" => {
                    if depth >= MAX_DEPTH {
                        continue;
                    }
                    let Some(Operand::Name(name)) = args.first() else {
                        continue;
                    };
                    let Some(stream) = resources.x_objects.get::<HayroStream<'a>>(name.as_slice())
                    else {
                        continue;
                    };
                    let dict = stream.dict();
                    if dict
                        .get::<hayro_interpret::hayro_syntax::object::Name>(b"Subtype")
                        .is_none_or(|subtype| subtype.as_ref() != b"Form")
                    {
                        continue;
                    }
                    let id = object_id(stream.obj_id());
                    let Ok(data) = stream.decoded() else {
                        continue;
                    };
                    self.plan.forms.entry(id).or_insert_with(|| data.to_vec());
                    let matrix = dict
                        .get::<Array<'a>>(b"Matrix")
                        .map(|array| array.iter::<f64>().collect::<Vec<f64>>())
                        .and_then(|m| <[f64; 6]>::try_from(m).ok())
                        .map_or(Affine::IDENTITY, Affine::new);
                    let own = dict.get::<HayroDict<'a>>(b"Resources").map(Resources::new);
                    let mut inner = state.clone();
                    inner.ctm *= matrix;
                    let draw = self.plan.draws.len();
                    self.plan.draws.push(Draw {
                        form: id,
                        parent: source,
                        op: op.start..op.end,
                        own_resources: own.is_some(),
                    });
                    let resources = own.as_ref().unwrap_or(resources);
                    self.walk(Source::Draw(draw), &data, resources, inner, depth + 1);
                }
                _ => {}
            }
        }
    }

    /// Place the codes `shown` shows, from text matrix `tm`, and note those under the areas.
    #[allow(clippy::too_many_arguments)]
    fn show(
        &mut self,
        source: Source,
        index: usize,
        op: &Op,
        prefix: Vec<u8>,
        shown: &[Operand],
        state: &State<'a>,
        tm: &mut Affine,
    ) {
        let fallback = if state.font.is_none() {
            self.fallback()
        } else {
            None
        };
        let font = state.font.as_ref().or(fallback.as_ref());
        let horizontal = font.is_none_or(Font::is_horizontal);
        let scale = if horizontal { state.scale } else { 1.0 };
        let temp = Affine::new([state.size * scale, 0.0, 0.0, state.size, 0.0, state.rise]);
        let mut elements = Vec::new();
        let mut removed = BTreeSet::new();
        let mut code_index = 0;
        let unread = font.is_none();
        for item in shown {
            match item {
                Operand::Number(n) => {
                    let moved = -n / UNITS * state.size * scale;
                    *tm *= if horizontal {
                        Affine::translate((moved, 0.0))
                    } else {
                        Affine::translate((0.0, -n / UNITS * state.size))
                    };
                    elements.push(Element::Adjust(*n));
                }
                Operand::String(bytes) => {
                    let mut codes = Vec::new();
                    let mut at = 0;
                    while at < bytes.len() {
                        let (code, length) = match font {
                            Some(font) => font.read_code(bytes, at),
                            None => (u32::from(bytes[at]), 1),
                        };
                        let length = length.clamp(1, bytes.len() - at);
                        let (advance, displacement, ink) = match font {
                            Some(font) => (
                                font.code_advance(code),
                                font.origin_displacement(code),
                                ink(font, code),
                            ),
                            None => (Vec2::ZERO, Vec2::ZERO, None),
                        };
                        let word = if code == 32 && length == 1 {
                            state.word_space
                        } else {
                            0.0
                        };
                        let place = state.ctm
                            * *tm
                            * temp
                            * Affine::scale(1.0 / UNITS)
                            * Affine::translate(displacement);
                        // The glyph's box, in glyph units: its advance, from below its baseline
                        // to above it, and its outline wherever that goes (a mark of no
                        // advance drawn over the letter before it).
                        // Written vertically, the glyph hangs below its origin, centred on it:
                        // before its displacement (-w/2, -vy), from 0 to w across and from vy
                        // less its advance down to vy.
                        let mut glyph = if horizontal {
                            KurboRect::new(0.0, -DESCENT * UNITS, advance.x, ASCENT * UNITS)
                        } else {
                            KurboRect::new(
                                0.0,
                                -displacement.y + advance.y,
                                -2.0 * displacement.x,
                                -displacement.y,
                            )
                        }
                        .abs();
                        if let Some(ink) = ink {
                            glyph = glyph.union(ink);
                        }
                        // A glyph of no width still means text: a twentieth of an em around
                        // its origin, so that an area over it takes it.
                        if glyph.width() < 1.0 {
                            glyph = glyph.inflate(UNITS / 20.0, 0.0);
                        }
                        let points = [
                            Point::new(glyph.x0, glyph.y0),
                            Point::new(glyph.x1, glyph.y0),
                            Point::new(glyph.x0, glyph.y1),
                            Point::new(glyph.x1, glyph.y1),
                        ]
                        .map(|p| place * p);
                        let under = if unread {
                            // Where each glyph lies is not known: the string's start decides.
                            let origin = place * Point::ZERO;
                            self.areas.iter().any(|area| area.contains(origin))
                        } else {
                            covered(&points, self.areas)
                        };
                        if under {
                            removed.insert(code_index);
                        }
                        let along = if horizontal { advance.x } else { advance.y };
                        let adjustment = (state.size != 0.0)
                            .then(|| -(along + (state.char_space + word) * UNITS / state.size));
                        let base = along / UNITS * state.size + state.char_space + word;
                        *tm *= if horizontal {
                            Affine::translate((base * scale, 0.0))
                        } else {
                            Affine::translate((0.0, base))
                        };
                        codes.push(Code {
                            bytes: bytes[at..at + length].to_vec(),
                            adjustment,
                        });
                        code_index += 1;
                        at += length;
                    }
                    elements.push(Element::Codes(codes));
                }
                _ => {}
            }
        }
        if unread && !removed.is_empty() {
            removed = (0..code_index).collect();
        }
        let ops = self.shown.entry(source).or_default();
        match ops.get_mut(&index) {
            // A form drawn again: what any drawing puts under an area is taken out.
            Some((_, existing)) => existing.removed.extend(removed),
            None => {
                ops.insert(
                    index,
                    (
                        op.start..op.end,
                        Shown {
                            prefix,
                            elements,
                            removed,
                            unread,
                        },
                    ),
                );
            }
        }
    }
}

/// The box of the outline of the glyph `code` shows, in glyph units; `None` for a glyph without
/// one (a space, a Type 3 glyph).
fn ink(font: &Font<'_>, code: u32) -> Option<KurboRect> {
    match font.glyph(font.map_code(code), code) {
        hayro_font::FontGlyph::Outline(glyph) => {
            let outline = glyph.outline();
            (outline.elements().len() > 1).then(|| outline.bounding_box())
        }
        _ => None,
    }
}

/// Whether the parallelogram of `points` (origin, along the baseline, up, both) overlaps an area
/// by any surface: no axis of either separates them (the separating axis theorem), a touch along
/// an edge not counting.
fn covered(points: &[Point; 4], areas: &[KurboRect]) -> bool {
    if !points.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
        return false;
    }
    let along = points[1] - points[0];
    let up = points[2] - points[0];
    let corners = [points[0], points[1], points[3], points[2]];
    areas.iter().any(|area| {
        let rect = [
            Point::new(area.x0, area.y0),
            Point::new(area.x1, area.y0),
            Point::new(area.x1, area.y1),
            Point::new(area.x0, area.y1),
        ];
        let axes = [
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(-along.y, along.x),
            Vec2::new(-up.y, up.x),
        ];
        axes.iter().all(|axis| {
            if axis.hypot2() == 0.0 {
                return true;
            }
            let project = |points: &[Point; 4]| {
                points.iter().fold((f64::MAX, f64::MIN), |(low, high), p| {
                    let t = p.to_vec2().dot(*axis);
                    (low.min(t), high.max(t))
                })
            };
            let (a0, a1) = project(&corners);
            let (b0, b1) = project(&rect);
            a1.min(b1) - a0.max(b0) > 1e-9 * axis.hypot()
        })
    })
}

/// A number as content streams write it.
fn number_text(value: f64) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "" | "-0" => "0".to_owned(),
        other => other.to_owned(),
    }
}

/// The `TJ` that replaces `shown`, the codes taken out turned into adjustments.
fn rewritten(shown: &Shown) -> Vec<u8> {
    let mut out = shown.prefix.clone();
    out.push(b'[');
    let mut pending: Option<f64> = None;
    let mut run: Vec<u8> = Vec::new();
    let flush_run = |out: &mut Vec<u8>, run: &mut Vec<u8>| {
        if !run.is_empty() {
            out.push(b'<');
            for b in run.iter() {
                out.extend_from_slice(format!("{b:02X}").as_bytes());
            }
            out.extend_from_slice(b"> ");
            run.clear();
        }
    };
    let flush_number = |out: &mut Vec<u8>, pending: &mut Option<f64>| {
        if let Some(n) = pending.take()
            && n != 0.0
        {
            out.extend_from_slice(number_text(n).as_bytes());
            out.push(b' ');
        }
    };
    let mut index = 0;
    for element in &shown.elements {
        match element {
            Element::Adjust(n) => {
                flush_run(&mut out, &mut run);
                *pending.get_or_insert(0.0) += n;
            }
            Element::Codes(codes) => {
                for code in codes {
                    if shown.removed.contains(&index) {
                        flush_run(&mut out, &mut run);
                        if let Some(adjustment) = code.adjustment {
                            *pending.get_or_insert(0.0) += adjustment;
                        }
                    } else {
                        flush_number(&mut out, &mut pending);
                        run.extend_from_slice(&code.bytes);
                    }
                    index += 1;
                }
            }
        }
    }
    flush_run(&mut out, &mut run);
    flush_number(&mut out, &mut pending);
    out.extend_from_slice(b"] TJ");
    out
}

/// `content` with the ranges of `replacements` written over, in the order of their starts; a
/// range overlapping the one before is left out.
fn replaced(content: &[u8], mut replacements: Vec<(Range<usize>, Vec<u8>)>) -> Vec<u8> {
    replacements.sort_by_key(|(range, _)| range.start);
    let mut out = Vec::with_capacity(content.len());
    let mut copied = 0;
    for (range, bytes) in replacements {
        if range.start < copied || range.end > content.len() {
            continue;
        }
        out.extend_from_slice(&content[copied..range.start]);
        out.push(b' ');
        out.extend_from_slice(&bytes);
        out.push(b' ');
        copied = range.end;
    }
    out.extend_from_slice(&content[copied..]);
    out
}

fn flate(mut dict: Dict, data: &[u8]) -> Object {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    // Writing into a Vec cannot fail.
    let compressed = encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    dict.remove(b"DecodeParms");
    dict.remove(b"Length");
    dict.insert("Filter", Name::from("FlateDecode"));
    Object::Stream(Stream {
        dict,
        data: compressed,
    })
}

/// Write the plan: the page's content anew where it loses text, and a copy of each form drawn
/// where a drawing loses text, its `Do` pointing to the copy under a name of its own.
fn write(
    doc: &mut Document,
    page: ObjId,
    content: &[u8],
    plan: Plan,
) -> sealwax_core::Result<RedactedText> {
    let mut report = RedactedText::default();
    for ops in plan.shown.values() {
        for (_, shown) in ops {
            report.operators += 1;
            report.glyphs += shown.removed.len();
            report.strings_unread += usize::from(shown.unread);
        }
    }
    // The drawings to copy: those losing text, and those drawing them. A drawing comes after
    // the one drawing it.
    let mut changed: Vec<bool> = (0..plan.draws.len())
        .map(|draw| plan.shown.contains_key(&Source::Draw(draw)))
        .collect();
    for draw in (0..plan.draws.len()).rev() {
        if changed[draw]
            && let Source::Draw(parent) = plan.draws[draw].parent
        {
            changed[parent] = true;
        }
    }
    let Some(Object::Dict(mut page_dict)) = doc.object(page)? else {
        return Ok(report);
    };
    let page_resources = match page_dict.get(b"Resources").cloned() {
        Some(resources) => Some(resources),
        None => inherited_resources(doc, &page_dict)?,
    };
    // Each copy and the name its parent draws it by, the drawings met last written first. The
    // names of the page's form XObjects, and of those its forms draw, are taken: a page redacted
    // twice keeps the copies of the first time.
    let mut copies: HashMap<usize, (Vec<u8>, ObjId)> = HashMap::new();
    let mut used_names: HashSet<Vec<u8>> = HashSet::new();
    let mut taken = vec![page_resources.clone()];
    for entry in &plan.draws {
        if let Some(Object::Stream(stream)) = doc.object(entry.form)? {
            taken.push(stream.dict.get(b"Resources").cloned());
        }
    }
    for resources in taken.into_iter().flatten() {
        let resources = match resources {
            Object::Ref(id) => doc.object(id)?,
            other => Some(other),
        };
        let xobjects = match resources
            .as_ref()
            .and_then(|r| r.as_dict())
            .and_then(|r| r.get(b"XObject"))
        {
            Some(Object::Ref(id)) => doc.object(*id)?,
            other => other.cloned(),
        };
        if let Some(Object::Dict(xobjects)) = xobjects {
            used_names.extend(xobjects.iter().map(|(name, _)| name.as_bytes().to_vec()));
        }
    }
    for draw in (0..plan.draws.len()).rev() {
        if !changed[draw] {
            continue;
        }
        let form = plan.draws[draw].form;
        let (Some(Object::Stream(stream)), Some(data)) = (doc.object(form)?, plan.forms.get(&form))
        else {
            continue;
        };
        let source = Source::Draw(draw);
        let resources = if plan.draws[draw].own_resources {
            stream.dict.get(b"Resources").cloned()
        } else {
            effective_resources(doc, &plan, plan.draws[draw].parent, page_resources.as_ref())?
        };
        let (data, resources) = rewrite_content(
            doc,
            &plan,
            source,
            data,
            resources,
            &copies,
            &mut used_names,
        )?;
        let mut dict = stream.dict.clone();
        if let Some(resources) = resources {
            dict.insert("Resources", resources);
        }
        let copy = doc.add_object(flate(dict, &data))?;
        let name = fresh_name(&mut used_names, draw);
        copies.insert(draw, (name, copy));
        report.forms += 1;
    }
    let draws_changed = changed
        .iter()
        .enumerate()
        .any(|(draw, c)| *c && plan.draws[draw].parent == Source::Page);
    if !plan.shown.contains_key(&Source::Page) && !draws_changed {
        return Ok(report);
    }
    let (data, resources) = rewrite_content(
        doc,
        &plan,
        Source::Page,
        content,
        page_resources,
        &copies,
        &mut used_names,
    )?;
    let stream = doc.add_object(flate(Dict::new(), &data))?;
    page_dict.insert("Contents", Object::Ref(stream));
    if let Some(resources) = resources {
        page_dict.insert("Resources", resources);
    }
    doc.set_object(page, Object::Dict(page_dict));
    Ok(report)
}

/// A name for the copy drawing `draw`, no other name of the page's.
fn fresh_name(used: &mut HashSet<Vec<u8>>, draw: usize) -> Vec<u8> {
    let mut suffix = 0;
    loop {
        let name = if suffix == 0 {
            format!("SwRedacted{draw}")
        } else {
            format!("SwRedacted{draw}_{suffix}")
        }
        .into_bytes();
        if used.insert(name.clone()) {
            return name;
        }
        suffix += 1;
    }
}

/// The resources the content of `source` reads, as written: the page's, or a form's own, or
/// those its parent's content reads.
fn effective_resources(
    doc: &Document,
    plan: &Plan,
    mut source: Source,
    page: Option<&Object>,
) -> sealwax_core::Result<Option<Object>> {
    loop {
        match source {
            Source::Page => return Ok(page.cloned()),
            Source::Draw(draw) => {
                let entry = &plan.draws[draw];
                if entry.own_resources {
                    return Ok(match doc.object(entry.form)? {
                        Some(Object::Stream(stream)) => stream.dict.get(b"Resources").cloned(),
                        _ => None,
                    });
                }
                source = entry.parent;
            }
        }
    }
}

/// The content of `source` with its text-showing operators written again and its `Do`s of
/// copied forms drawing the copies, and its resources naming them.
fn rewrite_content(
    doc: &Document,
    plan: &Plan,
    source: Source,
    content: &[u8],
    resources: Option<Object>,
    copies: &HashMap<usize, (Vec<u8>, ObjId)>,
    used_names: &mut HashSet<Vec<u8>>,
) -> sealwax_core::Result<(Vec<u8>, Option<Object>)> {
    let mut replacements: Vec<(Range<usize>, Vec<u8>)> = plan
        .shown
        .get(&source)
        .map(|ops| {
            ops.iter()
                .map(|(range, shown)| (range.clone(), rewritten(shown)))
                .collect()
        })
        .unwrap_or_default();
    let mut added: Vec<(Vec<u8>, ObjId)> = Vec::new();
    for (draw, entry) in plan.draws.iter().enumerate() {
        if entry.parent != source {
            continue;
        }
        if let Some((name, copy)) = copies.get(&draw) {
            let mut call = b"/".to_vec();
            call.extend_from_slice(name);
            call.extend_from_slice(b" Do");
            replacements.push((entry.op.clone(), call));
            added.push((name.clone(), *copy));
        }
    }
    let data = replaced(content, replacements);
    if added.is_empty() {
        return Ok((data, resources));
    }
    let mut dict = match &resources {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Dict(dict)) => dict,
            _ => Dict::new(),
        },
        Some(Object::Dict(dict)) => dict.clone(),
        _ => Dict::new(),
    };
    let mut xobjects = match dict.get(b"XObject") {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Dict(xobjects)) => xobjects,
            _ => Dict::new(),
        },
        Some(Object::Dict(xobjects)) => xobjects.clone(),
        _ => Dict::new(),
    };
    for (name, _) in xobjects.iter() {
        used_names.insert(name.as_bytes().to_vec());
    }
    for (name, copy) in added {
        xobjects.insert(Name::new(name), Object::Ref(copy));
    }
    dict.insert("XObject", Object::Dict(xobjects));
    Ok((data, Some(Object::Dict(dict))))
}

/// The resources a page inherits from its parents.
fn inherited_resources(doc: &Document, page: &Dict) -> sealwax_core::Result<Option<Object>> {
    let mut parent = page.get(b"Parent").and_then(Object::as_ref_id);
    let mut seen = HashSet::new();
    while let Some(id) = parent {
        if !seen.insert(id) {
            break;
        }
        let Some(Object::Dict(node)) = doc.object(id)? else {
            break;
        };
        if let Some(resources) = node.get(b"Resources") {
            return Ok(Some(resources.clone()));
        }
        parent = node.get(b"Parent").and_then(Object::as_ref_id);
    }
    Ok(None)
}
