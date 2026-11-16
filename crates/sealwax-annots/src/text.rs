//! Text drawn with Helvetica in WinAnsiEncoding: what the forms and texts this crate draws use.
//! The PDF/A conversion embeds the program of the standard font (PDFium's Foxit font).

/// Helvetica's advance widths for each WinAnsiEncoding code, in thousandths of the font size:
/// Adobe's metrics (0 for a code the encoding leaves undefined).
const HELVETICA: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
    0, 556, 0, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, 0, 222, 222,
    333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 0, 500, 667, 278, 333, 556, 556, 556, 556,
    260, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333, 333, 333, 556, 537, 278, 333,
    333, 365, 556, 834, 834, 834, 611, 667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667,
    278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667,
    611, 556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278, 556, 556,
    556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

/// The WinAnsiEncoding code of a character, `None` for one the encoding does not have.
pub(crate) fn win_ansi(character: char) -> Option<u8> {
    let code = u32::from(character);
    let special = match character {
        '€' => 0x80,
        '‚' => 0x82,
        'ƒ' => 0x83,
        '„' => 0x84,
        '…' => 0x85,
        '†' => 0x86,
        '‡' => 0x87,
        'ˆ' => 0x88,
        '‰' => 0x89,
        'Š' => 0x8A,
        '‹' => 0x8B,
        'Œ' => 0x8C,
        'Ž' => 0x8E,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '˜' => 0x98,
        '™' => 0x99,
        'š' => 0x9A,
        '›' => 0x9B,
        'œ' => 0x9C,
        'ž' => 0x9E,
        'Ÿ' => 0x9F,
        _ => 0,
    };
    if special != 0 {
        return Some(special);
    }
    match code {
        0x20..=0x7E | 0xA0..=0xFF => u8::try_from(code).ok(),
        _ => None,
    }
}

/// A line of text in WinAnsiEncoding: a character the encoding does not have becomes `?`, and is
/// counted.
pub(crate) fn encode(text: &str, replaced: &mut usize) -> Vec<u8> {
    text.chars()
        .map(|character| {
            win_ansi(character).unwrap_or_else(|| {
                *replaced += 1;
                b'?'
            })
        })
        .collect()
}

/// The width of encoded text at `size` points.
pub(crate) fn width(encoded: &[u8], size: f64) -> f64 {
    encoded
        .iter()
        .map(|code| f64::from(HELVETICA[usize::from(*code)]))
        .sum::<f64>()
        * size
        / 1000.0
}

/// A PDF string literal.
pub(crate) fn literal(encoded: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(encoded.len() + 2);
    out.push(b'(');
    for byte in encoded {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(*byte);
            }
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            _ => out.push(*byte),
        }
    }
    out.push(b')');
    out
}

/// A number in a content stream: at most four decimals, no trailing zero.
pub(crate) fn number(value: f64) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "" | "-0" => "0".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn widths_and_encoding() {
        let mut replaced = 0;
        let encoded = encode("Été €✓", &mut replaced);
        assert_eq!(encoded, [0xC9, b't', 0xE9, b' ', 0x80, b'?']);
        assert_eq!(replaced, 1);
        assert_eq!(width(b"A", 10.0), 6.67);
        assert_eq!(width(&[0x92], 1000.0), 222.0);
    }

    #[test]
    fn literals_and_numbers() {
        assert_eq!(literal(b"a(b)\\c"), b"(a\\(b\\)\\\\c)");
        assert_eq!(number(1.0), "1");
        assert_eq!(number(0.125_00), "0.125");
        assert_eq!(number(-0.000_01), "0");
        assert_eq!(number(69.2185), "69.2185");
    }
}
