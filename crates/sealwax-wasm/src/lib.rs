//! sealwax for browsers and web views: WebAssembly bindings, built by `build.sh` into the npm
//! package `sealwax`. Bytes in, bytes out, as the Python package; errors are the package's own
//! classes (`PdfError` and those under it, from `js/errors.js`), and never quote a document's
//! content.
//!
//! A panic cannot unwind on this target: where the native library catches one (a page hayro
//! cannot interpret), the module traps instead, and its instance should be dropped (a worker
//! restarted).

use std::collections::{BTreeMap, HashMap};

use js_sys::{Array, Object, Reflect, Uint8Array};
use sealwax_annots::{
    FieldChoice, FieldKind, FieldValue, FillOptions, FillValue, Flattened, TemplateOptions,
    Written, fields, fill_fields_with, flatten, flatten_fields,
};
use sealwax_core::{
    Document as CoreDocument, Limits, OpenOptions, PageGeometry, PagePick, Rect, SaveOptions,
    XrefFormat,
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/js/errors.js")]
extern "C" {
    #[wasm_bindgen(js_name = makeError)]
    fn make_error(kind: &str, message: &str, limit: Option<String>) -> JsValue;
}

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** What processing a document may spend, and for how long; a bound left out keeps its default,
 * calibrated on real documents. `timeoutMs` counts from the call that opens the document, or from
 * the call of a function given bytes. `unbounded` lifts every bound but the timeout: for
 * documents the caller produced itself. */
export interface Limits {
  timeoutMs?: number;
  unbounded?: boolean;
  maxOperators?: number;
  maxShownBytes?: number;
  maxPageCost?: number;
  maxImagePixels?: number;
  maxMeshBytes?: number;
  maxCodeRange?: number;
  maxStreamBytes?: number;
  minDashPeriod?: number;
  maxAnnotations?: number;
  maxRenderPixels?: number;
}

export interface OpenOptions {
  password?: string;
  /** The page count the caller accepts, [least, most]: another is refused. */
  pageCount?: [number, number];
  limits?: Limits;
}

export interface SaveOptions {
  /** "stream" (cross-reference stream and object streams, the default) or "table". */
  xref?: "stream" | "table";
}

/** x1, y1, x2, y2 in the page's default user space (points, y up). */
export type Box = [number, number, number, number];

export interface PageGeometry {
  /** From 1. */
  pageNumber: number;
  /** Clipped to the media box. */
  cropBox: Box;
  mediaBox: Box;
  rotation: 0 | 90 | 180 | 270;
  userUnit: number;
}

export interface Field {
  /** The full name: the partial names from the top, joined by periods. */
  name: string;
  /** `/TU`. */
  label: string | null;
  kind: "text" | "checkbox" | "radio" | "push_button" | "combo_box" | "list_box" | "signature" | "other";
  /** From 1. */
  pageNumber: number;
  rect: Box;
  cropBox: Box;
  /** Text, several selections, or null. */
  value: string | string[] | null;
  /** A button's `/AS`, its bytes. */
  state: Uint8Array | null;
  required: boolean;
  readOnly: boolean;
  maxLength: number | null;
  /** A combo box that takes text typed beside its options. */
  editable: boolean;
  /** Text, or [export, display] pairs. */
  choices: (string | [string, string])[];
  /** The names of a button's appearances, their bytes as the document spells them. */
  buttonStates: Uint8Array[];
  buttonCaption: string | null;
}

export interface Written {
  value: string;
  /** The value the field held before; a button's state, "Off" when it had none. */
  previous: string;
}

export interface Filled {
  /** The occurrences written: widgets of a field named. */
  occurrences: number;
  /** Characters the appearance's font lacks, drawn as "?" (the value keeps them). */
  charactersReplaced: number;
  /**
   * The names not written, each field left as it was: no field of that name on the pages, a
   * signature or push button, a button given a state none of its widgets has, and with `strict`
   * a value the field does not allow.
   */
  refused: string[];
  /** With `keepFilled`, the fields left as they were because they held another value. */
  kept: string[];
  /** Each field written, by name. */
  written: Record<string, Written>;
}

export interface NewAnnotation {
  pageIndex: number;
  kind:
    | "ink"
    | "highlight"
    | "underline"
    | "strikeout"
    | "squiggly"
    | "square"
    | "circle"
    | "line"
    | "polygon"
    | "polyline"
    | "freetext";
  /** Ink: lists of [x, y] points. */
  strokes?: [number, number][][];
  /** Text markup: per line top-left, top-right, bottom-left, bottom-right, eight numbers. */
  quads?: number[][];
  /** A line's two ends, a polygon's or polyline's points. */
  points?: [number, number][];
  /** A square, circle or free text: [x1, y1, x2, y2]. */
  rect?: Box;
  text?: string;
  size?: number;
  /** [r, g, b] from 0 to 1. */
  color?: [number, number, number];
  fill?: [number, number, number];
  width?: number;
  opacity?: number;
  author?: string;
  contents?: string;
}

export interface RedactOptions {
  /** What becomes of an image under an area: its pixels there blanked (the default), the whole image taken out, or kept. */
  images?: "pixels" | "remove" | "keep";
  /** Which paths painted over an area go: touched even in part (the default), covered whole, or none. */
  paths?: "touched" | "covered" | "keep";
}

export interface Redacted {
  glyphs: number;
  operators: number;
  forms: number;
  stringsUnread: number;
  images: number;
  imagesRemoved: number;
  paths: number;
}

export interface AppliedRedactions {
  marks: number;
  pages: number;
  linksRemoved: number;
  redacted: Redacted;
}

export interface TemplateContent {
  pages: number;
  /** The fields, each name once. */
  fields: number;
  /** The annotations a person added over the pages. */
  annotations: number;
}

export interface Flattened {
  painted: number;
  drawn: number;
  linksKept: number;
  /** Annotations taken out, by subtype. */
  dropped: Record<string, number>;
  charactersReplaced: number;
}

export interface Metadata {
  version: string;
  encrypted: boolean;
  linearized: boolean;
  hasForms: boolean;
  hasSignatures: boolean;
  /** The part and conformance the XMP claims ("3A"). */
  pdfa: string | null;
  /** The document information's text entries, as written. */
  info: Record<string, string>;
}

/** A page rendered for a canvas: `new ImageData(pixels, width, height)`. */
export interface RenderedPage {
  width: number;
  height: number;
  /** RGBA, 4 bytes per pixel, row by row from the top, opaque over white. */
  pixels: Uint8ClampedArray;
export interface PhotoPages {
  added: number;
  /** The photos, from 0, that could not be read. */
  unreadable: number[];
}

export interface OutlineEntry {
  /** From 1. */
  level: number;
  title: string;
  /** From 1; null when the entry leads nowhere in the document. */
  pageNumber: number | null;
}
"#;

/// The JavaScript error of a core error.
fn core_error(error: &sealwax_core::Error) -> JsValue {
    use sealwax_core::Error;
    let message = error.to_string();
    match error {
        Error::PasswordRequired => make_error("password", &message, None),
        Error::Malformed
        | Error::PagesMissing { .. }
        | Error::PageCountOutOfRange { .. }
        | Error::Io(_) => make_error("read", &message, None),
        Error::LimitExceeded { what } => make_error("limit", &message, Some((*what).to_owned())),
        Error::Timeout => make_error("timeout", &message, Some("deadline".to_owned())),
        Error::InvalidArgument { .. } => make_error("argument", &message, None),
        Error::FieldNameTaken => make_error("name", &message, None),
        _ => make_error("pdf", &message, None),
    }
}

/// The JavaScript error of an annotation error.
fn annots_error(error: &sealwax_annots::Error) -> JsValue {
    use sealwax_annots::Error;
    match error {
        Error::Pdf(inner) => core_error(inner),
        Error::Source { error: inner, .. } => {
            let kind = match inner.as_ref() {
                Error::Pdf(sealwax_core::Error::PasswordRequired) => "password",
                Error::Pdf(
                    sealwax_core::Error::Malformed
                    | sealwax_core::Error::PagesMissing { .. }
                    | sealwax_core::Error::PageCountOutOfRange { .. }
                    | sealwax_core::Error::Io(_),
                ) => "read",
                Error::Pdf(sealwax_core::Error::LimitExceeded { .. }) => "limit",
                Error::Pdf(sealwax_core::Error::Timeout) => "timeout",
                Error::Pdf(_) => "pdf",
                _ => "xfdf",
            };
            let limit = match inner.as_ref() {
                Error::Pdf(sealwax_core::Error::LimitExceeded { what }) => Some((*what).to_owned()),
                Error::Pdf(sealwax_core::Error::Timeout) => Some("deadline".to_owned()),
                _ => None,
            };
            make_error(kind, &error.to_string(), limit)
        }
        _ => make_error("xfdf", &error.to_string(), None),
    }
}

/// The JavaScript error of a rendering error.
fn raster_error(error: &sealwax_raster::Error) -> JsValue {
    match error {
        sealwax_raster::Error::Pdf(inner) => core_error(inner),
        sealwax_raster::Error::Annots(inner) => annots_error(inner),
        other => make_error("pdf", &other.to_string(), None),
    }
}

/// The rendering options `{ quarterTurns }` describes, at `scale`.
fn render_options(
    scale: f64,
    options: Option<&JsValue>,
) -> Result<sealwax_raster::RenderOptions, JsValue> {
    let mut render = sealwax_raster::RenderOptions::new(scale);
    if let Some(turns) = options.and_then(|o| get(o, "quarterTurns")) {
        match turns.as_f64() {
            Some(n) if n.is_finite() && n.fract() == 0.0 && n.abs() < 1_000_000.0 => {
                #[allow(clippy::cast_possible_truncation)]
                {
                    render = render.quarter_turns(n as i32);
                }
            }
            _ => return Err(argument("quarterTurns must be a whole number")),
        }
    }
    Ok(render)
}

fn argument(message: &str) -> JsValue {
    make_error("argument", message, None)
}

/// `value[key]`, when it is set (not undefined nor null).
fn get(value: &JsValue, key: &str) -> Option<JsValue> {
    if !value.is_object() {
        return None;
    }
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

fn set(target: &Object, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(target, &JsValue::from_str(key), &value.into());
}

fn numbers(values: &[f64]) -> Array {
    values.iter().map(|v| JsValue::from_f64(*v)).collect()
}

fn rect(rect: Rect) -> Array {
    numbers(&[rect.x1, rect.y1, rect.x2, rect.y2])
}

/// A count as a JavaScript number (exact up to 2^53, past any count sealwax makes).
#[allow(clippy::cast_precision_loss)]
fn count(value: usize) -> JsValue {
    JsValue::from_f64(value as f64)
}

/// A whole number of `value[key]`, at least 0.
fn bound(value: &JsValue, key: &str) -> Result<Option<u64>, JsValue> {
    let Some(number) = get(value, key) else {
        return Ok(None);
    };
    match number.as_f64() {
        Some(n)
            if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0 =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(Some(n as u64))
        }
        _ => Err(argument(&format!(
            "limits.{key} must be a whole number >= 0"
        ))),
    }
}

/// The limits a `Limits` object describes, their deadline counted from now.
fn limits(value: Option<JsValue>) -> Result<Limits, JsValue> {
    let Some(value) = value.filter(JsValue::is_object) else {
        return Ok(Limits::default());
    };
    let mut limits = if get(&value, "unbounded").is_some_and(|v| v.is_truthy()) {
        Limits::unbounded()
    } else {
        Limits::default()
    };
    let fields: [(&str, &mut u64); 9] = [
        ("maxOperators", &mut limits.max_operators),
        ("maxShownBytes", &mut limits.max_shown_bytes),
        ("maxPageCost", &mut limits.max_page_cost),
        ("maxImagePixels", &mut limits.max_image_pixels),
        ("maxMeshBytes", &mut limits.max_mesh_bytes),
        ("maxCodeRange", &mut limits.max_code_range),
        ("maxStreamBytes", &mut limits.max_stream_bytes),
        ("maxAnnotations", &mut limits.max_annotations),
        ("maxRenderPixels", &mut limits.max_render_pixels),
    ];
    for (key, field) in fields {
        if let Some(number) = bound(&value, key)? {
            *field = number;
        }
    }
    if let Some(period) = get(&value, "minDashPeriod") {
        match period.as_f64() {
            Some(p) if p.is_finite() && p >= 0.0 => limits.min_dash_period = p,
            _ => {
                return Err(argument(
                    "limits.minDashPeriod must be a finite number >= 0",
                ));
            }
        }
    }
    if let Some(timeout) = get(&value, "timeoutMs") {
        match timeout.as_f64() {
            Some(ms) if ms.is_finite() && ms >= 0.0 => {
                limits = limits.timeout(std::time::Duration::from_secs_f64(ms / 1000.0));
            }
            _ => return Err(argument("limits.timeoutMs must be a finite number >= 0")),
        }
    }
    Ok(limits)
}

/// The options an `OpenOptions` object describes.
fn open_options(value: Option<JsValue>) -> Result<OpenOptions, JsValue> {
    let mut options = OpenOptions::default();
    let Some(value) = value.filter(JsValue::is_object) else {
        return Ok(options);
    };
    if let Some(password) = get(&value, "password") {
        options = options.password(
            password
                .as_string()
                .ok_or_else(|| argument("password must be a string"))?,
        );
    }
    if let Some(range) = get(&value, "pageCount") {
        let bounds: Vec<f64> = Array::from(&range)
            .iter()
            .filter_map(|v| v.as_f64())
            .collect();
        match bounds[..] {
            [least, most]
                if least.fract() == 0.0 && most.fract() == 0.0 && 0.0 <= least && least <= most =>
            {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    options = options.page_count(least as usize..=most as usize);
                }
            }
            _ => return Err(argument("pageCount must be [least, most], whole numbers")),
        }
    }
    Ok(options.limits(limits(get(&value, "limits"))?))
}

fn bytes(data: &[u8]) -> Uint8Array {
    Uint8Array::from(data)
}

/// `doc` saved in full, then rewritten smaller, read again with `limits`.
fn compact(doc: &CoreDocument, limits: &Limits) -> Result<Uint8Array, JsValue> {
    let saved = doc
        .save(&SaveOptions::default())
        .map_err(|error| core_error(&error))?;
    let options = OpenOptions::default()
        .limits(*limits)
        .max_file_bytes(usize::MAX);
    sealwax_core::optimize(&saved, &options)
        .map(|optimized| bytes(&optimized.data))
        .map_err(|error| core_error(&error))
}

/// The PDFs `sources` (each a `Uint8Array`) opened with `limits`; an error names the source
/// (from 1).
fn open_sources(sources: &Array, limits: Limits) -> Result<Vec<CoreDocument>, JsValue> {
    let options = OpenOptions::default().limits(limits);
    let mut documents = Vec::with_capacity(sources.length() as usize);
    for (index, source) in sources.iter().enumerate() {
        if !source.is_instance_of::<Uint8Array>() {
            return Err(argument("each source must be a Uint8Array"));
        }
        let document =
            CoreDocument::open(Uint8Array::from(source).to_vec(), &options).map_err(|error| {
                annots_error(&sealwax_annots::Error::Source {
                    position: index + 1,
                    error: Box::new(error.into()),
                })
            })?;
        documents.push(document);
    }
    Ok(documents)
}

/// The pages of an `arrange` list: each a page index or a `[page, quarter turns]` pair.
fn pages(list: &Array) -> Result<Vec<(usize, i32)>, JsValue> {
    let whole = |value: JsValue| -> Option<f64> {
        value.as_f64().filter(|n| n.is_finite() && n.fract() == 0.0)
    };
    let mut pages = Vec::with_capacity(list.length() as usize);
    for item in list.iter() {
        let (page, turns) = if Array::is_array(&item) {
            let pair = Array::from(&item);
            (whole(pair.get(0)), whole(pair.get(1)))
        } else {
            (whole(item), Some(0.0))
        };
        match (page, turns) {
            (Some(page), Some(turns))
                if (0.0..4_294_967_296.0).contains(&page) && turns.abs() < 1_000_000.0 =>
            {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                pages.push((page as usize, turns as i32));
            }
            _ => {
                return Err(argument(
                    "each page is an index or an [index, quarter turns] pair",
                ));
            }
        }
    }
    Ok(pages)
}

fn flattened(report: &Flattened) -> Object {
    let object = Object::new();
    set(&object, "painted", count(report.painted));
    set(&object, "drawn", count(report.drawn));
    set(&object, "linksKept", count(report.links_kept));
    set(&object, "dropped", counts(&report.dropped));
    set(
        &object,
        "charactersReplaced",
        count(report.characters_replaced),
    );
    object
}

fn counts(map: &BTreeMap<String, usize>) -> Object {
    let object = Object::new();
    for (key, value) in map {
        set(&object, key, count(*value));
    }
    object
}

fn geometry(page: &PageGeometry) -> Object {
    let object = Object::new();
    set(&object, "pageNumber", count(page.page_number));
    set(&object, "cropBox", rect(page.crop_box));
    set(&object, "mediaBox", rect(page.media_box));
    set(&object, "rotation", page.rotation.degrees());
    set(&object, "userUnit", page.user_unit);
    object
}

#[allow(clippy::cast_precision_loss)]
fn field(field: &sealwax_annots::Field) -> Object {
    let object = Object::new();
    set(&object, "name", field.name.as_str());
    set(
        &object,
        "label",
        field.label.clone().map_or(JsValue::NULL, JsValue::from),
    );
    let kind = match field.kind {
        FieldKind::Text => "text",
        FieldKind::CheckBox => "checkbox",
        FieldKind::Radio => "radio",
        FieldKind::PushButton => "push_button",
        FieldKind::ComboBox => "combo_box",
        FieldKind::ListBox => "list_box",
        FieldKind::Signature => "signature",
        _ => "other",
    };
    set(&object, "kind", kind);
    set(&object, "pageNumber", count(field.page_index + 1));
    set(&object, "rect", rect(field.rect));
    set(&object, "cropBox", rect(field.crop_box));
    let value = match &field.value {
        Some(FieldValue::Text(text)) => JsValue::from(text.as_str()),
        Some(FieldValue::Name(name)) => JsValue::from(sealwax_annots::name_text(name)),
        Some(FieldValue::List(list)) => list
            .iter()
            .map(|item| JsValue::from(item.as_str()))
            .collect::<Array>()
            .into(),
        None => JsValue::NULL,
    };
    set(&object, "value", value);
    set(
        &object,
        "state",
        field
            .state
            .as_deref()
            .map_or(JsValue::NULL, |state| bytes(state).into()),
    );
    set(&object, "required", field.required);
    set(&object, "readOnly", field.read_only);
    set(&object, "editable", field.editable);
    set(
        &object,
        "maxLength",
        field
            .max_length
            .map_or(JsValue::NULL, |length| JsValue::from_f64(length as f64)),
    );
    let choices: Array = field
        .choices
        .iter()
        .map(|choice| match choice {
            FieldChoice::Single(text) => JsValue::from(text.as_str()),
            FieldChoice::Pair { export, display } => {
                Array::of2(&export.as_str().into(), &display.as_str().into()).into()
            }
        })
        .collect();
    set(&object, "choices", choices);
    let states: Array = field
        .button_states
        .iter()
        .map(|state| JsValue::from(bytes(state)))
        .collect();
    set(&object, "buttonStates", states);
    set(
        &object,
        "buttonCaption",
        field
            .button_caption
            .clone()
            .map_or(JsValue::NULL, JsValue::from),
    );
    object
}

fn save_options(options: Option<JsValue>) -> Result<SaveOptions, JsValue> {
    let format = match options.as_ref().and_then(|o| get(o, "xref")) {
        None => XrefFormat::Stream,
        Some(xref) => match xref.as_string().as_deref() {
            Some("stream") => XrefFormat::Stream,
            Some("table") => XrefFormat::Table,
            _ => return Err(argument("xref must be \"stream\" or \"table\"")),
        },
    };
    Ok(SaveOptions::default().xref(format))
}

/// An opened PDF document.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Document {
    inner: CoreDocument,
}

#[wasm_bindgen]
impl Document {
    /// Open the PDF in `data` (copied).
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(
        data: &[u8],
        #[wasm_bindgen(unchecked_param_type = "OpenOptions")] options: Option<JsValue>,
    ) -> Result<Document, JsValue> {
        let options = open_options(options)?;
        CoreDocument::open(data.to_vec(), &options)
            .map(|inner| Self { inner })
            .map_err(|error| core_error(&error))
    }

    /// The number of pages.
    #[wasm_bindgen(js_name = pageCount)]
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    /// Whether the document changed since it was opened.
    #[wasm_bindgen(js_name = isModified)]
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.inner.is_modified()
    }

    /// The geometry of every page, in page order.
    #[wasm_bindgen(unchecked_return_type = "PageGeometry[]")]
    #[must_use]
    pub fn geometry(&self) -> Array {
        self.inner
            .geometry()
            .iter()
            .map(|page| JsValue::from(geometry(page)))
            .collect()
    }

    /// The document saved in full: every reachable object rewritten, decrypted and renumbered.
    ///
    /// # Errors
    /// A `PdfError` when it cannot be saved.
    pub fn save(
        &self,
        #[wasm_bindgen(unchecked_param_type = "SaveOptions")] options: Option<JsValue>,
    ) -> Result<Uint8Array, JsValue> {
        let options = save_options(options)?;
        self.inner
            .save(&options)
            .map(|data| bytes(&data))
            .map_err(|error| core_error(&error))
    }

    /// The document with what changed appended as an incremental update: the input's bytes
    /// first, unchanged (signatures stay valid).
    ///
    /// # Errors
    /// A `PdfError` when it cannot be saved.
    #[wasm_bindgen(js_name = saveIncremental)]
    pub fn save_incremental(&self) -> Result<Uint8Array, JsValue> {
        self.inner
            .save_incremental()
            .map(|data| bytes(&data))
            .map_err(|error| core_error(&error))
    }

    /// The occurrences of the form's fields on page `pageIndex` (from 0), every page's when
    /// left out: one per widget, a name repeated on several widgets never merged.
    ///
    /// # Errors
    /// A `PdfError`, or a `RangeError` for a page the document does not have.
    #[wasm_bindgen(unchecked_return_type = "Field[]")]
    pub fn fields(&self, page_index: Option<usize>) -> Result<Array, JsValue> {
        fields(&self.inner, page_index)
            .map(|found| found.iter().map(|f| JsValue::from(field(f))).collect())
            .map_err(|error| core_error(&error))
    }

    /// Fill the fields `values` names, by full name, every occurrence of each: a text or choice
    /// field gets the text and an appearance drawn for it (an empty text clears it). A button
    /// gets the state named, given as bytes exactly as `buttonStates` spells it or as text; or
    /// the state of its widget whose export value (`/Opt`) it is; `false`, an empty value, "off",
    /// "no", "0" and the like turn it `Off`; `true`, "yes", "on", "1", "x" and the like check a
    /// button that has one state besides `Off` (a check box). With `keepFilled`, a field holding another
    /// value is left as it is (`kept`); with `strict`, what a reader would not let a person type
    /// is refused (a read-only field, a text longer than its maximum length or with characters its
    /// font cannot draw, a choice the field does not offer).
    ///
    /// # Errors
    /// A `PdfError`, or a `RangeError` for a value that is neither text nor bytes.
    #[wasm_bindgen(js_name = fillFields, unchecked_return_type = "Filled")]
    pub fn fill_fields(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "Record<string, string | boolean | Uint8Array>")]
        values: JsValue,
        #[wasm_bindgen(unchecked_param_type = "{ keepFilled?: boolean; strict?: boolean }")]
        options: Option<JsValue>,
    ) -> Result<Object, JsValue> {
        let mut wanted = HashMap::new();
        for entry in Object::entries(&Object::from(values)).iter() {
            let pair = Array::from(&entry);
            let name = pair
                .get(0)
                .as_string()
                .ok_or_else(|| argument("a field's name must be a string"))?;
            let value = pair.get(1);
            let value = if let Some(text) = value.as_string() {
                FillValue::Text(text)
            } else if let Some(flag) = value.as_bool() {
                // A check box checked or not; a text field gets the word.
                FillValue::Text(if flag { "true" } else { "false" }.into())
            } else if value.is_instance_of::<Uint8Array>() {
                FillValue::Bytes(Uint8Array::from(value).to_vec())
            } else {
                return Err(argument(
                    "each value must be a string, a boolean or a Uint8Array",
                ));
            };
            wanted.insert(name, value);
        }
        let flag = |key: &str| {
            options
                .as_ref()
                .and_then(|o| get(o, key))
                .is_some_and(|v| v.is_truthy())
        };
        let options = FillOptions::default()
            .keep_filled(flag("keepFilled"))
            .strict(flag("strict"));
        let filled = fill_fields_with(&mut self.inner, &wanted, &options)
            .map_err(|error| core_error(&error))?;
        let object = Object::new();
        set(&object, "occurrences", count(filled.occurrences));
        set(
            &object,
            "charactersReplaced",
            count(filled.characters_replaced),
        );
        let names = |names: &[String]| -> Array {
            names
                .iter()
                .map(|name| JsValue::from(name.as_str()))
                .collect()
        };
        set(&object, "refused", names(&filled.refused));
        set(&object, "kept", names(&filled.kept));
        let written = Object::new();
        for (name, field) in &filled.written {
            let entry = Object::new();
            set(&entry, "value", field.value.as_str());
            set(&entry, "previous", field.previous.as_str());
            set(&written, name, entry);
        }
        set(&object, "written", written);
        Ok(object)
    }

    /// Undo a filling: each field `written` names (as `fillFields` returned it) gets back the
    /// value it held before, unless it was changed since or refuses it. Returns those, left as
    /// they are; a name the form no longer has is passed over.
    ///
    /// # Errors
    /// A `PdfError`, or a `RangeError` for an entry without its `value` and `previous`.
    #[wasm_bindgen(js_name = unfillFields, unchecked_return_type = "string[]")]
    pub fn unfill_fields(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "Record<string, Written>")] written: JsValue,
    ) -> Result<Array, JsValue> {
        let mut fields = std::collections::BTreeMap::new();
        for entry in Object::entries(&Object::from(written)).iter() {
            let pair = Array::from(&entry);
            let name = pair
                .get(0)
                .as_string()
                .ok_or_else(|| argument("a field's name must be a string"))?;
            let field = pair.get(1);
            let text = |key: &str| get(&field, key).and_then(|v| v.as_string());
            let (Some(value), Some(previous)) = (text("value"), text("previous")) else {
                return Err(argument("each field written needs its value and previous"));
            };
            fields.insert(name, Written::new(&value, &previous));
        }
        let kept = sealwax_annots::unfill_fields(&mut self.inner, &fields)
            .map_err(|error| core_error(&error))?;
        Ok(kept
            .iter()
            .map(|name| JsValue::from(name.as_str()))
            .collect())
    }

    /// Make the document a template: every field cleared with its empty appearance drawn, the
    /// XFA form taken out, the annotations a person added over the pages taken out (markup and
    /// their pop-ups; links and widgets stay) unless `keepAnnotations`, the information's title
    /// set to `title`, its author, subject and keywords and the XMP metadata taken out. What was
    /// replaced is left out of a full `save()`. What the pages' content draws stays.
    ///
    /// # Errors
    /// A `PdfError`, or a `RangeError` without a title.
    #[wasm_bindgen(
        js_name = clearTemplate,
        unchecked_return_type = "{ fields: number; annotationsRemoved: number }"
    )]
    pub fn clear_template(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "{ title: string; keepAnnotations?: boolean }")]
        options: JsValue,
    ) -> Result<Object, JsValue> {
        let title = get(&options, "title")
            .and_then(|v| v.as_string())
            .ok_or_else(|| argument("title must be a string"))?;
        let keep = get(&options, "keepAnnotations").is_some_and(|v| v.is_truthy());
        let options = TemplateOptions::new(&title).keep_annotations(keep);
        let cleared = sealwax_annots::clear_template(&mut self.inner, &options)
            .map_err(|error| annots_error(&error))?;
        let object = Object::new();
        set(&object, "fields", count(cleared.fields));
        set(
            &object,
            "annotationsRemoved",
            count(cleared.annotations_removed),
        );
        Ok(object)
    }

    /// What `clearTemplate` would clear or take out.
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(js_name = templateContent, unchecked_return_type = "TemplateContent")]
    pub fn template_content(&self) -> Result<Object, JsValue> {
        let content =
            sealwax_annots::template_content(&self.inner).map_err(|error| annots_error(&error))?;
        let object = Object::new();
        set(&object, "pages", count(content.pages));
        set(&object, "fields", count(content.fields));
        set(&object, "annotations", count(content.annotations));
        Ok(object)
    }

    /// Add an annotation, its appearance drawn, in the page's default user space (unrotated,
    /// y up): `kind` "ink" (`strokes`, lists of [x, y] points), "highlight", "underline",
    /// "strikeout" or "squiggly" (`quads`: per line the corners top-left, top-right, bottom-left,
    /// bottom-right, eight numbers), "square" or "circle" (`rect`), "line" (`points`, its two
    /// ends), "polygon" or "polyline" (`points`), "freetext" (`rect`, `text`, `size`, 12 by
    /// default). `color` ([r, g, b] from 0 to 1) draws the strokes, markup, border and text,
    /// black by default and yellow for a highlight; `fill` the inside of a square, circle or
    /// polygon, or a free text's background; `width` the strokes and borders (1 by default, 0:
    /// no border); `opacity` from 0 to 1.
    ///
    /// # Errors
    /// A `RangeError` for a missing or bad argument.
    #[wasm_bindgen(js_name = addAnnotation)]
    pub fn add_annotation(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "NewAnnotation")] annotation: JsValue,
    ) -> Result<(), JsValue> {
        use sealwax_annots::AnnotationKind as Kind;
        let numbers_of = |value: &JsValue| -> Option<Vec<f64>> {
            if !Array::is_array(value) {
                return None;
            }
            Array::from(value).iter().map(|v| v.as_f64()).collect()
        };
        let point_list = |value: &JsValue| -> Option<Vec<[f64; 2]>> {
            if !Array::is_array(value) {
                return None;
            }
            Array::from(value)
                .iter()
                .map(|point| match numbers_of(&point)?.as_slice() {
                    [x, y] => Some([*x, *y]),
                    _ => None,
                })
                .collect()
        };
        let field = |key: &str| get(&annotation, key).filter(|v| !v.is_undefined() && !v.is_null());
        let kind_name = field("kind")
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        let missing = |what: &str| argument(&format!("a {kind_name} needs {what}"));
        let quads = || -> Result<Vec<[f64; 8]>, JsValue> {
            let value = field("quads").ok_or_else(|| missing("quads"))?;
            if !Array::is_array(&value) {
                return Err(missing("quads"));
            }
            Array::from(&value)
                .iter()
                .map(|quad| {
                    numbers_of(&quad)
                        .and_then(|n| <[f64; 8]>::try_from(n).ok())
                        .ok_or_else(|| argument("each quad must be eight numbers"))
                })
                .collect()
        };
        let rect = || -> Result<[f64; 4], JsValue> {
            field("rect")
                .and_then(|r| numbers_of(&r))
                .and_then(|n| <[f64; 4]>::try_from(n).ok())
                .ok_or_else(|| missing("rect: [x1, y1, x2, y2]"))
        };
        let points = || -> Result<Vec<[f64; 2]>, JsValue> {
            field("points")
                .and_then(|p| point_list(&p))
                .ok_or_else(|| missing("points: [x, y][]"))
        };
        let rgb = |key: &str| -> Result<Option<[f64; 3]>, JsValue> {
            match field(key) {
                None => Ok(None),
                Some(value) => numbers_of(&value)
                    .and_then(|n| <[f64; 3]>::try_from(n).ok())
                    .map(Some)
                    .ok_or_else(|| argument("a colour must be [r, g, b]")),
            }
        };
        let kind = match kind_name.as_str() {
            "ink" => {
                let value = field("strokes").ok_or_else(|| missing("strokes"))?;
                if !Array::is_array(&value) {
                    return Err(missing("strokes"));
                }
                let strokes: Option<Vec<Vec<[f64; 2]>>> =
                    Array::from(&value).iter().map(|s| point_list(&s)).collect();
                Kind::Ink {
                    strokes: strokes.ok_or_else(|| missing("strokes: [x, y][][]"))?,
                }
            }
            "highlight" => Kind::Highlight { quads: quads()? },
            "underline" => Kind::Underline { quads: quads()? },
            "strikeout" => Kind::StrikeOut { quads: quads()? },
            "squiggly" => Kind::Squiggly { quads: quads()? },
            "square" => Kind::Square { rect: rect()? },
            "circle" => Kind::Circle { rect: rect()? },
            "line" => match points()?.as_slice() {
                [from, to] => Kind::Line {
                    from: *from,
                    to: *to,
                },
                _ => return Err(argument("a line needs two points")),
            },
            "polygon" => Kind::Polygon { points: points()? },
            "polyline" => Kind::PolyLine { points: points()? },
            "freetext" => Kind::FreeText {
                rect: rect()?,
                text: field("text")
                    .and_then(|v| v.as_string())
                    .ok_or_else(|| missing("text"))?,
                size: field("size").and_then(|v| v.as_f64()).unwrap_or(12.0),
            },
            _ => return Err(argument("unknown annotation kind")),
        };
        let page = field("pageIndex")
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
            .ok_or_else(|| argument("pageIndex must be a whole number"))?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut new = sealwax_annots::NewAnnotation::new(page as usize, kind)
            .fill(rgb("fill")?)
            .width(field("width").and_then(|v| v.as_f64()).unwrap_or(1.0))
            .opacity(field("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0))
            .author(field("author").and_then(|v| v.as_string()))
            .contents(field("contents").and_then(|v| v.as_string()));
        if let Some(color) = rgb("color")? {
            new = new.color(color);
        }
        sealwax_annots::add_annotation(&mut self.inner, &new)
            .map(|_| ())
            .map_err(|error| annots_error(&error))
    }

    /// Create a field: `kind` "text", "date" (a text field carrying Acrobat's `yyyy-mm-dd` date
    /// actions) or "checkbox" (states `Yes` and `Off`), named `name` (not empty, no period), on
    /// page `pageIndex` (from 0) at `rect` ([x1, y1, x2, y2]) on the page as it shows: turned by
    /// its rotation, from the top-left corner of its crop box, y down, in points. It gets a gray
    /// border one point wide and no background, and turns with the page so that its text reads
    /// upright. The form is created if the document has none.
    ///
    /// # Errors
    /// A `FieldNameTakenError` when the form already has a field of that name; a `RangeError`
    /// for a bad kind, name, page or rectangle.
    #[wasm_bindgen(js_name = addField)]
    pub fn add_field(
        &mut self,
        #[wasm_bindgen(
            unchecked_param_type = "{ name: string; kind: \"text\" | \"date\" | \"checkbox\"; pageIndex: number; rect: Box; required?: boolean }"
        )]
        field: JsValue,
    ) -> Result<(), JsValue> {
        let name = get(&field, "name")
            .and_then(|v| v.as_string())
            .ok_or_else(|| argument("name must be a string"))?;
        let kind = match get(&field, "kind").and_then(|v| v.as_string()).as_deref() {
            Some("text") => sealwax_annots::NewFieldKind::Text,
            Some("date") => sealwax_annots::NewFieldKind::Date,
            Some("checkbox") => sealwax_annots::NewFieldKind::CheckBox,
            _ => return Err(argument("kind must be \"text\", \"date\" or \"checkbox\"")),
        };
        let page = get(&field, "pageIndex")
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
            .ok_or_else(|| argument("pageIndex must be a whole number"))?;
        let rect: Vec<f64> = get(&field, "rect")
            .map(|r| Array::from(&r).iter().filter_map(|v| v.as_f64()).collect())
            .unwrap_or_default();
        let [x1, y1, x2, y2] = rect[..] else {
            return Err(argument("rect must be [x1, y1, x2, y2]"));
        };
        let required = get(&field, "required").is_some_and(|v| v.is_truthy());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let new = sealwax_annots::NewField::new(&name, kind, page as usize, [x1, y1, x2, y2])
            .required(required);
        sealwax_annots::add_field(&mut self.inner, &new)
            .map(|_| ())
            .map_err(|error| annots_error(&error))
    }

    /// Add a page for each photo of `photos` (JPEG or PNG), in order, after the document's
    /// pages: a US Letter sheet, portrait or landscape as the photo shows, half an inch of
    /// margin, the photo centered and fitted without being stretched; a JPEG embedded as it is,
    /// the orientation of its EXIF data made the page's rotation. Returns `added` and
    /// `unreadable` (the photos, from 0, that could not be read).
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(js_name = addPhotoPages, unchecked_return_type = "PhotoPages")]
    pub fn add_photo_pages(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "Uint8Array[]")] photos: Array,
    ) -> Result<Object, JsValue> {
        let photos = photo_bytes(&photos)?;
        let borrowed: Vec<&[u8]> = photos.iter().map(Vec::as_slice).collect();
        let report = sealwax_annots::add_photo_pages(&mut self.inner, &borrowed)
            .map_err(|error| annots_error(&error))?;
        Ok(photo_report(report.added, &report.unreadable))
    }

    /// Every occurrence's value, by full name: a text or choice field's value, a button's state
    /// (`Off` when it has none), each as text.
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(js_name = fieldValues, unchecked_return_type = "Record<string, string[]>")]
    pub fn field_values(&self) -> Result<Object, JsValue> {
        let values =
            sealwax_annots::field_values(&self.inner).map_err(|error| core_error(&error))?;
        let object = Object::new();
        for (name, occurrences) in values {
            let list: Array = occurrences
                .iter()
                .map(|value| JsValue::from(value.as_str()))
                .collect();
            set(&object, &name, list);
        }
        Ok(object)
    }

    /// Flatten every annotation but links into its page and take the form out; with
    /// `fieldsOnly`, the form's widgets alone, every other annotation kept as it is.
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(unchecked_return_type = "Flattened")]
    pub fn flatten(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "{ fieldsOnly?: boolean }")] options: Option<JsValue>,
    ) -> Result<Object, JsValue> {
        let fields_only = options
            .as_ref()
            .and_then(|o| get(o, "fieldsOnly"))
            .is_some_and(|v| v.is_truthy());
        let report = if fields_only {
            flatten_fields(&mut self.inner)
        } else {
            flatten(&mut self.inner)
        }
        .map_err(|error| annots_error(&error))?;
        Ok(flattened(&report))
    }

    /// Page `pageIndex` (from 0) rendered as a reader shows it (annotations and field values
    /// included), as PNG bytes: 8-bit RGB over white, the crop box as shown (rotated, then turned
    /// `quarterTurns` more quarter turns clockwise), at `scale` pixels per point, sides rounded
    /// up. A modified document is saved and read again once, until it changes again.
    ///
    /// # Errors
    /// A `PdfError` (a `PdfLimitError` past `maxRenderPixels`), or a `RangeError`.
    #[wasm_bindgen(js_name = renderPng)]
    pub fn render_png(
        &self,
        page_index: usize,
        scale: f64,
        #[wasm_bindgen(unchecked_param_type = "{ quarterTurns?: number }")] options: Option<
            JsValue,
        >,
    ) -> Result<Uint8Array, JsValue> {
        let options = render_options(scale, options.as_ref())?;
        match sealwax_raster::render(&self.inner, page_index, &options, &|| false)
            .map_err(|error| raster_error(&error))?
        {
            Some(raster) => raster
                .png()
                .map(|png| bytes(&png))
                .map_err(|error| raster_error(&error)),
            None => Err(make_error("pdf", "rendering cancelled", None)),
        }
    }

    /// Page `pageIndex` rendered as `renderPng` renders it, as pixels a canvas takes:
    /// `{ width, height, pixels }`, `pixels` a `Uint8ClampedArray` of 4 bytes per pixel (RGBA,
    /// opaque over white) for `new ImageData(pixels, width, height)`. `cancel`, an `Int32Array`
    /// over a `SharedArrayBuffer`, stops the rendering between its steps once its first element
    /// is not 0 (set from another thread with `Atomics.store`): the result is then `null`, and
    /// nothing is drawn.
    ///
    /// # Errors
    /// A `PdfError` (a `PdfLimitError` past `maxRenderPixels`), or a `RangeError`.
    #[wasm_bindgen(js_name = renderRgba, unchecked_return_type = "RenderedPage | null")]
    pub fn render_rgba(
        &self,
        page_index: usize,
        scale: f64,
        #[wasm_bindgen(unchecked_param_type = "{ quarterTurns?: number; cancel?: Int32Array }")]
        options: Option<JsValue>,
    ) -> Result<JsValue, JsValue> {
        let render = render_options(scale, options.as_ref())?;
        let token = options
            .as_ref()
            .and_then(|o| get(o, "cancel"))
            .and_then(|token| token.dyn_into::<js_sys::Int32Array>().ok());
        let cancelled = || {
            token
                .as_ref()
                .is_some_and(|token| js_sys::Atomics::load(token, 0).is_ok_and(|value| value != 0))
        };
        let Some(raster) = sealwax_raster::render(&self.inner, page_index, &render, &cancelled)
            .map_err(|error| raster_error(&error))?
        else {
            return Ok(JsValue::NULL);
        };
        let object = Object::new();
        set(&object, "width", raster.width);
        set(&object, "height", raster.height);
        set(
            &object,
            "pixels",
            js_sys::Uint8ClampedArray::from(raster.pixels.as_slice()),
        );
        Ok(object.into())
    }

    /// What the document says about itself, never failing.
    #[wasm_bindgen(unchecked_return_type = "Metadata")]
    #[must_use]
    pub fn metadata(&self) -> Object {
        let metadata = self.inner.metadata();
        let object = Object::new();
        set(&object, "version", metadata.version.as_str());
        set(&object, "encrypted", metadata.encrypted);
        set(&object, "linearized", metadata.linearized);
        set(&object, "hasForms", metadata.has_forms);
        set(&object, "hasSignatures", metadata.has_signatures);
        set(
            &object,
            "pdfa",
            metadata.pdfa.map_or(JsValue::NULL, JsValue::from),
        );
        let info = Object::new();
        for (key, value) in &metadata.info {
            set(&info, key, value.as_str());
        }
        set(&object, "info", info);
        object
    }

    /// The outline, flat and depth first; named destinations resolved, never a guess.
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(unchecked_return_type = "OutlineEntry[]")]
    pub fn outline(&self) -> Result<Array, JsValue> {
        let entries = self.inner.outline().map_err(|error| core_error(&error))?;
        Ok(entries
            .into_iter()
            .map(|entry| {
                let object = Object::new();
                set(&object, "level", count(entry.level));
                set(&object, "title", entry.title);
                set(
                    &object,
                    "pageNumber",
                    entry
                        .page_index
                        .map_or(JsValue::NULL, |index| count(index + 1)),
                );
                JsValue::from(object)
            })
            .collect())
    }

    /// Pages `[start, stop)` (from 0; at least one) in a new PDF, saved compressed: each page with
    /// its contents, resources and annotations; the outline, the form and the structure tree
    /// left behind.
    ///
    /// # Errors
    /// A `PdfError`, or a `RangeError` for a range the document does not have.
    #[wasm_bindgen(js_name = extractPages)]
    pub fn extract_pages(&self, start: usize, stop: usize) -> Result<Uint8Array, JsValue> {
        let extracted = self
            .inner
            .extract_pages(start..stop)
            .map_err(|error| core_error(&error))?;
        let saved = extracted
            .save(&SaveOptions::default())
            .map_err(|error| core_error(&error))?;
        let options = OpenOptions::default()
            .limits(*self.inner.limits())
            .max_file_bytes(usize::MAX);
        sealwax_core::optimize(&saved, &options)
            .map(|optimized| bytes(&optimized.data))
            .map_err(|error| core_error(&error))
    }

    /// A new PDF of this document's pages `pages`, in that order, saved compressed: each entry a
    /// page index (from 0) or a `[page, quarter turns]` pair, the quarter turns (clockwise;
    /// counterclockwise when negative) added to the page's rotation. Turning, deleting, moving,
    /// reordering and taking out pages are each one arrangement. The form comes along for the
    /// pages kept, each field with its widgets on them; nothing of the pages left out is copied.
    ///
    /// # Errors
    /// A `RangeError` for an empty list, a page the document does not have, or a page twice; a
    /// `PdfError`.
    pub fn arrange(
        &self,
        #[wasm_bindgen(unchecked_param_type = "(number | [number, number])[]")] pages_list: Array,
    ) -> Result<Uint8Array, JsValue> {
        let wanted = pages(&pages_list)?;
        let arranged = self
            .inner
            .arrange(&wanted)
            .map_err(|error| core_error(&error))?;
        compact(&arranged, self.inner.limits())
    }

    /// A new PDF, saved compressed, of this document's pages with every page of the PDFs
    /// `sources` inserted before page `at` (from 0; the page count to append), one after the
    /// other, opened with this document's limits. The forms come along; a field an inserted
    /// document names as an earlier one does is renamed `name (2)`, `name (3)`...
    ///
    /// # Errors
    /// A `RangeError` when `at` is past the last page; the error of a source that cannot be read,
    /// its message naming it.
    #[wasm_bindgen(js_name = insertPages)]
    pub fn insert_pages(
        &self,
        at: usize,
        #[wasm_bindgen(unchecked_param_type = "Uint8Array[]")] sources: Array,
    ) -> Result<Uint8Array, JsValue> {
        let others = open_sources(&sources, *self.inner.limits())?;
        let borrowed: Vec<&CoreDocument> = others.iter().collect();
        let inserted = self
            .inner
            .insert(at, &borrowed)
            .map_err(|error| core_error(&error))?;
        compact(&inserted, self.inner.limits())
    }

    /// Write onto each page the media box, crop box and rotation it inherits from its page tree
    /// and does not hold itself, for an engine that does not look for them there (PDFium's
    /// flattening writes US Letter where a page holds no box). Returns the pages changed.
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(js_name = ownPageBoxes)]
    pub fn own_page_boxes(&mut self) -> Result<usize, JsValue> {
        self.inner
            .own_page_boxes()
            .map_err(|error| core_error(&error))
    }

    /// After another engine flattened the form: take out the widgets it left because they are
    /// hidden (`Hidden` or `NoView`), then the form itself, which still holds the values.
    /// Returns the widgets taken out.
    ///
    /// # Errors
    /// A `PdfError` when a page still holds a visible widget (the flattening did not happen):
    /// nothing is changed then.
    #[wasm_bindgen(js_name = clearFlattenedForm)]
    pub fn clear_flattened_form(&mut self) -> Result<usize, JsValue> {
        self.inner
            .clear_flattened_form()
            .map_err(|error| core_error(&error))
    }

    /// Take out of page `pageIndex` (from 0) what it draws under `areas` (boxes in its default
    /// user space, unrotated, y up): the glyphs (a glyph an area overlaps goes whole, nothing
    /// after it moves), the images' pixels (`images`: "pixels", "remove" or "keep"), the paths
    /// painted there (`paths`: "touched", "covered" or "keep"). Annotations stay: flatten first.
    /// Follow with `rebuiltForRedaction()`: what the pages no longer reach stays in this
    /// document.
    ///
    /// # Errors
    /// A `PdfError`, or a `RangeError` for a bad area or option.
    #[wasm_bindgen(js_name = redact, unchecked_return_type = "Redacted")]
    pub fn redact(
        &mut self,
        page_index: usize,
        #[wasm_bindgen(unchecked_param_type = "Box[]")] areas: Array,
        #[wasm_bindgen(unchecked_param_type = "RedactOptions")] options: Option<JsValue>,
    ) -> Result<Object, JsValue> {
        let mut rects = Vec::new();
        for area in areas.iter() {
            let numbers: Option<Vec<f64>> = Array::is_array(&area)
                .then(|| Array::from(&area).iter().map(|v| v.as_f64()).collect())
                .flatten();
            let [x1, y1, x2, y2] = numbers
                .and_then(|n| <[f64; 4]>::try_from(n).ok())
                .ok_or_else(|| argument("each area must be [x1, y1, x2, y2]"))?;
            rects.push(sealwax_core::Rect::from_corners(x1, y1, x2, y2));
        }
        let options = redact_options(options.as_ref())?;
        let done = sealwax_text::redact(&mut self.inner, page_index, &rects, &options)
            .map_err(|error| core_error(&error))?;
        Ok(redacted_object(&done))
    }

    /// Apply every redaction mark (`/Redact`): the other annotations flattened first (links
    /// stay), what the pages draw under the marks taken out as `redact` does, each mark's area
    /// painted with its interior colour and its overlay text written, the marks and the links
    /// under them taken out. Follow with `rebuiltForRedaction()`.
    ///
    /// # Errors
    /// A `PdfError`, or a `RangeError` for a bad option.
    #[wasm_bindgen(js_name = applyRedactions, unchecked_return_type = "AppliedRedactions")]
    pub fn apply_redactions(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "RedactOptions")] options: Option<JsValue>,
    ) -> Result<Object, JsValue> {
        let options = redact_options(options.as_ref())?;
        let applied = sealwax_annots::apply_redactions(&mut self.inner, &options)
            .map_err(|error| annots_error(&error))?;
        let object = Object::new();
        set(&object, "marks", count(applied.marks));
        set(&object, "pages", count(applied.pages));
        set(&object, "linksRemoved", count(applied.links_removed));
        set(&object, "redacted", redacted_object(&applied.redacted));
        Ok(object)
    }

    /// Whether a page holds a redaction mark (a `/Redact` annotation).
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(js_name = hasRedactionMarks)]
    pub fn has_redaction_marks(&self) -> Result<bool, JsValue> {
        self.inner
            .has_redaction_marks()
            .map_err(|error| core_error(&error))
    }

    /// A new PDF, saved compressed, of this document's pages alone, for a copy whose redactions
    /// another engine applied: each page keeps only `/Type`, its boxes, `/Rotate`, `/UserUnit`,
    /// `/Resources`, `/Contents`, `/Annots` and `/Group`; the widgets and hidden annotations are
    /// taken out; the optional content is kept, so that a layer turned off stays off; the form,
    /// the document information, the XMP metadata, the outline and the embedded files stay
    /// behind.
    ///
    /// # Errors
    /// A `PdfError`.
    #[wasm_bindgen(js_name = rebuiltForRedaction)]
    pub fn rebuilt_for_redaction(&self) -> Result<Uint8Array, JsValue> {
        let rebuilt = self
            .inner
            .rebuilt_for_redaction()
            .map_err(|error| core_error(&error))?;
        compact(&rebuilt, self.inner.limits())
    }
}

/// The documents `sources` one after the other, each flattened first; with `flatten: false`,
/// their forms kept instead, a field an earlier document already names renamed `name (2)`,
/// `name (3)`...
///
/// # Errors
/// The error of the first source that cannot be read or merged; its message says which.
#[wasm_bindgen(js_name = mergeBytes)]
pub fn merge_bytes(
    #[wasm_bindgen(unchecked_param_type = "Uint8Array[]")] sources: Array,
    #[wasm_bindgen(unchecked_param_type = "{ flatten?: boolean; limits?: Limits }")]
    options: Option<JsValue>,
) -> Result<Uint8Array, JsValue> {
    let flatten = options
        .as_ref()
        .and_then(|o| get(o, "flatten"))
        .is_none_or(|v| v.is_truthy());
    let limits = limits(options.as_ref().and_then(|o| get(o, "limits")))?;
    let documents = open_sources(&sources, limits)?;
    let merged = if flatten {
        sealwax_annots::merge(documents)
            .map(|(merged, _)| merged)
            .map_err(|error| annots_error(&error))?
    } else {
        let borrowed: Vec<&CoreDocument> = documents.iter().collect();
        let picks: Vec<PagePick> = documents
            .iter()
            .enumerate()
            .flat_map(|(index, doc)| {
                (0..doc.page_count()).map(move |page| PagePick::new(index, page, 0))
            })
            .collect();
        CoreDocument::compose(&borrowed, &picks, true).map_err(|error| core_error(&error))?
    };
    merged
        .save(&SaveOptions::default())
        .map(|data| bytes(&data))
        .map_err(|error| core_error(&error))
}

/// Whether the PDF in `data` holds a redaction mark (a `/Redact` annotation). A PDF that cannot
/// be read counts as marked: a copy of it must not skip what redacting asks for.
#[wasm_bindgen(js_name = hasRedactionMarks)]
#[must_use]
pub fn has_redaction_marks(
    data: &[u8],
    #[wasm_bindgen(unchecked_param_type = "{ limits?: Limits }")] options: Option<JsValue>,
) -> bool {
    let Ok(limits) = limits(options.as_ref().and_then(|o| get(o, "limits"))) else {
        return true;
    };
    CoreDocument::open(data.to_vec(), &OpenOptions::default().limits(limits))
        .and_then(|doc| doc.has_redaction_marks())
        .unwrap_or(true)
}

/// A new PDF of a page for each photo of `photos` (JPEG or PNG), as `addPhotoPages` adds
/// them: `{ pdf, unreadable }`.
///
/// # Errors
/// A `RangeError` when no photo can be read.
#[wasm_bindgen(js_name = photosToPdf, unchecked_return_type = "{ pdf: Uint8Array; unreadable: number[] }")]
pub fn photos_to_pdf(
    #[wasm_bindgen(unchecked_param_type = "Uint8Array[]")] photos: Array,
    #[wasm_bindgen(unchecked_param_type = "{ limits?: Limits }")] options: Option<JsValue>,
) -> Result<Object, JsValue> {
    let limits = limits(options.as_ref().and_then(|o| get(o, "limits")))?;
    let photos = photo_bytes(&photos)?;
    let borrowed: Vec<&[u8]> = photos.iter().map(Vec::as_slice).collect();
    let (doc, report) =
        sealwax_annots::photos_document(&borrowed, limits).map_err(|error| annots_error(&error))?;
    let saved = doc
        .save(&SaveOptions::default())
        .map_err(|error| core_error(&error))?;
    let object = photo_report(report.added, &report.unreadable);
    set(&object, "pdf", bytes(&saved));
    Ok(object)
}

fn photo_bytes(photos: &Array) -> Result<Vec<Vec<u8>>, JsValue> {
    photos
        .iter()
        .map(|photo| {
            if photo.is_instance_of::<Uint8Array>() {
                Ok(Uint8Array::from(photo).to_vec())
            } else {
                Err(argument("each photo must be a Uint8Array"))
            }
        })
        .collect()
}

fn photo_report(added: usize, unreadable: &[usize]) -> Object {
    let object = Object::new();
    set(&object, "added", count(added));
    let list: Array = unreadable.iter().map(|index| count(*index)).collect();
    set(&object, "unreadable", list);
    object
}

fn redact_options(options: Option<&JsValue>) -> Result<sealwax_text::RedactOptions, JsValue> {
    let text = |key: &str| {
        options
            .and_then(|o| get(o, key))
            .and_then(|v| v.as_string())
    };
    let images = match text("images").as_deref() {
        None | Some("pixels") => sealwax_text::ImageRedaction::Pixels,
        Some("remove") => sealwax_text::ImageRedaction::Remove,
        Some("keep") => sealwax_text::ImageRedaction::Keep,
        Some(_) => {
            return Err(argument(
                "images must be \"pixels\", \"remove\" or \"keep\"",
            ));
        }
    };
    let paths = match text("paths").as_deref() {
        None | Some("touched") => sealwax_text::PathRedaction::Touched,
        Some("covered") => sealwax_text::PathRedaction::Covered,
        Some("keep") => sealwax_text::PathRedaction::Keep,
        Some(_) => {
            return Err(argument(
                "paths must be \"touched\", \"covered\" or \"keep\"",
            ));
        }
    };
    Ok(sealwax_text::RedactOptions::default()
        .images(images)
        .paths(paths))
}

fn redacted_object(done: &sealwax_text::Redacted) -> Object {
    let object = Object::new();
    set(&object, "glyphs", count(done.glyphs));
    set(&object, "operators", count(done.operators));
    set(&object, "forms", count(done.forms));
    set(&object, "stringsUnread", count(done.strings_unread));
    set(&object, "images", count(done.images));
    set(&object, "imagesRemoved", count(done.images_removed));
    set(&object, "paths", count(done.paths));
    object
}

/// The PDF in `data` with every annotation but links flattened into its page.
///
/// # Errors
/// A `PdfError`.
#[wasm_bindgen(js_name = flattenBytes)]
pub fn flatten_bytes(
    data: &[u8],
    #[wasm_bindgen(unchecked_param_type = "{ limits?: Limits }")] options: Option<JsValue>,
) -> Result<Uint8Array, JsValue> {
    let options =
        OpenOptions::default().limits(limits(options.as_ref().and_then(|o| get(o, "limits")))?);
    let mut document =
        CoreDocument::open(data.to_vec(), &options).map_err(|error| core_error(&error))?;
    flatten(&mut document).map_err(|error| annots_error(&error))?;
    document
        .save(&SaveOptions::default())
        .map(|saved| bytes(&saved))
        .map_err(|error| core_error(&error))
}

/// The PDF in `data` rewritten smaller, losslessly, or as it was when it is encrypted, signed, or
/// not made smaller.
///
/// # Errors
/// A `PdfError`.
#[wasm_bindgen(js_name = optimizeBytes)]
pub fn optimize_bytes(
    data: &[u8],
    #[wasm_bindgen(unchecked_param_type = "{ limits?: Limits }")] options: Option<JsValue>,
) -> Result<Uint8Array, JsValue> {
    let options =
        OpenOptions::default().limits(limits(options.as_ref().and_then(|o| get(o, "limits")))?);
    sealwax_core::optimize(data, &options)
        .map(|optimized| bytes(&optimized.data))
        .map_err(|error| core_error(&error))
}

/// The version of sealwax.
#[wasm_bindgen]
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
