//! Photos made pages: a page for each JPEG or PNG, as a phone's photos are printed.

use sealwax_core::{Dict, Document, Name, Object, Rect};

use crate::Result;
use crate::image::{self, PictureError};
use crate::jpeg;
use crate::pdf;

/// US Letter, in points.
const LETTER: (f64, f64) = (612.0, 792.0);
/// Half an inch.
const MARGIN: f64 = 36.0;
/// The most pixels a PNG photo is decoded with, whatever the limits: an RGBA picture of 64
/// million pixels (a 50-megapixel phone's) takes 256 MB.
const MAX_PNG_PIXELS: u64 = 64_000_000;

/// What [`add_photo_pages`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PhotoPages {
    /// The pages added, one per photo read.
    pub added: usize,
    /// The photos that could not be read (from 0), in order: not a JPEG or a PNG, damaged, or
    /// larger than the limits' `max_image_pixels`.
    pub unreadable: Vec<usize>,
}

/// Add a page for each photo of `photos` (JPEG or PNG), in order, after the document's pages: a
/// US Letter sheet, portrait or landscape as the photo shows, with a margin of half an inch, the
/// photo centered and fitted without being stretched. A JPEG is embedded as it is, never encoded
/// again (`DCTDecode`, its colour space read from its header); a PNG keeps its transparency. The
/// orientation a phone writes in a JPEG's EXIF data becomes the page's rotation (and a mirror,
/// for the orientations that flip), so that the photo shows upright. The limits'
/// `max_image_pixels` apply to every photo before anything is decoded. A photo that cannot be
/// read gets no page, and is named in [`PhotoPages::unreadable`].
///
/// # Errors
/// As [`Document::add_object`], past the deadline included.
pub fn add_photo_pages(doc: &mut Document, photos: &[&[u8]]) -> Result<PhotoPages> {
    let mut report = PhotoPages::default();
    for (index, photo) in photos.iter().enumerate() {
        doc.check_time()?;
        match add_photo_page(doc, photo)? {
            true => report.added += 1,
            false => report.unreadable.push(index),
        }
    }
    Ok(report)
}

/// A new document of a page for each photo of `photos`, as [`add_photo_pages`] adds them.
///
/// # Errors
/// [`sealwax_core::Error::InvalidArgument`] when no photo can be read: there is no document
/// without pages.
pub fn photos_document(
    photos: &[&[u8]],
    limits: sealwax_core::Limits,
) -> Result<(Document, PhotoPages)> {
    let mut doc = Document::empty_with(limits)?;
    let report = add_photo_pages(&mut doc, photos)?;
    if report.added == 0 {
        return Err(sealwax_core::Error::InvalidArgument {
            what: "photos: none can be read",
        }
        .into());
    }
    Ok((doc, report))
}

/// The page of one photo; `false` when it cannot be read.
fn add_photo_page(doc: &mut Document, photo: &[u8]) -> Result<bool> {
    let max_pixels = doc.limits().max_image_pixels;
    let (image, width, height, orientation) = if let Some(header) = jpeg::header(photo) {
        if u64::from(header.width) * u64::from(header.height) > max_pixels {
            return Ok(false);
        }
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("XObject"));
        dict.insert("Subtype", Name::from("Image"));
        dict.insert("Width", i64::from(header.width));
        dict.insert("Height", i64::from(header.height));
        dict.insert(
            "ColorSpace",
            Name::from(match header.components {
                1 => "DeviceGray",
                4 => "DeviceCMYK",
                _ => "DeviceRGB",
            }),
        );
        dict.insert("BitsPerComponent", 8_i64);
        dict.insert("Filter", Name::from("DCTDecode"));
        if header.components == 4 && header.adobe {
            dict.insert(
                "Decode",
                pdf::numbers(&[1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]),
            );
        }
        let stream = sealwax_core::Stream {
            dict,
            data: photo.to_vec(),
        };
        let id = doc.add_object(Object::Stream(stream))?;
        (id, header.width, header.height, header.orientation)
    } else {
        match image::decode_within(photo, max_pixels.min(MAX_PNG_PIXELS)) {
            Ok(decoded) => {
                let added = image::add(doc, &decoded)?;
                (added.id, added.width, added.height, 1)
            }
            Err(
                PictureError::NotPng | PictureError::TooLarge { .. } | PictureError::Unreadable,
            ) => {
                return Ok(false);
            }
        }
    };
    // EXIF: flip horizontally first (2, 4, 5, 7), then turn clockwise.
    let (mirrored, quarter_turns) = match orientation {
        2 => (true, 0),
        3 => (false, 2),
        4 => (true, 2),
        5 => (true, 3),
        6 => (false, 1),
        7 => (true, 1),
        8 => (false, 3),
        _ => (false, 0),
    };
    let (width, height) = (f64::from(width), f64::from(height));
    // The sheet as the photo shows, then as the page holds it before its rotation.
    let (shown_width, shown_height) = if quarter_turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };
    let sheet = if shown_width > shown_height {
        (LETTER.1, LETTER.0)
    } else {
        LETTER
    };
    let (page_width, page_height) = if quarter_turns % 2 == 1 {
        (sheet.1, sheet.0)
    } else {
        sheet
    };
    let fit = ((page_width - 2.0 * MARGIN) / width).min((page_height - 2.0 * MARGIN) / height);
    let (drawn_width, drawn_height) = (width * fit, height * fit);
    let (x, y) = (
        (page_width - drawn_width) / 2.0,
        (page_height - drawn_height) / 2.0,
    );
    let matrix = if mirrored {
        [-drawn_width, 0.0, 0.0, drawn_height, x + drawn_width, y]
    } else {
        [drawn_width, 0.0, 0.0, drawn_height, x, y]
    };
    let content = format!(
        "q {} {} {} {} {} {} cm /Photo Do Q",
        matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5]
    );
    let contents = doc.add_object(pdf::flate(Dict::new(), content.as_bytes()))?;
    let mut xobjects = Dict::new();
    xobjects.insert("Photo", Object::Ref(image));
    let mut resources = Dict::new();
    resources.insert("XObject", xobjects);
    let mut page = Dict::new();
    page.insert("Resources", resources);
    page.insert("Contents", Object::Ref(contents));
    if quarter_turns != 0 {
        page.insert("Rotate", i64::from(quarter_turns) * 90);
    }
    doc.push_page(page, Rect::from_corners(0.0, 0.0, page_width, page_height))?;
    Ok(true)
}
