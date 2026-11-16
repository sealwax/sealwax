//! The 14 standard fonts of PDF, and the programs embedded in their place: the Foxit fonts of
//! PDFium (BSD, `assets/fonts/LICENSE-FOXIT`), bare CFF, which PDFium itself draws when a
//! standard font is not embedded.

/// One of the 14 standard fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Standard {
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Symbol,
    ZapfDingbats,
}

/// Font descriptor flags (ISO 32000-1, 9.8.2).
pub(crate) const FIXED_PITCH: i64 = 1;
pub(crate) const SERIF: i64 = 1 << 1;
pub(crate) const SYMBOLIC: i64 = 1 << 2;
pub(crate) const NONSYMBOLIC: i64 = 1 << 5;
pub(crate) const ITALIC: i64 = 1 << 6;
pub(crate) const FORCE_BOLD: i64 = 1 << 18;

/// Ascent, descent, cap height and dominant vertical stem width, from Adobe's metrics.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Metrics {
    pub(crate) ascent: f64,
    pub(crate) descent: f64,
    pub(crate) cap_height: f64,
    pub(crate) stem_v: f64,
}

#[derive(Clone, Copy)]
enum Family {
    Helvetica,
    Courier,
    Times,
}

impl Standard {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 14] = [
        Self::Helvetica,
        Self::HelveticaBold,
        Self::HelveticaOblique,
        Self::HelveticaBoldOblique,
        Self::Courier,
        Self::CourierBold,
        Self::CourierOblique,
        Self::CourierBoldOblique,
        Self::TimesRoman,
        Self::TimesBold,
        Self::TimesItalic,
        Self::TimesBoldItalic,
        Self::Symbol,
        Self::ZapfDingbats,
    ];

    /// The CFF program embedded for this font.
    pub(crate) fn program(self) -> &'static [u8] {
        match self {
            Self::Helvetica => include_bytes!("../../assets/fonts/FoxitSans.cff"),
            Self::HelveticaBold => include_bytes!("../../assets/fonts/FoxitSansBold.cff"),
            Self::HelveticaOblique => include_bytes!("../../assets/fonts/FoxitSansItalic.cff"),
            Self::HelveticaBoldOblique => {
                include_bytes!("../../assets/fonts/FoxitSansBoldItalic.cff")
            }
            Self::Courier => include_bytes!("../../assets/fonts/FoxitFixed.cff"),
            Self::CourierBold => include_bytes!("../../assets/fonts/FoxitFixedBold.cff"),
            Self::CourierOblique => include_bytes!("../../assets/fonts/FoxitFixedItalic.cff"),
            Self::CourierBoldOblique => {
                include_bytes!("../../assets/fonts/FoxitFixedBoldItalic.cff")
            }
            Self::TimesRoman => include_bytes!("../../assets/fonts/FoxitSerif.cff"),
            Self::TimesBold => include_bytes!("../../assets/fonts/FoxitSerifBold.cff"),
            Self::TimesItalic => include_bytes!("../../assets/fonts/FoxitSerifItalic.cff"),
            Self::TimesBoldItalic => include_bytes!("../../assets/fonts/FoxitSerifBoldItalic.cff"),
            Self::Symbol => include_bytes!("../../assets/fonts/FoxitSymbol.cff"),
            Self::ZapfDingbats => include_bytes!("../../assets/fonts/FoxitDingbats.cff"),
        }
    }

    /// Symbol and ZapfDingbats: their own encoding, glyphs outside the Latin character set.
    pub(crate) fn symbolic(self) -> bool {
        matches!(self, Self::Symbol | Self::ZapfDingbats)
    }

    fn family(self) -> Option<Family> {
        match self {
            Self::Helvetica
            | Self::HelveticaBold
            | Self::HelveticaOblique
            | Self::HelveticaBoldOblique => Some(Family::Helvetica),
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                Some(Family::Courier)
            }
            Self::TimesRoman | Self::TimesBold | Self::TimesItalic | Self::TimesBoldItalic => {
                Some(Family::Times)
            }
            Self::Symbol | Self::ZapfDingbats => None,
        }
    }

    fn italic(self) -> bool {
        matches!(
            self,
            Self::HelveticaOblique
                | Self::HelveticaBoldOblique
                | Self::CourierOblique
                | Self::CourierBoldOblique
                | Self::TimesItalic
                | Self::TimesBoldItalic
        )
    }

    /// The descriptor flags of this font.
    pub(crate) fn flags(self) -> i64 {
        let mut flags = if self.symbolic() {
            SYMBOLIC
        } else {
            NONSYMBOLIC
        };
        match self.family() {
            Some(Family::Courier) => flags |= FIXED_PITCH,
            Some(Family::Times) => flags |= SERIF,
            _ => {}
        }
        if self.italic() {
            flags |= ITALIC;
        }
        flags
    }

    pub(crate) fn metrics(self) -> Metrics {
        let (ascent, descent, cap_height, stem_v) = match self {
            Self::Helvetica | Self::HelveticaOblique => (718.0, -207.0, 718.0, 88.0),
            Self::HelveticaBold | Self::HelveticaBoldOblique => (718.0, -207.0, 718.0, 140.0),
            Self::Courier | Self::CourierOblique => (629.0, -157.0, 562.0, 51.0),
            Self::CourierBold | Self::CourierBoldOblique => (629.0, -157.0, 562.0, 106.0),
            Self::TimesRoman => (683.0, -217.0, 662.0, 84.0),
            Self::TimesBold => (683.0, -217.0, 676.0, 139.0),
            Self::TimesItalic => (683.0, -217.0, 653.0, 76.0),
            Self::TimesBoldItalic => (683.0, -217.0, 669.0, 121.0),
            Self::Symbol => (1010.0, -293.0, 1010.0, 85.0),
            Self::ZapfDingbats => (820.0, -143.0, 820.0, 90.0),
        };
        Metrics {
            ascent,
            descent,
            cap_height,
            stem_v,
        }
    }

    /// The width Adobe's metrics give a glyph where the Foxit font's differs. Elsewhere the two
    /// agree (measured on every glyph the metrics name, 14/11/2026).
    pub(crate) fn adobe_width(self, name: &str) -> Option<f64> {
        let overrides: &[(&str, f64)] = match self {
            Self::Helvetica => &[
                ("guillemotleft", 556.0),
                ("guillemotright", 556.0),
                ("guilsinglright", 333.0),
                ("Euro", 556.0),
            ],
            Self::HelveticaBold => &[("at", 975.0), ("Euro", 556.0)],
            Self::HelveticaOblique => &[("at", 1015.0), ("guilsinglleft", 333.0), ("Euro", 556.0)],
            Self::HelveticaBoldOblique => &[
                ("five", 556.0),
                ("at", 975.0),
                ("oslash", 611.0),
                ("Euro", 556.0),
            ],
            Self::TimesRoman | Self::TimesBold => &[("Euro", 500.0)],
            Self::TimesBoldItalic => &[("Euro", 500.0)],
            _ => &[],
        };
        overrides
            .iter()
            .find(|(glyph, _)| *glyph == name)
            .map(|(_, width)| *width)
    }

    fn with_style(family: Family, bold: bool, italic: bool) -> Self {
        match (family, bold, italic) {
            (Family::Helvetica, false, false) => Self::Helvetica,
            (Family::Helvetica, true, false) => Self::HelveticaBold,
            (Family::Helvetica, false, true) => Self::HelveticaOblique,
            (Family::Helvetica, true, true) => Self::HelveticaBoldOblique,
            (Family::Courier, false, false) => Self::Courier,
            (Family::Courier, true, false) => Self::CourierBold,
            (Family::Courier, false, true) => Self::CourierOblique,
            (Family::Courier, true, true) => Self::CourierBoldOblique,
            (Family::Times, false, false) => Self::TimesRoman,
            (Family::Times, true, false) => Self::TimesBold,
            (Family::Times, false, true) => Self::TimesItalic,
            (Family::Times, true, true) => Self::TimesBoldItalic,
        }
    }

    /// The standard font to embed in place of a font that is not embedded, from its name and
    /// what its descriptor says. `None` for a symbolic font none of them can stand in for
    /// (Wingdings and the like).
    pub(crate) fn substitute(
        base_font: &str,
        flags: Option<i64>,
        weight: Option<f64>,
        italic_angle: Option<f64>,
    ) -> Option<Self> {
        let name: String = strip_subset(base_font)
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        if name == "symbol" || name == "symbolmt" {
            return Some(Self::Symbol);
        }
        if name.starts_with("zapfdingbats") || name == "dingbats" {
            return Some(Self::ZapfDingbats);
        }
        let flags = flags.unwrap_or(0);
        let family = if name.contains("courier") || name.contains("mono") {
            Family::Courier
        } else if name.contains("times") {
            Family::Times
        } else if name.contains("helvetica") || name.contains("arial") || name.contains("sans") {
            Family::Helvetica
        } else if flags & SYMBOLIC != 0 && flags & NONSYMBOLIC == 0 {
            return None;
        } else if flags & FIXED_PITCH != 0 {
            Family::Courier
        } else if flags & SERIF != 0 {
            Family::Times
        } else {
            Family::Helvetica
        };
        let bold = ["bold", "black", "heavy", "semibold", "demi"]
            .iter()
            .any(|word| name.contains(word))
            || flags & FORCE_BOLD != 0
            || weight.is_some_and(|w| w >= 600.0);
        let italic = name.contains("italic")
            || name.contains("oblique")
            || flags & ITALIC != 0
            || italic_angle.is_some_and(|a| a.abs() > 0.5);
        Some(Self::with_style(family, bold, italic))
    }
}

/// Whether a font of this name has the metrics of a standard font: the 14 themselves, and
/// Arial, Times New Roman and Courier New, drawn to Helvetica's, Times' and Courier's.
pub(crate) fn metric_compatible(base_font: &str) -> bool {
    let name: String = strip_subset(base_font)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "helvetica",
        "arial",
        "times",
        "courier",
        "symbol",
        "zapfdingbats",
    ]
    .iter()
    .any(|family| name.starts_with(family))
}

/// `ABCDEF+Name` is `Name`.
pub(crate) fn strip_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((prefix, rest))
            if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            rest
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::{NONSYMBOLIC, SERIF, SYMBOLIC, Standard};

    #[test]
    fn substitutes() {
        let cases = [
            ("Helvetica", None, Some(Standard::Helvetica)),
            (
                "Helvetica-BoldOblique",
                None,
                Some(Standard::HelveticaBoldOblique),
            ),
            ("ArialMT", None, Some(Standard::Helvetica)),
            (
                "Arial,BoldItalic",
                None,
                Some(Standard::HelveticaBoldOblique),
            ),
            ("Arial-BoldMT", None, Some(Standard::HelveticaBold)),
            (
                "TimesNewRomanPS-ItalicMT",
                None,
                Some(Standard::TimesItalic),
            ),
            ("Times-Roman", None, Some(Standard::TimesRoman)),
            ("CourierNewPSMT", None, Some(Standard::Courier)),
            ("Courier-Bold", None, Some(Standard::CourierBold)),
            ("Symbol", None, Some(Standard::Symbol)),
            ("ZapfDingbats", None, Some(Standard::ZapfDingbats)),
            (
                "Formata-Regular",
                Some(NONSYMBOLIC),
                Some(Standard::Helvetica),
            ),
            (
                "Garamond",
                Some(NONSYMBOLIC | SERIF),
                Some(Standard::TimesRoman),
            ),
            ("Wingdings", Some(SYMBOLIC), None),
            ("ABCDEF+Helvetica", None, Some(Standard::Helvetica)),
        ];
        for (name, flags, expected) in cases {
            assert_eq!(
                Standard::substitute(name, flags, None, None),
                expected,
                "{name}"
            );
        }
        assert_eq!(
            Standard::substitute("Corporate", Some(NONSYMBOLIC), Some(700.0), Some(-12.0)),
            Some(Standard::HelveticaBoldOblique)
        );
    }
}
