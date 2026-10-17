use rightkit_typeset::{font, FontError, FontFace, Measurement};

fn be16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn be32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

// Exercise both idDelta & idRangeOffset, including delta after glyph-array lookup.
fn format4() -> Vec<u8> {
    let mut out = Vec::new();
    for value in [
        4, 42, 0, 6, 4, 1, 2, 0x4D, 0x69, 0xFFFF, 0, 0x4D, 0x69, 0xFFFF,
    ] {
        be16(&mut out, value);
    }
    for value in [1u16.wrapping_sub(0x4D), 1, 1, 0, 4, 0, 1] {
        be16(&mut out, value);
    }
    out
}

fn format12() -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 12);
    be16(&mut out, 0);
    be32(&mut out, 52);
    be32(&mut out, 0);
    be32(&mut out, 3);
    for (code, glyph) in [(0x4D, 1), (0x69, 2), (0x1F984, 3)] {
        be32(&mut out, code);
        be32(&mut out, code);
        be32(&mut out, glyph);
    }
    out
}

fn cmap(subtables: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 0);
    be16(&mut out, subtables.len() as u16);
    let mut offset = 4 + 8 * subtables.len();
    for sub in subtables {
        be16(&mut out, 3);
        be16(&mut out, if sub[1] == 12 { 10 } else { 1 });
        be32(&mut out, offset as u32);
        offset += sub.len();
    }
    for sub in subtables {
        out.extend_from_slice(sub);
    }
    out
}

fn sfnt(cmap: Vec<u8>, advances: &[u16], glyphs: u16) -> Vec<u8> {
    let mut head = vec![0; 54];
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    let mut hhea = vec![0; 36];
    hhea[34..36].copy_from_slice(&(advances.len() as u16).to_be_bytes());
    let mut hmtx = Vec::new();
    for &advance in advances {
        be16(&mut hmtx, advance);
        be16(&mut hmtx, 0);
    }
    for _ in advances.len()..usize::from(glyphs) {
        be16(&mut hmtx, 0); // Remaining glyphs store only left side bearings.
    }
    let mut maxp = vec![0; 6];
    maxp[4..6].copy_from_slice(&glyphs.to_be_bytes());
    let tables = [
        (b"cmap", cmap),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"maxp", maxp),
    ];
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, tables.len() as u16);
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, body) in &tables {
        out.extend_from_slice(*tag);
        be32(&mut out, 0);
        be32(&mut out, offset as u32);
        be32(&mut out, body.len() as u32);
        offset += body.len();
    }
    for (_, body) in tables {
        out.extend_from_slice(&body);
    }
    out
}

#[test]
fn public_api_measures_proportional_text_and_unique_missing_characters() {
    let face: font::FontFace =
        FontFace::parse(sfnt(cmap(&[format4()]), &[500, 900, 250], 3)).unwrap();
    assert_eq!(face.units_per_em(), 1000);
    assert_eq!((face.glyph_id('M'), face.glyph_id('i')), (1, 2));
    let measured: Measurement = face.measure("Mi\n\t\rx🦄x🦄").unwrap();
    assert_eq!(measured.advance_units, 3150);
    assert_eq!(measured.missing, vec!['x', '🦄']);
    assert_eq!(measured.width_px(100), 315);
    assert_eq!(face.measure("").unwrap().advance_units, 0);
    assert!(face.measure("\n\t\0").unwrap().missing.is_empty());
}

#[test]
fn full_unicode_measurement_reuses_final_hmetric() {
    let face = FontFace::parse(sfnt(cmap(&[format12()]), &[500, 750], 4)).unwrap();
    assert_eq!(face.glyph_id('🦄'), 3);
    assert_eq!(face.advance_units(3).unwrap(), 750);
    assert_eq!(face.advance_units(4).unwrap(), 0);
    let measured = face.measure("Mi🦄").unwrap();
    assert_eq!(measured.advance_units, 2250);
    assert!(measured.missing.is_empty());
    assert_eq!(measured.width_px(10), 23);
}

#[test]
fn full_unicode_cmap_is_preferred_regardless_of_record_order() {
    let mut full = format12();
    full[24..28].copy_from_slice(&2u32.to_be_bytes()); // M maps to narrow glyph 2.
    for subtables in [vec![format4(), full.clone()], vec![full, format4()]] {
        let face = FontFace::parse(sfnt(cmap(&subtables), &[500, 900, 250], 4)).unwrap();
        assert_eq!(face.glyph_id('M'), 2);
        assert_eq!(face.measure("M").unwrap().advance_units, 250);
    }
}

#[test]
fn collection_reads_first_face_with_file_relative_table_offsets() {
    let mut bytes = sfnt(cmap(&[format12()]), &[500, 900, 250], 4);
    for index in 0..5 {
        let at = 12 + index * 16 + 8;
        let offset = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        bytes[at..at + 4].copy_from_slice(&(offset + 20).to_be_bytes());
    }
    let mut collection = Vec::new();
    for value in [0x7474_6366, 0x0001_0000, 2, 20, 0] {
        be32(&mut collection, value);
    }
    collection.extend_from_slice(&bytes);
    let face = FontFace::parse(collection).unwrap();
    assert_eq!(face.clone().measure("Mi🦄").unwrap().advance_units, 1400);
}

#[test]
fn public_errors_identify_malformed_missing_and_unsupported_fonts() {
    assert!(matches!(
        FontFace::parse(vec![]),
        Err(FontError::Malformed(_))
    ));
    let mut bytes = sfnt(cmap(&[format4()]), &[500, 900, 250], 3);
    bytes[12..16].copy_from_slice(b"xxxx");
    assert_eq!(
        FontFace::parse(bytes).unwrap_err(),
        FontError::MissingTable("cmap")
    );
    let unsupported = sfnt(cmap(&[vec![0, 0, 0, 6, 0, 0]]), &[500], 1);
    assert_eq!(
        FontFace::parse(unsupported).unwrap_err(),
        FontError::NoUsableCmap
    );
    assert_eq!(
        FontError::MissingTable("head").to_string(),
        "font has no head table"
    );
}

#[test]
fn pixel_scaling_rounds_once_after_summing_advances() {
    let measured = Measurement {
        units_per_em: 1000,
        advance_units: 250,
        missing: vec![],
    };
    assert_eq!(measured.width_px(1), 0);
    assert_eq!(measured.width_px(2), 1);
    assert_eq!(measured.width_px(0), 0);
    let face = FontFace::parse(sfnt(cmap(&[format4()]), &[500, 900, 250], 3)).unwrap();
    assert_eq!(face.measure("ii").unwrap().width_px(1), 1);
}
