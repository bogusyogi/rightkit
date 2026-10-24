// rightkit-typeset 0.1.0 consumed by exact registry pin (default features: no LaTeX input, no PNG
// output, no embedded fonts), outside the workspace.
//
// Proves, with no font file and no host resource: the Typst request preamble and body wrapping; the
// ppi fallback; the refusals (no fonts, unreadable font bytes); font metric reading (FontFace parse,
// glyph ids, advances, measurement with missing characters, pixel scaling, cmap formats 4 and 12,
// the first face of a collection, and each FontError); and a real Typst compile and SVG render of
// markup and math through a font that this program builds byte by byte (a 96-glyph TrueType face
// whose letter A is a box), including the compile errors for file access, package fetching and bad
// syntax.
// Public API only.
// Does not prove: rendering with a real typeface, the `latex`, `png` and `embedded-fonts` features,
// kerning or shaping quality, or any font bundled with an app.
// State: in-memory values only. No network, no files, no process spawn, no user data.
use rightkit_typeset::{
    font, FontError, FontFace, FontSource, Measurement, Output, Rendered, Request, Source,
    TypesetError, Typesetter,
};

fn be16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn be32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Assemble an sfnt file (TrueType outlines) from tables; the directory is sorted by tag and each
/// table is padded to four bytes.
fn sfnt(mut tables: Vec<(&'static [u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|(tag, _)| **tag);
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
        offset += body.len().next_multiple_of(4);
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    out
}

fn format4() -> Vec<u8> {
    // Exercises idDelta and idRangeOffset: M -> glyph 1, i -> glyph 2.
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

/// A metrics-only face: head, hhea, hmtx, maxp and a cmap, enough for `FontFace`.
fn metrics_font(cmap: Vec<u8>, advances: &[u16], glyphs: u16) -> Vec<u8> {
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
        be16(&mut hmtx, 0);
    }
    let mut maxp = vec![0; 6];
    maxp[4..6].copy_from_slice(&glyphs.to_be_bytes());
    sfnt(vec![
        (b"cmap", cmap),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"maxp", maxp),
    ])
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// A complete 96-glyph TrueType face for Typst: U+0020..=U+007E map to glyphs 1..=95, every glyph
/// is 600 units wide except the 500 unit `.notdef`, and only the letter A (glyph 34) has an
/// outline, a 500 by 700 box. Family name "Libertinus Serif": Typst chooses its math font from its own default family names
/// (New Computer Modern Math, then the Libertinus Serif fallback), so a face supplied for math must carry
/// one of them. This face is a fixture and is not that typeface.
fn render_font() -> Vec<u8> {
    const GLYPHS: u16 = 96;
    let mut head = vec![0; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[4..8].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    head[40..42].copy_from_slice(&500i16.to_be_bytes()); // xMax
    head[42..44].copy_from_slice(&700i16.to_be_bytes()); // yMax
    let mut hhea = vec![0; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[4..6].copy_from_slice(&800i16.to_be_bytes()); // ascender
    hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes()); // descender
    hhea[10..12].copy_from_slice(&600u16.to_be_bytes()); // advanceWidthMax
    hhea[18..20].copy_from_slice(&1i16.to_be_bytes()); // caretSlopeRise
    hhea[34..36].copy_from_slice(&2u16.to_be_bytes()); // numberOfHMetrics
    let mut maxp = vec![0; 32];
    maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&GLYPHS.to_be_bytes());
    maxp[6..8].copy_from_slice(&4u16.to_be_bytes()); // maxPoints
    maxp[8..10].copy_from_slice(&1u16.to_be_bytes()); // maxContours
    maxp[14..16].copy_from_slice(&2u16.to_be_bytes()); // maxZones
    let mut hmtx = Vec::new();
    for advance in [500u16, 600] {
        be16(&mut hmtx, advance);
        be16(&mut hmtx, 0);
    }
    for _ in 2..GLYPHS {
        be16(&mut hmtx, 0); // left side bearings; the advance of the last metric is shared
    }
    let mut subtable = Vec::new();
    be16(&mut subtable, 12);
    be16(&mut subtable, 0);
    be32(&mut subtable, 28);
    be32(&mut subtable, 0);
    be32(&mut subtable, 1);
    for value in [0x20, 0x7E, 1] {
        be32(&mut subtable, value);
    }
    // Glyph 34 (U+0041): one contour, the box (0,0) (0,700) (500,700) (500,0).
    let mut glyf = Vec::new();
    be16(&mut glyf, 1);
    for value in [0i16, 0, 500, 700] {
        glyf.extend_from_slice(&value.to_be_bytes());
    }
    be16(&mut glyf, 3); // endPtsOfContours
    be16(&mut glyf, 0); // instructionLength
    glyf.extend_from_slice(&[1, 1, 1, 1]); // on-curve points, 16-bit deltas
    for delta in [0i16, 0, 500, 0, 0, 700, 0, -700] {
        glyf.extend_from_slice(&delta.to_be_bytes());
    }
    glyf.resize(36, 0);
    let mut loca = Vec::new(); // short format: offsets in units of two bytes
    for glyph in 0..=GLYPHS {
        be16(&mut loca, if glyph <= 34 { 0 } else { 18 });
    }
    let names = [
        (1u16, "Libertinus Serif"),
        (2, "Regular"),
        (4, "Libertinus Serif"),
        (6, "LibertinusSerif-Regular"),
    ];
    let mut name = Vec::new();
    be16(&mut name, 0);
    be16(&mut name, names.len() as u16);
    be16(&mut name, (6 + 12 * names.len()) as u16);
    let mut strings = Vec::new();
    for (id, text) in names {
        let bytes = utf16(text);
        for value in [
            3u16,
            1,
            0x0409,
            id,
            bytes.len() as u16,
            strings.len() as u16,
        ] {
            be16(&mut name, value);
        }
        strings.extend_from_slice(&bytes);
    }
    name.extend_from_slice(&strings);
    sfnt(vec![
        (b"cmap", cmap_with(subtable)),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
        (b"name", name),
    ])
}

fn cmap_with(subtable: Vec<u8>) -> Vec<u8> {
    cmap(&[subtable])
}

fn request_text() {
    let default = Request::new(Source::Typst("= Title".into()));
    assert_eq!(default.output, Output::Svg);
    assert_eq!(
        (
            default.font_size_pt,
            default.margin_pt,
            default.font_family.clone()
        ),
        (24.0, 4.0, None)
    );
    assert_eq!(
        default.to_typst().unwrap(),
        "#set page(width: auto, height: auto, margin: 4pt, fill: none)\n#set text(size: 24pt)\n= Title"
    );
    let math = Request::new(Source::Math("a^2 + b^2".into()))
        .to_typst()
        .unwrap();
    assert!(math.ends_with("$ a^2 + b^2 $"));
    let mut styled = Request::new(Source::Typst("x".into()));
    styled.font_size_pt = 12.5;
    styled.margin_pt = 0.0;
    styled.font_family = Some("A\"B\\C".into());
    assert_eq!(
        styled.to_typst().unwrap(),
        "#set page(width: auto, height: auto, margin: 0pt, fill: none)\n#set text(size: 12.5pt, font: \"A\\\"B\\\\C\")\nx"
    );
    assert_eq!(default, Request::new(Source::Typst("= Title".into())));
    assert_ne!(default, Request::new(Source::Math("= Title".into())));
    assert!(matches!(FontSource::default(), FontSource::None));
    assert_eq!(Output::default(), Output::Svg);
}

fn typesetter_construction() {
    for bad in [-1.0, 0.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            Typesetter::new(FontSource::None, bad).unwrap().ppi(),
            144.0,
            "ppi {bad}"
        );
    }
    assert_eq!(Typesetter::new(FontSource::None, 96.0).unwrap().ppi(), 96.0);
    let none = Typesetter::new(FontSource::None, 144.0).unwrap();
    assert_eq!(
        none.render(&Request::new(Source::Typst("hi".into()))),
        Err(TypesetError::NoFonts)
    );
    // An empty list of font files is the same as none; unreadable bytes are refused up front.
    let empty = Typesetter::new(FontSource::Bytes(vec![]), 144.0).unwrap();
    assert_eq!(
        empty.render(&Request::new(Source::Math("x".into()))),
        Err(TypesetError::NoFonts)
    );
    assert!(matches!(
        Typesetter::new(FontSource::Bytes(vec![vec![1, 2, 3]]), 144.0),
        Err(TypesetError::UnreadableFonts)
    ));
    for (error, text) in [
        (TypesetError::NoFonts, "no fonts were provided"),
        (
            TypesetError::UnreadableFonts,
            "no font could be read from the supplied bytes",
        ),
        (TypesetError::NoPages, "document produced no pages"),
        (
            TypesetError::Compile("boom".into()),
            "typst compile failed: boom",
        ),
        (
            TypesetError::Latex("bad".into()),
            "latex conversion failed: bad",
        ),
        (TypesetError::Encode("png".into()), "png encode failed: png"),
    ] {
        assert_eq!(error.to_string(), text);
    }
}

fn font_metrics() {
    // Proportional face with a format 4 cmap: M 900, i 250 (glyph 1 and 2), .notdef 500.
    let face: font::FontFace =
        FontFace::parse(metrics_font(cmap(&[format4()]), &[500, 900, 250], 3)).unwrap();
    assert_eq!(face.units_per_em(), 1000);
    assert_eq!(
        (face.glyph_id('M'), face.glyph_id('i'), face.glyph_id('x')),
        (1, 2, 0)
    );
    assert_eq!(face.advance_units(1).unwrap(), 900);
    let measured: Measurement = face.measure("Mi\n\t\rx\u{1F984}x\u{1F984}").unwrap();
    assert_eq!(measured.advance_units, 900 + 250 + 4 * 500);
    assert_eq!(
        measured.missing,
        vec!['x', '\u{1F984}'],
        "each missing character is listed once"
    );
    assert_eq!(measured.units_per_em, 1000);
    assert_eq!(measured.width_px(100), 315);
    assert_eq!(face.measure("").unwrap().advance_units, 0);
    assert!(
        face.measure("\n\t\0").unwrap().missing.is_empty(),
        "control characters count nothing"
    );
    assert_eq!(
        face.measure("ii").unwrap().width_px(1),
        1,
        "rounding happens once, after summing"
    );
    let narrow = Measurement {
        units_per_em: 1000,
        advance_units: 250,
        missing: vec![],
    };
    assert_eq!(
        (narrow.width_px(1), narrow.width_px(2), narrow.width_px(0)),
        (0, 1, 0)
    );

    // A format 12 cmap reaches beyond the BMP; glyphs past numberOfHMetrics reuse the last advance.
    let wide = FontFace::parse(metrics_font(cmap(&[format12()]), &[500, 750], 4)).unwrap();
    assert_eq!(wide.glyph_id('\u{1F984}'), 3);
    assert_eq!(wide.advance_units(3).unwrap(), 750);
    assert_eq!(wide.advance_units(4).unwrap(), 0, "past numGlyphs");
    assert_eq!(wide.measure("Mi\u{1F984}").unwrap().advance_units, 2250);

    // Format 12 wins whichever order the records are in.
    let mut full = format12();
    full[24..28].copy_from_slice(&2u32.to_be_bytes());
    for subtables in [vec![format4(), full.clone()], vec![full, format4()]] {
        let face = FontFace::parse(metrics_font(cmap(&subtables), &[500, 900, 250], 4)).unwrap();
        assert_eq!(face.glyph_id('M'), 2);
    }

    // The first face of a collection, with table offsets relative to the file.
    let mut bytes = metrics_font(cmap(&[format12()]), &[500, 900, 250], 4);
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
    assert_eq!(
        FontFace::parse(collection)
            .unwrap()
            .measure("Mi\u{1F984}")
            .unwrap()
            .advance_units,
        1400
    );

    // Each FontError, by value.
    assert!(matches!(
        FontFace::parse(vec![]),
        Err(FontError::Malformed(_))
    ));
    assert!(matches!(
        FontFace::parse(b"not a font".to_vec()),
        Err(FontError::Malformed(_))
    ));
    let mut broken = metrics_font(cmap(&[format4()]), &[500, 900, 250], 3);
    broken[12..16].copy_from_slice(b"xxxx");
    assert_eq!(
        FontFace::parse(broken).unwrap_err(),
        FontError::MissingTable("cmap")
    );
    let unsupported = metrics_font(cmap(&[vec![0, 0, 0, 6, 0, 0]]), &[500], 1);
    assert_eq!(
        FontFace::parse(unsupported).unwrap_err(),
        FontError::NoUsableCmap
    );
    assert_eq!(
        FontError::MissingTable("head").to_string(),
        "font has no head table"
    );
    assert_eq!(
        FontError::NoUsableCmap.to_string(),
        "font has no cmap subtable in format 4 or 12"
    );
    let mut truncated = metrics_font(cmap(&[format4()]), &[500, 900, 250], 3);
    truncated.truncate(truncated.len() - 30);
    assert!(FontFace::parse(truncated).is_err());

    // The renderable face reads too: A is glyph 34 and 600 units wide.
    let face = FontFace::parse(render_font()).unwrap();
    assert_eq!(
        (
            face.glyph_id('A'),
            face.glyph_id(' '),
            face.glyph_id('\u{e9}')
        ),
        (34, 1, 0)
    );
    let measured = face.measure("AAA\u{e9}").unwrap();
    assert_eq!(
        (measured.advance_units, measured.missing),
        (1800 + 500, vec!['\u{e9}'])
    );
    assert_eq!(face.measure("AAA").unwrap().width_px(24), 43);
}

fn svg_width_pt(svg: &str) -> f64 {
    let at = svg.find("width=\"").expect("svg width attribute") + "width=\"".len();
    let end = svg[at..].find("pt\"").expect("width in points");
    svg[at..at + end].parse().expect("numeric width")
}

fn render_with_a_built_font() {
    let typesetter = Typesetter::new(FontSource::Bytes(vec![render_font()]), 144.0).unwrap();
    let render = |source: Source| match typesetter.render(&Request::new(source)) {
        Ok(Rendered::Svg(svg)) => svg,
        other => panic!("expected an SVG, got {other:?}"),
    };

    // Auto-sized page: the width is the text advance plus both margins. Five A at 24 pt and 600
    // units are 72 pt, plus 4 pt each side.
    let five = render(Source::Typst("AAAAA".into()));
    assert!(five.contains("<svg"), "{five}");
    assert!((svg_width_pt(&five) - 80.0).abs() < 0.01, "{five}");
    assert!(
        five.contains("<path") || five.contains("<use"),
        "the A outline is drawn: {five}"
    );
    assert_eq!(
        five,
        render(Source::Typst("AAAAA".into())),
        "rendering is deterministic"
    );
    let ten = render(Source::Typst("AAAAAAAAAA".into()));
    assert!((svg_width_pt(&ten) - 152.0).abs() < 0.01, "{ten}");

    // Size, margin and family are request fields.
    let mut small = Request::new(Source::Typst("AAAAA".into()));
    small.font_size_pt = 12.0;
    small.margin_pt = 0.0;
    small.font_family = Some("Libertinus Serif".into());
    match typesetter.render(&small) {
        Ok(Rendered::Svg(svg)) => assert!((svg_width_pt(&svg) - 36.0).abs() < 0.01, "{svg}"),
        other => panic!("expected an SVG, got {other:?}"),
    }

    // Math and headings compile through the same world. Typst chooses the math font from its own
    // default family names, so the fixture face is named after the Libertinus Serif fallback.
    assert!(render(Source::Math("A^2 + A".into())).contains("<svg"));
    assert!(render(Source::Typst("= AAA\n\nAA AA".into())).contains("<svg"));

    // The world has no file access and no package fetching; bad syntax is a compile error.
    for hostile in [
        "#image(\"missing.png\")",
        "#read(\"/etc/hostname\")",
        "#import \"@preview/example:0.1.0\": *",
        "#include \"other.typ\"",
        "#(",
    ] {
        match typesetter.render(&Request::new(Source::Typst(hostile.into()))) {
            Err(TypesetError::Compile(message)) => assert!(!message.is_empty(), "{hostile}"),
            other => panic!("{hostile}: expected a compile error, got {other:?}"),
        }
    }
}

fn main() {
    request_text();
    typesetter_construction();
    font_metrics();
    render_with_a_built_font();
    println!("OK rightkit-typeset 0.1.0 (registry consumer)");
}
