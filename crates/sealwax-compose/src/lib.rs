//! PDF documents composed from laid-out pages.
//!
//! A [`Composition`] describes what each page draws, already laid out: glyph runs with the
//! position of every glyph, paths, pictures, link areas. [`compose`] writes it as a new
//! [`sealwax_core::Document`], which the rest of sealwax then archives, fills or signs.
//!
//! Whatever lays the pages out (a word processor's layout, an HTML engine) stays outside: this
//! crate only writes PDF. Font programs (TrueType or CFF outlines, [`font`]) are embedded as
//! subsets with their `ToUnicode` maps, so the text reads back; each glyph lands where the
//! composition puts it, whatever the program's own advances.
//!
//! What a composition holds that cannot be written (a font program that cannot be embedded, a
//! picture past [`sealwax_core::Limits::max_image_pixels`]) is left out and counted in
//! [`Composed::skipped`], rather than failing the document.

mod content;
pub mod font;
mod fonts;
mod model;

use std::collections::BTreeMap;

use sealwax_core::{Dict, Document, Limits, Name, ObjId, Object, Rect, Stream};

pub use model::{
    Color, Composition, FillRule, Font, Glyph, GradientStop, Image, ImageColorSpace, ImageFilter,
    Info, Item, LineCap, LineJoin, LinearGradient, Link, LinkTarget, Matrix, Page, Paint, Path,
    Segment, Stroke, TextMode, TextRun,
};

use content::{Used, Writer};
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
    limits.check_time()?;
    if composition.pages.is_empty() {
        return Err(sealwax_core::Error::InvalidArgument { what: "pages" }.into());
    }
    let mut doc = Document::empty_with(*limits)?;
    let mut fonts = Fonts::new(&composition.fonts);
    let mut shared = Shared::default();
    let mut skipped = Skipped::default();
    let mut page_ids = Vec::with_capacity(composition.pages.len());
    for page in &composition.pages {
        limits.check_time()?;
        let valid = |side: f64| side.is_finite() && side > 0.0;
        if !(valid(page.width) && valid(page.height)) {
            return Err(sealwax_core::Error::InvalidArgument { what: "page size" }.into());
        }
        let (content, used) = {
            let mut writer = Writer::new(&mut doc, composition, &mut fonts, &mut skipped, limits);
            writer.items(&page.items, 0)?;
            writer.finish()
        };
        let resources = shared.resources(&mut doc, composition, &fonts, &used)?;
        let contents = doc.add_object(flate(Dict::new(), &content))?;
        let mut dict = Dict::new();
        dict.insert("Resources", resources);
        dict.insert("Contents", Object::Ref(contents));
        let media_box = Rect::from_corners(0.0, 0.0, page.width, page.height);
        page_ids.push(doc.push_page(dict, media_box)?);
    }
    for (page, id) in composition.pages.iter().zip(&page_ids) {
        add_links(&mut doc, &page.links, *id, &page_ids, &mut skipped)?;
    }
    fonts.write(&mut doc)?;
    add_info(&mut doc, &composition.info)?;
    Ok(Composed {
        document: doc,
        skipped,
    })
}

use font::flate;

/// The objects pages share: pictures and opacity states, written on first use.
#[derive(Default)]
struct Shared {
    images: BTreeMap<usize, ObjId>,
    fill_alphas: BTreeMap<u16, ObjId>,
    stroke_alphas: BTreeMap<u16, ObjId>,
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
            let id = doc.add_object(Object::Dict(pattern(gradient)))?;
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

/// The shading pattern of `gradient`: an axial shading whose function goes through its stops,
/// in `DeviceRGB`, or in a linear `CalRGB` (sRGB primaries, D65) for linear light.
fn pattern(gradient: &LinearGradient) -> Dict {
    let color = |c: [f64; 3]| -> Object {
        let c = if gradient.linear_light {
            c.map(linear)
        } else {
            c.map(|v| v.clamp(0.0, 1.0))
        };
        Object::Array(c.iter().map(|v| Object::Real(*v)).collect())
    };
    let mut stops = gradient.stops.clone();
    stops.retain(|s| s.offset.is_finite());
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if let Some(first) = stops.first().copied().filter(|s| s.offset > 0.0) {
        stops.insert(
            0,
            GradientStop {
                offset: 0.0,
                ..first
            },
        );
    }
    if let Some(last) = stops.last().copied().filter(|s| s.offset < 1.0) {
        stops.push(GradientStop {
            offset: 1.0,
            ..last
        });
    }
    let segment = |a: &GradientStop, b: &GradientStop| {
        let mut f = Dict::new();
        f.insert("FunctionType", Object::Integer(2));
        f.insert(
            "Domain",
            Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
        );
        f.insert("C0", color(a.color));
        f.insert("C1", color(b.color));
        f.insert("N", Object::Integer(1));
        Object::Dict(f)
    };
    let function = match stops.as_slice() {
        [] => segment(
            &GradientStop {
                offset: 0.0,
                color: [0.0; 3],
            },
            &GradientStop {
                offset: 1.0,
                color: [0.0; 3],
            },
        ),
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
                        .map(|s| Object::Real(s.offset))
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

/// An image XObject of `image`, its soft mask written first.
fn write_image(doc: &mut Document, image: &Image) -> Result<ObjId, Error> {
    let mask = match &image.soft_mask {
        Some(mask) => Some(write_image(doc, mask)?),
        None => None,
    };
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("XObject"));
    dict.insert("Subtype", Name::from("Image"));
    dict.insert("Width", Object::Integer(i64::from(image.width)));
    dict.insert("Height", Object::Integer(i64::from(image.height)));
    let space = match &image.color_space {
        ImageColorSpace::Gray => Object::Name(Name::from("DeviceGray")),
        ImageColorSpace::Rgb => Object::Name(Name::from("DeviceRGB")),
        ImageColorSpace::Cmyk => Object::Name(Name::from("DeviceCMYK")),
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

/// The link annotations of the page at `page`.
fn add_links(
    doc: &mut Document,
    links: &[Link],
    page: ObjId,
    pages: &[ObjId],
    skipped: &mut Skipped,
) -> Result<(), Error> {
    let mut annotations = Vec::new();
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
        annotations.push(Object::Ref(doc.add_object(Object::Dict(annotation))?));
    }
    if annotations.is_empty() {
        return Ok(());
    }
    if let Some(Object::Dict(mut dict)) = doc.object(page)? {
        dict.insert("Annots", Object::Array(annotations));
        doc.set_object(page, Object::Dict(dict));
    }
    Ok(())
}

/// The document information dictionary and the catalog's language.
fn add_info(doc: &mut Document, info: &Info) -> Result<(), Error> {
    let mut dict = Dict::new();
    for (key, value) in [
        ("Title", &info.title),
        ("Author", &info.author),
        ("Subject", &info.subject),
        ("Keywords", &info.keywords),
    ] {
        if let Some(value) = value {
            dict.insert(key, Object::String(sealwax_core::encode_text_string(value)));
        }
    }
    if !dict.is_empty() {
        let id = doc.add_object(Object::Dict(dict))?;
        doc.set_info_id(Some(id));
    }
    if let Some(language) = &info.language {
        let catalog = doc.catalog_id();
        if let Some(Object::Dict(mut dict)) = doc.object(catalog)? {
            dict.insert(
                "Lang",
                Object::String(sealwax_core::encode_text_string(language)),
            );
            doc.set_object(catalog, Object::Dict(dict));
        }
    }
    Ok(())
}
