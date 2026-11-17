//! What a signing page's XFDF holds: the pictures, texts and drawings a signer added, the
//! placeholder of a field placed by XFDF only, and the frame of `position_metadata` against PDF
//! user space.
//!
//! The XFDF comes from a browser: only the subset a signing page writes is accepted, the XFDF a
//! web PDF viewer exports for the one annotation it just created (`trn-*` attributes), its
//! `<annots>` renamed `<add>`:
//!
//! - `<fields />` and `<modify />`, empty; `<delete>`, whose `<id page="0">name</id>` entries are
//!   read but never applied; the viewer's own `<pdf-info>` and `<pages>`, ignored;
//! - in `<add>`, `<stamp page="0-based" rect="x1,y1,x2,y2">` whose `<imagedata>` is a PNG data URL
//!   (signature, initials, date and check box all draw a canvas exported as PNG), `<freetext>`,
//!   the vector text of a text field, and `<ink>`, a signature drawn by hand (its `<inklist>` of
//!   `<gesture>`s). Anything else refuses the whole XFDF.
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
/// The points the drawings of one XFDF may have, all strokes counted.
const MAX_INK_POINTS: usize = 500_000;

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
    /// The font's name, as the viewer gave it (`defaultstyle`, else `defaultappearance`).
    pub font: Option<String>,
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

/// A drawing the XFDF holds (`<ink>`): a signature drawn by hand, its strokes in PDF user space.
#[derive(Clone, PartialEq)]
pub struct Ink {
    /// 1-based.
    pub page_number: usize,
    /// Normalized, in PDF user space.
    pub rect: Rect,
    /// Each stroke's points, in PDF user space.
    pub strokes: Vec<Vec<[f64; 2]>>,
    /// RGB, black when not given.
    pub color: Vec<f64>,
    /// The strokes' width (`width`), 1 when not given.
    pub width: f64,
}

impl fmt::Debug for Ink {
    // The strokes are a signature: kept out of debug output and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ink")
            .field("page_number", &self.page_number)
            .field("rect", &self.rect)
            .field("strokes", &self.strokes.len())
            .field("points", &self.strokes.iter().map(Vec::len).sum::<usize>())
            .field("width", &self.width)
            .finish_non_exhaustive()
    }
}

/// An annotation of `<add>`, in the order of the XFDF.
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Picture(Picture),
    Text(Text),
    Ink(Ink),
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
                let points: usize = marks
                    .iter()
                    .map(|mark| match mark {
                        Mark::Ink(ink) => ink.strokes.iter().map(Vec::len).sum(),
                        _ => 0,
                    })
                    .sum();
                if points > MAX_INK_POINTS {
                    return Err(refused(format!(
                        "its drawings have more than {MAX_INK_POINTS} points"
                    )));
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

// --- writing ------------------------------------------------------------------------------

/// A picture to write ([`write_command`]): a field signed with a picture (signature, initials,
/// date, check box), as a signing page adds it.
#[derive(Clone, PartialEq)]
pub struct NewPicture {
    /// From 0.
    pub page_index: usize,
    /// In PDF user space, the page unrotated, y upward.
    pub rect: Rect,
    /// Degrees counterclockwise (0, 90, 180 or 270): the page's rotation, for a picture that
    /// shows upright on a turned page.
    pub rotation: u16,
    /// The PNG.
    pub png: Vec<u8>,
    /// The annotation's name (`name`): what identifies it, as a viewer's annotation id does.
    pub name: String,
    /// Written as the annotation's custom data (`trn-custom-data`), a JSON object of strings:
    /// `fieldId` names the field it fills.
    pub custom_data: Vec<(String, String)>,
}

impl fmt::Debug for NewPicture {
    // The picture is a signature: kept out of debug output and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewPicture")
            .field("page_index", &self.page_index)
            .field("rect", &self.rect)
            .field("rotation", &self.rotation)
            .field("png_bytes", &self.png.len())
            .finish_non_exhaustive()
    }
}

/// A free text to write ([`write_command`]): a text field signed, as a signing page adds it.
#[derive(Clone, PartialEq)]
pub struct NewText {
    /// From 0.
    pub page_index: usize,
    /// In PDF user space, the page unrotated, y upward.
    pub rect: Rect,
    /// Degrees counterclockwise (0, 90, 180 or 270), as [`NewPicture::rotation`].
    pub rotation: u16,
    /// What the signer typed.
    pub contents: String,
    /// In points.
    pub size: f64,
    /// RGB, each from 0 to 1.
    pub color: [f64; 3],
    pub align: Align,
    pub vertical: VerticalAlign,
    /// The font's name, as the viewer names it (`Helvetica`): letters, digits, `-` and `_`.
    pub font: String,
    /// As [`NewPicture::name`].
    pub name: String,
    /// As [`NewPicture::custom_data`].
    pub custom_data: Vec<(String, String)>,
}

impl fmt::Debug for NewText {
    // What the signer typed is kept out of debug output and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewText")
            .field("page_index", &self.page_index)
            .field("rect", &self.rect)
            .field("rotation", &self.rotation)
            .field("characters", &self.contents.chars().count())
            .field("size", &self.size)
            .field("font", &self.font)
            .finish_non_exhaustive()
    }
}

/// A drawing to write ([`write_command`]): a signature drawn by hand.
#[derive(Clone, PartialEq)]
pub struct NewInk {
    /// From 0.
    pub page_index: usize,
    /// Each stroke's points, in PDF user space (the page unrotated, y upward).
    pub strokes: Vec<Vec<[f64; 2]>>,
    /// RGB, each from 0 to 1.
    pub color: [f64; 3],
    /// The strokes' width, in points.
    pub width: f64,
    /// As [`NewPicture::name`].
    pub name: String,
    /// As [`NewPicture::custom_data`].
    pub custom_data: Vec<(String, String)>,
}

impl fmt::Debug for NewInk {
    // The strokes are a signature: kept out of debug output and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewInk")
            .field("page_index", &self.page_index)
            .field("strokes", &self.strokes.len())
            .field("width", &self.width)
            .finish_non_exhaustive()
    }
}

/// An annotation to write, in the order of `<add>`.
#[derive(Debug, Clone, PartialEq)]
pub enum NewMark {
    Picture(NewPicture),
    Text(NewText),
    Ink(NewInk),
}

/// The XFDF a signing page sends for the annotations it added: the viewer's export in command
/// form (`<fields />`, `<add>`, `<modify />`, `<delete>`), exactly the subset [`read_command`]
/// reads and `apply_xfdf` draws. `deleted` lists the annotations it deletes, as (page index from
/// 0, name): its placeholders. The XFDF is read back before it is returned: what it writes is
/// what it reads.
///
/// # Errors
///
/// [`Refused`] for a mark that cannot be written: an empty or infinite rectangle, a rotation
/// other than 0, 90, 180 or 270, a picture that is not a PNG, a name empty, longer than 255
/// characters or holding a control character, a text size not positive, a colour component out
/// of 0 to 1, a font name of other characters, or a text or custom data holding a character XML
/// cannot carry.
pub fn write_command(marks: &[NewMark], deleted: &[(usize, String)]) -> Result<String, Refused> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?><xfdf xmlns=\"http://ns.adobe.com/xfdf/\" \
         xml:space=\"preserve\"><fields />\n<add>",
    );
    for (index, mark) in marks.iter().enumerate() {
        let position = index + 1;
        match mark {
            NewMark::Picture(picture) => write_picture(&mut out, picture, position)?,
            NewMark::Text(text) => write_text(&mut out, text, position)?,
            NewMark::Ink(ink) => write_ink(&mut out, ink, position)?,
        }
    }
    out.push_str("</add>\n<modify />\n");
    if deleted.is_empty() {
        out.push_str("<delete />");
    } else {
        out.push_str("<delete>");
        for (page_index, name) in deleted {
            checked_name(name, 0)?;
            out.push_str(&format!(
                "<id page=\"{page_index}\">{}</id>",
                escaped(name, false)?
            ));
        }
        out.push_str("</delete>");
    }
    out.push_str("</xfdf>");
    let read = read_command(&out)?;
    if !written_as_read(marks, deleted, &read) {
        return Err(refused("the XFDF written does not read back as given"));
    }
    Ok(out)
}

fn write_picture(out: &mut String, picture: &NewPicture, position: usize) -> Result<(), Refused> {
    if !picture.png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(refused(format!("annotation {position} is not a PNG")));
    }
    out.push_str(&format!(
        "<stamp {}><trn-custom-data bytes=\"{}\"/><imagedata>{PNG_DATA_URL}{}</imagedata></stamp>",
        common_attributes(
            picture.page_index,
            picture.rect,
            picture.rotation,
            &picture.name,
            position
        )?,
        custom_json(&picture.custom_data, position)?,
        base64::engine::general_purpose::STANDARD.encode(&picture.png),
    ));
    Ok(())
}

fn write_text(out: &mut String, text: &NewText, position: usize) -> Result<(), Refused> {
    if !(text.size.is_finite() && text.size > 0.0) {
        return Err(refused(format!(
            "annotation {position} has a size not positive"
        )));
    }
    if !text.color.iter().all(|c| (0.0..=1.0).contains(c)) {
        return Err(refused(format!(
            "annotation {position} has a colour out of 0 to 1"
        )));
    }
    let font_ok = !text.font.is_empty()
        && text.font.len() <= 64
        && text
            .font
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !font_ok {
        return Err(refused(format!(
            "annotation {position} has a font name refused"
        )));
    }
    let [r, g, b] = text.color;
    let hex = |c: f64| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = (c * 255.0).round() as u8;
        format!("{byte:02X}")
    };
    let color = format!("#{}{}{}", hex(r), hex(g), hex(b));
    let size = text.size;
    let align = match text.align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
    };
    let vertical = match text.vertical {
        VerticalAlign::Top => "top",
        VerticalAlign::Center => "center",
        VerticalAlign::Bottom => "bottom",
    };
    let font = &text.font;
    out.push_str(&format!(
        "<freetext {} width=\"0\" TextColor=\"{color}\" FontSize=\"{size}\">\
         <trn-custom-data bytes=\"{}\"/><contents>{}</contents>\
         <defaultappearance>{r} {g} {b} rg /{font} {size} Tf</defaultappearance>\
         <defaultstyle>font: {font} {size}pt; text-align: {align}; text-vertical-align: {vertical}; \
         color: {color}</defaultstyle></freetext>",
        common_attributes(text.page_index, text.rect, text.rotation, &text.name, position)?,
        custom_json(&text.custom_data, position)?,
        escaped(&text.contents, true)?,
    ));
    Ok(())
}

fn write_ink(out: &mut String, ink: &NewInk, position: usize) -> Result<(), Refused> {
    let points: Vec<[f64; 2]> = ink.strokes.iter().flatten().copied().collect();
    if points.is_empty() || !points.iter().flatten().all(|v| v.is_finite()) {
        return Err(refused(format!("annotation {position} draws nothing")));
    }
    if !(ink.width.is_finite() && ink.width >= 0.0) {
        return Err(refused(format!(
            "annotation {position} has a width refused"
        )));
    }
    if !ink.color.iter().all(|c| (0.0..=1.0).contains(c)) {
        return Err(refused(format!(
            "annotation {position} has a colour out of 0 to 1"
        )));
    }
    // The rectangle holds the strokes and their width.
    let half = (ink.width / 2.0).max(0.5);
    let fold = |pick: fn(&[f64; 2]) -> f64, f: fn(f64, f64) -> f64, start: f64| {
        points.iter().map(pick).fold(start, f)
    };
    let rect = Rect::from_corners(
        fold(|p| p[0], f64::min, f64::MAX) - half,
        fold(|p| p[1], f64::min, f64::MAX) - half,
        fold(|p| p[0], f64::max, f64::MIN) + half,
        fold(|p| p[1], f64::max, f64::MIN) + half,
    );
    let mut gestures = String::new();
    for stroke in ink.strokes.iter().filter(|s| !s.is_empty()) {
        let points: Vec<String> = stroke
            .iter()
            .map(|p| format!("{},{}", p[0], p[1]))
            .collect();
        gestures.push_str(&format!("<gesture>{}</gesture>", points.join(";")));
    }
    out.push_str(&format!(
        "<ink {} color=\"{}\" width=\"{}\"><trn-custom-data bytes=\"{}\"/><inklist>{gestures}</inklist></ink>",
        common_attributes(ink.page_index, rect, 0, &ink.name, position)?,
        hex(ink.color),
        ink.width,
        custom_json(&ink.custom_data, position)?,
    ));
    Ok(())
}

/// `#RRGGBB`, each component rounded to a byte.
fn hex(color: [f64; 3]) -> String {
    let byte = |c: f64| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = (c * 255.0).round() as u8;
        format!("{byte:02X}")
    };
    format!("#{}{}{}", byte(color[0]), byte(color[1]), byte(color[2]))
}

/// `page`, `rect`, `flags`, `name` and, when turned, `rotation`.
fn common_attributes(
    page_index: usize,
    rect: Rect,
    rotation: u16,
    name: &str,
    position: usize,
) -> Result<String, Refused> {
    let values = [rect.x1, rect.y1, rect.x2, rect.y2];
    if !values.iter().all(|v| v.is_finite()) || !(rect.x2 > rect.x1 && rect.y2 > rect.y1) {
        return Err(refused(format!(
            "annotation {position} has an empty rectangle"
        )));
    }
    if !matches!(rotation, 0 | 90 | 180 | 270) {
        return Err(refused(format!(
            "annotation {position} is turned by other than a quarter turn"
        )));
    }
    checked_name(name, position)?;
    let turned = if rotation == 0 {
        String::new()
    } else {
        format!(" rotation=\"{rotation}\"")
    };
    Ok(format!(
        "page=\"{page_index}\" rect=\"{},{},{},{}\" flags=\"print,readonly,locked\" name=\"{}\"{turned}",
        rect.x1,
        rect.y1,
        rect.x2,
        rect.y2,
        escaped(name, false)?
    ))
}

fn checked_name(name: &str, position: usize) -> Result<(), Refused> {
    if name.trim().is_empty() || name.chars().count() > 255 || name.chars().any(char::is_control) {
        return Err(refused(format!("annotation {position} has a name refused")));
    }
    Ok(())
}

fn custom_json(data: &[(String, String)], position: usize) -> Result<String, Refused> {
    let map: serde_json::Map<String, serde_json::Value> = data
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect();
    let json = serde_json::to_string(&map)
        .map_err(|_| refused(format!("annotation {position} has custom data refused")))?;
    escaped(&json, false)
}

/// `text` escaped for XML 1.0: in an attribute, or as text (a carriage return kept as a
/// reference, which XML would read as a line feed). A character XML 1.0 cannot hold is refused.
fn escaped(text: &str, content: bool) -> Result<String, Refused> {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if !content => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\n' if !content => out.push_str("&#10;"),
            '\t' if !content => out.push_str("&#9;"),
            '\n' | '\t' => out.push(c),
            c if u32::from(c) < 0x20 || matches!(c, '\u{FFFE}' | '\u{FFFF}') => {
                return Err(refused("a character XML cannot carry"));
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Whether `read`, the XFDF written read back, holds what was given.
fn written_as_read(marks: &[NewMark], deleted: &[(usize, String)], read: &Command) -> bool {
    let deletions = read.deleted.len() == deleted.len()
        && read
            .deleted
            .iter()
            .zip(deleted)
            .all(|((page, name), (index, given))| *page == index + 1 && name == given.trim());
    deletions
        && read.marks.len() == marks.len()
        && read.marks.iter().zip(marks).all(|pair| match pair {
            (Mark::Picture(read), NewMark::Picture(given)) => {
                read.page_number == given.page_index + 1
                    && read.rect == normalized(given.rect)
                    && read.rotation == given.rotation
                    && read.png == given.png
            }
            (Mark::Ink(read), NewMark::Ink(given)) => {
                let strokes: Vec<&Vec<[f64; 2]>> =
                    given.strokes.iter().filter(|s| !s.is_empty()).collect();
                read.page_number == given.page_index + 1
                    && read.strokes.len() == strokes.len()
                    && read.strokes.iter().zip(&strokes).all(|(a, b)| a == *b)
                    && read.width == given.width
                    && hex([read.color[0], read.color[1], read.color[2]]) == hex(given.color)
            }
            (Mark::Text(read), NewMark::Text(given)) => {
                read.page_number == given.page_index + 1
                    && read.rect == normalized(given.rect)
                    && read.rotation == given.rotation
                    && read.contents == given.contents
                    && read.size == given.size
                    && read.color == given.color
                    && read.align == given.align
                    && read.vertical == given.vertical
                    && read.border == 0.0
                    && read.font.as_deref() == Some(given.font.as_str())
            }
            _ => false,
        })
}

fn normalized(rect: Rect) -> Rect {
    Rect::from_corners(rect.x1, rect.y1, rect.x2, rect.y2)
}

// --- the annotations ------------------------------------------------------------------------

fn mark(annotation: &Element, position: usize) -> Result<Mark, Refused> {
    match annotation.name.as_str() {
        "stamp" => picture(annotation, position).map(Mark::Picture),
        "freetext" => text(annotation, position).map(Mark::Text),
        "ink" => ink(annotation, position).map(Mark::Ink),
        other => Err(refused(format!(
            "annotation {position} is a <{}>, only PNG stamps, free texts and drawings are drawn",
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

/// A drawing: `<inklist>` and its `<gesture>`s, `x,y;x,y;...` in PDF user space; its colour and
/// width. Its points are where they go: it is not turned.
fn ink(annotation: &Element, position: usize) -> Result<Ink, Refused> {
    if drawable(annotation, position)? != 0 {
        return Err(refused(format!("annotation {position} is rotated")));
    }
    if let Some(other) = annotation
        .children
        .iter()
        .find(|child| !matches!(child.name.as_str(), "inklist" | "trn-custom-data"))
    {
        return Err(refused(format!(
            "annotation {position} holds a <{}>",
            shown(&other.name)
        )));
    }
    if annotation
        .attribute("opacity")
        .is_some_and(|opacity| number(opacity) != Some(1.0))
    {
        return Err(refused(format!("annotation {position} is translucent")));
    }
    let lists: Vec<&Element> = annotation
        .children
        .iter()
        .filter(|child| child.name == "inklist")
        .collect();
    let [list] = lists.as_slice() else {
        return Err(refused(format!(
            "annotation {position} carries no single <inklist>"
        )));
    };
    let mut strokes = Vec::new();
    for gesture in &list.children {
        if gesture.name != "gesture" {
            return Err(refused(format!(
                "annotation {position} holds a <{}> in its <inklist>",
                shown(&gesture.name)
            )));
        }
        let mut stroke = Vec::new();
        for point in gesture
            .text
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let coordinates: Vec<Option<f64>> =
                point.split(',').map(|v| number(v.trim())).collect();
            let [Some(x), Some(y)] = coordinates.as_slice() else {
                return Err(refused(format!(
                    "annotation {position} has a point that is not two numbers"
                )));
            };
            stroke.push([*x, *y]);
            if stroke.len() > MAX_INK_POINTS {
                return Err(refused(format!(
                    "its drawings have more than {MAX_INK_POINTS} points"
                )));
            }
        }
        if !stroke.is_empty() {
            strokes.push(stroke);
        }
    }
    if strokes.is_empty() {
        return Err(refused(format!("annotation {position} draws nothing")));
    }
    Ok(Ink {
        page_number: page_number(annotation)?,
        rect: rect(annotation)?,
        strokes,
        color: annotation
            .attribute("color")
            .and_then(hex_color)
            .unwrap_or_else(|| vec![0.0, 0.0, 0.0]),
        width: annotation
            .attribute("width")
            .and_then(number)
            .filter(|w| *w >= 0.0)
            .unwrap_or(1.0),
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
        font: style
            .as_ref()
            .and_then(|s| s.font.clone())
            .or_else(|| appearance.as_ref().and_then(|a| a.font.clone())),
    })
}

/// What a `/DA` string says: the font size of `Tf`, the colour of the last `g`, `rg` or `k`.
#[derive(Debug, Default)]
pub(crate) struct DefaultAppearance {
    pub(crate) size: Option<f64>,
    pub(crate) color: Option<Vec<f64>>,
    /// The name `Tf` sets.
    pub(crate) font: Option<String>,
}

pub(crate) fn default_appearance(da: &str) -> DefaultAppearance {
    let mut found = DefaultAppearance::default();
    let mut operands: Vec<f64> = Vec::new();
    let mut last_name: Option<String> = None;
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
                found.font = last_name.take();
                0
            }
            _ => 0,
        };
        if wanted > 0 && operands.len() >= wanted {
            found.color = Some(operands[operands.len() - wanted..].to_vec());
        }
        // A name (the font) does not clear the operands of `Tf`.
        if let Some(name) = token.strip_prefix('/') {
            last_name = Some(name.to_owned()).filter(|n| !n.is_empty());
        } else {
            operands.clear();
        }
    }
    found
}

/// What the viewer's `defaultstyle` (CSS declarations) says.
struct DefaultStyle {
    /// The family of `font`.
    font: Option<String>,
    size: Option<f64>,
    color: Option<Vec<f64>>,
    align: Align,
    vertical: VerticalAlign,
}

fn default_style(css: &str) -> DefaultStyle {
    let mut style = DefaultStyle {
        font: None,
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
                // `font: Caveat 16pt`: the family is what comes before the size.
                let family: Vec<&str> = value
                    .split_whitespace()
                    .take_while(|part| part.strip_suffix("pt").and_then(number).is_none())
                    .collect();
                style.font = Some(family.join(" ").trim_matches(['"', '\'']).to_owned())
                    .filter(|f| !f.is_empty());
            }
            "font-family" => {
                style.font =
                    Some(value.trim_matches(['"', '\'']).to_owned()).filter(|f| !f.is_empty());
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

    fn new_picture(rotation: u16) -> NewPicture {
        NewPicture {
            page_index: 2,
            rect: Rect::from_corners(100.5, 600.0, 160.25, 620.0),
            rotation,
            png: PNG.to_vec(),
            name: "a1b2c3".to_owned(),
            custom_data: vec![
                ("fieldId".to_owned(), "f-1".to_owned()),
                ("signed".to_owned(), "true".to_owned()),
            ],
        }
    }

    fn new_text(contents: &str, rotation: u16) -> NewText {
        NewText {
            page_index: 0,
            rect: Rect::from_corners(10.0, 10.0, 130.0, 35.0),
            rotation,
            contents: contents.to_owned(),
            size: 14.5,
            color: [0.0, 0.2, 1.0],
            align: Align::Center,
            vertical: VerticalAlign::Center,
            font: "Helvetica".to_owned(),
            name: "t-9".to_owned(),
            custom_data: vec![("fieldId".to_owned(), "f-2".to_owned())],
        }
    }

    #[test]
    fn what_is_written_reads_back_as_given() {
        for rotation in [0, 90, 180, 270] {
            let marks = [
                NewMark::Picture(new_picture(rotation)),
                NewMark::Text(new_text("Łukasz <Wójcik> & \"co\"\r\n\tété", rotation)),
            ];
            let deleted = [(2, "placeholder-1".to_owned())];
            let xfdf = write_command(&marks, &deleted).unwrap();
            let command = read_command(&xfdf).unwrap();
            let [Mark::Picture(picture), Mark::Text(text)] = command.marks.as_slice() else {
                panic!("a picture then a text");
            };
            assert_eq!(picture.page_number, 3);
            assert_eq!(
                picture.rect,
                Rect::from_corners(100.5, 600.0, 160.25, 620.0)
            );
            assert_eq!((picture.rotation, text.rotation), (rotation, rotation));
            assert_eq!(picture.png, PNG);
            assert_eq!(picture.field_id.as_deref(), Some("f-1"));
            assert_eq!(text.contents, "Łukasz <Wójcik> & \"co\"\r\n\tété");
            assert_eq!((text.size, text.border), (14.5, 0.0));
            assert_eq!(text.color, [0.0, 0.2, 1.0]);
            assert_eq!(
                (text.align, text.vertical),
                (Align::Center, VerticalAlign::Center)
            );
            assert_eq!(command.deleted, [(3, "placeholder-1".to_owned())]);
            // What a server merging the fields' XFDF as text needs: one bare <add>, plain XML.
            assert_eq!(xfdf.matches("<add>").count(), 1);
            assert_eq!(xfdf.matches("</add>").count(), 1);
            assert_eq!(added_annotation_count(&xfdf).unwrap(), 2);
            assert!(!xfdf.contains("<!"));
            assert_eq!(xfdf.matches("<?").count(), 1);
            assert!(!format!("{:?}", marks[1]).contains("Wójcik"));
        }
        let empty = write_command(&[], &[]).unwrap();
        assert!(read_command(&empty).unwrap().marks.is_empty());
    }

    /// A drawn signature as the signing pages stored them: its attributes, custom data and strokes.
    const INK: &str = "<ink color=\"#000000\" creationdate=\"D:20250101120000Z\" date=\"D:20250101120000Z\" \
        flags=\"print,norotate,locked\" name=\"i-1\" page=\"1\" rect=\"100,200,180,240\" subject=\"Signature\" \
        title=\"Signer\"><trn-custom-data bytes=\"{&quot;trn-signature-type&quot;:&quot;ink&quot;}\"/>\
        <inklist><gesture>100.5,210;120,230.25;150,215</gesture><gesture> 170,220 ; 175,235 </gesture>\
        <gesture>160,205</gesture></inklist></ink>";

    #[test]
    fn a_drawing_is_read_with_its_strokes_colour_and_width() {
        let command = read_command(&exported(&[INK], "<delete />")).unwrap();
        let [Mark::Ink(ink)] = command.marks.as_slice() else {
            panic!("a drawing");
        };
        assert_eq!(ink.page_number, 2);
        assert_eq!(ink.rect, Rect::from_corners(100.0, 200.0, 180.0, 240.0));
        assert_eq!(
            ink.strokes,
            [
                vec![[100.5, 210.0], [120.0, 230.25], [150.0, 215.0]],
                vec![[170.0, 220.0], [175.0, 235.0]],
                vec![[160.0, 205.0]],
            ]
        );
        assert_eq!(
            (ink.color.as_slice(), ink.width),
            (&[0.0, 0.0, 0.0][..], 1.0)
        );
        assert!(!format!("{ink:?}").contains("100.5"));
        let wide = INK.replace("color=\"#000000\"", "color=\"#0000FF\" width=\"2.5\"");
        let command = read_command(&exported(&[&wide], "<delete />")).unwrap();
        let [Mark::Ink(ink)] = command.marks.as_slice() else {
            panic!("a drawing");
        };
        assert_eq!(
            (ink.color.as_slice(), ink.width),
            (&[0.0, 0.0, 1.0][..], 2.5)
        );
    }

    #[test]
    fn drawings_past_their_points_are_refused() {
        let points = vec!["1,1"; MAX_INK_POINTS / 2 + 1].join(";");
        let one = format!(
            "<ink page=\"0\" rect=\"0,0,9,9\"><inklist><gesture>{points}</gesture></inklist></ink>"
        );
        assert!(read_command(&exported(&[&one], "<delete />")).is_ok());
        assert!(read_command(&exported(&[&one, &one], "<delete />")).is_err());
    }

    #[test]
    fn a_drawing_written_reads_back_as_given() {
        let ink = NewInk {
            page_index: 1,
            strokes: vec![
                vec![[100.5, 210.0], [120.0, 230.25]],
                vec![],
                vec![[160.0, 205.0]],
            ],
            color: [0.0, 0.0, 1.0],
            width: 2.0,
            name: "i-1".to_owned(),
            custom_data: vec![("trn-signature-type".to_owned(), "ink".to_owned())],
        };
        let xfdf = write_command(&[NewMark::Ink(ink.clone())], &[]).unwrap();
        let command = read_command(&xfdf).unwrap();
        let [Mark::Ink(read)] = command.marks.as_slice() else {
            panic!("a drawing");
        };
        assert_eq!(
            read.strokes,
            [vec![[100.5, 210.0], [120.0, 230.25]], vec![[160.0, 205.0]]]
        );
        assert_eq!((read.width, read.page_number), (2.0, 2));
        // The rectangle holds the strokes and their width.
        assert_eq!(read.rect, Rect::from_corners(99.5, 204.0, 161.0, 231.25));
        let refused = |change: &dyn Fn(&mut NewInk)| {
            let mut ink = ink.clone();
            change(&mut ink);
            write_command(&[NewMark::Ink(ink)], &[]).is_err()
        };
        assert!(refused(&|i| i.strokes = vec![vec![]]));
        assert!(refused(&|i| i.strokes = vec![vec![[f64::NAN, 1.0]]]));
        assert!(refused(&|i| i.width = -1.0));
        assert!(refused(&|i| i.color = [0.0, 0.0, 2.0]));
    }

    #[test]
    fn what_cannot_be_written_is_refused() {
        let picture = |change: &dyn Fn(&mut NewPicture)| {
            let mut picture = new_picture(0);
            change(&mut picture);
            write_command(&[NewMark::Picture(picture)], &[])
        };
        let text = |change: &dyn Fn(&mut NewText)| {
            let mut text = new_text("Jean", 0);
            change(&mut text);
            write_command(&[NewMark::Text(text)], &[])
        };
        assert!(picture(&|p| p.rotation = 45).is_err());
        assert!(picture(&|p| p.rect = Rect::from_corners(1.0, 1.0, 1.0, 5.0)).is_err());
        assert!(picture(&|p| p.rect.x2 = f64::NAN).is_err());
        assert!(picture(&|p| p.png = b"GIF89a".to_vec()).is_err());
        assert!(picture(&|p| p.name = " ".to_owned()).is_err());
        assert!(picture(&|p| p.name = "a\nb".to_owned()).is_err());
        assert!(picture(&|p| p.name = "x".repeat(256)).is_err());
        assert!(picture(&|p| p.custom_data = vec![("k".to_owned(), "\u{1}".to_owned())]).is_ok());
        assert!(text(&|t| t.size = 0.0).is_err());
        assert!(text(&|t| t.color = [0.0, 1.5, 0.0]).is_err());
        assert!(text(&|t| t.font = "Hel vetica".to_owned()).is_err());
        assert!(text(&|t| t.font = "Caveat-Bold_2".to_owned()).is_ok());
        assert!(text(&|t| t.contents = "a\u{1}b".to_owned()).is_err());
        assert!(write_command(&[], &[(0, String::new())]).is_err());
    }

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
            exported(&["<square page=\"0\" rect=\"10,10,90,30\" />"], "<delete />"),
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\" rotation=\"90\"><inklist><gesture>10,10;20,20</gesture></inklist></ink>"], "<delete />"),
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\" opacity=\"0.5\"><inklist><gesture>10,10;20,20</gesture></inklist></ink>"], "<delete />"),
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\"><inklist><gesture>10,10;20,20</gesture></inklist><popup /></ink>"], "<delete />"),
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\"><inklist><gesture>10,10,3;20,20</gesture></inklist></ink>"], "<delete />"),
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\"><inklist><gesture>10,nan</gesture></inklist></ink>"], "<delete />"),
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\"><inklist /></ink>"], "<delete />"),
            exported(&["<ink page=\"0\" rect=\"10,10,90,30\" />"], "<delete />"),
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
