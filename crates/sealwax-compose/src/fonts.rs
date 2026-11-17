//! The fonts a composition's text is drawn with: programs embedded as subset composite fonts
//! ([`crate::font`]), standard-14 fonts written with the widths the layout measured.

use std::collections::BTreeMap;
use std::sync::Arc;

use sealwax_core::{Dict, Document, Name, ObjId, Object};

use crate::font::{EmbeddedFont, FontProgram};

use crate::Error;
use crate::model::{Font, Glyph};

/// The PDF standard 14 fonts.
const STANDARD: [&str; 14] = [
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
    "Symbol",
    "ZapfDingbats",
];

enum Slot {
    Embedded(EmbeddedFont),
    Standard {
        id: ObjId,
        name: &'static str,
        /// The width each code was drawn with, the first one given.
        widths: BTreeMap<u8, f64>,
    },
    /// A program that cannot be embedded (`CFF2` outlines, too large, unreadable).
    Unusable,
}

/// A glyph as a `TJ` string shows it, and the advance the font gives it (thousandths of an em).
pub(crate) struct Placed {
    pub(crate) code: String,
    pub(crate) advance: f64,
}

pub(crate) struct Fonts<'a> {
    fonts: &'a [Font],
    /// Read on first use.
    slots: Vec<Option<Slot>>,
}

impl<'a> Fonts<'a> {
    pub(crate) fn new(fonts: &'a [Font]) -> Self {
        Self {
            fonts,
            slots: fonts.iter().map(|_| None).collect(),
        }
    }

    /// Whether text can be drawn in font `index`, read (and its object reserved) the first time.
    pub(crate) fn usable(&mut self, doc: &mut Document, index: usize) -> Result<bool, Error> {
        let Some(font) = self.fonts.get(index) else {
            return Ok(false);
        };
        let Some(slot) = self.slots.get_mut(index) else {
            return Ok(false);
        };
        if slot.is_none() {
            *slot = Some(match font {
                Font::Program { program, face } => {
                    match FontProgram::parse(Arc::clone(program), *face) {
                        Some(program) => {
                            let id = doc.add_object(Object::Null)?;
                            Slot::Embedded(EmbeddedFont::new(Arc::new(program), id))
                        }
                        None => Slot::Unusable,
                    }
                }
                Font::Standard(name) => match STANDARD.iter().find(|s| **s == name) {
                    Some(name) => Slot::Standard {
                        id: doc.add_object(Object::Null)?,
                        name,
                        widths: BTreeMap::new(),
                    },
                    None => Slot::Unusable,
                },
            });
        }
        Ok(!matches!(slot, Some(Slot::Unusable) | None))
    }

    /// `glyph` drawn in font `index` (made [`Fonts::usable`] first): its code and advance.
    pub(crate) fn place(&mut self, index: usize, glyph: &Glyph) -> Placed {
        match self.slots.get_mut(index).and_then(Option::as_mut) {
            Some(Slot::Embedded(font)) => Placed {
                code: format!("{:04X}", font.code(glyph.id, &glyph.text)),
                advance: font.program().advance(glyph.id),
            },
            Some(Slot::Standard { widths, .. }) => {
                let code = u8::try_from(glyph.id).unwrap_or(b'?');
                let advance = *widths.entry(code).or_insert(glyph.advance);
                Placed {
                    code: format!("{code:02X}"),
                    advance,
                }
            }
            Some(Slot::Unusable) | None => Placed {
                code: String::new(),
                advance: 0.0,
            },
        }
    }

    /// The object of font `index`, if it was used.
    pub(crate) fn object(&self, index: usize) -> Option<ObjId> {
        match self.slots.get(index)?.as_ref()? {
            Slot::Embedded(font) => Some(font.id()),
            Slot::Standard { id, .. } => Some(*id),
            Slot::Unusable => None,
        }
    }

    /// Writes every font used, at the object reserved for it.
    pub(crate) fn write(&self, doc: &mut Document) -> Result<(), Error> {
        for slot in self.slots.iter().flatten() {
            match slot {
                Slot::Embedded(font) => font.write(doc)?,
                Slot::Standard { id, name, widths } => {
                    doc.set_object(*id, Object::Dict(standard(name, widths)));
                }
                Slot::Unusable => {}
            }
        }
        Ok(())
    }
}

/// A standard-14 font in WinAnsiEncoding with the widths it was drawn with: viewers place its
/// glyphs as the layout did.
fn standard(name: &str, widths: &BTreeMap<u8, f64>) -> Dict {
    let mut font = Dict::new();
    font.insert("Type", Name::from("Font"));
    font.insert("Subtype", Name::from("Type1"));
    font.insert("BaseFont", Name::from(name));
    if name != "Symbol" && name != "ZapfDingbats" {
        font.insert("Encoding", Name::from("WinAnsiEncoding"));
    }
    if let (Some((&first, _)), Some((&last, _))) =
        (widths.first_key_value(), widths.last_key_value())
    {
        font.insert("FirstChar", Object::Integer(i64::from(first)));
        font.insert("LastChar", Object::Integer(i64::from(last)));
        let all: Vec<Object> = (first..=last)
            .map(|code| Object::Real(widths.get(&code).copied().unwrap_or(0.0)))
            .collect();
        font.insert("Widths", Object::Array(all));
    }
    font
}
