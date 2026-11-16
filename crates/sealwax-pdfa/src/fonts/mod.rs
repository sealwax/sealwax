//! Fonts: PDF/A requires the program of every font used for rendering (ISO 19005-3, 6.2.11.4.1),
//! with widths that agree with the font dictionary (6.2.11.5).
//!
//! A simple font that is not embedded gets the program of the standard font that stands in for
//! it, as PDFium already draws it:
//! - the glyph of each code is what the encoding names; a font without an encoding is read in
//!   WinAnsiEncoding, the way PDFium shows it;
//! - the widths of the dictionary are kept, so that no glyph moves, and the program is changed
//!   to match them; a standard font without widths gets Adobe's;
//! - a `ToUnicode` CMap says what each code means, from its glyph name.

mod cff;
mod dingbats;
mod encodings;
pub(crate) mod standard;
mod to_unicode;
mod truetype;
mod type1;
pub(crate) mod unicode;

use std::collections::{BTreeMap, HashMap, HashSet};

use sealwax_core::{Dict, Document, Name, ObjId, Object, Result};

use crate::Report;
use crate::color::flate_stream;
use crate::edit::{self, Location};
use crate::scan::{FontUse, Scan};
use cff::{Blank, Cff};
use standard::{NONSYMBOLIC, SYMBOLIC, Standard, strip_subset};

/// Glyph names that name the same glyph as another, which a font may have instead
/// (ISO 32000-1, annex D, notes on WinAnsiEncoding).
const ALIASES: [(&str, &str); 2] = [("nbspace", "space"), ("sfthyphen", "hyphen")];

/// A standard font's program, parsed, with its glyphs by name and by meaning.
struct Program {
    font: Standard,
    cff: Cff<'static>,
    by_name: HashMap<&'static str, u16>,
    by_text: HashMap<String, u16>,
}

impl Program {
    fn new(font: Standard) -> Option<Self> {
        let cff = Cff::parse(font.program())?;
        let mut by_name = HashMap::new();
        let mut by_text = HashMap::new();
        for gid in 1..u16::try_from(cff.glyph_count()).ok()? {
            if let Some(name) = cff.name(gid) {
                by_name.entry(name).or_insert(gid);
                if let Some(text) = to_unicode::unicode_of(name) {
                    by_text.entry(text).or_insert(gid);
                }
            }
        }
        Some(Self {
            font,
            cff,
            by_name,
            by_text,
        })
    }

    /// The glyph for `name`: by name, by an alias, or by what the name means.
    fn glyph(&self, name: &str) -> Option<u16> {
        if let Some(gid) = self.by_name.get(name) {
            return Some(*gid);
        }
        if let Some((_, alias)) = ALIASES.iter().find(|(from, _)| *from == name)
            && let Some(gid) = self.by_name.get(alias)
        {
            return Some(*gid);
        }
        self.by_text.get(&to_unicode::unicode_of(name)?).copied()
    }

    /// The width a glyph is drawn with when the dictionary gives none: Adobe's metrics.
    fn default_width(&self, gid: u16) -> f64 {
        let name = self.cff.name(gid).unwrap_or_default();
        self.font
            .adobe_width(name)
            .or_else(|| self.cff.width(gid))
            .unwrap_or(0.0)
    }
}

fn base_table(name: &[u8]) -> Option<&'static [&'static str; 256]> {
    match name {
        b"WinAnsiEncoding" => Some(&encodings::WIN_ANSI),
        b"MacRomanEncoding" => Some(&encodings::MAC_ROMAN),
        b"MacExpertEncoding" => Some(&encodings::MAC_EXPERT),
        b"StandardEncoding" => Some(&encodings::STANDARD),
        _ => None,
    }
}

/// `/Differences`: the glyph name of each code it names.
fn differences(array: &[Object]) -> BTreeMap<u8, String> {
    let mut names = BTreeMap::new();
    let mut code: Option<i64> = None;
    for item in array {
        match item {
            Object::Integer(value) => code = Some(*value),
            Object::Name(name) => {
                if let Some(current) = code {
                    if let Ok(byte) = u8::try_from(current) {
                        names.insert(byte, String::from_utf8_lossy(name.as_bytes()).into_owned());
                    }
                    code = Some(current + 1);
                }
            }
            _ => {}
        }
    }
    names
}

/// How the codes of a font are read, the way readers draw them.
struct Encoding {
    /// The glyph name drawn for each code.
    names: Vec<Option<String>>,
    /// Codes no encoding defines. PDFium draws the Latin-1 letter of the code when the font has
    /// it (`à` for 0xE0), else nothing, and moves on by the width of a space: so does the
    /// embedded font (measured 14/11/2026).
    fallback: [bool; 256],
    /// The base encoding written; `None`: the program's own, which is StandardEncoding for the
    /// Latin fonts, what a font without an encoding is read in.
    base: Option<&'static [u8]>,
    /// The names of the base encoding.
    base_names: &'static [&'static str; 256],
    /// The font named no base encoding.
    implicit: bool,
}

impl Encoding {
    /// What a code means, for the `ToUnicode` CMap.
    fn text(&self, code: u8) -> Option<String> {
        if self.fallback[usize::from(code)] {
            // The code itself, as PDFium extracts it; U+0000 is not allowed (6.2.11.7.2).
            return (code != 0).then(|| char::from(code).to_string());
        }
        to_unicode::unicode_of(self.names[usize::from(code)].as_deref()?)
    }
}

fn encoding(doc: &Document, font: &Dict, standard: Standard) -> Result<Encoding> {
    let value = match font.get(b"Encoding") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    let (named, own_differences) = match &value {
        Some(Object::Name(name)) => (Some(name.as_bytes().to_vec()), BTreeMap::new()),
        Some(Object::Dict(dict)) => {
            let base = dict
                .get(b"BaseEncoding")
                .and_then(Object::as_name)
                .map(<[u8]>::to_vec);
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
    let named_table = named.as_deref().and_then(base_table);
    let (base, base_names): (Option<&'static [u8]>, &'static [&'static str; 256]) = if standard
        .symbolic()
    {
        // Their codes are read through their own encoding: viewers ignore a Latin one.
        let table = if standard == Standard::Symbol {
            &encodings::SYMBOL
        } else {
            &encodings::ZAPF_DINGBATS
        };
        (None, table)
    } else {
        match (named.as_deref(), named_table) {
            (Some(b"MacRomanEncoding"), Some(table)) => (Some(&b"MacRomanEncoding"[..]), table),
            (Some(b"MacExpertEncoding"), Some(table)) => (Some(&b"MacExpertEncoding"[..]), table),
            (Some(b"WinAnsiEncoding"), Some(table)) => (Some(&b"WinAnsiEncoding"[..]), table),
            // StandardEncoding, named (which PDF does not allow) or implied.
            _ => (None, &encodings::STANDARD),
        }
    };
    let mut names: Vec<Option<String>> = base_names
        .iter()
        .map(|name| (!name.is_empty()).then(|| (*name).to_owned()))
        .collect();
    for (code, name) in own_differences {
        // `.notdef` names no glyph: the code is read as if undefined.
        names[usize::from(code)] = (name != ".notdef").then_some(name);
    }
    // "In WinAnsiEncoding, all unused codes greater than 40 [octal] map to the bullet
    // character" (ISO 32000-1, annex D, note 5), as PDFium draws them.
    if base == Some(&b"WinAnsiEncoding"[..]) {
        for slot in names.iter_mut().skip(0x21) {
            if slot.is_none() {
                *slot = Some("bullet".to_owned());
            }
        }
    }
    let mut fallback = [false; 256];
    if !standard.symbolic() {
        for code in 0..=255u8 {
            let slot = &mut names[usize::from(code)];
            if slot.is_some() {
                continue;
            }
            fallback[usize::from(code)] = true;
            let mut buffer = [0u8; 64];
            let latin1 = (code >= 0xA1)
                .then(|| read_fonts::ps::agl::char_to_name(u32::from(code), &mut buffer))
                .flatten();
            *slot = Some(latin1.unwrap_or("space").to_owned());
        }
    }
    Ok(Encoding {
        names,
        fallback,
        base,
        base_names,
        implicit: named_table.is_none() && !standard.symbolic(),
    })
}

/// What a font descriptor says about the font it describes.
fn descriptor_hints(descriptor: Option<&Dict>) -> (Option<i64>, Option<f64>, Option<f64>) {
    let Some(descriptor) = descriptor else {
        return (None, None, None);
    };
    (
        descriptor.get(b"Flags").and_then(Object::as_i64),
        descriptor.get(b"FontWeight").and_then(Object::as_f64),
        descriptor.get(b"ItalicAngle").and_then(Object::as_f64),
    )
}

fn embedded(descriptor: Option<&Dict>) -> bool {
    descriptor.is_some_and(|d| {
        [&b"FontFile"[..], b"FontFile2", b"FontFile3"]
            .iter()
            .any(|key| d.contains_key(key))
    })
}

/// The widths the dictionary gives, by code, when it gives them consistently.
fn dictionary_widths(doc: &Document, font: &Dict) -> Result<Option<(u8, Vec<f64>)>> {
    let first = font.get(b"FirstChar").and_then(Object::as_i64);
    let widths = match font.get(b"Widths") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    let (Some(first), Some(Object::Array(widths))) = (first, widths) else {
        return Ok(None);
    };
    let Ok(first) = u8::try_from(first) else {
        return Ok(None);
    };
    let mut values = Vec::with_capacity(widths.len());
    for width in &widths {
        values.push(
            edit::resolved(doc, width)?
                .as_ref()
                .and_then(Object::as_f64)
                .unwrap_or(0.0),
        );
    }
    if values.is_empty() || usize::from(first) + values.len() > 256 {
        return Ok(None);
    }
    Ok(Some((first, values)))
}

/// Codes and their widths.
type CodeWidths = Vec<(u8, f64)>;

/// Programs written so far, by font and widths changed: identical ones are shared.
type Written = HashMap<(Standard, Vec<(u16, u64)>), ObjId>;

/// Embed the programs of the simple fonts used for rendering that have none.
pub(crate) fn apply(doc: &mut Document, scan: &Scan, report: &mut Report) -> Result<()> {
    // Which fonts lack a program is decided before any is embedded: fonts can share a
    // descriptor, which the first one embedded would make look embedded for the others.
    let mut pending = Vec::new();
    for (location, used) in &scan.fonts {
        if !used.rendered || used.composite {
            continue;
        }
        let Some(font) = edit::read_dict(doc, location)? else {
            continue;
        };
        let subtype = font
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or_default();
        if !matches!(subtype, b"Type1" | b"TrueType" | b"MMType1") {
            continue;
        }
        let descriptor = if font.contains_key(b"FontDescriptor") {
            edit::read_dict(doc, &location.key(b"FontDescriptor"))?
        } else {
            None
        };
        if !embedded(descriptor.as_ref()) {
            pending.push((location, used));
        }
    }
    let mut aligned: HashSet<ObjId> = HashSet::new();
    for (location, used) in &scan.fonts {
        if !used.rendered {
            continue;
        }
        if used.composite {
            align_composite_widths(doc, location, used, &mut aligned, report)?;
        } else {
            align_simple_widths(doc, location, used, &mut aligned, report)?;
        }
    }
    let mut programs: HashMap<Standard, Option<Program>> = HashMap::new();
    let mut written: Written = HashMap::new();
    let mut descriptors: HashSet<ObjId> = HashSet::new();
    for (location, used) in pending {
        embed(
            doc,
            location,
            used,
            &mut programs,
            &mut written,
            &mut descriptors,
            report,
        )?;
    }
    Ok(())
}

/// The widths `/W` gives, by CID.
fn cid_widths(doc: &Document, value: &Object) -> Result<BTreeMap<u16, f64>> {
    let mut widths = BTreeMap::new();
    let Some(Object::Array(items)) = edit::resolved(doc, value)? else {
        return Ok(widths);
    };
    let number = |object: &Object| -> Result<Option<f64>> {
        Ok(edit::resolved(doc, object)?
            .as_ref()
            .and_then(Object::as_f64))
    };
    let mut at = 0;
    while at + 1 < items.len() {
        let Some(first) = number(&items[at])? else {
            break;
        };
        match edit::resolved(doc, &items[at + 1])? {
            Some(Object::Array(list)) => {
                for (offset, width) in list.iter().enumerate() {
                    let cid = first + f64::from(u32::try_from(offset).unwrap_or(u32::MAX));
                    if let (Some(width), Ok(cid)) = (number(width)?, cid_of(cid)) {
                        widths.insert(cid, width);
                    }
                }
                at += 2;
            }
            Some(last) => {
                let (Some(last), Some(width)) = (last.as_f64(), items.get(at + 2)) else {
                    break;
                };
                let Some(width) = number(width)? else {
                    break;
                };
                let (Ok(first), Ok(last)) = (cid_of(first), cid_of(last)) else {
                    break;
                };
                for cid in first..=last {
                    widths.insert(cid, width);
                }
                at += 3;
            }
            None => break,
        }
    }
    Ok(widths)
}

fn cid_of(value: f64) -> std::result::Result<u16, ()> {
    if value.fract() != 0.0 || !(0.0..=f64::from(u16::MAX)).contains(&value) {
        return Err(());
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as u16)
}

/// A composite font with a TrueType program: the program's advance widths are made those of
/// `/W` and `/DW`, which the text is laid out with (6.2.11.5).
fn align_composite_widths(
    doc: &mut Document,
    location: &Location,
    used: &FontUse,
    aligned: &mut HashSet<ObjId>,
    report: &mut Report,
) -> Result<()> {
    let Some(font) = edit::read_dict(doc, location)? else {
        return Ok(());
    };
    let descendants = match font.get(b"DescendantFonts") {
        Some(Object::Ref(id)) => Location::of(*id),
        Some(_) => location.key(b"DescendantFonts"),
        None => return Ok(()),
    };
    let cid_location = descendants.index(0);
    let Some(cid_font) = edit::read_dict(doc, &cid_location)? else {
        return Ok(());
    };
    if cid_font.get(b"Subtype").and_then(Object::as_name) != Some(b"CIDFontType2") {
        return Ok(());
    }
    let Some(descriptor) = edit::read_dict(doc, &cid_location.key(b"FontDescriptor"))? else {
        return Ok(());
    };
    let Some(file) = descriptor.get(b"FontFile2").and_then(Object::as_ref_id) else {
        return Ok(());
    };
    // A program whose data is damaged is left as it is (see `edit::stream_data`).
    let Some(data) = edit::stream_data(doc, file, true) else {
        report.residual_add("program_unreadable", 1);
        return Ok(());
    };
    let Some(program) = truetype::TrueType::parse(&data) else {
        report.residual_add("program_unreadable", 1);
        return Ok(());
    };
    let map = match cid_font.get(b"CIDToGIDMap") {
        Some(Object::Ref(id)) => edit::decoded(doc, *id),
        _ => None,
    };
    let gid_of = |cid: u16| -> Option<u16> {
        match &map {
            None => Some(cid),
            Some(bytes) => {
                let at = 2 * usize::from(cid);
                Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
            }
        }
    };
    let widths = match cid_font.get(b"W") {
        Some(value) => cid_widths(doc, value)?,
        None => BTreeMap::new(),
    };
    let default = cid_font
        .get(b"DW")
        .and_then(Object::as_f64)
        .unwrap_or(1000.0);
    // The CIDs shown first, then those /W names: a glyph two CIDs share gets the width of
    // the first.
    let shown = used
        .cids
        .iter()
        .map(|cid| (*cid, widths.get(cid).copied().unwrap_or(default)));
    let mut decided: BTreeMap<u16, f64> = BTreeMap::new();
    for (cid, width) in shown.chain(widths.iter().map(|(c, w)| (*c, *w))) {
        let Some(gid) = gid_of(cid).filter(|g| usize::from(*g) < program.glyph_count()) else {
            continue;
        };
        match decided.get(&gid) {
            Some(already) if (already - width).abs() > 0.5 => {
                report.residual_add("width_conflict", usize::from(used.cids.contains(&cid)));
            }
            Some(_) => {}
            None => {
                decided.insert(gid, width);
            }
        }
    }
    let patches: Vec<(u16, f64)> = decided
        .into_iter()
        .filter(|(gid, width)| {
            program
                .width(*gid)
                .is_none_or(|own| (own - width).abs() > 0.01)
        })
        .collect();
    if patches.is_empty() {
        return Ok(());
    }
    if !aligned.insert(file) {
        // A program another font of this document already had its widths changed in.
        report.residual_add("width_not_changed", 1);
        return Ok(());
    }
    let Some(bytes) = program.with_widths(&patches) else {
        report.residual_add("width_not_changed", 1);
        return Ok(());
    };
    let Some(Object::Stream(stream)) = doc.object_without_data(file)? else {
        return Ok(());
    };
    let mut dict = stream.dict;
    for key in [&b"Filter"[..], b"DecodeParms", b"DP", b"Length"] {
        dict.remove(key);
    }
    dict.insert("Length1", i64::try_from(bytes.len()).unwrap_or(i64::MAX));
    doc.set_object(file, Object::Stream(flate_stream(dict, &bytes)));
    report.change("program_widths_aligned", 1);
    Ok(())
}

/// The program of a simple font, as read to align its widths.
enum Simple<'a> {
    Cff(Box<Cff<'a>>),
    Type1(Box<type1::Type1>),
}

impl Simple<'_> {
    fn glyph(&self, name: &str) -> Option<u16> {
        match self {
            Self::Cff(program) => program.glyph(name),
            Self::Type1(program) => program.glyph(name),
        }
    }

    fn built_in(&self, code: u8) -> Option<u16> {
        match self {
            Self::Cff(program) => program.built_in(code),
            Self::Type1(program) => program.built_in(code),
        }
    }

    fn width(&self, gid: u16) -> Option<f64> {
        match self {
            Self::Cff(program) => program.width(gid),
            Self::Type1(program) => program.width(gid),
        }
    }

    /// Whether `.notdef` draws nothing: the first glyph of a CFF program, the one so named in
    /// a Type 1 program.
    fn notdef_draws_nothing(&self) -> bool {
        match self {
            Self::Cff(program) => program.draws_nothing(0),
            Self::Type1(program) => program
                .glyph(".notdef")
                .is_some_and(|gid| program.draws_nothing(gid)),
        }
    }
}

/// Make the widths of the program of a simple font, CFF (`FontFile3`, `Type1C`) or Type 1
/// (`FontFile`), those of the font dictionary for every code shown (6.2.11.5), and give the codes
/// whose glyph the program lacks a blank one when its `.notdef` draws nothing (6.2.11.4.1).
#[allow(clippy::too_many_lines)]
fn align_simple_widths(
    doc: &mut Document,
    location: &Location,
    used: &FontUse,
    aligned: &mut HashSet<ObjId>,
    report: &mut Report,
) -> Result<()> {
    let Some(font) = edit::read_dict(doc, location)? else {
        return Ok(());
    };
    if font.get(b"Subtype").and_then(Object::as_name) != Some(b"Type1") {
        return Ok(());
    }
    let Some(descriptor) = edit::read_dict(doc, &location.key(b"FontDescriptor"))? else {
        return Ok(());
    };
    let (file, type1) = match (
        descriptor.get(b"FontFile3").and_then(Object::as_ref_id),
        descriptor.get(b"FontFile").and_then(Object::as_ref_id),
    ) {
        (Some(file), _) => (file, false),
        (None, Some(file)) => (file, true),
        (None, None) => return Ok(()),
    };
    let Some(Object::Stream(stream)) = doc.object_without_data(file)? else {
        return Ok(());
    };
    if !type1 && stream.dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Type1C") {
        return Ok(());
    }
    // A program whose data is damaged is left as it is (see `edit::stream_data`).
    let Some(data) = edit::stream_data(doc, file, true) else {
        report.residual_add("program_unreadable", 1);
        return Ok(());
    };
    let program = if type1 {
        let length = |key: &[u8]| {
            stream
                .dict
                .get(key)
                .and_then(Object::as_i64)
                .and_then(|n| usize::try_from(n).ok())
        };
        let lengths = [length(b"Length1"), length(b"Length2"), length(b"Length3")];
        match type1::Type1::parse(&data, lengths) {
            Some(program) => Simple::Type1(Box::new(program)),
            None => {
                report.residual_add("program_unreadable", 1);
                return Ok(());
            }
        }
    } else {
        match Cff::parse(&data) {
            Some(program) => Simple::Cff(Box::new(program)),
            None => return Ok(()),
        }
    };
    let missing = descriptor
        .get(b"MissingWidth")
        .and_then(Object::as_f64)
        .unwrap_or(0.0);
    // The glyph of a code: through the font's encoding, else the program's own.
    let value = match font.get(b"Encoding") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    let (base, own): (Option<&[&str; 256]>, BTreeMap<u8, String>) = match &value {
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
    // The name the font's encoding gives a code.
    let name = |code: u8| -> Option<&str> {
        own.get(&code).map(String::as_str).or_else(|| {
            base.map(|table| table[usize::from(code)])
                .filter(|n| !n.is_empty())
        })
    };
    let glyph = |code: u8| -> Option<u16> {
        match name(code) {
            Some(name) => program.glyph(name),
            None => program.built_in(code),
        }
    };
    let Some((first, widths)) = dictionary_widths(doc, &font)? else {
        // A font without widths (6.2.11.2): readers take each from the program, `.notdef`'s for a
        // code without a glyph. The dictionary is given those, for every code.
        let notdef = program.glyph(".notdef").or(match program {
            Simple::Cff(_) => Some(0),
            Simple::Type1(_) => None,
        });
        let fallback = notdef.and_then(|gid| program.width(gid)).unwrap_or(missing);
        let table: Vec<Object> = (0..=255u8)
            .map(|code| {
                let width = glyph(code)
                    .and_then(|gid| program.width(gid))
                    .unwrap_or(fallback);
                Object::Real((width * 1000.0).round() / 1000.0)
            })
            .collect();
        let added = edit::edit_dict(doc, location, false, |dict| {
            dict.insert("FirstChar", 0);
            dict.insert("LastChar", 255);
            dict.insert("Widths", Object::Array(table));
        })?;
        report.change("widths_added", usize::from(added));
        return Ok(());
    };
    let width = |code: u8| -> f64 {
        usize::from(code)
            .checked_sub(usize::from(first))
            .and_then(|index| widths.get(index))
            .copied()
            .unwrap_or(missing)
    };
    let mut decided: BTreeMap<u16, f64> = BTreeMap::new();
    let mut blanks: Vec<Blank<'_>> = Vec::new();
    for code in used.codes.iter().copied() {
        let Some(gid) = glyph(code) else {
            // A code the producer's subset has no glyph for (6.2.11.4.1): readers draw
            // `.notdef`. Where it draws nothing, the code gets a blank glyph of its name (in
            // the built-in encoding, that of Standard Encoding, the space's most often).
            let blank = match name(code) {
                Some(name) => Some((name, None)),
                None => Some(encodings::STANDARD[usize::from(code)])
                    .filter(|n| !n.is_empty())
                    .map(|name| (name, Some(code))),
            };
            match blank {
                Some((name, code_in_program))
                    if program.notdef_draws_nothing()
                        && program.glyph(name).is_none()
                        && blanks.iter().all(|b| b.name != name) =>
                {
                    blanks.push(Blank {
                        name,
                        width: width(code),
                        code: code_in_program,
                    });
                }
                _ => report.residual_add("glyph_missing", 1),
            }
            continue;
        };
        let target = width(code);
        match decided.get(&gid) {
            Some(already) if (already - target).abs() > 0.5 => {
                report.residual_add("width_conflict", 1);
            }
            Some(_) => {}
            None => {
                decided.insert(gid, target);
            }
        }
    }
    let patches: Vec<(u16, f64)> = decided
        .into_iter()
        .filter(|(gid, target)| {
            program
                .width(*gid)
                .is_none_or(|own| (own - target).abs() > 0.01)
        })
        .collect();
    let rewrite = matches!(&program, Simple::Type1(program) if program.needs_rewriting());
    if patches.is_empty() && blanks.is_empty() && !rewrite {
        return Ok(());
    }
    // A program shared by fonts is changed once, for the first.
    let edited = match &program {
        Simple::Cff(cff) => cff.edited(&patches, &blanks).map(|bytes| (bytes, None)),
        Simple::Type1(type1) => type1
            .edited(&patches, &blanks)
            .map(|written| (written.bytes, Some(written.lengths))),
    };
    let (bytes, lengths) = match edited {
        Some(edited) if aligned.insert(file) => edited,
        Some(_) if patches.is_empty() && blanks.is_empty() => return Ok(()),
        _ => {
            if !patches.is_empty() {
                report.residual_add("width_not_changed", 1);
            }
            report.residual_add("glyph_missing", blanks.len());
            return Ok(());
        }
    };
    let mut dict = stream.dict;
    for key in [&b"Filter"[..], b"DecodeParms", b"DP", b"Length"] {
        dict.remove(key);
    }
    if let Some(lengths) = lengths {
        for (key, length) in [&b"Length1"[..], b"Length2", b"Length3"]
            .iter()
            .zip(lengths)
        {
            dict.insert(*key, i64::try_from(length).unwrap_or(i64::MAX));
        }
    }
    doc.set_object(file, Object::Stream(flate_stream(dict, &bytes)));
    if rewrite {
        report.change("program_rewritten", 1);
    }
    if !patches.is_empty() {
        report.change("program_widths_aligned", 1);
    }
    report.change("glyph_blank_added", blanks.len());
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn embed(
    doc: &mut Document,
    location: &Location,
    used: &FontUse,
    programs: &mut HashMap<Standard, Option<Program>>,
    written: &mut Written,
    descriptors: &mut HashSet<ObjId>,
    report: &mut Report,
) -> Result<()> {
    let Some(font) = edit::read_dict(doc, location)? else {
        return Ok(());
    };
    let descriptor_location = location.key(b"FontDescriptor");
    let descriptor = if font.contains_key(b"FontDescriptor") {
        edit::read_dict(doc, &descriptor_location)?
    } else {
        None
    };
    // A descriptor another font of this document already got a program in: this one gets its
    // own copy, since its widths, hence its program, can differ.
    let shared = font
        .get(b"FontDescriptor")
        .and_then(Object::as_ref_id)
        .is_some_and(|id| !descriptors.insert(id));
    let base_font = font
        .get(b"BaseFont")
        .and_then(Object::as_name)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_default();
    let (flags, weight, angle) = descriptor_hints(descriptor.as_ref());
    let Some(standard) = Standard::substitute(&base_font, flags, weight, angle) else {
        report.residual_add("font_without_substitute", 1);
        return Ok(());
    };
    let Some(program) = programs
        .entry(standard)
        .or_insert_with(|| Program::new(standard))
        .as_ref()
    else {
        report.residual_add("font_without_substitute", 1);
        return Ok(());
    };

    let encoding = encoding(doc, &font, standard)?;
    // The glyph of each code, and the glyph names the written encoding must change.
    let mut glyphs: [Option<u16>; 256] = [None; 256];
    // Codes drawn blank, at the width of a space, as PDFium draws a code without a glyph.
    let mut blank = encoding.fallback;
    let space = program.glyph("space");
    let mut changed_names: Vec<(u8, &str)> = Vec::new();
    let mut missing = 0;
    for code in 0..=255u8 {
        let Some(name) = &encoding.names[usize::from(code)] else {
            continue;
        };
        let gid = match program.glyph(name) {
            Some(gid) => gid,
            // A glyph the substitute does not have: blank, which is what a reader without the
            // original font shows, but the character is lost; the report says so.
            None if !standard.symbolic() => {
                missing += usize::from(used.codes.contains(&code));
                blank[usize::from(code)] = true;
                let Some(space) = space else {
                    continue;
                };
                space
            }
            None => {
                missing += usize::from(used.codes.contains(&code));
                continue;
            }
        };
        glyphs[usize::from(code)] = Some(gid);
        let actual = program.cff.name(gid).unwrap_or_default();
        // A symbolic font's codes are read through the program's built-in encoding.
        let read = if encoding.base.is_none() {
            program
                .cff
                .built_in(code)
                .and_then(|g| program.cff.name(g))
                .unwrap_or_default()
        } else {
            encoding.base_names[usize::from(code)]
        };
        if read != actual {
            changed_names.push((code, actual));
        }
    }
    report.residual_add("glyph_missing", missing);

    // Widths: the dictionary's, else Adobe's metrics.
    let (first, widths) = match dictionary_widths(doc, &font)? {
        Some(found) => (found.0, found.1),
        None => {
            let defined: Vec<u8> = (0..=255u8)
                .filter(|code| glyphs[usize::from(*code)].is_some())
                .collect();
            let (Some(first), Some(last)) = (defined.first(), defined.last()) else {
                report.residual_add("font_without_glyphs", 1);
                return Ok(());
            };
            if !standard::metric_compatible(&base_font) {
                // A font none of the standard ones has the metrics of, without widths: its text
                // is laid out with the substitute's, as each reader does with its own.
                report.residual_add("layout_from_substitute_metrics", 1);
            }
            let space = space.map_or(0.0, |gid| program.default_width(gid));
            let widths = (*first..=*last)
                .map(|code| {
                    if blank[usize::from(code)] {
                        space
                    } else {
                        glyphs[usize::from(code)].map_or(0.0, |gid| program.default_width(gid))
                    }
                })
                .collect();
            (*first, widths)
        }
    };
    // The width each glyph gets: that of its first code, the codes the content shows first.
    let mut decided: BTreeMap<u16, f64> = BTreeMap::new();
    let in_range: Vec<(u8, f64)> = widths
        .iter()
        .enumerate()
        .filter_map(|(index, width)| Some((u8::try_from(usize::from(first) + index).ok()?, *width)))
        .collect();
    let (shown, others): (CodeWidths, CodeWidths) = in_range
        .into_iter()
        .partition(|(code, _)| used.codes.contains(code));
    for (code, width) in shown.into_iter().chain(others) {
        let Some(gid) = glyphs[usize::from(code)] else {
            continue;
        };
        match decided.get(&gid) {
            Some(already) if (already - width).abs() > 0.5 => {
                report.residual_add("width_conflict", usize::from(used.codes.contains(&code)));
            }
            Some(_) => {}
            None => {
                decided.insert(gid, width);
            }
        }
    }
    let patches: BTreeMap<u16, f64> = decided
        .into_iter()
        .filter(|(gid, width)| {
            program
                .cff
                .width(*gid)
                .is_none_or(|own| (own - width).abs() > 0.001)
        })
        .collect();
    let outside = used
        .codes
        .iter()
        .filter(|code| {
            let code = usize::from(**code);
            glyphs[code].is_some()
                && (code < usize::from(first) || code >= usize::from(first) + widths.len())
        })
        .count();
    report.residual_add("code_outside_widths", outside);

    let key: Vec<(u16, u64)> = patches.iter().map(|(g, w)| (*g, w.to_bits())).collect();
    let file = match written.get(&(standard, key.clone())) {
        Some(id) => *id,
        None => {
            let patch: Vec<(u16, f64)> = patches.iter().map(|(g, w)| (*g, *w)).collect();
            let bytes = if patch.is_empty() {
                Some(standard.program().to_vec())
            } else {
                program.cff.with_widths(&patch)
            };
            let Some(bytes) = bytes else {
                report.residual_add("width_not_changed", 1);
                return Ok(());
            };
            let mut dict = Dict::new();
            dict.insert("Subtype", Name::from("Type1C"));
            let id = doc.add_object(Object::Stream(flate_stream(dict, &bytes)))?;
            written.insert((standard, key), id);
            id
        }
    };

    // The descriptor: the existing one completed, or a new one.
    let font_name = Name::new(strip_subset(&base_font).as_bytes().to_vec());
    let metrics = standard.metrics();
    let bbox = program.cff.bbox();
    let italic_angle = program.cff.italic_angle();
    let complete = |d: &mut Dict| {
        d.insert("Type", Name::from("FontDescriptor"));
        if !d.contains_key(b"FontName") {
            d.insert("FontName", font_name.clone());
        }
        let own = d.get(b"Flags").and_then(Object::as_i64).unwrap_or(0);
        d.insert(
            "Flags",
            (own & !(SYMBOLIC | NONSYMBOLIC)) | standard.flags(),
        );
        if !d.contains_key(b"FontBBox") {
            d.insert(
                "FontBBox",
                bbox.iter().map(|v| Object::Real(*v)).collect::<Vec<_>>(),
            );
        }
        if !d.contains_key(b"ItalicAngle") {
            d.insert("ItalicAngle", italic_angle);
        }
        for (key, value) in [
            ("Ascent", metrics.ascent),
            ("Descent", metrics.descent),
            ("CapHeight", metrics.cap_height),
            ("StemV", metrics.stem_v),
        ] {
            if !d.contains_key(key.as_bytes()) {
                d.insert(key, value);
            }
        }
        d.remove(b"FontFile");
        d.remove(b"FontFile2");
        d.insert("FontFile3", file);
    };
    let new_descriptor = match descriptor {
        Some(_) if !shared => {
            edit::edit_dict(doc, &descriptor_location, false, complete)?;
            None
        }
        existing => {
            let mut d = existing.unwrap_or_default();
            complete(&mut d);
            Some(doc.add_object(Object::Dict(d))?)
        }
    };

    // What each code means, unless the font says it already.
    let to_unicode = if font.contains_key(b"ToUnicode") {
        None
    } else {
        let mappings: Vec<(u32, String)> = (0..=255u8)
            .filter(|code| glyphs[usize::from(*code)].is_some())
            .filter_map(|code| Some((u32::from(code), encoding.text(code)?)))
            .collect();
        if mappings.is_empty() {
            None
        } else {
            Some(doc.add_object(Object::Stream(flate_stream(
                Dict::new(),
                &to_unicode::cmap(1, &mappings),
            )))?)
        }
    };

    let written_encoding: Option<Object> = match (encoding.base, changed_names.is_empty()) {
        (Some(base), true) => Some(Name::new(base.to_vec()).into()),
        (None, true) => None,
        (base, false) => {
            let mut list: Vec<Object> = Vec::new();
            let mut previous: Option<u8> = None;
            for (code, name) in &changed_names {
                if previous.is_none_or(|p| p.checked_add(1) != Some(*code)) {
                    list.push(i64::from(*code).into());
                }
                list.push(Name::new(name.as_bytes().to_vec()).into());
                previous = Some(*code);
            }
            let mut dict = Dict::new();
            dict.insert("Type", Name::from("Encoding"));
            if let Some(base) = base {
                dict.insert("BaseEncoding", Name::new(base.to_vec()));
            }
            dict.insert("Differences", list);
            Some(dict.into())
        }
    };
    let last = i64::from(first) + i64::try_from(widths.len()).unwrap_or(0) - 1;
    edit::edit_dict(doc, location, false, |dict| {
        dict.insert("Subtype", Name::from("Type1"));
        dict.insert("FirstChar", i64::from(first));
        dict.insert("LastChar", last);
        dict.insert(
            "Widths",
            widths.iter().map(|w| Object::Real(*w)).collect::<Vec<_>>(),
        );
        match &written_encoding {
            Some(value) => {
                dict.insert("Encoding", value.clone());
            }
            None => {
                dict.remove(b"Encoding");
            }
        }
        if let Some(id) = new_descriptor {
            dict.insert("FontDescriptor", id);
        }
        if let Some(id) = to_unicode {
            dict.insert("ToUnicode", id);
        }
    })?;
    report.change("font_embedded", 1);
    report.change("font_encoding_implicit", usize::from(encoding.implicit));
    report.change(
        "glyph_from_latin1",
        used.codes
            .iter()
            .filter(|code| encoding.fallback[usize::from(**code)])
            .count(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::io::Read as _;

    use sealwax_core::{Document, Object, OpenOptions, SaveOptions};

    use super::cff::Cff;
    use crate::{Level, Options, Report, convert};

    /// A one-page PDF showing `content` with font `/F1` = object 4 (`font`).
    fn pdf(content: &[u8], font: &str) -> Vec<u8> {
        pdf_with(content, font, None)
    }

    /// As [`pdf`], with a font program as object 6.
    fn pdf_with(content: &[u8], font: &str, program: Option<(&str, &[u8])>) -> Vec<u8> {
        let content_object = [
            format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
            content.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat();
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] \
               /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
                .to_vec(),
            font.as_bytes().to_vec(),
            content_object,
        ];
        if let Some((dict, data)) = program {
            objects.push(
                [
                    format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes(),
                    data.to_vec(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            );
        }
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

    struct Converted {
        doc: Document,
        report: Report,
    }

    impl Converted {
        fn new(data: Vec<u8>) -> Self {
            let mut doc = Document::open(data, &OpenOptions::default()).unwrap();
            let report = convert(&mut doc, &Options::new(Level::U)).unwrap();
            let saved = doc.save(&SaveOptions::default()).unwrap();
            Self {
                doc: Document::open(saved, &OpenOptions::default()).unwrap(),
                report,
            }
        }

        fn get(&self, object: &Object) -> Object {
            match object {
                Object::Ref(id) => self.doc.object(*id).unwrap().unwrap(),
                other => other.clone(),
            }
        }

        fn font(&self) -> sealwax_core::Dict {
            let page = self
                .doc
                .object(self.doc.page_ids()[0].unwrap())
                .unwrap()
                .unwrap();
            let resources = self.get(page.as_dict().unwrap().get(b"Resources").unwrap());
            let fonts = self.get(resources.as_dict().unwrap().get(b"Font").unwrap());
            let font = self.get(fonts.as_dict().unwrap().get(b"F1").unwrap());
            font.as_dict().unwrap().clone()
        }

        fn stream_data(&self, value: &Object) -> Vec<u8> {
            let Object::Stream(stream) = self.get(value) else {
                panic!("not a stream");
            };
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(stream.data.as_slice())
                .read_to_end(&mut out)
                .unwrap();
            out
        }

        /// What the font's `ToUnicode` CMap says each code means.
        fn to_unicode(&self, font: &sealwax_core::Dict) -> impl Fn(u32) -> Option<String> {
            let data = self.stream_data(font.get(b"ToUnicode").unwrap());
            let cmap = hayro_cmap::CMap::parse(&data, |_| None).unwrap();
            move |code| {
                cmap.lookup_bf_string(code).map(|value| match value {
                    hayro_cmap::BfString::Char(c) => c.to_string(),
                    hayro_cmap::BfString::String(s) => s,
                })
            }
        }

        fn program(&self) -> Vec<u8> {
            let font = self.font();
            let descriptor = self.get(font.get(b"FontDescriptor").unwrap());
            let file = descriptor
                .as_dict()
                .unwrap()
                .get(b"FontFile3")
                .unwrap()
                .clone();
            let Object::Stream(stream) = self.get(&file) else {
                panic!("no program");
            };
            assert_eq!(
                stream.dict.get(b"Subtype").and_then(Object::as_name),
                Some(&b"Type1C"[..])
            );
            self.stream_data(&file)
        }

        fn widths(&self) -> (i64, Vec<f64>) {
            let font = self.font();
            let first = font.get(b"FirstChar").and_then(Object::as_i64).unwrap();
            let widths = self.get(font.get(b"Widths").unwrap());
            (
                first,
                widths
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| w.as_f64().unwrap())
                    .collect(),
            )
        }
    }

    #[test]
    fn helvetica_without_encoding_is_read_as_pdfium_reads_it() {
        // Without an encoding, a code is read in StandardEncoding: 0x27 is "’", 0xAB "«" (556
        // wide for Adobe, 612 in the Foxit program). StandardEncoding leaves 0xE0 undefined:
        // PDFium draws "à", moving on by the width of a space.
        let converted = Converted::new(pdf(
            b"BT /F1 12 Tf 10 10 Td (A'\xE0\xAB) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        ));
        let font = converted.font();
        // The program's own encoding (StandardEncoding), with the codes it leaves undefined.
        let encoding = converted.get(font.get(b"Encoding").unwrap());
        let encoding = encoding.as_dict().unwrap();
        assert!(encoding.get(b"BaseEncoding").is_none());
        let differences = super::differences(
            encoding
                .get(b"Differences")
                .and_then(Object::as_array)
                .unwrap(),
        );
        assert_eq!(differences.get(&0xE0).map(String::as_str), Some("agrave"));
        assert!(!differences.contains_key(&0x27));
        let (first, widths) = converted.widths();
        let width = |code: u8| widths[usize::from(code) - usize::try_from(first).unwrap()];
        assert_eq!(
            (width(b'A'), width(b'\''), width(0xE0), width(0xAB)),
            (667.0, 222.0, 278.0, 556.0)
        );
        let program = converted.program();
        let cff = Cff::parse(&program).unwrap();
        assert_eq!(cff.width(cff.glyph("guillemotleft").unwrap()), Some(556.0));
        assert_eq!(cff.width(cff.glyph("agrave").unwrap()), Some(278.0));
        let to_unicode = converted.to_unicode(&font);
        for (code, text) in [(0x41, "A"), (0x27, "\u{2019}"), (0xE0, "à"), (0xAB, "«")] {
            assert_eq!(to_unicode(code).as_deref(), Some(text), "{code:02X}");
        }
        assert_eq!(
            converted.report.changes().get("font_encoding_implicit"),
            Some(&1)
        );
        assert_eq!(
            converted.report.changes().get("glyph_from_latin1"),
            Some(&1)
        );
        assert!(
            converted.report.residual().is_empty(),
            "{:?}",
            converted.report.residual()
        );
    }

    #[test]
    fn a_true_type_font_keeps_its_widths_and_the_program_follows() {
        // ArialMT, not embedded: Helvetica's program, with Arial's widths as the dictionary
        // gives them (here 600 for "A" to show the program follows).
        let converted = Converted::new(pdf(
            b"BT /F1 12 Tf 10 10 Td (AB) Tj ET",
            "<< /Type /Font /Subtype /TrueType /BaseFont /ArialMT /Encoding /WinAnsiEncoding \
             /FirstChar 65 /LastChar 66 /Widths [600 667] /FontDescriptor << /Type /FontDescriptor \
             /FontName /ArialMT /Flags 32 /FontBBox [-665 -325 2000 1040] /ItalicAngle 0 \
             /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 >> >>",
        ));
        let font = converted.font();
        assert_eq!(
            font.get(b"Subtype").and_then(Object::as_name),
            Some(&b"Type1"[..])
        );
        assert_eq!(converted.widths(), (65, vec![600.0, 667.0]));
        let program = converted.program();
        let cff = Cff::parse(&program).unwrap();
        assert_eq!(cff.width(cff.glyph("A").unwrap()), Some(600.0));
        assert_eq!(cff.width(cff.glyph("B").unwrap()), Some(667.0));
        // The descriptor keeps Arial's metrics.
        let descriptor = converted.get(font.get(b"FontDescriptor").unwrap());
        assert_eq!(
            descriptor
                .as_dict()
                .unwrap()
                .get(b"Ascent")
                .and_then(Object::as_f64),
            Some(905.0)
        );
    }

    #[test]
    fn a_glyph_two_codes_share_gets_the_width_of_the_code_shown() {
        // 0xA0 (no-break space) is drawn with the space glyph; the producer gave it 0, the
        // space 278. The content shows the space: its width wins, whatever the order.
        let mut widths = vec!["0"; 0xA0 - 0x20 + 1];
        widths[0] = "278";
        let converted = Converted::new(pdf(
            b"BT /F1 12 Tf 10 10 Td (a b) Tj ET",
            &format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /ArialMT /Encoding /WinAnsiEncoding \
                 /FirstChar 32 /LastChar 160 /Widths [{}] >>",
                widths.join(" ")
            ),
        ));
        let program = converted.program();
        let cff = Cff::parse(&program).unwrap();
        assert_eq!(cff.width(cff.glyph("space").unwrap()), Some(278.0));
    }

    #[test]
    fn zapf_dingbats_codes_are_spelled_out() {
        // "4" is the check mark of form fields. Foxit's program declares StandardEncoding,
        // which has no dingbat: the dictionary names every glyph.
        let converted = Converted::new(pdf(
            b"BT /F1 12 Tf 10 10 Td (4) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>",
        ));
        let font = converted.font();
        let encoding = converted.get(font.get(b"Encoding").unwrap());
        let encoding = encoding.as_dict().unwrap();
        assert!(encoding.get(b"BaseEncoding").is_none());
        let differences = super::differences(
            encoding
                .get(b"Differences")
                .and_then(Object::as_array)
                .unwrap(),
        );
        assert_eq!(differences.get(&b'4').map(String::as_str), Some("a20"));
        assert_eq!(
            converted.to_unicode(&font)(0x34).as_deref(),
            Some("\u{2714}")
        );
        let descriptor = converted.get(font.get(b"FontDescriptor").unwrap());
        let flags = descriptor
            .as_dict()
            .unwrap()
            .get(b"Flags")
            .and_then(Object::as_i64)
            .unwrap();
        assert_eq!(flags & 4, 4, "symbolic");
    }

    #[test]
    fn a_code_named_notdef_moves_on_by_a_space() {
        // pdf.js's issue4304.pdf: /Differences sends the space to /.notdef. PDFium draws
        // nothing and moves on by the width of a space; so does the embedded font.
        let converted = Converted::new(pdf(
            b"BT /F1 12 Tf 10 10 Td (a b) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman \
             /Encoding << /Differences [32 /.notdef] >> >>",
        ));
        let (first, widths) = converted.widths();
        assert_eq!(
            widths[usize::from(b' ') - usize::try_from(first).unwrap()],
            250.0
        );
        assert!(
            converted.report.residual().is_empty(),
            "{:?}",
            converted.report.residual()
        );
    }

    /// A Type 1 test program and the lengths of its three parts.
    fn type1_program() -> (Vec<u8>, String) {
        let program = super::type1::test_program(
            &[(".notdef", 0, 250), ("A", 10, 722), ("B", 10, 667)],
            None,
        );
        let clear = program.windows(6).position(|w| w == b"eexec\n").unwrap() + 6;
        let trailer = 1 + 8 * 65 + b"cleartomark\n".len();
        let lengths = format!(
            "/Length1 {clear} /Length2 {} /Length3 {trailer}",
            program.len() - clear - trailer
        );
        (program, lengths)
    }

    fn type1_font(widths: &str) -> String {
        format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Embedded {widths} /FontDescriptor \
             << /Type /FontDescriptor /FontName /Embedded /Flags 32 /FontBBox [0 0 1000 1000] \
             /ItalicAngle 0 /Ascent 718 /Descent -207 /CapHeight 718 /StemV 88 /FontFile 6 0 R >> >>"
        )
    }

    impl Converted {
        fn type1(&self) -> super::type1::Type1 {
            let descriptor = self.get(self.font().get(b"FontDescriptor").unwrap());
            let file = descriptor
                .as_dict()
                .unwrap()
                .get(b"FontFile")
                .unwrap()
                .clone();
            let Object::Stream(stream) = self.get(&file) else {
                panic!("no program");
            };
            let length = |key: &[u8]| {
                stream
                    .dict
                    .get(key)
                    .and_then(Object::as_i64)
                    .and_then(|n| usize::try_from(n).ok())
            };
            let lengths = [length(b"Length1"), length(b"Length2"), length(b"Length3")];
            super::type1::Type1::parse(&self.stream_data(&file), lengths).unwrap()
        }
    }

    #[test]
    fn an_embedded_type1_program_follows_the_widths_of_its_dictionary() {
        // The program gives "A" 722 where the dictionary says 600: the program is changed, the
        // dictionary kept, as for CFF.
        let (program, lengths) = type1_program();
        let converted = Converted::new(pdf_with(
            b"BT /F1 12 Tf 10 10 Td (AB) Tj ET",
            &type1_font("/FirstChar 65 /LastChar 66 /Widths [600 667]"),
            Some((&lengths, &program)),
        ));
        assert_eq!(converted.widths(), (65, vec![600.0, 667.0]));
        let embedded = converted.type1();
        assert_eq!(embedded.width(embedded.glyph("A").unwrap()), Some(600.0));
        assert_eq!(embedded.width(embedded.glyph("B").unwrap()), Some(667.0));
        assert_eq!(
            converted.report.changes().get("program_widths_aligned"),
            Some(&1)
        );
        assert!(
            converted.report.residual().is_empty(),
            "{:?}",
            converted.report.residual()
        );
    }

    #[test]
    fn an_embedded_font_without_widths_gets_those_of_its_program() {
        // Readers take the widths from the program: the dictionary is given them, every code.
        let (program, lengths) = type1_program();
        let converted = Converted::new(pdf_with(
            b"BT /F1 12 Tf 10 10 Td (A) Tj ET",
            &type1_font(""),
            Some((&lengths, &program)),
        ));
        let (first, widths) = converted.widths();
        assert_eq!((first, widths.len()), (0, 256));
        assert_eq!(widths[usize::from(b'A')], 722.0);
        assert_eq!(widths[usize::from(b'B')], 667.0);
        // A code without a glyph: `.notdef`'s width.
        assert_eq!(widths[usize::from(b'z')], 250.0);
        assert_eq!(converted.report.changes().get("widths_added"), Some(&1));
        assert!(
            converted.report.residual().is_empty(),
            "{:?}",
            converted.report.residual()
        );
    }

    #[test]
    fn an_embedded_cff_program_follows_the_widths_of_its_dictionary() {
        // A font its producer embedded, whose program gives "A" 667 where the dictionary
        // says 600: the program is changed, the dictionary kept.
        let program = super::standard::Standard::Helvetica.program();
        let converted = Converted::new(pdf_with(
            b"BT /F1 12 Tf 10 10 Td (A) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Embedded /FirstChar 65 /LastChar 65 \
             /Widths [600] /FontDescriptor << /Type /FontDescriptor /FontName /Embedded \
             /Flags 32 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 718 /Descent -207 \
             /CapHeight 718 /StemV 88 /FontFile3 6 0 R >> >>",
            Some(("/Subtype /Type1C", program)),
        ));
        assert_eq!(converted.widths(), (65, vec![600.0]));
        let embedded = converted.program();
        let cff = Cff::parse(&embedded).unwrap();
        assert_eq!(cff.width(cff.glyph("A").unwrap()), Some(600.0));
        assert_eq!(
            converted.report.changes().get("program_widths_aligned"),
            Some(&1)
        );
        assert_eq!(converted.report.changes().get("font_embedded"), None);
    }

    #[test]
    fn a_composite_true_type_program_follows_w() {
        // CIDs 0 and 1, glyphs 0 and 1 (Identity): /W gives 600 and 900 where the program has
        // 500 and 1000. CID 2 is shown but not in /W: /DW applies.
        let program = super::truetype::test_program();
        let converted = Converted::new(pdf_with(
            b"BT /F1 12 Tf 10 10 Td <000000010002> Tj ET",
            "<< /Type /Font /Subtype /Type0 /BaseFont /Sans /Encoding /Identity-H \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sans \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /CIDToGIDMap /Identity /W [0 [600 900]] /DW 700 /FontDescriptor << \
             /Type /FontDescriptor /FontName /Sans /Flags 32 /FontBBox [0 0 1000 1000] \
             /ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 \
             /FontFile2 6 0 R >> >>] >>",
            Some((&format!("/Length1 {}", program.len()), &program)),
        ));
        assert_eq!(
            converted.report.changes().get("program_widths_aligned"),
            Some(&1)
        );
        let font = converted.font();
        let descendants = converted.get(font.get(b"DescendantFonts").unwrap());
        let cid_font = converted.get(&descendants.as_array().unwrap()[0]);
        let descriptor = converted.get(cid_font.as_dict().unwrap().get(b"FontDescriptor").unwrap());
        let file = descriptor
            .as_dict()
            .unwrap()
            .get(b"FontFile2")
            .unwrap()
            .clone();
        let data = converted.stream_data(&file);
        let program = super::truetype::TrueType::parse(&data).unwrap();
        let widths: Vec<f64> = (0..3).map(|g| program.width(g).unwrap().round()).collect();
        assert_eq!(widths, [600.0, 900.0, 700.0]);
        let Object::Stream(stream) = converted.get(&file) else {
            panic!("not a stream");
        };
        assert_eq!(
            stream.dict.get(b"Length1").and_then(Object::as_i64),
            i64::try_from(data.len()).ok()
        );
    }

    #[test]
    fn a_code_the_producer_subset_lacks_gets_a_blank_glyph() {
        // The program has `a` and `b`, no space; `.notdef` draws nothing. Without `/Encoding`,
        // the code is given in the program's own encoding; with WinAnsiEncoding, the glyph is
        // found by its name.
        let program = super::cff::test_program(Some(super::cff::TEST_ENCODING));
        let mut widths = vec!["0"; 67];
        widths[0] = "200";
        widths[65] = "500";
        widths[66] = "600";
        let widths = widths.join(" ");
        for (flags, encoding) in [(4, ""), (32, "/Encoding /WinAnsiEncoding")] {
            let converted = Converted::new(pdf_with(
                b"BT /F1 12 Tf 10 10 Td (a b) Tj ET",
                &format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+T {encoding} \
                     /FirstChar 32 /LastChar 98 /Widths [{widths}] /FontDescriptor << \
                     /Type /FontDescriptor /FontName /ABCDEF+T /Flags {flags} \
                     /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 700 /Descent -200 \
                     /CapHeight 700 /StemV 80 /FontFile3 6 0 R >> >>"
                ),
                Some(("/Subtype /Type1C", &program)),
            ));
            let changes = converted.report.changes();
            assert_eq!(changes.get("glyph_blank_added"), Some(&1), "{encoding}");
            assert_eq!(changes.get("program_widths_aligned"), None, "{encoding}");
            assert!(converted.report.residual().is_empty(), "{encoding}");
            let embedded = converted.program();
            let cff = Cff::parse(&embedded).unwrap();
            let space = cff.glyph("space").unwrap();
            assert_eq!(cff.width(space), Some(200.0));
            assert!(cff.draws_nothing(space));
            if encoding.is_empty() {
                assert_eq!(cff.built_in(b' '), Some(space));
            } else {
                assert_eq!(cff.built_in(b' '), None);
            }
            assert_eq!(cff.built_in(b'a'), Some(1));
            assert_eq!(cff.width(1), Some(500.0));
        }
    }

    #[test]
    fn a_glyph_the_substitute_lacks_is_reported() {
        // The Foxit fonts have Adobe's standard Latin character set: no "abreve".
        let converted = Converted::new(pdf(
            b"BT /F1 12 Tf 10 10 Td (A) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding << /Differences [65 /abreve] >> >>",
        ));
        assert_eq!(converted.report.residual().get("glyph_missing"), Some(&1));
    }

    #[test]
    fn a_symbolic_font_nothing_stands_in_for_is_left() {
        let converted = Converted::new(pdf(
            b"BT /F1 12 Tf 10 10 Td (a) Tj ET",
            "<< /Type /Font /Subtype /TrueType /BaseFont /Wingdings-Regular /FirstChar 97 \
             /LastChar 97 /Widths [500] /FontDescriptor << /Type /FontDescriptor \
             /FontName /Wingdings-Regular /Flags 4 >> >>",
        ));
        assert_eq!(
            converted.report.residual().get("font_without_substitute"),
            Some(&1)
        );
        assert_eq!(
            converted.report.residual().get("font_not_embedded"),
            Some(&1)
        );
    }
}
