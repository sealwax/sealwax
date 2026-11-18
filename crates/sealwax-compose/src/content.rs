//! A page's content stream, written from its items: the graphics state is tracked so that only
//! what changes is written, and each glyph is placed by `TJ` where the layout put it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use sealwax_core::Document;

use crate::fonts::{Fonts, Placed};
use crate::model::{
    Color, Composition, FillRule, Image, ImageColorSpace, Item, LineCap, LineJoin, LinearGradient,
    Paint, Path, Segment, Stroke, TextMode, TextRun,
};
use crate::{Error, Skipped};

/// How deep groups may nest.
const MAX_DEPTH: usize = 64;
/// The deadline is checked once per this many items.
const CHECK_EVERY: usize = 1024;

/// Significant digits a content stream number keeps: more than single precision holds, so
/// that a small scale (a form drawn in a unit square, a picture's inverse size) loses nothing
/// a layout computed in `f32` had.
const SIGNIFICANT: i32 = 8;

/// A number as a content stream writes it: [`SIGNIFICANT`] digits, no trailing zeros, no
/// exponent, no `-0`.
pub(crate) fn number(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    let magnitude = if value == 0.0 {
        0
    } else {
        // A finite, non-zero f64 has a decimal exponent within ±324: the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        let exponent = value.abs().log10().floor() as i32;
        exponent
    };
    let decimals = usize::try_from((SIGNIFICANT - 1 - magnitude).clamp(0, 12)).unwrap_or(0);
    let mut text = format!("{value:.decimals$}");
    if text.contains('.') {
        let trimmed = text.trim_end_matches('0').trim_end_matches('.').len();
        text.truncate(trimmed);
    }
    if text == "-0" {
        text = "0".to_owned();
    }
    text
}

/// What a page's content uses from the document: fonts, pictures and opacities by index.
#[derive(Debug, Default)]
pub(crate) struct Used {
    pub(crate) fonts: BTreeSet<usize>,
    pub(crate) images: BTreeSet<usize>,
    /// Fill (`ca`) and stroke (`CA`) opacities, in thousandths.
    pub(crate) fill_alphas: BTreeSet<u16>,
    pub(crate) stroke_alphas: BTreeSet<u16>,
    /// Named `P{index}`, each once.
    pub(crate) gradients: Vec<LinearGradient>,
    /// The index of each gradient, by [`gradient_key`].
    gradient_indexes: BTreeMap<Vec<u64>, usize>,
}

/// What tells a gradient from another: the bits of its numbers, in order.
pub(crate) fn gradient_key(gradient: &LinearGradient) -> Vec<u64> {
    let LinearGradient {
        matrix,
        start,
        end,
        stops,
        extend,
        linear_light,
    } = gradient;
    let flags = [extend[0], extend[1], *linear_light];
    matrix
        .iter()
        .chain(start)
        .chain(end)
        .map(|v| v.to_bits())
        .chain(flags.iter().map(|flag| u64::from(*flag)))
        .chain(stops.iter().flat_map(|stop| {
            std::iter::once(stop.offset)
                .chain(stop.color)
                .map(f64::to_bits)
        }))
        .collect()
}

/// The state `q` saves and `Q` restores, as far as the writer sets it.
#[derive(Debug, Clone, PartialEq)]
struct State {
    /// `None` after a pattern fill: the next colour is written whatever it is.
    fill: Option<Color>,
    stroke: Color,
    fill_alpha: u16,
    stroke_alpha: u16,
    line: Stroke,
    /// `Tz`, percent.
    scaling: f64,
    /// `Tr`.
    mode: u8,
}

impl Default for State {
    fn default() -> Self {
        Self {
            fill: Some(Color::Gray(0.0)),
            stroke: Color::Gray(0.0),
            fill_alpha: 1000,
            stroke_alpha: 1000,
            line: Stroke::default(),
            scaling: 100.0,
            mode: 0,
        }
    }
}

/// Why `image` cannot be written, if it cannot: it or its soft mask past
/// [`sealwax_core::Limits::max_image_pixels`], a palette of no 1 to 256 colours, a soft mask
/// in colour.
fn unwritable(image: &Image, limits: &sealwax_core::Limits) -> Option<&'static str> {
    let pixels = |image: &Image| u64::from(image.width) * u64::from(image.height);
    let mask = image.soft_mask.as_deref();
    if pixels(image) > limits.max_image_pixels
        || mask.is_some_and(|mask| pixels(mask) > limits.max_image_pixels)
    {
        return Some("pictures past max_image_pixels");
    }
    if let ImageColorSpace::IndexedRgb(palette) = &image.color_space
        && !(palette.len() % 3 == 0 && (3..=768).contains(&palette.len()))
    {
        return Some("pictures whose palette has no 1 to 256 colours");
    }
    if mask.is_some_and(|mask| mask.color_space != ImageColorSpace::Gray) {
        return Some("pictures whose soft mask is not gray");
    }
    None
}

/// Thousandths of an opacity.
fn permille(alpha: f64) -> u16 {
    // Clamped to 0..=1000 first: the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = (alpha.clamp(0.0, 1.0) * 1000.0).round() as u16;
    value
}

pub(crate) struct Writer<'a, 'f> {
    doc: &'a mut Document,
    out: String,
    state: State,
    saved: Vec<State>,
    pub(crate) used: Used,
    composition: &'a Composition,
    fonts: &'a mut Fonts<'f>,
    skipped: &'a mut Skipped,
    limits: &'a sealwax_core::Limits,
    items: usize,
}

impl<'a, 'f> Writer<'a, 'f> {
    pub(crate) fn new(
        doc: &'a mut Document,
        composition: &'a Composition,
        fonts: &'a mut Fonts<'f>,
        skipped: &'a mut Skipped,
        limits: &'a sealwax_core::Limits,
    ) -> Self {
        Self {
            doc,
            out: String::new(),
            state: State::default(),
            saved: Vec::new(),
            used: Used::default(),
            composition,
            fonts,
            skipped,
            limits,
            items: 0,
        }
    }

    pub(crate) fn finish(self) -> (Vec<u8>, Used) {
        (self.out.into_bytes(), self.used)
    }

    fn op(&mut self, operands: &[f64], operator: &str) {
        for value in operands {
            self.out.push_str(&number(*value));
            self.out.push(' ');
        }
        self.out.push_str(operator);
        self.out.push('\n');
    }

    pub(crate) fn items(&mut self, items: &[Item], depth: usize) -> Result<(), Error> {
        if depth > MAX_DEPTH {
            return Err(sealwax_core::Error::LimitExceeded {
                what: "group nesting",
            }
            .into());
        }
        for item in items {
            self.items += 1;
            if self.items.is_multiple_of(CHECK_EVERY) {
                self.limits.check_time()?;
            }
            self.item(item, depth)?;
        }
        Ok(())
    }

    fn item(&mut self, item: &Item, depth: usize) -> Result<(), Error> {
        match item {
            Item::Group(items) => {
                self.out.push_str("q\n");
                self.saved.push(self.state.clone());
                self.items(items, depth + 1)?;
                self.out.push_str("Q\n");
                if let Some(state) = self.saved.pop() {
                    self.state = state;
                }
            }
            Item::Transform(m) => self.op(m, "cm"),
            Item::Clip { path, fill_rule } => {
                self.path(path);
                self.out.push_str(match fill_rule {
                    FillRule::NonZero => "W n\n",
                    FillRule::EvenOdd => "W* n\n",
                });
            }
            Item::Fill {
                path,
                fill_rule,
                paint,
            } => {
                self.fill_paint(*paint);
                self.path(path);
                self.out.push_str(match fill_rule {
                    FillRule::NonZero => "f\n",
                    FillRule::EvenOdd => "f*\n",
                });
            }
            Item::Stroke {
                path,
                paint,
                stroke,
            } => {
                self.stroke_paint(*paint);
                self.line(stroke);
                self.path(path);
                self.out.push_str("S\n");
            }
            Item::FillGradient {
                path,
                fill_rule,
                gradient,
            } => {
                // A gradient has no opacity of its own: it is opaque.
                self.fill_alpha(1000);
                let used = &mut self.used;
                let index = *used
                    .gradient_indexes
                    .entry(gradient_key(gradient))
                    .or_insert_with(|| {
                        used.gradients.push(gradient.clone());
                        used.gradients.len() - 1
                    });
                let _ = writeln!(self.out, "/Pattern cs /P{index} scn");
                self.state.fill = None;
                self.path(path);
                self.out.push_str(match fill_rule {
                    FillRule::NonZero => "f\n",
                    FillRule::EvenOdd => "f*\n",
                });
            }
            Item::Text(run) => self.text(run)?,
            Item::Image(index) => match self.composition.images.get(*index) {
                Some(image) => match unwritable(image, self.limits) {
                    None => {
                        self.used.images.insert(*index);
                        let _ = writeln!(self.out, "/Im{index} Do");
                    }
                    Some(why) => self.skipped.count(why, 1),
                },
                None => self.skipped.count("picture indexes out of range", 1),
            },
        }
        Ok(())
    }

    fn path(&mut self, path: &Path) {
        for segment in &path.0 {
            match *segment {
                Segment::MoveTo(x, y) => self.op(&[x, y], "m"),
                Segment::LineTo(x, y) => self.op(&[x, y], "l"),
                Segment::CubicTo(x1, y1, x2, y2, x, y) => self.op(&[x1, y1, x2, y2, x, y], "c"),
                Segment::Rect(x, y, w, h) => self.op(&[x, y, w, h], "re"),
                Segment::Close => self.out.push_str("h\n"),
            }
        }
    }

    fn color(&mut self, color: Color, stroke: bool) {
        match color {
            Color::Gray(g) => self.op(&[g], if stroke { "G" } else { "g" }),
            Color::Rgb(r, g, b) => self.op(&[r, g, b], if stroke { "RG" } else { "rg" }),
        }
    }

    fn fill_paint(&mut self, paint: Paint) {
        if self.state.fill != Some(paint.color) {
            self.color(paint.color, false);
            self.state.fill = Some(paint.color);
        }
        self.fill_alpha(permille(paint.alpha));
    }

    /// Fill opacity, in thousandths.
    fn fill_alpha(&mut self, alpha: u16) {
        if self.state.fill_alpha != alpha {
            self.used.fill_alphas.insert(alpha);
            let _ = writeln!(self.out, "/Ca{alpha} gs");
            self.state.fill_alpha = alpha;
        }
    }

    fn stroke_paint(&mut self, paint: Paint) {
        if self.state.stroke != paint.color {
            self.color(paint.color, true);
            self.state.stroke = paint.color;
        }
        let alpha = permille(paint.alpha);
        if self.state.stroke_alpha != alpha {
            self.used.stroke_alphas.insert(alpha);
            let _ = writeln!(self.out, "/CA{alpha} gs");
            self.state.stroke_alpha = alpha;
        }
    }

    fn line(&mut self, stroke: &Stroke) {
        let current = self.state.line.clone();
        if current.width != stroke.width {
            self.op(&[stroke.width.max(0.0)], "w");
        }
        if current.cap != stroke.cap {
            let cap = match stroke.cap {
                LineCap::Butt => 0.0,
                LineCap::Round => 1.0,
                LineCap::Square => 2.0,
            };
            self.op(&[cap], "J");
        }
        if current.join != stroke.join {
            let join = match stroke.join {
                LineJoin::Miter => 0.0,
                LineJoin::Round => 1.0,
                LineJoin::Bevel => 2.0,
            };
            self.op(&[join], "j");
        }
        if current.miter_limit != stroke.miter_limit {
            self.op(&[stroke.miter_limit.max(1.0)], "M");
        }
        if current.dash != stroke.dash || current.dash_phase != stroke.dash_phase {
            self.out.push('[');
            let dashes: Vec<String> = stroke.dash.iter().map(|d| number(*d)).collect();
            self.out.push_str(&dashes.join(" "));
            let _ = writeln!(self.out, "] {} d", number(stroke.dash_phase));
        }
        self.state.line = stroke.clone();
    }

    /// The run's glyphs in one `TJ`, each moved to its `x` from where the font's own advance
    /// leaves the pen. A glyph the font lacks is left out.
    fn text(&mut self, run: &TextRun) -> Result<(), Error> {
        let scale = run.horizontal_scaling;
        if run.glyphs.is_empty() || !(run.size.is_finite() && run.size != 0.0 && scale != 0.0) {
            return Ok(());
        }
        if !self.fonts.usable(self.doc, run.font)? {
            self.skipped
                .count("glyphs in an unembeddable font", run.glyphs.len());
            return Ok(());
        }
        self.used.fonts.insert(run.font);
        self.fill_paint(run.fill);
        let mode = match run.mode {
            TextMode::Fill => 0,
            TextMode::Stroke => 1,
            TextMode::FillStroke => 2,
            TextMode::Invisible => 3,
        };
        if matches!(run.mode, TextMode::Stroke | TextMode::FillStroke) {
            let (paint, width) = run.stroke.unwrap_or((run.fill, 1.0));
            self.stroke_paint(paint);
            let mut line = self.state.line.clone();
            line.width = width;
            self.line(&line);
        }
        self.out.push_str("BT\n");
        let _ = writeln!(self.out, "/F{} {} Tf", run.font, number(run.size));
        if self.state.scaling != scale * 100.0 {
            self.op(&[scale * 100.0], "Tz");
            self.state.scaling = scale * 100.0;
        }
        if self.state.mode != mode {
            self.op(&[f64::from(mode)], "Tr");
            self.state.mode = mode;
        }
        self.op(&run.matrix, "Tm");
        let unit = run.size * scale / 1000.0;
        let mut array = String::from("[");
        let mut open = false;
        let mut pen = 0.0;
        let mut lacking = 0;
        for glyph in &run.glyphs {
            let Some(Placed { code, advance }) = self.fonts.place(run.font, glyph) else {
                lacking += 1;
                continue;
            };
            let adjust = (pen - glyph.x) / unit;
            if adjust.abs() >= 0.0005 {
                if open {
                    array.push('>');
                    open = false;
                }
                array.push_str(&number(adjust));
            }
            if !open {
                array.push('<');
                open = true;
            }
            array.push_str(&code);
            pen = glyph.x + advance * unit;
        }
        if open {
            array.push('>');
        }
        array.push_str("] TJ\nET\n");
        self.out.push_str(&array);
        if lacking > 0 {
            self.skipped.count("glyphs their font lacks", lacking);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::number;

    #[test]
    fn numbers_are_short() {
        assert_eq!(number(1.0), "1");
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(-1e-13), "0");
        assert_eq!(number(12.345_678_9), "12.345679");
        assert_eq!(number(1.0 / 300.0), "0.0033333333");
        assert_eq!(number(595.275_6), "595.2756");
        assert_eq!(number(f64::NAN), "0");
    }
}
