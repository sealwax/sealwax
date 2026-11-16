//! The revision a signature appends (ISO 32000-1, 12.8; ETSI EN 319 142-1): a signature field
//! merged with its invisible widget on the first page, the signature dictionary, the form's
//! `/Fields` and `/SigFlags`, and for a certification the DocMDP reference and the catalog's
//! `/Perms`. Nothing else is touched: after a certification, validators compare revisions.
//!
//! The dictionary is written with a `/ByteRange` of placeholders and a `/Contents` of zeros, in
//! a plain object (never in an object stream); once the file is laid out, the byte range is
//! written over its placeholders, padded with spaces, and the signature over the zeros.

use std::ops::Range;

use sealwax_core::{
    Dict, Document, Name, ObjId, Object, OpenOptions, decode_text_string, encode_text_string,
};

use crate::{Error, Kind, Options, Result};

/// The placeholder of each number of `/ByteRange` but the first: ten digits, enough for any
/// offset of a file sealwax writes.
const PLACEHOLDER: i64 = 9_999_999_999;
const PLACEHOLDER_DIGITS: usize = 10;

/// A file with a signature dictionary waiting for its signature.
#[derive(Debug, Clone)]
pub(crate) struct Laid {
    pub(crate) data: Vec<u8>,
    /// The hexadecimal digits of `/Contents`, between `<` and `>`.
    pub(crate) contents: Range<usize>,
}

impl Laid {
    /// The bytes the signature covers: everything but `/Contents` and its brackets.
    pub(crate) fn signed_parts(&self) -> [&[u8]; 2] {
        [
            &self.data[..self.contents.start - 1],
            &self.data[self.contents.end + 1..],
        ]
    }
}

fn dict_of(doc: &Document, value: Option<&Object>) -> Result<Option<(Option<ObjId>, Dict)>> {
    Ok(match value {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Dict(dict)) => Some((Some(*id), dict)),
            _ => None,
        },
        Some(Object::Dict(dict)) => Some((None, dict.clone())),
        _ => None,
    })
}

fn resolved(doc: &Document, value: &Object) -> Result<Option<Object>> {
    match value {
        Object::Ref(id) => Ok(doc.object(*id)?),
        other => Ok(Some(other.clone())),
    }
}

/// The form's fields as (full name, field dictionary), top-level and below. A full name is the
/// partial names (`/T`) from the top down, each decoded as the text string it is, joined by
/// periods (ISO 32000-1, 12.7.3.2): a name written in UTF-16BE is the same name as in
/// PDFDocEncoding.
pub(crate) fn fields(doc: &Document, form: &Dict) -> Result<Vec<(String, Dict)>> {
    let mut found = Vec::new();
    let Some(Object::Array(top)) = form
        .get(b"Fields")
        .map(|f| resolved(doc, f))
        .transpose()?
        .flatten()
    else {
        return Ok(found);
    };
    let mut stack: Vec<(Object, String, usize)> =
        top.into_iter().map(|f| (f, String::new(), 0)).collect();
    let mut seen = std::collections::HashSet::new();
    while let Some((value, parent, depth)) = stack.pop() {
        if depth > 32 || found.len() > 100_000 {
            break;
        }
        if let Object::Ref(id) = value
            && !seen.insert(id)
        {
            continue;
        }
        let Some(Object::Dict(field)) = resolved(doc, &value)? else {
            continue;
        };
        let mut name = parent.clone();
        if let Some(Object::String(part)) = field.get(b"T") {
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(&decode_text_string(part));
        }
        let kids = field.get(b"Kids").cloned();
        found.push((name.clone(), field));
        if let Some(Object::Array(kids)) = kids
            .as_ref()
            .map(|k| resolved(doc, k))
            .transpose()?
            .flatten()
        {
            stack.extend(kids.into_iter().map(|kid| (kid, name.clone(), depth + 1)));
        }
    }
    Ok(found)
}

/// Whether a field is a signature field with a value.
pub(crate) fn is_signed(field: &Dict) -> bool {
    field.get(b"FT").and_then(Object::as_name) == Some(&b"Sig"[..]) && field.contains_key(b"V")
}

/// The form of the document, and its object number if indirect.
pub(crate) fn form(doc: &Document, catalog: &Dict) -> Result<Option<(Option<ObjId>, Dict)>> {
    dict_of(doc, catalog.get(b"AcroForm"))
}

/// Each signed field: its full name, and its value (the signature dictionary) with its object
/// number when indirect.
pub(crate) fn signatures(doc: &Document) -> Result<Vec<(String, Option<ObjId>, Dict)>> {
    let Some(Object::Dict(catalog)) = doc.object(doc.catalog_id())? else {
        return Ok(Vec::new());
    };
    let Some((_, form)) = form(doc, &catalog)? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for (name, field) in fields(doc, &form)? {
        if !is_signed(&field) {
            continue;
        }
        let (id, value) = match field.get(b"V") {
            Some(Object::Ref(id)) => (Some(*id), doc.object(*id)?),
            Some(other) => (None, Some(other.clone())),
            None => (None, None),
        };
        if let Some(Object::Dict(value)) = value {
            found.push((name, id, value));
        }
    }
    Ok(found)
}

/// The signature dictionary the catalog's `/Perms /DocMDP` names: the certification's.
pub(crate) fn certification(doc: &Document) -> Result<Option<ObjId>> {
    let Some(Object::Dict(catalog)) = doc.object(doc.catalog_id())? else {
        return Ok(None);
    };
    Ok(dict_of(doc, catalog.get(b"Perms"))?
        .and_then(|(_, perms)| perms.get(b"DocMDP").and_then(Object::as_ref_id)))
}

/// The `/Contents` of the signature in the signed field `name`, as the file holds it (padded).
pub(crate) fn signature_contents(doc: &Document, name: &str) -> Result<Option<Vec<u8>>> {
    let Some(Object::Dict(catalog)) = doc.object(doc.catalog_id())? else {
        return Ok(None);
    };
    let Some((_, form)) = form(doc, &catalog)? else {
        return Ok(None);
    };
    for (full_name, field) in fields(doc, &form)? {
        if full_name != name || !is_signed(&field) {
            continue;
        }
        let value = field
            .get(b"V")
            .map(|v| resolved(doc, v))
            .transpose()?
            .flatten();
        if let Some(Object::Dict(signature)) = value
            && let Some(Object::String(contents)) = signature.get(b"Contents")
        {
            return Ok(Some(contents.clone()));
        }
    }
    Ok(None)
}

/// Append the signature's revision to `pdf`, its dictionary waiting for
/// `options.reserved_contents_bytes` bytes of signature.
pub(crate) fn lay_out(pdf: &[u8], options: &Options, time: &str) -> Result<Laid> {
    let mut signature = Dict::new();
    signature.insert("Type", Name::from("Sig"));
    signature.insert("Filter", Name::from("Adobe.PPKLite"));
    signature.insert("SubFilter", Name::from("ETSI.CAdES.detached"));
    signature.insert("M", Object::String(time.as_bytes().to_vec()));
    for (key, value) in [
        ("Reason", &options.reason),
        ("Location", &options.location),
        ("ContactInfo", &options.contact_info),
    ] {
        if let Some(value) = value {
            signature.insert(key, Object::String(encode_text_string(value)));
        }
    }
    let certification = matches!(options.kind, Kind::Certification { .. });
    if let Kind::Certification { permissions } = options.kind {
        let mut params = Dict::new();
        params.insert("Type", Name::from("TransformParams"));
        params.insert("P", i64::from(permissions));
        params.insert("V", Name::from("1.2"));
        let mut reference = Dict::new();
        reference.insert("Type", Name::from("SigRef"));
        reference.insert("TransformMethod", Name::from("DocMDP"));
        reference.insert("TransformParams", params);
        signature.insert("Reference", vec![Object::Dict(reference)]);
    }
    append(
        pdf,
        &options.field_name,
        signature,
        options.reserved_contents_bytes,
        certification,
    )
}

/// Append a document timestamp's revision to `pdf` (ISO 32000-2, 12.8.5; PAdES B-LTA): its
/// dictionary (`/DocTimeStamp`, `ETSI.RFC3161`) waiting for `reserved` bytes of token.
pub(crate) fn lay_out_timestamp(pdf: &[u8], field_name: &str, reserved: usize) -> Result<Laid> {
    let mut timestamp = Dict::new();
    timestamp.insert("Type", Name::from("DocTimeStamp"));
    timestamp.insert("Filter", Name::from("Adobe.PPKLite"));
    timestamp.insert("SubFilter", Name::from("ETSI.RFC3161"));
    append(pdf, field_name, timestamp, reserved, false)
}

/// Append to `pdf` a revision with the signature field `field_name` whose value is `signature`
/// (completed with its byte range and its `/Contents`, `reserved` bytes of zeros), and for a
/// certification the catalog's `/Perms`.
fn append(
    pdf: &[u8],
    field_name: &str,
    mut signature: Dict,
    reserved: usize,
    certification: bool,
) -> Result<Laid> {
    if field_name.is_empty() || field_name.contains('.') {
        return Err(Error::FieldName(field_name.to_owned()));
    }
    let mut doc = Document::open(pdf.to_vec(), &OpenOptions::default())?;
    let catalog_id = doc.catalog_id();
    let Some(Object::Dict(mut catalog)) = doc.object(catalog_id)? else {
        return Err(Error::Pdf(sealwax_core::Error::Malformed));
    };
    let form = dict_of(&doc, catalog.get(b"AcroForm"))?;
    let existing = match &form {
        Some((_, form)) => fields(&doc, form)?,
        None => Vec::new(),
    };
    if existing.iter().any(|(name, _)| name == field_name) {
        return Err(Error::FieldExists(field_name.to_owned()));
    }
    let certified = dict_of(&doc, catalog.get(b"Perms"))?
        .is_some_and(|(_, perms)| perms.contains_key(b"DocMDP"));
    if certification && (certified || existing.iter().any(|(_, field)| is_signed(field))) {
        return Err(Error::NotFirstSignature);
    }
    let Some(page_id) = doc.page_ids().first().copied().flatten() else {
        return Err(Error::NoPage);
    };

    // The signature dictionary, its byte range and value to come.
    signature.insert(
        "ByteRange",
        vec![
            Object::from(0i64),
            PLACEHOLDER.into(),
            PLACEHOLDER.into(),
            PLACEHOLDER.into(),
        ],
    );
    signature.insert("Contents", Object::String(vec![0; reserved]));
    let signature_id = doc.add_object(Object::Dict(signature))?;

    // The field, merged with its invisible widget, printed and locked.
    let mut field = Dict::new();
    field.insert("Type", Name::from("Annot"));
    field.insert("Subtype", Name::from("Widget"));
    field.insert("FT", Name::from("Sig"));
    field.insert("T", Object::String(encode_text_string(field_name)));
    field.insert("V", signature_id);
    field.insert(
        "Rect",
        vec![Object::from(0i64), 0i64.into(), 0i64.into(), 0i64.into()],
    );
    field.insert("F", 132i64);
    field.insert("P", page_id);
    let field_id = doc.add_object(Object::Dict(field))?;

    // The page's annotations.
    let Some(Object::Dict(mut page)) = doc.object(page_id)? else {
        return Err(Error::NoPage);
    };
    match page.get(b"Annots") {
        Some(Object::Ref(id)) if matches!(doc.object(*id)?, Some(Object::Array(_))) => {
            let id = *id;
            if let Some(Object::Array(mut annots)) = doc.object(id)? {
                annots.push(Object::Ref(field_id));
                doc.set_object(id, Object::Array(annots));
            }
        }
        Some(Object::Array(annots)) => {
            let mut annots = annots.clone();
            annots.push(Object::Ref(field_id));
            page.insert("Annots", annots);
            doc.set_object(page_id, Object::Dict(page));
        }
        _ => {
            page.insert("Annots", vec![Object::Ref(field_id)]);
            doc.set_object(page_id, Object::Dict(page));
        }
    }

    // The form.
    let mut catalog_changed = false;
    match form {
        Some((Some(form_id), mut form)) => {
            add_field(&mut doc, &mut form, field_id)?;
            doc.set_object(form_id, Object::Dict(form));
        }
        Some((None, mut form)) => {
            add_field(&mut doc, &mut form, field_id)?;
            catalog.insert("AcroForm", form);
            catalog_changed = true;
        }
        None => {
            let mut form = Dict::new();
            form.insert("Fields", vec![Object::Ref(field_id)]);
            form.insert("SigFlags", 3i64);
            let form_id = doc.add_object(Object::Dict(form))?;
            catalog.insert("AcroForm", form_id);
            catalog_changed = true;
        }
    }
    if certification {
        let mut perms = Dict::new();
        perms.insert("DocMDP", signature_id);
        catalog.insert("Perms", perms);
        catalog_changed = true;
    }
    if catalog_changed {
        doc.set_object(catalog_id, Object::Dict(catalog));
    }

    let data = doc.save_incremental()?;
    let base = pdf.len();
    locate(data, base, signature_id, reserved)
}

fn add_field(doc: &mut Document, form: &mut Dict, field: ObjId) -> Result<()> {
    match form.get(b"Fields") {
        Some(Object::Ref(id)) => {
            let id = *id;
            let mut fields = match doc.object(id)? {
                Some(Object::Array(fields)) => fields,
                _ => Vec::new(),
            };
            fields.push(Object::Ref(field));
            doc.set_object(id, Object::Array(fields));
        }
        Some(Object::Array(fields)) => {
            let mut fields = fields.clone();
            fields.push(Object::Ref(field));
            form.insert("Fields", fields);
        }
        _ => {
            form.insert("Fields", vec![Object::Ref(field)]);
        }
    }
    form.insert("SigFlags", 3i64);
    Ok(())
}

/// Find, in what the revision appended after `base`, the signature dictionary's placeholders,
/// and write the byte range over them.
fn locate(mut data: Vec<u8>, base: usize, signature: ObjId, reserved: usize) -> Result<Laid> {
    let header = format!("\n{} {} obj", signature.number, signature.generation);
    let start = find(&data, header.as_bytes(), base).ok_or(Error::Layout)?;
    let end = find(&data, b"endobj", start).ok_or(Error::Layout)?;
    let range_key = find(&data[..end], b"/ByteRange", start).ok_or(Error::Layout)?;
    let mut numbers = Vec::new();
    let mut at = range_key;
    let placeholder = PLACEHOLDER.to_string();
    for _ in 0..3 {
        let found = find(&data[..end], placeholder.as_bytes(), at).ok_or(Error::Layout)?;
        numbers.push(found);
        at = found + PLACEHOLDER_DIGITS;
    }
    let contents_key = find(&data[..end], b"/Contents", start).ok_or(Error::Layout)?;
    let open = find(&data[..end], b"<", contents_key).ok_or(Error::Layout)?;
    let digits = open + 1..open + 1 + reserved * 2;
    if data.get(digits.end) != Some(&b'>') || data[digits.clone()].iter().any(|b| *b != b'0') {
        return Err(Error::Layout);
    }
    let after = digits.end + 1;
    let values = [open, after, data.len() - after];
    for (position, value) in numbers.iter().zip(values) {
        let text = format!("{value:<width$}", width = PLACEHOLDER_DIGITS);
        if text.len() != PLACEHOLDER_DIGITS {
            return Err(Error::Layout);
        }
        data[*position..position + PLACEHOLDER_DIGITS].copy_from_slice(text.as_bytes());
    }
    Ok(Laid {
        data,
        contents: digits,
    })
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// Write `cms` (DER) into the waiting `/Contents`, in hexadecimal, the rest left zeros.
pub(crate) fn fill(laid: &mut Laid, cms: &[u8]) -> Result<()> {
    let room = laid.contents.len() / 2;
    if cms.len() > room {
        return Err(Error::ContentsTooLarge {
            needed: cms.len(),
            reserved: room,
        });
    }
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for (index, byte) in cms.iter().enumerate() {
        let at = laid.contents.start + index * 2;
        laid.data[at] = DIGITS[usize::from(byte >> 4)];
        laid.data[at + 1] = DIGITS[usize::from(byte & 0x0f)];
    }
    Ok(())
}
