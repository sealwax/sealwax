//! How far above and below the baseline a font's glyph boxes reach: its ascent and descent, read
//! as MuPDF reads them, so that boxes agree with PyMuPDF's.
//!
//! What follows was measured with PyMuPDF on fonts and pages made for it (MuPDF's code is not
//! read):
//! - The font descriptor's `/Ascent` and `/Descent` (thousandths of an em) come first, each on
//!   its own.
//! - For a side the descriptor lacks, an embedded program with TrueType outlines gives its
//!   `hhea` ascender and descender, or, both 0, its `OS/2` typographic ones, or, both 0 again,
//!   its Windows ones. A program with CFF or Type 1 outlines gives 0.8 and −0.2.
//! - A font that is not embedded gives the metrics of the font MuPDF draws it with when it is
//!   named as one of the 14 standard fonts (spaces aside: `Arial,Bold`, `TimesNewRomanPSMT`).
//!   Under any other name, it gives 0.8 and −0.2, or, without a font descriptor, those of
//!   Times-Roman, which MuPDF then draws it with.
//! - A Type 3 font gives its `/FontBBox` through its `/FontMatrix`, or, when that box is empty,
//!   the union of its glyphs' `d1` boxes.
//! - A positive descent is read as negative, and an ascent that is not above 0 gives 0.8 and
//!   −0.2 for both.
//! - A box less than an em tall is stretched to an em, ascent and descent in proportion.

use hayro_interpret::hayro_syntax::object::{Array, Dict, Name, Stream};

use crate::scan::{Operand, operands, operators};

/// How many glyphs of a Type 3 font without a box are read for theirs.
const MAX_TYPE3_GLYPHS: usize = 4096;

/// A font's ascent and descent, in em: the descent is 0 or below.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Metrics {
    pub(crate) ascent: f64,
    pub(crate) descent: f64,
}

impl Metrics {
    /// What a font whose metrics are not known gets.
    pub(crate) const USUAL: Self = Self {
        ascent: 0.8,
        descent: -0.2,
    };

    fn checked(ascent: f64, descent: f64) -> Self {
        let descent = if descent > 0.0 { -descent } else { descent };
        if ascent > 0.0 && ascent.is_finite() && descent.is_finite() {
            Self { ascent, descent }
        } else {
            Self::USUAL
        }
    }

    /// How far below (negative) and above the baseline a glyph's box reaches, in em: from the
    /// descent to the ascent, stretched to an em when they are closer.
    pub(crate) fn span(self) -> (f64, f64) {
        // The ascent is above 0 and the descent not: the height is above 0.
        let height = self.ascent - self.descent;
        if height < 1.0 {
            (self.descent / height, self.ascent / height)
        } else {
            (self.descent, self.ascent)
        }
    }
}

/// The metrics of the font `font` (a font dictionary).
pub(crate) fn of_font(font: &Dict<'_>) -> Metrics {
    if is_type3(font) {
        return type3(font);
    }
    let descriptor = descriptor(font);
    let given = |key: &[u8]| {
        descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.get::<f64>(key))
            .map(|value| value / 1000.0)
    };
    let (ascent, descent) = (given(b"Ascent"), given(b"Descent"));
    if let (Some(ascent), Some(descent)) = (ascent, descent) {
        return Metrics::checked(ascent, descent);
    }
    // A side the descriptor lacks: the program read only then.
    let (own_ascent, own_descent) = match descriptor.as_ref().and_then(program) {
        Some(program) => program
            .decoded()
            .ok()
            .and_then(|data| truetype(&data))
            .unwrap_or((Metrics::USUAL.ascent, Metrics::USUAL.descent)),
        None => font
            .get::<Name<'_>>(b"BaseFont")
            .and_then(|name| standard(name.as_ref()))
            .unwrap_or(if descriptor.is_some() {
                (Metrics::USUAL.ascent, Metrics::USUAL.descent)
            } else {
                Standard::TimesRoman.metrics()
            }),
    };
    Metrics::checked(ascent.unwrap_or(own_ascent), descent.unwrap_or(own_descent))
}

/// The descriptor of `font`, or of its descendant.
fn descriptor<'a>(font: &Dict<'a>) -> Option<Dict<'a>> {
    let descendant = font
        .get::<Array<'_>>(b"DescendantFonts")
        .and_then(|fonts| fonts.iter::<Dict<'_>>().next());
    descendant
        .as_ref()
        .unwrap_or(font)
        .get::<Dict<'_>>(b"FontDescriptor")
}

/// The program a font descriptor embeds, if it does.
fn program<'a>(descriptor: &Dict<'a>) -> Option<Stream<'a>> {
    [&b"FontFile2"[..], b"FontFile3", b"FontFile"]
        .into_iter()
        .find_map(|key| descriptor.get::<Stream<'_>>(key))
}

/// Whether `font` (a font dictionary) is drawn with a program it does not embed: a font that
/// is neither embedded nor of Type 3.
pub(crate) fn is_substituted(font: &Dict<'_>) -> bool {
    !is_type3(font) && descriptor(font).as_ref().and_then(program).is_none()
}

fn is_type3(font: &Dict<'_>) -> bool {
    font.get::<Name<'_>>(b"Subtype").as_deref() == Some(&b"Type3"[..])
}

/// A big-endian `u16` of `data` at `at`.
fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn i16_at(data: &[u8], at: usize) -> Option<i16> {
    u16_at(data, at).map(|value| i16::from_be_bytes(value.to_be_bytes()))
}

fn u32_at(data: &[u8], at: usize) -> Option<usize> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])).ok()
}

/// The table `tag` of the font program starting at `base` in `data`.
fn table<'d>(data: &'d [u8], base: usize, tag: &[u8; 4]) -> Option<&'d [u8]> {
    let count = usize::from(u16_at(data, base.checked_add(4)?)?);
    (0..count).find_map(|i| {
        let record = base.checked_add(12)?.checked_add(16 * i)?;
        if data.get(record..record.checked_add(4)?)? != tag {
            return None;
        }
        let offset = u32_at(data, record + 8)?;
        let length = u32_at(data, record + 12)?;
        data.get(offset..offset.checked_add(length)?)
    })
}

/// The ascent and descent, in em, of a program with TrueType outlines (the first face of a
/// collection); `None` for any other.
fn truetype(data: &[u8]) -> Option<(f64, f64)> {
    let base = if data.get(..4)? == b"ttcf" {
        u32_at(data, 12)?
    } else {
        0
    };
    table(data, base, b"glyf")?;
    let units_per_em = f64::from(u16_at(table(data, base, b"head")?, 18)?);
    if units_per_em == 0.0 {
        return None;
    }
    let hhea = table(data, base, b"hhea")?;
    let mut ascent = i32::from(i16_at(hhea, 4)?);
    let mut descent = i32::from(i16_at(hhea, 6)?);
    if ascent == 0
        && descent == 0
        && let Some(os2) = table(data, base, b"OS/2")
    {
        ascent = i32::from(i16_at(os2, 68).unwrap_or(0));
        descent = i32::from(i16_at(os2, 70).unwrap_or(0));
        if ascent == 0 && descent == 0 {
            ascent = i32::from(u16_at(os2, 74).unwrap_or(0));
            descent = -i32::from(u16_at(os2, 76).unwrap_or(0));
        }
    }
    Some((
        f64::from(ascent) / units_per_em,
        f64::from(descent) / units_per_em,
    ))
}

/// The standard font MuPDF draws a font named `name` with (spaces aside), when it is one.
const ALIASES: [(&str, Standard); 63] = {
    use Standard::{
        Courier, CourierBold, CourierBoldOblique, CourierOblique, Helvetica, HelveticaBold,
        HelveticaBoldOblique, HelveticaOblique, Symbol, TimesBold, TimesBoldItalic, TimesItalic,
        TimesRoman, ZapfDingbats,
    };
    [
        ("Arial", Helvetica),
        ("ArialMT", Helvetica),
        ("Arial-Bold", HelveticaBold),
        ("Arial-BoldMT", HelveticaBold),
        ("Arial-Italic", HelveticaOblique),
        ("Arial-ItalicMT", HelveticaOblique),
        ("Arial-BoldItalic", HelveticaBoldOblique),
        ("Arial-BoldItalicMT", HelveticaBoldOblique),
        ("Arial,Bold", HelveticaBold),
        ("Arial,Italic", HelveticaOblique),
        ("Arial,BoldItalic", HelveticaBoldOblique),
        ("Helvetica", Helvetica),
        ("Helvetica-Bold", HelveticaBold),
        ("Helvetica-Italic", HelveticaOblique),
        ("Helvetica-Oblique", HelveticaOblique),
        ("Helvetica-BoldItalic", HelveticaBoldOblique),
        ("Helvetica-BoldOblique", HelveticaBoldOblique),
        ("Helvetica,Bold", HelveticaBold),
        ("Helvetica,Italic", HelveticaOblique),
        ("Helvetica,BoldItalic", HelveticaBoldOblique),
        ("TimesNewRoman", TimesRoman),
        ("TimesNewRoman-Bold", TimesBold),
        ("TimesNewRoman-Italic", TimesItalic),
        ("TimesNewRoman-BoldItalic", TimesBoldItalic),
        ("TimesNewRoman,Bold", TimesBold),
        ("TimesNewRoman,Italic", TimesItalic),
        ("TimesNewRoman,BoldItalic", TimesBoldItalic),
        ("TimesNewRomanPS", TimesRoman),
        ("TimesNewRomanPSMT", TimesRoman),
        ("TimesNewRomanPS-Bold", TimesBold),
        ("TimesNewRomanPS-BoldMT", TimesBold),
        ("TimesNewRomanPS-Italic", TimesItalic),
        ("TimesNewRomanPS-ItalicMT", TimesItalic),
        ("TimesNewRomanPS-BoldItalic", TimesBoldItalic),
        ("TimesNewRomanPS-BoldItalicMT", TimesBoldItalic),
        ("Times-Roman", TimesRoman),
        ("Times-Bold", TimesBold),
        ("Times-Italic", TimesItalic),
        ("Times-BoldItalic", TimesBoldItalic),
        ("CourierNew", Courier),
        ("CourierNew-Bold", CourierBold),
        ("CourierNew-Italic", CourierOblique),
        ("CourierNew-BoldItalic", CourierBoldOblique),
        ("CourierNew,Bold", CourierBold),
        ("CourierNew,Italic", CourierOblique),
        ("CourierNew,BoldItalic", CourierBoldOblique),
        ("CourierNewPSMT", Courier),
        ("CourierNewPS-BoldMT", CourierBold),
        ("CourierNewPS-ItalicMT", CourierOblique),
        ("CourierNewPS-BoldItalicMT", CourierBoldOblique),
        ("Courier", Courier),
        ("Courier-Bold", CourierBold),
        ("Courier-Oblique", CourierOblique),
        ("Courier-BoldOblique", CourierBoldOblique),
        ("Courier,Bold", CourierBold),
        ("Courier,Italic", CourierOblique),
        ("Courier,BoldItalic", CourierBoldOblique),
        ("Symbol", Symbol),
        ("SymbolMT", Symbol),
        ("Symbol,Bold", Symbol),
        ("Symbol,Italic", Symbol),
        ("Symbol,BoldItalic", Symbol),
        ("ZapfDingbats", ZapfDingbats),
    ]
};

/// The 14 standard fonts, as MuPDF draws them.
#[derive(Debug, Clone, Copy)]
enum Standard {
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    Symbol,
    ZapfDingbats,
}

impl Standard {
    /// The ascent and descent, in em, of the font MuPDF draws it with.
    fn metrics(self) -> (f64, f64) {
        match self {
            Self::Helvetica => (1.075, -0.299),
            Self::HelveticaBold => (1.07, -0.307),
            Self::HelveticaOblique => (1.07, -0.284),
            Self::HelveticaBoldOblique => (1.073, -0.309),
            Self::TimesRoman => (1.053, -0.281),
            Self::TimesBold => (1.044, -0.341),
            Self::TimesItalic => (0.951, -0.27),
            Self::TimesBoldItalic => (0.972, -0.324),
            Self::Courier => (0.932, -0.317),
            Self::CourierBold => (1.007, -0.393),
            Self::CourierOblique => (0.92, -0.317),
            Self::CourierBoldOblique => (0.997, -0.393),
            Self::Symbol => (1.01, -0.293),
            Self::ZapfDingbats => (0.819, -0.144),
        }
    }
}

/// The ascent and descent of a font not embedded and named `name`, when it is named as a
/// standard font.
fn standard(name: &[u8]) -> Option<(f64, f64)> {
    let name: Vec<u8> = name.iter().copied().filter(|b| *b != b' ').collect();
    ALIASES
        .iter()
        .find(|(alias, _)| alias.as_bytes() == name)
        .map(|(_, font)| font.metrics())
}

/// The metrics of a Type 3 font: its box (or its glyphs') through its matrix.
fn type3(font: &Dict<'_>) -> Metrics {
    let matrix = font
        .get::<[f64; 6]>(b"FontMatrix")
        .unwrap_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
    let empty = |[x0, y0, x1, y1]: [f64; 4]| x0 == x1 || y0 == y1;
    let bbox = font
        .get::<[f64; 4]>(b"FontBBox")
        .filter(|bbox| !empty(*bbox))
        .or_else(|| glyph_boxes(font));
    let Some([x0, y0, x1, y1]) = bbox else {
        return Metrics::USUAL;
    };
    // The box's corners through the matrix, upright.
    let [_, b, _, d, _, f] = matrix;
    let ys = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| b * x + d * y + f);
    let low = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let high = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Metrics::checked(high, low)
}

/// The union of the boxes the glyphs of a Type 3 font give with `d1`, if any does.
fn glyph_boxes(font: &Dict<'_>) -> Option<[f64; 4]> {
    let procedures = font.get::<Dict<'_>>(b"CharProcs")?;
    let mut union: Option<[f64; 4]> = None;
    for name in procedures.keys().take(MAX_TYPE3_GLYPHS) {
        let Some(data) = procedures
            .get::<Stream<'_>>(name.as_ref())
            .and_then(|procedure| procedure.decoded().ok())
        else {
            continue;
        };
        let Some(op) = operators(&data).into_iter().next() else {
            continue;
        };
        if &data[op.operator.clone()] != b"d1" {
            continue;
        }
        let numbers: Vec<f64> = operands(&data[op.start..op.operator.start])
            .into_iter()
            .filter_map(|operand| match operand {
                Operand::Number(n) => Some(n),
                _ => None,
            })
            .collect();
        let [_, _, x0, y0, x1, y1] = numbers[..] else {
            continue;
        };
        let glyph = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        union = Some(match union {
            None => glyph,
            Some(u) => [
                u[0].min(glyph[0]),
                u[1].min(glyph[1]),
                u[2].max(glyph[2]),
                u[3].max(glyph[3]),
            ],
        });
    }
    union
}

/// The metrics of the program `data` a glyph is drawn from, when its font is not known: those of
/// its TrueType outlines, else the usual ones.
pub(crate) fn of_program(data: &[u8]) -> Metrics {
    truetype(data).map_or(Metrics::USUAL, |(ascent, descent)| {
        Metrics::checked(ascent, descent)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Metrics, standard, truetype};

    /// A program of the tables `tags`, empty but for `head` (1000 units per em), `hhea` and
    /// `OS/2` with the metrics given.
    fn program(tags: &[&[u8; 4]], hhea: (i16, i16), typo: (i16, i16), win: (u16, u16)) -> Vec<u8> {
        let table = |tag: &[u8; 4]| -> Vec<u8> {
            let mut data = vec![0u8; 80];
            match tag {
                b"head" => data[18..20].copy_from_slice(&1000u16.to_be_bytes()),
                b"hhea" => {
                    data[4..6].copy_from_slice(&hhea.0.to_be_bytes());
                    data[6..8].copy_from_slice(&hhea.1.to_be_bytes());
                }
                b"OS/2" => {
                    data[68..70].copy_from_slice(&typo.0.to_be_bytes());
                    data[70..72].copy_from_slice(&typo.1.to_be_bytes());
                    data[74..76].copy_from_slice(&win.0.to_be_bytes());
                    data[76..78].copy_from_slice(&win.1.to_be_bytes());
                }
                _ => {}
            }
            data
        };
        let count = u16::try_from(tags.len()).unwrap();
        let mut out = vec![0, 1, 0, 0];
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut offset = 12 + 16 * tags.len();
        for tag in tags {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
            out.extend_from_slice(&80u32.to_be_bytes());
            offset += 80;
        }
        for tag in tags {
            out.extend_from_slice(&table(tag));
        }
        out
    }

    #[test]
    fn truetype_programs_give_hhea_else_typographic_else_windows_metrics() {
        let all: &[&[u8; 4]] = &[b"OS/2", b"glyf", b"head", b"hhea"];
        assert_eq!(
            truetype(&program(all, (900, -300), (750, -250), (1000, 300))),
            Some((0.9, -0.3))
        );
        assert_eq!(
            truetype(&program(all, (0, 0), (750, -250), (1000, 300))),
            Some((0.75, -0.25))
        );
        assert_eq!(
            truetype(&program(all, (0, 0), (0, 0), (950, 250))),
            Some((0.95, -0.25))
        );
        // CFF outlines: not read.
        let cff: &[&[u8; 4]] = &[b"CFF ", b"OS/2", b"head", b"hhea"];
        assert_eq!(truetype(&program(cff, (900, -300), (0, 0), (0, 0))), None);
    }

    #[test]
    fn a_box_is_an_em_tall_at_least() {
        let span = |ascent, descent| Metrics::checked(ascent, descent).span();
        assert_eq!(span(0.9, -0.3), (-0.3, 0.9));
        // 0.4 and 0.1: stretched to 0.8 and 0.2.
        let (below, above) = span(0.4, -0.1);
        assert!((below + 0.2).abs() < 1e-12 && (above - 0.8).abs() < 1e-12);
        // A positive descent reads as negative; an ascent at 0 or below gives the usual.
        assert_eq!(span(0.9, 0.25), (-0.25, 0.9));
        assert_eq!(span(0.0, -0.25), (-0.2, 0.8));
        assert_eq!(span(-0.9, -0.25), (-0.2, 0.8));
        assert_eq!(span(3.0, -1.0), (-1.0, 3.0));
    }

    #[test]
    fn standard_fonts_are_found_under_their_other_names() {
        assert_eq!(standard(b"Arial,Bold"), Some((1.07, -0.307)));
        assert_eq!(standard(b"Times New Roman"), Some((1.053, -0.281)));
        assert_eq!(standard(b"ArialBold"), None);
        // A subset prefix names another font.
        assert_eq!(standard(b"ABCDEF+ArialMT"), None);
    }
}
