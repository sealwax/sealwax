//! What sealwax may spend on one document.
//! Past these bounds an operation fails with [`Error::LimitExceeded`] or [`Error::Timeout`], the
//! same way each time for the same input, rather than exhaust the memory or the time of the
//! process that runs it.
//!
//! The bounds that hold whatever the time are checked before a page is interpreted (rendered,
//! tagged): every content stream drawing the page reads is walked once, and what drawing the page
//! costs is added up without drawing it, forms drawn ten times in forms drawn ten times included.
//! What the interpreter would build from a font, an image or a shading is bounded too.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use hayro_syntax::Filter;
use hayro_syntax::content::{Instruction, UntypedIter};
use hayro_syntax::object::{Array, Dict, Name, Object, ObjectIdentifier, Stream};
use hayro_syntax::page::{Page, Resources};

use crate::decode::{Step, decoded_size};
use crate::error::{Error, Result};

/// How many operators pass between two looks at the clock.
const TICKS_PER_CLOCK: u32 = 4096;
/// The most bytes one byte of `FlateDecode` data decodes to: a 258-byte match in under two bits.
const DEFLATE_RATIO: u64 = 1032;
/// The most bytes one byte of `LZWDecode` data decodes to: a 12-bit code for a string of up to
/// 3,838 bytes.
const LZW_RATIO: u64 = 2560;

/// Bounds on what an operation spends on a document. The defaults are far above what real
/// documents need, and far below what exhausts a worker: measured on 4,124 documents (dev and
/// public corpora, 16/11/2026), a content stream had at most 430,000 operators, a page cost at
/// most 480,000, a text operator showed at most 65,538 bytes, an image had at most 212 million
/// pixels and a mesh shading 72 KB.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Limits {
    /// Operators in one content stream.
    pub max_operators: u64,
    /// Bytes one text-showing operator shows: the interpreter makes a glyph of each code.
    pub max_shown_bytes: u64,
    /// What drawing one page costs: its operators and the bytes it shows, with those of each
    /// form, tiling pattern, Type 3 glyph and annotation appearance it draws, counted each time it
    /// draws them.
    pub max_page_cost: u64,
    /// Pixels of one image, as its dictionary declares them: the interpreter decodes an image to
    /// that size.
    pub max_image_pixels: u64,
    /// Decoded bytes of one mesh shading: the interpreter keeps a triangle for every few bytes.
    pub max_mesh_bytes: u64,
    /// Codes one range of a CID font's widths (`/W`, `/W2`) spans: the interpreter keeps a width
    /// for each.
    pub max_code_range: u64,
    /// Bytes one stream the interpreter reads may decode to: content, fonts, meshes. An image
    /// may decode to eight bytes a pixel when that is more. The interpreter decodes a stream
    /// whole, and a few kilobytes of `FlateDecode` can decode to gigabytes.
    pub max_stream_bytes: u64,
    /// The shortest period of a dash pattern, in user space units: stroking draws one segment
    /// per period, a hundred million for a page-long line with a period of 10⁻⁵.
    pub min_dash_period: f64,
    /// Annotations on one page: flattening draws each, and a page can list one annotation a
    /// million times in a few megabytes.
    pub max_annotations: u64,
    /// Pixels of one page rendered to an image: the renderer holds four bytes for each. An A0
    /// poster at 150 pixels per inch has 35 million.
    pub max_render_pixels: u64,
    /// When to give up: an operation still running then fails with [`Error::Timeout`].
    pub deadline: Option<Instant>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_operators: 10_000_000,
            max_shown_bytes: 1 << 20,
            max_page_cost: 20_000_000,
            max_image_pixels: 1 << 28,
            max_mesh_bytes: 16 << 20,
            max_code_range: 1 << 16,
            max_stream_bytes: 256 << 20,
            min_dash_period: 1e-3,
            max_annotations: 50_000,
            max_render_pixels: 50_000_000,
            deadline: None,
        }
    }
}

impl Limits {
    /// No bound at all, no deadline: for documents sealwax can trust, and for measuring.
    pub fn unbounded() -> Self {
        Self {
            max_operators: u64::MAX,
            max_shown_bytes: u64::MAX,
            max_page_cost: u64::MAX,
            max_image_pixels: u64::MAX,
            max_mesh_bytes: u64::MAX,
            max_code_range: u64::MAX,
            max_stream_bytes: u64::MAX,
            min_dash_period: 0.0,
            max_annotations: u64::MAX,
            max_render_pixels: u64::MAX,
            deadline: None,
        }
    }

    /// Give up `after` from now.
    pub fn timeout(mut self, after: Duration) -> Self {
        self.deadline = Instant::now().checked_add(after);
        self
    }

    /// Check that `stream` decodes to at most [`Limits::max_stream_bytes`], at every stage of its
    /// filters, before anything decodes it whole: a stream whose filters cannot reach the bound
    /// is not decoded; another is decoded as a stream, without keeping its bytes, each stage
    /// stopped past the bound. An image filter last is bounded by the pixels its dictionary
    /// declares; anywhere else, the stream is refused.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`].
    pub fn check_stream(&self, stream: &Stream<'_>) -> Result<()> {
        self.check_stream_within(stream, self.max_stream_bytes)
            .map(|_| ())
    }

    /// As [`Limits::check_stream`] with bound `at_most`; returns the bytes counted, if counted.
    fn check_stream_within(&self, stream: &Stream<'_>, at_most: u64) -> Result<Option<u64>> {
        let filters = stream.filters();
        let raw = stream.raw_data();
        let raw_len = u64::try_from(raw.len()).unwrap_or(u64::MAX);
        let mut bound = raw_len;
        for filter in &filters {
            bound = bound.saturating_mul(match filter {
                Filter::FlateDecode => DEFLATE_RATIO,
                Filter::LzwDecode => LZW_RATIO,
                Filter::RunLengthDecode => 64,
                Filter::Ascii85Decode => 4,
                // Images are bounded by their pixels.
                _ => 1,
            });
        }
        if bound <= at_most {
            return Ok(None);
        }
        let mut steps = Vec::with_capacity(filters.len());
        for (index, filter) in filters.iter().enumerate() {
            steps.push(match filter {
                Filter::FlateDecode => Step::Flate,
                Filter::LzwDecode => Step::Lzw {
                    early_change: early_change(stream, index),
                },
                Filter::RunLengthDecode => Step::RunLength,
                Filter::Ascii85Decode => Step::Ascii85,
                Filter::AsciiHexDecode => Step::AsciiHex,
                // `raw_data` is already decrypted.
                Filter::Crypt if index == 0 => continue,
                // An image filter, last, decodes to the pixels its dictionary declares, bounded
                // apart. Elsewhere in a chain it is nothing sealwax can bound.
                _ if index + 1 == filters.len() => continue,
                _ => return Err(exceeded("max_stream_bytes")),
            });
        }
        match decoded_size(&raw, &steps, at_most) {
            Some(counted) => Ok(Some(counted)),
            None => Err(exceeded("max_stream_bytes")),
        }
    }

    /// [`Error::Timeout`] once the deadline has passed.
    ///
    /// # Errors
    /// [`Error::Timeout`].
    pub fn check_time(&self) -> Result<()> {
        match self.deadline {
            Some(deadline) if Instant::now() >= deadline => Err(Error::Timeout),
            _ => Ok(()),
        }
    }
}

/// The most interpreting pages spends on each bound of [`Limits`] but time: what calibrates them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Peaks {
    /// Operators in a content stream.
    pub operators: u64,
    /// Bytes a text operator showed.
    pub shown_bytes: u64,
    /// What drawing a page cost.
    pub page_cost: u64,
    /// Pixels of an image.
    pub image_pixels: u64,
    /// Decoded bytes of a mesh shading.
    pub mesh_bytes: u64,
    /// Codes a range of CID font widths spanned.
    pub code_range: u64,
    /// Bytes a stream decoded to, among those counted or decoded.
    pub stream_bytes: u64,
    /// The shortest dash period set, if any.
    pub dash_period: Option<f64>,
    /// Annotations on a page.
    pub annotations: u64,
}

fn exceeded(what: &'static str) -> Error {
    Error::LimitExceeded { what }
}

/// Walks what drawing pages reads, each stream once, and adds up what drawing them costs.
pub(crate) struct Walker<'a> {
    limits: Limits,
    /// What drawing each form, tiling pattern or appearance costs, once known.
    costs: HashMap<ObjectIdentifier, u64>,
    /// Those being walked: one drawn again before its walk is done draws itself.
    open: HashSet<ObjectIdentifier>,
    /// What drawing one glyph of each font checked costs: its glyph procedures' for a Type 3
    /// font, none for another.
    fonts: HashMap<ObjectIdentifier, u64>,
    ticks: u32,
    pub(crate) peaks: Peaks,
    _pages: std::marker::PhantomData<Page<'a>>,
}

impl<'a> Walker<'a> {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            costs: HashMap::new(),
            open: HashSet::new(),
            fonts: HashMap::new(),
            ticks: 0,
            peaks: Peaks::default(),
            _pages: std::marker::PhantomData,
        }
    }

    /// What drawing `page` costs, its annotations' appearances included.
    pub(crate) fn page(&mut self, page: &Page<'a>) -> Result<u64> {
        let bound = self.limits.max_stream_bytes;
        match page.raw().get::<Object<'a>>(b"Contents") {
            Some(Object::Stream(contents)) => self.stream(&contents, bound)?,
            Some(Object::Array(parts)) => {
                for part in parts.iter::<Stream<'a>>() {
                    self.stream(&part, bound)?;
                }
            }
            _ => {}
        }
        let mut cost = match page.page_stream() {
            Some(data) => {
                let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
                self.peaks.stream_bytes = self.peaks.stream_bytes.max(bytes);
                self.content(data, page.resources())?
            }
            None => 0,
        };
        if let Some(annotations) = page.raw().get::<Array<'a>>(b"Annots") {
            let count = u64::try_from(annotations.raw_iter().count()).unwrap_or(u64::MAX);
            self.peaks.annotations = self.peaks.annotations.max(count);
            if count > self.limits.max_annotations {
                return Err(exceeded("max_annotations"));
            }
            for annotation in annotations.iter::<Dict<'a>>() {
                self.tick()?;
                let drawn = match annotation.get::<Object<'a>>(b"AP") {
                    Some(Object::Dict(appearances)) => self.appearances(&appearances)?,
                    _ => 0,
                };
                cost = self.add(cost, drawn.saturating_add(1))?;
            }
        }
        self.peaks.page_cost = self.peaks.page_cost.max(cost);
        Ok(cost)
    }

    /// Check that `stream` decodes to at most `at_most` bytes before anything decodes it.
    fn stream(&mut self, stream: &Stream<'_>, at_most: u64) -> Result<()> {
        if let Some(counted) = self.limits.check_stream_within(stream, at_most)? {
            self.peaks.stream_bytes = self.peaks.stream_bytes.max(counted);
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        self.ticks += 1;
        if self.ticks >= TICKS_PER_CLOCK {
            self.ticks = 0;
            self.limits.check_time()?;
        }
        Ok(())
    }

    fn add(&self, cost: u64, more: u64) -> Result<u64> {
        let total = cost.saturating_add(more);
        if total > self.limits.max_page_cost {
            return Err(exceeded("max_page_cost"));
        }
        Ok(total)
    }

    /// The normal appearance: a stream, or one stream per state, of which one is drawn.
    fn appearances(&mut self, appearances: &Dict<'a>) -> Result<u64> {
        match appearances.get::<Object<'a>>(b"N") {
            Some(Object::Stream(stream)) => self.form(&stream, None),
            Some(Object::Dict(states)) => {
                let mut most = 0;
                for key in states.keys() {
                    if let Some(stream) = states.get::<Stream<'a>>(key.as_ref()) {
                        most = most.max(self.form(&stream, None)?);
                    }
                }
                Ok(most)
            }
            _ => Ok(0),
        }
    }

    /// What drawing `data` with `resources` costs, within the limits.
    pub(crate) fn content_of(&mut self, data: &[u8], resources: &Resources<'a>) -> Result<u64> {
        self.content(data, resources)
    }

    fn content(&mut self, data: &[u8], resources: &Resources<'a>) -> Result<u64> {
        let mut cost = 0;
        let mut operators: u64 = 0;
        // What a glyph of the current font costs past its own (a Type 3 glyph is drawn by a
        // content stream). Kept until the next `Tf`, past a `Q` that would restore an earlier
        // font: an over-estimate, never an under-estimate.
        let mut glyph: u64 = 0;
        let mut iter = UntypedIter::new(data);
        while let Some(instruction) = iter.next() {
            operators += 1;
            self.peaks.operators = self.peaks.operators.max(operators);
            if operators > self.limits.max_operators {
                return Err(exceeded("max_operators"));
            }
            self.tick()?;
            let more = match &**instruction.operator {
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    let shown = shown_bytes(&instruction);
                    self.peaks.shown_bytes = self.peaks.shown_bytes.max(shown);
                    if shown > self.limits.max_shown_bytes {
                        return Err(exceeded("max_shown_bytes"));
                    }
                    shown.saturating_mul(glyph.saturating_add(1))
                }
                b"Tf" => {
                    // `font size Tf`: the name comes first.
                    let font = match instruction.operands().next() {
                        Some(Object::Name(name)) => resources.fonts.get::<Dict<'a>>(name),
                        _ => None,
                    };
                    glyph = match font {
                        Some(font) => self.font(&font, resources)?,
                        None => 0,
                    };
                    0
                }
                b"Do" => match last_name(&instruction)
                    .and_then(|n| resources.x_objects.get::<Stream<'a>>(&*n))
                {
                    Some(xobject) => self.xobject(&xobject, resources)?,
                    None => 0,
                },
                b"sh" => match last_name(&instruction)
                    .and_then(|n| resources.shadings.get::<Object<'a>>(&*n))
                {
                    Some(shading) => self.shading(&shading)?,
                    None => 0,
                },
                b"scn" | b"SCN" => match last_name(&instruction)
                    .and_then(|n| resources.patterns.get::<Object<'a>>(&*n))
                {
                    Some(pattern) => self.pattern(&pattern, resources)?,
                    None => 0,
                },
                b"gs" => match last_name(&instruction)
                    .and_then(|n| resources.ext_g_states.get::<Dict<'a>>(&*n))
                {
                    Some(state) => self.graphics_state(&state, resources)?,
                    None => 0,
                },
                b"d" => {
                    if let Some(Object::Array(dashes)) = instruction.operands().next() {
                        let period: f64 = dashes
                            .iter::<f64>()
                            .filter(|v| v.is_finite())
                            .map(f64::abs)
                            .sum();
                        if period > 0.0 {
                            let shortest = self.peaks.dash_period.map_or(period, |p| p.min(period));
                            self.peaks.dash_period = Some(shortest);
                            if period < self.limits.min_dash_period {
                                return Err(exceeded("min_dash_period"));
                            }
                        }
                    }
                    0
                }
                b"BI" => {
                    for operand in instruction.operands() {
                        if let Object::Stream(image) = operand {
                            self.image(image, true)?;
                        }
                    }
                    0
                }
                _ => 0,
            };
            cost = self.add(cost, more.saturating_add(1))?;
        }
        Ok(cost)
    }

    fn xobject(&mut self, xobject: &Stream<'a>, resources: &Resources<'a>) -> Result<u64> {
        match xobject.dict().get::<Name<'a>>(b"Subtype").as_deref() {
            Some(b"Image") => {
                self.image(xobject, false)?;
                if let Some(mask) = xobject.dict().get::<Stream<'a>>(b"SMask") {
                    self.image(&mask, false)?;
                }
                Ok(1)
            }
            Some(b"Form") => self.form(xobject, Some(resources)),
            _ => Ok(0),
        }
    }

    /// A form, a tiling pattern or an appearance: content drawn with its own resources, or with
    /// those of what draws it when it has none.
    fn form(&mut self, stream: &Stream<'a>, inherited: Option<&Resources<'a>>) -> Result<u64> {
        let id = stream.dict().obj_id();
        if let Some(cost) = id.and_then(|id| self.costs.get(&id)) {
            return Ok(*cost);
        }
        if let Some(id) = id
            && !self.open.insert(id)
        {
            return Err(exceeded("forms drawing themselves"));
        }
        let own = stream
            .dict()
            .get::<Dict<'a>>(b"Resources")
            .map(Resources::new);
        let empty;
        let resources = match (&own, inherited) {
            (Some(own), _) => own,
            (None, Some(inherited)) => inherited,
            (None, None) => {
                empty = Resources::new(Dict::default());
                &empty
            }
        };
        // What does not decode, the interpreter does not draw either.
        let cost = match self.stream(stream, self.limits.max_stream_bytes) {
            Err(error) => Err(error),
            Ok(()) => match stream.decoded() {
                Ok(data) => {
                    let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
                    self.peaks.stream_bytes = self.peaks.stream_bytes.max(bytes);
                    self.content(&data, resources)
                }
                Err(_) => Ok(0),
            },
        };
        if let Some(id) = id {
            self.open.remove(&id);
        }
        let cost = cost?;
        if let Some(id) = id {
            self.costs.insert(id, cost);
        }
        Ok(cost)
    }

    /// An image: its pixels, then what its data decodes to, up to eight bytes a pixel.
    fn image(&mut self, image: &Stream<'_>, inline: bool) -> Result<()> {
        let dict = image.dict();
        let side = |long: &[u8], short: &[u8]| {
            dict.get::<f64>(long)
                .or_else(|| inline.then(|| dict.get::<f64>(short)).flatten())
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(0.0)
        };
        let pixels = side(b"Width", b"W") * side(b"Height", b"H");
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = pixels.min(u64::MAX as f64) as u64;
        self.peaks.image_pixels = self.peaks.image_pixels.max(whole);
        #[allow(clippy::cast_precision_loss)]
        if pixels > self.limits.max_image_pixels as f64 {
            return Err(exceeded("max_image_pixels"));
        }
        self.stream(
            image,
            self.limits.max_stream_bytes.max(whole.saturating_mul(8)),
        )
    }

    fn shading(&mut self, shading: &Object<'a>) -> Result<u64> {
        let Object::Stream(mesh) = shading else {
            return Ok(1);
        };
        self.stream(mesh, self.limits.max_stream_bytes)?;
        if let Ok(data) = mesh.decoded() {
            let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
            self.peaks.mesh_bytes = self.peaks.mesh_bytes.max(bytes);
            if bytes > self.limits.max_mesh_bytes {
                return Err(exceeded("max_mesh_bytes"));
            }
        }
        Ok(1)
    }

    fn pattern(&mut self, pattern: &Object<'a>, resources: &Resources<'a>) -> Result<u64> {
        match pattern {
            Object::Stream(tiling) => self.form(tiling, Some(resources)),
            Object::Dict(shading_pattern) => match shading_pattern.get::<Object<'a>>(b"Shading") {
                Some(shading) => self.shading(&shading),
                None => Ok(0),
            },
            _ => Ok(0),
        }
    }

    fn graphics_state(&mut self, state: &Dict<'a>, resources: &Resources<'a>) -> Result<u64> {
        match state
            .get::<Dict<'a>>(b"SMask")
            .and_then(|mask| mask.get::<Stream<'a>>(b"G"))
        {
            Some(group) => self.form(&group, Some(resources)),
            None => Ok(0),
        }
    }

    /// What drawing one glyph of `font` costs past the glyph itself, after checking what the
    /// interpreter would build from it.
    fn font(&mut self, font: &Dict<'a>, resources: &Resources<'a>) -> Result<u64> {
        let id = font.obj_id();
        if let Some(cost) = id.and_then(|id| self.fonts.get(&id)) {
            return Ok(*cost);
        }
        let subtype = font.get::<Name<'a>>(b"Subtype");
        let bound = self.limits.max_stream_bytes;
        for key in [&b"ToUnicode"[..], b"Encoding"] {
            if let Some(cmap) = font.get::<Stream<'a>>(key) {
                self.stream(&cmap, bound)?;
            }
        }
        let descendant = font
            .get::<Array<'a>>(b"DescendantFonts")
            .and_then(|fonts| fonts.iter::<Dict<'a>>().next());
        let described = descendant.as_ref().unwrap_or(font);
        if let Some(descriptor) = described.get::<Dict<'a>>(b"FontDescriptor") {
            for key in [&b"FontFile"[..], b"FontFile2", b"FontFile3"] {
                if let Some(program) = descriptor.get::<Stream<'a>>(key) {
                    self.stream(&program, bound)?;
                }
            }
        }
        let cost = match subtype.as_deref() {
            Some(b"Type0") => {
                if let Some(descendant) = &descendant {
                    for (key, values) in [(&b"W"[..], 1), (b"W2", 3)] {
                        if let Some(widths) = descendant.get::<Array<'a>>(key) {
                            self.widths(&widths, values)?;
                        }
                    }
                }
                0
            }
            Some(b"Type3") => {
                let own = font.get::<Dict<'a>>(b"Resources").map(Resources::new);
                let mut most = 0;
                if let Some(procedures) = font.get::<Dict<'a>>(b"CharProcs") {
                    for key in procedures.keys() {
                        self.tick()?;
                        if let Some(procedure) = procedures.get::<Stream<'a>>(key.as_ref()) {
                            most = most.max(
                                self.form(&procedure, Some(own.as_ref().unwrap_or(resources)))?,
                            );
                        }
                    }
                }
                most
            }
            _ => 0,
        };
        if let Some(id) = id {
            self.fonts.insert(id, cost);
        }
        Ok(cost)
    }

    /// A CID font's `/W` or `/W2`: `c [...]` entries, or `first last` ranges followed by
    /// `values` numbers (one for `/W`, three for `/W2`).
    fn widths(&mut self, widths: &Array<'a>, values: usize) -> Result<()> {
        let entries: Vec<Object<'a>> = widths.iter::<Object<'a>>().collect();
        let mut at = 0;
        while at < entries.len() {
            self.tick()?;
            match (entries.get(at), entries.get(at + 1)) {
                (Some(Object::Number(_)), Some(Object::Array(_))) => at += 2,
                (Some(Object::Number(first)), Some(Object::Number(last))) => {
                    let span = last.as_f64() - first.as_f64();
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let whole = span.clamp(0.0, u64::MAX as f64) as u64;
                    self.peaks.code_range = self.peaks.code_range.max(whole);
                    #[allow(clippy::cast_precision_loss)]
                    if !span.is_finite() || span >= self.limits.max_code_range as f64 {
                        return Err(exceeded("max_code_range"));
                    }
                    at += 2 + values;
                }
                _ => at += 1,
            }
        }
        Ok(())
    }
}

/// The last operand of `instruction`, when it is a name: the resource most operators use.
fn last_name<'a>(instruction: &Instruction<'_, 'a>) -> Option<Name<'a>> {
    match instruction.operands().last()? {
        Object::Name(name) => Some(name.clone()),
        _ => None,
    }
}

/// The bytes the strings of a text-showing operator hold.
fn shown_bytes(instruction: &Instruction<'_, '_>) -> u64 {
    let mut shown: u64 = 0;
    for operand in instruction.operands() {
        match operand {
            Object::String(text) => {
                shown =
                    shown.saturating_add(u64::try_from(text.as_bytes().len()).unwrap_or(u64::MAX));
            }
            Object::Array(items) => {
                for item in items.iter::<Object<'_>>() {
                    if let Object::String(text) = item {
                        shown = shown.saturating_add(
                            u64::try_from(text.as_bytes().len()).unwrap_or(u64::MAX),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    shown
}

/// Whether the `LZWDecode` filter at `index` of `stream`'s filters widens its codes early, as
/// its `/EarlyChange` says (1, the default).
fn early_change(stream: &Stream<'_>, index: usize) -> bool {
    let dict = stream.dict();
    let parameters = match dict
        .get::<Object<'_>>(b"DecodeParms")
        .or_else(|| dict.get::<Object<'_>>(b"DP"))
    {
        Some(Object::Dict(parameters)) if index == 0 => Some(parameters),
        Some(Object::Array(all)) => all.iter::<Object<'_>>().nth(index).and_then(|p| match p {
            Object::Dict(parameters) => Some(parameters),
            _ => None,
        }),
        _ => None,
    };
    parameters
        .and_then(|p| p.get::<i64>(b"EarlyChange"))
        .is_none_or(|value| value != 0)
}

/// Check, in the bytes of a file, the object streams and cross-reference streams hayro decodes
/// whole as soon as it opens the document, before anything else can look at them. Each is decoded
/// as a stream, without keeping its bytes, from its `stream` keyword: its filters end by
/// themselves (deflate's last block, the end markers), whatever its `/Length` says or the bytes
/// after it. Together they may decode to [`Limits::max_stream_bytes`], at every stage.
///
/// Streams with an indirect `/Filter` or with filters not decoded here, and encrypted ones
/// (object streams of an encrypted document), are not checked here.
pub(crate) fn check_structure_streams(data: &[u8], limits: &Limits) -> Result<()> {
    let mut total: u64 = 0;
    for kind in [&b"/ObjStm"[..], b"/XRef"] {
        let mut from = 0;
        while let Some(found) = find(data, kind, from) {
            from = found + kind.len();
            // `/XRefStm` and other longer names are not this one.
            if data.get(from).is_some_and(|b| b.is_ascii_alphanumeric()) {
                continue;
            }
            let Some(dict) = enclosing_dictionary(data, found) else {
                continue;
            };
            let Some(steps) = filter_steps(dict.text) else {
                continue;
            };
            let Some(start) = data.get(dict.stream_data..) else {
                continue;
            };
            limits.check_time()?;
            let left = limits.max_stream_bytes.saturating_sub(total);
            let Some(counted) = decoded_size(start, &steps, left) else {
                return Err(exceeded("max_stream_bytes"));
            };
            total = total.saturating_add(counted);
        }
    }
    Ok(())
}

/// The dictionary text of the stream object around `at` (from its `obj` keyword to its `stream`
/// keyword), and where its data starts.
struct StreamDictionary<'d> {
    text: &'d [u8],
    stream_data: usize,
}

/// How far around a name its stream dictionary is looked for.
const DICTIONARY_REACH: usize = 4096;

fn enclosing_dictionary(data: &[u8], at: usize) -> Option<StreamDictionary<'_>> {
    let back = at.saturating_sub(DICTIONARY_REACH);
    let header = rfind(data.get(back..at)?, b"obj").map(|i| back + i + 3)?;
    let ahead = data.get(at..(at + DICTIONARY_REACH).min(data.len()))?;
    let keyword = at + find(ahead, b"stream", 0)?;
    let text = data.get(header..keyword)?;
    if find(text, b"endobj", 0).is_some() {
        return None;
    }
    // The data starts after the end of line that follows the keyword.
    let mut start = keyword + b"stream".len();
    if data.get(start) == Some(&b'\r') {
        start += 1;
    }
    if data.get(start) == Some(&b'\n') {
        start += 1;
    }
    Some(StreamDictionary {
        text,
        stream_data: start,
    })
}

/// The filters `/Filter` names in `dictionary`, directly, when they are all ones
/// [`decoded_size`] decodes (by their full or abbreviated names).
fn filter_steps(dictionary: &[u8]) -> Option<Vec<Step>> {
    let at = find(dictionary, b"/Filter", 0)? + b"/Filter".len();
    let rest = dictionary.get(at..)?;
    let rest = rest.get(rest.iter().position(|b| !b.is_ascii_whitespace())?..)?;
    let names: &[u8] = if rest.first() == Some(&b'[') {
        rest.get(1..rest.iter().position(|b| *b == b']')?)?
    } else {
        let end = rest
            .iter()
            .skip(1)
            .position(|b| b.is_ascii_whitespace() || b"/<>[]()".contains(b))
            .map_or(rest.len(), |p| p + 1);
        rest.get(..end)?
    };
    let mut steps = Vec::new();
    for name in names.split(|b| b.is_ascii_whitespace() || *b == b'/') {
        steps.push(match name {
            b"" => continue,
            b"FlateDecode" | b"Fl" => Step::Flate,
            b"LZWDecode" | b"LZW" => Step::Lzw { early_change: true },
            b"RunLengthDecode" | b"RL" => Step::RunLength,
            b"ASCII85Decode" | b"A85" => Step::Ascii85,
            b"ASCIIHexDecode" | b"AHx" => Step::AsciiHex,
            _ => return None,
        });
    }
    (!steps.is_empty()).then_some(steps)
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut at = from;
    while at + needle.len() <= haystack.len() {
        let next = haystack.get(at..)?.iter().position(|b| *b == first)?;
        at += next;
        if haystack.get(at..at + needle.len())? == needle {
            return Some(at);
        }
        at += 1;
    }
    None
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}
