//! Facts about a document that size the work ahead: fonts, tagging, forms, annotations, images,
//! encryption, revisions. Technical facts only: no text from the document is kept, except the
//! producer software and font names.

use std::collections::{BTreeMap, BTreeSet};

use hayro_syntax::object::{Array, Dict, Name, ObjectIdentifier, Stream};
use hayro_syntax::page::Resources;

use crate::document::Document;
use crate::text::decode_text_string;

/// How deep Form XObjects are followed, and how far a field or page tree is walked up or down.
const MAX_DEPTH: usize = 16;

const STANDARD_14: [&str; 14] = [
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
    "Symbol",
    "ZapfDingbats",
];

/// Everything the census records about one document.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Census {
    /// Number of pages.
    pub pages: usize,
    /// Version from the header (`1.7`, `2.0`…).
    pub version: String,
    /// Producer software from the document information, when any.
    pub producer: Option<String>,
    /// An `/Encrypt` dictionary is present (the document opened with the empty password).
    pub encrypted: bool,
    /// End-of-file markers: one per revision (incremental updates add one each).
    pub revisions: usize,
    /// PDF/A level the document claims in its XMP metadata (`3A`, `2B`…).
    pub pdfa_claim: Option<String>,
    /// Tagging facts.
    pub tagging: Tagging,
    /// Interactive form facts.
    pub form: Form,
    /// Annotations by subtype: (total, without an appearance stream).
    pub annotations: BTreeMap<String, (usize, usize)>,
    /// Every distinct font used by page or Form XObject resources.
    pub fonts: Vec<Font>,
    /// Image XObjects by their first filter (`DCTDecode`, `JBIG2Decode`…, `none`).
    pub images: BTreeMap<String, usize>,
    /// Form XObjects drawn by more than one page.
    pub shared_form_xobjects: usize,
    /// Page facts.
    pub page_facts: PageFacts,
    /// The most interpreting a page spends on each of the [`crate::Limits`], when measured
    /// ([`Document::measure_pages`]): the census itself does not interpret pages.
    #[cfg_attr(feature = "serde", serde(default))]
    pub peaks: Option<crate::Peaks>,
}

/// Tagging facts (PDF/A level "a").
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Tagging {
    /// `/MarkInfo << /Marked true >>`.
    pub marked: bool,
    /// `/StructTreeRoot` is present.
    pub struct_tree: bool,
    /// `/Lang` is present in the catalog.
    pub lang: bool,
}

/// Interactive form facts.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Form {
    /// The catalog has an `/AcroForm`.
    pub acroform: bool,
    /// Terminal fields.
    pub fields: usize,
    /// `/NeedAppearances true`.
    pub need_appearances: bool,
    /// An `/XFA` entry is present.
    pub xfa: bool,
    /// Signature fields that hold a signature.
    pub signed_signature_fields: usize,
    /// Signature fields left empty.
    pub empty_signature_fields: usize,
}

/// One distinct font.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Font {
    /// `Type1`, `TrueType`, `Type0`, `Type3`, `MMType1`.
    pub subtype: String,
    /// Base font name, without the subset prefix (`ABCDEF+`).
    pub base_font: String,
    /// Subset prefix present.
    pub subset: bool,
    /// The font program is embedded (Type 3 fonts always are).
    pub embedded: bool,
    /// One of the 14 standard fonts, not embedded.
    pub standard_14: bool,
    /// A `/ToUnicode` CMap is present.
    pub to_unicode: bool,
    /// Encoding: a name, `Differences`, or none.
    pub encoding: Option<String>,
    /// The font descriptor's Symbolic flag.
    pub symbolic: bool,
}

/// Page facts.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PageFacts {
    /// Pages by rotation in degrees (`"0"`, `"90"`…; text keys survive any serde format).
    pub rotations: BTreeMap<String, usize>,
    /// Pages by size class (`A4`, `Letter`, `Legal`, `other`), orientation ignored.
    pub sizes: BTreeMap<String, usize>,
    /// Pages whose UserUnit is not 1.
    pub user_unit_not_1: usize,
    /// Pages with no `/MediaBox` on themselves or any ancestor.
    pub missing_media_box: usize,
}

impl Document {
    /// Survey the document. Never fails: what cannot be read is left out.
    pub fn census(&self) -> Census {
        let pdf = &self.pdf;
        let data: &[u8] = pdf.data().as_ref();
        let catalog = pdf.xref().get::<Dict<'_>>(pdf.xref().root_id());
        let mut census = Census {
            pages: self.page_count(),
            version: format!("{:?}", pdf.version()),
            producer: pdf.metadata().producer.as_deref().map(decode_text),
            encrypted: contains(data, b"/Encrypt"),
            revisions: count(data, b"%%EOF").max(1),
            ..Census::default()
        };
        if let Some(catalog) = &catalog {
            census.pdfa_claim = catalog
                .get::<Stream<'_>>(b"Metadata")
                .filter(|s| self.limits().check_stream(s).is_ok())
                .and_then(|s| s.decoded().ok().map(|d| pdfa_claim(&d)))
                .flatten();
            census.tagging = Tagging {
                marked: catalog
                    .get::<Dict<'_>>(b"MarkInfo")
                    .and_then(|m| m.get::<bool>(b"Marked"))
                    .unwrap_or(false),
                struct_tree: catalog.contains_key(b"StructTreeRoot"),
                lang: catalog.contains_key(b"Lang"),
            };
            if let Some(acroform) = catalog.get::<Dict<'_>>(b"AcroForm") {
                census.form = form(&acroform);
            }
        }

        let mut fonts: BTreeMap<FontKey, Font> = BTreeMap::new();
        let mut form_pages: BTreeMap<ObjectIdentifier, BTreeSet<usize>> = BTreeMap::new();
        let mut visited_forms = BTreeSet::new();
        let mut inherited = std::collections::HashMap::new();
        for (index, page) in pdf.pages().iter().enumerate() {
            let raw = page.raw();
            if let Some(annots) = raw.get::<Array<'_>>(b"Annots") {
                for annot in annots.iter::<Dict<'_>>() {
                    let subtype = name(&annot, b"Subtype").unwrap_or_else(|| "none".into());
                    let entry = census.annotations.entry(subtype).or_default();
                    entry.0 += 1;
                    entry.1 += usize::from(!annot.contains_key(b"AP"));
                }
            }
            let resources = page.resources();
            collect_resources(
                resources,
                &mut fonts,
                &mut census.images,
                &mut visited_forms,
                0,
            );
            for key in resources.x_objects.keys() {
                if let Some(stream) = resources.x_objects.get::<Stream<'_>>(key.as_ref())
                    && name(stream.dict(), b"Subtype").as_deref() == Some("Form")
                {
                    form_pages.entry(stream.obj_id()).or_default().insert(index);
                }
            }

            let geometry =
                &crate::document::page_geometry(index + 1, page, pdf.xref(), &mut inherited);
            *census
                .page_facts
                .rotations
                .entry(geometry.rotation.degrees().to_string())
                .or_default() += 1;
            *census
                .page_facts
                .sizes
                .entry(size_class(geometry.media_box.width(), geometry.media_box.height()).into())
                .or_default() += 1;
            census.page_facts.user_unit_not_1 +=
                usize::from((geometry.user_unit - 1.0).abs() > f64::EPSILON);
            census.page_facts.missing_media_box += usize::from(
                crate::document::page_boxes(raw, pdf.xref(), &mut inherited)
                    .0
                    .is_none(),
            );
        }
        census.fonts = fonts.into_values().collect();
        census.shared_form_xobjects = form_pages.values().filter(|p| p.len() > 1).count();
        census
    }
}

/// Fonts are deduplicated by object when they have one, else by name and subtype.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FontKey {
    Object(ObjectIdentifier),
    Direct(String, String),
}

fn collect_resources(
    resources: &Resources<'_>,
    fonts: &mut BTreeMap<FontKey, Font>,
    images: &mut BTreeMap<String, usize>,
    visited_forms: &mut BTreeSet<ObjectIdentifier>,
    depth: usize,
) {
    for key in resources.fonts.keys() {
        let Some(dict) = resources.fonts.get::<Dict<'_>>(key.as_ref()) else {
            continue;
        };
        let font = font(&dict);
        let font_key = match dict.obj_id() {
            Some(id) => FontKey::Object(id),
            None => FontKey::Direct(font.subtype.clone(), font.base_font.clone()),
        };
        fonts.entry(font_key).or_insert(font);
    }
    if depth >= MAX_DEPTH {
        return;
    }
    for key in resources.x_objects.keys() {
        let Some(stream) = resources.x_objects.get::<Stream<'_>>(key.as_ref()) else {
            continue;
        };
        if !visited_forms.insert(stream.obj_id()) {
            continue;
        }
        match name(stream.dict(), b"Subtype").as_deref() {
            Some("Image") => {
                let filter = first_filter(stream.dict()).unwrap_or_else(|| "none".into());
                *images.entry(filter).or_default() += 1;
            }
            Some("Form") => {
                if let Some(inner) = stream.dict().get::<Dict<'_>>(b"Resources") {
                    collect_resources(
                        &Resources::new(inner),
                        fonts,
                        images,
                        visited_forms,
                        depth + 1,
                    );
                }
            }
            _ => {}
        }
    }
}

fn font(dict: &Dict<'_>) -> Font {
    let subtype = name(dict, b"Subtype").unwrap_or_else(|| "unknown".into());
    let raw_base = name(dict, b"BaseFont").unwrap_or_default();
    let (subset, base_font) = match raw_base.split_once('+') {
        Some((prefix, rest))
            if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            (true, rest.to_string())
        }
        _ => (false, raw_base),
    };
    let descriptor = if subtype == "Type0" {
        dict.get::<Array<'_>>(b"DescendantFonts")
            .and_then(|a| a.iter::<Dict<'_>>().next())
            .and_then(|d| d.get::<Dict<'_>>(b"FontDescriptor"))
    } else {
        dict.get::<Dict<'_>>(b"FontDescriptor")
    };
    let embedded = subtype == "Type3"
        || descriptor.as_ref().is_some_and(|d| {
            d.contains_key(b"FontFile")
                || d.contains_key(b"FontFile2")
                || d.contains_key(b"FontFile3")
        });
    let encoding = if let Some(n) = name(dict, b"Encoding") {
        Some(n)
    } else if dict.get::<Dict<'_>>(b"Encoding").is_some() {
        Some("Differences".into())
    } else if dict.get::<Stream<'_>>(b"Encoding").is_some() {
        Some("embedded CMap".into())
    } else {
        None
    };
    Font {
        standard_14: !embedded && STANDARD_14.contains(&base_font.as_str()),
        symbolic: descriptor
            .as_ref()
            .and_then(|d| d.get::<i64>(b"Flags"))
            .is_some_and(|flags| flags & 4 != 0),
        to_unicode: dict.contains_key(b"ToUnicode"),
        subtype,
        base_font,
        subset,
        embedded,
        encoding,
    }
}

pub(crate) fn form(acroform: &Dict<'_>) -> Form {
    let mut form = Form {
        acroform: true,
        need_appearances: acroform.get::<bool>(b"NeedAppearances").unwrap_or(false),
        xfa: acroform.contains_key(b"XFA"),
        ..Form::default()
    };
    if let Some(fields) = acroform.get::<Array<'_>>(b"Fields") {
        let mut stack: Vec<(Dict<'_>, Option<String>, usize)> =
            fields.iter::<Dict<'_>>().map(|f| (f, None, 0)).collect();
        let mut seen = BTreeSet::new();
        while let Some((field, inherited_type, depth)) = stack.pop() {
            if let Some(id) = field.obj_id()
                && !seen.insert(id)
            {
                continue;
            }
            let field_type = name(&field, b"FT").or(inherited_type);
            let kids: Vec<Dict<'_>> = field
                .get::<Array<'_>>(b"Kids")
                .map(|k| {
                    k.iter::<Dict<'_>>()
                        .filter(|kid| kid.contains_key(b"T") || kid.contains_key(b"FT"))
                        .collect()
                })
                .unwrap_or_default();
            if kids.is_empty() || depth >= MAX_DEPTH {
                form.fields += 1;
                if field_type.as_deref() == Some("Sig") {
                    if field.contains_key(b"V") {
                        form.signed_signature_fields += 1;
                    } else {
                        form.empty_signature_fields += 1;
                    }
                }
            } else {
                stack.extend(
                    kids.into_iter()
                        .map(|kid| (kid, field_type.clone(), depth + 1)),
                );
            }
        }
    }
    form
}

fn name(dict: &Dict<'_>, key: &[u8]) -> Option<String> {
    dict.get::<Name<'_>>(key)
        .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned())
}

fn first_filter(dict: &Dict<'_>) -> Option<String> {
    name(dict, b"Filter").or_else(|| {
        dict.get::<Array<'_>>(b"Filter")
            .and_then(|a| a.iter::<Name<'_>>().next())
            .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned())
    })
}

fn size_class(width: f64, height: f64) -> &'static str {
    let (short, long) = if width < height {
        (width, height)
    } else {
        (height, width)
    };
    let near = |a: f64, b: f64| (a - b).abs() <= 3.0;
    match (short, long) {
        (s, l) if near(s, 595.0) && near(l, 842.0) => "A4",
        (s, l) if near(s, 612.0) && near(l, 792.0) => "Letter",
        (s, l) if near(s, 612.0) && near(l, 1008.0) => "Legal",
        _ => "other",
    }
}

/// `pdfaid:part` and `pdfaid:conformance`, as elements or attributes, e.g. `3A`.
pub(crate) fn pdfa_claim(xmp: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(xmp);
    let value = |key: &str| -> Option<String> {
        let at = text.find(key)? + key.len();
        let rest = text[at..].trim_start_matches(['=', '"', '\'', '>', ' ']);
        let value: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        (!value.is_empty()).then_some(value)
    };
    let part = value("pdfaid:part")?;
    Some(format!(
        "{part}{}",
        value("pdfaid:conformance")
            .unwrap_or_default()
            .to_uppercase()
    ))
}

/// Text from the document information, decoded, without control characters, capped at 80
/// characters.
fn decode_text(bytes: &[u8]) -> String {
    decode_text_string(bytes)
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_string()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}
