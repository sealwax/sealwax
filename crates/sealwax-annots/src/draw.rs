//! Appearances drawn for what has none: a form field's value from its default appearance, a
//! check mark, a box, ink strokes, and the free text of an XFDF. Text is Helvetica in
//! WinAnsiEncoding, whatever `/DA` names.

use crate::text::{self, number};
use crate::xfdf::{Align, VerticalAlign};

/// The smallest size an automatic size goes down to.
const MIN_SIZE: f64 = 4.0;
/// Field flags (ISO 32000-1, 12.7.4).
pub(crate) const MULTILINE: i64 = 1 << 12;
pub(crate) const PASSWORD: i64 = 1 << 13;
pub(crate) const COMB: i64 = 1 << 24;
pub(crate) const COMBO: i64 = 1 << 17;

/// A colour operator for 1, 3 or 4 components: `g`, `rg`, `k` (fill) or `G`, `RG`, `K` (stroke).
pub(crate) fn color(components: &[f64], stroke: bool) -> Option<String> {
    let operator = match (components.len(), stroke) {
        (1, false) => "g",
        (3, false) => "rg",
        (4, false) => "k",
        (1, true) => "G",
        (3, true) => "RG",
        (4, true) => "K",
        _ => return None,
    };
    let values: Vec<String> = components.iter().map(|v| number(*v)).collect();
    Some(format!("{} {operator}", values.join(" ")))
}

/// A widget's background and border (`/MK` `BG` and `BC`, `/BS` width).
#[derive(Debug, Default, Clone)]
pub(crate) struct Frame {
    pub(crate) background: Option<Vec<f64>>,
    pub(crate) border: Option<Vec<f64>>,
    pub(crate) border_width: f64,
    /// `/BS /S`.
    pub(crate) style: BorderStyle,
}

/// A border's style (`/BS /S`).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) enum BorderStyle {
    #[default]
    Solid,
    /// `/D` with its dash array.
    Dashed(Vec<f64>),
    /// `/B`: lighter on the left and top.
    Beveled,
    /// `/I`: darker on the left and top.
    Inset,
    /// `/U`: the bottom line only.
    Underline,
}

impl BorderStyle {
    /// Beveled and inset borders take twice their width.
    pub(crate) fn raised(&self) -> bool {
        matches!(self, Self::Beveled | Self::Inset)
    }
}

impl Frame {
    /// The background, then the border, as PDFium draws it (`GenerateBorderAP` of
    /// `core/fpdfdoc/cpdf_generateap.cpp`, BSD-3-Clause): a solid border is the ring between the
    /// rectangle and the rectangle less its width, filled; a dashed one is stroked half its width
    /// inside. A beveled or inset one, as Acrobat draws it.
    fn drawing(&self, width: f64, height: f64) -> Vec<String> {
        let mut drawing = Vec::new();
        if let Some(fill) = self.background.as_deref().and_then(|c| color(c, false)) {
            drawing.push(format!(
                "{fill} 0 0 {} {} re f",
                number(width),
                number(height)
            ));
        }
        let w = self.border_width;
        if w <= 0.0 {
            return drawing;
        }
        let half = w / 2.0;
        let rect = |inset: f64| {
            format!(
                "{} {} {} {} re",
                number(inset),
                number(inset),
                number(width - 2.0 * inset),
                number(height - 2.0 * inset)
            )
        };
        let point = |x: f64, y: f64| format!("{} {}", number(x), number(y));
        let fill = self.border.as_deref().and_then(|c| color(c, false));
        let stroke = self.border.as_deref().and_then(|c| color(c, true));
        match &self.style {
            BorderStyle::Solid => {
                if let Some(fill) = fill {
                    drawing.push(format!("{fill} {} {} f*", rect(0.0), rect(w)));
                }
            }
            BorderStyle::Dashed(dash) => {
                if let Some(stroke) = stroke {
                    let dash: Vec<String> = dash.iter().map(|v| number(*v)).collect();
                    drawing.push(format!(
                        "{stroke} {} w [{}] 0 d {} m {} l {} l {} l {} l S",
                        number(w),
                        dash.join(" "),
                        point(half, half),
                        point(half, height - half),
                        point(width - half, height - half),
                        point(width - half, half),
                        point(half, half)
                    ));
                }
            }
            BorderStyle::Beveled | BorderStyle::Inset => {
                // As Acrobat draws it: the ring of the border's colour, then inside it a bevel
                // as wide, lighter on the left and top (white, or 0.5 gray inset), 0.75 gray on
                // the right and bottom.
                let (light, dark) = if self.style == BorderStyle::Beveled {
                    (1.0, 0.75)
                } else {
                    (0.5, 0.75)
                };
                let (w2, right, top) = (2.0 * w, width - w, height - w);
                drawing.push(format!(
                    "{} g {} m {} l {} l {} l {} l {} l f",
                    number(light),
                    point(w, w),
                    point(w, top),
                    point(right, top),
                    point(width - w2, height - w2),
                    point(w2, height - w2),
                    point(w2, w2)
                ));
                drawing.push(format!(
                    "{} g {} m {} l {} l {} l {} l {} l f",
                    number(dark),
                    point(right, top),
                    point(right, w),
                    point(w, w),
                    point(w2, w2),
                    point(width - w2, w2),
                    point(width - w2, height - w2)
                ));
                if let Some(fill) = fill {
                    drawing.push(format!("{fill} {} {} f*", rect(0.0), rect(w)));
                }
            }
            BorderStyle::Underline => {
                if let Some(stroke) = stroke {
                    drawing.push(format!(
                        "{stroke} {} w {} m {} l S",
                        number(w),
                        point(0.0, half),
                        point(width, half)
                    ));
                }
            }
        }
        drawing
    }
}

/// What a text or choice field shows and how.
#[derive(Debug, Default, Clone)]
pub(crate) struct TextField {
    /// The lines of its value (a choice field's export values shown by their display text).
    pub(crate) lines: Vec<String>,
    pub(crate) choice: bool,
    pub(crate) flags: i64,
    /// `/DA` font size: 0 is automatic.
    pub(crate) size: f64,
    pub(crate) color: Option<Vec<f64>>,
    /// `/Q`: 0 left, 1 centred, 2 right.
    pub(crate) quadding: i64,
    pub(crate) max_length: Option<i64>,
    pub(crate) frame: Frame,
}

fn split_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| {
            let parts: Vec<String> = line
                .split(['\n', '\r'])
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if parts.is_empty() {
                vec![String::new()]
            } else {
                parts
            }
        })
        .collect()
}

/// `lines` wrapped at word boundaries to `width` points at `size`.
fn wrapped(lines: &[String], width: f64, size: f64) -> Vec<String> {
    let mut out = Vec::new();
    let mut ignored = 0;
    for line in lines {
        let mut current = String::new();
        for word in line.split(' ') {
            let candidate = if current.is_empty() {
                word.to_owned()
            } else {
                format!("{current} {word}")
            };
            if !current.is_empty()
                && text::width(&text::encode(&candidate, &mut ignored), size) > width
            {
                out.push(std::mem::replace(&mut current, word.to_owned()));
            } else {
                current = candidate;
            }
        }
        out.push(current);
    }
    out
}

fn tm(left: f64, baseline: f64, encoded: &[u8]) -> Vec<u8> {
    let mut line = format!("1 0 0 1 {} {} Tm ", number(left), number(baseline)).into_bytes();
    line.extend_from_slice(&text::literal(encoded));
    line.extend_from_slice(b" Tj");
    line
}

/// Where a field's text goes, in multiples of the font size: one line, its baseline 0.65
/// below the middle, halved, `(h - 0.65 s) / 2`; an automatic size of 0.85 times the height; lines
/// of a multiline field 1.2 apart, the first 1.0667 below the top less the border, 12 points when
/// automatic; a padding of the border's width (twice for a beveled or inset border), and 2 more
/// for a multiline field; centred over the whole width; a comb's characters centred in their
/// cells.
const SINGLE_DROP: f64 = 0.65;
const AUTOMATIC_SIZE: f64 = 0.85;
const FIELD_LINE_HEIGHT: f64 = 1.2;
const MULTILINE_DROP: f64 = 1.0667;
const MULTILINE_SIZE: f64 = 12.0;
const MULTILINE_PADDING: f64 = 2.0;

/// A text or choice field's value drawn from its default appearance: size (0 is automatic), colour, quadding, multiline, comb and password. Returns the
/// drawing and the characters WinAnsiEncoding does not have.
pub(crate) fn text_field(field: &TextField, width: f64, height: f64) -> (Vec<u8>, usize) {
    let mut replaced = 0;
    let mut lines = field.lines.clone();
    if !field.choice && field.flags & PASSWORD != 0 {
        lines = lines
            .iter()
            .map(|line| "*".repeat(line.chars().count()))
            .collect();
    }
    let multiline = (!field.choice && field.flags & MULTILINE != 0)
        || (field.choice && field.flags & COMBO == 0);
    if multiline {
        lines = split_lines(&lines);
    } else {
        let joined = lines.join(" ");
        lines = vec![split_lines(&[joined]).join(" ")];
    }
    let border = field.frame.border_width * if field.frame.style.raised() { 2.0 } else { 1.0 };
    let padding = if multiline {
        border + MULTILINE_PADDING
    } else {
        border
    };
    let inner = (width - 2.0 * padding).max(1.0);
    let mut size = field.size;
    if size <= 0.0 {
        if multiline {
            size = MULTILINE_SIZE;
            while size > MIN_SIZE
                && wrapped(&lines, inner, size).len() as f64 * size * FIELD_LINE_HEIGHT
                    > height - 2.0 * border
            {
                size -= 0.5;
            }
        } else {
            size = MIN_SIZE.max(height * AUTOMATIC_SIZE);
            let mut ignored = 0;
            let text_width = lines.first().map_or(0.0, |line| {
                text::width(&text::encode(line, &mut ignored), 1.0)
            });
            if text_width > 0.0 {
                size = MIN_SIZE.max(size.min(inner / text_width));
            }
        }
    }
    let fill = field
        .color
        .as_deref()
        .and_then(|c| color(c, false))
        .unwrap_or_else(|| "0 g".to_owned());
    let mut drawing: Vec<Vec<u8>> = vec![b"/Tx BMC".to_vec(), b"q".to_vec()];
    drawing.extend(
        field
            .frame
            .drawing(width, height)
            .into_iter()
            .map(String::into_bytes),
    );
    drawing.push(
        format!(
            "{} {} {} {} re W n",
            number(border),
            number(border),
            number(width - 2.0 * border),
            number(height - 2.0 * border)
        )
        .into_bytes(),
    );
    drawing.push(format!("BT /Helv {} Tf {fill}", number(size)).into_bytes());
    let single_baseline = (height - SINGLE_DROP * size) / 2.0;
    match field.max_length {
        Some(max)
            if !field.choice
                && field.flags & COMB != 0
                && !multiline
                && max > 0
                && !lines.is_empty() =>
        {
            #[allow(clippy::cast_precision_loss)]
            let cell = width / max as f64;
            let count = usize::try_from(max).unwrap_or(usize::MAX);
            for (index, character) in lines[0].chars().take(count).enumerate() {
                let encoded = text::encode(&character.to_string(), &mut replaced);
                #[allow(clippy::cast_precision_loss)]
                let left = cell * index as f64 + (cell - text::width(&encoded, size)) / 2.0;
                drawing.push(tm(left, single_baseline, &encoded));
            }
        }
        _ => {
            let (shown, mut baseline) = if multiline {
                (
                    wrapped(&lines, inner, size),
                    height - border - MULTILINE_DROP * size,
                )
            } else {
                (lines, single_baseline)
            };
            for line in shown {
                let encoded = text::encode(&line, &mut replaced);
                let line_width = text::width(&encoded, size);
                let left = match field.quadding {
                    1 => (width - line_width) / 2.0,
                    2 => width - padding - line_width,
                    _ => padding,
                };
                drawing.push(tm(left, baseline, &encoded));
                baseline -= size * FIELD_LINE_HEIGHT;
            }
        }
    }
    drawing.extend([b"ET".to_vec(), b"Q".to_vec(), b"EMC".to_vec()]);
    (drawing.join(&b'\n'), replaced)
}

/// A check box without appearance: a ZapfDingbats check mark when checked, its frame only when
/// not.
pub(crate) fn check_box(checked: bool, frame: &Frame, width: f64, height: f64) -> Vec<u8> {
    let mut drawing = frame.drawing(width, height);
    if checked {
        let size = width.min(height) * 0.8;
        let left = (width - size * 0.846) / 2.0;
        let bottom = (height - size * 0.7) / 2.0;
        drawing.push(format!(
            "BT /ZaDb {} Tf 0 g {} {} Td (4) Tj ET",
            number(size),
            number(left),
            number(bottom)
        ));
    }
    drawing.join("\n").into_bytes()
}

/// Where the lines of a free text go, in multiples of the font size: lines 1.2 apart; centred
/// vertically, the first baseline 0.6375 below the middle of the rectangle, raised by half the
/// block of the other lines; at the top, 0.75 below the top less the padding. Text aligned at the
/// bottom is drawn near the top, 0.8 below it, as signed documents have shown it so far.
const LINE_HEIGHT: f64 = 1.2;
const CENTRE_DROP: f64 = 0.6375;
const TOP_DROP: f64 = 0.75;
const BOTTOM_DROP: f64 = 0.8;

/// The free text of an XFDF (`<freetext>`), drawn in its rectangle: Helvetica at its size and
/// colour, a padding of twice the border's width, the lines the viewer showed when it gave them, else its contents wrapped to the rectangle; its border and background when it has
/// them. Returns the drawing and the characters WinAnsiEncoding does not have.
pub(crate) fn free_text(free: &crate::xfdf::Text, width: f64, height: f64) -> (Vec<u8>, usize) {
    let mut replaced = 0;
    let size = free.size;
    let padding = 2.0 * free.border;
    let inner = (width - 2.0 * padding).max(1.0);
    let lines = match &free.lines {
        Some(lines) if same_words(lines, &free.contents) => aligned_lines(lines, &free.contents),
        _ => wrapped(
            &split_lines(std::slice::from_ref(&free.contents)),
            inner,
            size,
        ),
    };
    let leading = size * LINE_HEIGHT;
    #[allow(clippy::cast_precision_loss)]
    let others = lines.len().saturating_sub(1) as f64;
    let first_baseline = match free.vertical {
        VerticalAlign::Center => height / 2.0 + others * leading / 2.0 - CENTRE_DROP * size,
        VerticalAlign::Top => height - padding - TOP_DROP * size,
        VerticalAlign::Bottom => height - padding - BOTTOM_DROP * size,
    };
    let mut drawing: Vec<Vec<u8>> = vec![b"q".to_vec()];
    if let Some(fill) = free.fill.as_deref().and_then(|c| color(c, false)) {
        drawing.push(format!("{fill} 0 0 {} {} re f", number(width), number(height)).into_bytes());
    }
    if free.border > 0.0 {
        let stroke = free
            .border_color
            .as_deref()
            .and_then(|c| color(c, true))
            .unwrap_or_else(|| "0 G".to_owned());
        let w = free.border;
        drawing.push(
            format!(
                "{stroke} {} w {} {} {} {} re S",
                number(w),
                number(w / 2.0),
                number(w / 2.0),
                number(width - w),
                number(height - w)
            )
            .into_bytes(),
        );
    }
    let fill = color(&free.color, false).unwrap_or_else(|| "0 g".to_owned());
    drawing.push(format!("BT /Helv {} Tf {fill}", number(size)).into_bytes());
    let mut baseline = first_baseline;
    for line in &lines {
        let encoded = text::encode(line, &mut replaced);
        let line_width = text::width(&encoded, size);
        let left = match free.align {
            Align::Left => padding,
            Align::Center => (width - line_width) / 2.0,
            Align::Right => width - padding - line_width,
        };
        drawing.push(tm(left, baseline, &encoded));
        baseline -= leading;
    }
    drawing.extend([b"ET".to_vec(), b"Q".to_vec()]);
    (drawing.join(&b'\n'), replaced)
}

/// The lines drawn: each as given, found in the contents after the previous one, with the spaces
/// skipped to reach it in front of it. A line the contents do not hold as it is (the viewer ends
/// a line with the space it broke at, even the last) is drawn as given.
fn aligned_lines(lines: &[String], contents: &str) -> Vec<String> {
    let characters: Vec<char> = contents.chars().collect();
    let mut at = 0;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let wanted: Vec<char> = line.chars().collect();
        let holds =
            |start: usize| characters.get(start..start + wanted.len()) == Some(wanted.as_slice());
        let mut start = at;
        while start < characters.len() && !holds(start) && characters[start].is_whitespace() {
            start += 1;
        }
        if holds(start) {
            let mut drawn: String = characters[at..start].iter().collect();
            drawn.push_str(line);
            out.push(drawn);
            at = start + wanted.len();
        } else {
            out.push(line.clone());
        }
    }
    out
}

/// Whether the viewer's lines hold the words of the contents, so that they only say where lines
/// break.
fn same_words(lines: &[String], contents: &str) -> bool {
    let words = |text: &str| -> String { text.split_whitespace().collect() };
    !lines.is_empty() && words(&lines.join(" ")) == words(contents)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_comb_field_spreads_its_characters_over_its_cells() {
        let value = "Montreal Quebec";
        let field = TextField {
            lines: vec![value.to_owned()],
            flags: COMB,
            size: 10.0,
            max_length: Some(15),
            ..TextField::default()
        };
        let (drawing, _) = text_field(&field, 240.0, 24.0);
        let drawing = String::from_utf8(drawing).unwrap();
        let lefts: Vec<f64> = drawing
            .lines()
            .filter_map(|line| line.strip_prefix("1 0 0 1 "))
            .map(|rest| rest.split(' ').next().unwrap().parse().unwrap())
            .collect();
        assert_eq!(lefts.len(), value.len());
        let cell = 240.0 / 15.0;
        for (index, left) in lefts.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let index = index as f64;
            assert!(index * cell <= *left && *left < (index + 1.0) * cell);
        }
    }

    #[test]
    fn an_automatic_size_fits_the_field() {
        let field = TextField {
            lines: vec!["Montreal Quebec".to_owned()],
            ..TextField::default()
        };
        let (drawing, _) = text_field(&field, 240.0, 24.0);
        let drawing = String::from_utf8(drawing).unwrap();
        let size: f64 = drawing
            .lines()
            .find_map(|line| line.strip_prefix("BT /Helv "))
            .and_then(|rest| rest.split(' ').next())
            .unwrap()
            .parse()
            .unwrap();
        // 0.85 times the height, within the width.
        assert!((size - 20.4).abs() < 1e-3, "{size}");
    }

    #[test]
    fn a_password_shows_stars_and_a_frame_its_colours() {
        let field = TextField {
            lines: vec!["secret".to_owned()],
            flags: PASSWORD,
            size: 10.0,
            frame: Frame {
                background: Some(vec![1.0, 1.0, 0.0]),
                border: Some(vec![0.0]),
                border_width: 1.0,
                style: BorderStyle::Solid,
            },
            ..TextField::default()
        };
        let (drawing, _) = text_field(&field, 100.0, 20.0);
        let drawing = String::from_utf8(drawing).unwrap();
        assert!(drawing.contains("(******) Tj"));
        assert!(drawing.contains("1 1 0 rg 0 0 100 20 re f"));
        assert!(
            drawing.contains("0 g 0 0 100 20 re 1 1 98 18 re f*"),
            "{drawing}"
        );
    }

    #[test]
    fn a_free_text_is_centred_vertically_and_aligned() {
        let free = crate::xfdf::Text {
            page_number: 1,
            rect: sealwax_core::Rect::from_corners(0.0, 0.0, 120.0, 25.0),
            contents: "Jean".to_owned(),
            lines: Some(vec!["Jean".to_owned()]),
            size: 16.0,
            color: vec![0.0, 0.0, 0.0],
            align: Align::Left,
            vertical: VerticalAlign::Center,
            border: 0.0,
            border_color: None,
            fill: None,
            rotation: 0,
        };
        let (drawing, replaced) = free_text(&free, 120.0, 25.0);
        let drawing = String::from_utf8(drawing).unwrap();
        assert_eq!(replaced, 0);
        assert!(drawing.contains("BT /Helv 16 Tf 0 0 0 rg"));
        assert!(!drawing.contains(" re S"), "no border");
        // 12.5 - 0.6375 x 16, at the left edge.
        assert!(drawing.contains("1 0 0 1 0 2.3 Tm (Jean) Tj"), "{drawing}");
        // Two lines, a border of 1: the block raised by half a line; at the top, a padding of 2.
        let two = crate::xfdf::Text {
            contents: "Hxg Hxg Hxg Hxg".to_owned(),
            lines: Some(vec!["Hxg Hxg".to_owned(), "Hxg Hxg".to_owned()]),
            border: 1.0,
            ..free.clone()
        };
        let drawing = String::from_utf8(free_text(&two, 120.0, 80.0).0).unwrap();
        assert!(drawing.contains("0 G 1 w 0.5 0.5 119 79 re S"), "{drawing}");
        assert!(
            drawing.contains("1 0 0 1 2 39.4 Tm (Hxg Hxg) Tj"),
            "{drawing}"
        );
        // The space the line broke at starts the next one.
        assert!(
            drawing.contains("1 0 0 1 2 20.2 Tm ( Hxg Hxg) Tj"),
            "{drawing}"
        );
        // The viewer's line ends with the space it broke at: drawn as given.
        let trailing = crate::xfdf::Text {
            lines: Some(vec!["Jean ".to_owned()]),
            ..free.clone()
        };
        let drawing = String::from_utf8(free_text(&trailing, 120.0, 25.0).0).unwrap();
        assert!(drawing.contains("Tm (Jean ) Tj"), "{drawing}");
        let top = crate::xfdf::Text {
            vertical: VerticalAlign::Top,
            ..two
        };
        let drawing = String::from_utf8(free_text(&top, 120.0, 80.0).0).unwrap();
        assert!(drawing.contains("1 0 0 1 2 66 Tm"), "{drawing}");
    }
}
