//! Words and lines in PDF painting order, measured in the writing direction of each run.

use hayro_interpret::kurbo::{Point, Vec2};
use sealwax_core::{Document, Error, Result};

use crate::actual::Key;
use crate::glyphs::visit_glyphs;
use crate::{Bounds, TextGlyph};

/// A highlight in default PDF user space. Corners are top left, top right, bottom left,
/// bottom right relative to the direction of writing.
pub type TextQuad = [[f64; 2]; 4];

/// A word and its zero-based indices in the page, block and line.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Word {
    pub text: String,
    pub bbox: Bounds,
    pub block: usize,
    pub line: usize,
    pub word: usize,
}

/// One line in content order. Its direction is a unit vector in PDF user space.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TextLine {
    pub bbox: Bounds,
    pub direction: [f64; 2],
    pub words: Vec<Word>,
}

/// Consecutive lines that share a direction and lie near each other.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TextBlock {
    pub bbox: Bounds,
    pub lines: Vec<TextLine>,
}

/// Extract the page's words in content order, optionally including annotation appearances.
///
/// # Errors
/// Returns page index, document limit and deadline errors.
pub fn words(doc: &Document, page: usize, annotations: bool) -> Result<Vec<Word>> {
    Ok(text_blocks(doc, page, annotations)?
        .into_iter()
        .flat_map(|block| block.lines)
        .flat_map(|line| line.words)
        .collect())
}

/// Extract the page's blocks, lines and words, including invisible text and replacements.
///
/// # Errors
/// Returns page index, document limit and deadline errors.
pub fn text_blocks(doc: &Document, page: usize, annotations: bool) -> Result<Vec<TextBlock>> {
    Ok(layout(doc, page, annotations, false)?.blocks)
}

/// Find non-overlapping occurrences of a phrase, ignoring case and folding whitespace.
/// Search omits a hyphen that wraps between lines of a block. Each occurrence
/// has a highlight for each of its lines, covering the matching parts of words.
///
/// # Errors
/// Returns page index, document limit and deadline errors.
pub fn search(
    doc: &Document,
    page: usize,
    needle: &str,
    annotations: bool,
) -> Result<Vec<Vec<TextQuad>>> {
    let layout = layout(doc, page, annotations, true)?;
    layout.index.find(doc, needle)
}

#[derive(Clone, Copy)]
struct Placement {
    origin: Point,
    end: Point,
    direction: Vec2,
    size: f64,
    annotation: bool,
}

impl Placement {
    fn from_glyph(g: &TextGlyph) -> Self {
        Self {
            origin: Point::new(g.origin[0], g.origin[1]),
            end: Point::new(g.end[0], g.end[1]),
            direction: Vec2::new(g.direction[0], g.direction[1]),
            size: g.size,
            annotation: g.annotation,
        }
    }

    fn gap(self, next: Self) -> Break {
        // Measure the unpainted advance and the baseline offset in ems. Thus the same
        // decisions apply to downward and rotated writing as to horizontal writing.
        let delta = next.origin - self.end;
        let along = delta.dot(self.direction);
        let across = delta.cross(self.direction).abs();
        let em = self.size.max(next.size).max(f64::EPSILON);
        if self.annotation != next.annotation
            || self.direction.dot(next.direction) < 0.995
            || across > 1.6 * em
        {
            Break::Block
        } else if across > 0.4 * em || !(-0.5 * em..0.9 * em).contains(&along) {
            Break::Line
        } else if along > 0.15 * em {
            Break::Word
        } else {
            Break::None
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Break {
    None,
    Word,
    Line,
    Block,
}

type Corners = [Point; 4];

fn xy(p: Point) -> [f64; 2] {
    [p.x, p.y]
}

fn bounds(corners: Corners) -> Bounds {
    let [first, rest @ ..] = corners;
    rest.iter()
        .fold([first.x, first.y, first.x, first.y], |b, p| {
            [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)]
        })
}

fn include(b: &mut Bounds, c: Bounds) {
    *b = [
        b[0].min(c[0]),
        b[1].min(c[1]),
        b[2].max(c[2]),
        b[3].max(c[3]),
    ];
}

/// Enclose corners in the first glyph's coordinate system. Its up axis can be slanted
/// or reflected. Taking page-aligned bounds first would lose the rotation and shear.
struct Envelope {
    anchor: Point,
    along: Vec2,
    up: Vec2,
    extents: Bounds,
}

impl Envelope {
    fn new(corners: Corners, along: Vec2) -> Self {
        let mut up = (corners[0] - corners[2]).normalize();
        if !up.x.is_finite() || !up.y.is_finite() || along.cross(up).abs() < 1e-9 {
            up = Vec2::new(-along.y, along.x);
        }
        let mut result = Self {
            anchor: corners[2],
            along,
            up,
            extents: [0.0; 4],
        };
        result.extend(corners);
        result
    }

    fn extend(&mut self, corners: Corners) {
        let det = self.along.cross(self.up);
        let local = corners.map(|point| {
            let v = point - self.anchor;
            Point::new(v.cross(self.up) / det, self.along.cross(v) / det)
        });
        include(&mut self.extents, bounds(local));
    }

    fn corners(&self) -> Corners {
        let [x0, y0, x1, y1] = self.extents;
        let point = |x, y| self.anchor + self.along * x + self.up * y;
        [point(x0, y1), point(x1, y1), point(x0, y0), point(x1, y0)]
    }
}

/// Adjacent source glyphs covered by one ActualText can represent a different number
/// of logical characters. Allocate them across the covered rows in proportion to the
/// number of source glyphs, dividing each row in its own writing direction.
struct Replacement {
    key: Key,
    text: String,
    rows: Vec<Row>,
    glyphs: usize,
}

struct Row {
    place: Placement,
    area: Envelope,
    glyphs: usize,
}

impl Replacement {
    fn push(&mut self, place: Placement, corners: Corners) {
        if let Some(row) = self.rows.last_mut()
            && row.place.gap(place) < Break::Line
        {
            row.glyphs += 1;
            row.area.extend(corners);
            row.place.end = place.end;
        } else {
            self.rows.push(Row {
                place,
                area: Envelope::new(corners, place.direction),
                glyphs: 1,
            });
        }
        self.glyphs += 1;
    }
}

struct Layout<'d> {
    doc: &'d Document,
    crop: Bounds,
    blocks: Vec<TextBlock>,
    index: SearchIndex,
    searching: bool,
    replacement: Option<Replacement>,
    previous: Option<Placement>,
    /// The first visible letter of the current line and its height across the baseline.
    line_start: Option<(Point, f64)>,
    next_break: Break,
    characters: u64,
}

fn layout(doc: &Document, page: usize, annotations: bool, searching: bool) -> Result<Layout<'_>> {
    let pages = doc.hayro().pages();
    let crop = pages
        .get(page)
        .ok_or(Error::InvalidArgument { what: "page index" })?
        .intersected_crop_box();
    let mut layout = Layout {
        doc,
        crop: [crop.x0, crop.y0, crop.x1, crop.y1],
        blocks: Vec::new(),
        index: SearchIndex::default(),
        searching,
        replacement: None,
        previous: None,
        line_start: None,
        next_break: Break::Block,
        characters: 0,
    };
    visit_glyphs(doc, page, annotations, false, &mut |glyph, corners, key| {
        layout.glyph(glyph, corners, key)
    })?;
    layout.finish_replacement()?;
    doc.check_time()?;
    Ok(layout)
}

impl Layout<'_> {
    fn glyph(&mut self, glyph: TextGlyph, corners: Corners, key: Option<&Key>) -> Result<()> {
        let place = Placement::from_glyph(&glyph);
        if self
            .replacement
            .as_ref()
            .is_some_and(|r| Some(&r.key) != key)
        {
            self.finish_replacement()?;
        }
        if let Some(key) = key {
            if let Some(text) = glyph.actual_text.filter(|t| !t.is_empty()) {
                self.replacement = Some(Replacement {
                    key: key.clone(),
                    text,
                    rows: Vec::new(),
                    glyphs: 0,
                });
            }
            if let Some(r) = &mut self.replacement
                && (!glyph.overdrawn || r.glyphs == 0)
            {
                r.push(place, corners);
            }
        } else if !glyph.overdrawn
            && let Some(text) = glyph.text
        {
            self.run(place, corners, &text)?;
        }
        Ok(())
    }

    fn finish_replacement(&mut self) -> Result<()> {
        if let Some(r) = self.replacement.take() {
            let total = r.text.chars().count();
            let mut chars = r.text.char_indices();
            let (mut glyphs, mut used, mut start) = (0, 0, 0);
            for row in r.rows {
                glyphs += row.glyphs;
                // Round to the nearest character at the boundary between two rows.
                let until = ((total as u128 * glyphs as u128 + r.glyphs as u128 / 2)
                    / r.glyphs as u128) as usize;
                let mut end = start;
                for (at, ch) in chars.by_ref().take(until - used) {
                    end = at + ch.len_utf8();
                }
                if end > start {
                    self.run(row.place, row.area.corners(), &r.text[start..end])?;
                }
                used = until;
                start = end;
            }
        }
        Ok(())
    }

    fn run(&mut self, place: Placement, corners: Corners, text: &str) -> Result<()> {
        let mut visible = false;
        let count = text.chars().count();
        // A ToUnicode or ActualText can expand one painted glyph to a long string.
        if self.characters.saturating_add(count as u64) > self.doc.limits().max_page_cost {
            return Err(Error::LimitExceeded {
                what: "max_page_cost",
            });
        }
        for (i, ch) in text.chars().enumerate() {
            self.characters += 1;
            if self.characters.is_multiple_of(4096) {
                self.doc.check_time()?;
            }
            let cell = if count == 1 {
                corners
            } else {
                let [tl, tr, bl, br] = corners;
                let from = i as f64 / count as f64;
                let until = (i + 1) as f64 / count as f64;
                [
                    tl.lerp(tr, from),
                    tl.lerp(tr, until),
                    bl.lerp(br, from),
                    bl.lerp(br, until),
                ]
            };
            let bbox = bounds(cell);
            let [left, bottom, right, top] = self.crop;
            if bbox[2] < left || bbox[3] < bottom || bbox[0] > right || bbox[1] > top {
                continue;
            }
            if !visible {
                if let Some(previous) = self.previous {
                    self.next_break = self.next_break.max(previous.gap(place));
                }
                self.previous = Some(place);
                visible = true;
            }
            if ch.is_whitespace() {
                let boundary = match ch {
                    '\r' | '\n' | '\u{2028}' => Break::Line,
                    '\u{2029}' => Break::Block,
                    _ => Break::Word,
                };
                self.next_break = self.next_break.max(boundary);
                continue;
            }
            if self.next_break >= Break::Line {
                // A new column returns upward; a paragraph can begin below with an
                // indent. Compare line starts, since comparing to the previous word's
                // end would turn ragged right margins into paragraph boundaries.
                let start = place.origin.lerp(place.end, i as f64 / count as f64);
                if let Some((previous, height)) = self.line_start {
                    let delta = start - previous;
                    let rise = place.direction.cross(delta);
                    let indent = delta.dot(place.direction);
                    if self.next_break == Break::Line
                        && ((rise > 0.6 * height && indent > 0.1 * height)
                            || (rise < -0.6 * height && indent > 0.5 * height))
                    {
                        self.next_break = Break::Block;
                    }
                }
                let height = (cell[0] - cell[2]).cross(place.direction).abs();
                self.line_start = Some((start, height.max(place.size)));
            }
            if self.searching {
                self.index.push(ch, cell, place.direction, self.next_break);
            } else {
                self.letter(ch, bbox, place.direction);
            }
            self.next_break = Break::None;
        }
        Ok(())
    }

    fn letter(&mut self, ch: char, bbox: Bounds, dir: Vec2) {
        if self.next_break == Break::Block {
            self.blocks.push(TextBlock {
                bbox,
                lines: Vec::new(),
            });
        }
        // A page starts with a block break. Each new block starts a line and a word.
        let b = self.blocks.len() - 1;
        let block = &mut self.blocks[b];
        include(&mut block.bbox, bbox);
        if self.next_break >= Break::Line {
            block.lines.push(TextLine {
                bbox,
                direction: [dir.x, dir.y],
                words: Vec::new(),
            });
        }
        let l = block.lines.len() - 1;
        let line = &mut block.lines[l];
        include(&mut line.bbox, bbox);
        if self.next_break >= Break::Word {
            line.words.push(Word {
                text: String::new(),
                bbox,
                block: b,
                line: l,
                word: line.words.len(),
            });
        }
        if let Some(word) = line.words.last_mut() {
            include(&mut word.bbox, bbox);
            word.text.push(ch);
        }
    }
}

#[derive(Default)]
struct SearchIndex {
    text: String,
    letters: Vec<Letter>,
    line: usize,
    hyphen: bool,
}

struct Letter {
    start: usize,
    end: usize,
    line: usize,
    direction: Vec2,
    quad: Corners,
}

impl SearchIndex {
    fn push(&mut self, ch: char, quad: Corners, dir: Vec2, boundary: Break) {
        if boundary >= Break::Line {
            self.line += 1;
        }
        if boundary >= Break::Word && !self.text.is_empty() {
            if boundary == Break::Line && self.hyphen {
                if let Some(hyphen) = self.letters.pop() {
                    self.text.truncate(hyphen.start);
                }
            } else {
                self.text.push(' ');
            }
        }
        self.hyphen = matches!(ch, '-' | '\u{ad}' | '\u{2010}');
        let start = self.text.len();
        push_folded(&mut self.text, ch);
        self.letters.push(Letter {
            start,
            end: self.text.len(),
            line: self.line,
            direction: dir,
            quad,
        });
    }

    fn find(&self, doc: &Document, needle: &str) -> Result<Vec<Vec<TextQuad>>> {
        let mut folded = String::new();
        let mut space = false;
        for (i, ch) in needle.chars().enumerate() {
            if i.is_multiple_of(4096) {
                doc.check_time()?;
            }
            if ch.is_whitespace() {
                space = !folded.is_empty();
                continue;
            }
            if space {
                folded.push(' ');
                space = false;
            }
            push_folded(&mut folded, ch);
            if folded.len() > self.text.len() {
                return Ok(Vec::new());
            }
        }
        if folded.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        let mut cursor = 0;
        let mut previous_span = None;
        for (start, _) in self.text.match_indices(&folded) {
            let end = start + folded.len();
            while cursor < self.letters.len() && self.letters[cursor].end <= start {
                cursor += 1;
            }
            let mut quads = Vec::new();
            let mut current: Option<(usize, Envelope)> = None;
            let mut i = cursor;
            while let Some(letter) = self.letters.get(i).filter(|l| l.start < end) {
                if let Some((line, area)) = &mut current
                    && *line == letter.line
                {
                    area.extend(letter.quad);
                } else {
                    if let Some((_, area)) = current.take() {
                        quads.push(area.corners().map(xy));
                    }
                    current = Some((letter.line, Envelope::new(letter.quad, letter.direction)));
                }
                i += 1;
                if i.is_multiple_of(4096) {
                    doc.check_time()?;
                }
            }
            if let Some((_, area)) = current {
                quads.push(area.corners().map(xy));
            }
            // Multiple folded letters can belong to the same source character (ß → ss).
            if !quads.is_empty() && previous_span != Some((cursor, i)) {
                hits.push(quads);
                previous_span = Some((cursor, i));
            }
        }
        doc.check_time()?;
        Ok(hits)
    }
}

fn push_folded(text: &mut String, ch: char) {
    if ch.is_ascii() {
        text.push(ch.to_ascii_lowercase());
    } else {
        text.push_str(&unicase::UniCase::unicode(ch.encode_utf8(&mut [0; 4])).to_folded_case());
    }
}
