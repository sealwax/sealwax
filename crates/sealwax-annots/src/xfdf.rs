//! What a signing page's XFDF holds: the pictures and texts a signer added, the placeholder of a
//! field placed by XFDF only, and the frame of `position_metadata` against PDF user space.
//!
//! The XFDF comes from a browser: only the subset a signing page writes is accepted, the XFDF a
//! web PDF viewer exports for the one annotation it just created (`trn-*` attributes), its
//! `<annots>` renamed `<add>`:
//!
//! - `<fields />` and `<modify />`, empty; `<delete>`, whose `<id page="0">name</id>` entries are
//!   read but never applied; the viewer's own `<pdf-info>` and `<pages>`, ignored;
//! - in `<add>`, `<stamp page="0-based" rect="x1,y1,x2,y2">` whose `<imagedata>` is a PNG data URL
//!   (signature, initials, date and check box all draw a canvas exported as PNG), and `<freetext>`,
//!   the vector text of a text field. Anything else refuses the whole XFDF.
//!
//! The XML must be plain: no DTD, entity, comment, CDATA section or processing instruction but the
//! leading declaration. The XFDF of several fields may be merged as text, so markup that hides
//! text could swallow the annotations of the fields after it.
//!
//! The frame of `position_metadata` is the signing page's: the viewer's page matrix for a PDF page
//! is `[1 0 0 -1 -cropX1 cropY2]`, without a rotation term. It is the page unrotated, origin at the
//! top-left corner of its crop box, x to the right, y downward, in points. The page's `/Rotate`
//! turns it with the page when it is displayed.

use std::fmt;

use base64::Engine as _;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};
use sealwax_core::Rect;

const XFDF_NAMESPACE: &str = "http://ns.adobe.com/xfdf/";
const PDF_INFO: &str = "{http://www.pdftron.com/pdfinfo}pdf-info";
const PNG_DATA_URL: &str = "data:image/png;base64,";
/// The field types of a signing page (`trn-custom-data`).
const FIELD_TYPES: [&str; 5] = ["SIGNATURE", "INITIALS", "DATE", "CHECKBOX", "FREETEXT"];
/// Elements nested deeper than this are no XFDF a signing page writes.
const MAX_DEPTH: usize = 32;
/// Elements an XFDF may hold.
const MAX_ELEMENTS: usize = 100_000;
/// The longest element name shown in a message: it comes from the browser.
const SHOWN: usize = 40;
/// A text's size when nothing gives one.
const DEFAULT_SIZE: f64 = 12.0;

/// Why an XFDF is refused. The message never holds a value of the XFDF but element names, cut
/// short.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("XFDF refused: {0}")]
pub struct Refused(String);

fn refused(reason: impl Into<String>) -> Refused {
    Refused(reason.into())
}

/// A picture the XFDF draws: its page, its rectangle in PDF user space, and the PNG.
#[derive(Clone, PartialEq)]
pub struct Picture {
    /// 1-based.
    pub page_number: usize,
    /// Normalized, in PDF user space.
    pub rect: Rect,
    /// The PNG as the signing page made it.
    pub png: Vec<u8>,
    /// Degrees (0, 90, 180 or 270) the picture is turned counterclockwise in user space, within
    /// `rect` (`rotation`): a signing page sets the page's own rotation, so that the picture
    /// shows upright on the page as shown.
    pub rotation: u16,
    /// The field it fills, as its custom data names it (`fieldId`), to tell the caller which
    /// field's picture it is.
    pub field_id: Option<String>,
}

impl fmt::Debug for Picture {
    // The picture is a signature: kept out of debug output and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("page_number", &self.page_number)
            .field("rect", &self.rect)
            .field("png_bytes", &self.png.len())
            .field("rotation", &self.rotation)
            .field("field_id", &self.field_id)
            .finish()
    }
}

/// Horizontal alignment of a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// The default.
    #[default]
    Left,
    Center,
    Right,
}

/// Vertical alignment of a text in its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalAlign {
    /// The default.
    #[default]
    Top,
    Center,
    Bottom,
}

/// A text the XFDF draws (`<freetext>`).
#[derive(Clone, PartialEq)]
pub struct Text {
    /// 1-based.
    pub page_number: usize,
    /// Normalized, in PDF user space.
    pub rect: Rect,
    /// What the signer typed.
    pub contents: String,
    /// The lines the viewer showed (`trn-wrapped-text-lines`), when given.
    pub lines: Option<Vec<String>>,
    /// Font size, in points.
    pub size: f64,
    /// Fill colour: gray, RGB or CMYK components.
    pub color: Vec<f64>,
    pub align: Align,
    pub vertical: VerticalAlign,
    /// Border width (`width`), 1 when not given, as for any annotation.
    pub border: f64,
    /// Border colour (`color`), black when not given.
    pub border_color: Option<Vec<f64>>,
    /// Background (`interior-color`), none when not given.
    pub fill: Option<Vec<f64>>,
    /// Degrees (0, 90, 180 or 270) the text is turned counterclockwise in user space, within
    /// `rect`, as [`Picture::rotation`].
    pub rotation: u16,
}

impl fmt::Debug for Text {
    // What the signer typed is kept out of debug output and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Text")
            .field("page_number", &self.page_number)
            .field("rect", &self.rect)
            .field("characters", &self.contents.chars().count())
            .field("size", &self.size)
            .field("align", &self.align)
            .field("vertical", &self.vertical)
            .finish_non_exhaustive()
    }
}

/// An annotation of `<add>`, in the order of the XFDF.
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Picture(Picture),
    Text(Text),
}

/// What an XFDF adds, and the annotations it deletes, as (page number, name).
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub marks: Vec<Mark>,
    pub deleted: Vec<(usize, String)>,
}

/// Where an old field placed by XFDF only sits, and the type its custom data names.
#[derive(Debug, Clone, PartialEq)]
pub struct Placeholder {
    /// 1-based.
    pub page_number: usize,
    /// Normalized, in PDF user space.
    pub rect: Rect,
    /// One of the signing page's field types, `None` when the custom data names none: the caller
    /// reads it from the field's identifier, as the signing page does.
    pub field_type: Option<String>,
}

/// Read what an XFDF a signing page wrote adds and deletes.
///
/// # Errors
///
/// [`Refused`] when the XFDF is not plain, well-formed XML with an `<xfdf>` root, or holds anything
/// outside the subset a signing page writes.
pub fn read_command(xfdf: &str) -> Result<Command, Refused> {
    let root = parse(xfdf)?;
    let mut marks = Vec::new();
    let mut deleted = Vec::new();
    for section in &root.children {
        match section.name.as_str() {
            PDF_INFO | "pages" => {}
            "fields" | "modify" => {
                if !section.children.is_empty() {
                    return Err(refused(format!("<{}> is not empty", section.name)));
                }
            }
            "add" => {
                for annotation in &section.children {
                    marks.push(mark(annotation, marks.len() + 1)?);
                }
            }
            "delete" => {
                for entry in &section.children {
                    if entry.name != "id" {
                        return Err(refused(format!(
                            "unsupported element <{}> in <delete>",
                            shown(&entry.name)
                        )));
                    }
                    deleted.push((page_number(entry)?, entry.text.trim().to_owned()));
                }
            }
            other => {
                return Err(refused(format!("unsupported element <{}>", shown(other))));
            }
        }
    }
    Ok(Command { marks, deleted })
}

/// Refuse an XFDF that is not plain XML; see [`read_command`].
///
/// # Errors
///
/// [`Refused`] as [`read_command`] for what is not plain, well-formed XML with an `<xfdf>` root.
pub fn check_plain(xfdf: &str) -> Result<(), Refused> {
    parse(xfdf).map(|_| ())
}

/// The annotations in the `<add>` sections of an XFDF.
///
/// # Errors
///
/// [`Refused`] as [`check_plain`].
pub fn added_annotation_count(xfdf: &str) -> Result<usize, Refused> {
    Ok(parse(xfdf)?
        .children
        .iter()
        .filter(|section| section.name == "add")
        .map(|section| section.children.len())
        .sum())
}

/// The placeholder of a field placed by XFDF only: its one `<freetext>` in `<add>`.
///
/// # Errors
///
/// [`Refused`] as [`check_plain`], or when the XFDF does not hold exactly one free text annotation
/// with a page and a rectangle.
pub fn placeholder(xfdf: &str) -> Result<Placeholder, Refused> {
    let root = parse(xfdf)?;
    let found: Vec<&Element> = root
        .children
        .iter()
        .filter(|section| section.name == "add")
        .flat_map(|section| &section.children)
        .filter(|annotation| annotation.name == "freetext")
        .collect();
    let [annotation] = found.as_slice() else {
        return Err(refused(format!(
            "{} free text annotations, a placeholder is one",
            found.len()
        )));
    };
    let field_type = custom_data(annotation)
        .and_then(|data| data.get("type")?.as_str().map(str::to_owned))
        .filter(|kind| FIELD_TYPES.contains(&kind.as_str()));
    Ok(Placeholder {
        page_number: page_number(annotation)?,
        rect: rect(annotation)?,
        field_type,
    })
}

/// A rectangle of `position_metadata` in PDF user space.
pub fn to_user_space(x: f64, y: f64, width: f64, height: f64, crop_box: Rect) -> Rect {
    let (left, top) = (crop_box.x1, crop_box.y2);
    Rect::from_corners(left + x, top - y - height, left + x + width, top - y)
}

/// A rectangle of PDF user space in the frame of `position_metadata`, as (x, y, width, height).
pub fn to_position_frame(rect: Rect, crop_box: Rect) -> (f64, f64, f64, f64) {
    let (left, top) = (crop_box.x1, crop_box.y2);
    (
        rect.x1 - left,
        top - rect.y2,
        rect.x2 - rect.x1,
        rect.y2 - rect.y1,
    )
}

// --- the annotations ------------------------------------------------------------------------

fn mark(annotation: &Element, position: usize) -> Result<Mark, Refused> {
    match annotation.name.as_str() {
        "stamp" => picture(annotation, position).map(Mark::Picture),
        "freetext" => text(annotation, position).map(Mark::Text),
        other => Err(refused(format!(
            "annotation {position} is a <{}>, only PNG stamps and free texts are drawn",
            shown(other)
        ))),
    }
}

/// What every annotation drawn must satisfy: turned by quarter turns only, not hidden. Returns
/// its rotation, in degrees counterclockwise from 0 to 270.
fn drawable(annotation: &Element, position: usize) -> Result<u16, Refused> {
    let rotation = annotation.attribute("rotation").unwrap_or("0");
    let degrees = number(rotation).ok_or_else(|| {
        refused(format!(
            "annotation {position} has a value that is not a number"
        ))
    })?;
    let turned = degrees.rem_euclid(360.0);
    let rotation = [0_u16, 90, 180, 270, 360]
        .into_iter()
        .find(|quarter| (turned - f64::from(*quarter)).abs() < 1e-6)
        .ok_or_else(|| {
            refused(format!(
                "annotation {position} is turned by other than a quarter turn"
            ))
        })?
        % 360;
    let hidden = annotation
        .attribute("flags")
        .unwrap_or_default()
        .split(',')
        .any(|flag| matches!(flag.trim(), "hidden" | "noview"));
    if hidden {
        return Err(refused(format!("annotation {position} is hidden")));
    }
    Ok(rotation)
}

fn picture(annotation: &Element, position: usize) -> Result<Picture, Refused> {
    let rotation = drawable(annotation, position)?;
    let contents: Vec<&Element> = annotation
        .children
        .iter()
        .filter(|child| child.name != "trn-custom-data")
        .collect();
    let [image] = contents.as_slice() else {
        return Err(refused(format!(
            "annotation {position} carries no single <imagedata>"
        )));
    };
    if image.name != "imagedata" {
        return Err(refused(format!(
            "annotation {position} carries no single <imagedata>"
        )));
    }
    let data: String = image.text.split_whitespace().collect();
    let Some(encoded) = data.strip_prefix(PNG_DATA_URL) else {
        return Err(refused(format!(
            "annotation {position} is not a PNG data URL"
        )));
    };
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| refused(format!("annotation {position} has malformed base64")))?;
    let field_id = custom_data(annotation)
        .and_then(|data| data.get("fieldId").cloned())
        .and_then(|id| match id {
            serde_json::Value::String(id) => Some(id),
            serde_json::Value::Number(id) => Some(id.to_string()),
            _ => None,
        });
    Ok(Picture {
        page_number: page_number(annotation)?,
        rect: rect(annotation)?,
        png,
        rotation,
        field_id,
    })
}

/// The children a `<freetext>` of a signing page holds.
const TEXT_CHILDREN: [&str; 5] = [
    "contents",
    "contents-richtext",
    "defaultappearance",
    "defaultstyle",
    "trn-custom-data",
];

fn text(annotation: &Element, position: usize) -> Result<Text, Refused> {
    let rotation = drawable(annotation, position)?;
    if let Some(other) = annotation
        .children
        .iter()
        .find(|child| !TEXT_CHILDREN.contains(&child.name.as_str()))
    {
        return Err(refused(format!(
            "annotation {position} holds a <{}>",
            shown(&other.name)
        )));
    }
    let child = |name: &str| annotation.children.iter().find(|c| c.name == name);
    let contents = child("contents")
        .map(|c| c.text.clone())
        .unwrap_or_default();
    let appearance = child("defaultappearance").map(|c| default_appearance(&c.text));
    let style = child("defaultstyle").map(|c| default_style(&c.text));
    let size = appearance
        .as_ref()
        .and_then(|a| a.size)
        .or_else(|| annotation.attribute("FontSize").and_then(number))
        .or_else(|| style.as_ref().and_then(|s| s.size))
        .filter(|size| *size > 0.0)
        .unwrap_or(DEFAULT_SIZE);
    let color = appearance
        .as_ref()
        .and_then(|a| a.color.clone())
        .or_else(|| annotation.attribute("TextColor").and_then(hex_color))
        .or_else(|| style.as_ref().and_then(|s| s.color.clone()))
        .unwrap_or_else(|| vec![0.0]);
    let lines = custom_data(annotation).and_then(|data| {
        let lines = data.get("trn-wrapped-text-lines")?;
        // The viewer writes the array as a JSON string.
        let parsed: serde_json::Value = match lines {
            serde_json::Value::String(text) => serde_json::from_str(text).ok()?,
            other => other.clone(),
        };
        parsed
            .as_array()?
            .iter()
            .map(|line| line.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>()
    });
    Ok(Text {
        page_number: page_number(annotation)?,
        rect: rect(annotation)?,
        contents,
        lines,
        size,
        color,
        align: style.as_ref().map(|s| s.align).unwrap_or_default(),
        vertical: style.as_ref().map(|s| s.vertical).unwrap_or_default(),
        border: annotation
            .attribute("width")
            .and_then(number)
            .filter(|w| *w >= 0.0)
            .unwrap_or(1.0),
        border_color: annotation.attribute("color").and_then(hex_color),
        fill: annotation.attribute("interior-color").and_then(hex_color),
        rotation,
    })
}

/// What a `/DA` string says: the font size of `Tf`, the colour of the last `g`, `rg` or `k`.
#[derive(Debug, Default)]
pub(crate) struct DefaultAppearance {
    pub(crate) size: Option<f64>,
    pub(crate) color: Option<Vec<f64>>,
}

pub(crate) fn default_appearance(da: &str) -> DefaultAppearance {
    let mut found = DefaultAppearance::default();
    let mut operands: Vec<f64> = Vec::new();
    for token in da.split_whitespace() {
        if let Some(value) = number(token) {
            operands.push(value);
            continue;
        }
        let wanted = match token {
            "g" => 1,
            "rg" => 3,
            "k" => 4,
            "Tf" => {
                found.size = operands.last().copied();
                0
            }
            _ => 0,
        };
        if wanted > 0 && operands.len() >= wanted {
            found.color = Some(operands[operands.len() - wanted..].to_vec());
        }
        // A name (the font) does not clear the operands of `Tf`.
        if !token.starts_with('/') {
            operands.clear();
        }
    }
    found
}

/// What the viewer's `defaultstyle` (CSS declarations) says.
struct DefaultStyle {
    size: Option<f64>,
    color: Option<Vec<f64>>,
    align: Align,
    vertical: VerticalAlign,
}

fn default_style(css: &str) -> DefaultStyle {
    let mut style = DefaultStyle {
        size: None,
        color: None,
        align: Align::Left,
        vertical: VerticalAlign::Top,
    };
    for declaration in css.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match property.trim() {
            "font" => {
                style.size = value
                    .split_whitespace()
                    .find_map(|part| part.strip_suffix("pt").and_then(number));
            }
            "font-size" => style.size = value.strip_suffix("pt").and_then(number),
            "color" => style.color = hex_color(value),
            "text-align" => {
                style.align = match value {
                    "center" => Align::Center,
                    "right" => Align::Right,
                    _ => Align::Left,
                };
            }
            "text-vertical-align" => {
                style.vertical = match value {
                    "center" | "middle" => VerticalAlign::Center,
                    "bottom" => VerticalAlign::Bottom,
                    _ => VerticalAlign::Top,
                };
            }
            _ => {}
        }
    }
    style
}

/// `#RRGGBB` as RGB components.
fn hex_color(value: &str) -> Option<Vec<f64>> {
    let hex = value.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    (0..3)
        .map(|i| {
            u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
                .ok()
                .map(|v| f64::from(v) / 255.0)
        })
        .collect()
}

fn custom_data(annotation: &Element) -> Option<serde_json::Map<String, serde_json::Value>> {
    let bytes = annotation
        .children
        .iter()
        .find(|child| child.name == "trn-custom-data")?
        .attribute("bytes")?;
    match serde_json::from_str(bytes).ok()? {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
}

fn page_number(element: &Element) -> Result<usize, Refused> {
    let page = element.attribute("page").unwrap_or_default().trim();
    if page.is_empty() || !page.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refused("a page index is missing or not a number"));
    }
    page.parse::<usize>()
        .ok()
        .and_then(|index| index.checked_add(1))
        .ok_or_else(|| refused("a page index is out of range"))
}

fn rect(element: &Element) -> Result<Rect, Refused> {
    let values: Vec<&str> = element
        .attribute("rect")
        .unwrap_or_default()
        .split(',')
        .collect();
    let [x1, y1, x2, y2] = values.as_slice() else {
        return Err(refused("a rect does not have 4 values"));
    };
    let value =
        |text: &str| number(text).ok_or_else(|| refused("a rect has a value that is not a number"));
    Ok(Rect::from_corners(
        value(x1)?,
        value(y1)?,
        value(x2)?,
        value(y2)?,
    ))
}

/// A finite number, as Python's `float()` reads one.
fn number(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn shown(name: &str) -> String {
    name.chars().take(SHOWN).collect()
}

// --- the XML ------------------------------------------------------------------------------------

/// An element, as far as reading needs: its name (without the XFDF namespace; a foreign one is
/// kept as `{namespace}name`), attributes, children and text.
#[derive(Debug, Default)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Element>,
    text: String,
}

impl Element {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The root `<xfdf>` element of plain, well-formed XML.
fn parse(xfdf: &str) -> Result<Element, Refused> {
    let malformed = |_| refused("not well-formed XML");
    let mut reader = NsReader::from_str(xfdf.trim());
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut elements = 0usize;
    let mut first = true;
    loop {
        let (namespace, event) = reader.read_resolved_event().map_err(malformed)?;
        let leading = std::mem::replace(&mut first, false);
        match event {
            Event::Decl(_) if leading => {}
            Event::Decl(_) | Event::PI(_) => {
                return Err(refused("it holds a processing instruction"));
            }
            Event::DocType(_) => return Err(refused("it declares a DTD")),
            Event::Comment(_) => return Err(refused("it holds a comment")),
            Event::CData(_) => return Err(refused("it holds a CDATA section")),
            Event::Start(ref start) | Event::Empty(ref start) => {
                // One root: an element after it is not well-formed XML.
                if root.is_some() {
                    return Err(refused("not well-formed XML"));
                }
                elements += 1;
                if elements > MAX_ELEMENTS || stack.len() >= MAX_DEPTH {
                    return Err(refused("too many elements"));
                }
                let local = start.local_name().as_ref().to_owned();
                let name = match namespace {
                    ResolveResult::Bound(ns) if ns.as_ref() != XFDF_NAMESPACE => {
                        format!("{{{}}}{local}", ns.as_ref())
                    }
                    ResolveResult::Unknown(_) => {
                        return Err(refused("an unknown namespace prefix"));
                    }
                    _ => local,
                };
                let mut attributes = Vec::new();
                for attribute in start.attributes() {
                    let attribute = attribute.map_err(|_| refused("not well-formed XML"))?;
                    let key = attribute.key.as_ref().to_owned();
                    let value = attribute
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|_| refused("an attribute refers to an entity"))?
                        .into_owned();
                    attributes.push((key, value));
                }
                let element = Element {
                    name,
                    attributes,
                    ..Element::default()
                };
                if matches!(event, Event::Empty(_)) {
                    close(element, &mut stack, &mut root);
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let element = stack.pop().ok_or_else(|| refused("not well-formed XML"))?;
                close(element, &mut stack, &mut root);
            }
            Event::Text(text) => {
                let content = text.xml10_content();
                match stack.last_mut() {
                    Some(element) => element.text.push_str(&content),
                    None if content.trim().is_empty() => {}
                    None => return Err(refused("not well-formed XML")),
                }
            }
            Event::GeneralRef(reference) => {
                let character = match reference
                    .resolve_char_ref()
                    .map_err(|_| refused("a malformed character reference"))?
                {
                    Some(character) => character,
                    None => match &*reference {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "apos" => '\'',
                        "quot" => '"',
                        _ => return Err(refused("it refers to an entity")),
                    },
                };
                match stack.last_mut() {
                    Some(element) => element.text.push(character),
                    None => return Err(refused("not well-formed XML")),
                }
            }
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(refused("not well-formed XML"));
    }
    let root = root.ok_or_else(|| refused("not well-formed XML"))?;
    if root.name != "xfdf" {
        return Err(refused("the root is not <xfdf>"));
    }
    Ok(root)
}

fn close(element: Element, stack: &mut [Element], root: &mut Option<Element>) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None => *root = Some(element),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use sealwax_core::Rect;

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really";

    fn data_url(content: &[u8], kind: &str) -> String {
        format!(
            "data:image/{kind};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(content)
        )
    }

    pub(crate) fn stamp(
        rect: &str,
        page: i64,
        image: Option<&str>,
        extra: &str,
        attributes: &str,
    ) -> String {
        let image = image.map_or_else(|| data_url(PNG, "png"), str::to_owned);
        format!(
            "<stamp page=\"{page}\" rect=\"{rect}\" flags=\"print,readonly,locked\" name=\"a1b2c3\" \
             title=\"Signer A\" subject=\"Draft\" date=\"D:20261110120000-05'00'\" icon=\"Draft\"{attributes}>\
             <trn-custom-data bytes=\"{{&quot;fieldId&quot;:&quot;f-1&quot;,&quot;signed&quot;:&quot;true&quot;}}\"/>\
             <imagedata>{image}</imagedata>{extra}</stamp>"
        )
    }

    fn default_stamp() -> String {
        stamp("100,600,160,620", 0, None, "", "")
    }

    /// One field's XFDF as a signing page sends it: the viewer's export, `<annots>` renamed `<add>`.
    pub(crate) fn exported(annotations: &[&str], delete: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\
             <pdf-info xmlns=\"http://www.pdftron.com/pdfinfo\" version=\"2\" import-version=\"4\" /><fields />\
             <fields />\n<add>{}</add>\n<modify />\n{delete}<pages><defmtx matrix=\"1,0,0,-1,0,800\" /></pages></xfdf>",
            annotations.concat()
        )
    }

    const FREETEXT: &str = "<freetext page=\"0\" rect=\"10,10,130,35\" flags=\"print,readonly,locked\" \
        name=\"n\" subject=\"Free Text\" width=\"0\" TextColor=\"#000000\" FontSize=\"16\">\
        <trn-custom-data bytes=\"{&quot;trn-wrapped-text-lines&quot;:&quot;[\\&quot;Jean\\&quot;]&quot;}\"/>\
        <contents>Jean</contents><contents-richtext><body><p><span>Jean</span></p></body></contents-richtext>\
        <defaultappearance>0 0 0 rg /Helvetica 16 Tf</defaultappearance>\
        <defaultstyle>font: Helvetica 16pt; text-align: left; text-vertical-align: center; color: #000000</defaultstyle>\
        </freetext>";

    #[test]
    fn pictures_texts_and_deletions_are_read_in_order() {
        let xfdf = exported(
            &[&stamp("160,620,100,600", 2, None, "", ""), FREETEXT],
            "<delete><id page=\"0\">placeholder-1</id></delete>",
        );
        let command = read_command(&xfdf).unwrap();
        let [Mark::Picture(picture), Mark::Text(text)] = command.marks.as_slice() else {
            panic!("a picture then a text");
        };
        assert_eq!(picture.page_number, 3);
        assert_eq!(picture.rect, Rect::from_corners(100.0, 600.0, 160.0, 620.0));
        assert_eq!(picture.png, PNG);
        assert!(!format!("{picture:?}").contains("not really"));
        assert_eq!(text.contents, "Jean");
        assert_eq!(text.lines.as_deref(), Some(&["Jean".to_owned()][..]));
        assert_eq!(text.size, 16.0);
        assert_eq!(text.color, [0.0, 0.0, 0.0]);
        assert_eq!(text.align, Align::Left);
        assert_eq!(text.vertical, VerticalAlign::Center);
        assert!(!format!("{text:?}").contains("Jean"));
        assert_eq!(command.deleted, [(1, "placeholder-1".to_owned())]);
    }

    #[test]
    fn a_rotation_by_quarter_turns_is_read_from_0_to_270() {
        for (attribute, expected) in [
            ("", 0),
            (" rotation=\"0\"", 0),
            (" rotation=\"90\"", 90),
            (" rotation=\"180.0\"", 180),
            (" rotation=\"270\"", 270),
            (" rotation=\"-90\"", 270),
            (" rotation=\"450\"", 90),
            (" rotation=\"360\"", 0),
        ] {
            let text = FREETEXT.replace("<freetext ", &format!("<freetext{attribute} "));
            let xfdf = exported(
                &[&stamp("100,600,160,620", 0, None, "", attribute), &text],
                "<delete />",
            );
            let command = read_command(&xfdf).unwrap();
            let [Mark::Picture(picture), Mark::Text(text)] = command.marks.as_slice() else {
                panic!("a picture then a text");
            };
            assert_eq!(
                (picture.rotation, text.rotation),
                (expected, expected),
                "{attribute}"
            );
        }
    }

    #[test]
    fn an_xfdf_without_annotations_is_read() {
        let command = read_command(&exported(&[], "<delete />")).unwrap();
        assert!(command.marks.is_empty() && command.deleted.is_empty());
    }

    #[test]
    fn what_the_signer_page_does_not_write_is_refused() {
        let refused_cases = [
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\"><inklist><gesture>10,10;20,20</gesture></inklist></ink>"], "<delete />"),
            exported(&["<stamp page=\"0\" rect=\"10,10,90,30\" />"], "<delete />"),
            exported(&[&stamp("100,600,160,620", 0, None, &format!("<imagedata>{}</imagedata>", data_url(PNG, "png")), "")], "<delete />"),
            exported(&[&stamp("100,600,160,620", 0, None, "<appearance>AAAA</appearance>", "")], "<delete />"),
            exported(&[&stamp("100,600,160,620", 0, Some(&data_url(b"\xff\xd8\xff\xe0", "jpeg")), "", "")], "<delete />"),
            exported(&[&stamp("100,600,160,620", 0, Some("data:image/svg+xml;base64,PHN2Zy8+"), "", "")], "<delete />"),
            exported(&[&stamp("100,600,160,620", 0, Some("data:image/png;base64,***"), "", "")], "<delete />"),
            exported(&[&default_stamp().replace("flags=\"print,readonly,locked\"", "flags=\"hidden\"")], "<delete />"),
            exported(&[&stamp("100,600,160,620", 0, None, "", " rotation=\"45\"")], "<delete />"),
            exported(&[&default_stamp().replace("page=\"0\" ", "")], "<delete />"),
            exported(&[&stamp("100,600,160,620", -1, None, "", "")], "<delete />"),
            exported(&[&stamp("1,2,3", 0, None, "", "")], "<delete />"),
            exported(&[&stamp("1,2,3,nan", 0, None, "", "")], "<delete />"),
            exported(&[&FREETEXT.replace("<contents>", "<popup/><contents>")], "<delete />"),
            exported(&[&FREETEXT.replace("flags=\"print,readonly,locked\"", "flags=\"noview\"")], "<delete />"),
            exported(&[], "<delete />").replacen("<fields />", "<fields><field name=\"a\"><value>b</value></field></fields>", 1),
            exported(&[], "<delete />").replace("<modify />", "<modify><square page=\"0\" rect=\"1,1,2,2\" /></modify>"),
            exported(&[], "<delete />").replace("<modify />", "<annots />"),
            exported(&[], "<delete><square /></delete>"),
            "<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\" />".to_owned(),
            "<xfdf><add>".to_owned(),
            "<?xml version=\"1.0\"?><!DOCTYPE xfdf [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;\">]>\
             <xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><add>&b;</add></xfdf>"
                .to_owned(),
            "<?xml version=\"1.0\"?><!DOCTYPE xfdf [<!ENTITY x SYSTEM \"file:///etc/passwd\">]>\
             <xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><add>&x;</add></xfdf>"
                .to_owned(),
            "<?xml version=\"1.0\"?><!DOCTYPE xfdf><xfdf xmlns=\"http://ns.adobe.com/xfdf/\" />".to_owned(),
            "<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><add>&unknown;</add></xfdf>".to_owned(),
            "<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"></xfdf><xfdf/>".to_owned(),
        ];
        for (index, xfdf) in refused_cases.iter().enumerate() {
            assert!(read_command(xfdf).is_err(), "case {index} was accepted");
        }
    }

    #[test]
    fn markup_that_hides_text_is_refused() {
        let plain = exported(&[&default_stamp()], "<delete />");
        assert!(check_plain(&plain).is_ok());
        for hiding in ["<!-- x -->", "<![CDATA[x]]>", "<?php x ?>"] {
            let xfdf = plain.replace("<modify />", &format!("<modify />{hiding}"));
            assert!(check_plain(&xfdf).is_err(), "{hiding}");
            assert!(read_command(&xfdf).is_err(), "{hiding}");
        }
    }

    #[test]
    fn annotations_added_are_counted() {
        let xfdf = exported(&[&default_stamp(), FREETEXT], "<delete />");
        assert_eq!(added_annotation_count(&xfdf), Ok(2));
    }

    const LEGACY: &str = r#"
        <?xml version="1.0" encoding="UTF-8" ?>
        <xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">
        <fields />
        <add>
        <freetext page="1" rect="72.5,100,222.5,140" name="signature1" title="Guest" subject="Signature">
        <trn-custom-data bytes="{&quot;type&quot;:&quot;SIGNATURE&quot;,&quot;id&quot;:&quot;8550a9bb&quot;}"/>
        <contents>Signature</contents>
        </freetext>
        </add>
        </xfdf>
    "#;

    #[test]
    fn a_placeholder_is_read_with_its_field_type() {
        let found = placeholder(LEGACY).unwrap();
        assert_eq!(
            found,
            Placeholder {
                page_number: 2,
                rect: Rect::from_corners(72.5, 100.0, 222.5, 140.0),
                field_type: Some("SIGNATURE".to_owned()),
            }
        );
        assert_eq!(
            placeholder(&LEGACY.replace("SIGNATURE", "SOMETHING"))
                .unwrap()
                .field_type,
            None
        );
        assert_eq!(
            placeholder(&LEGACY.replace("trn-custom-data", "other-data"))
                .unwrap()
                .field_type,
            None
        );
        let signed = exported(&[&default_stamp()], "<delete />");
        assert!(
            placeholder(&signed)
                .unwrap_err()
                .to_string()
                .contains("0 free text")
        );
    }

    #[test]
    fn the_position_frame_is_the_unrotated_page_from_the_crop_box_top_left() {
        let crop = Rect::from_corners(50.0, 70.0, 550.0, 760.0);
        let rect = to_user_space(100.0, 150.0, 120.0, 40.0, crop);
        assert_eq!(rect, Rect::from_corners(150.0, 570.0, 270.0, 610.0));
        assert_eq!(to_position_frame(rect, crop), (100.0, 150.0, 120.0, 40.0));
    }

    #[test]
    fn a_default_appearance_gives_its_size_and_color() {
        let found = default_appearance("0 0 1 rg /Helv 11 Tf");
        assert_eq!(found.size, Some(11.0));
        assert_eq!(found.color, Some(vec![0.0, 0.0, 1.0]));
        let found = default_appearance("/Helv 0 Tf 0.5 g");
        assert_eq!(found.size, Some(0.0));
        assert_eq!(found.color, Some(vec![0.5]));
    }
}
