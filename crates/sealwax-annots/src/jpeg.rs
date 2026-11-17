//! What a JPEG's header says, read without decoding anything: its size, its colour components,
//! whether Adobe stored its CMYK inverted, and the orientation its EXIF data gives.

/// A JPEG's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// 1 (gray), 3 (RGB) or 4 (CMYK).
    pub(crate) components: u8,
    /// An Adobe marker (`APP14`): its CMYK samples are stored inverted, as Photoshop writes them.
    pub(crate) adobe: bool,
    /// The EXIF orientation, 1 to 8 (1 when it says none): the first EXIF segment's, as
    /// browsers read it; a later one, or another `APP1` segment (XMP), changes nothing.
    pub(crate) orientation: u8,
}

/// The header of `data`, when it is a JPEG of 8-bit samples a PDF reader decodes (`DCTDecode`).
pub(crate) fn header(data: &[u8]) -> Option<Header> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    let mut adobe = false;
    let mut orientation = None;
    let mut frame: Option<(u32, u32, u8)> = None;
    loop {
        // Fill bytes before a marker.
        while data.get(at) == Some(&0xFF) && data.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if data.get(at) != Some(&0xFF) {
            return None;
        }
        let marker = *data.get(at + 1)?;
        at += 2;
        // Markers without a segment.
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            continue;
        }
        // The image ends before its data.
        if marker == 0xD9 {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]));
        let segment = data.get(at + 2..at.checked_add(length)?)?;
        match marker {
            // The image data starts: the frame header must have come.
            0xDA => {
                let (width, height, components) = frame?;
                return Some(Header {
                    width,
                    height,
                    components,
                    adobe,
                    orientation: orientation.unwrap_or(1),
                });
            }
            0xE1 if orientation.is_none() && segment.starts_with(b"Exif\0\0") => {
                orientation = Some(exif_orientation(&segment[6..]).unwrap_or(1));
            }
            0xEE if segment.starts_with(b"Adobe") => adobe = true,
            // Baseline, extended and progressive frames, Huffman-coded: what every PDF reader
            // decodes. Lossless, hierarchical and arithmetic-coded ones are not embedded.
            0xC0..=0xC2 => {
                let [precision, h1, h0, w1, w0, components, ..] = segment else {
                    return None;
                };
                let width = u32::from(u16::from_be_bytes([*w1, *w0]));
                let height = u32::from(u16::from_be_bytes([*h1, *h0]));
                if *precision != 8 || width == 0 || height == 0 {
                    return None;
                }
                if !matches!(components, 1 | 3 | 4) {
                    return None;
                }
                frame = Some((width, height, *components));
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            _ => {}
        }
        at += length;
    }
}

/// The orientation tag (0x0112) of the first image file directory of EXIF data (a TIFF
/// structure), when it holds one from 1 to 8.
fn exif_orientation(tiff: &[u8]) -> Option<u8> {
    let little = match tiff.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?];
        Some(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let bytes = [
            *tiff.get(at)?,
            *tiff.get(at + 1)?,
            *tiff.get(at + 2)?,
            *tiff.get(at + 3)?,
        ];
        Some(if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    let directory = usize::try_from(u32_at(4)?).ok()?;
    let entries = usize::from(u16_at(directory)?);
    for index in 0..entries.min(512) {
        let entry = directory + 2 + index * 12;
        // A SHORT, its value in the entry's first two bytes.
        if u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3 {
            let value = u16_at(entry + 8)?;
            return u8::try_from(value).ok().filter(|v| (1..=8).contains(v));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Header, exif_orientation, header};

    #[test]
    fn a_header_says_size_components_and_orientation() {
        let photo = include_bytes!("../tests/data/photo-40x20-exif6.jpg");
        assert_eq!(
            header(photo),
            Some(Header {
                width: 40,
                height: 20,
                components: 3,
                adobe: false,
                orientation: 6,
            })
        );
        let cmyk = header(include_bytes!("../tests/data/photo-8x8-cmyk.jpg")).unwrap();
        assert_eq!(
            (cmyk.components, cmyk.adobe, cmyk.orientation),
            (4, true, 1)
        );
        assert_eq!(header(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(header(&photo[..40]), None, "a header cut short");
        let frame = photo.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        let scan = photo.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
        assert_eq!(header(&photo[..scan]), None, "no image data");
        let mut arithmetic = photo.to_vec();
        arithmetic[frame + 1] = 0xC9;
        assert_eq!(header(&arithmetic), None, "arithmetic coding");
        // Two EXIF segments: the first one counts, even without an orientation, as Chrome and
        // Apple's ImageIO read them; an XMP segment before it does not.
        let exif = photo.windows(6).position(|w| w == b"Exif\0\0").unwrap() - 4;
        let length = usize::from(u16::from_be_bytes([photo[exif + 2], photo[exif + 3]]));
        let six = &photo[exif..exif + 2 + length];
        let segment = |payload: &[u8]| {
            let mut out = vec![0xFF, 0xE1];
            out.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
            out.extend_from_slice(payload);
            out
        };
        let little = |entry: &[u8]| {
            let mut payload = b"Exif\0\0II*\0\x08\0\0\0\x01\0".to_vec();
            payload.extend_from_slice(entry);
            payload.extend_from_slice(&[0; 4]);
            segment(&payload)
        };
        let one = little(b"\x12\x01\x03\0\x01\0\0\0\x01\0\0\0");
        let none = little(b"\x31\x01\x02\0\x04\0\0\0abc\0");
        let xmp = segment(b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>");
        let with = |before: &[u8], after: &[u8]| {
            let mut out = photo[..exif].to_vec();
            out.extend_from_slice(before);
            out.extend_from_slice(six);
            out.extend_from_slice(after);
            out.extend_from_slice(&photo[exif + six.len()..]);
            header(&out).unwrap().orientation
        };
        assert_eq!(with(&one, &[]), 1, "orientation 1, then 6");
        assert_eq!(with(&[], &one), 6, "6, then 1");
        assert_eq!(with(&none, &[]), 1, "none, then 6");
        assert_eq!(with(&[], &none), 6, "6, then none");
        assert_eq!(with(&xmp, &[]), 6, "XMP, then 6");
        // Big-endian EXIF, orientation 3.
        let tiff = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x03\0\0\0\0\0\0";
        assert_eq!(exif_orientation(tiff), Some(3));
    }
}
