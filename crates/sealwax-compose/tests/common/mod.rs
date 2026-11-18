//! A TrueType program made for the tests (as `sealwax-annots/tests/fonts.rs` makes one).

#![allow(dead_code, clippy::unwrap_used)]

fn be16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn be32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// A TrueType program named `name` whose glyph `i + 1` is a box of width `500 + 10 i` for
/// `characters[i]`, glyph 0 its `.notdef`. Advances differ, so that widths tell glyphs apart.
pub fn program(name: &str, characters: &[char]) -> Vec<u8> {
    program_with(name, characters, false, false)
}

/// [`program`], its table directory also holding a record (`zzzz`) that points far past the
/// end of the file, as some fonts Word embeds have.
pub fn program_with_bad_record(name: &str, characters: &[char]) -> Vec<u8> {
    program_with(name, characters, true, false)
}

/// [`program`], its name record pointing past the names: the program reads (without its name),
/// the subsetter refuses to cut it.
pub fn program_with_bad_name(characters: &[char]) -> Vec<u8> {
    program_with("Test", characters, false, true)
}

fn program_with(name: &str, characters: &[char], bad_record: bool, bad_name: bool) -> Vec<u8> {
    let mut unique: Vec<char> = Vec::new();
    for c in characters {
        if !unique.contains(c) {
            unique.push(*c);
        }
    }
    let characters = unique.as_slice();
    let count = u16::try_from(characters.len() + 1).unwrap();
    let advance = |glyph: u16| {
        if glyph == 0 {
            500
        } else {
            500 + 10 * (glyph - 1)
        }
    };
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for glyph in 0..count {
        be32(&mut loca, u32::try_from(glyf.len()).unwrap());
        let width = advance(glyph) - 100;
        // One contour, a box from (50, 0) to (50 + width, 700), corners on the curve.
        let mut g = Vec::new();
        for value in [1u16, 50, 0, 50 + width, 700, 3, 0] {
            be16(&mut g, value);
        }
        g.extend_from_slice(&[0x01; 4]);
        for dx in [
            50i16,
            i16::try_from(width).unwrap(),
            0,
            -i16::try_from(width).unwrap(),
        ] {
            g.extend_from_slice(&dx.to_be_bytes());
        }
        for dy in [0i16, 0, 700, 0] {
            g.extend_from_slice(&dy.to_be_bytes());
        }
        while g.len() % 4 != 0 {
            g.push(0);
        }
        glyf.extend_from_slice(&g);
    }
    be32(&mut loca, u32::try_from(glyf.len()).unwrap());

    let mut head = Vec::new();
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0);
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, 0);
    be16(&mut head, 1000);
    head.extend_from_slice(&[0; 16]);
    for value in [0i16, -200, 1000, 800] {
        head.extend_from_slice(&value.to_be_bytes());
    }
    be16(&mut head, 0);
    be16(&mut head, 8);
    be16(&mut head, 2);
    be16(&mut head, 1);
    be16(&mut head, 0);

    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    for value in [800i16, -200, 0] {
        hhea.extend_from_slice(&value.to_be_bytes());
    }
    be16(&mut hhea, advance(count - 1));
    hhea.extend_from_slice(&[0; 22]);
    be16(&mut hhea, count);

    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0000_5000);
    be16(&mut maxp, count);

    let mut hmtx = Vec::new();
    for glyph in 0..count {
        be16(&mut hmtx, advance(glyph));
        be16(&mut hmtx, 50);
    }

    // cmap format 4: a segment for each character, then the closing one.
    let mut codes: Vec<(u16, u16)> = characters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            (
                u16::try_from(u32::from(*c)).unwrap(),
                u16::try_from(i + 1).unwrap(),
            )
        })
        .collect();
    codes.sort_unstable();
    let segments = u16::try_from(codes.len() + 1).unwrap();
    let mut sub = Vec::new();
    be16(&mut sub, 4);
    be16(&mut sub, 16 + 8 * segments);
    be16(&mut sub, 0);
    be16(&mut sub, 2 * segments);
    let power = 15 - segments.leading_zeros() as u16;
    be16(&mut sub, 2 * (1 << power));
    be16(&mut sub, power);
    be16(&mut sub, 2 * segments - 2 * (1 << power));
    for (code, _) in &codes {
        be16(&mut sub, *code);
    }
    be16(&mut sub, 0xFFFF);
    be16(&mut sub, 0);
    for (code, _) in &codes {
        be16(&mut sub, *code);
    }
    be16(&mut sub, 0xFFFF);
    for (code, glyph) in &codes {
        be16(&mut sub, glyph.wrapping_sub(*code));
    }
    be16(&mut sub, 1);
    for _ in 0..segments {
        be16(&mut sub, 0);
    }
    let mut cmap = Vec::new();
    be16(&mut cmap, 0);
    be16(&mut cmap, 1);
    be16(&mut cmap, 3);
    be16(&mut cmap, 1);
    be32(&mut cmap, 12);
    cmap.extend_from_slice(&sub);

    // name: the PostScript name (6), Windows Unicode.
    let encoded: Vec<u8> = name.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut name_table = Vec::new();
    for value in [
        0u16,
        1,
        18,
        3,
        1,
        0x409,
        6,
        u16::try_from(encoded.len()).unwrap(),
        if bad_name { 0x4000 } else { 0 },
    ] {
        be16(&mut name_table, value);
    }
    name_table.extend_from_slice(&encoded);

    let mut post = Vec::new();
    be32(&mut post, 0x0003_0000);
    post.extend_from_slice(&[0; 28]);

    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
        (b"name", name_table),
        (b"post", post),
    ];
    let records = tables.len() + usize::from(bad_record);
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, u16::try_from(records).unwrap());
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * records;
    let mut body = Vec::new();
    for (tag, table) in &tables {
        out.extend_from_slice(*tag);
        be32(&mut out, 0);
        be32(&mut out, u32::try_from(offset).unwrap());
        be32(&mut out, u32::try_from(table.len()).unwrap());
        body.extend_from_slice(table);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * records + body.len();
    }
    if bad_record {
        out.extend_from_slice(b"zzzz");
        be32(&mut out, 0);
        be32(&mut out, 0x5555_0000);
        be32(&mut out, 0x5555_0000);
    }
    out.extend_from_slice(&body);
    out
}
