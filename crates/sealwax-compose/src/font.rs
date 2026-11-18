//! Font programs embedded in a document: a face read once (outlines, `cmap`, advances,
//! metrics), the glyphs drawn with it recorded, and the font written as a composite font
//! (`Type0`, `Identity-H`) with a subset of the program and a `ToUnicode` map, so that its text
//! reads back.
//!
//! TrueType outlines (`glyf`) are embedded as a `CIDFontType2` (`FontFile2`), CFF outlines as a
//! `CIDFontType0` (`FontFile3`, `CIDFontType0C`), from a file or from a collection. The subset is
//! cut by [subsetter](https://github.com/typst/subsetter), which numbers the glyphs kept from 1:
//! content streams draw a glyph by the code [`EmbeddedFont::code`] gives it, which is also its
//! CID and its glyph id in the subset.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use read_fonts::types::{NameId, Tag};
use read_fonts::{FontRef, TableProvider as _};
use sealwax_core::{Dict, Document, Name, ObjId, Object, Stream};

/// The largest program taken.
pub const MAX_PROGRAM: usize = 64 << 20;

/// How a program draws its glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outlines {
    TrueType,
    Cff,
}

/// A face of a TrueType or OpenType program, read.
pub struct FontProgram {
    data: Arc<Vec<u8>>,
    face: u32,
    outlines: Outlines,
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

impl std::fmt::Debug for FontProgram {
    // The program's name and size, not its bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontProgram")
            .field("name", &self.name)
            .field("face", &self.face)
            .field("outlines", &self.outlines)
            .field("bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

impl FontProgram {
    /// Face `face` of `data` (0 unless `data` is a collection); `None` when it is no program
    /// that can be embedded: outlines neither in `glyf` nor in `CFF ` (a variable `CFF2` font),
    /// tables missing or unreadable, or larger than [`MAX_PROGRAM`].
    #[must_use]
    pub fn parse(data: Arc<Vec<u8>>, face: u32) -> Option<Self> {
        if data.len() > MAX_PROGRAM {
            return None;
        }
        let font = FontRef::from_index(&data, face).ok()?;
        let outlines = if font.table_data(Tag::new(b"glyf")).is_some()
            && font.table_data(Tag::new(b"loca")).is_some()
        {
            Outlines::TrueType
        } else if font.table_data(Tag::new(b"CFF ")).is_some() {
            Outlines::Cff
        } else {
            return None;
        };
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
        let name = postscript_name(&font).unwrap_or_else(|| "Font".to_owned());
        Some(Self {
            bbox: [
                em(f64::from(head.x_min())),
                em(f64::from(head.y_min())),
                em(f64::from(head.x_max())),
                em(f64::from(head.y_max())),
            ],
            data,
            face,
            outlines,
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
        FontRef::from_index(&self.data, self.face).ok()
    }

    /// The glyph of `character`; `None` when the font has none (or only `.notdef`).
    #[must_use]
    pub fn glyph(&self, character: char) -> Option<u16> {
        let glyph = self.font()?.cmap().ok()?.map_codepoint(character)?;
        u16::try_from(glyph.to_u32())
            .ok()
            .filter(|g| *g != 0 && *g < self.glyphs)
    }

    /// The advance of `glyph`, in thousandths of an em.
    #[must_use]
    pub fn advance(&self, glyph: u16) -> f64 {
        let advance = self
            .font()
            .and_then(|font| font.hmtx().ok()?.advance(glyph.into()))
            .unwrap_or(0);
        f64::from(advance) * 1000.0 / self.units_per_em
    }

    /// How many glyphs the face has.
    #[must_use]
    pub fn glyph_count(&self) -> u16 {
        self.glyphs
    }

    /// The PostScript name, as the font is named in a document.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The PostScript name (name ID 6), kept to letters, digits, `-` and `_`, 63 at most.
fn postscript_name(font: &FontRef<'_>) -> Option<String> {
    let table = font.name().ok()?;
    let name = table
        .name_record()
        .iter()
        .filter(|record| record.name_id() == NameId::POSTSCRIPT_NAME)
        .find_map(|record| {
            let text: String = record.string(table.string_data()).ok()?.chars().collect();
            Some(text)
        })?;
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(63)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The glyphs drawn so far: their codes in the subset, their advances and what they stand for.
struct Drawn {
    remapper: subsetter::GlyphRemapper,
    /// By code: the advance the font's widths give, in thousandths of an em. `.notdef` (code 0)
    /// has the default width, 0.
    advances: Vec<f64>,
    /// By code: the text the first drawing of the glyph gave.
    texts: BTreeMap<u16, String>,
}

impl Default for Drawn {
    fn default() -> Self {
        // The remapper gives `.notdef` code 0 before any glyph is drawn.
        Self {
            remapper: subsetter::GlyphRemapper::new(),
            advances: vec![0.0],
            texts: BTreeMap::new(),
        }
    }
}

/// A program as a document draws with it, written at its object once every drawing is made.
pub struct EmbeddedFont {
    program: Arc<FontProgram>,
    id: ObjId,
    drawn: RefCell<Drawn>,
}

impl std::fmt::Debug for EmbeddedFont {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedFont")
            .field("program", &self.program)
            .field("id", &self.id)
            .field("glyphs", &self.drawn.borrow().remapper.num_gids())
            .finish_non_exhaustive()
    }
}

impl EmbeddedFont {
    /// `program`, to be written at `id` (an object reserved with [`Document::add_object`]) by
    /// [`EmbeddedFont::write`].
    #[must_use]
    pub fn new(program: Arc<FontProgram>, id: ObjId) -> Self {
        Self {
            program,
            id,
            drawn: RefCell::new(Drawn::default()),
        }
    }

    /// The object the font is written to.
    #[must_use]
    pub fn id(&self) -> ObjId {
        self.id
    }

    #[must_use]
    pub fn program(&self) -> &FontProgram {
        &self.program
    }

    /// The code that draws `glyph` (written as two bytes, big-endian), the glyph recorded as
    /// standing for `text` unless an earlier drawing gave it a text. A glyph past the program's
    /// is drawn as `.notdef`, code 0.
    pub fn code(&self, glyph: u16, text: &str) -> u16 {
        if glyph >= self.program.glyphs {
            return 0;
        }
        let mut drawn = self.drawn.borrow_mut();
        let code = drawn.remapper.remap(glyph);
        // Codes are given in turn: a new one is the next.
        if usize::from(code) == drawn.advances.len() {
            let advance = (self.program.advance(glyph) * 1000.0).round() / 1000.0;
            drawn.advances.push(advance);
        }
        if !text.is_empty() && code != 0 {
            drawn.texts.entry(code).or_insert_with(|| text.to_owned());
        }
        code
    }

    /// The advance of the glyph `code` draws, in thousandths of an em, as the font written
    /// gives it: `.notdef` (code 0) has the default width, 0.
    #[must_use]
    pub fn code_advance(&self, code: u16) -> f64 {
        self.drawn
            .borrow()
            .advances
            .get(usize::from(code))
            .copied()
            .unwrap_or(0.0)
    }

    /// Writes the font at its object: the subset of the glyphs drawn, their widths, the text
    /// they stand for. `false`, and nothing written, when the subsetter cannot cut the program.
    ///
    /// A panic in the subsetter is caught as a program it cannot cut, where panics unwind: in
    /// WebAssembly, where they abort, the module stops.
    ///
    /// # Errors
    /// What [`Document::add_object`] returns.
    pub fn write(&self, doc: &mut Document) -> sealwax_core::Result<bool> {
        let drawn = self.drawn.borrow();
        let program = &self.program;
        let subset = catch_unwind(AssertUnwindSafe(|| {
            subsetter::subset(&program.data, program.face, &drawn.remapper).ok()
        }))
        .ok()
        .flatten();
        let Some(subset) = subset else {
            return Ok(false);
        };
        let originals: Vec<u16> = drawn.remapper.remapped_gids().collect();
        let tag = subset_tag(&originals, &program.name);
        let base_font = format!("{tag}+{}", program.name);

        let (file_key, file, subtype) = match program.outlines {
            Outlines::TrueType => {
                let mut dict = Dict::new();
                dict.insert(
                    "Length1",
                    Object::Integer(i64::try_from(subset.len()).unwrap_or(i64::MAX)),
                );
                ("FontFile2", flate(dict, &subset), "CIDFontType2")
            }
            Outlines::Cff => {
                let cff = FontRef::new(&subset)
                    .ok()
                    .and_then(|font| font.table_data(Tag::new(b"CFF ")))
                    .map(|data| data.as_bytes().to_vec());
                let Some(cff) = cff else {
                    return Ok(false);
                };
                let mut dict = Dict::new();
                dict.insert("Subtype", Name::from("CIDFontType0C"));
                ("FontFile3", flate(dict, &cff), "CIDFontType0")
            }
        };
        let file = doc.add_object(file)?;

        let mut descriptor = Dict::new();
        descriptor.insert("Type", Name::from("FontDescriptor"));
        descriptor.insert("FontName", Name::from(base_font.as_str()));
        // Symbolic: its glyphs are reached by CID, not by a standard encoding.
        descriptor.insert("Flags", Object::Integer(4));
        descriptor.insert(
            "FontBBox",
            Object::Array(
                program
                    .bbox
                    .iter()
                    .map(|v| Object::Real(v.round()))
                    .collect(),
            ),
        );
        descriptor.insert("ItalicAngle", Object::Real(program.italic_angle));
        descriptor.insert("Ascent", Object::Real(program.ascent.round()));
        descriptor.insert("Descent", Object::Real(program.descent.round()));
        descriptor.insert("CapHeight", Object::Real(program.cap_height.round()));
        descriptor.insert("StemV", Object::Integer(80));
        descriptor.insert(file_key, Object::Ref(file));
        let descriptor = doc.add_object(Object::Dict(descriptor))?;

        // One run from code 1: `.notdef` (code 0) keeps the default width, 0.
        let widths: Vec<Object> = drawn
            .advances
            .iter()
            .skip(1)
            .map(|w| Object::Real(*w))
            .collect();
        let mut system = Dict::new();
        system.insert("Registry", Object::String(b"Adobe".to_vec()));
        system.insert("Ordering", Object::String(b"Identity".to_vec()));
        system.insert("Supplement", Object::Integer(0));
        let mut cid_font = Dict::new();
        cid_font.insert("Type", Name::from("Font"));
        cid_font.insert("Subtype", Name::from(subtype));
        cid_font.insert("BaseFont", Name::from(base_font.as_str()));
        cid_font.insert("CIDSystemInfo", Object::Dict(system));
        cid_font.insert("FontDescriptor", Object::Ref(descriptor));
        if program.outlines == Outlines::TrueType {
            cid_font.insert("CIDToGIDMap", Name::from("Identity"));
        }
        cid_font.insert("DW", Object::Integer(0));
        if !widths.is_empty() {
            cid_font.insert(
                "W",
                Object::Array(vec![Object::Integer(1), Object::Array(widths)]),
            );
        }
        let cid_font = doc.add_object(Object::Dict(cid_font))?;

        let to_unicode = doc.add_object(flate(Dict::new(), to_unicode(&drawn.texts).as_bytes()))?;

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
        Ok(true)
    }
}

/// A stream compressed with `FlateDecode`.
pub(crate) fn flate(mut dict: Dict, data: &[u8]) -> Object {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let compressed = encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    if compressed.is_empty() && !data.is_empty() {
        return Object::Stream(Stream {
            dict,
            data: data.to_vec(),
        });
    }
    dict.insert("Filter", Name::from("FlateDecode"));
    Object::Stream(Stream {
        dict,
        data: compressed,
    })
}

/// Six capital letters standing for the subset (ISO 32000-1, 9.6.4), the same for the same
/// glyphs of the same font.
fn subset_tag(glyphs: &[u16], name: &str) -> String {
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

/// The `ToUnicode` CMap of the codes drawn.
fn to_unicode(texts: &BTreeMap<u16, String>) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &String)> = texts.iter().collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (code, text) in chunk {
            let mut units = String::new();
            for unit in text.encode_utf16() {
                let _ = write!(units, "{unit:04X}");
            }
            let _ = writeln!(out, "<{code:04X}> <{units}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}
