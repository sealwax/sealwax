//! PDF text strings (ISO 32000-1, 7.9.2.2): the strings that hold text meant for people, such as
//! field names, file names and the document information.

/// PDFDocEncoding, from 0x80 to 0xA0 (ISO 32000-1, annex D). The other bytes are Latin-1, but
/// for 0x18 to 0x1F (accents) below.
const PDF_DOC_HIGH: [u32; 33] = [
    0x2022, 0x2020, 0x2021, 0x2026, 0x2014, 0x2013, 0x0192, 0x2044, 0x2039, 0x203A, 0x2212, 0x2030,
    0x201E, 0x201C, 0x201D, 0x2018, 0x2019, 0x201A, 0x2122, 0xFB01, 0xFB02, 0x0141, 0x0152, 0x0160,
    0x0178, 0x017D, 0x0131, 0x0142, 0x0153, 0x0161, 0x017E, 0xFFFD, 0x20AC,
];
const PDF_DOC_ACCENTS: [u32; 8] = [
    0x02D8, 0x02C7, 0x02C6, 0x02D9, 0x02DD, 0x02DB, 0x02DA, 0x02DC,
];

/// How a text string is written, as [`decode_text_string`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf16Be,
    /// After `FF FE`, which the standard does not define but MuPDF reads.
    Utf16Le,
    Utf8,
    /// UTF-8 without a byte order mark, which the standard does not allow but many writers use.
    UnmarkedUtf8,
    PdfDoc,
}

/// How `bytes` are written, and the bytes after their byte order mark.
fn encoding(bytes: &[u8]) -> (Encoding, &[u8]) {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return (Encoding::Utf16Be, rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return (Encoding::Utf16Le, rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (Encoding::Utf8, rest);
    }
    // Text past ASCII that is UTF-8, without PDFDocEncoding's accents (which are ASCII control
    // codes, and turn MuPDF away from UTF-8 too).
    let unmarked_utf8 = !bytes.is_ascii()
        && !bytes.iter().any(|b| (0x18..=0x1F).contains(b))
        && std::str::from_utf8(bytes).is_ok();
    if unmarked_utf8 {
        (Encoding::UnmarkedUtf8, bytes)
    } else {
        (Encoding::PdfDoc, bytes)
    }
}

/// The text a PDF text string holds, read as MuPDF reads it: UTF-16BE or UTF-8 after their byte
/// order marks, UTF-16LE after its own, UTF-8 when the bytes are UTF-8 past ASCII and hold none
/// of PDFDocEncoding's accents (0x18 to 0x1F), PDFDocEncoding otherwise. The standard knows
/// neither UTF-16LE nor UTF-8 without a mark, but writers use them, the second often, and a
/// string in PDFDocEncoding is seldom also UTF-8 past ASCII. What does not decode becomes U+FFFD,
/// and the null characters that end it (a C string's terminator, written with it) are dropped.
#[must_use]
pub fn decode_text_string(bytes: &[u8]) -> String {
    let mut text = decoded(bytes);
    text.truncate(text.trim_end_matches('\0').len());
    text
}

fn decoded(bytes: &[u8]) -> String {
    let (encoding, rest) = encoding(bytes);
    let utf16 = |from: fn([u8; 2]) -> u16| -> String {
        let units = rest.as_chunks::<2>().0.iter().map(|pair| from(*pair));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    };
    match encoding {
        Encoding::Utf16Be => utf16(u16::from_be_bytes),
        Encoding::Utf16Le => utf16(u16::from_le_bytes),
        Encoding::Utf8 | Encoding::UnmarkedUtf8 => String::from_utf8_lossy(rest).into_owned(),
        Encoding::PdfDoc => rest
            .iter()
            .map(|&b| {
                let code = match b {
                    0x18..=0x1F => PDF_DOC_ACCENTS[usize::from(b - 0x18)],
                    0x80..=0xA0 => PDF_DOC_HIGH[usize::from(b - 0x80)],
                    0x7F | 0xAD => 0xFFFD,
                    _ => u32::from(b),
                };
                char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
            })
            .collect(),
    }
}

/// Whether a reader that follows the standard to the letter reads `bytes` as
/// [`decode_text_string`] does: not when they are UTF-16LE or UTF-8 without a byte order mark,
/// which it reads in PDFDocEncoding.
#[must_use]
pub fn is_standard_text_string(bytes: &[u8]) -> bool {
    !matches!(
        encoding(bytes).0,
        Encoding::Utf16Le | Encoding::UnmarkedUtf8
    )
}

/// `text` as a PDF text string: its bytes when they are printable ASCII (or tabs and line
/// breaks), which PDFDocEncoding reads the same; UTF-16BE after a byte order mark otherwise.
#[must_use]
pub fn encode_text_string(text: &str) -> Vec<u8> {
    if text
        .bytes()
        .all(|b| (0x20..0x7F).contains(&b) || matches!(b, b'\t' | b'\n' | b'\r'))
    {
        return text.as_bytes().to_vec();
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::{decode_text_string, encode_text_string, is_standard_text_string};

    #[test]
    fn text_strings_in_every_encoding() {
        let cases: [(&[u8], &str, bool); 16] = [
            // A C string's terminator.
            (b"Title\x00\x00", "Title", true),
            (b"\xFE\xFF\x00A\x00\x00", "A", true),
            (b"\xFE\xFF\x00A\x00\xE9", "A\u{e9}", true),
            (b"\xFF\xFEA\x00\xE9\x00", "A\u{e9}", false),
            (b"\xEF\xBB\xBFA\xC3\xA9", "A\u{e9}", true),
            // UTF-8 without a mark.
            (b"Caf\xC3\xA9", "Caf\u{e9}", false),
            (b"l\xE2\x80\x99eau", "l\u{2019}eau", false),
            (b"a\xF0\x9F\x98\x80", "a\u{1F600}", false),
            (b"\x09Caf\xC3\xA9", "\tCaf\u{e9}", false),
            // PDFDocEncoding: ASCII, bytes that are not UTF-8, UTF-8 beside an accent.
            (b"plain", "plain", true),
            (b"\x80 x", "\u{2022} x", true),
            (b"\xE9t\xE9", "\u{e9}t\u{e9}", true),
            (
                b"A\x80\xA0\xE9\x18",
                "A\u{2022}\u{20AC}\u{e9}\u{02D8}",
                true,
            ),
            (b"Caf\xC3\xA9 \xE9", "Caf\u{c3}\u{a9} \u{e9}", true),
            (b"\x18Caf\xC3\xA9", "\u{02D8}Caf\u{c3}\u{a9}", true),
            (b"a\xC0\x80b", "a\u{c0}\u{2022}b", true),
        ];
        for (bytes, text, standard) in cases {
            assert_eq!(decode_text_string(bytes), text, "{bytes:02X?}");
            assert_eq!(is_standard_text_string(bytes), standard, "{bytes:02X?}");
        }
        assert_eq!(encode_text_string("Signature1"), b"Signature1");
        assert_eq!(encode_text_string("\u{e9}"), [0xFE, 0xFF, 0x00, 0xE9]);
        assert_eq!(encode_text_string("a\u{1}"), [0xFE, 0xFF, 0, b'a', 0, 1]);
        for text in ["Sign\u{e9}", "\u{65e5}\u{672c}", "plain", "a\u{1F600}b"] {
            assert_eq!(decode_text_string(&encode_text_string(text)), text);
        }
    }
}
