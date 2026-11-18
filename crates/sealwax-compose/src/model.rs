//! The description of laid-out pages a document is composed from.
//!
//! Coordinates are PDF user space: points, origin at the page's bottom-left corner, y up.

use std::sync::Arc;

/// A 2-D affine matrix `[a, b, c, d, e, f]`: `x' = a·x + c·y + e`, `y' = b·x + d·y + f`.
pub type Matrix = [f64; 6];

/// The pages of a document and what they draw with.
#[derive(Debug, Clone, Default)]
pub struct Composition {
    pub pages: Vec<Page>,
    /// The fonts [`TextRun::font`] indexes.
    pub fonts: Vec<Font>,
    /// The pictures [`Item::Image`] indexes.
    pub images: Vec<Image>,
    pub info: Info,
    /// The logical structure (tagged PDF), tying the pages' marked content
    /// ([`Item::BeginMarked`]) and links to what they are.
    pub structure: Option<Structure>,
    /// The bookmarks, in reading order.
    pub outline: Vec<OutlineEntry>,
}

/// What a document says about itself.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    /// BCP 47 language tag of the text (the catalog's `/Lang`).
    pub language: Option<String>,
    /// Claim PDF/UA-1 (ISO 14289-1) in the XMP packet, for a composition its layout knows to
    /// meet it. The claim is written only when nothing was left out of the document.
    pub pdf_ua: bool,
}

/// One page.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    pub items: Vec<Item>,
    pub links: Vec<Link>,
}

/// What a page draws, in painting order. A [`Item::Transform`] or [`Item::Clip`] applies to the
/// items after it, to the end of the enclosing [`Item::Group`] (or page).
#[derive(Debug, Clone)]
pub enum Item {
    /// Items drawn in a state of their own: transforms and clips inside end with the group.
    Group(Vec<Item>),
    /// Concatenated to the current transform.
    Transform(Matrix),
    /// Intersected with the current clip.
    Clip {
        path: Path,
        fill_rule: FillRule,
    },
    Fill {
        path: Path,
        fill_rule: FillRule,
        paint: Paint,
    },
    Stroke {
        path: Path,
        paint: Paint,
        stroke: Stroke,
    },
    /// The path filled with a gradient.
    FillGradient {
        path: Path,
        fill_rule: FillRule,
        gradient: LinearGradient,
    },
    Text(TextRun),
    /// A picture of [`Composition::images`], drawn into the unit square of the current
    /// transform.
    Image(usize),
    /// What is drawn from here to the matching [`Item::EndMarked`] is marked: content of a
    /// structure element, or an artifact. Marked sequences nest, and may begin and end in
    /// different groups; one left open ends with its page.
    BeginMarked(Mark),
    EndMarked,
}

/// What a marked sequence of a page is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// Content of a structure element of type `tag`, which lists it as [`Kid::Content`] by
    /// `mcid`, unique on its page.
    Element { tag: String, mcid: u32 },
    /// Not part of the document's content: page furniture, of a kind if known.
    Artifact(Option<ArtifactKind>),
}

/// The kinds of artifacts (ISO 32000-1, 14.8.2.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// Running heads, page numbers.
    Pagination,
    /// Rules and other layout marks.
    Layout,
    /// Cut marks, colour bars.
    Page,
    /// A background picture or colour.
    Background,
}

/// The logical structure: its elements, the first the root, each listing what it holds in
/// reading order.
#[derive(Debug, Clone, Default)]
pub struct Structure {
    pub elements: Vec<Element>,
}

/// An element of the structure.
#[derive(Debug, Clone, Default)]
pub struct Element {
    /// Its structure type, written as given: a standard one (`Document`, `P`, `H1`, `L`, `LI`,
    /// `Table`, `TD`, `Figure`, `Link`...), since no role map is written.
    pub kind: String,
    pub kids: Vec<Kid>,
    /// Alternate text (a figure's, a formula's).
    pub alt: Option<String>,
    /// The text the element's content stands for.
    pub actual_text: Option<String>,
    /// BCP 47 tag of the element's language, when not the document's.
    pub lang: Option<String>,
    /// The element's identifier.
    pub id: Option<String>,
    pub table_cell: Option<TableCell>,
    pub list_numbering: Option<ListNumbering>,
}

/// What an element holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kid {
    /// An element of [`Structure::elements`], by index: each but the root is the kid of one.
    Element(usize),
    /// The sequence of page `page` marked [`Mark::Element`] with `mcid`.
    Content { page: usize, mcid: u32 },
    /// A link area: the index in [`Page::links`] of page `page`.
    Link { page: usize, link: usize },
}

/// A table cell's attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableCell {
    /// For a header cell, the cells it heads.
    pub header_scope: Option<HeaderScope>,
    /// Columns the cell spans.
    pub col_span: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderScope {
    Row,
    Column,
    Both,
}

/// How a list's items are numbered, as PDF names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListNumbering {
    Unordered,
    Ordered,
    Description,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerAlpha,
    UpperAlpha,
}

/// A bookmark.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineEntry {
    /// Its level: an entry nests under the nearest entry before it of a smaller level.
    pub level: u32,
    pub title: String,
    /// The page of the composition it leads to.
    pub page: usize,
    /// The y shown at the top of the window; none shows the whole page.
    pub top: Option<f64>,
}

/// A path in the current user space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Path(pub Vec<Segment>);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CubicTo(f64, f64, f64, f64, f64, f64),
    /// A closed rectangle: corner, width, height.
    Rect(f64, f64, f64, f64),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// A colour and its opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    pub color: Color,
    /// 0 (transparent) to 1 (opaque).
    pub alpha: f64,
}

impl Paint {
    /// `color`, opaque.
    #[must_use]
    pub fn opaque(color: Color) -> Self {
        Self { color, alpha: 1.0 }
    }
}

/// Device colours, components from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    Gray(f64),
    Rgb(f64, f64, f64),
}

/// A linear (axial) gradient: its colours change from `start` to `end` and stay the same across.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    /// Gradient space to the page's default user space, as for a PDF pattern: the transforms
    /// in force where the gradient fills do not move it. The identity when `start` and `end`
    /// are page coordinates.
    pub matrix: Matrix,
    pub start: [f64; 2],
    pub end: [f64; 2],
    /// Offsets from 0 (`start`) to 1 (`end`), ascending.
    pub stops: Vec<GradientStop>,
    /// Whether the first and the last colours go on before `start` and past `end`.
    pub extend: [bool; 2],
    /// Interpolated in linear light, as Word draws gradients, rather than between sRGB values.
    pub linear_light: bool,
}

/// A colour of a gradient, sRGB components from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub offset: f64,
    pub color: [f64; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    /// Dash lengths; empty for a solid line.
    pub dash: Vec<f64>,
    pub dash_phase: f64,
}

impl Default for Stroke {
    /// PDF's initial line: 1 unit wide, butt caps, mitered joins, solid.
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_phase: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

/// Glyphs of one font on one baseline.
#[derive(Debug, Clone)]
pub struct TextRun {
    /// Index into [`Composition::fonts`].
    pub font: usize,
    /// In text space units.
    pub size: f64,
    /// Text space to the current user space at the run's origin.
    pub matrix: Matrix,
    /// Horizontal scaling of the glyphs, 1 = 100%.
    pub horizontal_scaling: f64,
    pub mode: TextMode,
    pub fill: Paint,
    /// Paint and line width of the outline, for [`TextMode::Stroke`] and
    /// [`TextMode::FillStroke`].
    pub stroke: Option<(Paint, f64)>,
    pub glyphs: Vec<Glyph>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMode {
    Fill,
    Stroke,
    FillStroke,
    /// Searchable, not visible.
    Invisible,
}

/// One glyph of a [`TextRun`].
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    /// The glyph id in the font program; for a [`Font::Standard`] font, its WinAnsi code.
    pub id: u16,
    /// Where its origin is along the baseline, in text space units from the run's origin
    /// (horizontal scaling included).
    pub x: f64,
    /// Its advance as the layout measured it, in thousandths of the font size: the width a
    /// [`Font::Standard`] font is given for it.
    pub advance: f64,
    /// The text it stands for, for extraction; empty for none.
    pub text: String,
}

/// A font.
#[derive(Debug, Clone)]
pub enum Font {
    /// A TrueType or OpenType program (outlines in `glyf` or `CFF `), embedded as a subset;
    /// `face` is the face in a collection.
    Program { program: Arc<Vec<u8>>, face: u32 },
    /// One of the PDF standard 14 fonts, by its name (`Helvetica-Bold`), drawn with WinAnsi
    /// codes and not embedded.
    Standard(String),
}

/// A picture, already encoded.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub color_space: ImageColorSpace,
    pub bits_per_component: u8,
    pub filter: ImageFilter,
    /// The samples, encoded by `filter`.
    pub data: Vec<u8>,
    /// Opacity per pixel: a [`ImageColorSpace::Gray`] picture.
    pub soft_mask: Option<Box<Image>>,
    pub interpolate: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageColorSpace {
    Gray,
    Rgb,
    Cmyk,
    /// A palette of RGB triples.
    IndexedRgb(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFilter {
    /// JPEG (`DCTDecode`), written as given.
    Jpeg,
    /// zlib (`FlateDecode`).
    Flate,
    /// Raw samples.
    None,
}

/// A link area.
#[derive(Debug, Clone)]
pub struct Link {
    /// Left, bottom, right, top.
    pub rect: [f64; 4],
    pub target: LinkTarget,
    /// What the link leads to, in words (the annotation's `/Contents`); empty for none.
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum LinkTarget {
    Uri(String),
    /// A page of the composition and the y shown at the top.
    Page {
        index: usize,
        top: f64,
    },
}
