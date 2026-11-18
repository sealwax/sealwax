//! Fonts a caller gives to draw free texts with: TrueType or OpenType (CFF) programs, embedded
//! as `sealwax-compose` embeds them (a subset composite font with its `ToUnicode` map: the text
//! reads back), or Helvetica.
//!
//! A character the font does not have is drawn as its `?` (or left out when it has none), never
//! as `.notdef`, and counted as replaced.

use std::fmt::Write as _;
use std::sync::Arc;

use sealwax_compose::font::{EmbeddedFont, FontProgram};
use sealwax_core::{Document, ObjId};

use crate::Result;

/// Fonts a caller gives for free texts: by the name an XFDF gives (`Caveat`), and one for every
/// other name. Without any, texts are drawn in Helvetica.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TextFonts {
    named: Vec<(String, Arc<Vec<u8>>)>,
    default: Option<Arc<Vec<u8>>>,
}

impl std::fmt::Debug for TextFonts {
    // The programs' names and sizes, not their bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextFonts")
            .field(
                "named",
                &self
                    .named
                    .iter()
                    .map(|(name, program)| (name.as_str(), program.len()))
                    .collect::<Vec<_>>(),
            )
            .field(
                "default",
                &self.default.as_ref().map(|program| program.len()),
            )
            .finish()
    }
}

impl TextFonts {
    /// No font: Helvetica.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `program` (TrueType or OpenType) for the texts whose font is `name` (compared without
    /// case).
    #[must_use]
    pub fn named(mut self, name: impl Into<String>, program: Vec<u8>) -> Self {
        self.named.push((name.into(), Arc::new(program)));
        self
    }

    /// `program` (TrueType or OpenType) for the texts whose font has no program of its own.
    #[must_use]
    pub fn default_font(mut self, program: Vec<u8>) -> Self {
        self.default = Some(Arc::new(program));
        self
    }

    /// Whether no font is given.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.named.is_empty() && self.default.is_none()
    }

    /// The programs read, refused when one is no program that can be embedded.
    pub(crate) fn read(&self) -> Result<ReadFonts> {
        let read = |data: &Arc<Vec<u8>>| {
            FontProgram::parse(Arc::clone(data), 0)
                .map(Arc::new)
                .ok_or(sealwax_core::Error::InvalidArgument { what: "font" })
        };
        let mut named = Vec::with_capacity(self.named.len());
        for (name, data) in &self.named {
            named.push((name.to_lowercase(), read(data)?));
        }
        let default = self.default.as_ref().map(read).transpose()?;
        Ok(ReadFonts { named, default })
    }
}

/// [`TextFonts`] read.
pub(crate) struct ReadFonts {
    named: Vec<(String, Arc<FontProgram>)>,
    default: Option<Arc<FontProgram>>,
}

impl ReadFonts {
    /// The program for a text whose font is `name`: its own, else the default; `None` for
    /// Helvetica.
    pub(crate) fn for_name(&self, name: Option<&str>) -> Option<&Arc<FontProgram>> {
        let wanted = name.map(str::to_lowercase);
        self.named
            .iter()
            .find(|(named, _)| Some(named) == wanted.as_ref())
            .map(|(_, program)| program)
            .or(self.default.as_ref())
    }
}

/// A font a drawing writes text with: Helvetica, or a program embedded.
pub(crate) trait TextFont {
    /// The bytes of the string operand that shows `text`; the characters it cannot show are
    /// counted in `replaced`.
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8>;
    /// The width of `encoded` at `size`.
    fn width(&self, encoded: &[u8], size: f64) -> f64;
    /// `encoded` as a string operand.
    fn operand(&self, encoded: &[u8]) -> Vec<u8>;
    /// The font's name in the resources.
    fn resource(&self) -> &str;
}

/// Helvetica, WinAnsiEncoding.
pub(crate) struct Helvetica;

impl TextFont for Helvetica {
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8> {
        crate::text::encode(text, replaced)
    }

    fn width(&self, encoded: &[u8], size: f64) -> f64 {
        crate::text::width(encoded, size)
    }

    fn operand(&self, encoded: &[u8]) -> Vec<u8> {
        crate::text::literal(encoded)
    }

    fn resource(&self) -> &str {
        "Helv"
    }
}

/// Helvetica-Bold, WinAnsiEncoding.
pub(crate) struct HelveticaBold;

impl HelveticaBold {
    /// Whether a text's font, as an XFDF names it, is Helvetica-Bold.
    pub(crate) fn named(name: Option<&str>) -> bool {
        name.is_some_and(|name| {
            let name: String = name
                .chars()
                .filter(|c| !matches!(c, '-' | ' ' | ',' | '_'))
                .collect();
            name.eq_ignore_ascii_case("HelveticaBold")
        })
    }
}

impl TextFont for HelveticaBold {
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8> {
        crate::text::encode(text, replaced)
    }

    fn width(&self, encoded: &[u8], size: f64) -> f64 {
        crate::text::width_bold(encoded, size)
    }

    fn operand(&self, encoded: &[u8]) -> Vec<u8> {
        crate::text::literal(encoded)
    }

    fn resource(&self) -> &str {
        "HeBo"
    }
}

/// A program a document embeds, as the drawings use it, named `resource` in their resources.
pub(crate) struct Embedded {
    font: EmbeddedFont,
    resource: String,
}

impl Embedded {
    /// `program`, written at `id` by [`Embedded::write`], named `resource` in the drawings'
    /// resources.
    pub(crate) fn new(program: Arc<FontProgram>, id: ObjId, resource: String) -> Self {
        Self {
            font: EmbeddedFont::new(program, id),
            resource,
        }
    }

    /// The object the font is written to.
    pub(crate) fn id(&self) -> ObjId {
        self.font.id()
    }

    /// Write the font at its object: the subset of the glyphs used, their widths, the
    /// characters they stand for. A program the subsetter cannot cut is refused, as one that
    /// cannot be read.
    pub(crate) fn write(&self, doc: &mut Document) -> Result<()> {
        if self.font.write(doc)? {
            Ok(())
        } else {
            Err(sealwax_core::Error::InvalidArgument { what: "font" }.into())
        }
    }
}

impl TextFont for Embedded {
    fn encode(&self, text: &str, replaced: &mut usize) -> Vec<u8> {
        let program = self.font.program();
        let mut out = Vec::with_capacity(2 * text.len());
        for character in text.chars() {
            let (glyph, shown) = match program.glyph(character) {
                Some(glyph) => (Some(glyph), character),
                None => {
                    *replaced += 1;
                    (program.glyph('?'), '?')
                }
            };
            if let Some(glyph) = glyph {
                let code = self.font.code(glyph, shown.encode_utf8(&mut [0; 4]));
                out.extend_from_slice(&code.to_be_bytes());
            }
        }
        out
    }

    fn width(&self, encoded: &[u8], size: f64) -> f64 {
        encoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| self.font.code_advance(u16::from_be_bytes(*pair)))
            .sum::<f64>()
            * size
            / 1000.0
    }

    fn operand(&self, encoded: &[u8]) -> Vec<u8> {
        let mut out = String::with_capacity(2 * encoded.len() + 2);
        out.push('<');
        for byte in encoded {
            let _ = write!(out, "{byte:02X}");
        }
        out.push('>');
        out.into_bytes()
    }

    fn resource(&self) -> &str {
        &self.resource
    }
}
