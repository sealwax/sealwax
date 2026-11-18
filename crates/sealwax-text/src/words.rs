//! The words of a page, in lines and blocks, as MuPDF's structured text makes them (what
//! PyMuPDF's `get_text("words")` gives): the glyphs in the order of the content, a line going on
//! while each glyph starts where the one before ended, a space added where a gap is wide enough,
//! and words cut at white space.

use sealwax_core::Document;

use crate::drawn::Bounds;
use crate::glyphs::{TextGlyph, glyphs_and_clips};

/// MuPDF's thresholds (`stext-device.c`), in ems of the glyph's size: a gap along the baseline
/// narrower than [`SPACE_MIN`] adds nothing, a wider one up to [`SPACE_MAX`] a space, a wider one
/// a new line (another column); a step back up to [`SPACE_MAX`] is glyphs overlapping. A glyph
/// farther than [`BASELINE_MAX`] from the baseline starts a new line, farther than
/// [`PARAGRAPH`] a new block.
const SPACE_MIN: f64 = 0.15;
const SPACE_MAX: f64 = 0.8;
const BASELINE_MAX: f64 = 0.8;
const PARAGRAPH: f64 = 1.5;
/// Drawn again closer than this, in ems, the same character is bold faked: counted once.
const FAKE_BOLD: f64 = 0.1;
/// A line starting this much to the right of the one before starts a paragraph (an indent).
const INDENT: f64 = 0.5;
/// Two directions closer than this (their cosine) are one.
const SAME_DIRECTION: f64 = 0.999;

/// A word of a page.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TextWord {
    /// Its text: the glyphs' (an `/ActualText` in their place where the document gives one).
    pub text: String,
    /// The box of its glyphs, in the page's default user space (unrotated, y up).
    pub bbox: Bounds,
    /// Its block, its line in the block and its place in the line, from 0.
    pub block: usize,
    pub line: usize,
    pub word: usize,
}

/// A character of a line: a glyph's, or a space added between two glyphs (no box).
struct Char {
    c: char,
    bbox: Option<Bounds>,
}

struct Line {
    block: usize,
    chars: Vec<Char>,
}

/// The words of page `page_index` (from 0), block by block and line by line, in the order of its
/// content; annotations left out. Glyphs drawn twice in the same place count once, and glyphs
/// outside the media box not at all.
///
/// # Errors
/// As [`glyphs`].
pub fn words(doc: &Document, page_index: usize) -> sealwax_core::Result<Vec<TextWord>> {
    let (found, clips) = glyphs_and_clips(doc, page_index, false)?;
    let media = doc
        .geometry()
        .get(page_index)
        .map(|page| page.media_box)
        .ok_or(sealwax_core::Error::InvalidArgument { what: "page index" })?;
    let media = [media.x1, media.y1, media.x2, media.y2];
    // A glyph wholly outside the clip in effect, or the media box, is not read (MuPDF's
    // `FZ_STEXT_CLIP`, which PyMuPDF's words ask for): what bounds it is its outline, else its
    // origin for a glyph that draws nothing (a space), else its box.
    let found: Vec<TextGlyph> = found
        .into_iter()
        .zip(clips)
        .filter(|(glyph, clip)| {
            let shape = glyph.ink.unwrap_or_else(|| {
                let blank = glyph.text.as_deref().is_some_and(|t| t.trim().is_empty());
                if blank {
                    let [x, y] = glyph.origin;
                    [x, y, x + 1e-7, y + 1e-7]
                } else {
                    glyph.bbox
                }
            });
            let area = clip.map_or(media, |c| {
                [
                    c[0].max(media[0]),
                    c[1].max(media[1]),
                    c[2].min(media[2]),
                    c[3].min(media[3]),
                ]
            });
            !(shape[2] <= area[0]
                || shape[3] <= area[1]
                || shape[0] >= area[2]
                || shape[1] >= area[3])
        })
        .map(|(glyph, _)| glyph)
        .collect();
    let lines = lines(&found);
    let mut words = Vec::new();
    let mut block = usize::MAX;
    let mut line_in_block = 0;
    for line in lines {
        if line.block == block {
            line_in_block += 1;
        } else {
            block = line.block;
            line_in_block = 0;
        }
        let mut word = 0;
        let mut text = String::new();
        let mut bbox: Option<Bounds> = None;
        let mut close =
            |text: &mut String, bbox: &mut Option<Bounds>, words: &mut Vec<TextWord>| {
                if let Some(found) = bbox.take()
                    && !text.is_empty()
                {
                    words.push(TextWord {
                        text: std::mem::take(text),
                        bbox: found,
                        block,
                        line: line_in_block,
                        word,
                    });
                    word += 1;
                }
                text.clear();
            };
        for c in line.chars {
            // PyMuPDF's delimiters: control characters, the space, the no-break space.
            if u32::from(c.c) <= 0x20 || c.c == '\u{A0}' {
                close(&mut text, &mut bbox, &mut words);
                continue;
            }
            text.push(c.c);
            if let Some(b) = c.bbox {
                bbox = Some(bbox.map_or(b, |a| union(a, b)));
            }
        }
        close(&mut text, &mut bbox, &mut words);
    }
    Ok(words)
}

fn union(a: Bounds, b: Bounds) -> Bounds {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// Whether MuPDF adds a space after `last`: not after a space, and only after the scripts that
/// separate words by spaces (`may_add_space`).
fn may_add_space(last: Option<char>) -> bool {
    last.is_some_and(|c| {
        let c = u32::from(c);
        c != 0x20 && (c < 0x700 || (0x2000..=0x20CF).contains(&c))
    })
}

/// Whether `c` is a combining mark that MuPDF puts on the glyph before (general category Mn), in
/// the blocks that hold most of them.
fn is_mark(c: char) -> bool {
    matches!(u32::from(c),
        0x0300..=0x036F | 0x0483..=0x0487 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x0900..=0x0902 | 0x093C | 0x0941..=0x0948 | 0x094D | 0x0951..=0x0957
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20F0 | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F)
}

/// Whether a line starting with `c` may be a list item (`plausible_bullet`): its indent then
/// starts no paragraph.
fn plausible_bullet(c: char) -> bool {
    matches!(u32::from(c),
        0x2A | 0xB7 | 0x2022 | 0x2023 | 0x2043 | 0x204C | 0x204D | 0x2219 | 0x25C9 | 0x25CB
        | 0x25CF | 0x25D8 | 0x25E6 | 0x2619..=0x261F | 0x2765 | 0x2767 | 0x29BE | 0x29BF
        | 0x2660..=0x2667 | 0x1F446..=0x1F449 | 0x1F597..=0x1F5A3 | 0x1FBC1..=0x1FBC4)
}

/// Where the pen stands after the glyphs read.
struct Pen {
    /// Where the last glyph ended, and where it started.
    end: [f64; 2],
    start: [f64; 2],
    /// The line's direction, and where its first glyph started.
    direction: [f64; 2],
    line_start: [f64; 2],
    /// The last character, and whether the line's first may be a bullet.
    last: Option<char>,
    bullet: bool,
}

/// The glyphs `found` in lines, each line's block numbered, as MuPDF's structured text device
/// adds them (`fz_add_stext_char_imp`).
fn lines(found: &[TextGlyph]) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut block = 0;
    let mut pen: Option<Pen> = None;
    for glyph in found {
        if glyph.overdrawn {
            continue;
        }
        let text: Vec<char> = match (&glyph.actual_text, &glyph.text) {
            (Some(actual), _) => actual.chars().collect(),
            (None, Some(text)) => text.chars().collect(),
            // No Unicode for it: a character still, as MuPDF has one.
            (None, None) => vec!['\u{FFFD}'],
        };
        let size = if glyph.size > 0.0 { glyph.size } else { 1.0 };
        let Some(&first) = text.first() else {
            // Covered by an `/ActualText` given on a glyph before: its box joins that text.
            if let Some(last) = lines.last_mut().and_then(|line| line.chars.last_mut())
                && !last.c.is_whitespace()
            {
                last.bbox = Some(last.bbox.map_or(glyph.bbox, |b| union(b, glyph.bbox)));
            }
            if let Some(pen) = pen.as_mut() {
                pen.start = glyph.origin;
                pen.end = glyph.end;
            }
            continue;
        };
        // A combining mark goes on the glyph before, the pen where it was.
        if is_mark(first)
            && let Some(line) = lines.last_mut()
        {
            line.chars.extend(text.iter().map(|&c| Char {
                c,
                bbox: Some(glyph.bbox),
            }));
            if let Some(pen) = pen.as_mut() {
                pen.last = text.last().copied();
            }
            continue;
        }
        let [dx, dy] = glyph.direction;
        let mut new_block = false;
        let mut new_line = true;
        let mut space = false;
        match &pen {
            Some(at) if dx * at.direction[0] + dy * at.direction[1] >= SAME_DIRECTION => {
                let moved = ((glyph.origin[0] - at.start[0]).powi(2)
                    + (glyph.origin[1] - at.start[1]).powi(2))
                .sqrt()
                    / size;
                if moved < FAKE_BOLD && at.last == Some(first) {
                    continue;
                }
                let delta = [glyph.origin[0] - at.end[0], glyph.origin[1] - at.end[1]];
                let along = (at.direction[0] * delta[0] + at.direction[1] * delta[1]) / size;
                let across = (-at.direction[1] * delta[0] + at.direction[0] * delta[1]) / size;
                if across.abs() < BASELINE_MAX {
                    if along.abs() < SPACE_MIN || (along < 0.0 && along > -SPACE_MAX) {
                        new_line = false;
                    } else if along > 0.0 && along < SPACE_MAX {
                        space = may_add_space(at.last);
                        new_line = false;
                    }
                } else if across.abs() <= PARAGRAPH {
                    // A line indented more than the one before starts a paragraph.
                    new_block = glyph.origin[0] - at.line_start[0] > INDENT && !at.bullet;
                } else {
                    new_block = true;
                }
            }
            _ => new_block = true,
        }
        if new_block && !lines.is_empty() {
            block += 1;
        }
        if new_line || lines.is_empty() {
            lines.push(Line {
                block,
                chars: Vec::new(),
            });
        }
        let Some(line) = lines.last_mut() else {
            continue;
        };
        if space && first != ' ' {
            line.chars.push(Char { c: ' ', bbox: None });
        }
        line.chars.extend(text.iter().map(|&c| Char {
            c,
            bbox: Some(glyph.bbox),
        }));
        let (line_start, bullet) = match (&pen, new_line) {
            (Some(at), false) => (at.line_start, at.bullet),
            _ => (glyph.origin, plausible_bullet(first)),
        };
        pen = Some(Pen {
            end: glyph.end,
            start: glyph.origin,
            direction: glyph.direction,
            line_start,
            last: text.last().copied(),
            bullet,
        });
    }
    lines
}
