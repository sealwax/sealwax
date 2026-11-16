//! Colour: the PDF/A output intent, and default colour spaces where the content uses a device
//! space the output intent does not cover (ISO 19005-3, 6.2.3 and 6.2.4.3).
//!
//! Both ICC profiles come from Compact ICC Profiles (CC0-1.0, THIRD_PARTY_NOTICES.md): sRGB
//! (`sRGB-v2-magic`, ICC v2, 736 bytes) and a CMYK profile modelled on the appearance of
//! U.S. Web Coated (SWOP) v2, the CMYK profile in common use (`CGATS001Compat-v2-micro`, 8 KB).

use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use sealwax_core::{Dict, Document, Name, ObjId, Object, Result, Stream};

use crate::Report;
use crate::edit::{self, Location};
use crate::scan::Scan;

const SRGB: &[u8] = include_bytes!("../assets/icc/sRGB-v2-magic.icc");
const CMYK: &[u8] = include_bytes!("../assets/icc/CGATS001Compat-v2-micro.icc");

/// A stream compressed with `FlateDecode`.
pub(crate) fn flate_stream(mut dict: Dict, data: &[u8]) -> Stream {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    // Writing into a Vec cannot fail.
    let compressed = encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    if compressed.is_empty() && !data.is_empty() {
        return Stream {
            dict,
            data: data.to_vec(),
        };
    }
    dict.insert("Filter", Name::from("FlateDecode"));
    Stream {
        dict,
        data: compressed,
    }
}

fn icc_stream(profile: &[u8], components: i64) -> Object {
    let mut dict = Dict::new();
    dict.insert("N", components);
    Object::Stream(flate_stream(dict, profile))
}

/// The colour components of the output intent's profile, read from its stream.
fn components(doc: &Document, profile: &Object) -> Result<Option<i64>> {
    Ok(edit::resolved(doc, profile)?
        .as_ref()
        .and_then(Object::as_dict)
        .and_then(|dict| dict.get(b"N"))
        .and_then(Object::as_i64))
}

/// The ICC profile streams the conversion added, created on first use.
#[derive(Debug, Default)]
pub(crate) struct Profiles {
    srgb: Option<ObjId>,
    cmyk: Option<ObjId>,
}

impl Profiles {
    pub(crate) fn srgb(&mut self, doc: &mut Document) -> Result<ObjId> {
        if let Some(id) = self.srgb {
            return Ok(id);
        }
        Ok(*self.srgb.insert(doc.add_object(icc_stream(SRGB, 3))?))
    }

    pub(crate) fn cmyk(&mut self, doc: &mut Document) -> Result<ObjId> {
        if let Some(id) = self.cmyk {
            return Ok(id);
        }
        Ok(*self.cmyk.insert(doc.add_object(icc_stream(CMYK, 4))?))
    }
}

/// Make sure the catalog carries a PDF/A output intent, and give default colour spaces to the
/// content that uses a device space it does not cover. Returns the intent's colour components.
pub(crate) fn apply(
    doc: &mut Document,
    scan: &Scan,
    profiles: &mut Profiles,
    report: &mut Report,
) -> Result<i64> {
    let catalog = Location::of(doc.catalog_id());
    let intent = output_intent(doc, &catalog, profiles, report)?;
    for (location, devices) in &scan.resources {
        let mut defaults: Vec<(&str, ObjId)> = Vec::new();
        if devices.rgb && intent != 3 {
            defaults.push(("DefaultRGB", profiles.srgb(doc)?));
        }
        if devices.cmyk && intent != 4 {
            defaults.push(("DefaultCMYK", profiles.cmyk(doc)?));
        }
        if defaults.is_empty() {
            continue;
        }
        let spaces = location.key(b"ColorSpace");
        let mut added = 0;
        edit::edit_dict(doc, &spaces, true, |dict| {
            for (key, profile) in &defaults {
                if !dict.contains_key(key.as_bytes()) {
                    dict.insert(*key, vec![Name::from("ICCBased").into(), (*profile).into()]);
                    added += 1;
                }
            }
        })?;
        report.change("default_color_space_added", added);
    }
    Ok(intent)
}

/// The colour components of the PDF/A output intent, added (sRGB) when there is none.
fn output_intent(
    doc: &mut Document,
    catalog: &Location,
    profiles: &mut Profiles,
    report: &mut Report,
) -> Result<i64> {
    let Some(root) = edit::read_dict(doc, catalog)? else {
        return Ok(3);
    };
    let intents = match root.get(b"OutputIntents") {
        Some(value) => edit::resolved(doc, value)?,
        None => None,
    };
    let intents: Vec<Object> = intents
        .as_ref()
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
        .unwrap_or_default();
    let mut pdfa: Option<(Object, i64)> = None;
    for intent in &intents {
        let Some(Object::Dict(dict)) = edit::resolved(doc, intent)? else {
            continue;
        };
        if dict.get(b"S").and_then(Object::as_name) == Some(b"GTS_PDFA1")
            && let Some(profile) = dict.get(b"DestOutputProfile")
            && let Some(n) = components(doc, profile)?
            && matches!(n, 1 | 3 | 4)
        {
            pdfa = Some((profile.clone(), n));
            break;
        }
    }
    if let Some((profile, n)) = pdfa {
        // Every intent with a profile must then share the PDF/A one (6.2.3): the others go.
        let mut kept = Vec::new();
        for intent in &intents {
            let shares = match edit::resolved(doc, intent)? {
                Some(Object::Dict(dict)) => dict
                    .get(b"DestOutputProfile")
                    .is_none_or(|own| *own == profile),
                _ => false,
            };
            if shares {
                kept.push(intent.clone());
            }
        }
        if kept.len() != intents.len() {
            report.change("output_intent_removed", intents.len() - kept.len());
            edit::edit_dict(doc, catalog, false, |dict| {
                dict.insert("OutputIntents", kept);
            })?;
        }
        return Ok(n);
    }
    let profile = profiles.srgb(doc)?;
    let mut intent = Dict::new();
    intent.insert("Type", Name::from("OutputIntent"));
    intent.insert("S", Name::from("GTS_PDFA1"));
    intent.insert(
        "OutputConditionIdentifier",
        Object::String(b"sRGB".to_vec()),
    );
    intent.insert(
        "RegistryName",
        Object::String(b"http://www.color.org".to_vec()),
    );
    intent.insert("Info", Object::String(b"sRGB IEC61966-2.1".to_vec()));
    intent.insert("DestOutputProfile", profile);
    let intent = doc.add_object(Object::Dict(intent))?;
    // Intents without a profile of their own can stay; those with one would differ from ours.
    let mut kept = vec![Object::Ref(intent)];
    for other in &intents {
        if let Some(Object::Dict(dict)) = edit::resolved(doc, other)?
            && !dict.contains_key(b"DestOutputProfile")
        {
            kept.push(other.clone());
        }
    }
    report.change("output_intent_removed", intents.len() + 1 - kept.len());
    report.change("output_intent_added", 1);
    edit::edit_dict(doc, catalog, false, |dict| {
        dict.insert("OutputIntents", kept);
    })?;
    Ok(3)
}
