//! A light reading of content streams: where each operator and its operands lie, so that a
//! stream can be rewritten around some of them before hayro interprets it.

/// An operator of a content stream: from its first operand to its end.
pub(crate) struct Op {
    pub(crate) start: usize,
    pub(crate) operator: std::ops::Range<usize>,
    pub(crate) end: usize,
}

fn is_white(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' | b'\0')
}

fn is_regular(b: u8) -> bool {
    !is_white(b) && !b"()<>[]{}/%".contains(&b)
}

fn skip_string(data: &[u8], mut at: usize) -> usize {
    let mut depth = 0usize;
    while at < data.len() {
        match data[at] {
            b'\\' => at += 1,
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return at + 1;
                }
            }
            _ => {}
        }
        at += 1;
    }
    data.len()
}

/// The end of an inline image whose `BI` ends at `at`: past `EI`, the first one standing alone
/// after the data.
fn inline_image_end(data: &[u8], at: usize) -> usize {
    let mut cursor = at;
    while cursor + 1 < data.len() {
        if data[cursor] == b'I'
            && data[cursor + 1] == b'D'
            && cursor > 0
            && !is_regular(data[cursor - 1])
            && data.get(cursor + 2).is_none_or(|b| !is_regular(*b))
        {
            break;
        }
        cursor += 1;
    }
    cursor += 3;
    while cursor + 1 < data.len() {
        if data[cursor] == b'E'
            && data[cursor + 1] == b'I'
            && is_white(data[cursor - 1])
            && data.get(cursor + 2).is_none_or(|b| is_white(*b))
        {
            return cursor + 2;
        }
        cursor += 1;
    }
    data.len()
}

/// The operators of `data`.
pub(crate) fn operators(data: &[u8]) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut operands: Option<usize> = None;
    // A word inside an array is an operand, as readers take it: `[(a) 0 Tc (b)] TJ` is one `TJ`.
    let mut depth = 0usize;
    let mut at = 0;
    loop {
        while at < data.len() && is_white(data[at]) {
            at += 1;
        }
        if data.get(at) == Some(&b'%') {
            while at < data.len() && !matches!(data[at], b'\r' | b'\n') {
                at += 1;
            }
            continue;
        }
        let Some(&b) = data.get(at) else {
            break;
        };
        let start = at;
        let mut operator = false;
        match b {
            b'(' => at = skip_string(data, at),
            b'<' if data.get(at + 1) == Some(&b'<') => at += 2,
            b'<' => {
                at = data[at..]
                    .iter()
                    .position(|c| *c == b'>')
                    .map_or(data.len(), |p| at + p + 1);
            }
            b'>' if data.get(at + 1) == Some(&b'>') => at += 2,
            b'[' => {
                depth += 1;
                at += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                at += 1;
            }
            b'/' => {
                at += 1;
                while at < data.len() && is_regular(data[at]) {
                    at += 1;
                }
            }
            _ if !is_regular(b) => at += 1,
            _ => {
                while at < data.len() && is_regular(data[at]) {
                    at += 1;
                }
                let word = &data[start..at];
                operator = depth == 0
                    && !(b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'))
                    && !matches!(word, b"true" | b"false" | b"null");
            }
        }
        if !operator {
            operands.get_or_insert(start);
            continue;
        }
        let word = start..at;
        if &data[word.clone()] == b"BI" {
            at = inline_image_end(data, at);
        }
        ops.push(Op {
            start: operands.take().unwrap_or(start),
            operator: word,
            end: at,
        });
    }
    ops
}

/// How deep arrays and dictionaries are followed in [`object_end`]: past that, the rest of the
/// data is taken as the object.
const MAX_NESTING: usize = 64;

/// Past the white space and comments from `at`.
fn skip_white(data: &[u8], mut at: usize) -> usize {
    while at < data.len() {
        if is_white(data[at]) {
            at += 1;
        } else if data[at] == b'%' {
            while at < data.len() && !matches!(data[at], b'\r' | b'\n') {
                at += 1;
            }
        } else {
            break;
        }
    }
    at
}

/// The end of the object at `at` (after white space): a string, a dictionary or an array whole,
/// else a name, a number or a keyword; a stray delimiter is an object of one byte.
pub(crate) fn object_end(data: &[u8], at: usize) -> usize {
    nested_end(data, skip_white(data, at), 0)
}

fn nested_end(data: &[u8], mut at: usize, depth: usize) -> usize {
    let Some(&first) = data.get(at) else {
        return data.len();
    };
    let close: &[u8] = match first {
        b'(' => return skip_string(data, at),
        b'<' if data.get(at + 1) == Some(&b'<') => {
            at += 2;
            b">>"
        }
        b'<' => {
            return data[at..]
                .iter()
                .position(|b| *b == b'>')
                .map_or(data.len(), |p| at + p + 1);
        }
        b'[' => {
            at += 1;
            b"]"
        }
        b'/' => {
            at += 1;
            while at < data.len() && is_regular(data[at]) {
                at += 1;
            }
            return at;
        }
        b if is_regular(b) => {
            while at < data.len() && is_regular(data[at]) {
                at += 1;
            }
            return at;
        }
        _ => return at + 1,
    };
    if depth >= MAX_NESTING {
        return data.len();
    }
    loop {
        at = skip_white(data, at);
        if at >= data.len() {
            return data.len();
        }
        if data[at..].starts_with(close) {
            return at + close.len();
        }
        at = nested_end(data, at, depth + 1);
    }
}

/// An entry of a dictionary written in a content stream: its key (escapes read), the range from
/// the key to the end of the value, and the value's.
pub(crate) struct Entry {
    pub(crate) key: Vec<u8>,
    pub(crate) whole: std::ops::Range<usize>,
    pub(crate) value: std::ops::Range<usize>,
}

/// The entries of the dictionary at `at` in `data` (after white space), and where it lies;
/// `None` when no dictionary starts there.
pub(crate) fn dict_entries(data: &[u8], at: usize) -> Option<(Vec<Entry>, std::ops::Range<usize>)> {
    let mut at = skip_white(data, at);
    if !data[at..].starts_with(b"<<") {
        return None;
    }
    let start = at;
    let end = object_end(data, at);
    at += 2;
    let mut entries = Vec::new();
    loop {
        at = skip_white(data, at);
        if at >= end || data[at..].starts_with(b">>") {
            break;
        }
        let key_end = nested_end(data, at, 1);
        if data[at] != b'/' {
            at = key_end;
            continue;
        }
        let value_start = skip_white(data, key_end);
        let value_end = nested_end(data, value_start, 1).min(end);
        entries.push(Entry {
            key: name(&data[at + 1..key_end]),
            whole: at..value_end,
            value: value_start..value_end,
        });
        at = value_end;
    }
    Some((entries, start..end))
}

/// The last name operand of `op`, without its `/`.
pub(crate) fn name_operand<'d>(data: &'d [u8], op: &Op) -> Option<&'d [u8]> {
    let operands = &data[op.start..op.operator.start];
    let slash = operands.iter().rposition(|b| *b == b'/')?;
    let name = &operands[slash + 1..];
    let end = name
        .iter()
        .position(|b| !is_regular(*b))
        .unwrap_or(name.len());
    Some(&name[..end])
}

/// An operand of a content-stream operator, as far as rewriting text needs: numbers, names,
/// strings (their bytes, escapes read), arrays of those.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Operand {
    Number(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Operand>),
    /// A dictionary, a boolean, `null`: what text never needs.
    Other,
}

/// The operands of `data`, the bytes before an operator.
pub(crate) fn operands(data: &[u8]) -> Vec<Operand> {
    let mut at = 0;
    let mut stack: Vec<Vec<Operand>> = vec![Vec::new()];
    while at < data.len() {
        let b = data[at];
        if is_white(b) {
            at += 1;
            continue;
        }
        let operand = match b {
            b'%' => {
                while at < data.len() && !matches!(data[at], b'\r' | b'\n') {
                    at += 1;
                }
                continue;
            }
            b'(' => {
                let end = skip_string(data, at);
                let operand =
                    Operand::String(literal(&data[at + 1..end.saturating_sub(1).max(at + 1)]));
                at = end;
                operand
            }
            b'<' if data.get(at + 1) == Some(&b'<') => {
                // A dictionary: skipped whole, nested ones included.
                let mut depth = 0usize;
                while at < data.len() {
                    if data[at..].starts_with(b"<<") {
                        depth += 1;
                        at += 2;
                    } else if data[at..].starts_with(b">>") {
                        depth = depth.saturating_sub(1);
                        at += 2;
                        if depth == 0 {
                            break;
                        }
                    } else if data[at] == b'(' {
                        at = skip_string(data, at);
                    } else {
                        at += 1;
                    }
                }
                Operand::Other
            }
            b'<' => {
                let end = data[at..]
                    .iter()
                    .position(|c| *c == b'>')
                    .map_or(data.len(), |p| at + p);
                let operand = Operand::String(hex(&data[at + 1..end]));
                at = (end + 1).min(data.len());
                operand
            }
            b'[' => {
                stack.push(Vec::new());
                at += 1;
                continue;
            }
            b']' => {
                at += 1;
                if stack.len() > 1 {
                    let items = stack.pop().unwrap_or_default();
                    Operand::Array(items)
                } else {
                    continue;
                }
            }
            b'/' => {
                let start = at + 1;
                at = start;
                while at < data.len() && is_regular(data[at]) {
                    at += 1;
                }
                Operand::Name(name(&data[start..at]))
            }
            _ if !is_regular(b) => {
                at += 1;
                Operand::Other
            }
            _ => {
                let start = at;
                while at < data.len() && is_regular(data[at]) {
                    at += 1;
                }
                std::str::from_utf8(&data[start..at])
                    .ok()
                    .and_then(|word| word.parse::<f64>().ok())
                    .map_or(Operand::Other, Operand::Number)
            }
        };
        if let Some(top) = stack.last_mut() {
            top.push(operand);
        }
    }
    // An array left open is closed where the operands end.
    while stack.len() > 1 {
        let items = stack.pop().unwrap_or_default();
        if let Some(top) = stack.last_mut() {
            top.push(Operand::Array(items));
        }
    }
    stack.pop().unwrap_or_default()
}

/// A name's bytes, its `#xx` escapes read.
fn name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'#'
            && let Some(value) = raw
                .get(at + 1..at + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(value);
            at += 3;
        } else {
            out.push(raw[at]);
            at += 1;
        }
    }
    out
}

/// A literal string's bytes, between its parentheses (ISO 32000-1, 7.3.4.2).
fn literal(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        let b = raw[at];
        at += 1;
        match b {
            b'\\' => {
                let Some(&next) = raw.get(at) else {
                    break;
                };
                at += 1;
                match next {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'0'..=b'7' => {
                        let mut value = u32::from(next - b'0');
                        for _ in 0..2 {
                            match raw.get(at) {
                                Some(digit @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(digit - b'0');
                                    at += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push((value & 0xFF) as u8);
                    }
                    // A line continued: the end of line is not part of the string.
                    b'\r' => {
                        if raw.get(at) == Some(&b'\n') {
                            at += 1;
                        }
                    }
                    b'\n' => {}
                    other => out.push(other),
                }
            }
            // An end of line in a string is a line feed.
            b'\r' => {
                if raw.get(at) == Some(&b'\n') {
                    at += 1;
                }
                out.push(b'\n');
            }
            other => out.push(other),
        }
    }
    out
}

/// A hexadecimal string's bytes, between its angle brackets: white space ignored, a missing last
/// digit read as 0.
fn hex(raw: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = raw
        .iter()
        .filter_map(|b| (*b as char).to_digit(16))
        .map(|d| d as u8)
        .collect();
    digits
        .chunks(2)
        .map(|pair| pair[0] << 4 | pair.get(1).copied().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Operand, dict_entries, object_end, operands};

    #[test]
    fn operands_are_read_with_their_escapes() {
        assert_eq!(
            operands(b"[(a\\(b\\)\\101\\\n c) -120 <48 6> /F#31] 12.5"),
            vec![
                Operand::Array(vec![
                    Operand::String(b"a(b)A c".to_vec()),
                    Operand::Number(-120.0),
                    Operand::String(vec![0x48, 0x60]),
                    Operand::Name(b"F1".to_vec()),
                ]),
                Operand::Number(12.5),
            ]
        );
        assert_eq!(
            operands(b"/Span <</MCID 3>>"),
            vec![Operand::Name(b"Span".to_vec()), Operand::Other]
        );
    }

    #[test]
    fn a_dictionary_is_read_entry_by_entry() {
        let data = b"/Span << /ActualText (a >> \\) b) /K [1 <</X 2>> [3]] %>>\n/A#6Ct <41> /MCID 7 >> rest";
        let (entries, range) = dict_entries(data, object_end(data, 0)).unwrap();
        let keys: Vec<&[u8]> = entries.iter().map(|e| e.key.as_slice()).collect();
        assert_eq!(keys, [&b"ActualText"[..], b"K", b"Alt", b"MCID"]);
        assert_eq!(&data[entries[0].value.clone()], b"(a >> \\) b)");
        assert_eq!(&data[entries[1].value.clone()], b"[1 <</X 2>> [3]]");
        assert_eq!(&data[entries[3].value.clone()], b"7");
        assert_eq!(&data[range.end..], b" rest");
        assert!(dict_entries(b"/Span /MC0", 5).is_none());
    }

    #[test]
    fn nesting_too_deep_takes_the_rest() {
        let mut data = b"<< /K ".to_vec();
        data.extend(std::iter::repeat_n(b'[', 1 << 20));
        assert_eq!(object_end(&data, 0), data.len());
        let (entries, _) = dict_entries(&data, 0).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
