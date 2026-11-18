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
//! copy, so that the other pages drawing the form keep it whole; what the page no longer draws
//! (the form, an image written anew or taken out) leaves its resources.
//!
//! What stands for content taken out goes with it: the `/ActualText`, `/Alt` and `/E` of the
//! marked-content sequences it was in (readers read them in its place), and of the structure
//! elements those are tied to, with their ancestors.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::ops::Range;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use hayro_font::{Font, FontCache};
use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::font::GlyphRun;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::hayro_syntax::content::TypedIter;
use hayro_interpret::hayro_syntax::object::{
    Array, Dict as HayroDict, ObjectIdentifier, Stream as HayroStream,
};
use hayro_interpret::hayro_syntax::page::Resources;
use hayro_interpret::kurbo::BezPath;
use hayro_interpret::kurbo::{Affine, Point, Rect as KurboRect, Shape as _, Vec2};
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageData, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret,
};
use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Stream};

use crate::scan::{Op, Operand, dict_entries, object_end, operands, operators};

/// Glyph units per em (hayro-font's).
const UNITS: f64 = 1000.0;
/// How far below its baseline a glyph's box goes, and above it, in ems.
const DESCENT: f64 = 0.2;
const ASCENT: f64 = 0.8;
/// How deep form XObjects are followed.
const MAX_DEPTH: usize = 12;
/// The most pixels an image is decoded with to blank some (a page scanned at 600 dpi has 35
/// million): a larger one under an area is taken out whole.
const MAX_BLANKED_PIXELS: u64 = 1 << 26;
/// The texts that stand for content (ISO 32000-1, 14.9.3 to 14.9.5): read in its place, they go
/// when some of it does.
const STANDS_FOR: [&[u8]; 3] = [b"ActualText", b"Alt", b"E"];
/// How many structure elements are climbed from one tied to content, and how deep a number tree
/// is followed and how many of its nodes are read.
const MAX_ANCESTORS: usize = 256;
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_NODES: usize = 4096;

/// What [`redact`] took out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Redacted {
    /// Glyphs taken out.
    pub glyphs: usize,
    /// Text-showing operators written again.
    pub operators: usize,
    /// Form XObjects written anew for the page.
    pub forms: usize,
    /// Strings taken out whole because their font could not be read: where each glyph lies is
    /// not known, only where the string starts.
    pub strings_unread: usize,
    /// Images drawn under an area, written anew with their pixels there blanked.
    pub images: usize,
    /// Images drawn under an area and taken out whole: asked for, or not decoded.
    pub images_removed: usize,
    /// Paths painted under an area, taken out.
    pub paths: usize,
    /// Dictionaries that lost the texts standing for content taken out (`/ActualText`, `/Alt`,
    /// `/E`): the properties of the marked-content sequences it was in, and the structure
    /// elements those are tied to, with their ancestors.
    pub replacement_texts: usize,
}

/// What becomes of an image drawn under an area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageRedaction {
    /// Its pixels under the areas blanked (made black, transparent where it has a soft mask), the
    /// rest kept: the image is written anew, in RGB or gray, 8 bits per component.
    #[default]
    Pixels,
    /// Taken out whole.
    Remove,
    /// Left as it is.
    Keep,
}

/// Which paths painted over an area are taken out (a clipping path keeps clipping).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathRedaction {
    /// Those the areas touch, even in part: a line crossing an area goes whole, on both sides.
    #[default]
    Touched,
    /// Those an area covers whole: a table's border crossing an area stays, a box drawn inside
    /// it goes.
    Covered,
    /// None.
    Keep,
}

/// What [`redact`] takes out under the areas, besides the text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RedactOptions {
    pub images: ImageRedaction,
    pub paths: PathRedaction,
}

impl RedactOptions {
    #[must_use]
    pub fn images(mut self, images: ImageRedaction) -> Self {
        self.images = images;
        self
    }

    #[must_use]
    pub fn paths(mut self, paths: PathRedaction) -> Self {
        self.paths = paths;
        self
    }
}

/// Take out of page `page_index` (from 0) what it draws under `areas`, rectangles in the page's
/// default user space (unrotated, y up), its form XObjects' included (see the module): the
/// glyphs, the images' pixels (or whole images, as `options` asks), the paths painted over an
/// area, and the texts standing for them. Annotations are left as they are: flatten them first
/// to redact what they show.
///
/// A document that lost content is marked redacted ([`Document::mark_redacted`]): it refuses an
/// incremental save, which would keep the bytes it was opened from. Follow with
/// [`Document::rebuilt_for_redaction`] and a full save.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have or an area
/// that is not finite; what [`Document::check_page`] refuses (the limits, the deadline);
/// [`sealwax_core::Error::Malformed`] for a content stream of the page that cannot be decoded
/// (it would be lost); as [`Document::save_incremental`] for a modified document read again;
/// [`sealwax_core::Error::LimitExceeded`] when a new object cannot be numbered.
pub fn redact(
    doc: &mut Document,
    page_index: usize,
    areas: &[Rect],
    options: &RedactOptions,
) -> sealwax_core::Result<Redacted> {
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
        return Ok(Redacted::default());
    }
    // hayro reads the document as it stands: a modified one is read again, its objects keeping
    // their numbers.
    let reread;
    let reader: &Document = if doc.is_modified() {
        reread = doc.reopened_keeping_numbers()?;
        &reread
    } else {
        doc
    };
    let content = page_content(reader, page_id)?;
    let plan = plan(reader, page_index, &content, &areas, *options)?;
    let done = write(doc, page_id, &content, plan)?;
    // What was taken out is still in the bytes the document was opened from.
    if done != Redacted::default() {
        doc.mark_redacted();
    }
    Ok(done)
}

/// The properties written in `BDC` operator `op`.
fn inline_properties(content: &[u8], op: &Op) -> Properties {
    let operands = &content[op.start..op.operator.start];
    let Some((entries, dict)) = dict_entries(operands, object_end(operands, 0)) else {
        return Properties::Inline {
            mcid: None,
            stripped: None,
        };
    };
    let mcid = entries
        .iter()
        .find(|entry| entry.key == b"MCID")
        .and_then(|entry| std::str::from_utf8(&operands[entry.value.clone()]).ok())
        .and_then(|value| value.trim().parse().ok());
    let stripped = entries
        .iter()
        .any(|entry| STANDS_FOR.contains(&entry.key.as_slice()))
        .then(|| {
            let mut out = operands[..dict.start].to_vec();
            out.extend_from_slice(b"<<");
            for entry in &entries {
                if !STANDS_FOR.contains(&entry.key.as_slice()) {
                    out.push(b' ');
                    out.extend_from_slice(&operands[entry.whole.clone()]);
                }
            }
            out.extend_from_slice(b" >>");
            out.extend_from_slice(&operands[dict.end..]);
            out.push(b' ');
            out.extend_from_slice(&content[op.operator.clone()]);
            (op.start..op.end, out)
        });
    Properties::Inline { mcid, stripped }
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
    /// Operators taken out or written otherwise (paths, images removed), by content.
    cuts: HashMap<Source, Vec<Replacement>>,
    /// Images drawn under an area, by content: the `Do` or inline image, and the image anew.
    images: HashMap<Source, Vec<(Range<usize>, Blanked)>>,
    paths: usize,
    images_removed: usize,
    /// The marked-content sequences met (`BDC`).
    sequences: Vec<Sequence>,
}

/// A marked-content sequence, and whether content of it was taken out.
#[derive(Debug, Clone)]
struct Sequence {
    source: Source,
    properties: Properties,
    lost: bool,
}

/// The properties of a marked-content sequence.
#[derive(Debug, Clone)]
enum Properties {
    /// Written in its `BDC`: its `/MCID`, and the operator written without the texts standing
    /// for content, when it has some.
    Inline {
        mcid: Option<i64>,
        stripped: Option<(Range<usize>, Vec<u8>)>,
    },
    /// Named in the resources' `/Properties`.
    Named(Vec<u8>),
}

/// Bytes of a content, and what is written in their place.
type Replacement = (Range<usize>, Vec<u8>);

/// An image whose pixels under the areas are blanked: its samples as decoded, until they are
/// blanked; then compressed (`FlateDecode`), so that a page of many images does not hold them
/// all decoded. A stencil mask is then 1 bit per pixel, 0 painting.
#[derive(Debug, Clone)]
struct Blanked {
    width: u32,
    height: u32,
    /// 3 (RGB), 1 (gray), 0 (a stencil mask: 1 marks painted).
    channels: u8,
    data: Vec<u8>,
    /// The soft mask, its own size.
    alpha: Option<(u32, u32, Vec<u8>)>,
    interpolate: bool,
}

/// The text state, as far as placing glyphs goes (ISO 32000-1, 9.3).
#[derive(Clone)]
struct State<'a> {
    ctm: Affine,
    line_width: f64,
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
            line_width: 1.0,
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
    options: RedactOptions,
    /// The most pixels an image is decoded with.
    max_pixels: u64,
    /// The pixels still to decode for this page: four of the largest images, then images under
    /// an area are taken out whole.
    budget: u64,
    pdf: &'a Pdf,
    cache: InterpreterCache<'a>,
    fonts: FontCache<'a>,
    settings: InterpreterSettings,
    plan: Plan,
    /// Shown operators of each stream, by operator index, while it is read.
    shown: HashMap<Source, HashMap<usize, (Range<usize>, Shown)>>,
    /// The marked-content sequences open, through the forms drawn: indexes into the plan's
    /// sequences (`None` for a `BMC`, which has no properties).
    marked: Vec<Option<usize>>,
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
    options: RedactOptions,
) -> sealwax_core::Result<Plan> {
    let pdf = reader.hayro();
    let pages = pdf.pages();
    let Some(page) = pages.get(page_index) else {
        return Err(sealwax_core::Error::InvalidArgument { what: "page index" });
    };
    let mut walker = Walker {
        areas,
        options,
        max_pixels: reader.limits().max_image_pixels.min(MAX_BLANKED_PIXELS),
        budget: 4 * reader.limits().max_image_pixels.min(MAX_BLANKED_PIXELS),
        pdf,
        cache: InterpreterCache::new(),
        fonts: FontCache::new(),
        settings: InterpreterSettings::default(),
        plan: Plan::default(),
        shown: HashMap::new(),
        marked: Vec::new(),
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
        // The path being built: its operators (another may come between them, a text object
        // drawn in the middle of a path), the box of its points in user space, and whether it
        // clips.
        let mut path: Option<(Vec<Range<usize>>, Option<KurboRect>)> = None;
        let mut clips = false;
        // The sequences open when this content starts are its drawing's; those it leaves open
        // close with it.
        let base = self.marked.len();
        for (index, op) in operators(content).iter().enumerate() {
            let operator = &content[op.operator.clone()];
            let args = operands(&content[op.start..op.operator.start]);
            let number = |at: usize| -> Option<f64> {
                match args.get(at) {
                    Some(Operand::Number(n)) => Some(*n),
                    _ => None,
                }
            };
            let points: Vec<(f64, f64)> = match operator {
                b"m" | b"l" | b"c" | b"v" | b"y" => {
                    let values: Vec<f64> = (0..args.len()).filter_map(number).collect();
                    values
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|[x, y]| (*x, *y))
                        .collect()
                }
                b"re" => match [0, 1, 2, 3].map(number) {
                    [Some(x), Some(y), Some(w), Some(h)] => {
                        vec![(x, y), (x + w, y), (x, y + h), (x + w, y + h)]
                    }
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            if matches!(operator, b"m" | b"l" | b"c" | b"v" | b"y" | b"re" | b"h") {
                let (ops, bounds) = path.get_or_insert((Vec::new(), None));
                ops.push(op.start..op.end);
                for (x, y) in points {
                    let p = state.ctm * Point::new(x, y);
                    if p.x.is_finite() && p.y.is_finite() {
                        let point = KurboRect::from_points(p, p);
                        *bounds = Some(bounds.map_or(point, |b| b.union(point)));
                    }
                }
                continue;
            }
            match operator {
                b"W" | b"W*" => clips = true,
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                    if let Some((built, bounds)) = path.take()
                        && operator != b"n"
                        && self.options.paths != PathRedaction::Keep
                        && let Some(bounds) = bounds
                    {
                        let stroked = matches!(operator, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
                        // Half the line's width in user space; a line of no width is drawn a
                        // device pixel wide.
                        let half = if stroked {
                            (state.line_width * state.ctm.determinant().abs().sqrt() / 2.0).max(0.5)
                        } else {
                            0.0
                        };
                        let painted = bounds.inflate(half, half);
                        let taken = self.areas.iter().any(|area| {
                            if self.options.paths == PathRedaction::Covered {
                                area.contains_rect(painted)
                            } else {
                                let overlap = area.intersect(painted);
                                overlap.width() > 0.0 && overlap.height() > 0.0
                            }
                        });
                        if taken {
                            // A clipping path keeps clipping, painting nothing; any other goes,
                            // operator by operator.
                            let cuts = self.plan.cuts.entry(source).or_default();
                            if clips {
                                cuts.push((op.start..op.end, b"n".to_vec()));
                            } else {
                                cuts.extend(built.into_iter().map(|range| (range, Vec::new())));
                                cuts.push((op.start..op.end, Vec::new()));
                            }
                            self.plan.paths += 1;
                            self.lose();
                        }
                    }
                    path = None;
                    clips = false;
                }
                b"w" => state.line_width = number(0).unwrap_or(state.line_width),
                b"BMC" => self.marked.push(None),
                b"BDC" => {
                    let properties = match args.get(1) {
                        Some(Operand::Name(name)) => Properties::Named(name.clone()),
                        _ => inline_properties(content, op),
                    };
                    self.marked.push(Some(self.plan.sequences.len()));
                    self.plan.sequences.push(Sequence {
                        source,
                        properties,
                        lost: false,
                    });
                }
                b"EMC" => {
                    if self.marked.len() > base {
                        self.marked.pop();
                    }
                }
                b"BI" => self.image(source, op, &content[op.start..op.end], resources, &state),
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
                    let subtype =
                        dict.get::<hayro_interpret::hayro_syntax::object::Name>(b"Subtype");
                    if subtype.as_ref().is_some_and(|s| s.as_ref() == b"Image") {
                        self.image(source, op, &content[op.start..op.end], resources, &state);
                        continue;
                    }
                    if subtype.is_none_or(|subtype| subtype.as_ref() != b"Form") {
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
        self.marked.truncate(base);
    }

    /// Content of the sequences open was taken out.
    fn lose(&mut self) {
        for index in self.marked.iter().flatten() {
            self.plan.sequences[*index].lost = true;
        }
    }

    /// An image drawn (`drawing` is its `Do`, or the inline image itself): under an area, its
    /// pixels there blanked, or the image taken out, as the options ask.
    fn image(
        &mut self,
        source: Source,
        op: &Op,
        drawing: &[u8],
        resources: &Resources<'a>,
        state: &State<'a>,
    ) {
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
            .map(|(x, y)| state.ctm * Point::new(x, y));
        if self.options.images == ImageRedaction::Keep || !covered(&corners, self.areas) {
            return;
        }
        let grabbed = if self.options.images == ImageRedaction::Pixels {
            self.grab(drawing, resources).filter(|image| {
                let pixels = u64::from(image.width) * u64::from(image.height);
                let within = pixels <= self.budget;
                self.budget = self.budget.saturating_sub(pixels);
                within
            })
        } else {
            None
        };
        let Some(mut image) = grabbed else {
            // Not decoded, or to be taken out: taken out whole.
            self.plan
                .cuts
                .entry(source)
                .or_default()
                .push((op.start..op.end, Vec::new()));
            self.plan.images_removed += 1;
            self.lose();
            return;
        };
        // The areas in the image's unit square, then in its pixels: the first row on top.
        let inverse = state.ctm.inverse();
        for area in self.areas {
            let corners = [
                Point::new(area.x0, area.y0),
                Point::new(area.x1, area.y0),
                Point::new(area.x0, area.y1),
                Point::new(area.x1, area.y1),
            ]
            .map(|p| inverse * p);
            let (mut u0, mut v0, mut u1, mut v1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for p in corners {
                (u0, v0, u1, v1) = (u0.min(p.x), v0.min(p.y), u1.max(p.x), v1.max(p.y));
            }
            let region = (u0.max(0.0), v0.max(0.0), u1.min(1.0), v1.min(1.0));
            if !(region.2 > region.0 && region.3 > region.1) {
                continue;
            }
            blank(
                &mut image.data,
                image.width,
                image.height,
                image.channels,
                region,
            );
            if let Some((width, height, alpha)) = image.alpha.as_mut() {
                blank(alpha, *width, *height, 1, region);
            }
        }
        if image.channels == 0 {
            image.data = packed_stencil(&image.data, image.width);
        }
        image.data = compressed(&image.data);
        if let Some((_, _, alpha)) = image.alpha.as_mut() {
            *alpha = compressed(alpha);
        }
        self.plan
            .images
            .entry(source)
            .or_default()
            .push((op.start..op.end, image));
        self.lose();
    }

    /// The pixels of the image `drawing` draws, as hayro decodes them for rendering.
    fn grab(&self, drawing: &[u8], resources: &Resources<'a>) -> Option<Blanked> {
        let mut grabber = Grabber {
            max_pixels: self.max_pixels,
            found: None,
        };
        let mut context = Context::new(
            Affine::IDENTITY,
            KurboRect::new(-1e9, -1e9, 1e9, 1e9),
            &self.cache,
            self.pdf.xref(),
            InterpreterSettings::default(),
        );
        let _ = catch_unwind(AssertUnwindSafe(|| {
            interpret(
                TypedIter::new(drawing),
                resources,
                &mut context,
                &mut grabber,
            );
        }));
        grabber.found
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
        if !removed.is_empty() {
            self.lose();
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

/// Blank the pixels of `region` (of the unit square, y up) in `data`: `channels` samples of 8 bits
/// per pixel (0 for a stencil of one sample per pixel, 1 marking painted, made 0).
fn blank(data: &mut [u8], width: u32, height: u32, channels: u8, region: (f64, f64, f64, f64)) {
    let (u0, v0, u1, v1) = region;
    let (w, h) = (f64::from(width), f64::from(height));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let span = |a: f64, b: f64, size: f64| -> (usize, usize) {
        (
            (a * size).floor().max(0.0) as usize,
            (b * size).ceil().min(size) as usize,
        )
    };
    let (x0, x1) = span(u0, u1, w);
    // The first row is the top of the unit square.
    let (y0, y1) = span(1.0 - v1, 1.0 - v0, h);
    let step = usize::from(channels.max(1));
    let row = width as usize * step;
    for y in y0..y1 {
        let start = y * row + x0 * step;
        let end = (y * row + x1 * step).min(data.len());
        if start < end {
            data[start..end].fill(0);
        }
    }
}

/// A device keeping the first image drawn, decoded, when it has at most `max_pixels`.
struct Grabber {
    max_pixels: u64,
    found: Option<Blanked>,
}

impl<'a> Device<'a> for Grabber {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_image(&mut self, image: Image<'a, '_>, _: ImageDrawProps<'a>) {
        if self.found.is_some()
            || u64::from(image.width()) * u64::from(image.height()) > self.max_pixels
        {
            return;
        }
        match image {
            Image::Raster(raster) => raster.with_rgba(
                |data, alpha| {
                    let interpolate = data.interpolate();
                    let (width, height) = (data.width(), data.height());
                    // Decoded larger than its dictionary said.
                    if u64::from(width) * u64::from(height) > self.max_pixels {
                        return;
                    }
                    let (channels, data) = match data {
                        ImageData::Rgb(rgb) => (3, rgb.data),
                        ImageData::Luma(luma) => (1, luma.data),
                    };
                    self.found = Some(Blanked {
                        width,
                        height,
                        channels,
                        data,
                        alpha: alpha.map(|a| (a.width, a.height, a.data)),
                        interpolate,
                    });
                },
                None,
            ),
            Image::Stencil(stencil) => stencil.with_stencil(
                |luma, _| {
                    if u64::from(luma.width) * u64::from(luma.height) > self.max_pixels {
                        return;
                    }
                    self.found = Some(Blanked {
                        width: luma.width,
                        height: luma.height,
                        channels: 0,
                        data: luma.data,
                        alpha: None,
                        interpolate: luma.interpolate,
                    });
                },
                None,
            ),
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

/// A stencil of one sample per pixel (128 and over painted) as 1 bit per pixel, rows of whole
/// bytes, 0 painting.
fn packed_stencil(samples: &[u8], width: u32) -> Vec<u8> {
    let width = width as usize;
    let mut rows = Vec::with_capacity(samples.len() / 8 + 1);
    for row in samples.chunks(width.max(1)) {
        for byte in row.chunks(8) {
            let mut bits = 0xFFu8;
            for (at, value) in byte.iter().enumerate() {
                if *value >= 128 {
                    bits &= !(0x80 >> at);
                }
            }
            rows.push(bits);
        }
    }
    rows
}

/// `data` compressed with zlib.
fn compressed(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    // Writing into a Vec cannot fail.
    encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .unwrap_or_default()
}

/// A stream of data already compressed with zlib.
fn compressed_stream(mut dict: Dict, data: Vec<u8>) -> Object {
    dict.insert("Filter", Name::from("FlateDecode"));
    Object::Stream(Stream { dict, data })
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
    mut plan: Plan,
) -> sealwax_core::Result<Redacted> {
    let mut report = Redacted {
        paths: plan.paths,
        images_removed: plan.images_removed,
        images: plan.images.values().map(Vec::len).sum(),
        ..Redacted::default()
    };
    for ops in plan.shown.values() {
        for (_, shown) in ops {
            report.operators += 1;
            report.glyphs += shown.removed.len();
            report.strings_unread += usize::from(shown.unread);
        }
    }
    report.replacement_texts = unmark(doc, page, &mut plan)?;
    // The drawings to copy: those losing text, and those drawing them. A drawing comes after
    // the one drawing it.
    let touched = |source: &Source| {
        plan.shown.contains_key(source)
            || plan.cuts.contains_key(source)
            || plan.images.contains_key(source)
    };
    let mut changed: Vec<bool> = (0..plan.draws.len())
        .map(|draw| touched(&Source::Draw(draw)))
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
    if !touched(&Source::Page) && !draws_changed {
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

/// A name for an image written anew, no other name of the page's.
fn fresh_image_name(used: &mut HashSet<Vec<u8>>) -> Vec<u8> {
    (0..)
        .map(|n| format!("SwImage{n}").into_bytes())
        .find(|name| used.insert(name.clone()))
        .unwrap_or_default()
}

/// The image written anew: RGB or gray, 8 bits per component, its soft mask beside it; a stencil
/// mask of 1 bit per pixel, 0 painting.
fn image_object(doc: &mut Document, image: &Blanked) -> sealwax_core::Result<ObjId> {
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("XObject"));
    dict.insert("Subtype", Name::from("Image"));
    dict.insert("Width", i64::from(image.width));
    dict.insert("Height", i64::from(image.height));
    if image.interpolate {
        dict.insert("Interpolate", Object::Bool(true));
    }
    if image.channels == 0 {
        dict.insert("ImageMask", Object::Bool(true));
        dict.insert("BitsPerComponent", 1_i64);
    } else {
        dict.insert(
            "ColorSpace",
            Name::from(if image.channels == 3 {
                "DeviceRGB"
            } else {
                "DeviceGray"
            }),
        );
        dict.insert("BitsPerComponent", 8_i64);
    }
    if let Some((width, height, alpha)) = &image.alpha {
        let mut mask = Dict::new();
        mask.insert("Type", Name::from("XObject"));
        mask.insert("Subtype", Name::from("Image"));
        mask.insert("Width", i64::from(*width));
        mask.insert("Height", i64::from(*height));
        mask.insert("ColorSpace", Name::from("DeviceGray"));
        mask.insert("BitsPerComponent", 8_i64);
        let mask = doc.add_object(compressed_stream(mask, alpha.clone()))?;
        dict.insert("SMask", Object::Ref(mask));
    }
    doc.add_object(compressed_stream(dict, image.data.clone()))
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
    doc: &mut Document,
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
    if let Some(cuts) = plan.cuts.get(&source) {
        replacements.extend(cuts.iter().cloned());
    }
    let mut added: Vec<(Vec<u8>, ObjId)> = Vec::new();
    for (range, image) in plan.images.get(&source).into_iter().flatten() {
        let id = image_object(doc, image)?;
        let name = fresh_image_name(used_names);
        let mut call = b"/".to_vec();
        call.extend_from_slice(&name);
        call.extend_from_slice(b" Do");
        replacements.push((range.clone(), call));
        added.push((name, id));
    }
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
    // What the content no longer draws (an image written anew or taken out, a form copied)
    // leaves its resources: the page would still reach it, and it holds what was taken out.
    // Unless a form reading these resources draws it.
    let mut needed = drawn_names(&data);
    for entry in &plan.draws {
        if !entry.own_resources
            && resources_source(plan, entry.parent) == source
            && let Some(form) = plan.forms.get(&entry.form)
        {
            needed.extend(drawn_names(form));
        }
    }
    let gone: Vec<Vec<u8>> = drawn_names(content)
        .into_iter()
        .filter(|name| !needed.contains(name))
        .collect();
    if added.is_empty() && gone.is_empty() {
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
    for name in &gone {
        xobjects.remove(name);
    }
    for (name, copy) in added {
        xobjects.insert(Name::new(name), Object::Ref(copy));
    }
    dict.insert("XObject", Object::Dict(xobjects));
    Ok((data, Some(Object::Dict(dict))))
}

/// Take the texts standing for content off the marked-content sequences that lost some, and off
/// the structure elements they are tied to; how many dictionaries lost them. Written in its
/// `BDC`, the operator is written again; named in the resources, or on a structure element, the
/// dictionary loses them where it is, for whatever else names it too: they stand for no content
/// that shows.
fn unmark(doc: &mut Document, page: ObjId, plan: &mut Plan) -> sealwax_core::Result<usize> {
    let mut count = 0;
    let mut tied: Vec<(Source, i64)> = Vec::new();
    let lost: Vec<Sequence> = plan.sequences.iter().filter(|s| s.lost).cloned().collect();
    for sequence in lost {
        match sequence.properties {
            Properties::Inline { mcid, stripped } => {
                if let Some(cut) = stripped {
                    plan.cuts.entry(sequence.source).or_default().push(cut);
                    count += 1;
                }
                tied.extend(mcid.map(|mcid| (sequence.source, mcid)));
            }
            Properties::Named(name) => {
                let Some(holder) = resources_holder(doc, plan, page, sequence.source)? else {
                    continue;
                };
                let mut mcid = None;
                let path: [&[u8]; 3] = [b"Resources", b"Properties", &name];
                let changed = edit_at(doc, holder, &path, &mut |dict| {
                    mcid = dict.get(b"MCID").and_then(Object::as_i64);
                    strip(dict)
                })?;
                count += usize::from(changed);
                tied.extend(mcid.map(|mcid| (sequence.source, mcid)));
            }
        }
    }
    Ok(count + untie(doc, plan, page, &tied)?)
}

/// Take the texts standing for content off `dict`: whether it had some.
fn strip(dict: &mut Dict) -> bool {
    let mut had = false;
    for key in STANDS_FOR {
        had |= dict.remove(key).is_some();
    }
    had
}

/// `object`, read through a reference.
fn resolved(doc: &Document, object: Option<&Object>) -> sealwax_core::Result<Option<Object>> {
    Ok(match object {
        Some(Object::Ref(id)) => doc.object(*id)?,
        other => other.cloned(),
    })
}

/// Change, with `edit`, the dictionary `path` leads to from object `id` (a dictionary or a
/// stream's), through references; written back where it lies. Whether `edit` changed it.
fn edit_at(
    doc: &mut Document,
    id: ObjId,
    path: &[&[u8]],
    edit: &mut dyn FnMut(&mut Dict) -> bool,
) -> sealwax_core::Result<bool> {
    match doc.object(id)? {
        Some(Object::Dict(mut dict)) => {
            let changed = edit_in(doc, &mut dict, path, edit)?;
            if changed {
                doc.set_object(id, Object::Dict(dict));
            }
            Ok(changed)
        }
        Some(Object::Stream(mut stream)) => {
            let changed = edit_in(doc, &mut stream.dict, path, edit)?;
            if changed {
                doc.set_object(id, Object::Stream(stream));
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

fn edit_in(
    doc: &mut Document,
    dict: &mut Dict,
    path: &[&[u8]],
    edit: &mut dyn FnMut(&mut Dict) -> bool,
) -> sealwax_core::Result<bool> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(edit(dict));
    };
    match dict.get(first).cloned() {
        Some(Object::Ref(id)) => edit_at(doc, id, rest, edit),
        Some(Object::Dict(mut inner)) => {
            let changed = edit_in(doc, &mut inner, rest, edit)?;
            if changed {
                dict.insert(Name::new(first.to_vec()), Object::Dict(inner));
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

/// The object whose `/Resources` the content of `source` reads: the page, or the node of the
/// page tree it inherits them from; a form, or what draws it when it has none.
fn resources_holder(
    doc: &Document,
    plan: &Plan,
    page: ObjId,
    source: Source,
) -> sealwax_core::Result<Option<ObjId>> {
    if let Source::Draw(draw) = resources_source(plan, source) {
        return Ok(Some(plan.draws[draw].form));
    }
    let mut node = Some(page);
    let mut seen = HashSet::new();
    while let Some(id) = node {
        if !seen.insert(id) {
            break;
        }
        let Some(Object::Dict(dict)) = doc.object(id)? else {
            break;
        };
        if dict.get(b"Resources").is_some() {
            return Ok(Some(id));
        }
        node = dict.get(b"Parent").and_then(Object::as_ref_id);
    }
    Ok(None)
}

/// Take the texts standing for content off the structure elements the sequences `tied` (their
/// content, their `/MCID`) are tied to by the structure tree's `/ParentTree`, and off the
/// elements above them, whose texts stand for their children's content too; how many lost some.
fn untie(
    doc: &mut Document,
    plan: &Plan,
    page: ObjId,
    tied: &[(Source, i64)],
) -> sealwax_core::Result<usize> {
    if tied.is_empty() {
        return Ok(0);
    }
    let catalog = doc.object(doc.catalog_id())?;
    let root = resolved(
        doc,
        catalog
            .as_ref()
            .and_then(|c| c.as_dict()?.get(b"StructTreeRoot")),
    )?;
    let tree = resolved(
        doc,
        root.as_ref().and_then(|r| r.as_dict()?.get(b"ParentTree")),
    )?;
    let Some(Object::Dict(tree)) = tree else {
        return Ok(0);
    };
    let mut elements = Vec::new();
    for (source, mcid) in tied {
        let holder = match source {
            Source::Page => page,
            Source::Draw(draw) => plan.draws[*draw].form,
        };
        let key = match doc.object(holder)? {
            Some(Object::Dict(dict)) => dict.get(b"StructParents").and_then(Object::as_i64),
            Some(Object::Stream(stream)) => {
                stream.dict.get(b"StructParents").and_then(Object::as_i64)
            }
            _ => None,
        };
        let Some(key) = key else {
            continue;
        };
        let items = number_tree_get(doc, &tree, key)?;
        let Some(Object::Array(items)) = resolved(doc, items.as_ref())? else {
            continue;
        };
        if let Some(Object::Ref(element)) = usize::try_from(*mcid).ok().and_then(|i| items.get(i)) {
            elements.push(*element);
        }
    }
    let mut seen = HashSet::new();
    let mut count = 0;
    for element in elements {
        let mut next = Some(element);
        for _ in 0..MAX_ANCESTORS {
            let Some(id) = next.filter(|id| seen.insert(*id)) else {
                break;
            };
            let Some(Object::Dict(mut dict)) = doc.object(id)? else {
                break;
            };
            if dict.get(b"Type").and_then(Object::as_name) == Some(b"StructTreeRoot") {
                break;
            }
            next = dict.get(b"P").and_then(Object::as_ref_id);
            if strip(&mut dict) {
                doc.set_object(id, Object::Dict(dict));
                count += 1;
            }
        }
    }
    Ok(count)
}

/// The value of `key` in the number tree `root`.
fn number_tree_get(doc: &Document, root: &Dict, key: i64) -> sealwax_core::Result<Option<Object>> {
    let mut stack = vec![(root.clone(), 0)];
    let mut read = 0;
    while let Some((node, depth)) = stack.pop() {
        if let Some(Object::Array(nums)) = resolved(doc, node.get(b"Nums"))?
            && let Some(pair) = nums
                .as_chunks::<2>()
                .0
                .iter()
                .find(|[k, _]| k.as_i64() == Some(key))
        {
            return Ok(Some(pair[1].clone()));
        }
        if depth >= MAX_TREE_DEPTH {
            continue;
        }
        let Some(Object::Array(kids)) = resolved(doc, node.get(b"Kids"))? else {
            continue;
        };
        for kid in kids.iter().rev() {
            read += 1;
            if read > MAX_TREE_NODES {
                return Ok(None);
            }
            let Some(Object::Dict(kid)) = resolved(doc, Some(kid))? else {
                continue;
            };
            let within = match resolved(doc, kid.get(b"Limits"))? {
                Some(Object::Array(limits)) => match limits.as_slice() {
                    [low, high] => match (low.as_i64(), high.as_i64()) {
                        (Some(low), Some(high)) => low <= key && key <= high,
                        _ => true,
                    },
                    _ => true,
                },
                _ => true,
            };
            if within {
                stack.push((kid, depth + 1));
            }
        }
    }
    Ok(None)
}

/// The names content `data` draws (`Do`).
fn drawn_names(data: &[u8]) -> HashSet<Vec<u8>> {
    operators(data)
        .iter()
        .filter(|op| &data[op.operator.clone()] == b"Do")
        .filter_map(
            |op| match operands(&data[op.start..op.operator.start]).pop() {
                Some(Operand::Name(name)) => Some(name),
                _ => None,
            },
        )
        .collect()
}

/// The content whose resources `source` reads: itself, or what draws it when it has none.
fn resources_source(plan: &Plan, mut source: Source) -> Source {
    while let Source::Draw(draw) = source {
        if plan.draws[draw].own_resources {
            break;
        }
        source = plan.draws[draw].parent;
    }
    source
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
