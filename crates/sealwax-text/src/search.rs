//! Text looked for on a page, as PyMuPDF's `search_for` finds it (MuPDF's search, case
//! ignored): the page's lines, as [`crate::words`] reads them, taken as one text where a line
//! break is a space and a hyphen ending a line goes, the lines joined; that text and what is
//! looked for read alike: letters in upper case, white space one space, hyphens one hyphen,
//! ligatures and full-width forms as the letters they stand for.

use sealwax_core::Document;

use crate::drawn::Bounds;
use crate::words::{Line, page_lines};

/// The longest text looked for, in characters.
const MAX_NEEDLE: usize = 1024;

/// Where `needle` reads on page `page_index` (from 0): each occurrence, in the order of the
/// page's text, as the boxes of its characters, one box for each line it spans, in the page's
/// default user space (y up). Occurrences do not overlap. Nothing for a text that folds to
/// nothing.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a page the document does not have or a text
/// longer than 1,024 characters; as [`crate::words`].
pub fn search(
    doc: &Document,
    page_index: usize,
    needle: &str,
) -> sealwax_core::Result<Vec<Vec<Bounds>>> {
    if needle.chars().count() > MAX_NEEDLE {
        return Err(sealwax_core::Error::InvalidArgument {
            what: "search text",
        });
    }
    let lines = page_lines(doc, page_index)?;
    let mut wanted = Vec::new();
    for c in needle.chars() {
        fold(c, &mut |c| push(&mut wanted, c, ()));
    }
    let wanted: Vec<char> = wanted.into_iter().map(|(c, ())| c).collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let text = haystack(&lines);
    let mut hits = Vec::new();
    let mut at = 0;
    while at + wanted.len() <= text.len() {
        if text[at..at + wanted.len()]
            .iter()
            .zip(&wanted)
            .all(|((c, _), w)| c == w)
        {
            hits.push(boxes(&lines, &text[at..at + wanted.len()]));
            at += wanted.len();
        } else {
            at += 1;
        }
    }
    Ok(hits)
}

/// The page's text as one, as MuPDF flattens it to search it, hyphenated words joined
/// (PyMuPDF's search dehyphenates): each line's characters, a soft hyphen left out; a hyphen
/// ending a line left out too, the line joined to the next; else a space between lines, but
/// after a line ending with white space; each character with the one of its line it comes from
/// (none for a space between lines).
fn haystack(lines: &[Line]) -> Vec<(char, Option<(usize, usize)>)> {
    let mut text: Vec<(char, Option<(usize, usize)>)> = Vec::new();
    // No space after white space.
    let mut spaced = false;
    for (index, line) in lines.iter().enumerate() {
        let mut join = false;
        for (at, c) in line.chars.iter().enumerate() {
            let last = at + 1 == line.chars.len();
            let hyphen = matches!(c.c, '-' | '\u{AD}' | '\u{2010}' | '\u{2011}');
            if c.c == '\u{AD}' || (hyphen && last) {
                join = last;
                continue;
            }
            fold(c.c, &mut |folded| {
                push(&mut text, folded, Some((index, at)));
            });
            spaced = c.c.is_whitespace();
        }
        if !join && index + 1 < lines.len() && !spaced {
            push(&mut text, ' ', None);
            spaced = true;
        }
    }
    text
}

/// `c` added to `text`, a space after a space left out.
fn push<T>(text: &mut Vec<(char, T)>, c: char, from: T) {
    if c == ' ' && text.last().is_some_and(|(last, _)| *last == ' ') {
        return;
    }
    text.push((c, from));
}

/// `c` as text and what is looked for are compared: a ligature as its letters, a full-width form
/// as its ASCII, any hyphen as `-`, white space as a space, a letter in upper case.
fn fold(c: char, out: &mut dyn FnMut(char)) {
    let letters: &[char] = match c {
        '\u{FB00}' => &['f', 'f'],
        '\u{FB01}' => &['f', 'i'],
        '\u{FB02}' => &['f', 'l'],
        '\u{FB03}' => &['f', 'f', 'i'],
        '\u{FB04}' => &['f', 'f', 'l'],
        '\u{FB05}' | '\u{FB06}' => &['s', 't'],
        _ => &[c],
    };
    for &c in letters {
        let code = u32::from(c);
        let c = if (0xFF01..=0xFF5E).contains(&code) {
            char::from_u32(code - 0xFF01 + 0x21).unwrap_or(c)
        } else {
            c
        };
        let c = match c {
            '-' | '\u{AD}' | '\u{2010}' | '\u{2011}' => '-',
            '\t'
            | '\n'
            | '\r'
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}' => ' ',
            _ => {
                let mut upper = c.to_uppercase();
                match (upper.next(), upper.next()) {
                    (Some(one), None) => one,
                    _ => c,
                }
            }
        };
        out(c);
    }
}

/// The boxes of the characters `found` covers, one for each line, in order.
fn boxes(lines: &[Line], found: &[(char, Option<(usize, usize)>)]) -> Vec<Bounds> {
    let mut boxes: Vec<(usize, Bounds)> = Vec::new();
    for (line, at) in found.iter().filter_map(|(_, from)| *from) {
        let Some(bbox) = lines
            .get(line)
            .and_then(|l| l.chars.get(at))
            .and_then(|c| c.bbox)
        else {
            continue;
        };
        match boxes.last_mut() {
            Some((last, b)) if *last == line => {
                *b = [
                    b[0].min(bbox[0]),
                    b[1].min(bbox[1]),
                    b[2].max(bbox[2]),
                    b[3].max(bbox[3]),
                ];
            }
            _ => boxes.push((line, bbox)),
        }
    }
    boxes.into_iter().map(|(_, b)| b).collect()
}
