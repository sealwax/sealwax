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
                operator = !(b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'))
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
