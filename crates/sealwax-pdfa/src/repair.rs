//! Repairs that change no pixel: entries PDF/A forbids or requires, set where they are.

use std::collections::{HashMap, HashSet};

use sealwax_core::{Dict, Document, Error, Name, ObjId, Object, Result};

use crate::Report;
use crate::color::flate_stream;
use crate::edit::{self, Location};
use crate::scan::Scan;

/// Action types PDF/A forbids (ISO 19005-3, 6.5.1).
const FORBIDDEN_ACTIONS: [&[u8]; 11] = [
    b"Launch",
    b"Sound",
    b"Movie",
    b"ResetForm",
    b"ImportData",
    b"Hide",
    b"SetOCGState",
    b"Rendition",
    b"Trans",
    b"GoTo3DView",
    b"JavaScript",
];
/// The only named actions PDF/A allows (6.5.1).
const ALLOWED_NAMED: [&[u8]; 4] = [b"NextPage", b"PrevPage", b"FirstPage", b"LastPage"];
/// Annotation flags (ISO 32000-1, 12.5.3).
const INVISIBLE: i64 = 1;
const HIDDEN: i64 = 2;
const PRINT: i64 = 4;
const NO_VIEW: i64 = 32;
const TOGGLE_NO_VIEW: i64 = 256;
/// How far the field tree is walked.
const MAX_FIELD_DEPTH: usize = 32;
const MAX_FIELDS: usize = 100_000;
/// How many actions a `/Next` chain may hold: far more than any document needs.
const MAX_ACTIONS: usize = 4096;

/// Whether an action, or one of the actions its `/Next` entries chain to, is one PDF/A forbids.
/// The chain is walked without recursion, each object once (a chain may loop back on itself), and
/// at most [`MAX_ACTIONS`] actions.
fn forbidden(doc: &Document, action: &Object) -> Result<bool> {
    let mut pending = vec![action.clone()];
    let mut seen = HashSet::new();
    let mut walked = 0;
    while let Some(item) = pending.pop() {
        if let Object::Ref(id) = item
            && !seen.insert(id)
        {
            continue;
        }
        walked += 1;
        if walked > MAX_ACTIONS {
            return Err(Error::LimitExceeded {
                what: "actions chained by /Next",
            });
        }
        let Some(Object::Dict(action)) = edit::resolved(doc, &item)? else {
            continue;
        };
        let kind = action
            .get(b"S")
            .and_then(Object::as_name)
            .unwrap_or_default();
        if FORBIDDEN_ACTIONS.contains(&kind) {
            return Ok(true);
        }
        if kind == b"Named"
            && !action
                .get(b"N")
                .and_then(Object::as_name)
                .is_some_and(|name| ALLOWED_NAMED.contains(&name))
        {
            return Ok(true);
        }
        match action.get(b"Next") {
            Some(Object::Array(next)) => pending.extend(next.iter().rev().cloned()),
            Some(next @ (Object::Dict(_) | Object::Ref(_))) => pending.push(next.clone()),
            _ => {}
        }
    }
    Ok(false)
}

/// What a widget's field says, through its parents.
struct Field {
    /// `/FT`.
    kind: Option<Vec<u8>>,
    /// A value shown: a non-empty `/V` other than `/Off`.
    valued: bool,
}

impl Field {
    fn of(doc: &Document, widget: &Dict) -> Result<Self> {
        let (mut kind, mut value) = (None, None);
        let mut current = Some(Object::Dict(widget.clone()));
        let mut depth = 0;
        while let Some(Object::Dict(dict)) = current {
            if kind.is_none() {
                kind = dict
                    .get(b"FT")
                    .and_then(Object::as_name)
                    .map(<[u8]>::to_vec);
            }
            if value.is_none()
                && let Some(v) = dict.get(b"V")
            {
                value = edit::resolved(doc, v)?;
            }
            depth += 1;
            current = match dict.get(b"Parent") {
                Some(parent) if depth <= MAX_FIELD_DEPTH => edit::resolved(doc, parent)?,
                _ => None,
            };
        }
        let valued = match &value {
            None | Some(Object::Null) => false,
            Some(Object::String(text)) => !text.is_empty(),
            Some(Object::Name(name)) => name.as_bytes() != b"Off",
            Some(Object::Array(items)) => !items.is_empty(),
            Some(_) => true,
        };
        Ok(Self { kind, valued })
    }
}

/// The width and height of an annotation's rectangle, `None` when it has none that is finite.
fn size(annotation: &Dict) -> Option<(f64, f64)> {
    let rect = annotation.get(b"Rect")?.as_array()?;
    let value = |i: usize| rect.get(i).and_then(Object::as_f64);
    let (w, h) = ((value(2)? - value(0)?).abs(), (value(3)? - value(1)?).abs());
    (w.is_finite() && h.is_finite()).then_some((w, h))
}

/// Whether a widget's `/MK` asks for something drawn: a background or border colour, or for a
/// button a caption or an icon (readers draw them for a widget without appearance).
fn characterised(doc: &Document, widget: &Dict, button: bool) -> Result<bool> {
    let Some(mk) = widget.get(b"MK") else {
        return Ok(false);
    };
    let Some(Object::Dict(mk)) = edit::resolved(doc, mk)? else {
        return Ok(false);
    };
    for key in [&b"BG"[..], b"BC"] {
        if let Some(colour) = mk.get(key)
            && edit::resolved(doc, colour)?
                .as_ref()
                .and_then(Object::as_array)
                .is_some_and(|c| !c.is_empty())
        {
            return Ok(true);
        }
    }
    Ok(button
        && [&b"CA"[..], b"RC", b"AC", b"I", b"RI", b"IX"]
            .iter()
            .any(|key| mk.contains_key(key)))
}

/// An appearance that draws nothing, `width` by `height`: a text field's says it is one
/// (ISO 32000-1, 12.7.3.3).
fn empty_appearance(doc: &mut Document, width: f64, height: f64, field: bool) -> Result<ObjId> {
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("XObject"));
    dict.insert("Subtype", Name::from("Form"));
    dict.insert(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    dict.insert("Resources", Dict::new());
    let content: &[u8] = if field { b"/Tx BMC\nEMC\n" } else { b"" };
    doc.add_object(Object::Stream(flate_stream(dict, content)))
}

/// A widget without appearance whose field shows nothing (no value, nothing `/MK` asks to draw)
/// gets one that draws nothing, as Acrobat writes (6.3.3). A button's is a
/// subdictionary with the state `Off`. Not a choice field: a list shows its options, and PDFium
/// draws a combo box's arrow. Returns whether it got one.
fn add_empty_appearance(
    doc: &mut Document,
    location: &Location,
    annotation: &Dict,
) -> Result<bool> {
    let Some((width, height)) = size(annotation) else {
        return Ok(false);
    };
    let field = Field::of(doc, annotation)?;
    let kind = field.kind.as_deref();
    let button = kind == Some(b"Btn");
    if field.valued || kind == Some(b"Ch") || characterised(doc, annotation, button)? {
        return Ok(false);
    }
    let stream = empty_appearance(doc, width, height, kind == Some(b"Tx"))?;
    edit::edit_dict(doc, location, false, |dict| {
        let mut ap = Dict::new();
        if button {
            let mut states = Dict::new();
            states.insert("Off", stream);
            ap.insert("N", states);
            dict.insert("AS", Name::from("Off"));
        } else {
            ap.insert("N", stream);
        }
        dict.insert("AP", ap);
    })
}

/// A button whose normal appearance is a stream: it becomes the one state of a subdictionary
/// (6.3.3), the state `/AS` names, else `On`. What is drawn does not change,
/// but for PDFium with a check box or a radio button: a stream is no appearance of theirs for it,
/// and it drew its own.
/// The widget gets its own appearance dictionary, since another may share the one it has.
fn wrap_button_appearance(
    doc: &mut Document,
    location: &Location,
    annotation: &Dict,
) -> Result<bool> {
    let Some(mut ap) = edit::read_dict(doc, &location.key(b"AP"))? else {
        return Ok(false);
    };
    let Some(stream @ Object::Ref(_)) = ap.get(b"N").cloned() else {
        return Ok(false);
    };
    if !matches!(edit::resolved(doc, &stream)?, Some(Object::Stream(_))) {
        return Ok(false);
    }
    let state = annotation
        .get(b"AS")
        .and_then(Object::as_name)
        .map_or_else(|| b"On".to_vec(), <[u8]>::to_vec);
    let mut states = Dict::new();
    states.insert(Name::new(state.clone()), stream);
    ap.insert("N", states);
    edit::edit_dict(doc, location, false, |dict| {
        dict.insert("AP", ap);
        dict.insert("AS", Name::new(state));
    })
}

/// Annotations: a print flag, the normal appearance only, no action PDF/A forbids. A widget
/// keeps no action at all (6.4.1).
fn annotations(doc: &mut Document, scan: &Scan, report: &mut Report) -> Result<()> {
    for location in &scan.annotations {
        let Some(annotation) = edit::read_dict(doc, location)? else {
            continue;
        };
        let subtype = annotation
            .get(b"Subtype")
            .and_then(Object::as_name)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let widget = subtype == b"Widget";
        let action_forbidden = match annotation.get(b"A") {
            Some(action) => widget || forbidden(doc, action)?,
            None => false,
        };
        let flags = annotation.get(b"F").and_then(Object::as_i64);
        let hidden =
            flags.is_some_and(|f| f & (INVISIBLE | HIDDEN | NO_VIEW | TOGGLE_NO_VIEW) != 0);
        if hidden {
            report.residual_add("annotation_hidden", 1);
        }
        let appearance = match annotation.get(b"AP") {
            Some(value) => edit::resolved(doc, value)?,
            None => None,
        };
        let extra_appearances = appearance
            .as_ref()
            .and_then(Object::as_dict)
            .is_some_and(|ap| ap.contains_key(b"D") || ap.contains_key(b"R"));
        let appearance_ref = annotation.get(b"AP").and_then(Object::as_ref_id);
        let mut changed = 0;
        edit::edit_dict(doc, location, false, |dict| {
            if subtype != b"Popup" && flags.is_none() {
                dict.insert("F", PRINT);
                changed += 1;
            }
            if action_forbidden {
                dict.remove(b"A");
                changed += 1;
            }
            if widget && dict.remove(b"AA").is_some() {
                changed += 1;
            }
            if extra_appearances
                && appearance_ref.is_none()
                && let Some(Object::Dict(ap)) = dict.get_mut(b"AP")
            {
                ap.remove(b"D");
                ap.remove(b"R");
                changed += 1;
            }
        })?;
        if extra_appearances && let Some(id) = appearance_ref {
            edit::edit_dict(doc, &Location::of(id), false, |ap| {
                ap.remove(b"D");
                ap.remove(b"R");
            })?;
            changed += 1;
        }
        report.change("annotation_repaired", usize::from(changed > 0));
        // An annotation whose rectangle is a point needs no appearance (6.3.3).
        let point = size(&annotation).is_some_and(|(w, h)| w == 0.0 && h == 0.0);
        let missing =
            appearance.is_none() && !matches!(subtype.as_slice(), b"Popup" | b"Link") && !point;
        if missing {
            if widget && add_empty_appearance(doc, location, &annotation)? {
                report.change("appearance_empty_added", 1);
            } else {
                report.residual_add("annotation_without_appearance", 1);
            }
        } else if widget
            && Field::of(doc, &annotation)?.kind.as_deref() == Some(b"Btn")
            && wrap_button_appearance(doc, location, &annotation)?
        {
            report.change("button_appearance_wrapped", 1);
        }
    }
    Ok(())
}

/// The interactive form: no `NeedAppearances` true, no XFA, no field action.
fn form(doc: &mut Document, catalog: &Location, report: &mut Report) -> Result<()> {
    let Some(root) = edit::read_dict(doc, catalog)? else {
        return Ok(());
    };
    if !root.contains_key(b"AcroForm") {
        return Ok(());
    }
    let acroform = catalog.key(b"AcroForm");
    let mut fields: Vec<Object> = Vec::new();
    edit::edit_dict(doc, &acroform, false, |form| {
        if form.get(b"NeedAppearances") == Some(&Object::Bool(true)) {
            form.remove(b"NeedAppearances");
            report.change("need_appearances_removed", 1);
        }
        if form.remove(b"XFA").is_some() {
            report.change("xfa_removed", 1);
        }
        if let Some(Object::Array(items)) = form.get(b"Fields") {
            fields.clone_from(items);
        }
    })?;
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(Object, usize)> = fields.into_iter().map(|f| (f, 0)).collect();
    while let Some((field, depth)) = stack.pop() {
        let Object::Ref(id) = field else {
            continue;
        };
        if depth > MAX_FIELD_DEPTH || seen.len() > MAX_FIELDS || !seen.insert(id) {
            continue;
        }
        let mut kids = Vec::new();
        edit::edit_dict(doc, &Location::of(id), false, |dict| {
            if dict.remove(b"AA").is_some() {
                report.change("field_action_removed", 1);
            }
            // A field merged with its widget lost its action with the annotations.
            if dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Widget")
                && dict.remove(b"A").is_some()
            {
                report.change("field_action_removed", 1);
            }
            if let Some(Object::Array(items)) = dict.get(b"Kids") {
                kids.clone_from(items);
            }
        })?;
        stack.extend(kids.into_iter().map(|kid| (kid, depth + 1)));
    }
    Ok(())
}

/// The catalog and the pages: no additional actions, no JavaScript, no forbidden opening action.
fn actions(doc: &mut Document, catalog: &Location, report: &mut Report) -> Result<()> {
    let Some(root) = edit::read_dict(doc, catalog)? else {
        return Ok(());
    };
    let open_forbidden = match root.get(b"OpenAction") {
        Some(action @ (Object::Dict(_) | Object::Ref(_))) => forbidden(doc, action)?,
        _ => false,
    };
    edit::edit_dict(doc, catalog, false, |dict| {
        if dict.remove(b"AA").is_some() {
            report.change("catalog_action_removed", 1);
        }
        if open_forbidden && dict.remove(b"OpenAction").is_some() {
            report.change("catalog_action_removed", 1);
        }
    })?;
    if root.contains_key(b"Names") {
        edit::edit_dict(doc, &catalog.key(b"Names"), false, |names| {
            if names.remove(b"JavaScript").is_some() {
                report.change("javascript_removed", 1);
            }
        })?;
    }
    for page in doc.page_ids().into_iter().flatten() {
        let has = edit::read_dict(doc, &Location::of(page))?.is_some_and(|d| d.contains_key(b"AA"));
        if has {
            edit::edit_dict(doc, &Location::of(page), false, |dict| {
                dict.remove(b"AA");
            })?;
            report.change("page_action_removed", 1);
        }
    }
    Ok(())
}

/// Optional content configurations need a name and no automatic state changes (6.9).
fn optional_content(doc: &mut Document, catalog: &Location, report: &mut Report) -> Result<()> {
    let Some(root) = edit::read_dict(doc, catalog)? else {
        return Ok(());
    };
    let Some(properties) = root.get(b"OCProperties") else {
        return Ok(());
    };
    let properties_location = match properties {
        Object::Ref(id) => Location::of(*id),
        _ => catalog.key(b"OCProperties"),
    };
    let Some(properties) = edit::read_dict(doc, &properties_location)? else {
        return Ok(());
    };
    let mut configs = vec![properties_location.key(b"D")];
    if let Some(value) = properties.get(b"Configs")
        && let Some(Object::Array(items)) = edit::resolved(doc, value)?
    {
        let base = match value {
            Object::Ref(id) => Location::of(*id),
            _ => properties_location.key(b"Configs"),
        };
        configs.extend((0..items.len()).map(|i| base.index(i)));
    }
    let mut names = std::collections::HashSet::new();
    for (index, config) in configs.iter().enumerate() {
        edit::edit_dict(doc, config, false, |dict| {
            let named = dict
                .get(b"Name")
                .and_then(Object::as_string)
                .is_some_and(|name| !name.is_empty() && names.insert(name.to_vec()));
            if !named {
                let mut name = if index == 0 {
                    b"Default".to_vec()
                } else {
                    format!("Configuration {index}").into_bytes()
                };
                while !names.insert(name.clone()) {
                    name.push(b'+');
                }
                dict.insert("Name", Object::String(name));
                report.change("optional_content_named", 1);
            }
            if dict.remove(b"AS").is_some() {
                report.change("optional_content_auto_state_removed", 1);
            }
        })?;
    }
    Ok(())
}

/// Font descriptors: `CharSet` and `CIDSet` are optional in PDF/A-3 but must be exact when
/// present (6.2.11.4.2): they go. A Type 2 CIDFont needs an explicit `CIDToGIDMap` (6.2.11.3.2):
/// `Identity`, which is what its absence means.
fn fonts(doc: &mut Document, scan: &Scan, report: &mut Report) -> Result<()> {
    for (location, used) in &scan.fonts {
        let Some(font) = edit::read_dict(doc, location)? else {
            continue;
        };
        let subtype = font.get(b"Subtype").and_then(Object::as_name);
        let mut descriptors: Vec<Location> = Vec::new();
        if subtype == Some(b"Type3") {
            continue;
        }
        if subtype == Some(b"Type0") {
            let Some(value) = font.get(b"DescendantFonts") else {
                continue;
            };
            let base = match value {
                Object::Ref(id) => Location::of(*id),
                _ => location.key(b"DescendantFonts"),
            };
            let descendant = base.index(0);
            let Some(cid_font) = edit::read_dict(doc, &descendant)? else {
                continue;
            };
            if cid_font.get(b"Subtype").and_then(Object::as_name) == Some(b"CIDFontType2")
                && !cid_font.contains_key(b"CIDToGIDMap")
            {
                edit::edit_dict(doc, &descendant, false, |dict| {
                    dict.insert("CIDToGIDMap", Name::from("Identity"));
                })?;
                report.change("cid_to_gid_map_added", 1);
            }
            descriptors.push(descendant.key(b"FontDescriptor"));
        } else {
            descriptors.push(location.key(b"FontDescriptor"));
        }
        for descriptor in descriptors {
            let Some(dict) = edit::read_dict(doc, &descriptor)? else {
                report.residual_add("font_not_embedded", usize::from(used.rendered));
                continue;
            };
            let embedded = [&b"FontFile"[..], b"FontFile2", b"FontFile3"]
                .iter()
                .any(|key| dict.contains_key(key));
            report.residual_add("font_not_embedded", usize::from(used.rendered && !embedded));
            if dict.contains_key(b"CIDSet") || dict.contains_key(b"CharSet") {
                edit::edit_dict(doc, &descriptor, false, |dict: &mut Dict| {
                    if dict.remove(b"CIDSet").is_some() {
                        report.change("cid_set_removed", 1);
                    }
                    if dict.remove(b"CharSet").is_some() {
                        report.change("char_set_removed", 1);
                    }
                })?;
            }
        }
    }
    Ok(())
}

/// Images must not ask for interpolation (6.2.8).
fn images(doc: &mut Document, scan: &Scan, report: &mut Report) -> Result<()> {
    for id in &scan.interpolated_images {
        if edit::edit_dict(doc, &Location::of(*id), false, |dict| {
            dict.insert("Interpolate", false);
        })? {
            report.change("interpolation_removed", 1);
        }
    }
    report.residual_add("inline_image_interpolated", scan.interpolated_inline_images);
    Ok(())
}

/// Give each page whose resources it inherits as a direct dictionary a copy of its own, so that
/// changing them changes what the page draws with.
pub(crate) fn materialize_resources(doc: &mut Document, pages: &[ObjId]) -> Result<()> {
    // What each node of the page tree hands down, read once: a node with many kids is a large
    // dictionary, read again for each of them otherwise. Only pages change here.
    let mut handed_down: HashMap<ObjId, Option<Object>> = HashMap::new();
    for page in pages {
        let mut found = None;
        let mut unknown = Vec::new();
        let mut current = edit::read_dict(doc, &Location::of(*page))?;
        for _ in 0..32 {
            let Some(dict) = current else {
                break;
            };
            if let Some(resources) = dict.get(b"Resources") {
                found = Some(resources.clone());
                break;
            }
            current = match dict.get(b"Parent") {
                Some(Object::Ref(parent)) => {
                    if let Some(known) = handed_down.get(parent) {
                        found.clone_from(known);
                        break;
                    }
                    unknown.push(*parent);
                    edit::read_dict(doc, &Location::of(*parent))?
                }
                _ => None,
            };
        }
        for node in unknown {
            handed_down.insert(node, found.clone());
        }
        if let Some(resources) = found {
            edit::edit_dict(doc, &Location::of(*page), false, |dict| {
                if !dict.contains_key(b"Resources") {
                    dict.insert("Resources", resources);
                }
            })?;
        }
    }
    Ok(())
}

/// The longest name PDF/A accepts (ISO 19005-3, 6.1.13).
const MAX_NAME_BYTES: usize = 127;
/// What a shortened name keeps of the long one.
const KEPT_NAME_BYTES: usize = 100;

/// A name of at most [`MAX_NAME_BYTES`] for a longer one: a prefix of it, cut between two
/// characters when it is UTF-8, and a hash of the whole (FNV-1a), so that two long names stay two.
fn shortened(name: &[u8]) -> Vec<u8> {
    let mut end = KEPT_NAME_BYTES.min(name.len());
    if let Ok(text) = std::str::from_utf8(name) {
        while !text.is_char_boundary(end) {
            end -= 1;
        }
    }
    let hash = name.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let mut out = name[..end].to_vec();
    out.extend_from_slice(format!("_{hash:016x}").as_bytes());
    out
}

/// Appearance state names longer than PDF/A accepts (6.1.13): a check box's export value can be
/// a sentence. Each is shortened wherever it names the state: the keys of the appearance
/// subdictionaries (`/N`, `/D`, `/R`), and `/AS`, `/V` and `/DV` (the field's value). Other long
/// names (a resource's, which content streams use) stay in the report.
fn long_state_names(doc: &mut Document, report: &mut Report) -> Result<()> {
    let mut renames: std::collections::BTreeMap<Vec<u8>, Vec<u8>> =
        std::collections::BTreeMap::new();
    let mut states: Vec<Location> = Vec::new();
    let reachable = doc.reachable_objects()?;
    for id in &reachable {
        let Some(Object::Dict(dict)) = doc.object_without_data(*id)? else {
            continue;
        };
        let Some(appearance) = dict.get(b"AP") else {
            continue;
        };
        let Some(Object::Dict(appearance)) = edit::resolved(doc, appearance)? else {
            continue;
        };
        for key in [&b"N"[..], b"D", b"R"] {
            let Some(value) = appearance.get(key) else {
                continue;
            };
            let Some(Object::Dict(subdictionary)) = edit::resolved(doc, value)? else {
                continue;
            };
            let long: Vec<Vec<u8>> = subdictionary
                .iter()
                .map(|(name, _)| name.as_bytes().to_vec())
                .filter(|name| name.len() > MAX_NAME_BYTES)
                .collect();
            if long.is_empty() {
                continue;
            }
            for name in long {
                let short = shortened(&name);
                renames.entry(name).or_insert(short);
            }
            states.push(Location::of(*id).key(b"AP").key(key));
        }
    }
    if renames.is_empty() {
        return Ok(());
    }
    for location in &states {
        edit::edit_dict(doc, location, false, |dict| {
            for (old, new) in &renames {
                if let Some(value) = dict.remove(old) {
                    dict.insert(Name::new(new.clone()), value);
                }
            }
        })?;
    }
    for id in reachable {
        let Some(Object::Dict(dict)) = doc.object_without_data(id)? else {
            continue;
        };
        let names_a_state = [&b"AS"[..], b"V", b"DV"].iter().any(|key| {
            dict.get(key)
                .and_then(Object::as_name)
                .is_some_and(|name| renames.contains_key(name))
        });
        if names_a_state {
            edit::edit_dict(doc, &Location::of(id), false, |dict| {
                for key in [&b"AS"[..], b"V", b"DV"] {
                    let new = dict
                        .get(key)
                        .and_then(Object::as_name)
                        .and_then(|name| renames.get(name))
                        .cloned();
                    if let Some(new) = new {
                        dict.insert(Name::new(key.to_vec()), Name::new(new));
                    }
                }
            })?;
        }
    }
    report.change("state_name_shortened", renames.len());
    Ok(())
}

/// Every repair of this module.
pub(crate) fn apply(doc: &mut Document, scan: &Scan, report: &mut Report) -> Result<()> {
    let catalog = Location::of(doc.catalog_id());
    annotations(doc, scan, report)?;
    long_state_names(doc, report)?;
    form(doc, &catalog, report)?;
    actions(doc, &catalog, report)?;
    optional_content(doc, &catalog, report)?;
    fonts(doc, scan, report)?;
    images(doc, scan, report)
}
