//! TrueType programs (`FontFile2`), as far as their widths go: the advance width of each glyph
//! (`hmtx`), and a copy of the program with some changed.
//!
//! As for CFF, the dictionary's widths are the ones the text is laid out with; the program is
//! changed to match them (ISO 19005-3, 6.2.11.5). Only `hmtx`, `hhea` and the checksums change.

/// How many tables a program may have.
const MAX_TABLES: usize = 512;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// A table record: tag, and where its bytes are.
#[derive(Debug, Clone)]
struct Table {
    tag: [u8; 4],
    offset: usize,
    length: usize,
}

/// A TrueType program.
#[derive(Debug)]
pub(crate) struct TrueType<'a> {
    data: &'a [u8],
    version: u32,
    tables: Vec<Table>,
    units_per_em: u16,
    /// Advance width of each glyph, in font units.
    advances: Vec<u16>,
    /// Left side bearing of each glyph.
    bearings: Vec<i16>,
}

impl<'a> TrueType<'a> {
    pub(crate) fn parse(data: &'a [u8]) -> Option<Self> {
        let version = u32_at(data, 0)?;
        if !matches!(version, 0x0001_0000 | 0x7472_7565) {
            return None;
        }
        let count = usize::from(u16_at(data, 4)?);
        if count > MAX_TABLES {
            return None;
        }
        let mut tables = Vec::with_capacity(count);
        for i in 0..count {
            let record = 12 + 16 * i;
            let tag: [u8; 4] = data.get(record..record + 4)?.try_into().ok()?;
            let offset = usize::try_from(u32_at(data, record + 8)?).ok()?;
            let length = usize::try_from(u32_at(data, record + 12)?).ok()?;
            data.get(offset..offset.checked_add(length)?)?;
            tables.push(Table {
                tag,
                offset,
                length,
            });
        }
        let table = |tag: &[u8; 4]| {
            tables
                .iter()
                .find(|t| &t.tag == tag)
                .and_then(|t| data.get(t.offset..t.offset + t.length))
        };
        let head = table(b"head")?;
        let units_per_em = u16_at(head, 18).filter(|u| *u > 0)?;
        let metrics = usize::from(u16_at(table(b"hhea")?, 34)?);
        let glyphs = usize::from(u16_at(table(b"maxp")?, 4)?);
        let hmtx = table(b"hmtx")?;
        if metrics == 0 || metrics > glyphs {
            return None;
        }
        let mut advances = Vec::with_capacity(glyphs);
        let mut bearings = Vec::with_capacity(glyphs);
        for g in 0..glyphs {
            if g < metrics {
                advances.push(u16_at(hmtx, 4 * g)?);
                bearings.push(i16::from_be_bytes(u16_at(hmtx, 4 * g + 2)?.to_be_bytes()));
            } else {
                advances.push(*advances.last()?);
                let at = 4 * metrics + 2 * (g - metrics);
                // Some programs end hmtx early: the bearing is then 0.
                bearings.push(u16_at(hmtx, at).map_or(0, |b| i16::from_be_bytes(b.to_be_bytes())));
            }
        }
        Some(Self {
            data,
            version,
            tables,
            units_per_em,
            advances,
            bearings,
        })
    }

    pub(crate) fn glyph_count(&self) -> usize {
        self.advances.len()
    }

    /// The advance width of glyph `gid`, in text space units per 1000 em.
    pub(crate) fn width(&self, gid: u16) -> Option<f64> {
        let advance = *self.advances.get(usize::from(gid))?;
        Some(f64::from(advance) * 1000.0 / f64::from(self.units_per_em))
    }

    /// A copy where each glyph of `widths` has that advance width (text space units per
    /// 1000 em, rounded to font units). `None` for a width a program cannot hold.
    pub(crate) fn with_widths(&self, widths: &[(u16, f64)]) -> Option<Vec<u8>> {
        let mut advances = self.advances.clone();
        for (gid, width) in widths {
            let units = (width * f64::from(self.units_per_em) / 1000.0).round();
            if !(0.0..=f64::from(u16::MAX)).contains(&units) {
                return None;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let units = units as u16;
            *advances.get_mut(usize::from(*gid))? = units;
        }
        // Every glyph gets its own metric: simpler than finding the run of equal advances.
        let mut hmtx = Vec::with_capacity(4 * advances.len());
        for (advance, bearing) in advances.iter().zip(&self.bearings) {
            hmtx.extend_from_slice(&advance.to_be_bytes());
            hmtx.extend_from_slice(&bearing.to_be_bytes());
        }
        let mut replaced: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        for table in &self.tables {
            let mut bytes = self
                .data
                .get(table.offset..table.offset + table.length)?
                .to_vec();
            match &table.tag {
                b"hmtx" => bytes.clone_from(&hmtx),
                b"hhea" => {
                    let count = u16::try_from(advances.len()).ok()?;
                    bytes.get_mut(34..36)?.copy_from_slice(&count.to_be_bytes());
                    let widest = advances.iter().copied().max().unwrap_or(0);
                    bytes
                        .get_mut(10..12)?
                        .copy_from_slice(&widest.to_be_bytes());
                }
                b"head" => bytes.get_mut(8..12)?.copy_from_slice(&[0; 4]),
                _ => {}
            }
            replaced.push((table.tag, bytes));
        }
        Some(self.write(&replaced))
    }

    fn write(&self, tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let count = u16::try_from(tables.len()).unwrap_or(0);
        let mut power = 1u16;
        let mut selector = 0u16;
        while power.saturating_mul(2) <= count {
            power *= 2;
            selector += 1;
        }
        let search_range = power.saturating_mul(16);
        let mut out = Vec::new();
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&search_range.to_be_bytes());
        out.extend_from_slice(&selector.to_be_bytes());
        out.extend_from_slice(
            &count
                .saturating_mul(16)
                .saturating_sub(search_range)
                .to_be_bytes(),
        );
        let mut offset = 12 + 16 * tables.len();
        let mut records = Vec::new();
        let mut body = Vec::new();
        // Records sorted by tag, as the format requires.
        let mut sorted: Vec<&([u8; 4], Vec<u8>)> = tables.iter().collect();
        sorted.sort_by_key(|(tag, _)| *tag);
        for (tag, bytes) in sorted {
            records.extend_from_slice(tag);
            records.extend_from_slice(&checksum(bytes).to_be_bytes());
            records.extend_from_slice(&u32::try_from(offset).unwrap_or(0).to_be_bytes());
            records.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(0).to_be_bytes());
            body.extend_from_slice(bytes);
            while body.len() % 4 != 0 {
                body.push(0);
            }
            offset = 12 + 16 * tables.len() + body.len();
        }
        out.extend_from_slice(&records);
        out.extend_from_slice(&body);
        // head.checkSumAdjustment makes the whole file sum to 0xB1B0AFBA.
        let adjustment = 0xB1B0_AFBA_u32.wrapping_sub(checksum(&out));
        let head = tables.iter().position(|(tag, _)| tag == b"head");
        if let Some(head) = head {
            let mut sorted_tags: Vec<[u8; 4]> = tables.iter().map(|(tag, _)| *tag).collect();
            sorted_tags.sort_unstable();
            let index = sorted_tags
                .iter()
                .position(|tag| tag == &tables[head].0)
                .unwrap_or(0);
            let record = 12 + 16 * index;
            let start = u32_at(&out, record + 8).and_then(|o| usize::try_from(o).ok());
            if let Some(start) = start
                && let Some(slot) = out.get_mut(start + 8..start + 12)
            {
                slot.copy_from_slice(&adjustment.to_be_bytes());
            }
        }
        out
    }
}

/// A program with only what width handling reads: head, hhea, maxp, hmtx (two full metrics for
/// three glyphs, 2048 units per em), and a table of another kind to carry over.
#[cfg(test)]
pub(crate) fn test_program() -> Vec<u8> {
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&2048u16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
    let mut maxp = vec![0, 0, 0x50, 0];
    maxp.extend_from_slice(&3u16.to_be_bytes());
    let mut hmtx = Vec::new();
    for (advance, bearing) in [(1024u16, 10i16), (2048, 20)] {
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&bearing.to_be_bytes());
    }
    hmtx.extend_from_slice(&30i16.to_be_bytes());
    test_sfnt(&[
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"maxp", maxp),
        (*b"zzzz", b"untouched".to_vec()),
    ])
}

/// An SFNT file holding `tables`, sorted by tag.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) fn test_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&u16::try_from(tables.len()).unwrap().to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, bytes) in tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(bytes).to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
        body.extend_from_slice(bytes);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() + body.len();
    }
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{TrueType, checksum, test_program as program, u32_at};

    #[test]
    fn widths_read_and_change() {
        let data = program();
        let font = TrueType::parse(&data).unwrap();
        assert_eq!(font.glyph_count(), 3);
        assert_eq!(font.width(0), Some(500.0));
        // The third glyph shares the last full metric.
        assert_eq!(font.width(2), Some(1000.0));
        let copy = font.with_widths(&[(2, 250.0), (0, 986.0)]).unwrap();
        let again = TrueType::parse(&copy).unwrap();
        assert_eq!(again.width(1), Some(1000.0));
        assert_eq!(again.width(2), Some(250.0));
        // 986 is 2019.3 font units, 2019 once rounded: within PDF/A's tolerance of 1.
        assert!((again.width(0).unwrap() - 986.0).abs() < 0.5);
        assert_eq!(again.bearings, vec![10, 20, 30]);
        // The other tables are carried over, and the whole file sums to the magic number.
        assert!(copy.windows(9).any(|w| w == b"untouched"));
        assert_eq!(checksum(&copy), 0xB1B0_AFBA);
        assert!(u32_at(&copy, 0) == Some(0x0001_0000));
    }

    #[test]
    fn damage_is_refused_not_panicked_on() {
        let data = program();
        for cut in [0, 4, 12, 40, data.len() - 16] {
            assert!(TrueType::parse(&data[..cut]).is_none(), "{cut}");
        }
        let mut flipped = data.clone();
        for i in (0..flipped.len()).step_by(7) {
            flipped[i] ^= 0xA5;
            if let Some(font) = TrueType::parse(&flipped) {
                let _ = font.with_widths(&[(0, 100.0), (1, 70000.0)]);
            }
        }
    }
}
