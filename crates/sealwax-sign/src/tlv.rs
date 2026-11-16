//! DER elements read as they come: their tag, their content, and their bytes as received. What a
//! signature covers is the bytes received, never a re-encoding of them, so the structures whose
//! signature is checked are walked here; their leaves are decoded with `der`.

/// One element: its tag byte, its content, and the whole element (header and content).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Element<'a> {
    pub(crate) tag: u8,
    pub(crate) content: &'a [u8],
    pub(crate) whole: &'a [u8],
}

pub(crate) const INTEGER: u8 = 0x02;
pub(crate) const OCTET_STRING: u8 = 0x04;
pub(crate) const SEQUENCE: u8 = 0x30;
pub(crate) const SET: u8 = 0x31;
pub(crate) const GENERALIZED_TIME: u8 = 0x18;

/// `[n]`, constructed (explicit tagging, or an implicitly tagged structure).
pub(crate) const fn context(n: u8) -> u8 {
    0xa0 | n
}

/// `[n]`, primitive (an implicitly tagged string).
pub(crate) const fn context_primitive(n: u8) -> u8 {
    0x80 | n
}

/// How deep elements of indefinite length may nest: far more than CMS needs.
const MAX_INDEFINITE_DEPTH: usize = 32;

/// The element at the start of `input`, and what follows it. Tags of one byte (all of CMS's).
/// Lengths are definite (DER), or indefinite for a constructed element (BER, as some signers
/// write a `ContentInfo`): its content then runs to the end-of-contents octets, which `whole`
/// includes and `content` leaves out.
pub(crate) fn element(input: &[u8]) -> Option<(Element<'_>, &[u8])> {
    element_within(input, 0)
}

fn element_within(input: &[u8], depth: usize) -> Option<(Element<'_>, &[u8])> {
    let (&tag, rest) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, mut rest) = rest.split_first()?;
    if first == 0x80 {
        // Indefinite: constructed only, its elements up to `00 00`.
        if tag & 0x20 == 0 || depth >= MAX_INDEFINITE_DEPTH {
            return None;
        }
        let header = input.len() - rest.len();
        let mut inner = rest;
        loop {
            if let [0, 0, after @ ..] = inner {
                let length = rest.len() - inner.len();
                return Some((
                    Element {
                        tag,
                        content: rest.get(..length)?,
                        whole: input.get(..header + length + 2)?,
                    },
                    after,
                ));
            }
            let (_, after) = element_within(inner, depth + 1)?;
            inner = after;
        }
    }
    let length = if first < 0x80 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let (bytes, after) = rest.split_at_checked(count)?;
        rest = after;
        bytes
            .iter()
            .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte))
    };
    let header = input.len() - rest.len();
    let (content, rest) = rest.split_at_checked(length)?;
    let whole = input.get(..header + length)?;
    Some((
        Element {
            tag,
            content,
            whole,
        },
        rest,
    ))
}

/// The one element `input` holds, nothing after it.
pub(crate) fn single(input: &[u8]) -> Option<Element<'_>> {
    match element(input)? {
        (element, []) => Some(element),
        _ => None,
    }
}

/// The elements a constructed content holds, in order.
pub(crate) fn children(content: &[u8]) -> Option<Vec<Element<'_>>> {
    let mut out = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        let (element, after) = element(rest)?;
        out.push(element);
        rest = after;
    }
    Some(out)
}

/// The children of `element` if it has tag `tag`.
pub(crate) fn children_of<'a>(element: &Element<'a>, tag: u8) -> Option<Vec<Element<'a>>> {
    if element.tag == tag {
        children(element.content)
    } else {
        None
    }
}

/// An element of tag `tag` around `content`, DER-encoded.
pub(crate) fn encode(tag: u8, content: &[u8]) -> Vec<u8> {
    let length = content.len();
    let mut out = vec![tag];
    if length < 0x80 {
        out.push(u8::try_from(length).unwrap_or(0));
    } else {
        let bytes = length.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | u8::try_from(bytes.len() - skip).unwrap_or(0));
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(content);
    out
}

/// A small non-negative INTEGER's value.
pub(crate) fn small_integer(element: &Element<'_>) -> Option<u32> {
    if element.tag != INTEGER || element.content.is_empty() || element.content.len() > 4 {
        return None;
    }
    if element.content[0] & 0x80 != 0 {
        return None;
    }
    Some(
        element
            .content
            .iter()
            .fold(0u32, |acc, byte| (acc << 8) | u32::from(*byte)),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{children, element, encode, single, small_integer};

    #[test]
    fn elements_and_lengths() {
        let long = encode(0x04, &[7u8; 300]);
        assert_eq!(&long[..4], &[0x04, 0x82, 0x01, 0x2c]);
        let (read, rest) = element(&long).unwrap();
        assert_eq!((read.tag, read.content.len(), rest.len()), (0x04, 300, 0));
        let sequence = encode(
            0x30,
            &[encode(0x02, &[1]), encode(0x02, &[0, 0x80])].concat(),
        );
        let items = children(single(&sequence).unwrap().content).unwrap();
        assert_eq!(small_integer(&items[0]), Some(1));
        assert_eq!(small_integer(&items[1]), Some(128));
        assert!(single(&sequence[..sequence.len() - 1]).is_none());
        // An indefinite length (BER), constructed only: its content runs to `00 00`.
        let ber = [0x30, 0x80, 0x02, 0x01, 0x05, 0x00, 0x00, 0xff];
        let (read, rest) = element(&ber).unwrap();
        assert_eq!(
            (read.tag, read.content, read.whole.len(), rest),
            (0x30, &ber[2..5], 7, &[0xff][..])
        );
        assert!(element(&[0x04, 0x80, 0x00, 0x00]).is_none(), "primitive");
        assert!(element(&[0x30, 0x80, 0x02, 0x01, 0x05]).is_none(), "no end");
        let deep: Vec<u8> = [0x30, 0x80]
            .repeat(100)
            .into_iter()
            .chain([0; 200])
            .collect();
        assert!(element(&deep).is_none(), "nested past the bound");
    }
}
