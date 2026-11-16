//! Drawing XObjects onto a page, after its own content.

use sealwax_core::{Dict, Document, Name, ObjId, Object, Rect, Result};

use crate::pdf;
use crate::text::number;

/// An XObject (a form or an image), the matrix that places it in the page's user space, and
/// what it shows, said in words: a picture's alternate description (ISO 32000-1, 14.9.3),
/// written as the `/Alt` of a `/Span` marked-content sequence around it, which tagging (PDF/A
/// level a) makes the alternate text of its figure.
#[derive(Debug, Clone)]
pub(crate) struct Placed {
    pub(crate) xobject: ObjId,
    pub(crate) matrix: [f64; 6],
    pub(crate) alt: Option<String>,
}

/// `text` as a PDF text string in a hexadecimal string: UTF-16BE after a byte order mark.
fn text_string(text: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in text.encode_utf16() {
        out.push_str(&format!("{unit:04X}"));
    }
    out.push('>');
    out
}

/// The resources a page uses: its own, else the nearest its page tree parents give it.
fn inherited_resources(
    doc: &Document,
    page: &Dict,
    handed_down: &mut pdf::HandedDown,
) -> Result<Dict> {
    Ok(match handed_down.get(doc, page, b"Resources")? {
        Some(Object::Dict(resources)) => resources,
        _ => Dict::new(),
    })
}

/// Store a page dictionary this crate changed, with the geometry the document reported for the
/// page: drawing never moves a page. On a damaged file the dictionary read now can differ from
/// the one measured when it was opened (a repaired cross-reference, hayro issue #1746), and a
/// save keeps what a changed page says. As in the save of `sealwax-core`, only what does not read
/// back as reported is written: another page can inherit from this one.
pub(crate) fn store(doc: &mut Document, page_id: ObjId, mut page: Dict) {
    if let Some(geometry) = doc.page_geometry(page_id) {
        let rect = |r: Rect| pdf::numbers(&[r.x1, r.y1, r.x2, r.y2]);
        if read_rect(page.get(b"MediaBox")) != Some(geometry.media_box) {
            page.insert("MediaBox", rect(geometry.media_box));
        }
        let crop_reads_back = match read_rect(page.get(b"CropBox")) {
            Some(own) => {
                let clipped = own.intersect(&geometry.media_box);
                let visible = clipped.width() > 0.0 && clipped.height() > 0.0;
                (if visible { clipped } else { geometry.media_box }) == geometry.crop_box
            }
            None => geometry.crop_box == geometry.media_box,
        };
        if !crop_reads_back {
            page.insert("CropBox", rect(geometry.crop_box));
        }
        let degrees = i64::from(geometry.rotation.degrees());
        let own = page
            .get(b"Rotate")
            .and_then(Object::as_i64)
            .map_or(0, |r| r.rem_euclid(360));
        if own != degrees {
            page.insert("Rotate", degrees);
        }
    }
    doc.set_object(page_id, Object::Dict(page));
}

/// A rectangle as the geometry reads it: normalized, at the `f32` precision hayro-syntax reads.
fn read_rect(value: Option<&Object>) -> Option<Rect> {
    let values = pdf::number_array(value?)?;
    let [a, b, c, d] = values.as_slice() else {
        return None;
    };
    #[allow(clippy::cast_possible_truncation)]
    let f = |v: f64| f64::from(v as f32);
    Some(Rect::from_corners(f(*a), f(*b), f(*c), f(*d)))
}

/// A name `/SwFlat<n>` the dictionary does not have.
fn free_name(xobjects: &Dict) -> Name {
    let mut number = xobjects.len();
    loop {
        let name = format!("SwFlat{number}");
        if !xobjects.contains_key(name.as_bytes()) {
            return Name::from(name.as_str());
        }
        number += 1;
    }
}

fn content(doc: &mut Document, data: &[u8]) -> Result<Object> {
    let mut framed = Vec::with_capacity(data.len() + 2);
    framed.push(b'\n');
    framed.extend_from_slice(data);
    framed.push(b'\n');
    Ok(Object::Ref(
        doc.add_object(pdf::flate(Dict::new(), &framed))?,
    ))
}

/// Draw each XObject by its matrix after the page's own content, which is wrapped in `q`/`Q` so
/// that its graphics state cannot leak into them. The page gets resources of its own: they may be
/// shared with other pages, or inherited.
pub(crate) fn draw(
    doc: &mut Document,
    page_id: ObjId,
    placed: &[Placed],
    handed_down: &mut pdf::HandedDown,
) -> Result<()> {
    if placed.is_empty() {
        return Ok(());
    }
    let Some(Object::Dict(mut page)) = doc.object(page_id)? else {
        return Ok(());
    };
    let mut resources = inherited_resources(doc, &page, handed_down)?;
    let mut xobjects = pdf::dict(doc, resources.get(b"XObject"))?.unwrap_or_default();
    let mut drawing = b"Q".to_vec();
    for item in placed {
        let name = free_name(&xobjects);
        let matrix: Vec<String> = item.matrix.iter().map(|v| number(*v)).collect();
        let shown = format!(
            "q {} cm /{} Do Q",
            matrix.join(" "),
            String::from_utf8_lossy(name.as_bytes())
        );
        let line = match &item.alt {
            Some(alt) => format!("\n/Span <</Alt {}>> BDC {shown} EMC", text_string(alt)),
            None => format!("\n{shown}"),
        };
        drawing.extend_from_slice(line.as_bytes());
        xobjects.insert(name, Object::Ref(item.xobject));
    }
    resources.insert("XObject", xobjects);
    page.insert("Resources", resources);
    let originals: Vec<Object> = match page.get(b"Contents") {
        Some(Object::Ref(id)) => match doc.object(*id)? {
            Some(Object::Array(items)) => items,
            Some(_) => vec![Object::Ref(*id)],
            None => Vec::new(),
        },
        Some(Object::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let mut contents = vec![content(doc, b"q")?];
    contents.extend(originals);
    contents.push(content(doc, &drawing)?);
    page.insert("Contents", Object::Array(contents));
    store(doc, page_id, page);
    Ok(())
}
