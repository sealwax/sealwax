//! A PNG picture as an image XObject: its colours in `DeviceRGB`, its transparency as a soft mask
//! (`SMask`).

use std::io::Cursor;

use png::{ColorType, Decoder, Limits, Transformations};
use sealwax_core::{Dict, Document, Name, ObjId, Object};

use crate::pdf;

/// The largest picture decoded, in pixels: 64 MB once decoded as RGBA. A signing page's largest
/// picture is a composite stamp drawn at 8 times a field of at most 400 x 200 points (3,200 x 1,600,
/// 5.1 million pixels). A PNG claiming more is refused before its data is inflated, which bounds
/// what a decompression bomb costs.
pub const MAX_PIXELS: u64 = 16_000_000;
const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// What the decoder may allocate: an RGBA picture of [`MAX_PIXELS`] and its rows.
const MAX_BYTES: usize = 80 << 20;

/// An image added to the document, and its size in pixels.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Image {
    pub(crate) id: ObjId,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Why a picture cannot be drawn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PictureError {
    #[error("is not a PNG")]
    NotPng,
    #[error("has {width} x {height} pixels, accepted: {MAX_PIXELS}")]
    TooLarge { width: u32, height: u32 },
    #[error("is not a readable PNG")]
    Unreadable,
}

/// The pixels of a PNG: RGB samples, and the alpha channel when one is not fully opaque.
pub(crate) struct Decoded {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgb: Vec<u8>,
    pub(crate) alpha: Option<Vec<u8>>,
}

/// The pixels a PNG declares, from its header alone: nothing is inflated.
pub(crate) fn pixels(png: &[u8]) -> Result<u64, PictureError> {
    if !png.starts_with(SIGNATURE) {
        return Err(PictureError::NotPng);
    }
    let mut decoder = Decoder::new_with_limits(Cursor::new(png), Limits { bytes: MAX_BYTES });
    let header = decoder
        .read_header_info()
        .map_err(|_| PictureError::Unreadable)?;
    Ok(u64::from(header.width) * u64::from(header.height))
}

pub(crate) fn decode(png: &[u8]) -> Result<Decoded, PictureError> {
    decode_within(png, MAX_PIXELS)
}

/// The PNG decoded, refused before its data is inflated when it declares more than
/// `max_pixels`.
pub(crate) fn decode_within(png: &[u8], max_pixels: u64) -> Result<Decoded, PictureError> {
    if !png.starts_with(SIGNATURE) {
        return Err(PictureError::NotPng);
    }
    let unreadable = |_| PictureError::Unreadable;
    // An RGBA picture of `max_pixels` and its rows.
    let bytes = usize::try_from(max_pixels.saturating_mul(5)).unwrap_or(usize::MAX);
    let mut decoder = Decoder::new_with_limits(Cursor::new(png), Limits { bytes });
    decoder.set_transformations(Transformations::normalize_to_color8());
    let header = decoder.read_header_info().map_err(unreadable)?;
    let (width, height) = (header.width, header.height);
    if u64::from(width) * u64::from(height) > max_pixels {
        return Err(PictureError::TooLarge { width, height });
    }
    let mut reader = decoder.read_info().map_err(unreadable)?;
    let size = reader
        .output_buffer_size()
        .ok_or(PictureError::Unreadable)?;
    let mut buffer = vec![0u8; size];
    let frame = reader.next_frame(&mut buffer).map_err(unreadable)?;
    buffer.truncate(frame.buffer_size());
    let channels = match frame.color_type {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Indexed => return Err(PictureError::Unreadable),
    };
    let pixels = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| PictureError::Unreadable)?;
    if buffer.len() < pixels * channels {
        return Err(PictureError::Unreadable);
    }
    let mut rgb = Vec::with_capacity(pixels * 3);
    let mut alpha = (channels == 2 || channels == 4).then(|| Vec::with_capacity(pixels));
    for pixel in buffer.chunks_exact(channels).take(pixels) {
        match channels {
            1 | 2 => rgb.extend_from_slice(&[pixel[0], pixel[0], pixel[0]]),
            _ => rgb.extend_from_slice(&pixel[..3]),
        }
        if let Some(alpha) = alpha.as_mut() {
            alpha.push(pixel[channels - 1]);
        }
    }
    // A picture opaque everywhere needs no soft mask.
    let alpha = alpha.filter(|values| values.iter().any(|v| *v != 255));
    Ok(Decoded {
        width,
        height,
        rgb,
        alpha,
    })
}

/// The PNG as an image XObject of the document, with its soft mask when it has transparency.
pub(crate) fn add(doc: &mut Document, decoded: &Decoded) -> sealwax_core::Result<Image> {
    let image_dict = |color_space: &str| {
        let mut dict = Dict::new();
        dict.insert("Type", Name::from("XObject"));
        dict.insert("Subtype", Name::from("Image"));
        dict.insert("Width", i64::from(decoded.width));
        dict.insert("Height", i64::from(decoded.height));
        dict.insert("ColorSpace", Name::from(color_space));
        dict.insert("BitsPerComponent", 8i64);
        dict
    };
    let mut dict = image_dict("DeviceRGB");
    if let Some(alpha) = &decoded.alpha {
        let mask = doc.add_object(pdf::flate(image_dict("DeviceGray"), alpha))?;
        dict.insert("SMask", Object::Ref(mask));
    }
    let id = doc.add_object(pdf::flate(dict, &decoded.rgb))?;
    Ok(Image {
        id,
        width: decoded.width,
        height: decoded.height,
    })
}
