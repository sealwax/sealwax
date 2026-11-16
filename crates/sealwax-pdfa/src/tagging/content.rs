//! A page's content cut into operators, the units of content the structure ties to, and the
//! marked-content sequences that tie them, inserted without changing anything else.
//!
//! Level N1 ties every piece of content to the structure, or marks it as an artifact. A unit is
//! what draws:
//! a text-showing operator, a path from its construction to its painting, an XObject, an inline
//! image, a shading. Sequences are inserted around whole operators, and only where every
//! `q`/`Q`, `BT`/`ET` and `BMC`/`BDC`/`EMC` they enclose is balanced: the page renders as before,
//! and the sequences nest as ISO 32000-1 (14.6) wants.

use std::ops::{Range, RangeInclusive};

use crate::inline::{image_end, is_regular, is_white, skip_string};

/// What a unit of content draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A text-showing operator: `Tj`, `TJ`, `'`, `"`.
    Text,
    /// A path, painted (stroked, filled or both); a path that only clips is no unit.
    Path,
    /// An XObject (`Do`): an image or a form.
    XObject,
    /// An inline image (`BI` … `EI`).
    InlineImage,
    /// A shading painted over the clip (`sh`).
    Shading,
}

/// An operator with its operands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Op {
    /// From its first operand to the end of the operator (of `EI` for an inline image).
    pub(crate) span: Range<usize>,
    /// The operator itself.
    pub(crate) operator: Range<usize>,
}

/// A unit of content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    pub(crate) kind: Kind,
    /// Its operators, by index.
    pub(crate) ops: RangeInclusive<usize>,
    /// Inside a sequence that already ties it to the structure (`/MCID`) or marks it as an
    /// artifact: left as it is.
    pub(crate) tied: bool,
    /// Inside a sequence with an `/ActualText` or an `/Alt`: the depth of marked content outside
    /// the outermost one. A sequence tying the unit goes around it rather than inside, where
    /// PDFium would no longer read the replacement text, and so does the figure a description
    /// is for.
    around: Option<u32>,
}

/// Brackets whose balance a sequence must keep: `q`/`Q`, `BT`/`ET`, marked content.
type Depth = [u32; 3];

const STATE: usize = 0;
const TEXT: usize = 1;
const MARKED: usize = 2;

fn lowest(a: Depth, b: Depth) -> Depth {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])]
}

/// A content stream, read.
#[derive(Debug)]
pub(crate) struct Content<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) ops: Vec<Op>,
    pub(crate) units: Vec<Unit>,
    /// One more than the largest `/MCID` the content uses (0 without one): where identifiers
    /// added to it start.
    pub(crate) next_mcid: usize,
    /// The depth of each bracket before each operator, and after the last.
    depths: Vec<Depth>,
}

/// How a unit is tied: to which element, or as an artifact. The caller numbers the elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    Element(usize),
    Artifact(Artifact),
}

/// What kind of artifact (ISO 32000-1, 14.8.2.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Artifact {
    /// Rules, frames, backgrounds of form fields: what lays the page out.
    Layout,
    /// Running heads and page numbers.
    Pagination,
    /// An image behind text (a scan under its OCR layer).
    Background,
}

/// Units of one mark, enclosed in one sequence: from operator `first` to `last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) first: usize,
    pub(crate) last: usize,
    pub(crate) mark: Mark,
}

fn is_number_start(b: u8) -> bool {
    b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.')
}

/// A token of a content stream: an operand (or part of one: a bracket of an array or a
/// dictionary) or an operator.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    span: Range<usize>,
    operator: bool,
}

/// The tokens of `data` from `at` on, comments skipped. Inline image data is not told apart
/// here: [`operators`] skips it.
fn token(data: &[u8], mut at: usize) -> Option<Token> {
    loop {
        while at < data.len() && is_white(data[at]) {
            at += 1;
        }
        if data.get(at) != Some(&b'%') {
            break;
        }
        while at < data.len() && !matches!(data[at], b'\r' | b'\n') {
            at += 1;
        }
    }
    let start = at;
    let b = *data.get(at)?;
    let mut operator = false;
    match b {
        b'(' => at = skip_string(data, at),
        b'<' if data.get(at + 1) == Some(&b'<') => at += 2,
        b'<' => {
            at = data[at..]
                .iter()
                .position(|c| *c == b'>')
                .map_or(data.len(), |p| at + p + 1);
        }
        b'>' if data.get(at + 1) == Some(&b'>') => at += 2,
        b'/' => {
            at += 1;
            while at < data.len() && is_regular(data[at]) {
                at += 1;
            }
        }
        _ if !is_regular(b) => at += 1,
        _ => {
            while at < data.len() && is_regular(data[at]) {
                at += 1;
            }
            let word = &data[start..at];
            operator = !is_number_start(b) && !matches!(word, b"true" | b"false" | b"null");
        }
    }
    Some(Token {
        span: start..at,
        operator,
    })
}

/// The operators of `data`, each with its operands.
fn operators(data: &[u8]) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut operands: Option<usize> = None;
    let mut at = 0;
    while let Some(token) = token(data, at) {
        at = token.span.end;
        if !token.operator {
            operands.get_or_insert(token.span.start);
            continue;
        }
        let mut end = token.span.end;
        if &data[token.span.clone()] == b"BI" {
            end = image_end(data, end).map_or(data.len(), |(_, end)| end);
            at = end;
        }
        ops.push(Op {
            span: operands.take().unwrap_or(token.span.start)..end,
            operator: token.span,
        });
    }
    ops
}

/// The operand tokens of `op`.
fn operands<'d>(data: &'d [u8], op: &Op) -> impl Iterator<Item = (Range<usize>, &'d [u8])> {
    let end = op.operator.start;
    let mut at = op.span.start;
    std::iter::from_fn(move || {
        let token = token(&data[..end], at)?;
        at = token.span.end;
        Some((token.span.clone(), &data[token.span]))
    })
}

/// `data` with the `/MCID`s that `keep` refuses blanked out, when it has some: those that lead
/// to no structure element (a merged document's pages keep the identifiers of a tree left
/// behind; some producers write identifiers their parent tree lacks). The sequences stay, with
/// their other properties (`/ActualText`, `/Lang`, optional content), and so do the offsets of
/// everything.
pub(crate) fn without_mcids(data: &[u8], keep: impl Fn(usize) -> bool) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    for op in operators(data) {
        if &data[op.operator.clone()] != b"BDC" {
            continue;
        }
        let tokens: Vec<_> = operands(data, &op).collect();
        for pair in tokens.windows(2) {
            let kept = std::str::from_utf8(pair[1].1)
                .ok()
                .and_then(|mcid| mcid.parse::<usize>().ok())
                .is_some_and(&keep);
            if pair[0].1 == b"/MCID"
                && pair[1].1.first().is_some_and(|b| is_number_start(*b))
                && !kept
            {
                let blank = pair[0].0.start..pair[1].0.end;
                out.get_or_insert_with(|| data.to_vec())[blank].fill(b' ');
            }
        }
    }
    out
}

impl<'a> Content<'a> {
    /// Read `data`.
    pub(crate) fn read(data: &'a [u8]) -> Self {
        let ops = operators(data);
        let mut units = Vec::new();
        let mut depths = Vec::with_capacity(ops.len() + 1);
        let mut depth: Depth = [0; 3];
        // For each open marked-content sequence: does it tie what it encloses, does it carry an
        // `/ActualText`?
        let mut sequences: Vec<(bool, bool)> = Vec::new();
        let mut path: Option<usize> = None;
        let mut next_mcid = 0usize;
        depths.push(depth);
        for (index, op) in ops.iter().enumerate() {
            let tied = sequences.iter().any(|(tied, _)| *tied);
            let around = sequences
                .iter()
                .position(|(_, actual)| *actual)
                .and_then(|depth| u32::try_from(depth).ok());
            let mut unit = |kind, first| {
                units.push(Unit {
                    kind,
                    ops: first..=index,
                    tied,
                    around,
                });
            };
            match &data[op.operator.clone()] {
                b"q" => {
                    depth[STATE] += 1;
                    path = None;
                }
                b"Q" => {
                    depth[STATE] = depth[STATE].saturating_sub(1);
                    path = None;
                }
                b"BT" => {
                    depth[TEXT] += 1;
                    path = None;
                }
                b"ET" => {
                    depth[TEXT] = depth[TEXT].saturating_sub(1);
                    path = None;
                }
                b"BMC" | b"BDC" => {
                    depth[MARKED] += 1;
                    let tokens: Vec<&[u8]> = operands(data, op).map(|(_, token)| token).collect();
                    let artifact = tokens.first() == Some(&&b"/Artifact"[..]);
                    let has = |key: &[u8]| tokens.iter().skip(1).any(|token| *token == key);
                    if let Some(mcid) = tokens
                        .windows(2)
                        .find(|pair| pair[0] == b"/MCID")
                        .and_then(|pair| std::str::from_utf8(pair[1]).ok()?.parse::<usize>().ok())
                    {
                        next_mcid = next_mcid.max(mcid.saturating_add(1));
                    }
                    sequences.push((
                        artifact || has(b"/MCID"),
                        has(b"/ActualText") || has(b"/Alt"),
                    ));
                    path = None;
                }
                b"EMC" => {
                    if depth[MARKED] > 0 {
                        depth[MARKED] -= 1;
                        sequences.pop();
                    }
                    path = None;
                }
                b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re" => {
                    path.get_or_insert(index);
                }
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                    unit(Kind::Path, path.take().unwrap_or(index));
                }
                b"n" => path = None,
                // A unit inside a path's construction (never valid) ends the path: units never
                // overlap.
                b"Tj" | b"TJ" | b"'" | b"\"" => {
                    unit(Kind::Text, index);
                    path = None;
                }
                b"Do" => {
                    unit(Kind::XObject, index);
                    path = None;
                }
                b"BI" => {
                    unit(Kind::InlineImage, index);
                    path = None;
                }
                b"sh" => {
                    unit(Kind::Shading, index);
                    path = None;
                }
                _ => {}
            }
            depths.push(depth);
        }
        Self {
            data,
            ops,
            units,
            next_mcid,
            depths,
        }
    }

    /// The sequences that tie each unit as `marks` says (`None`: left as it is), as few as the
    /// brackets allow: consecutive units of one mark share a sequence when a balanced stretch
    /// of operators holds them and nothing else that draws.
    pub(crate) fn runs(&self, marks: &[Option<Mark>]) -> Vec<Run> {
        let mut runs = Vec::new();
        let mut current: Option<(Run, Depth)> = None;
        // The first operator a sequence may start at.
        let mut floor = 0;
        for (index, unit) in self.units.iter().enumerate() {
            let (first, last) = (*unit.ops.start(), *unit.ops.end());
            let limit = self
                .units
                .get(index + 1)
                .map_or(self.ops.len(), |next| *next.ops.start());
            let mark = marks.get(index).copied().flatten();
            if let (Some(mark), Some((run, low))) = (mark, current.as_mut())
                && run.mark == mark
                && self.extend(run, low, last, unit.around, floor, limit)
            {
                continue;
            }
            if let Some((run, _)) = current.take() {
                floor = run.last + 1;
                runs.push(run);
            }
            match mark {
                Some(mark) => {
                    let mut run = Run { first, last, mark };
                    let mut low = self.depths[first..=last + 1]
                        .iter()
                        .fold(self.depths[first], |low, depth| lowest(low, *depth));
                    if let Some(around) = unit.around {
                        let (mut wider, mut wider_low) = (run, low);
                        wider_low[MARKED] = wider_low[MARKED].min(around);
                        if self.settle(&mut wider, &mut wider_low, floor, limit) {
                            (run, low) = (wider, wider_low);
                        }
                    }
                    current = Some((run, low));
                }
                None => floor = last + 1,
            }
        }
        if let Some((run, _)) = current {
            runs.push(run);
        }
        runs
    }

    /// Stretch `run` to operator `last`, and further out, between `floor` and `limit`
    /// (excluded), until the brackets it encloses balance (and, with `around`, until it encloses
    /// the sequences with an `/ActualText` around the new unit); `low` is the lowest depth inside.
    fn extend(
        &self,
        run: &mut Run,
        low: &mut Depth,
        last: usize,
        around: Option<u32>,
        floor: usize,
        limit: usize,
    ) -> bool {
        let mut stretched = Run { last, ..*run };
        let mut lowest_inside = self.depths[run.last + 1..=last + 1]
            .iter()
            .fold(*low, |low, depth| lowest(low, *depth));
        if let Some(around) = around {
            lowest_inside[MARKED] = lowest_inside[MARKED].min(around);
        }
        if self.settle(&mut stretched, &mut lowest_inside, floor, limit) {
            *run = stretched;
            *low = lowest_inside;
            return true;
        }
        false
    }

    /// Widen `run`, between `floor` and `limit` (excluded), until it starts and ends at depth
    /// `low`, the lowest inside it.
    fn settle(&self, run: &mut Run, low: &mut Depth, floor: usize, limit: usize) -> bool {
        loop {
            if self.depths[run.first] != *low {
                if run.first == floor {
                    return false;
                }
                run.first -= 1;
                *low = lowest(*low, self.depths[run.first]);
            } else if self.depths[run.last + 1] != *low {
                if run.last + 1 >= limit {
                    return false;
                }
                run.last += 1;
                *low = lowest(*low, self.depths[run.last + 1]);
            } else {
                return true;
            }
        }
    }

    /// The content with each unit in a marked-content sequence of its own, tagged `tag`, with
    /// its index as `/I`, for [`sealwax_text::drawn`]. An inline image is replaced by the unit
    /// square it fills: hayro-syntax would read what follows it again (issue #1610).
    pub(crate) fn instrumented(&self, tag: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + self.units.len() * 24);
        let mut copied = 0;
        for (index, unit) in self.units.iter().enumerate() {
            let start = self.ops[*unit.ops.start()].span.start;
            let end = self.ops[*unit.ops.end()].span.end;
            out.extend_from_slice(&self.data[copied..start]);
            out.extend_from_slice(format!("\n/{tag} <</I {index}>> BDC\n").as_bytes());
            if unit.kind == Kind::InlineImage {
                out.extend_from_slice(b"q 0 0 1 1 re f Q");
            } else {
                out.extend_from_slice(&self.data[start..end]);
            }
            out.extend_from_slice(b"\nEMC\n");
            copied = end;
        }
        out.extend_from_slice(&self.data[copied..]);
        out
    }

    /// The content with `before` and `after` of each run inserted around it. Runs are in order
    /// and do not overlap, as [`Self::runs`] makes them.
    pub(crate) fn write(
        &self,
        runs: &[Run],
        mut wrap: impl FnMut(&Run) -> (Vec<u8>, Vec<u8>),
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + runs.len() * 32);
        let mut copied = 0;
        for run in runs {
            let (before, after) = wrap(run);
            let start = self.ops[run.first].span.start;
            let end = self.ops[run.last].span.end;
            out.extend_from_slice(&self.data[copied..start]);
            out.push(b'\n');
            out.extend_from_slice(&before);
            out.push(b'\n');
            out.extend_from_slice(&self.data[start..end]);
            out.push(b'\n');
            out.extend_from_slice(&after);
            out.push(b'\n');
            copied = end;
        }
        out.extend_from_slice(&self.data[copied..]);
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Artifact, Content, Kind, Mark, Run};

    fn words(data: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(data)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    fn operator_names(content: &Content<'_>) -> Vec<String> {
        content
            .ops
            .iter()
            .map(|op| String::from_utf8_lossy(&content.data[op.operator.clone()]).into_owned())
            .collect()
    }

    #[test]
    fn operators_keep_their_operands() {
        let data = b"q 1 0 0 1 0 0 cm /F1 12 Tf [(a\\)b) -20 <0041>] TJ % a comment Tj\n/P <</MCID 0>> BDC (x)' EMC Q";
        let content = Content::read(data);
        assert_eq!(
            operator_names(&content),
            ["q", "cm", "Tf", "TJ", "BDC", "'", "EMC", "Q"]
        );
        let tj = &content.ops[3];
        assert_eq!(&data[tj.span.clone()], b"[(a\\)b) -20 <0041>] TJ");
        let bdc = &content.ops[4];
        assert_eq!(&data[bdc.span.clone()], b"/P <</MCID 0>> BDC");
    }

    #[test]
    fn units_and_what_ties_them_already() {
        let data = b"0 0 m 10 10 l S 0 0 5 5 re W n /Im1 Do BI /W 1 /H 1 ID x EI /Sh sh \
            /Artifact BMC 1 1 1 1 re f EMC BT /P <</MCID 3>> BDC (a) Tj EMC (b) Tj ET";
        let content = Content::read(data);
        let kinds: Vec<(Kind, bool)> = content.units.iter().map(|u| (u.kind, u.tied)).collect();
        assert_eq!(
            kinds,
            [
                (Kind::Path, false),
                (Kind::XObject, false),
                (Kind::InlineImage, false),
                (Kind::Shading, false),
                (Kind::Path, true),
                (Kind::Text, true),
                (Kind::Text, false),
            ]
        );
        assert_eq!(content.units[0].ops, 0..=2);
    }

    #[test]
    fn identifiers_of_a_tree_left_behind_are_blanked_out() {
        let data = b"/P <</MCID 12 /ActualText (/MCID 3)>> BDC (a) Tj EMC /Artifact BMC EMC";
        let cleaned = super::without_mcids(data, |_| false).unwrap();
        assert_eq!(cleaned.len(), data.len());
        assert_eq!(
            words(&cleaned).join(" "),
            "/P << /ActualText (/MCID 3)>> BDC (a) Tj EMC /Artifact BMC EMC"
        );
        let content = Content::read(&cleaned);
        assert!(!content.units[0].tied);
        assert_eq!(super::without_mcids(b"/P BMC (a) Tj EMC", |_| false), None);
        // Identifiers that lead somewhere stay.
        let both = b"/P <</MCID 0>> BDC (a) Tj EMC /P <</MCID 1>> BDC (b) Tj EMC";
        let cleaned = super::without_mcids(both, |mcid| mcid == 0).unwrap();
        assert_eq!(
            words(&cleaned).join(" "),
            "/P <</MCID 0>> BDC (a) Tj EMC /P << >> BDC (b) Tj EMC"
        );
    }

    #[test]
    fn each_unit_is_marked_for_the_analysis_and_inline_images_become_squares() {
        let data = b"0 0 m (a) Tj 1 1 l S BI /W 1 /H 1 ID x EI";
        let content = Content::read(data);
        assert_eq!(
            words(&content.instrumented("U")).join(" "),
            "0 0 m /U <</I 0>> BDC (a) Tj EMC /U <</I 1>> BDC 1 1 l S EMC \
             /U <</I 2>> BDC q 0 0 1 1 re f Q EMC"
        );
    }

    #[test]
    fn inline_image_data_is_not_read_as_operators() {
        let data = b"BI /W 2 /H 1 /BPC 8 /CS /G ID Tj) EI Q (a) Tj";
        let content = Content::read(data);
        assert_eq!(operator_names(&content), ["BI", "Q", "Tj"]);
    }

    fn wrapped(data: &[u8], marks: &[Option<Mark>]) -> Vec<String> {
        let content = Content::read(data);
        let runs = content.runs(marks);
        words(&content.write(&runs, |run: &Run| match run.mark {
            Mark::Element(element) => (format!("<{element}").into_bytes(), b">".to_vec()),
            Mark::Artifact(_) => (b"<A".to_vec(), b">".to_vec()),
        }))
    }

    #[test]
    fn units_of_one_element_share_a_sequence_across_balanced_brackets() {
        let data = b"BT /F1 9 Tf 1 0 0 1 9 9 Tm (a) Tj ET BT (b) Tj ET q 0 0 m 1 1 l S Q";
        let marks = [
            Some(Mark::Element(0)),
            Some(Mark::Element(0)),
            Some(Mark::Artifact(Artifact::Layout)),
        ];
        assert_eq!(
            wrapped(data, &marks).join(" "),
            "<0 BT /F1 9 Tf 1 0 0 1 9 9 Tm (a) Tj ET BT (b) Tj ET > q <A 0 0 m 1 1 l S > Q"
        );
    }

    #[test]
    fn sequences_stay_inside_what_another_mark_shares() {
        let data = b"BT (a) Tj (b) Tj ET BT (c) Tj ET";
        let marks = [
            Some(Mark::Element(0)),
            Some(Mark::Element(1)),
            Some(Mark::Element(0)),
        ];
        assert_eq!(
            wrapped(data, &marks).join(" "),
            "BT <0 (a) Tj > <1 (b) Tj > ET BT <0 (c) Tj > ET"
        );
    }

    #[test]
    fn unbalanced_brackets_and_untouched_units_split_sequences() {
        let data = b"q (a) Tj Q Q (b) Tj /P <</MCID 0>> BDC (c) Tj EMC (d) Tj";
        let content = Content::read(data);
        assert!(content.units[2].tied);
        assert_eq!(content.next_mcid, 1);
        let marks = [
            Some(Mark::Element(0)),
            Some(Mark::Element(0)),
            None,
            Some(Mark::Element(0)),
        ];
        assert_eq!(
            wrapped(data, &marks).join(" "),
            "<0 q (a) Tj Q Q (b) Tj > /P <</MCID 0>> BDC (c) Tj EMC <0 (d) Tj >"
        );
    }

    #[test]
    fn a_sequence_goes_around_a_replacement_text_rather_than_inside() {
        let data =
            b"BT /Span <</ActualText (x)>> BDC (a) Tj EMC ET /Span <</Lang (fr)>> BDC (b) Tj EMC";
        let marks = [Some(Mark::Element(0)), Some(Mark::Element(1))];
        assert_eq!(
            wrapped(data, &marks).join(" "),
            "BT <0 /Span <</ActualText (x)>> BDC (a) Tj EMC > ET \
             /Span <</Lang (fr)>> BDC <1 (b) Tj > EMC"
        );
    }

    #[test]
    fn a_sequence_never_crosses_an_unclosed_bracket() {
        // The first `q` never closes: the units on either side of it cannot share a sequence.
        let data = b"(a) Tj q (b) Tj";
        let marks = [Some(Mark::Element(0)), Some(Mark::Element(0))];
        assert_eq!(wrapped(data, &marks).join(" "), "<0 (a) Tj > q <0 (b) Tj >");
    }
}
