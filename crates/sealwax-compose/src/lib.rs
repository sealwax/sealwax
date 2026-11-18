//! PDF documents composed from laid-out pages.
//!
//! A [`Composition`] describes what each page draws, already laid out: glyph runs with the
//! position of every glyph, paths, pictures, link areas; and, for a tagged document, the marked
//! content its logical [`Structure`] ties together, and its bookmarks. [`compose`] writes it as
//! a new [`sealwax_core::Document`], which the rest of sealwax then archives, fills or signs:
//! PDF/A level a keeps the structure it is given.
//!
//! Whatever lays the pages out (a word processor's layout, an HTML engine) stays outside: this
//! crate only writes PDF. Font programs (TrueType or CFF outlines, [`font`]) are embedded as
//! subsets with their `ToUnicode` maps, so the text reads back; each glyph lands where the
//! composition puts it, whatever the program's own advances.
//!
//! What a composition holds that cannot be written (a font program that cannot be embedded or
//! cut, a glyph its font lacks, a picture past [`sealwax_core::Limits::max_image_pixels`]) is
//! left out and counted in [`Composed::skipped`], rather than failing the document.

mod content;
pub mod font;
mod fonts;
mod metadata;
mod model;
mod structure;

use std::collections::{BTreeMap, BTreeSet};

use sealwax_core::{Dict, Document, Limits, Name, ObjId, Object, Rect, Stream};

pub use model::{
    ArtifactKind, Color, Composition, Element, FillRule, Font, Glyph, GradientStop, HeaderScope,
    Image, ImageColorSpace, ImageFilter, Info, Item, Kid, LineCap, LineJoin, LinearGradient, Link,
    LinkTarget, ListNumbering, Mark, Matrix, OutlineEntry, Page, Paint, Path, Segment, Stroke,
    Structure, TableCell, TextMode, TextRun,
};

use content::{Used, Writer, gradient_key};
use fonts::Fonts;

/// What [`compose`] can fail with. Messages never contain document content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The document could not be built: a bound of [`Limits`] passed, the deadline passed.
    #[error(transparent)]
    Pdf(#[from] sealwax_core::Error),
}

/// What a composition held that was left out, by kind, with how many of each.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Skipped(BTreeMap<&'static str, usize>);

impl Skipped {
    pub(crate) fn count(&mut self, what: &'static str, n: usize) {
        *self.0.entry(what).or_default() += n;
    }

    /// Whether nothing was left out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The kinds left out and how many of each.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, usize)> + '_ {
        self.0.iter().map(|(what, n)| (*what, *n))
    }
}

/// A composed document and what was left out of it.
#[derive(Debug)]
#[non_exhaustive]
pub struct Composed {
    pub document: Document,
    pub skipped: Skipped,
}

/// Writes `composition` as a new document, held to `limits` (their deadline included).
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] for a composition without pages or with a page that
/// has no area; [`sealwax_core::Error::Timeout`] past the deadline;
/// [`sealwax_core::Error::LimitExceeded`] for groups nested past 64 levels.
pub fn compose(composition: &Composition, limits: &Limits) -> Result<Composed, Error> {
    // Whether the subsetter cuts a program is known once every page has drawn with it: the
    // composition is then written again without the programs it refused. Those are unusable
    // from the start, so each pass refuses others, and the second refuses none.
    let mut refused = BTreeSet::new();
    loop {
        match write(composition, limits, &refused)? {
            Written::Composed(composed) => return Ok(*composed),
            Written::Refused(fonts) => {
                let before = refused.len();
                refused.extend(fonts);
                if refused.len() == before {
                    return Err(sealwax_core::Error::InvalidArgument { what: "font" }.into());
                }
            }
        }
    }
}

/// A pass of [`compose`].
enum Written {
    Composed(Box<Composed>),
    /// The fonts whose programs the subsetter refused.
    Refused(Vec<usize>),
}

/// `composition` written, the fonts at `refused` left out.
fn write(
    composition: &Composition,
    limits: &Limits,
    refused: &BTreeSet<usize>,
) -> Result<Written, Error> {
    limits.check_time()?;
    if composition.pages.is_empty() {
        return Err(sealwax_core::Error::InvalidArgument { what: "pages" }.into());
    }
    let mut doc = Document::empty_with(*limits)?;
    let mut fonts = Fonts::new(&composition.fonts, refused);
    let mut shared = Shared::default();
    let mut skipped = Skipped::default();
    let mut page_ids = Vec::with_capacity(composition.pages.len());
    let mut page_mcids = Vec::with_capacity(composition.pages.len());
    for page in &composition.pages {
        limits.check_time()?;
        let valid = |side: f64| side.is_finite() && side > 0.0;
        if !(valid(page.width) && valid(page.height)) {
            return Err(sealwax_core::Error::InvalidArgument { what: "page size" }.into());
        }
        let (content, mut used) = {
            let mut writer = Writer::new(&mut doc, composition, &mut fonts, &mut skipped, limits);
            writer.items(&page.items, 0)?;
            writer.finish()
        };
        let resources = shared.resources(&mut doc, composition, &fonts, &used)?;
        page_mcids.push(std::mem::take(&mut used.mcids));
        let contents = doc.add_object(flate(Dict::new(), &content))?;
        let mut dict = Dict::new();
        dict.insert("Resources", resources);
        dict.insert("Contents", Object::Ref(contents));
        let media_box = Rect::from_corners(0.0, 0.0, page.width, page.height);
        page_ids.push(doc.push_page(dict, media_box)?);
    }
    let mut page_links = Vec::with_capacity(page_ids.len());
    for (page, id) in composition.pages.iter().zip(&page_ids) {
        page_links.push(add_links(
            &mut doc,
            &page.links,
            *id,
            &page_ids,
            &mut skipped,
        )?);
    }
    if let Some(tree) = &composition.structure {
        let pages = structure::Pages {
            ids: &page_ids,
            mcids: &page_mcids,
            links: &page_links,
        };
        structure::write(&mut doc, tree, &pages, &mut skipped, limits)?;
    }
    structure::write_outline(&mut doc, &composition.outline, page_ids.len(), &mut skipped)?;
    let unwritten = fonts.write(&mut doc)?;
    if !unwritten.is_empty() {
        return Ok(Written::Refused(unwritten));
    }
    metadata::write(&mut doc, &composition.info, skipped.is_empty())?;
    Ok(Written::Composed(Box::new(Composed {
        document: doc,
        skipped,
    })))
}

use font::flate;

/// The objects pages share: pictures, opacity states and gradients, written on first use.
#[derive(Default)]
struct Shared {
    images: BTreeMap<usize, ObjId>,
    fill_alphas: BTreeMap<u16, ObjId>,
    stroke_alphas: BTreeMap<u16, ObjId>,
    /// By [`gradient_key`].
    patterns: BTreeMap<Vec<u64>, ObjId>,
}

impl Shared {
    /// A page's resources: what its content uses, by the names it uses them by.
    fn resources(
        &mut self,
        doc: &mut Document,
        composition: &Composition,
        fonts: &Fonts,
        used: &Used,
    ) -> Result<Dict, Error> {
        let mut resources = Dict::new();
        let mut font_dict = Dict::new();
        for index in &used.fonts {
            if let Some(id) = fonts.object(*index) {
                font_dict.insert(format!("F{index}").as_str(), Object::Ref(id));
            }
        }
        if !font_dict.is_empty() {
            resources.insert("Font", font_dict);
        }
        let mut xobjects = Dict::new();
        for index in &used.images {
            let id = match self.images.get(index) {
                Some(id) => *id,
                None => {
                    let Some(image) = composition.images.get(*index) else {
                        continue;
                    };
                    let id = write_image(doc, image)?;
                    self.images.insert(*index, id);
                    id
                }
            };
            xobjects.insert(format!("Im{index}").as_str(), Object::Ref(id));
        }
        if !xobjects.is_empty() {
            resources.insert("XObject", xobjects);
        }
        let mut states = Dict::new();
        for (alphas, written, key, prefix) in [
            (&used.fill_alphas, &mut self.fill_alphas, "ca", "Ca"),
            (&used.stroke_alphas, &mut self.stroke_alphas, "CA", "CA"),
        ] {
            for alpha in alphas {
                let id = match written.get(alpha) {
                    Some(id) => *id,
                    None => {
                        let mut state = Dict::new();
                        state.insert("Type", Name::from("ExtGState"));
                        state.insert(key, Object::Real(f64::from(*alpha) / 1000.0));
                        let id = doc.add_object(Object::Dict(state))?;
                        written.insert(*alpha, id);
                        id
                    }
                };
                states.insert(format!("{prefix}{alpha}").as_str(), Object::Ref(id));
            }
        }
        if !states.is_empty() {
            resources.insert("ExtGState", states);
        }
        let mut patterns = Dict::new();
        for (index, gradient) in used.gradients.iter().enumerate() {
            let key = gradient_key(gradient);
            let id = match self.patterns.get(&key) {
                Some(id) => *id,
                None => {
                    let id = doc.add_object(Object::Dict(pattern(gradient)))?;
                    self.patterns.insert(key, id);
                    id
                }
            };
            patterns.insert(format!("P{index}").as_str(), Object::Ref(id));
        }
        if !patterns.is_empty() {
            resources.insert("Pattern", patterns);
        }
        Ok(resources)
    }
}

/// An sRGB component in linear light.
fn linear(component: f64) -> f64 {
    let c = component.clamp(0.0, 1.0);
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// `stops` (offset, colour), ascending, brought within 0 to 1, the domain of the shading's
/// function: the stops outside give way to one at 0 and one at 1, with the colour the gradient
/// has there. Black without stops.
fn within(stops: &[(f64, [f64; 3])]) -> Vec<(f64, [f64; 3])> {
    // The colour at `t`, between the last stop up to it and the first from it.
    let at = |t: f64| -> [f64; 3] {
        let before = stops.iter().rev().find(|(offset, _)| *offset <= t);
        let after = stops.iter().find(|(offset, _)| *offset >= t);
        match (before, after) {
            (Some(&(a, from)), Some(&(b, to))) if b > a => {
                let f = (t - a) / (b - a);
                std::array::from_fn(|i| from[i] + (to[i] - from[i]) * f)
            }
            (Some(&(_, color)), _) | (None, Some(&(_, color))) => color,
            (None, None) => [0.0; 3],
        }
    };
    let mut kept: Vec<(f64, [f64; 3])> = stops
        .iter()
        .copied()
        .filter(|(offset, _)| (0.0..=1.0).contains(offset))
        .collect();
    if kept.first().is_none_or(|(offset, _)| *offset > 0.0) {
        kept.insert(0, (0.0, at(0.0)));
    }
    if kept.last().is_none_or(|(offset, _)| *offset < 1.0) {
        kept.push((1.0, at(1.0)));
    }
    kept
}

/// The shading pattern of `gradient`: an axial shading whose function goes through its stops,
/// in `DeviceRGB`, or in a linear `CalRGB` (sRGB primaries, D65) for linear light.
fn pattern(gradient: &LinearGradient) -> Dict {
    // The stops in the shading's colour space, where its function interpolates.
    let mut stops: Vec<(f64, [f64; 3])> = gradient
        .stops
        .iter()
        .filter(|stop| stop.offset.is_finite())
        .map(|stop| {
            let color = if gradient.linear_light {
                stop.color.map(linear)
            } else {
                stop.color.map(|v| v.clamp(0.0, 1.0))
            };
            (stop.offset, color)
        })
        .collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let stops = within(&stops);
    let color = |c: [f64; 3]| Object::Array(c.iter().map(|v| Object::Real(*v)).collect());
    let segment = |(_, a): &(f64, [f64; 3]), (_, b): &(f64, [f64; 3])| {
        let mut f = Dict::new();
        f.insert("FunctionType", Object::Integer(2));
        f.insert(
            "Domain",
            Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
        );
        f.insert("C0", color(*a));
        f.insert("C1", color(*b));
        f.insert("N", Object::Integer(1));
        Object::Dict(f)
    };
    let function = match stops.as_slice() {
        [] => segment(&(0.0, [0.0; 3]), &(1.0, [0.0; 3])),
        [only] => segment(only, only),
        [a, b] => segment(a, b),
        many => {
            let mut f = Dict::new();
            f.insert("FunctionType", Object::Integer(3));
            f.insert(
                "Domain",
                Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
            );
            f.insert(
                "Functions",
                Object::Array(many.windows(2).map(|w| segment(&w[0], &w[1])).collect()),
            );
            f.insert(
                "Bounds",
                Object::Array(
                    many[1..many.len() - 1]
                        .iter()
                        .map(|(offset, _)| Object::Real(*offset))
                        .collect(),
                ),
            );
            f.insert(
                "Encode",
                Object::Array(
                    many.windows(2)
                        .flat_map(|_| [Object::Real(0.0), Object::Real(1.0)])
                        .collect(),
                ),
            );
            Object::Dict(f)
        }
    };
    let space = if gradient.linear_light {
        let mut cal = Dict::new();
        cal.insert(
            "WhitePoint",
            Object::Array(
                [0.9505, 1.0, 1.089]
                    .iter()
                    .map(|v| Object::Real(*v))
                    .collect(),
            ),
        );
        cal.insert(
            "Matrix",
            Object::Array(
                [
                    0.4124, 0.2126, 0.0193, 0.3576, 0.7152, 0.1192, 0.1805, 0.0722, 0.9505,
                ]
                .iter()
                .map(|v| Object::Real(*v))
                .collect(),
            ),
        );
        Object::Array(vec![Object::Name(Name::from("CalRGB")), Object::Dict(cal)])
    } else {
        Object::Name(Name::from("DeviceRGB"))
    };
    let [x0, y0] = gradient.start;
    let [x1, y1] = gradient.end;
    let mut shading = Dict::new();
    shading.insert("ShadingType", Object::Integer(2));
    shading.insert("ColorSpace", space);
    shading.insert(
        "Coords",
        Object::Array([x0, y0, x1, y1].iter().map(|v| Object::Real(*v)).collect()),
    );
    shading.insert("Function", function);
    shading.insert(
        "Extend",
        Object::Array(gradient.extend.iter().map(|e| Object::Bool(*e)).collect()),
    );
    let mut pattern = Dict::new();
    pattern.insert("Type", Name::from("Pattern"));
    pattern.insert("PatternType", Object::Integer(2));
    pattern.insert("Shading", shading);
    if gradient.matrix != [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
        pattern.insert(
            "Matrix",
            Object::Array(gradient.matrix.iter().map(|v| Object::Real(*v)).collect()),
        );
    }
    pattern
}

/// An image XObject of `image`, its soft mask written first (a soft mask's own is not: a
/// reader ignores it).
fn write_image(doc: &mut Document, image: &Image) -> Result<ObjId, Error> {
    let mask = match &image.soft_mask {
        Some(mask) => Some(image_object(doc, mask, None)?),
        None => None,
    };
    image_object(doc, image, mask)
}

/// An image XObject of `image`'s samples, with `mask` as its soft mask.
fn image_object(doc: &mut Document, image: &Image, mask: Option<ObjId>) -> Result<ObjId, Error> {
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("XObject"));
    dict.insert("Subtype", Name::from("Image"));
    dict.insert("Width", Object::Integer(i64::from(image.width)));
    dict.insert("Height", Object::Integer(i64::from(image.height)));
    let space = match &image.color_space {
        ImageColorSpace::Gray => Object::Name(Name::from("DeviceGray")),
        ImageColorSpace::Rgb => Object::Name(Name::from("DeviceRGB")),
        ImageColorSpace::Cmyk => Object::Name(Name::from("DeviceCMYK")),
        // A palette of 1 to 256 colours (see `content::unwritable`).
        ImageColorSpace::IndexedRgb(palette) => {
            let entries = i64::try_from(palette.len() / 3).unwrap_or(1).max(1);
            Object::Array(vec![
                Object::Name(Name::from("Indexed")),
                Object::Name(Name::from("DeviceRGB")),
                Object::Integer(entries - 1),
                Object::String(palette.clone()),
            ])
        }
    };
    dict.insert("ColorSpace", space);
    dict.insert(
        "BitsPerComponent",
        Object::Integer(i64::from(image.bits_per_component)),
    );
    let filter = match image.filter {
        ImageFilter::Jpeg => Some("DCTDecode"),
        ImageFilter::Flate => Some("FlateDecode"),
        ImageFilter::None => None,
    };
    if let Some(filter) = filter {
        dict.insert("Filter", Name::from(filter));
    }
    if image.interpolate {
        dict.insert("Interpolate", Object::Bool(true));
    }
    if let Some(mask) = mask {
        dict.insert("SMask", Object::Ref(mask));
    }
    Ok(doc.add_object(Object::Stream(Stream {
        dict,
        data: image.data.clone(),
    }))?)
}

/// The link annotations of the page at `page`: for each link, its annotation, if written.
fn add_links(
    doc: &mut Document,
    links: &[Link],
    page: ObjId,
    pages: &[ObjId],
    skipped: &mut Skipped,
) -> Result<Vec<Option<ObjId>>, Error> {
    let mut annotations = Vec::new();
    let mut written = Vec::with_capacity(links.len());
    for link in links {
        let mut annotation = Dict::new();
        annotation.insert("Type", Name::from("Annot"));
        annotation.insert("Subtype", Name::from("Link"));
        annotation.insert(
            "Rect",
            Object::Array(link.rect.iter().map(|v| Object::Real(*v)).collect()),
        );
        annotation.insert(
            "Border",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );
        annotation.insert("F", Object::Integer(4));
        annotation.insert("P", Object::Ref(page));
        if !link.description.is_empty() {
            annotation.insert(
                "Contents",
                Object::String(sealwax_core::encode_text_string(&link.description)),
            );
        }
        match &link.target {
            LinkTarget::Uri(uri) => {
                let mut action = Dict::new();
                action.insert("S", Name::from("URI"));
                action.insert("URI", Object::String(uri.as_bytes().to_vec()));
                annotation.insert("A", action);
            }
            LinkTarget::Page { index, top } => {
                let Some(target) = pages.get(*index) else {
                    skipped.count("links to pages out of range", 1);
                    written.push(None);
                    continue;
                };
                annotation.insert(
                    "Dest",
                    Object::Array(vec![
                        Object::Ref(*target),
                        Object::Name(Name::from("XYZ")),
                        Object::Integer(0),
                        Object::Real(*top),
                        Object::Null,
                    ]),
                );
            }
        }
        let id = doc.add_object(Object::Dict(annotation))?;
        annotations.push(Object::Ref(id));
        written.push(Some(id));
    }
    if annotations.is_empty() {
        return Ok(written);
    }
    if let Some(Object::Dict(mut dict)) = doc.object(page)? {
        dict.insert("Annots", Object::Array(annotations));
        doc.set_object(page, Object::Dict(dict));
    }
    Ok(written)
}
