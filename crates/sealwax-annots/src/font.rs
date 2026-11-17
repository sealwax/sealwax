//! TrueType fonts a caller gives to draw free texts with: a program read once (its `cmap`,
//! advances and metrics), the glyphs a drawing uses recorded, and the font written as a composite
//! font (`Type0`, `Identity-H`, a `CIDFontType2` whose CIDs are the program's glyph ids) with
//! a subset of the program (`FontFile2`: the glyphs used kept, the others emptied, every id in
//! its place), its widths from the program's own and a `ToUnicode` map: the text reads back.
//!
//! Only TrueType outlines (`glyf`) are taken: the subset is cut by `loca`. A character the font
//! does not have is drawn as its `?` (or left out when it has none), never as `.notdef`, and
//! counted as replaced.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use read_fonts::types::{NameId, Tag};
use read_fonts::{FontRef, TableProvider as _};
use sealwax_core::{Dict, Document, Name, ObjId, Object};

use crate::Result;
use crate::pdf;

/// The tables a subset keeps: what a PDF reader needs to draw TrueType glyphs (ISO 32000-1,
/// 9.9), and `OS/2`; `post` is cut to its header.
const KEPT: [&[u8; 4]; 10] = [
    b"OS/2", b"cvt ", b"fpgm", b"glyf", b"head", b"hhea", b"hmtx", b"loca", b"maxp", b"prep",
];
/// How deep a composite glyph's components may nest.
const MAX_NESTING: usize = 16;
/// The largest program taken.
const MAX_PROGRAM: usize = 64 << 20;

/// Fonts a caller gives for free texts: by the name an XFDF gives (`Caveat`), and one for every
/// other name. Without any, texts are drawn in Helvetica.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TextFonts {
    named: Vec<(String, Arc<Vec<u8>>)>,
    default: Option<Arc<Vec<u8>>>,
}

impl std::fmt::Debug for TextFonts {
    // The programs' names and sizes, not their bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextFonts")
            .field(
                "named",
                &self
                    .named
                    .iter()
                    .map(|(name, program)| (name.as_str(), program.len()))
                    .collect::<Vec<_>>(),
            )
            .field(
                "default",
                &self.default.as_ref().map(|program| program.len()),
            )
            .finish()
    }
}

impl TextFonts {
    /// No font: Helvetica.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `program` (TrueType) for the texts whose font is `name` (compared without case).
    #[must_use]
    pub fn named(mut self, name: impl Into<String>, program: Vec<u8>) -> Self {
        self.named.push((name.into(), Arc::new(program)));
        self
    }

    /// `program` (TrueType) for the texts whose font has no program of its own.
    #[must_use]
    pub fn default_font(mut self, program: Vec<u8>) -> Self {
        self.default = Some(Arc::new(program));
        self
    }

    /// Whether no font is given.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.named.is_empty() && self.default.is_none()
    }

    /// The programs read, refused when one is no TrueType program.
    pub(crate) fn read(&self) -> Result<ReadFonts> {
        let read = |data: &Arc<Vec<u8>>| {
            Program::parse(Arc::clone(data))
                .map(Arc::new)
                .ok_or(sealwax_core::Error::InvalidArgument { what: "font" })
        };
        let mut named = Vec::with_capacity(self.named.len());
        for (name, data) in &self.named {
            named.push((name.to_lowercase(), read(data)?));
        }
        let default = self.default.as_ref().map(read).transpose()?;
        Ok(ReadFonts { named, default })
    }
}

/// [`TextFonts`] read.
pub(crate) struct ReadFonts {
    named: Vec<(String, Arc<Program>)>,
    default: Option<Arc<Program>>,
}

impl ReadFonts {
    /// The program for a text whose font is `name`: its own, else the default; `None` for
    /// Helvetica.
    pub(crate) fn for_name(&self, name: Option<&str>) -> Option<&Arc<Program>> {
        let wanted = name.map(str::to_lowercase);
        self.named
            .iter()
            .find(|(named, _)| Some(named) == wanted.as_ref())
            .map(|(_, program)| program)
            .or(self.default.as_ref())
    }
}

/// A TrueType program, read.
pub(crate) struct Program {
    data: Arc<Vec<u8>>,
    units_per_em: f64,
    glyphs: u16,
    /// In thousandths of an em: what a font descriptor says.
    bbox: [f64; 4],
    ascent: f64,
    descent: f64,
    cap_height: f64,
    italic_angle: f64,
    /// The PostScript name, kept to what a PDF name holds well.
    name: String,
}

impl Program {
    fn parse(data: Arc<Vec<u8>>) -> Option<Self> {
        if data.len() > MAX_PROGRAM {
            return None;
        }
        let font = FontRef::new(&data).ok()?;
        font.table_data(Tag::new(b"glyf"))?;
        font.table_data(Tag::new(b"loca"))?;
        font.cmap().ok()?;
        font.hmtx().ok()?;
        let head = font.head().ok()?;
        let units_per_em = f64::from(head.units_per_em());
        if !(16.0..=16384.0).contains(&units_per_em) {
            return None;
        }
        let glyphs = font.maxp().ok()?.num_glyphs();
        let em = |value: f64| value * 1000.0 / units_per_em;
        let hhea = font.hhea().ok()?;
        let ascent = em(f64::from(hhea.ascender().to_i16()));
        let descent = em(f64::from(hhea.descender().to_i16()));
        let cap_height = font
            .os2()
            .ok()
            .and_then(|os2| os2.s_cap_height())
            .map_or(ascent, |cap| em(f64::from(cap)));
        let italic_angle = font
            .post()
            .map(|post| post.italic_angle().to_f64())
            .unwrap_or(0.0);
        let name = font
            .name()
            .ok()
            .and_then(|table| {
                table
                    .name_record()
                    .iter()
                    .filter(|record| record.name_id() == NameId::POSTSCRIPT_NAME)
                    .find_map(|record| {
                        let text: String =
                            record.string(table.string_data()).ok()?.chars().collect();
                        Some(text)
                    })
            })
            .map(|name| {
                name.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                    .take(63)
                    .collect::<String>()
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Font".to_owned());
        Some(Self {
            bbox: [
                em(f64::from(head.x_min())),
                em(f64::from(head.y_min())),
                em(f64::from(head.x_max())),
                em(f64::from(head.y_max())),
            ],
            data,
            units_per_em,
            glyphs,
            ascent,
            descent,
            cap_height,
            italic_angle,
            name,
        })
    }

    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::new(&self.data).ok()
    }

    /// The glyph of `character`; `None` when the font has none (or only `.notdef`).
    fn glyph(&self, character: char) -> Option<u16> {
        let glyph = self.font()?.cmap().ok()?.map_codepoint(character)?;
        u16::try_from(glyph.to_u32())
            .ok()
            .filter(|g| *g != 0 && *g < self.glyphs)
    }

    /// The advance of `glyph`, in thousandths of an em.
    fn advance(&self, glyph: u16) -> f64 {
        let advance = self
            .font()
            .and_then(|font| font.hmtx().ok()?.advance(glyph.into()))
            .unwrap_or(0);
        f64::from(advance) * 1000.0 / self.units_per_em
    }
}

/// A font a drawing writes text with: Helvetica, or a program embedded.
pub(crate) trait TextFont {
    /// The bytes of the string operand that shows `text`; the characters it cannot show are
    /// counted in `replaced`.
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8>;
    /// The width of `encoded` at `size`.
    fn width(&self, encoded: &[u8], size: f64) -> f64;
    /// `encoded` as a string operand.
    fn operand(&self, encoded: &[u8]) -> Vec<u8>;
    /// The font's name in the resources.
    fn resource(&self) -> &str;
}

/// Helvetica, WinAnsiEncoding.
pub(crate) struct Helvetica;

impl TextFont for Helvetica {
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8> {
        crate::text::encode(text, replaced)
    }

    fn width(&self, encoded: &[u8], size: f64) -> f64 {
        crate::text::width(encoded, size)
    }

    fn operand(&self, encoded: &[u8]) -> Vec<u8> {
        crate::text::literal(encoded)
    }

    fn resource(&self) -> &str {
        "Helv"
    }
}

/// Helvetica-Bold, WinAnsiEncoding.
pub(crate) struct HelveticaBold;

impl HelveticaBold {
    /// Whether a text's font, as an XFDF names it, is Helvetica-Bold.
    pub(crate) fn named(name: Option<&str>) -> bool {
        name.is_some_and(|name| {
            let name: String = name
                .chars()
                .filter(|c| !matches!(c, '-' | ' ' | ',' | '_'))
                .collect();
            name.eq_ignore_ascii_case("HelveticaBold")
        })
    }
}

impl TextFont for HelveticaBold {
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8> {
        crate::text::encode(text, replaced)
    }

    fn width(&self, encoded: &[u8], size: f64) -> f64 {
        crate::text::width_bold(encoded, size)
    }

    fn operand(&self, encoded: &[u8]) -> Vec<u8> {
        crate::text::literal(encoded)
    }

    fn resource(&self) -> &str {
        "HeBo"
    }
}

/// A program a document embeds, as the drawings use it: the glyphs they show and the
/// characters each stands for.
pub(crate) struct Embedded {
    program: Arc<Program>,
    resource: String,
    used: RefCell<BTreeMap<u16, String>>,
    /// The object the font is written to once every drawing is made.
    pub(crate) id: ObjId,
}

impl Embedded {
    /// `program`, written at `id` by [`Embedded::write`], named `resource` in the drawings'
    /// resources.
    pub(crate) fn new(program: Arc<Program>, id: ObjId, resource: String) -> Self {
        Self {
            program,
            resource,
            used: RefCell::new(BTreeMap::new()),
            id,
        }
    }

    /// Write the font at its object: the subset of the glyphs used, their widths, the
    /// characters they stand for.
    pub(crate) fn write(&self, doc: &mut Document) -> Result<()> {
        let used = self.used.borrow();
        let keep: BTreeSet<u16> = used.keys().copied().collect();
        let subset = subset(&self.program.data, &keep)
            .ok_or(sealwax_core::Error::InvalidArgument { what: "font" })?;
        let tag = subset_tag(&keep, &self.program.name);
        let base_font = format!("{tag}+{}", self.program.name);

        let mut file = Dict::new();
        file.insert(
            "Length1",
            Object::Integer(i64::try_from(subset.len()).unwrap_or(i64::MAX)),
        );
        let file = doc.add_object(pdf::flate(file, &subset))?;

        let program = &self.program;
        let mut descriptor = Dict::new();
        descriptor.insert("Type", Name::from("FontDescriptor"));
        descriptor.insert("FontName", Name::from(base_font.as_str()));
        // Symbolic: its glyphs are reached by id, not by a standard encoding.
        descriptor.insert("Flags", Object::Integer(4));
        descriptor.insert("FontBBox", pdf::numbers(&program.bbox.map(f64::round)));
        descriptor.insert("ItalicAngle", Object::Real(program.italic_angle));
        descriptor.insert("Ascent", Object::Real(program.ascent.round()));
        descriptor.insert("Descent", Object::Real(program.descent.round()));
        descriptor.insert("CapHeight", Object::Real(program.cap_height.round()));
        descriptor.insert("StemV", Object::Integer(80));
        descriptor.insert("FontFile2", Object::Ref(file));
        let descriptor = doc.add_object(Object::Dict(descriptor))?;

        let mut widths = Vec::new();
        for glyph in &keep {
            widths.push(Object::Integer(i64::from(*glyph)));
            widths.push(Object::Array(vec![Object::Real(
                (program.advance(*glyph) * 1000.0).round() / 1000.0,
            )]));
        }
        let mut system = Dict::new();
        system.insert("Registry", Object::String(b"Adobe".to_vec()));
        system.insert("Ordering", Object::String(b"Identity".to_vec()));
        system.insert("Supplement", Object::Integer(0));
        let mut cid_font = Dict::new();
        cid_font.insert("Type", Name::from("Font"));
        cid_font.insert("Subtype", Name::from("CIDFontType2"));
        cid_font.insert("BaseFont", Name::from(base_font.as_str()));
        cid_font.insert("CIDSystemInfo", Object::Dict(system));
        cid_font.insert("FontDescriptor", Object::Ref(descriptor));
        cid_font.insert("CIDToGIDMap", Name::from("Identity"));
        cid_font.insert("DW", Object::Integer(0));
        cid_font.insert("W", Object::Array(widths));
        let cid_font = doc.add_object(Object::Dict(cid_font))?;

        let to_unicode = doc.add_object(pdf::flate(Dict::new(), to_unicode(&used).as_bytes()))?;

        let mut font = Dict::new();
        font.insert("Type", Name::from("Font"));
        font.insert("Subtype", Name::from("Type0"));
        font.insert("BaseFont", Name::from(base_font.as_str()));
        font.insert("Encoding", Name::from("Identity-H"));
        font.insert(
            "DescendantFonts",
            Object::Array(vec![Object::Ref(cid_font)]),
        );
        font.insert("ToUnicode", Object::Ref(to_unicode));
        doc.set_object(self.id, Object::Dict(font));
        Ok(())
    }
}

impl TextFont for Embedded {
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 * text.len());
        let mut used = self.used.borrow_mut();
        for character in text.chars() {
            let (glyph, shown) = match self.program.glyph(character) {
                Some(glyph) => (Some(glyph), character),
                None => {
                    *replaced += 1;
                    (self.program.glyph('?'), '?')
                }
            };
            if let Some(glyph) = glyph {
                out.extend_from_slice(&glyph.to_be_bytes());
                used.entry(glyph).or_insert_with(|| shown.to_string());
            }
        }
        out
    }

    fn width(&self, encoded: &[u8], size: f64) -> f64 {
        encoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| self.program.advance(u16::from_be_bytes(*pair)))
            .sum::<f64>()
            * size
            / 1000.0
    }

    fn operand(&self, encoded: &[u8]) -> Vec<u8> {
        let mut out = String::with_capacity(2 * encoded.len() + 2);
        out.push('<');
        for byte in encoded {
            let _ = write!(out, "{byte:02X}");
        }
        out.push('>');
        out.into_bytes()
    }

    fn resource(&self) -> &str {
        &self.resource
    }
}

/// Six capital letters standing for the subset (ISO 32000-1, 9.6.4), the same for the same
/// glyphs of the same font.
fn subset_tag(glyphs: &BTreeSet<u16>, name: &str) -> String {
    // FNV-1a over the name and the glyphs.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name
        .bytes()
        .chain(glyphs.iter().flat_map(|g| g.to_be_bytes()))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    (0..6)
        .map(|i| {
            #[allow(clippy::cast_possible_truncation)]
            let letter = ((hash >> (i * 8)) % 26) as u8;
            char::from(b'A' + letter)
        })
        .collect()
}

/// The `ToUnicode` CMap of the glyphs used.
fn to_unicode(used: &BTreeMap<u16, String>) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &String)> = used.iter().collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (glyph, text) in chunk {
            let mut units = String::new();
            for unit in text.encode_utf16() {
                let _ = write!(units, "{unit:04X}");
            }
            let _ = writeln!(out, "<{glyph:04X}> <{units}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

// --- the subset ---------------------------------------------------------------------------

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// The tables of a TrueType program, by tag.
fn tables(data: &[u8]) -> Option<BTreeMap<[u8; 4], &[u8]>> {
    let count = usize::from(u16_at(data, 4)?);
    let mut found = BTreeMap::new();
    for i in 0..count {
        let record = 12 + 16 * i;
        let tag: [u8; 4] = data.get(record..record + 4)?.try_into().ok()?;
        let offset = usize::try_from(u32_at(data, record + 8)?).ok()?;
        let length = usize::try_from(u32_at(data, record + 12)?).ok()?;
        found.insert(tag, data.get(offset..offset.checked_add(length)?)?);
    }
    Some(found)
}

/// The program with the glyphs of `keep` (and `.notdef`, and the components of composite
/// glyphs) kept, every other glyph emptied, each glyph keeping its id; the tables a PDF reader
/// does not need left out.
fn subset(data: &[u8], keep: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let tables = tables(data)?;
    let head = *tables.get(b"head")?;
    let glyf = *tables.get(b"glyf")?;
    let loca = *tables.get(b"loca")?;
    let glyphs = usize::from(u16_at(tables.get(b"maxp")?, 4)?);
    let long = u16_at(head, 50)? == 1;
    let offset = |glyph: usize| -> Option<usize> {
        if long {
            usize::try_from(u32_at(loca, 4 * glyph)?).ok()
        } else {
            Some(2 * usize::from(u16_at(loca, 2 * glyph)?))
        }
    };
    let glyph_data = |glyph: usize| -> Option<&[u8]> {
        let (start, end) = (offset(glyph)?, offset(glyph + 1)?);
        if end < start {
            return None;
        }
        glyf.get(start..end)
    };
    // The glyphs kept, composite glyphs' components with them.
    let mut kept = BTreeSet::from([0usize]);
    let mut queue: Vec<(usize, usize)> = keep
        .iter()
        .map(|g| (usize::from(*g), 0))
        .filter(|(g, _)| *g < glyphs)
        .collect();
    while let Some((glyph, depth)) = queue.pop() {
        if !kept.insert(glyph) && glyph != 0 {
            continue;
        }
        let outline = glyph_data(glyph)?;
        if outline.len() < 10 || (u16_at(outline, 0)? as i16) >= 0 || depth >= MAX_NESTING {
            continue;
        }
        let mut at = 10;
        loop {
            let flags = u16_at(outline, at)?;
            let component = usize::from(u16_at(outline, at + 2)?);
            if component < glyphs && !kept.contains(&component) {
                queue.push((component, depth + 1));
            }
            at += 4 + if flags & 0x0001 != 0 { 4 } else { 2 };
            at += if flags & 0x0008 != 0 {
                2
            } else if flags & 0x0040 != 0 {
                4
            } else if flags & 0x0080 != 0 {
                8
            } else {
                0
            };
            if flags & 0x0020 == 0 {
                break;
            }
        }
    }
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity(4 * (glyphs + 1));
    for glyph in 0..glyphs {
        new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).ok()?.to_be_bytes());
        if kept.contains(&glyph) {
            new_glyf.extend_from_slice(glyph_data(glyph)?);
            while new_glyf.len() % 4 != 0 {
                new_glyf.push(0);
            }
        }
    }
    new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).ok()?.to_be_bytes());

    let mut new_head = head.to_vec();
    new_head.get_mut(8..12)?.copy_from_slice(&[0; 4]);
    new_head
        .get_mut(50..52)?
        .copy_from_slice(&1u16.to_be_bytes());
    let mut written: BTreeMap<[u8; 4], Vec<u8>> = BTreeMap::new();
    for tag in KEPT {
        let table = match tag {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            other => match tables.get(other) {
                Some(table) => table.to_vec(),
                None => continue,
            },
        };
        written.insert(*tag, table);
    }
    if let Some(post) = tables.get(b"post").and_then(|post| post.get(..32)) {
        let mut post = post.to_vec();
        post[..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
        written.insert(*b"post", post);
    }
    Some(sfnt(&written))
}

/// A TrueType program of `tables`, sorted by tag, with its checksums.
fn sfnt(tables: &BTreeMap<[u8; 4], Vec<u8>>) -> Vec<u8> {
    let count = u16::try_from(tables.len()).unwrap_or(u16::MAX);
    let power = if count == 0 {
        0
    } else {
        15 - count.leading_zeros() as u16
    };
    let search_range = (1u16 << power) * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&power.to_be_bytes());
    out.extend_from_slice(&(count * 16 - search_range).to_be_bytes());
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    let mut head_at = None;
    for (tag, table) in tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(table).to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(&u32::try_from(table.len()).unwrap_or(u32::MAX).to_be_bytes());
        if tag == b"head" {
            head_at = Some(offset);
        }
        body.extend_from_slice(table);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() + body.len();
    }
    out.extend_from_slice(&body);
    if let Some(at) = head_at {
        let adjustment = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[at + 8..at + 12].copy_from_slice(&adjustment.to_be_bytes());
    }
    out
}
