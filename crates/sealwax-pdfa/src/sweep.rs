//! Repairs on every object a save writes, wherever it sits: what PDF/A forbids in any
//! dictionary, whatever refers to it.
//!
//! - Object-level XMP (pages, forms, images): PDF/A checks every packet against its schemas,
//!   and editors leave properties of their own there (6.6.2.3.1). Only the catalog's packet,
//!   which the conversion writes, is kept.
//! - Overprint mode 1 is allowed only for DeviceCMYK under a CMYK output intent (6.2.4.2); under
//!   an RGB intent, CMYK content draws through an ICC-based default space.
//! - The process colour space of DeviceN attributes (ISO 32000-1, table 72) counts as a device
//!   space (6.2.4.3): the matching ICC profile replaces it.

use sealwax_core::{Document, Name, ObjId, Object, Result};

use crate::Report;
use crate::color::Profiles;

/// How deep the walk goes inside one object.
const MAX_NESTING: usize = 64;
/// The longest name PDF/A accepts (ISO 19005-3, 6.1.13).
const MAX_NAME_BYTES: usize = 127;

/// What one object needs.
#[derive(Debug, Default)]
struct Needs {
    metadata: usize,
    overprint_mode: usize,
    srgb: usize,
    cmyk: usize,
    long_names: usize,
}

impl Needs {
    fn any_change(&self) -> bool {
        self.metadata + self.overprint_mode + self.srgb + self.cmyk > 0
    }
}

/// Read-only: what `object` needs, and its names too long to keep.
fn inspect(
    object: &Object,
    top: bool,
    catalog: bool,
    intent: i64,
    needs: &mut Needs,
    depth: usize,
) {
    if depth > MAX_NESTING {
        return;
    }
    match object {
        Object::Array(items) => {
            for item in items {
                inspect(item, false, false, intent, needs, depth + 1);
            }
        }
        Object::Name(name) => {
            if name.as_bytes().len() > MAX_NAME_BYTES {
                needs.long_names += 1;
            }
        }
        Object::Dict(_) | Object::Stream(_) => {
            let Some(dict) = object.as_dict() else {
                return;
            };
            if !(top && catalog) && dict.contains_key(b"Metadata") {
                needs.metadata += 1;
            }
            if intent != 4 && dict.get(b"OPM").and_then(Object::as_i64) == Some(1) {
                needs.overprint_mode += 1;
            }
            if dict.contains_key(b"Components") {
                match dict.get(b"ColorSpace").and_then(Object::as_name) {
                    Some(b"DeviceCMYK") if intent != 4 => needs.cmyk += 1,
                    Some(b"DeviceRGB") if intent != 3 => needs.srgb += 1,
                    _ => {}
                }
            }
            for (key, value) in dict.iter() {
                if key.as_bytes().len() > MAX_NAME_BYTES {
                    needs.long_names += 1;
                }
                inspect(value, false, false, intent, needs, depth + 1);
            }
        }
        _ => {}
    }
}

/// Make the changes [`inspect`] found.
fn fix(
    object: &mut Object,
    top: bool,
    catalog: bool,
    intent: i64,
    profiles: (Option<ObjId>, Option<ObjId>),
    depth: usize,
) {
    if depth > MAX_NESTING {
        return;
    }
    let dict = match object {
        Object::Array(items) => {
            for item in items {
                fix(item, false, false, intent, profiles, depth + 1);
            }
            return;
        }
        Object::Dict(dict) => dict,
        Object::Stream(stream) => &mut stream.dict,
        _ => return,
    };
    if !(top && catalog) {
        dict.remove(b"Metadata");
    }
    if intent != 4 && dict.get(b"OPM").and_then(Object::as_i64) == Some(1) {
        dict.insert("OPM", 0);
    }
    if dict.contains_key(b"Components") {
        let (srgb, cmyk) = profiles;
        let profile = match dict.get(b"ColorSpace").and_then(Object::as_name) {
            Some(b"DeviceCMYK") if intent != 4 => cmyk,
            Some(b"DeviceRGB") if intent != 3 => srgb,
            _ => None,
        };
        if let Some(profile) = profile {
            dict.insert(
                "ColorSpace",
                vec![Name::from("ICCBased").into(), profile.into()],
            );
        }
    }
    for (_, value) in dict.iter_mut() {
        fix(value, false, false, intent, profiles, depth + 1);
    }
}

/// Walk every object a save would write.
pub(crate) fn apply(
    doc: &mut Document,
    intent: i64,
    profiles: &mut Profiles,
    report: &mut Report,
) -> Result<()> {
    let catalog = doc.catalog_id();
    let mut total = Needs::default();
    for id in doc.reachable_objects()? {
        // Read without stream data; a stream is read in full only to change it.
        let Some(object) = doc.object_without_data(id)? else {
            continue;
        };
        let mut needs = Needs::default();
        inspect(&object, true, id == catalog, intent, &mut needs, 0);
        total.long_names += needs.long_names;
        if !needs.any_change() {
            continue;
        }
        let Some(mut object) = doc.object(id)? else {
            continue;
        };
        let srgb = if needs.srgb > 0 {
            Some(profiles.srgb(doc)?)
        } else {
            None
        };
        let cmyk = if needs.cmyk > 0 {
            Some(profiles.cmyk(doc)?)
        } else {
            None
        };
        fix(&mut object, true, id == catalog, intent, (srgb, cmyk), 0);
        doc.set_object(id, object);
        total.metadata += needs.metadata;
        total.overprint_mode += needs.overprint_mode;
        total.srgb += needs.srgb;
        total.cmyk += needs.cmyk;
    }
    report.change("object_metadata_removed", total.metadata);
    report.change("overprint_mode_reset", total.overprint_mode);
    report.change("process_color_space_replaced", total.srgb + total.cmyk);
    report.residual_add("name_too_long", total.long_names);
    Ok(())
}
