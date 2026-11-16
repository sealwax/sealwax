//! What the content of a document uses, walked as a PDF/A validator walks it: the pages, the
//! form XObjects they draw, annotation appearances, tiling patterns, soft masks and the glyph
//! procedures of Type 3 fonts.
//!
//! The walk reads the input as hayro-syntax parses it. What it finds is located
//! ([`Location`]) so that the conversion can then change it in the document's overlay.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Deref;

use hayro_syntax::Pdf;
use hayro_syntax::content::TypedIter;
use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, ObjRef, Object, ObjectIdentifier, Stream};
use sealwax_core::{Document, ObjId};

use crate::edit::Location;

/// How deep forms, patterns, soft masks and glyph procedures may nest.
const MAX_DEPTH: usize = 24;
/// How many colour space definitions may lead to one another.
const MAX_SPACE_DEPTH: usize = 8;
/// How many graphics states `q` may save before the rest are ignored.
const MAX_SAVED_STATES: usize = 1024;
/// How far up the page tree inherited resources are looked for.
const MAX_TREE_DEPTH: usize = 32;
/// How many bytes of the codes shown are kept, in the order shown, to tell the document's
/// language: enough for some thousand words.
const MAX_SAMPLE_BYTES: usize = 32 << 10;
/// A `TJ` adjustment (thousandths of the font size, negative to the right) at least this wide
/// separates words.
const WORD_GAP: f64 = -150.0;

/// Device colour spaces (ISO 32000-1, 8.6.4): PDF/A accepts them only through an output intent
/// or a default colour space (ISO 19005-3, 6.2.4.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Devices {
    pub(crate) gray: bool,
    pub(crate) rgb: bool,
    pub(crate) cmyk: bool,
}

impl Devices {
    fn merge(&mut self, other: Self) {
        self.gray |= other.gray;
        self.rgb |= other.rgb;
        self.cmyk |= other.cmyk;
    }

    const GRAY: Self = Self {
        gray: true,
        rgb: false,
        cmyk: false,
    };
    const RGB: Self = Self {
        gray: false,
        rgb: true,
        cmyk: false,
    };
    const CMYK: Self = Self {
        gray: false,
        rgb: false,
        cmyk: true,
    };
}

/// How a font is used by the content.
#[derive(Debug, Clone, Default)]
pub(crate) struct FontUse {
    /// Shown in a rendering mode other than 3 (invisible): PDF/A then requires its program
    /// (ISO 19005-3, 6.2.11.4.1).
    pub(crate) rendered: bool,
    /// Whether it is a composite (Type 0) font.
    pub(crate) composite: bool,
    /// The character codes shown, for a simple font: one byte each.
    pub(crate) codes: BTreeSet<u8>,
    /// The CIDs shown, for a composite font encoded `Identity-H` or `Identity-V`: two bytes
    /// each.
    pub(crate) cids: BTreeSet<u16>,
}

/// What separates a string shown from the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Break {
    /// Nothing: the same `TJ`, with an adjustment narrower than a space.
    None,
    /// The same `TJ`, with an adjustment as wide as a space.
    Gap,
    /// Another operator: a word ends there only when the producer does not draw its spaces.
    Operator,
}

/// Codes shown by one string of the content, kept to tell the document's language.
#[derive(Debug)]
pub(crate) struct Shown {
    pub(crate) font: Location,
    pub(crate) codes: Vec<u8>,
    pub(crate) after: Break,
}

/// What the walk found.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Device colour spaces used by the content drawn with each resource dictionary: a default
    /// colour space goes into that dictionary.
    pub(crate) resources: BTreeMap<Location, Devices>,
    /// Fonts shown, by the location of their dictionary.
    pub(crate) fonts: BTreeMap<Location, FontUse>,
    /// Image XObjects (and soft masks) that ask for interpolation.
    pub(crate) interpolated_images: BTreeSet<ObjId>,
    /// Inline images that ask for interpolation: changing them means rewriting the content.
    pub(crate) interpolated_inline_images: usize,
    /// Annotation dictionaries.
    pub(crate) annotations: Vec<Location>,
    /// Pages whose resources are inherited from an ancestor as a direct dictionary: the
    /// conversion copies them onto the page before changing them.
    pub(crate) inherited_resources: Vec<ObjId>,
    /// Content streams that could not be decoded.
    pub(crate) undecodable: usize,
    /// Forms, patterns or glyph procedures nested deeper than sealwax follows.
    pub(crate) too_deep: usize,
    /// The first strings shown, up to [`MAX_SAMPLE_BYTES`].
    pub(crate) sample: Vec<Shown>,
    sample_bytes: usize,
}

/// Walk every page of `doc`.
pub(crate) fn scan(doc: &Document) -> Scan {
    let pdf = doc.hayro();
    let mut scanner = Scanner {
        pdf,
        out: Scan::default(),
        handed_down: HashMap::new(),
        forms: HashSet::new(),
        type3: HashSet::new(),
        patterns: HashSet::new(),
    };
    for (page, id) in pdf.pages().iter().zip(doc.page_ids()) {
        let Some(id) = id else {
            continue;
        };
        let raw = page.raw();
        let page_location = Location::of(id);
        let (resources, location) = scanner.page_resources(id, raw);
        if let Some(group) = raw.get::<Dict<'_>>(b"Group")
            && let Some(space) = group.get::<Object<'_>>(b"CS")
        {
            let devices = scanner.space(space, resources.as_ref(), 0);
            scanner
                .out
                .resources
                .entry(location.clone())
                .or_default()
                .merge(devices);
        }
        match page.page_stream() {
            Some(data) => {
                scanner.content(data, resources.as_ref(), &location, State::default(), 0);
            }
            None if raw.contains_key(b"Contents") => scanner.out.undecodable += 1,
            None => {}
        }
        scanner.annotations(raw, &page_location);
    }
    scanner.out
}

fn obj_id(reference: ObjRef) -> Option<ObjId> {
    Some(ObjId {
        number: reference.obj_number,
        generation: u16::try_from(reference.gen_number).ok()?,
    })
}

/// The text state the walk follows.
#[derive(Debug, Clone, Default)]
struct State {
    font: Option<(Location, Kind)>,
    mode: i64,
}

/// How a font's codes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// One byte per code.
    Simple,
    /// Composite, two bytes per code, each the CID.
    Identity,
    /// Composite, through another CMap.
    Composite,
}

/// The resources a node of the page tree hands down to its pages.
#[derive(Clone)]
enum Handed<'a> {
    Nothing,
    /// Those of a node, by reference: an object of their own when the reference has a number.
    Shared(Option<Dict<'a>>, Option<ObjId>),
    /// A direct dictionary of a node.
    Direct(Dict<'a>),
}

struct Scanner<'a> {
    pdf: &'a Pdf,
    out: Scan,
    /// What each node of the page tree hands down, read once: a node with many kids is a large
    /// dictionary, read again for each of them otherwise.
    handed_down: HashMap<ObjectIdentifier, Handed<'a>>,
    /// Forms walked, and whether the walk was invisible text only.
    forms: HashSet<(ObjId, bool)>,
    type3: HashSet<Location>,
    patterns: HashSet<ObjId>,
}

impl<'a> Scanner<'a> {
    /// The dictionary under `key`, and where it lives.
    fn child(
        &self,
        dict: &Dict<'a>,
        key: &[u8],
        location: &Location,
    ) -> Option<(Dict<'a>, Location)> {
        match dict.get_raw::<Dict<'a>>(key)? {
            MaybeRef::Ref(reference) => {
                let id = obj_id(reference)?;
                Some((
                    self.pdf.xref().get::<Dict<'a>>(reference.into())?,
                    Location::of(id),
                ))
            }
            MaybeRef::NotRef(child) => Some((child, location.key(key))),
        }
    }

    /// The resources a page draws with: its own, or those of the nearest ancestor that has some.
    fn page_resources(&mut self, page: ObjId, raw: &Dict<'a>) -> (Option<Dict<'a>>, Location) {
        let own = Location::of(page).key(b"Resources");
        if let Some((resources, location)) = self.child(raw, b"Resources", &Location::of(page)) {
            return (Some(resources), location);
        }
        // The ancestors not known yet, nearest first.
        let mut unknown = Vec::new();
        let mut handed = Handed::Nothing;
        let mut parent = raw.get_raw::<Dict<'a>>(b"Parent");
        for _ in 0..MAX_TREE_DEPTH {
            // An unknown node only is read: a node with many kids is a large dictionary.
            let node = match parent {
                Some(MaybeRef::Ref(reference)) => {
                    let id = ObjectIdentifier::from(reference);
                    if let Some(known) = self.handed_down.get(&id) {
                        handed = known.clone();
                        break;
                    }
                    match self.pdf.xref().get::<Dict<'a>>(id) {
                        Some(node) => node,
                        None => break,
                    }
                }
                Some(MaybeRef::NotRef(node)) => node,
                None => break,
            };
            match node.get_raw::<Dict<'a>>(b"Resources") {
                Some(MaybeRef::Ref(reference)) => {
                    let resources = self.pdf.xref().get::<Dict<'a>>(reference.into());
                    handed = Handed::Shared(resources, obj_id(reference));
                    unknown.push(node);
                    break;
                }
                Some(MaybeRef::NotRef(resources)) => {
                    handed = Handed::Direct(resources);
                    unknown.push(node);
                    break;
                }
                None => {}
            }
            parent = node.get_raw::<Dict<'a>>(b"Parent");
            unknown.push(node);
        }
        for node in unknown {
            if let Some(id) = node.obj_id() {
                self.handed_down.insert(id, handed.clone());
            }
        }
        match handed {
            Handed::Nothing => (None, own),
            Handed::Shared(resources, Some(id)) => (resources, Location::of(id)),
            Handed::Shared(resources, None) => (resources, own),
            Handed::Direct(resources) => {
                self.out.inherited_resources.push(page);
                (Some(resources), own)
            }
        }
    }

    fn annotations(&mut self, page: &Dict<'a>, page_location: &Location) {
        let (annotations, location) = match page.get_raw::<Array<'a>>(b"Annots") {
            Some(MaybeRef::Ref(reference)) => {
                let Some(id) = obj_id(reference) else {
                    return;
                };
                let Some(array) = self.pdf.xref().get::<Array<'a>>(reference.into()) else {
                    return;
                };
                (array, Location::of(id))
            }
            Some(MaybeRef::NotRef(array)) => (array, page_location.key(b"Annots")),
            None => return,
        };
        for (index, item) in annotations.raw_iter().enumerate() {
            let (annotation, location) = match item {
                MaybeRef::Ref(reference) => {
                    let Some(id) = obj_id(reference) else {
                        continue;
                    };
                    let Some(dict) = self.pdf.xref().get::<Dict<'a>>(reference.into()) else {
                        continue;
                    };
                    (dict, Location::of(id))
                }
                MaybeRef::NotRef(Object::Dict(dict)) => (dict, location.index(index)),
                MaybeRef::NotRef(_) => continue,
            };
            self.out.annotations.push(location);
            // Only the normal appearance is kept in PDF/A (6.3.3): it is the one walked.
            let Some(appearance) = annotation
                .get::<Dict<'a>>(b"AP")
                .and_then(|ap| ap.get::<Object<'a>>(b"N"))
            else {
                continue;
            };
            match appearance {
                Object::Stream(stream) => self.form(&stream, None, None, &State::default(), 1),
                Object::Dict(states) => {
                    for key in states.keys() {
                        if let Some(stream) = states.get::<Stream<'a>>(key.deref()) {
                            self.form(&stream, None, None, &State::default(), 1);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Walk a content stream drawn with `resources`, found at `location`.
    #[allow(clippy::too_many_lines)]
    fn content(
        &mut self,
        data: &[u8],
        resources: Option<&Dict<'a>>,
        location: &Location,
        mut state: State,
        depth: usize,
    ) {
        let mut devices = Devices::default();
        let mut saved: Vec<State> = Vec::new();
        // Inline images are taken out first (hayro issue #1610, see `inline`).
        let (data, images) = crate::inline::split(data);
        for image in &images {
            if image.interpolate {
                self.out.interpolated_inline_images += 1;
            }
            if !image.mask
                && let Some(space) = &image.space
            {
                devices.merge(match space.as_slice() {
                    b"G" | b"DeviceGray" => Devices::GRAY,
                    b"RGB" | b"DeviceRGB" => Devices::RGB,
                    b"CMYK" | b"DeviceCMYK" => Devices::CMYK,
                    other => self.named_space(&Name::new_unescaped(other), resources, 0),
                });
            }
        }
        let mut ops = TypedIter::new(&data);
        while let Some(op) = ops.next() {
            match op {
                TypedInstruction::SaveState(_) => {
                    if saved.len() < MAX_SAVED_STATES {
                        saved.push(state.clone());
                    }
                }
                TypedInstruction::RestoreState(_) => {
                    if let Some(previous) = saved.pop() {
                        state = previous;
                    }
                }
                TypedInstruction::StrokeColorDeviceGray(_)
                | TypedInstruction::NonStrokeColorDeviceGray(_) => devices.merge(Devices::GRAY),
                TypedInstruction::StrokeColorDeviceRgb(_)
                | TypedInstruction::NonStrokeColorDeviceRgb(_) => devices.merge(Devices::RGB),
                TypedInstruction::StrokeColorCmyk(_) | TypedInstruction::NonStrokeColorCmyk(_) => {
                    devices.merge(Devices::CMYK);
                }
                TypedInstruction::ColorSpaceStroke(op) => {
                    devices.merge(self.named_space(op.0, resources, 0));
                }
                TypedInstruction::ColorSpaceNonStroke(op) => {
                    devices.merge(self.named_space(op.0, resources, 0));
                }
                TypedInstruction::StrokeColorNamed(op) => {
                    if let Some(name) = op.1 {
                        self.pattern(name, resources, location, &state, &mut devices, depth);
                    }
                }
                TypedInstruction::NonStrokeColorNamed(op) => {
                    if let Some(name) = op.1 {
                        self.pattern(name, resources, location, &state, &mut devices, depth);
                    }
                }
                TypedInstruction::Shading(op) => {
                    if let Some(shading) = resources
                        .and_then(|r| r.get::<Dict<'a>>(b"Shading"))
                        .and_then(|s| s.get::<Object<'a>>(op.0.deref()))
                    {
                        devices.merge(self.shading(shading, resources));
                    }
                }
                TypedInstruction::XObject(op) => {
                    self.xobject(op.0, resources, location, &state, &mut devices, depth);
                }
                TypedInstruction::InlineImage(op) => {
                    let dict = op.0.dict();
                    let interpolate = dict
                        .get::<bool>(b"I")
                        .or_else(|| dict.get::<bool>(b"Interpolate"));
                    if interpolate == Some(true) {
                        self.out.interpolated_inline_images += 1;
                    }
                    let mask = dict
                        .get::<bool>(b"IM")
                        .or_else(|| dict.get::<bool>(b"ImageMask"))
                        .unwrap_or(false);
                    if !mask
                        && let Some(space) = dict
                            .get::<Object<'_>>(b"CS")
                            .or_else(|| dict.get::<Object<'_>>(b"ColorSpace"))
                    {
                        devices.merge(self.inline_space(&space, resources));
                    }
                }
                TypedInstruction::SetGraphicsState(op) => {
                    if let Some(group) = resources
                        .and_then(|r| r.get::<Dict<'a>>(b"ExtGState"))
                        .and_then(|g| g.get::<Dict<'a>>(op.0.deref()))
                        .and_then(|g| g.get::<Dict<'a>>(b"SMask"))
                        .and_then(|m| m.get::<Stream<'a>>(b"G"))
                    {
                        self.form(
                            &group,
                            resources,
                            Some(location),
                            &State::default(),
                            depth + 1,
                        );
                    }
                }
                TypedInstruction::TextFont(op) => {
                    state.font = self.font(op.0, resources, location, depth);
                }
                TypedInstruction::TextRenderingMode(op) => state.mode = op.0.as_i64(),
                TypedInstruction::ShowText(op) => {
                    self.show(&state, op.0.as_bytes(), Break::Operator);
                }
                TypedInstruction::NextLineAndShowText(op) => {
                    self.show(&state, op.0.as_bytes(), Break::Operator);
                }
                TypedInstruction::ShowTextWithParameters(op) => {
                    self.show(&state, op.2.as_bytes(), Break::Operator);
                }
                TypedInstruction::ShowTexts(op) => {
                    let mut after = Break::Operator;
                    for item in op.0.iter::<Object<'_>>() {
                        match item {
                            Object::String(text) => {
                                self.show(&state, text.as_bytes(), after);
                                after = Break::None;
                            }
                            Object::Number(gap)
                                if gap.as_f64() <= WORD_GAP && after == Break::None =>
                            {
                                after = Break::Gap;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        self.out
            .resources
            .entry(location.clone())
            .or_default()
            .merge(devices);
    }

    fn show(&mut self, state: &State, bytes: &[u8], after: Break) {
        let Some((location, kind)) = &state.font else {
            return;
        };
        if self.out.sample_bytes < MAX_SAMPLE_BYTES && !bytes.is_empty() {
            self.out.sample_bytes += bytes.len();
            self.out.sample.push(Shown {
                font: location.clone(),
                codes: bytes.to_vec(),
                after,
            });
        }
        let used = self.out.fonts.entry(location.clone()).or_default();
        used.rendered |= state.mode != 3;
        used.composite = *kind != Kind::Simple;
        match kind {
            Kind::Simple => used.codes.extend(bytes.iter().copied()),
            Kind::Identity => used.cids.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes(*pair)),
            ),
            Kind::Composite => {}
        }
    }

    /// Select font `name`: where its dictionary lives, and whether it is composite. A Type 3
    /// font's glyph procedures are walked the first time.
    fn font(
        &mut self,
        name: &Name<'_>,
        resources: Option<&Dict<'a>>,
        location: &Location,
        depth: usize,
    ) -> Option<(Location, Kind)> {
        let resources = resources?;
        let (fonts, fonts_location) = self.child(resources, b"Font", location)?;
        let (font, font_location) = self.child(&fonts, name.deref(), &fonts_location)?;
        let subtype = font.get::<Name<'_>>(b"Subtype");
        let subtype = subtype.as_deref();
        if subtype == Some(&b"Type3"[..]) && self.type3.insert(font_location.clone()) {
            let (glyph_resources, glyph_location) =
                match self.child(&font, b"Resources", &font_location) {
                    Some((dict, at)) => (Some(dict), at),
                    None => (Some(resources.clone()), location.clone()),
                };
            if let Some(procedures) = font.get::<Dict<'a>>(b"CharProcs") {
                for key in procedures.keys() {
                    if let Some(glyph) = procedures.get::<Stream<'a>>(key.deref()) {
                        if depth + 1 > MAX_DEPTH {
                            self.out.too_deep += 1;
                            break;
                        }
                        match glyph.decoded() {
                            Ok(data) => self.content(
                                &data,
                                glyph_resources.as_ref(),
                                &glyph_location,
                                State::default(),
                                depth + 1,
                            ),
                            Err(_) => self.out.undecodable += 1,
                        }
                    }
                }
            }
        }
        let kind = if subtype != Some(&b"Type0"[..]) {
            Kind::Simple
        } else if matches!(
            font.get::<Name<'_>>(b"Encoding").as_deref(),
            Some(b"Identity-H" | b"Identity-V")
        ) {
            Kind::Identity
        } else {
            Kind::Composite
        };
        Some((font_location, kind))
    }

    fn xobject(
        &mut self,
        name: &Name<'_>,
        resources: Option<&Dict<'a>>,
        location: &Location,
        state: &State,
        devices: &mut Devices,
        depth: usize,
    ) {
        let Some(stream) = resources
            .and_then(|r| r.get::<Dict<'a>>(b"XObject"))
            .and_then(|x| x.get::<Stream<'a>>(name.deref()))
        else {
            return;
        };
        let subtype = stream.dict().get::<Name<'_>>(b"Subtype");
        match subtype.as_deref() {
            Some(b"Image") => self.image(&stream, resources, devices),
            Some(b"Form") => self.form(&stream, resources, Some(location), state, depth + 1),
            _ => {}
        }
    }

    fn image(&mut self, stream: &Stream<'a>, resources: Option<&Dict<'a>>, devices: &mut Devices) {
        let dict = stream.dict();
        if dict.get::<bool>(b"Interpolate") == Some(true)
            && let Some(id) = obj_id(stream.obj_id().into())
        {
            self.out.interpolated_images.insert(id);
        }
        if let Some(mask) = dict.get::<Stream<'a>>(b"SMask")
            && mask.dict().get::<bool>(b"Interpolate") == Some(true)
            && let Some(id) = obj_id(mask.obj_id().into())
        {
            self.out.interpolated_images.insert(id);
        }
        if !dict.get::<bool>(b"ImageMask").unwrap_or(false)
            && let Some(space) = dict.get::<Object<'a>>(b"ColorSpace")
        {
            devices.merge(self.space(space, resources, 0));
        }
    }

    /// Walk a form XObject. Without resources of its own, it draws with its caller's.
    fn form(
        &mut self,
        stream: &Stream<'a>,
        caller_resources: Option<&Dict<'a>>,
        caller_location: Option<&Location>,
        state: &State,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            self.out.too_deep += 1;
            return;
        }
        let Some(id) = obj_id(stream.obj_id().into()) else {
            return;
        };
        // A form drawn as invisible text first is walked again when drawn visibly.
        if !self.forms.insert((id, state.mode == 3)) {
            return;
        }
        let form_location = Location::of(id);
        let (resources, location) = match self.child(stream.dict(), b"Resources", &form_location) {
            Some((dict, at)) => (Some(dict), at),
            None => match (caller_resources, caller_location) {
                (Some(dict), Some(at)) => (Some(dict.clone()), at.clone()),
                _ => (None, form_location.key(b"Resources")),
            },
        };
        let mut devices = Devices::default();
        if let Some(space) = stream
            .dict()
            .get::<Dict<'a>>(b"Group")
            .and_then(|g| g.get::<Object<'a>>(b"CS"))
        {
            devices.merge(self.space(space, resources.as_ref(), 0));
        }
        self.out
            .resources
            .entry(location.clone())
            .or_default()
            .merge(devices);
        match stream.decoded() {
            Ok(data) => self.content(&data, resources.as_ref(), &location, state.clone(), depth),
            Err(_) => self.out.undecodable += 1,
        }
    }

    fn pattern(
        &mut self,
        name: &Name<'_>,
        resources: Option<&Dict<'a>>,
        location: &Location,
        state: &State,
        devices: &mut Devices,
        depth: usize,
    ) {
        let Some(pattern) = resources
            .and_then(|r| r.get::<Dict<'a>>(b"Pattern"))
            .and_then(|p| p.get::<Object<'a>>(name.deref()))
        else {
            return;
        };
        match pattern {
            // A tiling pattern: content of its own.
            Object::Stream(stream) => {
                let Some(id) = obj_id(stream.obj_id().into()) else {
                    return;
                };
                if self.patterns.insert(id) {
                    self.form(&stream, resources, Some(location), state, depth + 1);
                }
            }
            // A shading pattern.
            Object::Dict(dict) => {
                if let Some(shading) = dict.get::<Object<'a>>(b"Shading") {
                    devices.merge(self.shading(shading, resources));
                }
            }
            _ => {}
        }
    }

    fn shading(&self, shading: Object<'a>, resources: Option<&Dict<'a>>) -> Devices {
        let dict = match shading {
            Object::Dict(dict) => dict,
            Object::Stream(stream) => stream.dict().clone(),
            _ => return Devices::default(),
        };
        dict.get::<Object<'a>>(b"ColorSpace")
            .map(|space| self.space(space, resources, 0))
            .unwrap_or_default()
    }

    fn named_space(&self, name: &Name<'_>, resources: Option<&Dict<'a>>, depth: usize) -> Devices {
        match name.deref() {
            b"DeviceGray" => Devices::GRAY,
            b"DeviceRGB" => Devices::RGB,
            b"DeviceCMYK" => Devices::CMYK,
            b"Pattern" => Devices::default(),
            other => resources
                .and_then(|r| r.get::<Dict<'a>>(b"ColorSpace"))
                .and_then(|spaces| spaces.get::<Object<'a>>(other))
                .map(|space| self.space(space, resources, depth + 1))
                .unwrap_or_default(),
        }
    }

    /// The device spaces a colour space uses: itself, the base of an indexed space, the
    /// alternate of a separation or DeviceN space.
    fn space(&self, space: Object<'a>, resources: Option<&Dict<'a>>, depth: usize) -> Devices {
        if depth > MAX_SPACE_DEPTH {
            return Devices::default();
        }
        match space {
            Object::Name(name) => self.named_space(&name, resources, depth),
            Object::Array(array) => {
                let mut items = array.iter::<Object<'a>>();
                let Some(Object::Name(family)) = items.next() else {
                    return Devices::default();
                };
                let nested = match family.deref() {
                    b"Indexed" | b"I" | b"Pattern" => items.next(),
                    b"Separation" | b"DeviceN" => items.nth(1),
                    _ => None,
                };
                nested
                    .map(|inner| self.space(inner, resources, depth + 1))
                    .unwrap_or_default()
            }
            _ => Devices::default(),
        }
    }

    /// An inline image's colour space, where abbreviations are allowed.
    fn inline_space(&self, space: &Object<'_>, resources: Option<&Dict<'a>>) -> Devices {
        match space {
            Object::Name(name) => match name.deref() {
                b"G" | b"DeviceGray" => Devices::GRAY,
                b"RGB" | b"DeviceRGB" => Devices::RGB,
                b"CMYK" | b"DeviceCMYK" => Devices::CMYK,
                other => resources
                    .and_then(|r| r.get::<Dict<'a>>(b"ColorSpace"))
                    .and_then(|spaces| spaces.get::<Object<'a>>(other))
                    .map(|space| self.space(space, resources, 1))
                    .unwrap_or_default(),
            },
            Object::Array(array) => {
                let mut items = array.iter::<Object<'_>>();
                let Some(Object::Name(family)) = items.next() else {
                    return Devices::default();
                };
                if matches!(family.deref(), b"Indexed" | b"I")
                    && let Some(base) = items.next()
                {
                    return self.inline_space(&base, resources);
                }
                Devices::default()
            }
            _ => Devices::default(),
        }
    }
}
