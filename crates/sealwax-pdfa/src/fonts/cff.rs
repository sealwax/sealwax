//! The Compact Font Format (Adobe Technical Note #5176), as far as embedding a font needs: the
//! glyph names, built-in encoding and advance widths of a name-keyed font with one face, and a
//! copy of it with some advance widths changed and some blank glyphs added.
//!
//! PDF/A requires the widths of a font dictionary and of its program to agree (ISO 19005-3,
//! 6.2.11.5). The text is laid out with the dictionary's widths: they are the ones kept, and
//! the program is changed to match. A glyph's advance width in a Type 2 charstring is the
//! optional first operand before the first stack-clearing operator; nothing else moves.
//!
//! PDF/A also requires the program to have a glyph for every code shown (6.2.11.4.1). A subset
//! can lack one, the space most often: readers then draw `.notdef`. Where `.notdef` draws
//! nothing, a blank glyph with the dictionary's width changes nothing drawn. Its code goes into
//! the custom encoding itself, not a supplement: veraPDF 1.28 reads a supplement's glyph name as
//! a place in the encoding.

use std::ops::Range;

use read_fonts::ps::Sid;

/// Glyphs a font may have (the CFF limit).
const MAX_GLYPHS: usize = 65_535;
/// Subroutine calls written in place before a glyph's first stack-clearing operator.
const MAX_INLINED: usize = 16;
/// The standard strings, string identifiers 0 to 390.
const STANDARD_STRINGS: u16 = 391;
/// The largest string identifier.
const MAX_SID: u16 = 64_999;

/// What [`Cff::walk`] keeps track of.
struct Walk {
    stack: Vec<f64>,
    stems: usize,
}

/// A DICT operand, kept as encoded so that it can be written back unchanged.
#[derive(Debug, Clone)]
enum Operand {
    Int(i32),
    Real(f64, Vec<u8>),
}

impl Operand {
    fn value(&self) -> f64 {
        match self {
            Self::Int(value) => f64::from(*value),
            Self::Real(value, _) => *value,
        }
    }
}

/// A DICT entry: operator (`12 x` is `1200 + x`) and operands.
#[derive(Debug, Clone)]
struct Entry {
    op: u16,
    operands: Vec<Operand>,
}

const CHARSET: u16 = 15;
const ENCODING: u16 = 16;
const CHAR_STRINGS: u16 = 17;
const PRIVATE: u16 = 18;
const SUBRS: u16 = 19;
const DEFAULT_WIDTH_X: u16 = 20;
const NOMINAL_WIDTH_X: u16 = 21;
const FONT_BBOX: u16 = 5;
const ITALIC_ANGLE: u16 = 1202;
const CHARSTRING_TYPE: u16 = 1206;
const FONT_MATRIX: u16 = 1207;
const ROS: u16 = 1230;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn offset_at(data: &[u8], at: usize, size: u8) -> Option<usize> {
    let bytes = data.get(at..at + usize::from(size))?;
    Some(
        bytes
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b)),
    )
}

/// An INDEX at `at`: the ranges of its items, and where it ends.
fn index(data: &[u8], at: usize) -> Option<(Vec<Range<usize>>, usize)> {
    let count = usize::from(u16_at(data, at)?);
    if count == 0 {
        return Some((Vec::new(), at + 2));
    }
    let size = *data.get(at + 2)?;
    if !(1..=4).contains(&size) {
        return None;
    }
    let offsets = at + 3;
    let base = offsets + (count + 1) * usize::from(size) - 1;
    let mut items = Vec::with_capacity(count);
    let mut previous = offset_at(data, offsets, size)?;
    if previous != 1 {
        return None;
    }
    for i in 1..=count {
        let next = offset_at(data, offsets + i * usize::from(size), size)?;
        if next < previous || base + next > data.len() {
            return None;
        }
        items.push(base + previous..base + next);
        previous = next;
    }
    Some((items, base + previous))
}

/// Parse a DICT.
fn dict(data: &[u8]) -> Option<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut operands = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let b0 = data[at];
        match b0 {
            0..=11 | 13..=21 => {
                entries.push(Entry {
                    op: u16::from(b0),
                    operands: std::mem::take(&mut operands),
                });
                at += 1;
            }
            12 => {
                let b1 = *data.get(at + 1)?;
                entries.push(Entry {
                    op: 1200 + u16::from(b1),
                    operands: std::mem::take(&mut operands),
                });
                at += 2;
            }
            28 => {
                let value = i16::from_be_bytes([*data.get(at + 1)?, *data.get(at + 2)?]);
                operands.push(Operand::Int(i32::from(value)));
                at += 3;
            }
            29 => {
                let bytes = data.get(at + 1..at + 5)?;
                operands.push(Operand::Int(i32::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ])));
                at += 5;
            }
            30 => {
                let start = at;
                let mut text = String::new();
                at += 1;
                'real: loop {
                    let byte = *data.get(at)?;
                    at += 1;
                    for nibble in [byte >> 4, byte & 0xF] {
                        match nibble {
                            0..=9 => text.push(char::from(b'0' + nibble)),
                            0xA => text.push('.'),
                            0xB => text.push('E'),
                            0xC => text.push_str("E-"),
                            0xE => text.push('-'),
                            0xF => break 'real,
                            _ => return None,
                        }
                    }
                }
                let value = text.parse::<f64>().unwrap_or(0.0);
                operands.push(Operand::Real(value, data[start..at].to_vec()));
            }
            32..=246 => {
                operands.push(Operand::Int(i32::from(b0) - 139));
                at += 1;
            }
            247..=250 => {
                let b1 = *data.get(at + 1)?;
                operands.push(Operand::Int(
                    (i32::from(b0) - 247) * 256 + i32::from(b1) + 108,
                ));
                at += 2;
            }
            251..=254 => {
                let b1 = *data.get(at + 1)?;
                operands.push(Operand::Int(
                    -(i32::from(b0) - 251) * 256 - i32::from(b1) - 108,
                ));
                at += 2;
            }
            _ => return None,
        }
    }
    Some(entries)
}

fn find(entries: &[Entry], op: u16) -> Option<&[Operand]> {
    entries
        .iter()
        .find(|e| e.op == op)
        .map(|e| e.operands.as_slice())
}

fn first_value(entries: &[Entry], op: u16) -> Option<f64> {
    find(entries, op)?.first().map(Operand::value)
}

/// A non-negative offset or size operand.
fn usize_operand(operand: &Operand) -> Option<usize> {
    match operand {
        Operand::Int(value) => usize::try_from(*value).ok(),
        Operand::Real(..) => None,
    }
}

/// A name-keyed CFF font with one face.
#[derive(Debug)]
pub(crate) struct Cff<'a> {
    data: &'a [u8],
    header: Range<usize>,
    names: Range<usize>,
    top: Vec<Entry>,
    strings_index: Range<usize>,
    strings: Vec<Range<usize>>,
    global_subrs: Range<usize>,
    char_strings: Vec<Range<usize>>,
    /// The string identifier of each glyph's name.
    sids: Vec<u16>,
    charset: Option<Range<usize>>,
    encoding: Option<Range<usize>>,
    /// The built-in encoding is Standard Encoding (no custom encoding, offset 0).
    standard_encoding: bool,
    /// The glyph of each code in the built-in encoding (0: none).
    built_in: [u16; 256],
    private: Vec<Entry>,
    local_subrs: Option<Range<usize>>,
    local_items: Vec<Range<usize>>,
    global_items: Vec<Range<usize>>,
    default_width: f64,
    nominal_width: f64,
    /// Glyph space units per 1000 text space units (1 for a 1000-unit em).
    units: f64,
}

impl<'a> Cff<'a> {
    /// Parse `data`. `None` for what this module does not handle: a CID-keyed font, several
    /// faces, Type 1 charstrings, or damage.
    pub(crate) fn parse(data: &'a [u8]) -> Option<Self> {
        let header_size = usize::from(*data.get(2)?);
        if *data.first()? != 1 || header_size < 4 {
            return None;
        }
        let (names, names_end) = index(data, header_size)?;
        let (tops, tops_end) = index(data, names_end)?;
        if names.len() != 1 || tops.len() != 1 {
            return None;
        }
        let top = dict(data.get(tops[0].clone())?)?;
        if find(&top, ROS).is_some() || first_value(&top, CHARSTRING_TYPE).is_some_and(|t| t != 2.0)
        {
            return None;
        }
        let (strings, strings_end) = index(data, tops_end)?;
        let (global_items, subrs_end) = index(data, strings_end)?;
        let char_strings_at = usize_operand(find(&top, CHAR_STRINGS)?.first()?)?;
        let (char_strings, _) = index(data, char_strings_at)?;
        let glyphs = char_strings.len();
        if glyphs == 0 || glyphs > MAX_GLYPHS {
            return None;
        }

        let charset_at = find(&top, CHARSET)
            .and_then(|o| o.first())
            .and_then(usize_operand)
            .unwrap_or(0);
        let (sids, charset) = match charset_at {
            // ISOAdobe: glyph i is named by standard string i.
            0 => (
                (0..glyphs).map(|g| u16::try_from(g).unwrap_or(0)).collect(),
                None,
            ),
            1 | 2 => return None,
            at => {
                let (sids, end) = charset(data, at, glyphs)?;
                (sids, Some(at..end))
            }
        };

        let encoding_at = find(&top, ENCODING)
            .and_then(|o| o.first())
            .and_then(usize_operand)
            .unwrap_or(0);
        let (built_in, encoding) = match encoding_at {
            0 => (standard_built_in(&sids), None),
            1 => ([0; 256], None),
            at => {
                let (codes, end) = encoding(data, at, &sids)?;
                (codes, Some(at..end))
            }
        };

        let private = find(&top, PRIVATE)?;
        let (size, at) = (
            usize_operand(private.first()?)?,
            usize_operand(private.get(1)?)?,
        );
        let private = dict(data.get(at..at.checked_add(size)?)?)?;
        let (local_subrs, local_items) = match find(&private, SUBRS).and_then(|o| o.first()) {
            Some(offset) => {
                let start = at.checked_add(usize_operand(offset)?)?;
                let (items, end) = index(data, start)?;
                (Some(start..end), items)
            }
            None => (None, Vec::new()),
        };
        let units = find(&top, FONT_MATRIX)
            .and_then(|m| m.first())
            .map_or(1.0, |scale| scale.value() * 1000.0);
        if !(units.is_finite() && units > 0.0) {
            return None;
        }
        Some(Self {
            data,
            header: 0..header_size,
            names: header_size..names_end,
            default_width: first_value(&private, DEFAULT_WIDTH_X).unwrap_or(0.0),
            nominal_width: first_value(&private, NOMINAL_WIDTH_X).unwrap_or(0.0),
            top,
            strings_index: tops_end..strings_end,
            strings,
            global_subrs: strings_end..subrs_end,
            char_strings,
            sids,
            charset,
            standard_encoding: encoding_at == 0,
            encoding,
            built_in,
            private,
            local_subrs,
            local_items,
            global_items,
            units,
        })
    }

    pub(crate) fn glyph_count(&self) -> usize {
        self.char_strings.len()
    }

    /// The name of glyph `gid`.
    pub(crate) fn name(&self, gid: u16) -> Option<&'a str> {
        let sid = *self.sids.get(usize::from(gid))?;
        let bytes = match Sid::new(sid).resolve_standard() {
            Ok(standard) => standard,
            Err(index) => self.data.get(self.strings.get(index)?.clone())?,
        };
        std::str::from_utf8(bytes).ok()
    }

    /// The glyph named `name`, if the font has one.
    pub(crate) fn glyph(&self, name: &str) -> Option<u16> {
        (1..self.char_strings.len())
            .filter_map(|g| u16::try_from(g).ok())
            .find(|g| self.name(*g) == Some(name))
    }

    /// The glyph of `code` in the font's built-in encoding.
    pub(crate) fn built_in(&self, code: u8) -> Option<u16> {
        Some(self.built_in[usize::from(code)]).filter(|g| *g != 0)
    }

    /// The font's bounding box and italic angle, in text space units per 1000 em.
    pub(crate) fn bbox(&self) -> [f64; 4] {
        let mut bbox = [0.0; 4];
        if let Some(values) = find(&self.top, FONT_BBOX) {
            for (slot, value) in bbox.iter_mut().zip(values) {
                *slot = value.value() * self.units;
            }
        }
        bbox
    }

    pub(crate) fn italic_angle(&self) -> f64 {
        first_value(&self.top, ITALIC_ANGLE).unwrap_or(0.0)
    }

    /// A glyph's charstring with the subroutines it calls before its first stack-clearing
    /// operator written in place (which changes nothing it draws), and its width operand there:
    /// value and bytes, `None` when it has none (the default width then applies). `Err` when
    /// the charstring cannot be read that far.
    #[allow(clippy::type_complexity)]
    fn expanded(&self, gid: u16) -> Result<(Vec<u8>, Option<(f64, Range<usize>)>), ()> {
        let range = self.char_strings.get(usize::from(gid)).ok_or(())?;
        let mut code = self.data.get(range.clone()).ok_or(())?.to_vec();
        let mut operands: Vec<(f64, Range<usize>)> = Vec::new();
        let mut inlined = 0;
        let mut at = 0;
        loop {
            let b0 = *code.get(at).ok_or(())?;
            if let Some((value, len)) = charstring_number(&code, at) {
                operands.push((value, at..at + len));
                at += len;
                continue;
            }
            let expected = match b0 {
                // callsubr, callgsubr: the subroutine is written in place of the call.
                10 | 29 => {
                    let (number, bytes) = operands.pop().ok_or(())?;
                    inlined += 1;
                    if bytes.end != at || inlined > MAX_INLINED || number.fract() != 0.0 {
                        return Err(());
                    }
                    let items = if b0 == 10 {
                        &self.local_items
                    } else {
                        &self.global_items
                    };
                    let body = self.subroutine(items, number).ok_or(())?;
                    let body = self.inlinable(body, operands.len()).ok_or(())?;
                    code.splice(bytes.start..=at, body.iter().copied());
                    at = bytes.start;
                    continue;
                }
                // hstem, vstem, hstemhm, vstemhm, hintmask, cntrmask: pairs.
                1 | 3 | 18 | 23 | 19 | 20 => operands.len() & !1,
                21 => 2,     // rmoveto
                4 | 22 => 1, // vmoveto, hmoveto
                // endchar, with the four seac arguments or none.
                14 => {
                    if operands.len() >= 4 {
                        4
                    } else {
                        0
                    }
                }
                _ => return Err(()),
            };
            let width = (operands.len() > expected)
                .then(|| operands.into_iter().next())
                .flatten()
                .map(|(value, bytes)| (self.nominal_width + value, bytes));
            return Ok((code, width));
        }
    }

    /// The subroutine `number` (unbiased) of `items`.
    fn subroutine(&self, items: &[Range<usize>], number: f64) -> Option<&'a [u8]> {
        let bias: i64 = match items.len() {
            0..1240 => 107,
            1240..33900 => 1131,
            _ => 32768,
        };
        if number.fract() != 0.0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        let index = usize::try_from(number as i64 + bias).ok()?;
        self.data.get(items.get(index)?.clone())
    }

    /// Walk `code` as far as `return` or `endchar`, following subroutine calls, keeping only
    /// what sizes a hint mask: the operands on the stack and the stems declared. Returns where
    /// the token it stopped at starts (`code.len()` when it ran to the end).
    fn walk(&self, code: &[u8], walk: &mut Walk, depth: usize) -> Option<usize> {
        if depth > MAX_INLINED {
            return None;
        }
        let mut at = 0;
        while at < code.len() {
            if let Some((value, len)) = charstring_number(code, at) {
                walk.stack.push(value);
                at += len;
                continue;
            }
            match code[at] {
                1 | 3 | 18 | 23 => {
                    walk.stems += walk.stack.len() / 2;
                    walk.stack.clear();
                    at += 1;
                }
                19 | 20 => {
                    walk.stems += walk.stack.len() / 2;
                    walk.stack.clear();
                    at += 1 + walk.stems.div_ceil(8);
                }
                op @ (10 | 29) => {
                    let number = walk.stack.pop()?;
                    let items = if op == 10 {
                        &self.local_items
                    } else {
                        &self.global_items
                    };
                    self.walk(self.subroutine(items, number)?, walk, depth + 1)?;
                    at += 1;
                }
                11 | 14 => return Some(at),
                // hflex, flex, hflex1, flex1; the arithmetic operators are not followed.
                12 => match code.get(at + 1)? {
                    34..=37 => {
                        walk.stack.clear();
                        at += 2;
                    }
                    _ => return None,
                },
                _ => {
                    walk.stack.clear();
                    at += 1;
                }
            }
        }
        Some(code.len())
    }

    /// A subroutine's body without its final `return`, so that it can be written in place of
    /// its call; `stack` operands are on the stack when it is called, before any stem.
    fn inlinable(&self, body: &'a [u8], stack: usize) -> Option<&'a [u8]> {
        let mut walk = Walk {
            stack: vec![0.0; stack],
            stems: 0,
        };
        let stop = self.walk(body, &mut walk, 0)?;
        match body.get(stop) {
            // `return`, last: dropped.
            Some(11) if stop + 1 == body.len() => body.get(..stop),
            // `endchar` ends the glyph; the end of the body, nothing to drop.
            Some(14) | None => Some(body),
            _ => None,
        }
    }

    /// The advance width of glyph `gid`, in text space units per 1000 em.
    pub(crate) fn width(&self, gid: u16) -> Option<f64> {
        let width = match self.expanded(gid).ok()?.1 {
            Some((width, _)) => width,
            None => self.default_width,
        };
        Some(width * self.units)
    }

    /// Whether glyph `gid` draws nothing: its charstring is at most a width, then `endchar`.
    pub(crate) fn draws_nothing(&self, gid: u16) -> bool {
        let Some(code) = self
            .char_strings
            .get(usize::from(gid))
            .and_then(|range| self.data.get(range.clone()))
        else {
            return false;
        };
        let (mut at, mut operands) = (0, 0);
        while let Some((_, len)) = charstring_number(code, at) {
            at += len;
            operands += 1;
        }
        code.get(at) == Some(&14) && operands <= 1
    }

    /// A copy of the font where each glyph of `widths` has that advance width (text space units
    /// per 1000 em). `None` when a glyph's width cannot be changed.
    pub(crate) fn with_widths(&self, widths: &[(u16, f64)]) -> Option<Vec<u8>> {
        self.edited(widths, &[])
    }

    /// A copy of the font with the advance widths of `widths` and the glyphs of `blanks` added
    /// after the others. `None` when a width cannot be changed or a glyph cannot be added.
    pub(crate) fn edited(&self, widths: &[(u16, f64)], blanks: &[Blank<'_>]) -> Option<Vec<u8>> {
        let mut char_strings: Vec<Vec<u8>> = self
            .char_strings
            .iter()
            .map(|r| self.data.get(r.clone()).map(<[u8]>::to_vec))
            .collect::<Option<_>>()?;
        for (gid, width) in widths {
            let (expanded, current) = self.expanded(*gid).ok()?;
            let start = current.map_or(0, |(_, bytes)| bytes.end);
            let mut code = self.width_operand(*width);
            code.extend_from_slice(expanded.get(start..)?);
            *char_strings.get_mut(usize::from(*gid))? = code;
        }
        if blanks.is_empty() {
            return Some(self.write(Parts {
                char_strings,
                ..Parts::default()
            }));
        }
        if char_strings.len() + blanks.len() > MAX_GLYPHS {
            return None;
        }
        let mut strings: Vec<&[u8]> = self
            .strings
            .iter()
            .map(|r| self.data.get(r.clone()))
            .collect::<Option<_>>()?;
        let mut sids = self.sids.clone();
        // Glyphs given a code go right after those the custom encoding gives one, and their
        // codes after these: a supplement is not read alike by every reader (veraPDF 1.28 takes
        // its glyph name for a place in the encoding). Glyphs found by name go last; a simple
        // font's glyphs are known by code or name, never by number.
        let coded_at = match self.encoding {
            Some(_) => self.encoded_glyphs()? + 1,
            None => char_strings.len(),
        };
        let mut codes: Vec<u8> = Vec::new();
        for blank in blanks {
            let sid = string_id(blank.name.as_bytes(), &mut strings)?;
            if sids.contains(&sid) {
                return None;
            }
            let mut code = self.width_operand(blank.width);
            code.push(14);
            let at = match blank.code {
                None => char_strings.len(),
                Some(code_point) if self.encoding.is_some() => {
                    codes.push(code_point);
                    coded_at + codes.len() - 1
                }
                // Standard Encoding finds the glyph by its name.
                Some(code_point)
                    if self.standard_encoding
                        && super::encodings::STANDARD[usize::from(code_point)] == blank.name =>
                {
                    char_strings.len()
                }
                Some(_) => return None,
            };
            if at > char_strings.len() {
                return None;
            }
            char_strings.insert(at, code);
            sids.insert(at, sid);
        }
        let mut charset = vec![0u8];
        for sid in sids.iter().skip(1) {
            charset.extend_from_slice(&sid.to_be_bytes());
        }
        let encoding = if codes.is_empty() {
            None
        } else {
            Some(self.extended(&codes)?)
        };
        let strings = (strings.len() != self.strings.len()).then(|| {
            let items: Vec<Vec<u8>> = strings.iter().map(|s| s.to_vec()).collect();
            encode_index(&items)
        });
        Some(self.write(Parts {
            char_strings,
            strings,
            charset: Some(charset),
            encoding,
        }))
    }

    /// The width operand that starts a charstring of advance width `width` (text space units
    /// per 1000 em): none for the default width.
    fn width_operand(&self, width: f64) -> Vec<u8> {
        let target = width / self.units;
        let mut code = Vec::new();
        if (target - self.default_width).abs() > f64::EPSILON {
            encode_charstring_number(&mut code, target - self.nominal_width);
        }
        code
    }

    /// How many glyphs, from glyph 1, the custom encoding gives a code (supplements aside).
    fn encoded_glyphs(&self) -> Option<usize> {
        let raw = self.data.get(self.encoding.clone()?)?;
        let count = usize::from(*raw.get(1)?);
        match *raw.first()? & 0x7F {
            0 => Some(count),
            1 => (0..count)
                .map(|range| raw.get(3 + 2 * range).map(|left| usize::from(*left) + 1))
                .sum(),
            _ => None,
        }
    }

    /// The custom encoding with `codes` given to the glyphs after those it encodes.
    fn extended(&self, codes: &[u8]) -> Option<Vec<u8>> {
        let raw = self.data.get(self.encoding.clone()?)?;
        let format = *raw.first()?;
        let count = usize::from(*raw.get(1)?);
        let main = match format & 0x7F {
            0 => 2 + count,
            1 => 2 + 2 * count,
            _ => return None,
        };
        let mut out = vec![format, u8::try_from(count + codes.len()).ok()?];
        out.extend_from_slice(raw.get(2..main)?);
        for code in codes {
            out.push(*code);
            if format & 0x7F == 1 {
                // A range of one code.
                out.push(0);
            }
        }
        // The supplements, unchanged: they name their glyphs.
        out.extend_from_slice(raw.get(main..)?);
        Some(out)
    }

    fn write(&self, parts: Parts) -> Vec<u8> {
        let raw = |range: &Range<usize>| &self.data[range.clone()];
        let header = raw(&self.header);
        let names = raw(&self.names);
        let strings = parts
            .strings
            .as_deref()
            .unwrap_or_else(|| raw(&self.strings_index));
        let global_subrs = raw(&self.global_subrs);
        let charset = parts
            .charset
            .as_deref()
            .unwrap_or_else(|| self.charset.as_ref().map(raw).unwrap_or_default());
        let encoding = parts
            .encoding
            .as_deref()
            .unwrap_or_else(|| self.encoding.as_ref().map(raw).unwrap_or_default());
        let char_strings = encode_index(&parts.char_strings);
        let subrs = self.local_subrs.as_ref().map(raw).unwrap_or_default();
        let has_charset = !charset.is_empty();
        let has_encoding = !encoding.is_empty();
        let mut top = self.top.clone();
        if has_charset && find(&top, CHARSET).is_none() {
            top.push(Entry {
                op: CHARSET,
                operands: vec![Operand::Int(0)],
            });
        }

        // The Private DICT, with its subroutines right after it.
        let private_len = encode_dict(&self.private, &|op| (op == SUBRS).then_some(vec![0])).len();
        let private = encode_dict(&self.private, &|op| {
            (op == SUBRS).then(|| vec![i32::try_from(private_len).unwrap_or(0)])
        });

        // Offsets are written on five bytes, so that the Top DICT's size does not depend on
        // them: measured with zeros, then written with the real values.
        let offsets =
            |charset_at: usize, encoding_at: usize, char_strings_at: usize, private_at: usize| {
                let as_i32 = |value: usize| i32::try_from(value).unwrap_or(0);
                let private_size = as_i32(private.len());
                move |op: u16| match op {
                    CHARSET if has_charset => Some(vec![as_i32(charset_at)]),
                    ENCODING if has_encoding => Some(vec![as_i32(encoding_at)]),
                    CHAR_STRINGS => Some(vec![as_i32(char_strings_at)]),
                    PRIVATE => Some(vec![private_size, as_i32(private_at)]),
                    _ => None,
                }
            };
        let top_len = encode_dict(&top, &offsets(0, 0, 0, 0)).len();
        let top_index_len = encode_index(&[vec![0; top_len]]).len();
        let start = header.len() + names.len() + top_index_len + strings.len() + global_subrs.len();
        let charset_at = start;
        let encoding_at = charset_at + charset.len();
        let char_strings_at = encoding_at + encoding.len();
        let private_at = char_strings_at + char_strings.len();
        let top = encode_dict(
            &top,
            &offsets(charset_at, encoding_at, char_strings_at, private_at),
        );

        let mut out = Vec::new();
        out.extend_from_slice(header);
        out.extend_from_slice(names);
        out.extend_from_slice(&encode_index(&[top]));
        out.extend_from_slice(strings);
        out.extend_from_slice(global_subrs);
        out.extend_from_slice(charset);
        out.extend_from_slice(encoding);
        out.extend_from_slice(&char_strings);
        out.extend_from_slice(&private);
        out.extend_from_slice(subrs);
        out
    }
}

/// A glyph to add to a font: it draws nothing.
pub(crate) struct Blank<'n> {
    pub(crate) name: &'n str,
    /// Advance width, in text space units per 1000 em.
    pub(crate) width: f64,
    /// A code of the built-in encoding to give it.
    pub(crate) code: Option<u8>,
}

/// What [`Cff::write`] writes in place of the font's own: `None` keeps it.
#[derive(Default)]
struct Parts {
    char_strings: Vec<Vec<u8>>,
    /// The String INDEX, encoded.
    strings: Option<Vec<u8>>,
    charset: Option<Vec<u8>>,
    encoding: Option<Vec<u8>>,
}

/// The string identifier of `name`: a standard string's, else that of the font's string, else
/// a new string's, added to `strings`.
fn string_id<'s>(name: &'s [u8], strings: &mut Vec<&'s [u8]>) -> Option<u16> {
    let standard =
        (0..STANDARD_STRINGS).find(|sid| Sid::new(*sid).resolve_standard().ok() == Some(name));
    if let Some(sid) = standard {
        return Some(sid);
    }
    let index = match strings.iter().position(|s| *s == name) {
        Some(index) => index,
        None => {
            strings.push(name);
            strings.len() - 1
        }
    };
    u16::try_from(index)
        .ok()?
        .checked_add(STANDARD_STRINGS)
        .filter(|sid| *sid <= MAX_SID)
}

/// A number in a Type 2 charstring at `at`: its value and length, `None` for an operator.
fn charstring_number(code: &[u8], at: usize) -> Option<(f64, usize)> {
    let b0 = *code.get(at)?;
    match b0 {
        28 => {
            let bytes = code.get(at + 1..at + 3)?;
            Some((f64::from(i16::from_be_bytes([bytes[0], bytes[1]])), 3))
        }
        32..=246 => Some((f64::from(i32::from(b0) - 139), 1)),
        247..=250 => {
            let b1 = *code.get(at + 1)?;
            Some((
                f64::from((i32::from(b0) - 247) * 256 + i32::from(b1) + 108),
                2,
            ))
        }
        251..=254 => {
            let b1 = *code.get(at + 1)?;
            Some((
                f64::from(-(i32::from(b0) - 251) * 256 - i32::from(b1) - 108),
                2,
            ))
        }
        255 => {
            let bytes = code.get(at + 1..at + 5)?;
            let fixed = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            Some((f64::from(fixed) / 65536.0, 5))
        }
        _ => None,
    }
}

/// The charset at `at` for `glyphs` glyphs: each glyph's string identifier, and where it ends.
fn charset(data: &[u8], at: usize, glyphs: usize) -> Option<(Vec<u16>, usize)> {
    let mut sids = vec![0u16];
    let format = *data.get(at)?;
    let mut cursor = at + 1;
    while sids.len() < glyphs {
        match format {
            0 => {
                sids.push(u16_at(data, cursor)?);
                cursor += 2;
            }
            1 | 2 => {
                let first = u16_at(data, cursor)?;
                let left = if format == 1 {
                    u16::from(*data.get(cursor + 2)?)
                } else {
                    u16_at(data, cursor + 2)?
                };
                cursor += if format == 1 { 3 } else { 4 };
                for i in 0..=left {
                    if sids.len() == glyphs {
                        break;
                    }
                    sids.push(first.checked_add(i)?);
                }
            }
            _ => return None,
        }
    }
    Some((sids, cursor))
}

/// The custom encoding at `at`: the glyph of each code, and where it ends.
fn encoding(data: &[u8], at: usize, sids: &[u16]) -> Option<([u16; 256], usize)> {
    let mut codes = [0u16; 256];
    let format = *data.get(at)?;
    let mut cursor = at + 1;
    match format & 0x7F {
        0 => {
            let count = usize::from(*data.get(cursor)?);
            cursor += 1;
            for gid in 1..=count {
                let code = *data.get(cursor)?;
                cursor += 1;
                if gid < sids.len() {
                    codes[usize::from(code)] = u16::try_from(gid).ok()?;
                }
            }
        }
        1 => {
            let ranges = usize::from(*data.get(cursor)?);
            cursor += 1;
            let mut gid = 1usize;
            for _ in 0..ranges {
                let first = usize::from(*data.get(cursor)?);
                let left = usize::from(*data.get(cursor + 1)?);
                cursor += 2;
                for slot in codes.iter_mut().skip(first).take(left + 1) {
                    if gid < sids.len() {
                        *slot = u16::try_from(gid).ok()?;
                    }
                    gid += 1;
                }
            }
        }
        _ => return None,
    }
    if format & 0x80 != 0 {
        let count = usize::from(*data.get(cursor)?);
        cursor += 1;
        for _ in 0..count {
            let code = *data.get(cursor)?;
            let sid = u16_at(data, cursor + 1)?;
            cursor += 3;
            if let Some(gid) = sids.iter().position(|s| *s == sid) {
                codes[usize::from(code)] = u16::try_from(gid).ok()?;
            }
        }
    }
    Some((codes, cursor))
}

/// The built-in encoding of a font that declares Standard Encoding.
fn standard_built_in(sids: &[u16]) -> [u16; 256] {
    let mut codes = [0u16; 256];
    for (code, name) in super::encodings::STANDARD.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        let gid = sids
            .iter()
            .position(|sid| Sid::new(*sid).resolve_standard().ok() == Some(name.as_bytes()));
        if let Some(gid) = gid.and_then(|g| u16::try_from(g).ok()) {
            codes[code] = gid;
        }
    }
    codes
}

fn encode_charstring_number(out: &mut Vec<u8>, value: f64) {
    let rounded = value.round();
    if (value - rounded).abs() > 1e-6 || !(-32768.0..=32767.0).contains(&rounded) {
        // 16.16 fixed point.
        let fixed = (value * 65536.0)
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX));
        #[allow(clippy::cast_possible_truncation)]
        let fixed = fixed as i32;
        out.push(255);
        out.extend_from_slice(&fixed.to_be_bytes());
        return;
    }
    #[allow(clippy::cast_possible_truncation)]
    let v = rounded as i32;
    encode_int(out, v, true);
}

/// An integer, in the shortest encoding (`charstring`: 28 is the 16-bit form in both).
fn encode_int(out: &mut Vec<u8>, v: i32, charstring: bool) {
    match v {
        -107..=107 => out.push(u8::try_from(v + 139).unwrap_or(139)),
        108..=1131 => {
            let w = v - 108;
            out.push(u8::try_from(247 + (w >> 8)).unwrap_or(247));
            out.push(u8::try_from(w & 0xFF).unwrap_or(0));
        }
        -1131..=-108 => {
            let w = -v - 108;
            out.push(u8::try_from(251 + (w >> 8)).unwrap_or(251));
            out.push(u8::try_from(w & 0xFF).unwrap_or(0));
        }
        -32768..=32767 => {
            out.push(28);
            out.extend_from_slice(&i16::try_from(v).unwrap_or(0).to_be_bytes());
        }
        _ if !charstring => {
            out.push(29);
            out.extend_from_slice(&v.to_be_bytes());
        }
        _ => {
            out.push(255);
            out.extend_from_slice(&v.saturating_mul(65536).to_be_bytes());
        }
    }
}

/// Encode a DICT; `replace` gives the operands of the entries written as five-byte offsets.
fn encode_dict(entries: &[Entry], replace: &dyn Fn(u16) -> Option<Vec<i32>>) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        if let Some(values) = replace(entry.op) {
            for value in values {
                out.push(29);
                out.extend_from_slice(&value.to_be_bytes());
            }
        } else {
            for operand in &entry.operands {
                match operand {
                    Operand::Int(value) => encode_int(&mut out, *value, false),
                    Operand::Real(_, raw) => out.extend_from_slice(raw),
                }
            }
        }
        if entry.op >= 1200 {
            out.push(12);
            out.push(u8::try_from(entry.op - 1200).unwrap_or(0));
        } else {
            out.push(u8::try_from(entry.op).unwrap_or(0));
        }
    }
    out
}

fn encode_index(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    let count = u16::try_from(items.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_be_bytes());
    if items.is_empty() {
        return out;
    }
    let total: usize = items.iter().map(Vec::len).sum::<usize>() + 1;
    let size: u8 = match total {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    };
    out.push(size);
    let mut offset = 1usize;
    let push = |out: &mut Vec<u8>, value: usize| {
        let bytes = value.to_be_bytes();
        out.extend_from_slice(&bytes[bytes.len() - usize::from(size)..]);
    };
    push(&mut out, offset);
    for item in items {
        offset += item.len();
        push(&mut out, offset);
    }
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

/// A font of three glyphs (`.notdef`, blank; `a`, drawn, 500 wide; `b`, blank, 600 wide), with
/// the custom encoding `encoding` or Standard Encoding, and no space.
#[cfg(test)]
pub(crate) fn test_program(encoding: Option<&[u8]>) -> Vec<u8> {
    let sid = |name: &[u8]| string_id(name, &mut Vec::new()).unwrap_or(0);
    let mut a = Vec::new();
    encode_charstring_number(&mut a, 500.0);
    a.extend_from_slice(&[139, 139, 21, 239, 6, 14]); // 0 0 rmoveto 100 hlineto endchar
    let mut b = Vec::new();
    encode_charstring_number(&mut b, 600.0);
    b.push(14);
    let char_strings = encode_index(&[vec![14], a, b]);
    let mut charset = vec![0u8];
    for name in [&b"a"[..], b"b"] {
        charset.extend_from_slice(&sid(name).to_be_bytes());
    }
    let own_encoding = encoding.is_some();
    let encoding = encoding.unwrap_or_default();
    let header = [1u8, 0, 4, 1];
    let names = encode_index(&[b"T".to_vec()]);
    let entry = |out: &mut Vec<u8>, values: &[usize], op: u8| {
        for value in values {
            out.push(29);
            out.extend_from_slice(&i32::try_from(*value).unwrap_or(0).to_be_bytes());
        }
        out.push(op);
    };
    let top_len = if own_encoding { 4 * 6 + 5 } else { 3 * 6 + 5 };
    let top_index_len = encode_index(&[vec![0; top_len]]).len();
    let charset_at = header.len() + names.len() + top_index_len + 2 + 2;
    let encoding_at = charset_at + charset.len();
    let char_strings_at = encoding_at + encoding.len();
    let private_at = char_strings_at + char_strings.len();
    let mut top = Vec::new();
    entry(&mut top, &[charset_at], 15);
    if own_encoding {
        entry(&mut top, &[encoding_at], 16);
    }
    entry(&mut top, &[char_strings_at], 17);
    entry(&mut top, &[0, private_at], 18);
    let mut out = header.to_vec();
    out.extend_from_slice(&names);
    out.extend_from_slice(&encode_index(&[top]));
    out.extend_from_slice(&[0, 0, 0, 0]); // String INDEX, Global Subr INDEX: empty
    out.extend_from_slice(&charset);
    out.extend_from_slice(encoding);
    out.extend_from_slice(&char_strings);
    out
}

/// The custom encoding of [`test_program`] that gives `a` and `b` their ASCII codes.
#[cfg(test)]
pub(crate) const TEST_ENCODING: &[u8] = &[0, 2, b'a', b'b'];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Blank, Cff, TEST_ENCODING, test_program};
    use crate::fonts::standard::Standard;

    #[test]
    fn every_foxit_font_parses() {
        for font in Standard::ALL {
            let cff = Cff::parse(font.program()).expect("parses");
            assert!(cff.glyph_count() > 100, "{font:?}");
            assert_eq!(cff.name(0), Some(".notdef"));
        }
    }

    #[test]
    fn every_foxit_glyph_width_can_be_read_and_changed() {
        for font in Standard::ALL {
            let cff = Cff::parse(font.program()).unwrap();
            let glyphs = u16::try_from(cff.glyph_count()).unwrap();
            let failing: Vec<&str> = (0..glyphs)
                .filter(|gid| cff.width(*gid).is_none())
                .map(|gid| cff.name(gid).unwrap_or("?"))
                .collect();
            assert!(failing.is_empty(), "{font:?}: {failing:?}");
            let all: Vec<(u16, f64)> = (1..glyphs).map(|gid| (gid, 500.0)).collect();
            let copy = cff.with_widths(&all).expect("every width changes");
            let again = Cff::parse(&copy).unwrap();
            assert!(
                (1..glyphs).all(|gid| again.width(gid) == Some(500.0)),
                "{font:?}"
            );
        }
    }

    #[test]
    fn names_widths_and_built_in_encoding() {
        let sans = Cff::parse(Standard::Helvetica.program()).unwrap();
        let a = sans.glyph("A").unwrap();
        assert_eq!(sans.name(a), Some("A"));
        assert_eq!(sans.width(a), Some(667.0));
        assert_eq!(sans.built_in(b'A'), Some(a));
        // Foxit's guillemets are wider than Adobe's Helvetica metrics (612, not 556).
        assert_eq!(
            sans.width(sans.glyph("guillemotleft").unwrap()),
            Some(612.0)
        );
        // Every Foxit font declares StandardEncoding, ZapfDingbats included: its codes must be
        // spelled out by the font dictionary.
        let dingbats = Cff::parse(Standard::ZapfDingbats.program()).unwrap();
        assert_eq!(dingbats.built_in(b'4'), None);
        assert!(dingbats.glyph("a20").is_some());
    }

    #[test]
    fn widths_change_and_nothing_else() {
        let program = Standard::Helvetica.program();
        let sans = Cff::parse(program).unwrap();
        let left = sans.glyph("guillemotleft").unwrap();
        let space = sans.glyph("space").unwrap();
        let rewritten = sans.with_widths(&[(left, 556.0), (space, 300.5)]).unwrap();
        let again = Cff::parse(&rewritten).expect("the copy parses");
        assert_eq!(again.glyph_count(), sans.glyph_count());
        assert_eq!(again.width(left), Some(556.0));
        assert_eq!(again.width(space), Some(300.5));
        for gid in 0..u16::try_from(sans.glyph_count()).unwrap() {
            assert_eq!(again.name(gid), sans.name(gid));
            if gid != left && gid != space {
                assert_eq!(again.width(gid), sans.width(gid), "{:?}", sans.name(gid));
            }
        }
        for code in 0..=255u8 {
            assert_eq!(again.built_in(code), sans.built_in(code));
        }
        // Unchanged widths give back an equivalent font.
        let copy = sans.with_widths(&[]).unwrap();
        let same = Cff::parse(&copy).unwrap();
        assert_eq!(same.width(left), Some(612.0));
    }

    #[test]
    fn damage_is_refused_not_panicked_on() {
        let program = Standard::TimesRoman.program();
        for cut in [0, 3, 10, 100, program.len() / 2] {
            let _ = Cff::parse(&program[..cut]);
        }
        let mut flipped = program.to_vec();
        for i in (0..flipped.len()).step_by(97) {
            flipped[i] ^= 0x5A;
            if let Some(cff) = Cff::parse(&flipped) {
                for gid in 0..u16::try_from(cff.glyph_count()).unwrap_or(0) {
                    let _ = cff.width(gid);
                    let _ = cff.name(gid);
                }
                let _ = cff.with_widths(&[(1, 500.0)]);
            }
        }
    }

    #[test]
    fn the_test_program_reads() {
        for encoding in [Some(TEST_ENCODING), None] {
            let data = test_program(encoding);
            let cff = Cff::parse(&data).expect("parses");
            assert_eq!(cff.glyph_count(), 3);
            assert_eq!(cff.glyph("a"), Some(1));
            assert_eq!(cff.width(1), Some(500.0));
            assert_eq!(cff.width(2), Some(600.0));
            assert_eq!(cff.built_in(b'a'), Some(1));
            assert_eq!(cff.built_in(b' '), None);
            assert!(cff.draws_nothing(0));
            assert!(!cff.draws_nothing(1));
            assert!(cff.draws_nothing(2));
        }
    }

    #[test]
    fn a_blank_glyph_is_added_with_its_code() {
        let data = test_program(Some(TEST_ENCODING));
        let cff = Cff::parse(&data).unwrap();
        let blanks = [
            Blank {
                name: "space",
                width: 200.0,
                code: Some(b' '),
            },
            // A name no standard string has: the String INDEX gets it.
            Blank {
                name: "g7",
                width: 0.0,
                code: Some(7),
            },
            Blank {
                name: "quoteright",
                width: 250.0,
                code: None,
            },
        ];
        let copy = cff.edited(&[(1, 520.0)], &blanks).expect("added");
        let again = Cff::parse(&copy).expect("the copy parses");
        assert_eq!(again.glyph_count(), 6);
        assert_eq!(again.glyph("space"), Some(3));
        assert_eq!(again.built_in(b' '), Some(3));
        assert_eq!(again.width(3), Some(200.0));
        assert!(again.draws_nothing(3));
        assert_eq!(again.name(4), Some("g7"));
        assert_eq!(again.built_in(7), Some(4));
        assert_eq!(again.width(4), Some(0.0));
        assert_eq!(again.glyph("quoteright"), Some(5));
        assert_eq!(again.built_in(0x27), None);
        // What was there stays.
        assert_eq!(again.width(1), Some(520.0));
        assert_eq!(again.width(2), Some(600.0));
        assert!(!again.draws_nothing(1));
        assert_eq!(again.built_in(b'a'), Some(1));
        assert_eq!(again.built_in(b'b'), Some(2));
        // Added again: the glyph comes after those encoded, the others move up.
        let more = again
            .edited(
                &[],
                &[Blank {
                    name: "period",
                    width: 250.0,
                    code: Some(b'.'),
                }],
            )
            .unwrap();
        let more = Cff::parse(&more).unwrap();
        assert_eq!(more.built_in(b' '), Some(3));
        assert_eq!(more.built_in(b'.'), Some(5));
        assert_eq!(more.name(4), Some("g7"));
        assert_eq!(more.glyph("quoteright"), Some(6));
        assert_eq!(more.width(6), Some(250.0));
    }

    #[test]
    fn a_coded_blank_glyph_comes_after_the_glyphs_encoded() {
        // Format 0 encoding only `a`; format 1, one range for `a` and `b`; and a supplement
        // giving `b` the code `B`, kept.
        for encoding in [
            &[0, 1, b'a'][..],
            &[1, 1, b'a', 1],
            &[0x80, 1, b'a', 1, b'B', 0, 0],
        ] {
            let supplement = encoding[0] & 0x80 != 0;
            let mut encoding = encoding.to_vec();
            if supplement {
                let b = super::string_id(b"b", &mut Vec::new())
                    .unwrap()
                    .to_be_bytes();
                encoding[5..7].copy_from_slice(&b);
            }
            let data = test_program(Some(&encoding));
            let cff = Cff::parse(&data).unwrap();
            let copy = cff
                .edited(
                    &[],
                    &[Blank {
                        name: "space",
                        width: 200.0,
                        code: Some(b' '),
                    }],
                )
                .unwrap();
            let again = Cff::parse(&copy).unwrap();
            let encoded = if encoding[0] & 0x7F == 1 { 2 } else { 1 };
            let space = encoded + 1;
            assert_eq!(again.glyph("space"), Some(space), "{encoding:?}");
            assert_eq!(again.built_in(b' '), Some(space));
            assert_eq!(again.built_in(b'a'), Some(1));
            assert_eq!(again.glyph("b"), Some(if encoded == 1 { 3 } else { 2 }));
            assert_eq!(again.width(again.glyph("b").unwrap()), Some(600.0));
            if supplement {
                assert_eq!(again.built_in(b'B'), again.glyph("b"));
            }
        }
    }

    #[test]
    fn a_blank_glyph_is_refused_where_it_cannot_be_read() {
        let own = test_program(Some(TEST_ENCODING));
        let own = Cff::parse(&own).unwrap();
        // A name the font has.
        let taken = Blank {
            name: "a",
            width: 0.0,
            code: None,
        };
        assert!(own.edited(&[], &[taken]).is_none());
        // Standard Encoding reads the space by its name, at its code only.
        let standard = test_program(None);
        let standard = Cff::parse(&standard).unwrap();
        let space = |code| Blank {
            name: "space",
            width: 200.0,
            code: Some(code),
        };
        let copy = standard.edited(&[], &[space(b' ')]).unwrap();
        let again = Cff::parse(&copy).unwrap();
        assert_eq!(again.built_in(b' '), Some(3));
        assert_eq!(again.built_in(b'a'), Some(1));
        assert!(standard.edited(&[], &[space(b'!')]).is_none());
    }

    #[test]
    fn a_foxit_font_takes_a_blank_glyph() {
        let cff = Cff::parse(Standard::TimesRoman.program()).unwrap();
        let glyphs = u16::try_from(cff.glyph_count()).unwrap();
        let copy = cff
            .edited(
                &[],
                &[Blank {
                    name: "uni2002",
                    width: 500.0,
                    code: None,
                }],
            )
            .unwrap();
        let again = Cff::parse(&copy).unwrap();
        assert_eq!(again.glyph("uni2002"), Some(glyphs));
        for gid in 0..glyphs {
            assert_eq!(again.name(gid), cff.name(gid));
            assert_eq!(again.width(gid), cff.width(gid));
        }
        for code in 0..=255u8 {
            assert_eq!(again.built_in(code), cff.built_in(code));
        }
    }
}
