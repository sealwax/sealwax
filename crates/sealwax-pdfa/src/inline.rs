//! Inline images, taken out of a content stream in one pass before hayro-syntax parses it.
//!
//! To find where an inline image's data ends, hayro-syntax parses what follows as content, and
//! does so again for each inline image found there: the time doubles with each image, and a long
//! chain overflows the stack (hayro issue #1610, open). Fuzzing the conversion found a damaged
//! file it would not finish in hours. The scan therefore removes inline images itself, linearly,
//! and reads from their dictionaries what it needs: colour space, interpolation, image mask.

use std::borrow::Cow;

/// What the scan needs from an inline image.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct InlineImage {
    /// The colour space: a name (abbreviated or not, or a resource), or the base of an indexed
    /// space given as an array.
    pub(crate) space: Option<Vec<u8>>,
    pub(crate) interpolate: bool,
    pub(crate) mask: bool,
}

pub(crate) fn is_white(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' | b'\0')
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

pub(crate) fn is_regular(b: u8) -> bool {
    !is_white(b) && !is_delimiter(b)
}

/// Where the literal string starting at `at` (on its `(`) ends.
pub(crate) fn skip_string(data: &[u8], mut at: usize) -> usize {
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

/// The tokens of an inline image dictionary: names (with their `/`), brackets, other words.
fn tokens(dict: &[u8]) -> Vec<&[u8]> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < dict.len() {
        let b = dict[at];
        if is_white(b) {
            at += 1;
        } else if b == b'[' || b == b']' {
            found.push(&dict[at..=at]);
            at += 1;
        } else if b == b'(' {
            at = skip_string(dict, at);
        } else if b == b'<' {
            at = dict[at..]
                .iter()
                .position(|c| *c == b'>')
                .map_or(dict.len(), |p| at + p + 1);
        } else {
            let start = at;
            at += 1;
            while at < dict.len() && is_regular(dict[at]) {
                at += 1;
            }
            found.push(&dict[start..at]);
        }
    }
    found
}

fn read_dict(dict: &[u8]) -> (InlineImage, Option<usize>) {
    let tokens = tokens(dict);
    let mut image = InlineImage::default();
    let mut length = None;
    for (i, token) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1).copied();
        match *token {
            b"/CS" | b"/ColorSpace" => {
                image.space = match next {
                    Some(b"[") => {
                        // [/Indexed base hival lookup]: the base is what can be a device space.
                        let family = tokens.get(i + 2).copied();
                        match family {
                            Some(b"/I" | b"/Indexed") => tokens.get(i + 3).copied(),
                            _ => None,
                        }
                    }
                    other => other,
                }
                .and_then(|name| name.strip_prefix(b"/"))
                .map(<[u8]>::to_vec);
            }
            b"/I" | b"/Interpolate" => image.interpolate = next == Some(b"true"),
            b"/IM" | b"/ImageMask" => image.mask = next == Some(b"true"),
            b"/L" | b"/Length" => {
                length = next
                    .and_then(|n| std::str::from_utf8(n).ok())
                    .and_then(|n| n.parse().ok());
            }
            _ => {}
        }
    }
    (image, length)
}

/// Where the data of an inline image starting at `start` ends: the first `EI` standing alone.
fn data_end(data: &[u8], start: usize) -> Option<usize> {
    let mut at = start;
    while at + 2 <= data.len() {
        if &data[at..at + 2] == b"EI"
            && (at == start || is_white(data[at - 1]))
            && data.get(at + 2).is_none_or(|b| !is_regular(*b))
        {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// The inline image whose `BI` ends at `at`: what the scan needs from it, and where it ends
/// (after its `EI`, or the end of `content` when no `EI` follows). `None` without an `ID`
/// operator: the rest is not content.
pub(crate) fn image_end(content: &[u8], at: usize) -> Option<(InlineImage, usize)> {
    // The dictionary runs to the `ID` operator.
    let mut cursor = at;
    let mut id = None;
    while cursor < content.len() {
        let c = content[cursor];
        if c == b'(' {
            cursor = skip_string(content, cursor);
        } else if is_regular(c) {
            let word = cursor;
            while cursor < content.len() && is_regular(content[cursor]) {
                cursor += 1;
            }
            if &content[word..cursor] == b"ID" {
                id = Some((word, cursor));
                break;
            }
        } else {
            cursor += 1;
        }
    }
    let (id_start, id_end) = id?;
    let (image, length) = read_dict(&content[at..id_start]);
    // One white-space character after ID, then the data.
    let data_start = (id_end + 1).min(content.len());
    let end = match length {
        Some(length) => {
            let after = data_start.saturating_add(length).min(content.len());
            data_end(content, after).map(|ei| ei + 2)
        }
        None => data_end(content, data_start).map(|ei| ei + 2),
    }
    .unwrap_or(content.len());
    Some((image, end))
}

/// `content` without its inline images, and what the scan needs from each.
pub(crate) fn split(content: &[u8]) -> (Cow<'_, [u8]>, Vec<InlineImage>) {
    let mut images = Vec::new();
    let mut out: Option<Vec<u8>> = None;
    let mut copied = 0;
    let mut at = 0;
    while at < content.len() {
        let b = content[at];
        if is_white(b) || matches!(b, b'[' | b']' | b'{' | b'}' | b')' | b'>') {
            at += 1;
        } else if b == b'%' {
            while at < content.len() && !matches!(content[at], b'\r' | b'\n') {
                at += 1;
            }
        } else if b == b'(' {
            at = skip_string(content, at);
        } else if b == b'<' {
            if content.get(at + 1) == Some(&b'<') {
                at += 2;
            } else {
                at = content[at..]
                    .iter()
                    .position(|c| *c == b'>')
                    .map_or(content.len(), |p| at + p + 1);
            }
        } else {
            let start = at;
            at += 1;
            while at < content.len() && is_regular(content[at]) {
                at += 1;
            }
            if &content[start..at] != b"BI" {
                continue;
            }
            let Some((image, end)) = image_end(content, at) else {
                // No data: the rest is not content anyway.
                break;
            };
            images.push(image);
            let buffer = out.get_or_insert_with(|| Vec::with_capacity(content.len()));
            buffer.extend_from_slice(&content[copied..start]);
            buffer.push(b' ');
            copied = end;
            at = end;
        }
    }
    match out {
        Some(mut buffer) => {
            buffer.extend_from_slice(&content[copied.min(content.len())..]);
            (Cow::Owned(buffer), images)
        }
        None => (Cow::Borrowed(content), images),
    }
}

#[cfg(test)]
mod tests {
    use super::{InlineImage, split};

    #[test]
    fn inline_images_are_taken_out() {
        let content = b"q 1 0 0 1 0 0 cm BI /W 2 /H 1 /CS /RGB /BPC 8 /I true ID \x01EI\x02\x03\x04\x05 EI Q (BI ID EI) Tj";
        let (rest, images) = split(content);
        assert_eq!(&*rest, &b"q 1 0 0 1 0 0 cm   Q (BI ID EI) Tj"[..]);
        assert_eq!(
            images,
            [InlineImage {
                space: Some(b"RGB".to_vec()),
                interpolate: true,
                mask: false
            }]
        );
    }

    #[test]
    fn indexed_spaces_masks_and_lengths() {
        let content = b"BI /W 1 /H 1 /IM true /L 4 ID xEIx EI BI /CS [/I /DeviceCMYK 1 <00>] /W 1 /H 1 ID z EI";
        let (rest, images) = split(content);
        assert_eq!(rest.iter().filter(|b| !b.is_ascii_whitespace()).count(), 0);
        assert!(images[0].mask);
        assert_eq!(images[1].space.as_deref(), Some(&b"DeviceCMYK"[..]));
    }

    #[test]
    fn many_images_and_a_damaged_one_take_linear_time() {
        // hayro issue #1610: time doubled with each image of such a chain.
        let mut content = Vec::new();
        for _ in 0..20_000 {
            content.extend_from_slice(b"q BI /W 1 /H 1 /BPC 8 /CS /G ID x EI Q %EI\n");
        }
        content.extend_from_slice(b"BI /W 9 /H 9 ID never ends");
        let started = std::time::Instant::now();
        let (rest, images) = split(&content);
        assert_eq!(images.len(), 20_001);
        assert!(!rest.windows(2).any(|w| w == b"BI"));
        assert!(started.elapsed().as_secs() < 5);
    }

    #[test]
    fn content_without_images_is_borrowed() {
        let (rest, images) = split(b"0 0 m 10 10 l S (BIG) Tj");
        assert!(matches!(rest, std::borrow::Cow::Borrowed(_)));
        assert!(images.is_empty());
    }
}
