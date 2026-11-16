//! `ToUnicode` CMaps (ISO 32000-1, 9.10.3).

use std::fmt::Write as _;

/// How many mappings one `beginbfchar` or `beginbfrange` block may hold.
const BLOCK: usize = 100;

/// What PDF/A allows a code to map to: some text, without U+0000, U+FEFF or U+FFFE
/// (ISO 19005-3, 6.2.11.7.2).
pub(crate) fn allowed(text: &str) -> bool {
    !text.is_empty()
        && !text
            .chars()
            .any(|c| matches!(c, '\u{0}' | '\u{FEFF}' | '\u{FFFE}'))
}

/// The Unicode text of a glyph name, by the Adobe Glyph List and the ITC Zapf Dingbats list,
/// `uniXXXX` and `uXXXX[XX]` names included. `None` when the name says nothing, or says
/// something PDF/A forbids.
pub(crate) fn unicode_of(name: &str) -> Option<String> {
    let text: String = read_fonts::ps::agl::name_to_chars(name).collect();
    allowed(&text).then_some(text)
}

fn hex(out: &mut String, width: usize, code: u32) {
    let _ = match width {
        1 => write!(out, "<{code:02X}>"),
        _ => write!(out, "<{code:04X}>"),
    };
}

fn utf16(out: &mut String, text: &str) {
    out.push('<');
    for unit in text.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
}

/// A run of codes counting up, whose texts are one UTF-16 unit each, counting up too.
struct Run {
    first: u32,
    last: u32,
    unit: u16,
}

/// A CMap mapping each code of `width` bytes (1 or 2) to its text. `mappings` are sorted by
/// code. Runs of codes and texts counting up together go in `bfrange` blocks, the rest in
/// `bfchar` blocks.
pub(crate) fn cmap(width: usize, mappings: &[(u32, String)]) -> Vec<u8> {
    let mut runs: Vec<Run> = Vec::new();
    let mut singles: Vec<(u32, &str)> = Vec::new();
    let mut open: Option<Run> = None;
    for (code, text) in mappings {
        let mut units = text.encode_utf16();
        let unit = match (units.next(), units.next()) {
            (Some(unit), None) => Some(unit),
            _ => None,
        };
        let extends = |run: &Run| {
            unit.is_some_and(|unit| {
                run.last.checked_add(1) == Some(*code)
                    // Only the last byte of the code and of the text may change in a range.
                    && run.first >> 8 == *code >> 8
                    && u32::from(run.unit & 0xFF) + (*code - run.first) <= 0xFF
                    && u32::from(run.unit) + (*code - run.first) == u32::from(unit)
            })
        };
        match open.take() {
            Some(mut run) if extends(&run) => {
                run.last = *code;
                open = Some(run);
                continue;
            }
            Some(run) => runs.push(run),
            None => {}
        }
        match unit {
            Some(unit) => {
                open = Some(Run {
                    first: *code,
                    last: *code,
                    unit,
                });
            }
            None => singles.push((*code, text)),
        }
    }
    runs.extend(open);
    // A run of one code is a single mapping.
    let mut chars: Vec<(u32, String)> = singles
        .into_iter()
        .map(|(code, text)| (code, text.to_owned()))
        .collect();
    let mut ranges = Vec::new();
    for run in runs {
        if run.first == run.last {
            chars.push((run.first, String::from_utf16_lossy(&[run.unit])));
        } else {
            ranges.push(run);
        }
    }
    chars.sort_by_key(|(code, _)| *code);

    let mut out = String::new();
    out.push_str(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n",
    );
    hex(&mut out, width, 0);
    out.push(' ');
    hex(&mut out, width, if width == 1 { 0xFF } else { 0xFFFF });
    out.push_str("\nendcodespacerange\n");
    for block in chars.chunks(BLOCK) {
        let _ = writeln!(out, "{} beginbfchar", block.len());
        for (code, text) in block {
            hex(&mut out, width, *code);
            out.push(' ');
            utf16(&mut out, text);
            out.push('\n');
        }
        out.push_str("endbfchar\n");
    }
    for block in ranges.chunks(BLOCK) {
        let _ = writeln!(out, "{} beginbfrange", block.len());
        for run in block {
            hex(&mut out, width, run.first);
            out.push(' ');
            hex(&mut out, width, run.last);
            let _ = writeln!(out, " <{:04X}>", run.unit);
        }
        out.push_str("endbfrange\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{cmap, unicode_of};

    #[test]
    fn names_to_text() {
        assert_eq!(unicode_of("eacute").as_deref(), Some("é"));
        assert_eq!(unicode_of("uni20AC").as_deref(), Some("€"));
        assert_eq!(unicode_of("a20").as_deref(), Some("\u{2714}"));
        assert_eq!(unicode_of("bullet").as_deref(), Some("\u{2022}"));
        assert_eq!(unicode_of("g123"), None);
        assert_eq!(unicode_of(".notdef"), None);
    }

    #[test]
    fn surrogate_pairs_and_blocks() {
        let mappings: Vec<(u32, String)> =
            (0..=150u32).map(|c| (c, "\u{1F600}".to_owned())).collect();
        let text = String::from_utf8(cmap(1, &mappings)).unwrap();
        assert!(text.contains("<96> <D83DDE00>"));
        assert!(text.contains("100 beginbfchar") && text.contains("51 beginbfchar"));
        assert!(!text.contains("bfrange"));
    }

    #[test]
    fn runs_become_ranges_within_the_last_byte() {
        let mut mappings: Vec<(u32, String)> = Vec::new();
        // a..e on codes 0x0101..0x0105: one range.
        for (i, c) in ('a'..='e').enumerate() {
            mappings.push((0x0101 + u32::try_from(i).unwrap(), c.to_string()));
        }
        // A ligature breaks a run; codes crossing 0x01FF/0x0200 start another.
        mappings.push((0x0106, "fi".to_owned()));
        mappings.push((0x01FF, "x".to_owned()));
        mappings.push((0x0200, "y".to_owned()));
        let text = String::from_utf8(cmap(2, &mappings)).unwrap();
        assert!(text.contains("<0000> <FFFF>"));
        assert!(text.contains("<0101> <0105> <0061>"), "{text}");
        assert!(text.contains("<0106> <00660069>"));
        assert!(text.contains("<01FF> <0078>") && text.contains("<0200> <0079>"));
        assert!(text.contains("3 beginbfchar") && text.contains("1 beginbfrange"));
    }

    #[test]
    fn written_maps_read_back() {
        let mappings: Vec<(u32, String)> = vec![
            (0x20, " ".to_owned()),
            (0x41, "A".to_owned()),
            (0x42, "B".to_owned()),
            (0x43, "C".to_owned()),
            (0xFE, "\u{FB01}".to_owned()),
        ];
        let data = cmap(1, &mappings);
        let read = hayro_cmap::CMap::parse(&data, |_| None).unwrap();
        for (code, text) in &mappings {
            let value = match read.lookup_bf_string(*code).unwrap() {
                hayro_cmap::BfString::Char(c) => c.to_string(),
                hayro_cmap::BfString::String(s) => s,
            };
            assert_eq!(&value, text);
        }
        assert!(read.lookup_bf_string(0x44).is_none());
    }
}
