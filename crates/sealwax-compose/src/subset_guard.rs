//! What subsetter 0.2.6 panics on, found before it runs. A panic is caught where panics unwind,
//! but in WebAssembly, where they abort, it stops the module. typst/subsetter#42 turns both cases
//! into errors: remove this module once a release has it.
//!
//! The TrueType check reads the program as subsetter 0.2.6 does (its table lookup, `loca` and
//! components), not as `read-fonts` does: on a malformed program the two can disagree, and only
//! what subsetter reads makes it panic.

use read_fonts::ps::cff::dict::{self, Entry};
use read_fonts::tables::cff::Cff;
use read_fonts::types::Tag;
use read_fonts::{FontData, FontRead as _, FontRef};

/// Whether subsetter 0.2.6 can cut face `face` of `data` to `glyphs` (`.notdef` among them)
/// without panicking. `false` also for some programs it would refuse anyway.
pub(crate) fn can_cut(data: &[u8], face: u32, glyphs: &[u16]) -> bool {
    let Ok(font) = FontRef::from_index(data, face) else {
        return false;
    };
    let table = |tag: &[u8; 4]| table(&font, data, Tag::new(tag));
    closure_fits(table(b"head"), table(b"loca"), table(b"glyf"), glyphs)
        && font_dicts_fit(table(b"CFF "))
}

/// Table `tag` as subsetter 0.2.6 finds it: by a binary search of the table records, sorted or
/// not.
fn table<'a>(font: &FontRef<'a>, data: &'a [u8], tag: Tag) -> Option<&'a [u8]> {
    let records = font.table_directory.table_records();
    let index = records
        .binary_search_by(|record| record.tag().cmp(&tag))
        .ok()?;
    let record = records.get(index)?;
    let start = usize::try_from(record.offset()).ok()?;
    let end = start.checked_add(usize::try_from(record.length()).ok()?)?;
    data.get(start..end)
}

/// Whether the closure of `glyphs` through composite glyphs stays under 65,536 glyphs: the glyph
/// remapper of subsetter 0.2.6 panics on the 65,536th (typst/subsetter#35). Components can name
/// glyphs the program lacks, and every one counts.
fn closure_fits(
    head: Option<&[u8]>,
    loca: Option<&[u8]>,
    glyf: Option<&[u8]>,
    glyphs: &[u16],
) -> bool {
    // Without these tables, subsetter forms no closure.
    let (Some(head), Some(loca), Some(glyf)) = (head, loca, glyf) else {
        return true;
    };
    let Some(long) = be_i16(head, 50).map(|format| format != 0) else {
        return true;
    };
    let offset = |glyph: usize| {
        if long {
            usize::try_from(be_u32(loca, glyph.checked_mul(4)?)?).ok()
        } else {
            be_u16(loca, glyph.checked_mul(2)?).map(|offset| 2 * usize::from(offset))
        }
    };
    let glyph_data = |glyph: u16| {
        let glyph = usize::from(glyph);
        glyf.get(offset(glyph)?..offset(glyph + 1)?)
    };

    let mut seen = vec![false; 1 << 16];
    let mut count = 0;
    let mut pending = Vec::new();
    for &glyph in glyphs {
        if !seen[usize::from(glyph)] {
            seen[usize::from(glyph)] = true;
            count += 1;
            pending.push(glyph);
        }
    }
    while let Some(glyph) = pending.pop() {
        let Some(data) = glyph_data(glyph) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        // subsetter refuses a glyph whose header it cannot read.
        let Some(contours) = be_i16(data, 0) else {
            return false;
        };
        if contours >= 0 {
            continue;
        }
        let Some(components) = data.get(10..) else {
            return false;
        };
        for component in components_of(components) {
            if !seen[usize::from(component)] {
                if count == usize::from(u16::MAX) {
                    return false;
                }
                seen[usize::from(component)] = true;
                count += 1;
                pending.push(component);
            }
        }
    }
    true
}

/// The glyphs the components of a composite glyph name (its data past the header), read as
/// subsetter 0.2.6 reads them: arguments and scales are skipped by their flags, and the list ends
/// where a component's flags or glyph cannot be read.
fn components_of(data: &[u8]) -> impl Iterator<Item = u16> + '_ {
    const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
    const WE_HAVE_A_SCALE: u16 = 0x0008;
    const MORE_COMPONENTS: u16 = 0x0020;
    const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
    const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
    let mut at = 0usize;
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let flags = be_u16(data, at)?;
        let glyph = be_u16(data, at.checked_add(2)?)?;
        let arguments = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        let scale = if flags & WE_HAVE_A_SCALE != 0 {
            2
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            4
        } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            8
        } else {
            0
        };
        at = at.saturating_add(4 + arguments + scale);
        done = flags & MORE_COMPONENTS == 0;
        Some(glyph)
    })
}

/// Whether the program has at most 255 font dictionaries, or is no CID-keyed CFF program.
/// subsetter 0.2.6 counts the dictionaries in a byte: from 256 it asserts in a debug build, and
/// its remapper overflows when a subset uses them all (typst/subsetter#34). A CFF table that
/// `read-fonts` cannot read is refused rather than guessed at.
fn font_dicts_fit(cff: Option<&[u8]>) -> bool {
    let Some(cff) = cff else {
        return true;
    };
    let Ok(table) = Cff::read(FontData::new(cff)) else {
        return false;
    };
    let Some(top) = table.top_dicts().get(0) else {
        return false;
    };
    for entry in dict::entries(top, None) {
        match entry {
            Err(_) => return false,
            Ok(Entry::FdArrayOffset(offset)) if be_u16(cff, offset).is_some_and(|n| n > 255) => {
                return false;
            }
            Ok(_) => {}
        }
    }
    true
}

fn be_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn be_i16(data: &[u8], at: usize) -> Option<i16> {
    Some(i16::from_be_bytes(
        data.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{can_cut, closure_fits, font_dicts_fit};
    use read_fonts::FontRef;
    use read_fonts::types::Tag;

    /// `head`, `loca` (long) and `glyf` of a program whose glyph 1 is a composite glyph naming
    /// the glyphs from 2 to `last`, glyph 0 empty, the others lacking.
    fn fan(last: u16) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut head = vec![0; 54];
        head[50..52].copy_from_slice(&1i16.to_be_bytes());
        let mut glyf = Vec::new();
        for value in [-1i16, 0, 0, 0, 0] {
            glyf.extend_from_slice(&value.to_be_bytes());
        }
        for component in 2..=last {
            let flags: u16 = if component < last { 0x0022 } else { 0x0002 };
            glyf.extend_from_slice(&flags.to_be_bytes());
            glyf.extend_from_slice(&component.to_be_bytes());
            glyf.extend_from_slice(&[0, 0]);
        }
        let end = u32::try_from(glyf.len()).unwrap();
        let loca = [0u32, 0, end]
            .iter()
            .flat_map(|o| o.to_be_bytes())
            .collect();
        (head, loca, glyf)
    }

    fn fits(tables: &(Vec<u8>, Vec<u8>, Vec<u8>), glyphs: &[u16]) -> bool {
        let (head, loca, glyf) = tables;
        closure_fits(Some(head), Some(loca), Some(glyf), glyphs)
    }

    #[test]
    fn the_closure_is_refused_from_its_65536th_glyph() {
        // .notdef, glyph 1 and its components: 65,535 glyphs, then 65,536.
        assert!(fits(&fan(65_534), &[0, 1]));
        assert!(!fits(&fan(65_535), &[0, 1]));
        // The composite glyph not drawn: nothing to follow.
        assert!(fits(&fan(65_535), &[0]));
        // A glyph drawn that a component also names counts once.
        assert!(fits(&fan(65_534), &[0, 1, 2, 65_534]));
    }

    #[test]
    fn cff_programs_are_refused_from_256_font_dictionaries() {
        let cff = |program: &'static [u8]| {
            let font = FontRef::new(program).unwrap();
            font.table_data(Tag::new(b"CFF ")).unwrap().as_bytes()
        };
        assert!(!font_dicts_fit(Some(cff(include_bytes!(
            "../tests/data/font-dicts.otf"
        )))));
        // No font dictionaries (not CID-keyed), or no CFF table.
        assert!(font_dicts_fit(Some(cff(include_bytes!(
            "../tests/data/boxes-cff.otf"
        )))));
        assert!(font_dicts_fit(None));
        assert!(!font_dicts_fit(Some(b"not a CFF table")));
    }

    #[test]
    fn programs_of_a_collection_are_found() {
        let collection = include_bytes!("../tests/data/boxes.ttc");
        assert!(can_cut(collection, 0, &[0, 1]) && can_cut(collection, 1, &[0, 1]));
        assert!(!can_cut(collection, 2, &[0, 1]));
    }
}
