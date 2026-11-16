//! A Unicode value for every glyph shown (ISO 19005-3, 6.2.11.7.2), from level u on: decision
//! D27, part (2).
//!
//! A code shown that the font does not map to Unicode the way veraPDF reads it (its `ToUnicode`
//! CMap when it has one, else the glyph names of its encoding) gets a value from, in this order:
//! 1. the glyph name its encoding gives, when a `ToUnicode` CMap hides it;
//! 2. the tables of the font program: the glyph's name (`post`, CFF charset, Type 1), then the
//!    program's `cmap`;
//! 3. for a composite font, the Unicode value Adobe gives the CID in its character collection
//!    (Adobe-Japan1, -GB1, -CNS1, -Korea1);
//! 4. the drawing: a glyph drawn point for point like a glyph of another font of the document
//!    whose meaning is known means what that one means (producers embed subsets of one font,
//!    some with glyph names and some without);
//! 5. for a simple font, the code itself read as Latin-1 when that is a printable character:
//!    what PDFium extracts, often right (TeX fonts, encodings after ASCII), and what the user
//!    copies from the document today (measured 14/11/2026);
//! 6. a glyph that draws nothing is a space;
//! 7. U+FFFD, the replacement character, counted in the report. A CID of Adobe-Identity says
//!    nothing: PDFium extracts it as if it were a character code.
//!
//! The font then gets a `ToUnicode` CMap with what its own said and a value for every code
//! shown. A font whose codes all map already is left as it is.
//!
//! At level a, a value in the Unicode Private Use Area would need an `ActualText` in the content
//! (6.2.11.7.3). For Symbol, Webdings and Wingdings it becomes the character the glyph stands for
//! ([`dingbats`]): Word gives their glyphs U+F000 plus their code, and the Adobe Glyph List puts
//! some Symbol glyphs there. Any other is no value, and the next source is asked.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use hayro_cmap::{BfString, CMap, CidFamily};
use read_fonts::FontRef;
use read_fonts::model::Font as Face;
use read_fonts::model::charmap::EncodingKind;
use read_fonts::model::glyph::outline::{DrawMemory, UnscaledOutlineSink};
use read_fonts::tables::glyf::PointFlags;
use read_fonts::types::{GlyphId, Tag};
use sealwax_core::{Dict, Document, ObjId, Object, Result};

use super::{base_table, differences, dingbats, encodings, to_unicode};
use crate::color::flate_stream;
use crate::edit::{self, Location};
use crate::scan::{FontUse, Scan};
use crate::{Level, Report};

/// Programs with more glyphs than this lend only the glyphs the document shows to the
/// recognition of drawings: the cost stays bounded.
const MAX_INDEXED_GLYPHS: u32 = 4096;

fn bf_text(value: BfString) -> String {
    match value {
        BfString::Char(c) => c.to_string(),
        BfString::String(text) => text,
    }
}

/// A font program, read.
fn face(data: Vec<u8>) -> Option<Face> {
    // An OpenType program holding only what its CFF outlines need may lack `maxp`, without
    // which the glyphs cannot be counted: the CFF table is read alone then.
    let cff = FontRef::new(&data)
        .ok()
        .and_then(|font| font.table_data(Tag::new(b"CFF ")))
        .map(|table| table.as_bytes().to_vec());
    let face = Face::new(data, 0)?;
    if face.num_glyphs() == 0
        && let Some(cff) = cff
    {
        return Face::new(cff, 0);
    }
    Some(face)
}

/// The stream holding the program of the font a descriptor describes, and the program read
/// when it can be. A program whose data is damaged is not read: FreeType refuses what is left
/// of it, and PDFium then draws the font as if it had none (measured 14/11/2026).
fn program(doc: &Document, descriptor: Option<&Dict>) -> Option<(ObjId, Option<Face>)> {
    let descriptor = descriptor?;
    let file = [&b"FontFile"[..], b"FontFile2", b"FontFile3"]
        .iter()
        .find_map(|key| descriptor.get(key).and_then(Object::as_ref_id))?;
    Some((file, edit::stream_data(doc, file, true).and_then(face)))
}

/// What a drawing looks like: nothing, or a shape known by a digest of its points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Drawing {
    Blank,
    Shape { units_per_em: u16, digest: u64 },
}

/// Collects the points of an outline, and whether any contour encloses an area.
#[derive(Default)]
struct Shape {
    points: Vec<i32>,
    contour: Vec<(i64, i64)>,
    visible: bool,
}

impl UnscaledOutlineSink for Shape {
    fn start_contour(&mut self) {
        self.contour.clear();
        self.points.push(i32::MIN);
    }

    fn point(&mut self, x: i32, y: i32, flags: PointFlags) {
        self.points.extend([x, y, i32::from(flags.to_bits())]);
        self.contour.push((i64::from(x), i64::from(y)));
    }

    fn end_contour(&mut self) {
        // Twice the area of the polygon through the points: zero for a point or a line.
        let n = self.contour.len();
        let area: i64 = (0..n)
            .map(|i| {
                let (x0, y0) = self.contour[i];
                let (x1, y1) = self.contour[(i + 1) % n];
                x0 * y1 - x1 * y0
            })
            .sum();
        self.visible |= area != 0;
    }
}

fn drawing(face: &Face, glyph: GlyphId) -> Option<Drawing> {
    let outline = face.outline_glyphs().get(glyph)?;
    let mut shape = Shape::default();
    outline
        .draw_unscaled(DrawMemory::Scratch, &mut shape)
        .ok()?;
    if !shape.visible {
        return Some(Drawing::Blank);
    }
    let mut hasher = DefaultHasher::new();
    shape.points.hash(&mut hasher);
    Some(Drawing::Shape {
        units_per_em: face.units_per_em(),
        digest: hasher.finish(),
    })
}

/// What the program's own tables say a glyph means: its name, then its `cmap`.
struct Tables {
    by_glyph: HashMap<GlyphId, String>,
}

impl Tables {
    fn new(face: &Face) -> Self {
        let mut by_glyph = HashMap::new();
        let charmap = face.charmap();
        // A symbol `cmap` holds codes, not Unicode values.
        if !charmap.unicode_is_symbol() {
            let mac_roman = charmap.unicode_is_mac_roman();
            for (code, glyph) in charmap.unicodes() {
                let text = if mac_roman {
                    usize::try_from(code)
                        .ok()
                        .and_then(|code| encodings::MAC_ROMAN.get(code))
                        .and_then(|name| to_unicode::unicode_of(name))
                } else {
                    char::from_u32(code).map(String::from)
                };
                if let Some(text) = text.filter(|t| to_unicode::allowed(t)) {
                    by_glyph.entry(glyph).or_insert(text);
                }
            }
        }
        Self { by_glyph }
    }

    fn text(&self, face: &Face, glyph: GlyphId) -> Option<String> {
        if let Some(name) = face.glyph_name(glyph)
            && !name.is_synthesized()
            && let Some(text) = to_unicode::unicode_of(name.as_str())
        {
            return Some(text);
        }
        self.by_glyph.get(&glyph).cloned()
    }
}

/// A font shown, and what its codes mean.
struct Subject {
    location: Location,
    /// Bytes per code: 1 for a simple font, 2 for a composite one.
    width: usize,
    /// The codes shown, and the glyph each draws when the program is read.
    codes: BTreeMap<u32, Option<GlyphId>>,
    /// The font's `ToUnicode` CMap: `Some(None)` when it has one that cannot be read.
    to_unicode: Option<Option<CMap>>,
    /// For a simple font, the glyph name its encoding gives each code.
    names: BTreeMap<u32, String>,
    /// For a composite font, the Unicode values of the CIDs of its character collection.
    collection: Option<Rc<CMap>>,
    /// The program, and the stream holding it.
    file: Option<ObjId>,
    face: Option<Face>,
    tables: Option<Tables>,
    /// For Symbol, Webdings and Wingdings, the characters their glyphs stand for.
    dingbats: Option<&'static [u32; 224]>,
    /// Whether Private Use values are refused (level a).
    no_private_use: bool,
}

/// Whether `text` holds a character of a Private Use Area.
fn private_use(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
    })
}

/// The name of a font, for the tables of [`dingbats`].
fn dingbat_table(font: &Dict) -> Option<&'static [u32; 224]> {
    let name = font.get(b"BaseFont").and_then(Object::as_name)?;
    dingbats::table(&String::from_utf8_lossy(name))
}

impl Subject {
    /// A value found from `source`, as kept: at level a, a Private Use value becomes the
    /// character a symbol font's glyph stands for, or no value.
    fn settle(&self, code: u32, text: String, source: Source) -> Option<(String, Source)> {
        if !self.no_private_use || !private_use(&text) {
            return Some((text, source));
        }
        let table = self.dingbats?;
        let mut chars = text.chars();
        let by_value = match (chars.next(), chars.next()) {
            (Some(private), None) => dingbats::value(table, private),
            _ => None,
        };
        // Adobe's names of some Symbol glyphs (`registerserif`, `radicalex`) are in the Private
        // Use Area: the glyph's place in the Symbol encoding says what it is.
        let value = by_value.or_else(|| {
            if self.width != 1 || !std::ptr::eq(table, &dingbats::SYMBOL) {
                return None;
            }
            let symbol_code = match self.glyph_name(code) {
                Some(name) => encodings::SYMBOL.iter().position(|n| *n == name)?,
                // Without an encoding, a code of Symbol is a code of its built-in encoding.
                None if self.names.is_empty() => usize::try_from(code).ok()?,
                None => return None,
            };
            dingbats::value(
                table,
                char::from_u32(0xF000 + u32::try_from(symbol_code).ok()?)?,
            )
        })?;
        Some((value.to_string(), Source::Dingbat))
    }

    /// The name of the glyph a code of a simple font draws: its encoding's, else its program's.
    fn glyph_name(&self, code: u32) -> Option<String> {
        if let Some(name) = self.names.get(&code) {
            return Some(name.clone());
        }
        let glyph = (*self.codes.get(&code)?)?;
        let name = self.face.as_ref()?.glyph_name(glyph)?;
        (!name.is_synthesized()).then(|| name.as_str().to_owned())
    }

    /// What the font itself says a code means, the way veraPDF reads it: a `ToUnicode` CMap
    /// alone when the font has one, even one that maps nothing (measured 14/11/2026); else the
    /// glyph name of its encoding.
    fn declared(&self, code: u32) -> Option<String> {
        match &self.to_unicode {
            Some(cmap) => cmap
                .as_ref()?
                .lookup_bf_string(code)
                .map(bf_text)
                .filter(|t| to_unicode::allowed(t)),
            None => to_unicode::unicode_of(self.names.get(&code)?),
        }
    }

    /// What the glyph name of the encoding says a code means.
    fn named(&self, code: u32) -> Option<String> {
        to_unicode::unicode_of(self.names.get(&code)?)
    }

    /// What the character collection says a CID means.
    fn by_collection(&self, code: u32) -> Option<String> {
        self.collection
            .as_ref()?
            .lookup_bf_string(code)
            .map(bf_text)
            .filter(|t| to_unicode::allowed(t))
    }

    /// The code itself, read as Latin-1 the way PDFium extracts it, when that is a printable
    /// character: for a simple font only, a CID being no character code.
    fn as_latin1(&self, code: u32) -> Option<String> {
        let printable = matches!(code, 0x20..=0x7E | 0xA0..=0xFF);
        (self.width == 1 && printable)
            .then(|| char::from_u32(code).map(String::from))
            .flatten()
    }

    /// What the program's tables say a code means.
    fn by_tables(&mut self, code: u32) -> Option<String> {
        let glyph = (*self.codes.get(&code)?)?;
        let face = self.face.as_ref()?;
        let tables = self.tables.get_or_insert_with(|| Tables::new(face));
        tables.text(face, glyph)
    }

    fn drawing(&self, code: u32) -> Option<Drawing> {
        drawing(self.face.as_ref()?, (*self.codes.get(&code)?)?)
    }
}

fn simple_names(doc: &Document, font: &Dict) -> Result<BTreeMap<u32, String>> {
    let value = match font.get(b"Encoding") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    let (base, own) = match &value {
        Some(Object::Name(name)) => (base_table(name.as_bytes()), BTreeMap::new()),
        Some(Object::Dict(dict)) => {
            let base = dict
                .get(b"BaseEncoding")
                .and_then(Object::as_name)
                .and_then(base_table);
            let list = match dict.get(b"Differences") {
                Some(list) => edit::resolved(doc, list)?
                    .as_ref()
                    .and_then(Object::as_array)
                    .map(differences)
                    .unwrap_or_default(),
                None => BTreeMap::new(),
            };
            (base, list)
        }
        _ => (None, BTreeMap::new()),
    };
    let mut names = BTreeMap::new();
    for code in 0..=255u8 {
        let name = own.get(&code).cloned().or_else(|| {
            base.map(|table| table[usize::from(code)])
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
        });
        if let Some(name) = name {
            names.insert(u32::from(code), name);
        }
    }
    Ok(names)
}

/// The glyph a simple font's program draws for a code (ISO 32000-1, 9.6.6).
fn simple_glyph(face: &Face, truetype: bool, name: Option<&str>, code: u8) -> Option<GlyphId> {
    let charmap = face.charmap();
    if let Some(name) = name {
        if let Some(glyph) = face.glyph_by_name(name) {
            return Some(glyph);
        }
        let text = to_unicode::unicode_of(name).unwrap_or_default();
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && let Some(glyph) = charmap.map_unicode(c)
        {
            return Some(glyph);
        }
    }
    let code = u32::from(code);
    let by = |kinds: &[EncodingKind], code: u32| {
        charmap
            .encodings()
            .filter(|e| kinds.contains(&e.kind()))
            .find_map(|e| charmap.map_code(e, code))
    };
    if truetype {
        by(&[EncodingKind::MsSymbol], 0xF000 + code)
            .or_else(|| by(&[EncodingKind::MsSymbol], code))
            .or_else(|| by(&[EncodingKind::AppleRoman], code))
            .or_else(|| by(&[EncodingKind::Unicode], code))
    } else {
        // The program's own encoding.
        by(
            &[
                EncodingKind::AdobeStandard,
                EncodingKind::AdobeCustom,
                EncodingKind::AdobeExpert,
            ],
            code,
        )
    }
}

fn read_to_unicode(doc: &Document, font: &Dict) -> Option<Option<CMap>> {
    let value = font.get(b"ToUnicode")?;
    Some(
        value
            .as_ref_id()
            .and_then(|id| edit::stream_data(doc, id, false))
            .and_then(|data| CMap::parse(&data, |_| None)),
    )
}

fn descriptor(doc: &Document, location: &Location, font: &Dict) -> Result<Option<Dict>> {
    if font.contains_key(b"FontDescriptor") {
        edit::read_dict(doc, &location.key(b"FontDescriptor"))
    } else {
        Ok(None)
    }
}

fn simple_subject(
    doc: &Document,
    location: &Location,
    font: &Dict,
    used: &FontUse,
    no_private_use: bool,
) -> Result<Subject> {
    let names = simple_names(doc, font)?;
    let truetype = font.get(b"Subtype").and_then(Object::as_name) == Some(b"TrueType");
    let (file, face) = program(doc, descriptor(doc, location, font)?.as_ref()).unzip();
    let face = face.flatten();
    let codes = used
        .codes
        .iter()
        .map(|code| {
            let glyph = face.as_ref().and_then(|face| {
                let name = names.get(&u32::from(*code)).map(String::as_str);
                simple_glyph(face, truetype, name, *code)
            });
            (u32::from(*code), glyph)
        })
        .collect();
    Ok(Subject {
        location: location.clone(),
        width: 1,
        codes,
        to_unicode: read_to_unicode(doc, font),
        names,
        collection: None,
        file,
        face,
        tables: None,
        dingbats: dingbat_table(font),
        no_private_use,
    })
}

/// The Unicode values of the CIDs of each character collection, read once.
type Collections = HashMap<CidFamily, Option<Rc<CMap>>>;

fn collection(collections: &mut Collections, family: CidFamily) -> Option<Rc<CMap>> {
    collections
        .entry(family)
        .or_insert_with_key(|family| {
            let data = hayro_cmap::load_embedded(family.ucs2_cmap()?)?;
            CMap::parse(data, hayro_cmap::load_embedded).map(Rc::new)
        })
        .clone()
}

/// The character collection the CIDFont at `location` declares.
fn family(doc: &Document, location: &Location) -> Result<Option<CidFamily>> {
    let Some(info) = edit::read_dict(doc, &location.key(b"CIDSystemInfo"))? else {
        return Ok(None);
    };
    let text = |key: &[u8]| match info.get(key) {
        Some(Object::String(value)) => Some(value.clone()),
        _ => None,
    };
    Ok(text(b"Registry")
        .zip(text(b"Ordering"))
        .map(|(registry, ordering)| CidFamily::from_registry_ordering(&registry, &ordering)))
}

/// Where the CIDFont of a composite font lives.
fn descendant(location: &Location, font: &Dict) -> Option<Location> {
    let descendants = match font.get(b"DescendantFonts")? {
        Object::Ref(id) => Location::of(*id),
        _ => location.key(b"DescendantFonts"),
    };
    Some(descendants.index(0))
}

fn composite_subject(
    doc: &Document,
    location: &Location,
    font: &Dict,
    used: &FontUse,
    collections: &mut Collections,
    no_private_use: bool,
) -> Result<Option<Subject>> {
    let Some(cid_location) = descendant(location, font) else {
        return Ok(None);
    };
    let Some(cid_font) = edit::read_dict(doc, &cid_location)? else {
        return Ok(None);
    };
    let (file, face) = program(doc, descriptor(doc, &cid_location, &cid_font)?.as_ref()).unzip();
    let face = face.flatten();
    let map = match cid_font.get(b"CIDToGIDMap") {
        Some(Object::Ref(id)) => edit::stream_data(doc, *id, false),
        _ => None,
    };
    let truetype = cid_font.get(b"Subtype").and_then(Object::as_name) == Some(b"CIDFontType2");
    let glyph_of = |cid: u16| -> Option<GlyphId> {
        let face = face.as_ref()?;
        if truetype {
            return match &map {
                None => Some(GlyphId::new(u32::from(cid))),
                Some(bytes) => {
                    let at = 2 * usize::from(cid);
                    let gid = u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]);
                    Some(GlyphId::new(u32::from(gid)))
                }
            };
        }
        if face.is_cid_keyed() {
            face.glyph_by_cid(u32::from(cid))
        } else {
            Some(GlyphId::new(u32::from(cid)))
        }
    };
    let codes = used
        .cids
        .iter()
        .map(|cid| (u32::from(*cid), glyph_of(*cid)))
        .collect();
    let collection = family(doc, &cid_location)?.and_then(|family| collection(collections, family));
    Ok(Some(Subject {
        location: location.clone(),
        width: 2,
        codes,
        to_unicode: read_to_unicode(doc, font),
        names: BTreeMap::new(),
        collection,
        file,
        face,
        tables: None,
        dingbats: dingbat_table(font),
        no_private_use,
    }))
}

/// Whether a composite font uses a character collection whose CIDs have known Unicode values:
/// one that reads its codes through a predefined CMap needs no `ToUnicode` then (6.2.11.7.2).
fn known_collection(doc: &Document, location: &Location, font: &Dict) -> Result<bool> {
    let Some(cid_location) = descendant(location, font) else {
        return Ok(false);
    };
    Ok(family(doc, &cid_location)?.is_some_and(|family| family.ucs2_cmap().is_some()))
}

/// How the code got its value, for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Declared,
    Name,
    Tables,
    Collection,
    Drawing,
    Code,
    Blank,
    Replacement,
    /// A Private Use value of a symbol font, read through [`dingbats`].
    Dingbat,
}

/// What the codes of a font shown mean once the conversion is done.
#[derive(Debug)]
pub(crate) struct Meaning {
    /// Bytes per code: 1 for a simple font, 2 for a composite one.
    pub(crate) width: usize,
    pub(crate) values: BTreeMap<u32, String>,
}

/// Give every code shown a Unicode value, from level u on. Returns what the codes of each font
/// mean, by the location of the font.
#[allow(clippy::too_many_lines)]
pub(crate) fn apply(
    doc: &mut Document,
    scan: &Scan,
    level: Level,
    report: &mut Report,
) -> Result<BTreeMap<Location, Meaning>> {
    let no_private_use = level == Level::A;
    let mut subjects = Vec::new();
    let mut collections = Collections::new();
    for (location, used) in &scan.fonts {
        let Some(font) = edit::read_dict(doc, location)? else {
            continue;
        };
        let subtype = font
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or_default();
        if subtype == b"Type0" {
            if used.cids.is_empty() {
                // Codes read through another CMap: not followed yet.
                let mapped =
                    font.contains_key(b"ToUnicode") || known_collection(doc, location, &font)?;
                report.residual_add("unicode_through_cmap", usize::from(!mapped));
                continue;
            }
            if let Some(subject) =
                composite_subject(doc, location, &font, used, &mut collections, no_private_use)?
            {
                subjects.push(subject);
            }
        } else if !used.codes.is_empty() {
            subjects.push(simple_subject(doc, location, &font, used, no_private_use)?);
        }
        // A font drawn without a program of its own is drawn with another font, which readers
        // pick glyphs from by the Unicode values of `ToUnicode` (PDFium does): writing one
        // would change what the page shows (measured 14/11/2026). So is a font whose program
        // cannot be read (PDFium then forgets it). Such a font is left as it is: counted
        // already when it has no program (`font_not_embedded`), here when it cannot be read.
        // A Type 3 font draws its own glyphs.
        if used.rendered
            && subtype != b"Type3"
            && let Some(last) = subjects.last()
            && last.location == *location
            && last.face.is_none()
        {
            report.residual_add(
                "unicode_program_unreadable",
                usize::from(last.file.is_some()),
            );
            subjects.pop();
        }
    }

    // What each code means, by the font itself first; the rest is decided below.
    let mut decided: Vec<BTreeMap<u32, (String, Source)>> = Vec::with_capacity(subjects.len());
    let mut pending: Vec<(usize, u32)> = Vec::new();
    let mut private_dropped = 0;
    for (index, subject) in subjects.iter_mut().enumerate() {
        let mut values = BTreeMap::new();
        let codes: Vec<u32> = subject.codes.keys().copied().collect();
        for code in codes {
            let mut dropped = false;
            let mut keep = |subject: &Subject, text: Option<String>, source: Source| {
                let text = text?;
                let kept = subject.settle(code, text, source);
                dropped |= kept.is_none();
                kept
            };
            let declared = subject.declared(code);
            let mut found = keep(subject, declared, Source::Declared);
            if found.is_none() {
                let named = subject.named(code);
                found = keep(subject, named, Source::Name);
            }
            if found.is_none() {
                let tables = subject.by_tables(code);
                found = keep(subject, tables, Source::Tables);
            }
            if found.is_none() {
                let collection = subject.by_collection(code);
                found = keep(subject, collection, Source::Collection);
            }
            private_dropped += usize::from(dropped);
            match found {
                Some(value) => {
                    values.insert(code, value);
                }
                None => pending.push((index, code)),
            }
        }
        decided.push(values);
    }
    report.change("unicode_private_use_dropped", private_dropped);

    if !pending.is_empty() {
        let index = drawings(&mut subjects, &decided, no_private_use);
        for (at, code) in pending {
            let subject = &subjects[at];
            let drawn = subject.drawing(code);
            let (text, source) = if let Some(text) = drawn.and_then(|shape| index.get(&shape)) {
                (text.clone(), Source::Drawing)
            } else if let Some(text) = subject.as_latin1(code) {
                (text, Source::Code)
            } else if drawn == Some(Drawing::Blank) {
                (" ".to_owned(), Source::Blank)
            } else {
                ("\u{FFFD}".to_owned(), Source::Replacement)
            };
            decided[at].insert(code, (text, source));
        }
    }

    for (subject, values) in subjects.iter().zip(&decided) {
        if values
            .values()
            .all(|(_, source)| *source == Source::Declared)
        {
            continue;
        }
        write(doc, subject, values)?;
        report.change("to_unicode_written", 1);
        let count = |wanted: Source| values.values().filter(|(_, s)| *s == wanted).count();
        report.change("unicode_from_glyph_name", count(Source::Name));
        report.change("unicode_from_program", count(Source::Tables));
        report.change("unicode_from_collection", count(Source::Collection));
        report.change("unicode_from_code", count(Source::Code));
        report.change("unicode_blank_as_space", count(Source::Blank));
        report.change("unicode_from_drawing", count(Source::Drawing));
        report.change("unicode_replacement_character", count(Source::Replacement));
        report.change("unicode_from_dingbat_table", count(Source::Dingbat));
    }
    Ok(subjects
        .iter()
        .zip(decided)
        .map(|(subject, values)| {
            let meaning = Meaning {
                width: subject.width,
                values: values
                    .into_iter()
                    .map(|(code, (text, _))| (code, text))
                    .collect(),
            };
            (subject.location.clone(), meaning)
        })
        .collect())
}

/// The meaning of each drawing in the document whose meaning is known: the glyphs shown with
/// a value the font itself or its program gives, and the named glyphs of small programs.
/// Where drawings agree point for point but meanings differ, the most frequent meaning wins.
fn drawings(
    subjects: &mut [Subject],
    decided: &[BTreeMap<u32, (String, Source)>],
    no_private_use: bool,
) -> HashMap<Drawing, String> {
    let mut votes: HashMap<Drawing, BTreeMap<String, usize>> = HashMap::new();
    let mut vote = |drawing: Option<Drawing>, text: &str| {
        if no_private_use && private_use(text) {
            return;
        }
        if let Some(drawing @ Drawing::Shape { .. }) = drawing {
            *votes
                .entry(drawing)
                .or_default()
                .entry(text.to_owned())
                .or_default() += 1;
        }
    };
    let mut indexed: HashSet<ObjId> = HashSet::new();
    for (subject, values) in subjects.iter_mut().zip(decided) {
        for (code, (text, _)) in values {
            vote(subject.drawing(*code), text);
        }
        let (Some(file), Some(face)) = (subject.file, subject.face.as_ref()) else {
            continue;
        };
        // A program several fonts share is read once.
        if face.num_glyphs() > MAX_INDEXED_GLYPHS || !indexed.insert(file) {
            continue;
        }
        let tables = subject.tables.get_or_insert_with(|| Tables::new(face));
        for gid in 1..face.num_glyphs() {
            let glyph = GlyphId::new(gid);
            if let Some(text) = tables.text(face, glyph) {
                vote(drawing(face, glyph), &text);
            }
        }
    }
    votes
        .into_iter()
        .filter_map(|(drawing, texts)| {
            let best = texts
                .into_iter()
                .max_by(|(a, n), (b, m)| n.cmp(m).then_with(|| b.cmp(a)))?;
            Some((drawing, best.0))
        })
        .collect()
}

/// Give the font a `ToUnicode` CMap: what its own said, what the glyph names of its encoding
/// say (veraPDF reads only the CMap once there is one), and the value of every code shown.
fn write(
    doc: &mut Document,
    subject: &Subject,
    values: &BTreeMap<u32, (String, Source)>,
) -> Result<()> {
    let mut mappings: BTreeMap<u32, String> = subject
        .names
        .iter()
        .filter_map(|(code, name)| Some((*code, to_unicode::unicode_of(name)?)))
        .collect();
    if let Some(Some(cmap)) = &subject.to_unicode {
        let last: u32 = if subject.width == 1 { 0xFF } else { 0xFFFF };
        for code in 0..=last {
            if let Some(text) = cmap
                .lookup_bf_string(code)
                .map(bf_text)
                .filter(|t| to_unicode::allowed(t))
            {
                mappings.insert(code, text);
            }
        }
    }
    for (code, (text, _)) in values {
        mappings.insert(*code, text.clone());
    }
    let mappings: Vec<(u32, String)> = mappings.into_iter().collect();
    let id = doc.add_object(Object::Stream(flate_stream(
        Dict::new(),
        &to_unicode::cmap(subject.width, &mappings),
    )))?;
    edit::edit_dict(doc, &subject.location, false, |dict| {
        dict.insert("ToUnicode", id);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Read as _;

    use sealwax_core::{Document, OpenOptions, SaveOptions};

    use super::super::truetype::test_sfnt;
    use crate::{Level, Options, Report, convert};

    /// A TrueType program of three glyphs, 1000 units per em: `.notdef` and glyph 1 draw
    /// nothing, glyph 2 is a triangle.
    fn program() -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        let mut hhea = vec![0u8; 36];
        hhea[34..36].copy_from_slice(&3u16.to_be_bytes());
        let mut maxp = 0x0001_0000u32.to_be_bytes().to_vec();
        maxp.extend_from_slice(&3u16.to_be_bytes());
        maxp.extend_from_slice(&[0; 26]);
        let mut hmtx = Vec::new();
        for advance in [500u16, 278, 600] {
            hmtx.extend_from_slice(&advance.to_be_bytes());
            hmtx.extend_from_slice(&0i16.to_be_bytes());
        }
        // One contour of three points on the curve, coordinates as two-byte deltas.
        let mut triangle = Vec::new();
        for value in [1i16, 0, 0, 500, 700] {
            triangle.extend_from_slice(&value.to_be_bytes());
        }
        triangle.extend_from_slice(&2u16.to_be_bytes());
        triangle.extend_from_slice(&0u16.to_be_bytes());
        triangle.extend_from_slice(&[0x01; 3]);
        for value in [0i16, 500, -250, 0, 0, 700] {
            triangle.extend_from_slice(&value.to_be_bytes());
        }
        // The short `loca` counts in words.
        triangle.push(0);
        let mut loca = Vec::new();
        for offset in [0u16, 0, 0, u16::try_from(triangle.len() / 2).unwrap()] {
            loca.extend_from_slice(&offset.to_be_bytes());
        }
        test_sfnt(&[
            (*b"glyf", triangle),
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (*b"loca", loca),
            (*b"maxp", maxp),
        ])
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        [
            format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes(),
            data.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat()
    }

    fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    const EMPTY_CMAP: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> \
        endcodespacerange endcmap CMapName currentdict /CMap defineresource pop end end";

    const TRIANGLE_CMAP: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> \
        endcodespacerange 1 beginbfchar <0002> <25B2> endbfchar endcmap \
        CMapName currentdict /CMap defineresource pop end end";

    /// What the page shows: CIDs 1 and 2 with F1, whose `ToUnicode` maps nothing, and CID 2
    /// with F2, another subset of the same program whose `ToUnicode` says what the triangle
    /// means.
    const SHOWN: &[u8] = b"BT /F1 12 Tf 10 10 Td <00010002> Tj /F2 12 Tf <0002> Tj ET";

    /// CID 2, the triangle, in the Private Use Area: Wingdings's check mark as Word maps it.
    const PRIVATE_CMAP: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> \
        endcodespacerange 1 beginbfchar <0002> <F0FC> endbfchar endcmap \
        CMapName currentdict /CMap defineresource pop end end";

    fn document(content: &[u8]) -> Vec<u8> {
        fixture(content, "ABCDEF+Shapes", TRIANGLE_CMAP)
    }

    /// The fonts of the tests, F2 named `second` with the `ToUnicode` `second_cmap`.
    fn fixture(content: &[u8], second: &str, second_cmap: &[u8]) -> Vec<u8> {
        let composite = |name: &str, descendant: u32, to_unicode: u32| {
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H \
                 /DescendantFonts [{descendant} 0 R] /ToUnicode {to_unicode} 0 R >>"
            )
            .into_bytes()
        };
        let cid_font = |descriptor: u32| {
            format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /ABCDEF+Shapes \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor {descriptor} 0 R /CIDToGIDMap /Identity /DW 1000 \
                 /W [0 [500 278 600]] >>"
            )
            .into_bytes()
        };
        let descriptor = |file: u32| {
            format!(
                "<< /Type /FontDescriptor /FontName /ABCDEF+Shapes /Flags 4 \
                 /FontBBox [0 0 500 700] /ItalicAngle 0 /Ascent 700 /Descent 0 /CapHeight 700 \
                 /StemV 80 /FontFile2 {file} 0 R >>"
            )
            .into_bytes()
        };
        let program = program();
        let length = format!("/Length1 {}", program.len());
        pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] \
               /Resources << /Font << /F1 5 0 R /F2 10 0 R /F3 15 0 R /F4 19 0 R /F5 18 0 R >> >> \
               /Contents 4 0 R >>"
                .to_vec(),
            stream("", content),
            composite("ABCDEF+Shapes", 6, 9),
            cid_font(7),
            descriptor(8),
            stream(&length, &program),
            stream("", EMPTY_CMAP),
            composite(second, 11, 14),
            cid_font(12),
            descriptor(13),
            stream(&length, &program),
            stream("", second_cmap),
            // F3: a simple font whose `ToUnicode` maps nothing, and whose encoding names glyphs.
            b"<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Shapes /FirstChar 1 \
               /LastChar 2 /Widths [278 600] /Encoding << /Differences [1 /uni00A0 /bullet] >> \
               /FontDescriptor 16 0 R /ToUnicode 9 0 R >>"
                .to_vec(),
            descriptor(17),
            stream(&length, &program),
            // F5: a simple font naming its glyph with a name that says nothing.
            b"<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Shapes /FirstChar 65 \
               /LastChar 65 /Widths [600] /Encoding << /Differences [65 /g65] >> \
               /FontDescriptor 16 0 R /ToUnicode 9 0 R >>"
                .to_vec(),
            // F4: a composite font of Adobe-Japan1, not embedded, without `ToUnicode`.
            b"<< /Type /Font /Subtype /Type0 /BaseFont /KozMinPro-Regular-Acro \
               /Encoding /Identity-H /DescendantFonts [20 0 R] >>"
                .to_vec(),
            b"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /KozMinPro-Regular-Acro \
               /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >> \
               /FontDescriptor 21 0 R /DW 1000 >>"
                .to_vec(),
            b"<< /Type /FontDescriptor /FontName /KozMinPro-Regular-Acro /Flags 4 \
               /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 880 /Descent -120 \
               /CapHeight 700 /StemV 80 >>"
                .to_vec(),
        ])
    }

    fn converted(level: Level) -> (Document, Report) {
        converted_from(document(SHOWN), level)
    }

    fn converted_from(data: Vec<u8>, level: Level) -> (Document, Report) {
        let mut doc = Document::open(data, &OpenOptions::default()).unwrap();
        let report = convert(&mut doc, &Options::new(level)).unwrap();
        let saved = doc.save(&SaveOptions::default()).unwrap();
        (
            Document::open(saved, &OpenOptions::default()).unwrap(),
            report,
        )
    }

    /// The `ToUnicode` of font `name` on the page: its data, and what it maps `code` to.
    fn to_unicode(doc: &Document, name: &[u8], code: u32) -> (Vec<u8>, Option<String>) {
        let get = |object: &sealwax_core::Object| match object {
            sealwax_core::Object::Ref(id) => doc.object(*id).unwrap().unwrap(),
            other => other.clone(),
        };
        let page = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
        let resources = get(page.as_dict().unwrap().get(b"Resources").unwrap());
        let fonts = get(resources.as_dict().unwrap().get(b"Font").unwrap());
        let font = get(fonts.as_dict().unwrap().get(name).unwrap());
        let reference = font.as_dict().unwrap().get(b"ToUnicode").unwrap().clone();
        let sealwax_core::Object::Stream(stream) = get(&reference) else {
            unreachable!("not a stream");
        };
        let mut data = stream.data.clone();
        if stream.dict.contains_key(b"Filter") {
            data.clear();
            flate2::read::ZlibDecoder::new(stream.data.as_slice())
                .read_to_end(&mut data)
                .unwrap();
        }
        let cmap = hayro_cmap::CMap::parse(&data, |_| None).unwrap();
        let text = cmap.lookup_bf_string(code).map(super::bf_text);
        (data, text)
    }

    #[test]
    fn glyphs_without_meaning_get_one_from_their_drawing() {
        let (doc, report) = converted(Level::U);
        // The blank glyph is a space; the triangle means what the other subset says.
        assert_eq!(to_unicode(&doc, b"F1", 1).1.as_deref(), Some(" "));
        assert_eq!(to_unicode(&doc, b"F1", 2).1.as_deref(), Some("\u{25B2}"));
        let changes = report.changes();
        assert_eq!(changes.get("unicode_blank_as_space"), Some(&1));
        assert_eq!(changes.get("unicode_from_drawing"), Some(&1));
        assert_eq!(changes.get("to_unicode_written"), Some(&1));
        assert!(report.residual().is_empty(), "{:?}", report.residual());
    }

    #[test]
    fn a_font_whose_codes_all_map_keeps_its_cmap() {
        let original = to_unicode(
            &Document::open(document(SHOWN), &OpenOptions::default()).unwrap(),
            b"F2",
            2,
        );
        let (doc, _) = converted(Level::U);
        assert_eq!(to_unicode(&doc, b"F2", 2), original);
    }

    #[test]
    fn level_b_does_not_look_at_unicode() {
        let (doc, report) = converted(Level::B);
        assert_eq!(to_unicode(&doc, b"F1", 1), (EMPTY_CMAP.to_vec(), None));
        assert!(!report.changes().contains_key("to_unicode_written"));
    }

    #[test]
    fn a_cmap_that_hides_the_glyph_names_gets_their_values() {
        // veraPDF reads the CMap alone once there is one: the names go into it.
        let (doc, report) = converted_from(document(b"BT /F3 12 Tf 10 10 Td <01> Tj ET"), Level::U);
        assert_eq!(to_unicode(&doc, b"F3", 1).1.as_deref(), Some("\u{A0}"));
        assert_eq!(to_unicode(&doc, b"F3", 2).1.as_deref(), Some("\u{2022}"));
        assert_eq!(report.changes().get("unicode_from_glyph_name"), Some(&1));
    }

    #[test]
    fn a_cid_of_a_known_collection_and_a_plain_code_keep_their_meaning() {
        // F4 shows invisible text, as an OCR layer does: its program is not needed.
        let (doc, report) = converted_from(
            document(b"BT 3 Tr /F4 12 Tf 10 10 Td <0022> Tj 0 Tr /F5 12 Tf <41> Tj ET"),
            Level::U,
        );
        // CID 34 of Adobe-Japan1 is A; code 0x41 of a font that says nothing is read as PDFium
        // reads it.
        assert_eq!(to_unicode(&doc, b"F4", 0x22).1.as_deref(), Some("A"));
        assert_eq!(to_unicode(&doc, b"F5", 0x41).1.as_deref(), Some("A"));
        let changes = report.changes();
        assert_eq!(changes.get("unicode_from_collection"), Some(&1));
        assert_eq!(changes.get("unicode_from_code"), Some(&1));
        assert!(!changes.contains_key("unicode_replacement_character"));
    }

    #[test]
    fn a_font_drawn_without_its_program_keeps_no_cmap() {
        // Readers draw it with another font, picking glyphs by its `ToUnicode`.
        let (doc, report) =
            converted_from(document(b"BT /F4 12 Tf 10 10 Td <0022> Tj ET"), Level::U);
        let page = doc.object(doc.page_ids()[0].unwrap()).unwrap().unwrap();
        let resources = page.as_dict().unwrap().get(b"Resources").unwrap().clone();
        let fonts = match resources {
            sealwax_core::Object::Dict(dict) => dict.get(b"Font").unwrap().clone(),
            other => doc.object(other.as_ref_id().unwrap()).unwrap().unwrap(),
        };
        let font = doc
            .object(
                fonts
                    .as_dict()
                    .unwrap()
                    .get(b"F4")
                    .unwrap()
                    .as_ref_id()
                    .unwrap(),
            )
            .unwrap()
            .unwrap();
        assert!(!font.as_dict().unwrap().contains_key(b"ToUnicode"));
        assert!(!report.changes().contains_key("to_unicode_written"));
        assert_eq!(report.residual().get("font_not_embedded"), Some(&1));
    }

    #[test]
    fn a_private_use_value_of_wingdings_is_its_character_at_level_a() {
        let data = || {
            fixture(
                b"BT /F2 12 Tf 10 10 Td <0002> Tj ET",
                "ABCDEF+Wingdings-Regular",
                PRIVATE_CMAP,
            )
        };
        // Level u accepts it.
        let (doc, report) = converted_from(data(), Level::U);
        assert_eq!(to_unicode(&doc, b"F2", 2).1.as_deref(), Some("\u{F0FC}"));
        assert!(!report.changes().contains_key("to_unicode_written"));
        let (doc, report) = converted_from(data(), Level::A);
        assert_eq!(to_unicode(&doc, b"F2", 2).1.as_deref(), Some("\u{2713}"));
        assert_eq!(report.changes().get("unicode_from_dingbat_table"), Some(&1));
    }

    #[test]
    fn a_private_use_value_of_another_font_is_no_value_at_level_a() {
        let data = fixture(
            b"BT /F2 12 Tf 10 10 Td <0002> Tj ET",
            "ABCDEF+Shapes",
            PRIVATE_CMAP,
        );
        let (doc, report) = converted_from(data, Level::A);
        // Nothing else says what the triangle means.
        assert_eq!(to_unicode(&doc, b"F2", 2).1.as_deref(), Some("\u{FFFD}"));
        let changes = report.changes();
        assert_eq!(changes.get("unicode_private_use_dropped"), Some(&1));
        assert_eq!(changes.get("unicode_replacement_character"), Some(&1));
    }

    #[test]
    fn a_symbol_glyph_adobe_names_in_the_private_use_area_is_its_character_at_level_a() {
        // `registerserif`, code 0xD2 of Symbol, is U+F6DA by the Adobe Glyph List.
        let cmap: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
            /CMapName /Adobe-Identity-UCS def /CMapType 2 def 1 begincodespacerange <00> <FF> \
            endcodespacerange 1 beginbfchar <D2> <F6DA> endbfchar endcmap \
            CMapName currentdict /CMap defineresource pop end end";
        let data = pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] \
               /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
            stream("", b"BT /F1 12 Tf 10 10 Td <D2> Tj ET"),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Symbol /ToUnicode 6 0 R >>".to_vec(),
            stream("", cmap),
        ]);
        let (doc, report) = converted_from(data, Level::A);
        assert_eq!(to_unicode(&doc, b"F1", 0xD2).1.as_deref(), Some("\u{AE}"));
        assert_eq!(report.changes().get("unicode_from_dingbat_table"), Some(&1));
    }

    #[test]
    fn a_glyph_nothing_recognizes_is_the_replacement_character() {
        // F2 shows nothing: nothing says what the triangle means.
        let data = document(b"BT /F1 12 Tf 10 10 Td <00010002> Tj ET");
        let mut doc = Document::open(data, &OpenOptions::default()).unwrap();
        let report = convert(&mut doc, &Options::new(Level::U)).unwrap();
        assert_eq!(
            report.changes().get("unicode_replacement_character"),
            Some(&1)
        );
    }
}
