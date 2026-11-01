//! Synthetic font faces: a narrow face and a wide face, built byte by byte.
//!
//! The faces carry no game data and no third-party font: every table is
//! written here, so a test that needs "a face without this glyph" and "a
//! face with this glyph" has exactly that, on every host.
//!
//! The two faces differ in three ways a test can observe:
//!
//! * their Latin glyphs have different advance widths (narrow vs wide),
//! * only the wide face carries the CJK code point [`WIDE_ONLY_CODEPOINT`],
//! * each face answers to two family names, one in English and one in
//!   Japanese, so a lookup by either name finds it.

/// The em size of the synthetic faces, in design units.
const UNITS_PER_EM: u16 = 1000;

/// The ascender of the synthetic faces, in design units.
const ASCENDER: i16 = 800;

/// The descender of the synthetic faces, in design units.
const DESCENDER: i16 = -200;

/// The code point only the wide face carries.
///
/// It is a CJK ideograph, so a face that maps it can stand in for a
/// Japanese-capable face.
pub const WIDE_ONLY_CODEPOINT: char = '\u{4E00}';

/// The family names of the narrow face.
///
/// The first is the English name, the second the Japanese one; both are
/// family records on the Microsoft platform.
pub const NARROW_NAMES: [&str; 2] = ["Krkr Test Narrow", "クリークテスト細"];

/// The family names of the wide face.
pub const WIDE_NAMES: [&str; 2] = ["Krkr Test Wide", "クリークテスト太"];

/// One glyph of a synthetic face: its code point, its advance width and the
/// rectangle it outlines.
#[derive(Clone)]
struct Glyph {
    /// The code point the glyph is mapped from (`0` for `.notdef`).
    codepoint: u32,
    /// The advance width, in design units.
    advance: u16,
    /// The outlined rectangle, in design units.
    bounds: (i16, i16, i16, i16),
}

/// Builds the narrow face: Latin glyphs only, no CJK glyph.
pub fn narrow_face() -> Vec<u8> {
    build_face(
        &NARROW_NAMES,
        &[
            Glyph {
                codepoint: 'A' as u32,
                advance: 400,
                bounds: (40, 0, 360, 700),
            },
            Glyph {
                codepoint: 'B' as u32,
                advance: 500,
                bounds: (40, 0, 460, 700),
            },
        ],
    )
}

/// Builds the wide face: wider Latin glyphs and one CJK glyph.
pub fn wide_face() -> Vec<u8> {
    build_face(
        &WIDE_NAMES,
        &[
            Glyph {
                codepoint: 'A' as u32,
                advance: 800,
                bounds: (40, 0, 760, 700),
            },
            Glyph {
                codepoint: 'B' as u32,
                advance: 900,
                bounds: (40, 0, 860, 700),
            },
            Glyph {
                codepoint: WIDE_ONLY_CODEPOINT as u32,
                advance: 900,
                bounds: (20, 0, 880, 700),
            },
        ],
    )
}

/// Builds a face with the given family names and glyphs.
///
/// The face is a TrueType font with the tables a rasterizer needs: `head`,
/// `hhea`, `maxp`, `hmtx`, `cmap`, `glyf`, `loca`, `name`, `OS/2` and `post`.
fn build_face(names: &[&str], glyphs: &[Glyph]) -> Vec<u8> {
    // Glyph 0 is `.notdef`: the face's own box, which no character maps to.
    let mut all: Vec<Glyph> = vec![Glyph {
        codepoint: 0,
        advance: 500,
        bounds: (50, 0, 450, 700),
    }];
    all.extend_from_slice(glyphs);

    let (glyf, loca) = glyf_and_loca(&all);
    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"OS/2", os2(&all)),
        (b"cmap", cmap(&all)),
        (b"glyf", glyf),
        (b"head", head(&all)),
        (b"hhea", hhea(&all)),
        (b"hmtx", hmtx(&all)),
        (b"loca", loca),
        (b"maxp", maxp(&all)),
        (b"name", name_table(names)),
        (b"post", post()),
    ];
    assemble(&tables)
}

/// The glyph outlines and the offsets that locate them.
fn glyf_and_loca(glyphs: &[Glyph]) -> (Vec<u8>, Vec<u8>) {
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for glyph in glyphs {
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        glyf.extend_from_slice(&simple_glyph(glyph));
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    (glyf, loca)
}

/// One simple glyph: a closed rectangular contour.
fn simple_glyph(glyph: &Glyph) -> Vec<u8> {
    let (x0, y0, x1, y1) = glyph.bounds;
    let mut out = Vec::new();
    out.extend_from_slice(&1i16.to_be_bytes());
    out.extend_from_slice(&x0.to_be_bytes());
    out.extend_from_slice(&y0.to_be_bytes());
    out.extend_from_slice(&x1.to_be_bytes());
    out.extend_from_slice(&y1.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    // Four on-curve points, each with a 16-bit delta.
    out.extend_from_slice(&[0x01; 4]);
    for x in [x0, 0, x1 - x0, 0] {
        out.extend_from_slice(&x.to_be_bytes());
    }
    for y in [y0, y1 - y0, 0, -(y1 - y0)] {
        out.extend_from_slice(&y.to_be_bytes());
    }
    out
}

/// The `head` table: the units per em, the glyph bounds and the long
/// `loca` format.
fn head(glyphs: &[Glyph]) -> Vec<u8> {
    let mut x_min = i16::MAX;
    let mut y_min = i16::MAX;
    let mut x_max = i16::MIN;
    let mut y_max = i16::MIN;
    for glyph in glyphs {
        let (x0, y0, x1, y1) = glyph.bounds;
        x_min = x_min.min(x0);
        y_min = y_min.min(y0);
        x_max = x_max.max(x1);
        y_max = y_max.max(y1);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0x5F0F3CF5u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&UNITS_PER_EM.to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&x_min.to_be_bytes());
    out.extend_from_slice(&y_min.to_be_bytes());
    out.extend_from_slice(&x_max.to_be_bytes());
    out.extend_from_slice(&y_max.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&2i16.to_be_bytes());
    out.extend_from_slice(&1i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out
}

/// The `hhea` table: the vertical metrics and the horizontal metric count.
fn hhea(glyphs: &[Glyph]) -> Vec<u8> {
    let advance_max = glyphs
        .iter()
        .map(|glyph| glyph.advance)
        .max()
        .unwrap_or_default();
    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&ASCENDER.to_be_bytes());
    out.extend_from_slice(&DESCENDER.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&advance_max.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&1i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    for _ in 0..4 {
        out.extend_from_slice(&0i16.to_be_bytes());
    }
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    out
}

/// The `maxp` table: the glyph count and the limits the outlines stay under.
fn maxp(glyphs: &[Glyph]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out
}

/// The `hmtx` table: one advance and one left side bearing per glyph.
fn hmtx(glyphs: &[Glyph]) -> Vec<u8> {
    let mut out = Vec::new();
    for glyph in glyphs {
        out.extend_from_slice(&glyph.advance.to_be_bytes());
        out.extend_from_slice(&glyph.bounds.0.to_be_bytes());
    }
    out
}

/// The `cmap` table: a format 4 subtable with one segment per code point.
///
/// The table carries the subtable through one encoding record: the Windows
/// platform's Unicode encoding.
fn cmap(glyphs: &[Glyph]) -> Vec<u8> {
    let subtable = cmap_subtable(glyphs);
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&subtable);
    out
}

/// The format 4 character map: one segment per code point.
fn cmap_subtable(glyphs: &[Glyph]) -> Vec<u8> {
    let mut segments: Vec<(u32, u16)> = Vec::new();
    for (index, glyph) in glyphs.iter().enumerate() {
        if glyph.codepoint == 0 {
            continue;
        }
        segments.push((glyph.codepoint, index as u16));
    }
    let seg_count = segments.len() + 1;
    let seg_count_x2 = (seg_count * 2) as u16;
    let entry_selector = seg_count.ilog2() as u16;
    let search_range = 2u16 << entry_selector;
    let range_shift = seg_count_x2 - search_range;

    let mut out = Vec::new();
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&seg_count_x2.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    for (codepoint, _) in &segments {
        out.extend_from_slice(&(*codepoint as u16).to_be_bytes());
    }
    out.extend_from_slice(&0xFFFFu16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for (codepoint, _) in &segments {
        out.extend_from_slice(&(*codepoint as u16).to_be_bytes());
    }
    out.extend_from_slice(&0xFFFFu16.to_be_bytes());
    for (codepoint, glyph) in &segments {
        let delta = (*glyph as i32 - *codepoint as i32) as i16;
        out.extend_from_slice(&delta.to_be_bytes());
    }
    out.extend_from_slice(&1i16.to_be_bytes());
    for _ in 0..seg_count {
        out.extend_from_slice(&0u16.to_be_bytes());
    }
    let length = out.len() as u16;
    out[2..4].copy_from_slice(&length.to_be_bytes());
    out
}

/// The `name` table: the family names, each in an English and a Japanese
/// record on the Microsoft platform.
fn name_table(names: &[&str]) -> Vec<u8> {
    let mut records: Vec<(u16, u16, u16, u16, u16, u16)> = Vec::new();
    let mut storage: Vec<u8> = Vec::new();
    for name in names {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut encoded = Vec::new();
        for unit in units {
            encoded.extend_from_slice(&unit.to_be_bytes());
        }
        for language in [0x0409u16, 0x0411u16] {
            let offset = storage.len() as u16;
            records.push((3, 1, language, 1, encoded.len() as u16, offset));
            storage.extend_from_slice(&encoded);
        }
    }
    // The shaping database requires a post-script name (name id 6) with a
    // supported encoding and skips a face that carries none.
    let post_script_name = "KrkrFace";
    let post_script: Vec<u8> = post_script_name
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    let offset = storage.len() as u16;
    records.push((3, 1, 0x0409, 6, post_script.len() as u16, offset));
    storage.extend_from_slice(&post_script);
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    out.extend_from_slice(&((6 + records.len() * 12) as u16).to_be_bytes());
    for (platform, encoding, language, name_id, length, offset) in &records {
        out.extend_from_slice(&platform.to_be_bytes());
        out.extend_from_slice(&encoding.to_be_bytes());
        out.extend_from_slice(&language.to_be_bytes());
        out.extend_from_slice(&name_id.to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
    }
    out.extend_from_slice(&storage);
    out
}

/// The `OS/2` table: the vertical metrics and the covered code points.
fn os2(glyphs: &[Glyph]) -> Vec<u8> {
    let mut first = u16::MAX;
    let mut last = 0u16;
    for glyph in glyphs {
        if glyph.codepoint == 0 || glyph.codepoint > u32::from(u16::MAX) {
            continue;
        }
        let codepoint = glyph.codepoint as u16;
        first = first.min(codepoint);
        last = last.max(codepoint);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&500i16.to_be_bytes());
    out.extend_from_slice(&400u16.to_be_bytes());
    out.extend_from_slice(&5u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for _ in 0..8 {
        out.extend_from_slice(&0i16.to_be_bytes());
    }
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&[0u8; 10]);
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(b"krkr");
    out.extend_from_slice(&0x40u16.to_be_bytes());
    out.extend_from_slice(&first.to_be_bytes());
    out.extend_from_slice(&last.to_be_bytes());
    out.extend_from_slice(&ASCENDER.to_be_bytes());
    out.extend_from_slice(&DESCENDER.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&(ASCENDER as u16).to_be_bytes());
    out.extend_from_slice(&(DESCENDER.unsigned_abs()).to_be_bytes());
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&500i16.to_be_bytes());
    out.extend_from_slice(&700i16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&32u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}

/// The `post` table: version 3, which carries no glyph names.
fn post() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0x00030000u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out
}

/// Writes the table directory and the tables themselves.
fn assemble(tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let num_tables = tables.len() as u16;
    let entry_selector = num_tables.ilog2() as u16;
    let search_range = 16u16 << entry_selector;
    let range_shift = num_tables * 16 - search_range;

    let directory_size = 12 + tables.len() * 16;
    let mut offsets = Vec::new();
    let mut data = Vec::new();
    for (_, table) in tables {
        while data.len() % 4 != 0 {
            data.push(0);
        }
        offsets.push(directory_size + data.len());
        data.extend_from_slice(table);
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    for ((tag, table), offset) in tables.iter().zip(&offsets) {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&checksum(table).to_be_bytes());
        out.extend_from_slice(&(*offset as u32).to_be_bytes());
        out.extend_from_slice(&(table.len() as u32).to_be_bytes());
    }
    out.extend_from_slice(&data);
    out
}

/// The checksum of a table: the sum of its 32-bit words.
fn checksum(table: &[u8]) -> u32 {
    let mut sum = 0u32;
    let (words, rest) = table.as_chunks::<4>();
    for word in words {
        sum = sum.wrapping_add(u32::from_be_bytes(*word));
    }
    if !rest.is_empty() {
        let mut last = [0u8; 4];
        last[..rest.len()].copy_from_slice(rest);
        sum = sum.wrapping_add(u32::from_be_bytes(last));
    }
    sum
}
