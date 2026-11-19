//! JPEGs and PNGs drawn on existing pages or fitted to new photo pages.

use sealwax_core::{Dict, Document, Error, Name, Object, Rect};

use crate::Result;
use crate::image::{self, Image, PictureError};
use crate::jpeg;
use crate::page::{self, Placed};
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
/// orientation a phone writes in a JPEG's EXIF data (its first EXIF segment, as browsers read
/// it) becomes the page's rotation (and a mirror, for the orientations that flip), so that the
/// photo shows upright. The limits'
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

/// An image ready to embed. JPEG data stays encoded.
enum Photo<'a> {
    Jpeg(&'a [u8], jpeg::Header),
    Png(image::Decoded),
}

impl<'a> Photo<'a> {
    fn read(data: &'a [u8], max_pixels: u64) -> std::result::Result<Self, PictureError> {
        if let Some(header) = jpeg::header(data) {
            if u64::from(header.width) * u64::from(header.height) > max_pixels {
                return Err(PictureError::TooLarge {
                    width: header.width,
                    height: header.height,
                });
            }
            Ok(Self::Jpeg(data, header))
        } else {
            image::decode_within(data, max_pixels.min(MAX_PNG_PIXELS)).map(Self::Png)
        }
    }

    /// The image XObject and its EXIF orientation.
    fn embed(self, doc: &mut Document) -> sealwax_core::Result<(Image, u8)> {
        let (photo, header) = match self {
            Self::Png(decoded) => return Ok((image::add(doc, &decoded)?, 1)),
            Self::Jpeg(data, header) => (data, header),
        };
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
        Ok((
            Image {
                id,
                width: header.width,
                height: header.height,
            },
            header.orientation,
        ))
    }
}

/// The horizontal flip followed by the clockwise quarter turns specified by EXIF.
fn orientation(exif: u8) -> (bool, u8) {
    match exif {
        2 => (true, 0),
        3 => (false, 2),
        4 => (true, 2),
        5 => (true, 3),
        6 => (false, 1),
        7 => (true, 1),
        8 => (false, 3),
        _ => (false, 0),
    }
}

/// Place the image's unit square into a PDF rectangle, applying the requested EXIF transform.
fn image_matrix(
    image: &Image,
    rect: Rect,
    mirrored: bool,
    turns: u8,
    keep_proportion: bool,
) -> [f64; 6] {
    let (mut width, mut height) = (rect.width(), rect.height());
    if keep_proportion {
        let (pixel_width, pixel_height) = if turns % 2 == 1 {
            (image.height, image.width)
        } else {
            (image.width, image.height)
        };
        let scale = (width / f64::from(pixel_width)).min(height / f64::from(pixel_height));
        width = f64::from(pixel_width) * scale;
        height = f64::from(pixel_height) * scale;
    }
    let x = rect.x1 + (rect.width() - width) / 2.0;
    let y = rect.y1 + (rect.height() - height) / 2.0;
    let mut matrix = match turns {
        1 => [0.0, -height, width, 0.0, x, y + height],
        2 => [-width, 0.0, 0.0, -height, x + width, y + height],
        3 => [0.0, height, -width, 0.0, x + width, y],
        _ => [width, 0.0, 0.0, height, x, y],
    };
    if mirrored {
        matrix[4] += matrix[0];
        matrix[5] += matrix[1];
        matrix[0] = -matrix[0];
        matrix[1] = -matrix[1];
    }
    matrix
}

/// Draw a JPEG or PNG over the existing content of page `page_index` (zero-based).
/// `rect` is in default PDF user space, before page rotation, with y increasing upwards.
/// With `keep_proportion`, the oriented image is fitted and centered in the rectangle.
/// Otherwise it fills the rectangle. JPEG bytes and PNG transparency are preserved.
/// The document's `max_image_pixels` applies. PNG decoding also has a ceiling of
/// 64 million pixels, shared with [`add_photo_pages`].
///
/// # Errors
/// Returns an invalid argument error for an unknown page, an invalid rectangle or an
/// unreadable image, or a limit error when the image exceeds the pixel budget.
pub fn insert_image(
    doc: &mut Document,
    page_index: usize,
    rect: Rect,
    data: &[u8],
    keep_proportion: bool,
) -> Result<()> {
    doc.check_time()?;
    let page_id = doc
        .page_ids()
        .get(page_index)
        .copied()
        .flatten()
        .ok_or(Error::InvalidArgument { what: "page index" })?;
    if ![rect.width(), rect.height()]
        .into_iter()
        .all(|side| side.is_finite() && side > 0.0)
    {
        return Err(Error::InvalidArgument {
            what: "image rectangle",
        }
        .into());
    }
    if !matches!(doc.object_without_data(page_id)?, Some(Object::Dict(_))) {
        return Err(Error::Malformed.into());
    }
    let photo = Photo::read(data, doc.limits().max_image_pixels).map_err(|e| match e {
        PictureError::TooLarge { .. } => Error::LimitExceeded {
            what: "max_image_pixels",
        },
        PictureError::NotPng | PictureError::Unreadable => Error::InvalidArgument {
            what: "JPEG or PNG image",
        },
    })?;
    doc.try_edit(|doc| {
        let (image, exif) = photo.embed(doc)?;
        let (mirrored, turns) = orientation(exif);
        let matrix = image_matrix(&image, rect, mirrored, turns, keep_proportion);
        let placed = Placed {
            xobject: image.id,
            matrix,
            alt: None,
        };
        page::draw(doc, page_id, &[placed], &mut pdf::HandedDown::default())?;
        doc.check_time()?;
        Ok(())
    })
}

/// The page of one photo; `false` when it cannot be read.
fn add_photo_page(doc: &mut Document, photo: &[u8]) -> Result<bool> {
    let Ok(photo) = Photo::read(photo, doc.limits().max_image_pixels) else {
        return Ok(false);
    };
    let (image, exif) = photo.embed(doc)?;
    let (mirrored, quarter_turns) = orientation(exif);
    // The sheet as the photo shows, then as the page holds it before its rotation.
    let (shown_width, shown_height) = if quarter_turns % 2 == 1 {
        (image.height, image.width)
    } else {
        (image.width, image.height)
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
    let printable = Rect::from_corners(MARGIN, MARGIN, page_width - MARGIN, page_height - MARGIN);
    // The page itself supplies the rotation for a full photo page.
    let matrix = image_matrix(&image, printable, mirrored, 0, true);
    let content = format!(
        "q {} {} {} {} {} {} cm /Photo Do Q",
        matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5]
    );
    let contents = doc.add_object(pdf::flate(Dict::new(), content.as_bytes()))?;
    let mut xobjects = Dict::new();
    xobjects.insert("Photo", Object::Ref(image.id));
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
