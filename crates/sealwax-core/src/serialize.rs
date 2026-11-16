//! Writing objects as PDF syntax.
//!
//! The output respects the syntax rules of PDF/A-2 and PDF/A-3 (ISO 19005-2, 6.1): a single
//! line feed after `stream` and before `endstream`, a `/Length` equal to the data written,
//! hexadecimal strings with an even number of digits, `obj` and `endobj` on their own lines.

use std::io::Write as _;

use crate::object::{Dict, Name, ObjId, Object, Stream};

/// What a reference becomes in the output: its new identifier, or `None` when the object does
/// not exist, in which case `null` is written (a reference to a missing object reads as null).
pub(crate) trait Refs {
    fn map(&self, id: ObjId) -> Option<ObjId>;
}

/// References written as they are.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SameRefs;

impl Refs for SameRefs {
    fn map(&self, id: ObjId) -> Option<ObjId> {
        Some(id)
    }
}

/// `N G obj`, the object, `endobj`.
pub(crate) fn indirect(out: &mut Vec<u8>, id: ObjId, object: &Object, refs: &impl Refs) {
    push_display(out, id.number);
    out.push(b' ');
    push_display(out, id.generation);
    out.extend_from_slice(b" obj\n");
    match object {
        Object::Stream(stream) => self::stream(out, stream, refs),
        other => direct(out, other, refs),
    }
    out.extend_from_slice(b"\nendobj\n");
}

/// A stream's dictionary, with `/Length` set from its data, then the data.
pub(crate) fn stream(out: &mut Vec<u8>, stream: &Stream, refs: &impl Refs) {
    out.extend_from_slice(b"<<");
    for (key, value) in stream.dict.iter() {
        if key.as_bytes() == b"Length" {
            continue;
        }
        out.push(b' ');
        name(out, key);
        out.push(b' ');
        direct(out, value, refs);
    }
    out.extend_from_slice(b" /Length ");
    push_display(out, stream.data.len());
    out.extend_from_slice(b" >>\nstream\n");
    out.extend_from_slice(&stream.data);
    out.extend_from_slice(b"\nendstream");
}

/// An object as it appears inside another one, or inside an object stream. A stream cannot
/// appear there: `null` is written instead.
pub(crate) fn direct(out: &mut Vec<u8>, object: &Object, refs: &impl Refs) {
    match object {
        Object::Null | Object::Stream(_) => out.extend_from_slice(b"null"),
        Object::Bool(true) => out.extend_from_slice(b"true"),
        Object::Bool(false) => out.extend_from_slice(b"false"),
        Object::Integer(value) => push_display(out, value),
        Object::Real(value) => real(out, *value),
        Object::String(bytes) => string(out, bytes),
        Object::Name(value) => name(out, value),
        Object::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b' ');
                }
                direct(out, item, refs);
            }
            out.push(b']');
        }
        Object::Dict(value) => dict(out, value, refs),
        Object::Ref(id) => match refs.map(*id) {
            Some(id) => {
                push_display(out, id.number);
                out.push(b' ');
                push_display(out, id.generation);
                out.extend_from_slice(b" R");
            }
            None => out.extend_from_slice(b"null"),
        },
    }
}

pub(crate) fn dict(out: &mut Vec<u8>, dict: &Dict, refs: &impl Refs) {
    out.extend_from_slice(b"<<");
    for (key, value) in dict.iter() {
        out.push(b' ');
        name(out, key);
        out.push(b' ');
        direct(out, value, refs);
    }
    out.extend_from_slice(b" >>");
}

/// The shortest decimal that reads back as the same `f64`. PDF has no exponent notation, and
/// Rust's `Display` never uses one. A non-finite value cannot be read from a PDF and is written
/// as 0.
fn real(out: &mut Vec<u8>, value: f64) {
    if !value.is_finite() || value == 0.0 {
        out.push(b'0');
    } else {
        push_display(out, value);
    }
}

/// `/Name`, with `#XX` for every byte that is not a regular printable character.
pub(crate) fn name(out: &mut Vec<u8>, name: &Name) {
    out.push(b'/');
    for &byte in name.as_bytes() {
        let regular = (0x21..=0x7e).contains(&byte)
            && !matches!(
                byte,
                b'#' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
            );
        if regular {
            out.push(byte);
        } else {
            out.push(b'#');
            hex_byte(out, byte);
        }
    }
}

/// Printable text as a literal string, anything else in hexadecimal.
fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    let printable = bytes
        .iter()
        .all(|&b| (0x20..=0x7e).contains(&b) || matches!(b, b'\n' | b'\r' | b'\t'));
    if printable {
        out.push(b'(');
        for &byte in bytes {
            match byte {
                b'(' | b')' | b'\\' => {
                    out.push(b'\\');
                    out.push(byte);
                }
                // A raw end of line inside a literal string reads back as a single line feed.
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\t' => out.extend_from_slice(b"\\t"),
                _ => out.push(byte),
            }
        }
        out.push(b')');
    } else {
        out.push(b'<');
        for &byte in bytes {
            hex_byte(out, byte);
        }
        out.push(b'>');
    }
}

fn hex_byte(out: &mut Vec<u8>, byte: u8) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    out.push(DIGITS[usize::from(byte >> 4)]);
    out.push(DIGITS[usize::from(byte & 0x0f)]);
}

pub(crate) fn push_display(out: &mut Vec<u8>, value: impl std::fmt::Display) {
    // Writing into a Vec cannot fail.
    let _ = write!(out, "{value}");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn text(object: &Object) -> String {
        let mut out = Vec::new();
        direct(&mut out, object, &SameRefs);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn numbers_are_written_without_exponent_or_loss() {
        assert_eq!(text(&Object::Real(0.1)), "0.1");
        assert_eq!(text(&Object::Real(612.0)), "612");
        assert_eq!(text(&Object::Real(-0.0)), "0");
        assert_eq!(text(&Object::Real(1e-7)), "0.0000001");
        assert_eq!(text(&Object::Real(f64::NAN)), "0");
        assert_eq!(text(&Object::Integer(-2_147_483_648)), "-2147483648");
        assert_eq!(
            text(&Object::Real(1.0 / 3.0)).parse::<f64>().unwrap(),
            1.0 / 3.0
        );
    }

    #[test]
    fn names_escape_delimiters_and_non_printable_bytes() {
        assert_eq!(
            text(&Object::Name(Name::from("A B#(/)"))),
            "/A#20B#23#28#2F#29"
        );
        assert_eq!(text(&Object::Name(Name::new(vec![0xe9]))), "/#E9");
        assert_eq!(text(&Object::Name(Name::from(""))), "/");
    }

    #[test]
    fn strings_are_literal_when_printable_hexadecimal_otherwise() {
        assert_eq!(
            text(&Object::String(b"a(b)\\c\r\n".to_vec())),
            "(a\\(b\\)\\\\c\\r\\n)"
        );
        assert_eq!(
            text(&Object::String(vec![0xfe, 0xff, 0, 0x41])),
            "<FEFF0041>"
        );
        assert_eq!(text(&Object::String(Vec::new())), "()");
    }

    #[test]
    fn references_to_missing_objects_become_null() {
        struct NoneExist;
        impl Refs for NoneExist {
            fn map(&self, _: ObjId) -> Option<ObjId> {
                None
            }
        }
        let mut out = Vec::new();
        direct(
            &mut out,
            &Object::Array(vec![Object::Ref(ObjId::new(3))]),
            &NoneExist,
        );
        assert_eq!(out, b"[null]");
        assert_eq!(text(&Object::Ref(ObjId::new(3))), "3 0 R");
    }

    #[test]
    fn a_stream_gets_the_length_of_its_data() {
        let stream = Stream {
            dict: [("Length", Object::Integer(999))].into_iter().collect(),
            data: b"BT ET".to_vec(),
        };
        let mut out = Vec::new();
        indirect(&mut out, ObjId::new(4), &Object::Stream(stream), &SameRefs);
        assert_eq!(
            out,
            b"4 0 obj\n<< /Length 5 >>\nstream\nBT ET\nendstream\nendobj\n"
        );
    }
}
