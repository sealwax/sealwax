//! What a page shows, from what its units draw: lines of text gathered into paragraphs, figures,
//! and the order a reader takes them in.
//!
//! The unit of content is the atom: a text-showing operator belongs to one paragraph whole, by
//! its median baseline and its box. Rows are units whose type overlaps (a superscript joins its
//! line), cut into lines where a gap is wider than the type is tall; paragraphs are lines that
//! follow one another at a constant spacing, in about the same size, without a bullet or a
//! number at their start, each reaching the right edge but the last. The reading order is a
//! recursive XY cut: bands from top to bottom where whitespace crosses the whole region, then
//! columns from left to right, and so on.

use sealwax_text::{Bounds, Drawn};

/// How a unit drew, as layout sees it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Seen {
    /// Glyphs, upright or not: their boxes, the size of the type, the baseline, the text.
    Text(Text),
    /// An image, without text: its box, and the description a sequence around it gives.
    Image(Bounds, Option<Vec<u8>>),
    /// Paths only, or nothing visible.
    Drawing,
    /// A text-showing operator that drew no glyph hayro could read: it follows the text
    /// around it in the content.
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Text {
    pub(crate) bounds: Bounds,
    pub(crate) size: f64,
    pub(crate) baseline: f64,
    /// The text runs left to right, upright on the page.
    pub(crate) upright: bool,
    pub(crate) text: String,
}

/// What a paragraph or a figure is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Paragraph,
    Figure,
    /// An image behind text: a scan under its OCR layer, a form's background.
    Background,
}

/// A paragraph or a figure: its units, in content order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub(crate) role: Role,
    pub(crate) units: Vec<usize>,
    pub(crate) bounds: Bounds,
    /// The text of a paragraph, in content order (to find running heads and page numbers).
    pub(crate) text: String,
    /// The description of a figure, when the content gives one.
    pub(crate) alt: Option<Vec<u8>>,
}

/// The size of a glyph with no size worth the name.
const MIN_SIZE: f64 = 0.5;

/// What a unit drew, as layout sees it; `image` for an inline image, which the analysis draws
/// as the square path it fills.
pub(crate) fn seen(drawn: &Drawn, image: bool) -> Seen {
    let glyphs: Vec<_> = drawn
        .glyphs
        .iter()
        .filter(|glyph| (glyph.up[0].hypot(glyph.up[1])) >= MIN_SIZE)
        .collect();
    if !glyphs.is_empty() {
        let mut sizes: Vec<f64> = glyphs.iter().map(|g| g.up[0].hypot(g.up[1])).collect();
        sizes.sort_by(f64::total_cmp);
        let size = sizes[sizes.len() / 2];
        let mut baselines: Vec<f64> = glyphs.iter().map(|g| g.origin[1]).collect();
        baselines.sort_by(f64::total_cmp);
        let upright = glyphs
            .iter()
            .all(|g| g.up[1] > 0.0 && g.up[0].abs() <= g.up[1] * 0.05);
        let mut bounds: Option<Bounds> = None;
        let mut text = String::new();
        for glyph in &glyphs {
            let em = glyph.up[0].hypot(glyph.up[1]);
            let (x, y) = (glyph.origin[0], glyph.origin[1]);
            let cell = if upright {
                let right = glyph.ink.map_or(x + em * 0.3, |ink| ink[2].max(x));
                [
                    x.min(glyph.ink.map_or(x, |ink| ink[0])),
                    y - em * 0.2,
                    right,
                    y + em * 0.8,
                ]
            } else {
                glyph.ink.unwrap_or([x, y, x, y])
            };
            bounds = Some(bounds.map_or(cell, |b| union(b, cell)));
            if let Some(t) = &glyph.text {
                text.push_str(t);
            }
        }
        return Seen::Text(Text {
            bounds: bounds.unwrap_or_default(),
            size,
            baseline: baselines[baselines.len() / 2],
            upright,
            text,
        });
    }
    if image {
        // An inline image is drawn, for the analysis, as the unit square it fills.
        let boxes = drawn.images.iter().chain(&drawn.paths);
        if let Some(found) = boxes.copied().reduce(union) {
            return Seen::Image(found, drawn.alt.clone());
        }
    }
    if let Some(found) = drawn.images.iter().copied().reduce(union) {
        return Seen::Image(found, drawn.alt.clone());
    }
    Seen::Drawing
}

pub(crate) fn union(a: Bounds, b: Bounds) -> Bounds {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn area(b: Bounds) -> f64 {
    (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0)
}

fn intersection(a: Bounds, b: Bounds) -> f64 {
    area([
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ])
}

/// A line of upright text: its main type's size and baseline, and how it starts.
struct Line {
    units: Vec<usize>,
    bounds: Bounds,
    size: f64,
    baseline: f64,
    first: String,
}

/// A paragraph being gathered.
struct Paragraph {
    units: Vec<usize>,
    bounds: Bounds,
    size: f64,
    last_baseline: f64,
    /// Where its last line ends, and the furthest any of its lines does.
    last_right: f64,
    max_right: f64,
    /// From its first line to its second.
    spacing: Option<f64>,
}

impl Paragraph {
    fn continues_with(&self, line: &Line) -> bool {
        let size = self.size.max(line.size);
        let spacing = self.last_baseline - line.baseline;
        let at_spacing = match self.spacing {
            None => spacing <= size * 1.45,
            Some(first) => (spacing - first).abs() <= size * 0.25,
        };
        // Text that wraps fills each line but its last to about the same right edge.
        let right = self.max_right.max(line.bounds[2]);
        let width = right - self.bounds[0].min(line.bounds[0]);
        at_spacing
            && ratio(self.size, line.size) < 1.15
            && !starts_item(&line.first)
            && self.last_right >= right - (size * 5.0).max(width * 0.2)
    }
}

/// The vertical extent of the type of `text`: from its descenders to its ascenders, roughly.
fn em(text: &Text) -> (f64, f64) {
    (
        text.baseline - text.size * 0.2,
        text.baseline + text.size * 0.8,
    )
}

/// A line starting so starts a list item: a bullet, or a number or letter followed by `.` or `)`.
fn starts_item(first: &str) -> bool {
    let mut chars = first.chars();
    let Some(c) = chars.next() else {
        return false;
    };
    if matches!(
        c,
        '\u{2022}'
            | '\u{25CF}'
            | '\u{25CB}'
            | '\u{25E6}'
            | '\u{25AA}'
            | '\u{25A0}'
            | '\u{25A1}'
            | '\u{25BA}'
            | '\u{25B8}'
            | '\u{2023}'
            | '\u{2043}'
            | '\u{2013}'
            | '-'
            | '*'
            | '\u{00B7}'
            | '\u{2713}'
            | '\u{2714}'
    ) {
        return true;
    }
    let rest: String = std::iter::once(c).chain(chars).collect();
    let rest = rest.strip_prefix('(').unwrap_or(&rest);
    let label: String = rest
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    !label.is_empty()
        && label.len() <= 3
        && matches!(rest[label.len()..].chars().next(), Some('.' | ')'))
}

/// The paragraphs and figures of a page, in reading order. `seen` has one entry per unit, in
/// content order; units that are `Drawing` are left out (artifacts). `page` is the crop box.
pub(crate) fn layout(seen: &[Option<Seen>], page: Bounds) -> Vec<Item> {
    let page_area = area(page).max(1.0);
    let mut items: Vec<Item> = Vec::new();
    let texts: Vec<(usize, &Text)> = seen
        .iter()
        .enumerate()
        .filter_map(|(unit, seen)| match seen {
            Some(Seen::Text(text)) => Some((unit, text)),
            _ => None,
        })
        .collect();
    // Upright text: rows of units whose type overlaps the row's main type by half its height
    // (a superscript joins its line), cut into lines where a gap is wider than the type is tall.
    let mut upright: Vec<(usize, &Text)> =
        texts.iter().copied().filter(|(_, t)| t.upright).collect();
    upright.sort_by(|a, b| {
        b.1.baseline
            .total_cmp(&a.1.baseline)
            .then(a.1.bounds[0].total_cmp(&b.1.bounds[0]))
    });
    let mut rows: Vec<(&Text, Vec<(usize, &Text)>)> = Vec::new();
    for (unit, text) in upright {
        if let Some((main, members)) = rows.last_mut() {
            let (low, high) = em(text);
            let (main_low, main_high) = em(main);
            let overlap = high.min(main_high) - low.max(main_low);
            if overlap >= (high - low).min(main_high - main_low) * 0.5 {
                if text.size > main.size {
                    *main = text;
                }
                members.push((unit, text));
                continue;
            }
        }
        rows.push((text, vec![(unit, text)]));
    }
    let mut lines: Vec<Line> = Vec::new();
    for (main, mut row) in rows {
        row.sort_by(|a, b| a.1.bounds[0].total_cmp(&b.1.bounds[0]));
        let mut current: Option<Line> = None;
        for (unit, text) in row {
            if let Some(line) = current.as_mut()
                && text.bounds[0] - line.bounds[2] <= main.size
            {
                line.units.push(unit);
                line.bounds = union(line.bounds, text.bounds);
                continue;
            }
            lines.extend(current.take());
            current = Some(Line {
                units: vec![unit],
                bounds: text.bounds,
                size: main.size,
                baseline: main.baseline,
                first: text.text.trim_start().chars().take(6).collect(),
            });
        }
        lines.extend(current);
    }
    // Paragraphs: from the top, a line joins the paragraph above it that it overlaps sideways,
    // when it follows at the paragraph's line spacing (at most 1.45 times the type for its second
    // line), in a type of about the same size, does not start a list item, and the line above did
    // not end short of it (the last line of a paragraph).
    lines.sort_by(|a, b| {
        b.baseline
            .total_cmp(&a.baseline)
            .then(a.bounds[0].total_cmp(&b.bounds[0]))
    });
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    for line in lines {
        let above = paragraphs.iter_mut().rev().find(|paragraph| {
            line.bounds[0] < paragraph.bounds[2]
                && paragraph.bounds[0] < line.bounds[2]
                && paragraph.last_baseline > line.baseline
        });
        if let Some(paragraph) = above
            && paragraph.continues_with(&line)
        {
            let spacing = paragraph.last_baseline - line.baseline;
            paragraph.spacing.get_or_insert(spacing);
            paragraph.units.extend(&line.units);
            paragraph.bounds = union(paragraph.bounds, line.bounds);
            paragraph.last_baseline = line.baseline;
            paragraph.last_right = line.bounds[2];
            paragraph.max_right = paragraph.max_right.max(line.bounds[2]);
            continue;
        }
        paragraphs.push(Paragraph {
            units: line.units,
            bounds: line.bounds,
            size: line.size,
            last_baseline: line.baseline,
            last_right: line.bounds[2],
            max_right: line.bounds[2],
            spacing: None,
        });
    }
    for paragraph in paragraphs {
        items.push(Item {
            role: Role::Paragraph,
            units: paragraph.units,
            bounds: paragraph.bounds,
            text: String::new(),
            alt: None,
        });
    }
    // Text that is not upright: one paragraph per unit.
    for (unit, text) in texts.iter().filter(|(_, t)| !t.upright) {
        items.push(Item {
            role: Role::Paragraph,
            units: vec![*unit],
            bounds: text.bounds,
            text: String::new(),
            alt: None,
        });
    }
    // Images: a figure each, or the background of text they lie under.
    let text_boxes: Vec<Bounds> = texts.iter().map(|(_, t)| t.bounds).collect();
    for (unit, seen) in seen.iter().enumerate() {
        let Some(Seen::Image(bounds, alt)) = seen else {
            continue;
        };
        let large = area(*bounds) >= page_area * 0.5;
        let under_text = text_boxes
            .iter()
            .any(|text| intersection(*text, *bounds) >= area(*text) * 0.5);
        items.push(Item {
            role: if large && under_text {
                Role::Background
            } else {
                Role::Figure
            },
            units: vec![unit],
            bounds: *bounds,
            text: String::new(),
            alt: alt.clone(),
        });
    }
    // Text drawn by no glyph hayro reads goes with the text before it in the content, or after.
    let owner = |unit: usize, items: &[Item]| {
        items
            .iter()
            .position(|item| item.role == Role::Paragraph && item.units.contains(&unit))
    };
    let unknown: Vec<usize> = seen
        .iter()
        .enumerate()
        .filter(|(_, seen)| matches!(seen, Some(Seen::Unknown)))
        .map(|(unit, _)| unit)
        .collect();
    for unit in unknown {
        let neighbour = (0..unit)
            .rev()
            .find_map(|other| owner(other, &items))
            .or_else(|| (unit + 1..seen.len()).find_map(|other| owner(other, &items)));
        match neighbour {
            Some(index) => items[index].units.push(unit),
            None => items.push(Item {
                role: Role::Paragraph,
                units: vec![unit],
                bounds: page,
                text: String::new(),
                alt: None,
            }),
        }
    }
    for item in &mut items {
        item.units.sort_unstable();
        item.units.dedup();
        if item.role == Role::Paragraph {
            item.text = item
                .units
                .iter()
                .filter_map(|unit| match &seen[*unit] {
                    Some(Seen::Text(text)) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect();
        }
    }
    let order = reading_order(&items);
    order
        .into_iter()
        .map(|index| items[index].clone())
        .collect()
}

fn ratio(a: f64, b: f64) -> f64 {
    let (low, high) = if a < b { (a, b) } else { (b, a) };
    if low <= 0.0 {
        f64::INFINITY
    } else {
        high / low
    }
}

/// How many items an XY cut sorts; beyond, items go top to bottom, left to right.
const MAX_CUT_ITEMS: usize = 20_000;

/// The order of `items`: a recursive XY cut, bands first.
fn reading_order(items: &[Item]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..items.len()).collect();
    if items.len() > MAX_CUT_ITEMS {
        indices.sort_by(|a, b| {
            items[*b].bounds[3]
                .total_cmp(&items[*a].bounds[3])
                .then(items[*a].bounds[0].total_cmp(&items[*b].bounds[0]))
        });
        return indices;
    }
    let mut out = Vec::with_capacity(items.len());
    cut(items, indices, true, &mut out, 0);
    out
}

/// Split `indices` into groups separated by whitespace along one axis: bands top to bottom
/// (`horizontal`), or columns left to right.
fn groups(items: &[Item], mut indices: Vec<usize>, horizontal: bool) -> Vec<Vec<usize>> {
    // Along the axis: low and high edge, in reading direction (top first for bands).
    let span = |index: &usize| {
        let b = items[*index].bounds;
        if horizontal {
            (-b[3], -b[1])
        } else {
            (b[0], b[2])
        }
    };
    indices.sort_by(|a, b| span(a).0.total_cmp(&span(b).0));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut reach = f64::NEG_INFINITY;
    for index in indices {
        let (low, high) = span(&index);
        match groups.last_mut() {
            Some(group) if low < reach => group.push(index),
            _ => groups.push(vec![index]),
        }
        reach = reach.max(high);
    }
    groups
}

/// How deep the cut goes: past it, items keep the order they are in.
const MAX_CUT_DEPTH: usize = 64;

fn cut(items: &[Item], indices: Vec<usize>, horizontal: bool, out: &mut Vec<usize>, depth: usize) {
    if indices.len() <= 1 || depth > MAX_CUT_DEPTH {
        out.extend(indices);
        return;
    }
    let first = groups(items, indices.clone(), horizontal);
    if first.len() > 1 {
        for group in first {
            cut(items, group, !horizontal, out, depth + 1);
        }
        return;
    }
    let second = groups(items, indices.clone(), !horizontal);
    if second.len() > 1 {
        for group in second {
            cut(items, group, horizontal, out, depth + 1);
        }
        return;
    }
    // Nothing separates them: overlapping items, top to bottom, then left to right.
    let mut rest = indices;
    rest.sort_by(|a, b| {
        items[*b].bounds[3]
            .total_cmp(&items[*a].bounds[3])
            .then(items[*a].bounds[0].total_cmp(&items[*b].bounds[0]))
    });
    out.extend(rest);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Role, Seen, Text, layout};

    fn text(x: f64, baseline: f64, width: f64, size: f64) -> Option<Seen> {
        Some(Seen::Text(Text {
            bounds: [x, baseline - size * 0.2, x + width, baseline + size * 0.8],
            size,
            baseline,
            upright: true,
            text: String::new(),
        }))
    }

    fn words(x: f64, baseline: f64, width: f64, size: f64, words: &str) -> Option<Seen> {
        let mut seen = text(x, baseline, width, size);
        if let Some(Seen::Text(text)) = seen.as_mut() {
            text.text = words.to_owned();
        }
        seen
    }

    const PAGE: [f64; 4] = [0.0, 0.0, 600.0, 800.0];

    fn paragraphs(seen: &[Option<Seen>]) -> Vec<Vec<usize>> {
        layout(seen, PAGE)
            .iter()
            .map(|item| item.units.clone())
            .collect()
    }

    #[test]
    fn a_superscript_stays_in_its_line() {
        let seen = [
            text(50.0, 700.0, 200.0, 10.0),
            // A note call, raised and smaller, right after the first operator.
            text(250.5, 703.5, 6.0, 7.0),
            text(50.0, 688.0, 200.0, 10.0),
        ];
        assert_eq!(paragraphs(&seen), [vec![0, 1, 2]]);
    }

    #[test]
    fn list_items_short_lines_and_headings_start_paragraphs() {
        let seen = [
            // A heading, then a paragraph whose last line ends short, then a list.
            words(50.0, 720.0, 120.0, 14.0, "Title"),
            words(50.0, 700.0, 300.0, 10.0, "The first"),
            words(50.0, 688.0, 100.0, 10.0, "ends here."),
            words(50.0, 676.0, 300.0, 10.0, "Another one"),
            words(50.0, 664.0, 300.0, 10.0, "\u{2022} An item"),
            words(50.0, 652.0, 300.0, 10.0, "2. Another item"),
            words(50.0, 640.0, 300.0, 10.0, "that goes on"),
        ];
        assert_eq!(
            paragraphs(&seen),
            [vec![0], vec![1, 2], vec![3], vec![4], vec![5, 6]]
        );
    }

    #[test]
    fn lines_at_a_line_spacing_make_one_paragraph() {
        // Two lines of a paragraph, each in two operators, then another paragraph after a gap.
        let seen = [
            text(50.0, 700.0, 100.0, 10.0),
            text(152.0, 700.0, 100.0, 10.0),
            text(50.0, 688.0, 150.0, 10.0),
            text(201.0, 688.0, 50.0, 10.0),
            text(50.0, 650.0, 200.0, 10.0),
        ];
        let items = layout(&seen, PAGE);
        let units: Vec<Vec<usize>> = items.iter().map(|item| item.units.clone()).collect();
        assert_eq!(units, [vec![0, 1, 2, 3], vec![4]]);
    }

    #[test]
    fn columns_are_read_one_after_the_other_under_a_heading() {
        let seen = [
            // A heading across the page, drawn last.
            text(50.0, 450.0, 400.0, 10.0),
            text(50.0, 400.0, 150.0, 10.0),
            text(50.0, 388.0, 150.0, 10.0),
            text(300.0, 400.0, 150.0, 10.0),
            text(300.0, 388.0, 150.0, 10.0),
            text(50.0, 500.0, 400.0, 16.0),
        ];
        let items = layout(&seen, PAGE);
        let units: Vec<Vec<usize>> = items.iter().map(|item| item.units.clone()).collect();
        assert_eq!(units, [vec![5], vec![0], vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn a_table_is_read_row_by_row() {
        let seen = [
            text(50.0, 400.0, 80.0, 10.0),
            text(50.0, 370.0, 80.0, 10.0),
            text(300.0, 400.0, 80.0, 10.0),
            text(300.0, 370.0, 80.0, 10.0),
        ];
        let items = layout(&seen, PAGE);
        let units: Vec<Vec<usize>> = items.iter().map(|item| item.units.clone()).collect();
        assert_eq!(units, [vec![0], vec![2], vec![1], vec![3]]);
    }

    #[test]
    fn images_are_figures_or_the_background_of_text() {
        let seen = [
            Some(Seen::Image([0.0, 0.0, 600.0, 800.0], None)),
            text(50.0, 700.0, 100.0, 10.0),
            Some(Seen::Image(
                [50.0, 100.0, 150.0, 200.0],
                Some(b"A picture".to_vec()),
            )),
            Some(Seen::Drawing),
            Some(Seen::Unknown),
        ];
        let items = layout(&seen, PAGE);
        let roles: Vec<(Role, Vec<usize>)> = items
            .iter()
            .map(|item| (item.role, item.units.clone()))
            .collect();
        assert!(roles.contains(&(Role::Background, vec![0])));
        assert!(roles.contains(&(Role::Figure, vec![2])));
        let figure = items.iter().find(|item| item.role == Role::Figure).unwrap();
        assert_eq!(figure.alt.as_deref(), Some(&b"A picture"[..]));
        // The unknown text follows the text before it; the drawing is no item.
        assert!(roles.contains(&(Role::Paragraph, vec![1, 4])));
        assert_eq!(roles.len(), 3);
    }
}
