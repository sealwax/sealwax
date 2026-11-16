//! Type 1 font programs (`FontFile`; Adobe Type 1 Font Format, 1990): read enough to know each
//! glyph's name and advance width, and write a copy with other widths or with blank glyphs added,
//! what PDF/A asks of a program (ISO 19005-3, 6.2.11.4.1 and 6.2.11.5), as [`super::cff`] does for
//! CFF.
//!
//! A program has three parts: a clear text one, up to `eexec`; the private dictionary and the
//! charstrings, encrypted (`eexec`, then each charstring again); and a trailer of zeros. A
//! program in the PFB format (segments with headers, which PDF/A forbids) is read too, and
//! written without its headers.

use std::collections::HashMap;
use std::ops::Range;

use super::cff::Blank;
use super::encodings;

const EEXEC_KEY: u16 = 55665;
const CHARSTRING_KEY: u16 = 4330;
const HSBW: u8 = 13;
const ENDCHAR: u8 = 14;
const ESCAPE: u8 = 12;
const SBW: u8 = 7;
const DIV: u8 = 12;
/// More glyphs than any font has: the charstrings of a damaged program end there.
const MAX_GLYPHS: usize = 65_535;

/// A Type 1 program, decrypted.
#[derive(Debug)]
pub(crate) struct Type1 {
    clear: Vec<u8>,
    /// The `eexec` part decrypted, its four leading bytes included.
    private: Vec<u8>,
    trailer: Vec<u8>,
    /// The bytes a charstring starts with before its instructions (`lenIV`), `None` when
    /// charstrings are not encrypted.
    len_iv: Option<usize>,
    /// Glyph space units per text space unit (the font matrix's first value).
    scale: f64,
    glyphs: Vec<Glyph>,
    by_name: HashMap<String, u16>,
    /// The name of each code in the built-in encoding.
    encoding: [Option<String>; 256],
    /// The built-in encoding is Standard Encoding.
    standard_encoding: bool,
    /// Where the number of charstrings is given (`/CharStrings 230 dict`), and that number.
    count_at: Range<usize>,
    declared: usize,
    /// Read from the PFB format, which PDF/A does not allow.
    pfb: bool,
    /// Where entries can be added: after the last charstring.
    entries_end: usize,
    /// The tokens around a charstring's bytes, as the font writes them (`RD`, `ND`).
    read_token: Vec<u8>,
    define_token: Vec<u8>,
}

#[derive(Debug)]
struct Glyph {
    name: String,
    /// Where the length of the charstring is written.
    length_at: Range<usize>,
    /// The charstring's bytes, encrypted as the font has them.
    data: Range<usize>,
    /// The charstring decrypted, without its leading bytes.
    code: Vec<u8>,
}

/// A program written: its bytes and the lengths of its three parts (`/Length1`, `/Length2`,
/// `/Length3`).
#[derive(Debug)]
pub(crate) struct Written {
    pub(crate) bytes: Vec<u8>,
    pub(crate) lengths: [usize; 3],
}

fn crypt(data: &[u8], key: u16, decrypting: bool) -> Vec<u8> {
    let mut r = key;
    data.iter()
        .map(|&byte| {
            let out = byte ^ (r >> 8) as u8;
            let cipher = if decrypting { byte } else { out };
            r = u16::from(cipher)
                .wrapping_add(r)
                .wrapping_mul(52845)
                .wrapping_add(22719);
            out
        })
        .collect()
}

fn is_white(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | 0)
}

fn is_delimiter(byte: u8) -> bool {
    is_white(byte) || b"()<>[]{}/%".contains(&byte)
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// The token at `at` (after white space): its range.
fn token(data: &[u8], mut at: usize) -> Option<Range<usize>> {
    while is_white(*data.get(at)?) {
        at += 1;
    }
    let start = at;
    at += 1;
    while data.get(at).is_some_and(|b| !is_delimiter(*b)) {
        at += 1;
    }
    Some(start..at)
}

fn integer(data: &[u8], range: Range<usize>) -> Option<i64> {
    std::str::from_utf8(data.get(range)?).ok()?.parse().ok()
}

/// The three parts of a program: clear text, encrypted, trailer.
fn parts(data: &[u8], lengths: [Option<usize>; 3]) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if data.first() == Some(&0x80) {
        // PFB: segments `0x80 type length(4, little endian)`, 1 text, 2 binary, 3 the end.
        let (mut clear, mut binary, mut trailer) = (Vec::new(), Vec::new(), Vec::new());
        let mut at = 0;
        while let [0x80, kind, rest @ ..] = data.get(at..)? {
            if *kind == 3 {
                break;
            }
            let length =
                usize::try_from(u32::from_le_bytes(rest.get(..4)?.try_into().ok()?)).ok()?;
            let segment = rest.get(4..4 + length)?;
            match (*kind, binary.is_empty()) {
                (1, true) => clear.extend_from_slice(segment),
                (1, false) => trailer.extend_from_slice(segment),
                (2, _) if trailer.is_empty() => binary.extend_from_slice(segment),
                _ => return None,
            }
            at += 6 + length;
        }
        return Some((clear, binary, trailer));
    }
    let eexec = find(data, b"eexec", 0)?;
    let mut start = eexec + b"eexec".len();
    while data
        .get(start)
        .is_some_and(|b| matches!(b, b'\r' | b'\n' | b' ' | b'\t'))
    {
        start += 1;
    }
    let clear = data.get(..start)?.to_vec();
    let end = match lengths {
        [_, Some(binary), _] if start + binary <= data.len() && binary > 4 => start + binary,
        // Without its length, the encrypted part ends where its text does, after `closefile`
        // and the line break that follows: the zeros of the trailer are not encrypted.
        _ => {
            let plain = crypt(data.get(start..)?, EEXEC_KEY, true);
            let close = find(&plain, b"closefile", 0)? + b"closefile".len();
            let ends_line = plain.get(close).is_some_and(|b| matches!(b, b'\r' | b'\n'));
            start + close + usize::from(ends_line)
        }
    };
    let mut binary = data.get(start..end)?.to_vec();
    // An encrypted part written in hexadecimal is read as its bytes.
    if binary.len() >= 4 && binary[..4].iter().all(u8::is_ascii_hexdigit) {
        let digits: Vec<u8> = binary.iter().copied().filter(|b| !is_white(*b)).collect();
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        binary = digits
            .chunks(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap_or("0");
                u8::from_str_radix(text, 16).unwrap_or(0) << if pair.len() == 1 { 4 } else { 0 }
            })
            .collect();
    }
    Some((clear, binary, data.get(end..)?.to_vec()))
}

/// A charstring number (Type 1, 6.2): its value and length.
fn number(code: &[u8], at: usize) -> Option<(f64, usize)> {
    let first = *code.get(at)?;
    match first {
        32..=246 => Some((f64::from(i16::from(first) - 139), 1)),
        247..=250 => {
            let second = *code.get(at + 1)?;
            Some((
                f64::from((i16::from(first) - 247) * 256 + i16::from(second) + 108),
                2,
            ))
        }
        251..=254 => {
            let second = *code.get(at + 1)?;
            Some((
                f64::from(-(i16::from(first) - 251) * 256 - i16::from(second) - 108),
                2,
            ))
        }
        255 => {
            let bytes: [u8; 4] = code.get(at + 1..at + 5)?.try_into().ok()?;
            Some((f64::from(i32::from_be_bytes(bytes)), 5))
        }
        _ => None,
    }
}

fn encode_number(value: i32) -> Vec<u8> {
    match value {
        -107..=107 => vec![(value + 139) as u8],
        108..=1131 => {
            let v = value - 108;
            vec![(v / 256 + 247) as u8, (v % 256) as u8]
        }
        -1131..=-108 => {
            let v = -value - 108;
            vec![(v / 256 + 251) as u8, (v % 256) as u8]
        }
        _ => {
            let mut bytes = vec![255];
            bytes.extend_from_slice(&value.to_be_bytes());
            bytes
        }
    }
}

/// The width a charstring sets (`hsbw` or `sbw`, its first instruction), in glyph space units,
/// and where its instructions after it start. Numbers and `div` may come before it.
fn width_of(code: &[u8]) -> Option<(f64, f64, usize)> {
    let mut stack: Vec<f64> = Vec::new();
    let mut at = 0;
    loop {
        let byte = *code.get(at)?;
        if byte >= 32 {
            let (value, length) = number(code, at)?;
            stack.push(value);
            at += length;
            continue;
        }
        match (byte, code.get(at + 1)) {
            (HSBW, _) => {
                let [side, width] = stack.get(stack.len().checked_sub(2)?..)? else {
                    return None;
                };
                return Some((*side, *width, at + 1));
            }
            (ESCAPE, Some(&SBW)) => {
                let [side, _, width, _] = stack.get(stack.len().checked_sub(4)?..)? else {
                    return None;
                };
                return Some((*side, *width, at + 2));
            }
            (ESCAPE, Some(&DIV)) => {
                let divisor = stack.pop()?;
                let dividend = stack.pop()?;
                if divisor == 0.0 {
                    return None;
                }
                stack.push(dividend / divisor);
                at += 2;
            }
            _ => return None,
        }
    }
}

impl Type1 {
    /// Read `data`, the parts of the stream's `/Length1`, `/Length2` and `/Length3` if given.
    /// `None` for a program this module cannot read.
    pub(crate) fn parse(data: &[u8], lengths: [Option<usize>; 3]) -> Option<Self> {
        let (clear, binary, trailer) = parts(data, lengths)?;
        if binary.len() < 4 {
            return None;
        }
        let private = crypt(&binary, EEXEC_KEY, true);
        let len_iv = match find(&private, b"/lenIV", 0) {
            Some(at) => match integer(&private, token(&private, at + 6)?)? {
                n if n < 0 => None,
                n => Some(usize::try_from(n).ok()?),
            },
            None => Some(4),
        };
        let scale = find(&clear, b"/FontMatrix", 0)
            .and_then(|at| {
                let open = find(&clear, b"[", at)?;
                let first = token(&clear, open + 1)?;
                std::str::from_utf8(clear.get(first)?)
                    .ok()?
                    .parse::<f64>()
                    .ok()
            })
            .filter(|scale| scale.is_finite() && *scale > 0.0)
            .unwrap_or(0.001);
        let (encoding, standard_encoding) = built_in_encoding(&clear);
        // The charstrings: `/CharStrings n dict dup begin`, then `/name length RD bytes ND`.
        let mut at = find(&private, b"/CharStrings", 0)? + b"/CharStrings".len();
        let count_at = token(&private, at)?;
        let declared = usize::try_from(integer(&private, count_at.clone())?).ok()?;
        at = count_at.end;
        // Up to the first entry.
        let mut glyphs = Vec::new();
        let (mut read_token, mut define_token) = (Vec::new(), Vec::new());
        loop {
            let next = token(&private, at)?;
            if private.get(next.start) == Some(&b'/') {
                at = next.start;
                break;
            }
            at = next.end;
        }
        let mut entries_end = at;
        while glyphs.len() < MAX_GLYPHS {
            let Some(name) = token(&private, at) else {
                break;
            };
            if private.get(name.start) != Some(&b'/') {
                break;
            }
            let glyph_name =
                String::from_utf8(private.get(name.start + 1..name.end)?.to_vec()).ok()?;
            let length_at = token(&private, name.end)?;
            let length = usize::try_from(integer(&private, length_at.clone())?).ok()?;
            let read = token(&private, length_at.end)?;
            // One space after the `RD` token, then the bytes.
            let start = read.end + 1;
            let data = start..start + length;
            let encrypted = private.get(data.clone())?;
            let code = match len_iv {
                Some(skip) => crypt(encrypted, CHARSTRING_KEY, true).get(skip..)?.to_vec(),
                None => encrypted.to_vec(),
            };
            if read_token.is_empty() {
                read_token = private.get(read.clone())?.to_vec();
            }
            // What defines the entry (`ND`, `|-`, `noaccess def`): up to the next name or `end`.
            let mut end = data.end;
            while let Some(next) = token(&private, end) {
                if private.get(next.start) == Some(&b'/')
                    || private.get(next.clone()) == Some(b"end")
                {
                    break;
                }
                end = next.end;
            }
            if define_token.is_empty() {
                define_token = private.get(data.end..end)?.to_vec();
            }
            glyphs.push(Glyph {
                name: glyph_name,
                length_at,
                data,
                code,
            });
            at = end;
            entries_end = end;
        }
        if glyphs.is_empty() {
            return None;
        }
        let by_name = glyphs
            .iter()
            .enumerate()
            .filter_map(|(index, glyph)| Some((glyph.name.clone(), u16::try_from(index).ok()?)))
            .collect();
        Some(Self {
            clear,
            private,
            trailer,
            len_iv,
            scale,
            glyphs,
            by_name,
            encoding,
            standard_encoding,
            count_at,
            declared,
            pfb: data.first() == Some(&0x80),
            entries_end,
            read_token,
            define_token,
        })
    }

    /// Whether the program must be written again even with nothing to change: it is in the PFB
    /// format, or announces another number of charstrings than it has (validators refuse both).
    pub(crate) fn needs_rewriting(&self) -> bool {
        self.pfb || self.declared != self.glyphs.len()
    }

    pub(crate) fn glyph(&self, name: &str) -> Option<u16> {
        self.by_name.get(name).copied()
    }

    /// The glyph of `code` in the program's own encoding.
    pub(crate) fn built_in(&self, code: u8) -> Option<u16> {
        self.glyph(self.encoding[usize::from(code)].as_deref()?)
    }

    /// The advance width of glyph `gid`, in text space units per 1000 em.
    pub(crate) fn width(&self, gid: u16) -> Option<f64> {
        let (_, width, _) = width_of(&self.glyphs.get(usize::from(gid))?.code)?;
        Some(width * (self.scale * 1000.0))
    }

    /// Whether glyph `gid` draws nothing: its width, then `endchar`.
    pub(crate) fn draws_nothing(&self, gid: u16) -> bool {
        let Some(glyph) = self.glyphs.get(usize::from(gid)) else {
            return false;
        };
        width_of(&glyph.code).is_some_and(|(_, _, at)| glyph.code.get(at) == Some(&ENDCHAR))
    }

    /// The charstring `code`, encrypted as the font's are.
    fn encrypted(&self, code: &[u8]) -> Vec<u8> {
        match self.len_iv {
            Some(skip) => {
                let mut plain = vec![0; skip];
                plain.extend_from_slice(code);
                crypt(&plain, CHARSTRING_KEY, false)
            }
            None => code.to_vec(),
        }
    }

    /// A copy of the program with the advance widths of `widths` (text space units per 1000 em)
    /// and the glyphs of `blanks` added, without PFB headers. `None` when a width cannot be
    /// changed, or a blank needs a code of a built-in encoding the program defines itself.
    pub(crate) fn edited(&self, widths: &[(u16, f64)], blanks: &[Blank<'_>]) -> Option<Written> {
        let units = |width: f64| -> Option<i32> {
            let value = (width / (self.scale * 1000.0)).round();
            (value.is_finite() && value.abs() < f64::from(i32::MAX)).then_some(value as i32)
        };
        let mut codes: HashMap<usize, Vec<u8>> = HashMap::new();
        for (gid, width) in widths {
            let glyph = self.glyphs.get(usize::from(*gid))?;
            let (side, _, at) = width_of(&glyph.code)?;
            // `sbw` (vertical metrics too) is left as it is, as is a fractional side bearing.
            if glyph.code.get(at - 1) != Some(&HSBW) || side.fract() != 0.0 {
                return None;
            }
            let mut code = encode_number(side as i32);
            code.extend(encode_number(units(*width)?));
            code.push(HSBW);
            code.extend_from_slice(glyph.code.get(at..)?);
            codes.insert(usize::from(*gid), code);
        }
        // A blank's code is that of Standard Encoding: only a program with that encoding gives
        // it the code without its encoding being changed.
        if blanks.iter().any(|blank| blank.code.is_some()) && !self.standard_encoding {
            return None;
        }
        let mut private = Vec::with_capacity(self.private.len());
        let count = self.glyphs.len() + blanks.len();
        private.extend_from_slice(self.private.get(..self.count_at.start)?);
        private.extend_from_slice(count.to_string().as_bytes());
        let mut cursor = self.count_at.end;
        for (index, glyph) in self.glyphs.iter().enumerate() {
            let Some(code) = codes.get(&index) else {
                continue;
            };
            let bytes = self.encrypted(code);
            private.extend_from_slice(self.private.get(cursor..glyph.length_at.start)?);
            private.extend_from_slice(bytes.len().to_string().as_bytes());
            private.extend_from_slice(self.private.get(glyph.length_at.end..glyph.data.start)?);
            private.extend_from_slice(&bytes);
            cursor = glyph.data.end;
        }
        private.extend_from_slice(self.private.get(cursor..self.entries_end)?);
        for blank in blanks {
            let mut code = encode_number(0);
            code.extend(encode_number(units(blank.width)?));
            code.extend([HSBW, ENDCHAR]);
            let bytes = self.encrypted(&code);
            private.push(b'\n');
            private.push(b'/');
            private.extend_from_slice(blank.name.as_bytes());
            private.push(b' ');
            private.extend_from_slice(bytes.len().to_string().as_bytes());
            private.push(b' ');
            private.extend_from_slice(&self.read_token);
            private.push(b' ');
            private.extend_from_slice(&bytes);
            private.extend_from_slice(&self.define_token);
        }
        private.extend_from_slice(self.private.get(self.entries_end..)?);
        let binary = crypt(&private, EEXEC_KEY, false);
        let mut bytes = self.clear.clone();
        bytes.extend_from_slice(&binary);
        bytes.extend_from_slice(&self.trailer);
        Some(Written {
            bytes,
            lengths: [self.clear.len(), binary.len(), self.trailer.len()],
        })
    }
}

/// The program's built-in encoding: Standard Encoding, or the `dup code /name put` of its own;
/// and whether it is Standard Encoding.
fn built_in_encoding(clear: &[u8]) -> ([Option<String>; 256], bool) {
    let mut table: [Option<String>; 256] = std::array::from_fn(|_| None);
    let Some(at) = find(clear, b"/Encoding", 0) else {
        return (table, false);
    };
    let standard = token(clear, at + b"/Encoding".len())
        .and_then(|next| clear.get(next))
        .is_some_and(|word| word == b"StandardEncoding");
    if standard {
        for (code, name) in encodings::STANDARD.iter().enumerate() {
            if !name.is_empty() {
                table[code] = Some((*name).to_owned());
            }
        }
        return (table, true);
    }
    // The `def` that ends the array: the word, not `.notdef`.
    let mut end = at;
    let end = loop {
        match find(clear, b"def", end) {
            Some(found)
                if found > 0
                    && clear.get(found - 1).copied().is_some_and(is_white)
                    && clear.get(found + 3).is_none_or(|b| is_delimiter(*b)) =>
            {
                break found;
            }
            Some(found) => end = found + 3,
            None => break clear.len(),
        }
    };
    let mut cursor = at;
    while let Some(dup) = find(clear, b"dup ", cursor).filter(|dup| *dup < end) {
        cursor = dup + 4;
        let Some(code) = token(clear, cursor) else {
            continue;
        };
        let Some(name) = token(clear, code.end) else {
            continue;
        };
        let code = integer(clear, code).and_then(|c| u8::try_from(c).ok());
        let name = clear
            .get(name)
            .and_then(|n| n.strip_prefix(b"/"))
            .and_then(|n| String::from_utf8(n.to_vec()).ok());
        if let (Some(code), Some(name)) = (code, name) {
            table[usize::from(code)] = Some(name);
        }
    }
    (table, false)
}

/// A Type 1 program for the tests: glyphs `(name, side bearing, width)` drawing nothing but
/// `.notdef`, a custom or standard encoding, charstrings encrypted with `lenIV` 4.
#[cfg(test)]
pub(crate) fn test_program(
    glyphs: &[(&str, i32, i32)],
    encoding: Option<&[(u8, &str)]>,
) -> Vec<u8> {
    let mut clear =
        b"%!PS-AdobeFont-1.0: Test 001\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n".to_vec();
    match encoding {
        None => clear.extend_from_slice(b"/Encoding StandardEncoding def\n"),
        Some(codes) => {
            clear.extend_from_slice(
                b"/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n",
            );
            for (code, name) in codes {
                clear.extend_from_slice(format!("dup {code} /{name} put\n").as_bytes());
            }
            clear.extend_from_slice(b"readonly def\n");
        }
    }
    clear.extend_from_slice(b"currentfile eexec\n");
    let mut private = b"\x00\x00\x00\x00dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n/lenIV 4 def\n2 index /CharStrings ".to_vec();
    private.extend_from_slice(format!("{} dict dup begin\n", glyphs.len()).as_bytes());
    for (name, side, width) in glyphs {
        let mut code = encode_number(*side);
        code.extend(encode_number(*width));
        code.push(HSBW);
        if *name != ".notdef" {
            // A dot: 0 0 rmoveto 10 0 rlineto closepath.
            code.extend(encode_number(0));
            code.extend(encode_number(0));
            code.push(21);
            code.extend(encode_number(10));
            code.extend(encode_number(0));
            code.push(5);
            code.push(9);
        }
        code.push(ENDCHAR);
        let mut plain = vec![0; 4];
        plain.extend_from_slice(&code);
        let bytes = crypt(&plain, CHARSTRING_KEY, false);
        private.extend_from_slice(format!("/{name} {} RD ", bytes.len()).as_bytes());
        private.extend_from_slice(&bytes);
        private.extend_from_slice(b" ND\n");
    }
    private.extend_from_slice(b"end\nend\nreadonly put\nnoaccess put\ndup /FontName get exch definefont pop\nmark currentfile closefile\n");
    let mut out = clear;
    out.extend_from_slice(&crypt(&private, EEXEC_KEY, false));
    out.extend_from_slice(b"\n");
    for _ in 0..8 {
        out.extend_from_slice(&[b'0'; 64]);
        out.push(b'\n');
    }
    out.extend_from_slice(b"cleartomark\n");
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Blank, Type1, test_program};

    fn glyphs() -> Vec<(&'static str, i32, i32)> {
        vec![(".notdef", 0, 250), ("A", 10, 722), ("space", 0, 278)]
    }

    #[test]
    fn names_widths_and_encodings_are_read() {
        let program = Type1::parse(&test_program(&glyphs(), None), [None; 3]).unwrap();
        let a = program.glyph("A").unwrap();
        assert_eq!(program.width(a), Some(722.0));
        assert_eq!(program.built_in(b'A'), Some(a));
        assert!(program.draws_nothing(program.glyph(".notdef").unwrap()));
        assert!(!program.draws_nothing(a));
        let custom = Type1::parse(&test_program(&glyphs(), Some(&[(1, "A")])), [None; 3]).unwrap();
        assert_eq!(custom.built_in(1), custom.glyph("A"));
        assert_eq!(custom.built_in(b'A'), None);
    }

    #[test]
    fn widths_change_and_blanks_are_added() {
        let original = test_program(&glyphs(), None);
        let program = Type1::parse(&original, [None; 3]).unwrap();
        let a = program.glyph("A").unwrap();
        let blank = Blank {
            name: "quotesingle",
            width: 191.0,
            code: None,
        };
        let written = program.edited(&[(a, 700.0)], &[blank]).unwrap();
        assert_eq!(written.lengths.iter().sum::<usize>(), written.bytes.len());
        let [clear, binary, _] = written.lengths;
        let again = Type1::parse(&written.bytes, [Some(clear), Some(binary), None]).unwrap();
        assert_eq!(again.width(again.glyph("A").unwrap()), Some(700.0));
        assert_eq!(again.width(again.glyph("space").unwrap()), Some(278.0));
        let added = again.glyph("quotesingle").unwrap();
        assert_eq!(again.width(added), Some(191.0));
        assert!(again.draws_nothing(added));
        // The glyph count is that of the charstrings.
        assert_eq!(again.glyphs.len(), 4);
        // read-fonts reads the copy, and finds the same widths.
        let other = read_fonts::ps::type1::Type1Font::new(&written.bytes).unwrap();
        let named: Vec<(String, f32)> = other
            .glyph_names()
            .map(|(gid, name)| {
                let width = other.evaluate_width(gid).unwrap().unwrap().to_f32();
                (name.to_owned(), width)
            })
            .collect();
        assert!(named.contains(&("A".to_owned(), 700.0)), "{named:?}");
        assert!(
            named.contains(&("quotesingle".to_owned(), 191.0)),
            "{named:?}"
        );
    }

    #[test]
    fn a_pfb_program_is_read_and_written_without_its_headers() {
        let raw = test_program(&glyphs(), None);
        let eexec = raw.windows(6).position(|w| w == b"eexec\n").unwrap() + 6;
        let trailer = raw.len() - (8 * 65 + b"cleartomark\n".len()) - 1;
        let mut pfb = Vec::new();
        for (kind, part) in [
            (1u8, &raw[..eexec]),
            (2, &raw[eexec..trailer]),
            (1, &raw[trailer..]),
        ] {
            pfb.extend_from_slice(&[0x80, kind]);
            pfb.extend_from_slice(&u32::try_from(part.len()).unwrap().to_le_bytes());
            pfb.extend_from_slice(part);
        }
        pfb.extend_from_slice(&[0x80, 3]);
        let program = Type1::parse(&pfb, [None; 3]).unwrap();
        assert_eq!(program.width(program.glyph("A").unwrap()), Some(722.0));
        let written = program.edited(&[], &[]).unwrap();
        assert_ne!(written.bytes[0], 0x80);
        assert_eq!(written.bytes, raw);
    }
}
