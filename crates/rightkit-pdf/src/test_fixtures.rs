//! Shared `#[cfg(test)]` fixtures for the pdf crate's unit tests.
//!
//! Three modules independently grew copies of the same minimal one-page PDF builder
//! (`inspect.rs`, `compose.rs`, `save.rs`) and two copies of the same minimal synthetic sfnt
//! builder (`font.rs`, `compose.rs`). Keeping one proven copy means a fix to the fixture's
//! spec-correctness propagates everywhere instead of drifting per file.

use lopdf::{dictionary, Document, Object, Stream};

#[cfg(feature = "fonts")]
pub(crate) fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
#[cfg(feature = "fonts")]
pub(crate) fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// Everything a one-page-PDF fixture needs. `Default` is the common shape: a single base-14
/// Helvetica `/F1` resource (WinAnsiEncoding, no embedded program), an empty content stream, and
/// a 612×792 letter `MediaBox` on the page.
pub(crate) struct PageFixture {
    /// The page's `/Contents` stream object.
    pub content: Stream,
    /// Whether `/Resources /Font` contains the `/F1` Helvetica entry. Tests exercising missing-font
    /// paths set this false (the resource dict is still present but empty).
    pub helvetica_f1: bool,
    /// Extra entries merged into the page dictionary (e.g. `Rotate`).
    pub page_extras: Vec<(&'static str, Object)>,
    /// `MediaBox` written on the page itself; `None` omits it (for inherited-box tests, pair with
    /// `pages_media_box`).
    pub media_box: Option<[i64; 4]>,
    /// `MediaBox` written on the parent `/Pages` node (inherited by the page).
    pub pages_media_box: Option<[i64; 4]>,
}

impl PageFixture {
    /// The common case: `content_ops` as the raw content stream with the standard font and box.
    pub(crate) fn for_content(content_ops: &[u8]) -> Self {
        Self {
            content: Stream::new(dictionary! {}, content_ops.to_vec()),
            helvetica_f1: true,
            page_extras: Vec::new(),
            media_box: Some([0, 0, 612, 792]),
            pages_media_box: None,
        }
    }
}

/// Assemble a minimal one-page `Document` from a [`PageFixture`].
pub(crate) fn one_page_pdf(fx: PageFixture) -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();

    let mut font_resources = dictionary! {};
    if fx.helvetica_f1 {
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        font_resources = dictionary! { "F1" => font_id };
    }
    let resources_id = doc.add_object(dictionary! { "Font" => font_resources });
    let content_id = doc.add_object(fx.content);

    let mut page_dict = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
    };
    if let Some([a, b, c, d]) = fx.media_box {
        page_dict.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(a),
                Object::Integer(b),
                Object::Integer(c),
                Object::Integer(d),
            ]),
        );
    }
    for (key, value) in fx.page_extras {
        page_dict.set(key, value);
    }
    let page_id = doc.add_object(page_dict);

    let mut pages_dict = dictionary! {
        "Type" => "Pages",
        "Kids" => Object::Array(vec![Object::Reference(page_id)]),
        "Count" => 1,
    };
    if let Some([a, b, c, d]) = fx.pages_media_box {
        pages_dict.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(a),
                Object::Integer(b),
                Object::Integer(c),
                Object::Integer(d),
            ]),
        );
    }
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc
}

/// A one-page document with caller-built font objects and one or more content streams.
///
/// `fonts` runs against the document being built and returns the `/Resources /Font` entries.
/// With `indirect_array` the page's `/Contents` is an *indirect reference to an array* of the
/// streams (the shape `uscis-i9.pdf` uses); otherwise it is a direct array (or a single stream
/// reference for one stream).
pub(crate) fn custom_page_pdf(
    fonts: impl FnOnce(&mut Document) -> Vec<(&'static str, lopdf::ObjectId)>,
    streams: &[&[u8]],
    indirect_array: bool,
) -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut font_resources = dictionary! {};
    for (name, id) in fonts(&mut doc) {
        font_resources.set(name, id);
    }
    let resources_id = doc.add_object(dictionary! { "Font" => font_resources });
    let stream_ids: Vec<lopdf::ObjectId> = streams
        .iter()
        .map(|bytes| doc.add_object(Stream::new(dictionary! {}, bytes.to_vec())))
        .collect();
    let refs = Object::Array(stream_ids.iter().map(|id| Object::Reference(*id)).collect());
    let contents: Object = if indirect_array {
        Object::Reference(doc.add_object(refs))
    } else if stream_ids.len() == 1 {
        Object::Reference(stream_ids[0])
    } else {
        refs
    };
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => contents,
        "Resources" => resources_id,
        "MediaBox" => Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    doc
}

/// Non-embedded Helvetica with WinAnsiEncoding (widths come from the Standard-14 table).
pub(crate) fn helvetica_font(doc: &mut Document) -> lopdf::ObjectId {
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    })
}

/// An embedded-subset-style simple Type1 font: `/FirstChar`, `/Widths`, a descriptor naming a
/// `/FontFile`, and the given `/Encoding` (omitted when `None`) and `ToUnicode` CMap source.
pub(crate) fn subset_type1_font(
    doc: &mut Document,
    first_char: i64,
    widths: &[i64],
    encoding: Option<Object>,
    to_unicode: Option<&str>,
) -> lopdf::ObjectId {
    let program = doc.add_object(Stream::new(
        dictionary! {},
        b"not a real font program".to_vec(),
    ));
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "ABCDEF+Subset",
        "Flags" => 4,
        "FontFile" => program,
    });
    let mut font = dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "ABCDEF+Subset",
        "FirstChar" => first_char,
        "LastChar" => first_char + widths.len() as i64 - 1,
        "Widths" => Object::Array(widths.iter().map(|w| Object::Integer(*w)).collect()),
        "FontDescriptor" => descriptor,
    };
    if let Some(encoding) = encoding {
        font.set("Encoding", encoding);
    }
    if let Some(cmap) = to_unicode {
        let id = doc.add_object(Stream::new(dictionary! {}, cmap.as_bytes().to_vec()));
        font.set("ToUnicode", id);
    }
    doc.add_object(font)
}

/// `/Encoding` dictionary with `/Differences` and, optionally, `/BaseEncoding` and `/Type`.
pub(crate) fn differences_encoding(
    base: Option<&str>,
    typed: bool,
    differences: Vec<Object>,
) -> Object {
    let mut dict = dictionary! { "Differences" => Object::Array(differences) };
    if typed {
        dict.set("Type", "Encoding");
    }
    if let Some(base) = base {
        dict.set("BaseEncoding", base);
    }
    Object::Dictionary(dict)
}

/// A `ToUnicode` CMap mapping each `(code, char)` through `beginbfchar`, `width` bytes per code.
pub(crate) fn to_unicode_cmap(width: usize, pairs: &[(u32, char)]) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         1 begincodespacerange <00> <FF> endcodespacerange\n",
    );
    out.push_str(&format!("{} beginbfchar\n", pairs.len()));
    for (code, ch) in pairs {
        out.push_str(&format!(
            "<{:0w$X}> <{:04X}>\n",
            code,
            *ch as u32,
            w = width * 2
        ));
    }
    out.push_str("endbfchar\nendcmap end end\n");
    out
}

/// A standards-shaped single-byte `ToUnicode` CMap (CIDSystemInfo, CMapName, codespace, `bfchar`,
/// `defineresource`): the form the PDF library's strict CMap parser accepts, which simple-font
/// decoding goes through when a font has no `/Encoding`.
pub(crate) fn strict_to_unicode_cmap(pairs: &[(u32, char)]) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n\
         <00> <FF>\n\
         endcodespacerange\n",
    );
    out.push_str(&format!("{} beginbfchar\n", pairs.len()));
    for (code, ch) in pairs {
        out.push_str(&format!("<{code:02X}> <{:04X}>\n", *ch as u32));
    }
    out.push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

/// A Type0 `Identity-H` font with explicit `/W` widths (`(cid, width)`) and a `ToUnicode` CMap
/// over `mapped`; the descendant carries no font program (inspection and composition read only
/// the dictionaries).
pub(crate) fn identity_h_font(
    doc: &mut Document,
    widths: &[(u32, i64)],
    mapped: &[(u32, char)],
) -> lopdf::ObjectId {
    identity_h_font_with_cmap(doc, widths, to_unicode_cmap(2, mapped))
}

/// [`identity_h_font`] with a caller-written `ToUnicode` CMap source (for `bfrange` forms).
pub(crate) fn identity_h_font_with_cmap(
    doc: &mut Document,
    widths: &[(u32, i64)],
    cmap_source: String,
) -> lopdf::ObjectId {
    let mut w = Vec::new();
    for (cid, width) in widths {
        w.push(Object::Integer(*cid as i64));
        w.push(Object::Array(vec![Object::Integer(*width)]));
    }
    let descendant = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "ABCDEF+Cid",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::String(b"Adobe".to_vec(), lopdf::StringFormat::Literal),
            "Ordering" => Object::String(b"Identity".to_vec(), lopdf::StringFormat::Literal),
            "Supplement" => 0,
        },
        "DW" => 1000,
        "W" => Object::Array(w),
        "CIDToGIDMap" => "Identity",
    });
    let cmap = doc.add_object(Stream::new(dictionary! {}, cmap_source.into_bytes()));
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "ABCDEF+Cid",
        "Encoding" => "Identity-H",
        "DescendantFonts" => Object::Array(vec![Object::Reference(descendant)]),
        "ToUnicode" => cmap,
    })
}

/// A `ToUnicode` CMap with the given raw body between its codespace range and `endcmap`.
pub(crate) fn raw_to_unicode_cmap(body: &str) -> String {
    format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         1 begincodespacerange <0000> <FFFF> endcodespacerange\n{body}\nendcmap end end\n"
    )
}

/// A TeX-style subset font: lowercase letters at their ASCII codes (width 500) and, when
/// `space_glyph`, a real `space` glyph at code 32 (width 250). Without it the font has no space
/// at all, so word gaps can only be `TJ` adjustments. Codes are defined by `/Differences` alone.
pub(crate) fn tex_style_font(doc: &mut Document, space_glyph: bool) -> lopdf::ObjectId {
    let mut differences = Vec::new();
    if space_glyph {
        differences.push(Object::Integer(32));
        differences.push(Object::Name(b"space".to_vec()));
    }
    differences.push(Object::Integer(97));
    for letter in b'a'..=b'z' {
        differences.push(Object::Name(vec![letter]));
    }
    let widths: Vec<i64> = (32i64..=122)
        .map(|code| match code {
            32 if space_glyph => 250,
            97..=122 => 500,
            _ => 0,
        })
        .collect();
    let encoding = differences_encoding(None, true, differences);
    subset_type1_font(doc, 32, &widths, Some(encoding), None)
}

/// A subset font whose `/Differences` make **code 32 the letter `x`** (97 and 98 are `a` and `b`),
/// every listed code 500 wide. Word spacing follows the code, so `Tw` applies to this "letter".
pub(crate) fn code_32_letter_font(doc: &mut Document) -> lopdf::ObjectId {
    let encoding = differences_encoding(
        None,
        true,
        vec![
            Object::Integer(32),
            Object::Name(b"x".to_vec()),
            Object::Integer(97),
            Object::Name(b"a".to_vec()),
            Object::Name(b"b".to_vec()),
        ],
    );
    let widths: Vec<i64> = (32i64..=98)
        .map(|code| if matches!(code, 32 | 97 | 98) { 500 } else { 0 })
        .collect();
    subset_type1_font(doc, 32, &widths, Some(encoding), None)
}

/// A subset font with its **space at code 5** (250 wide), `a` and `b` at 97 and 98 (500 wide).
/// Code 32 is not in the font, so `Tw` applies to no glyph of it.
pub(crate) fn code_5_space_font(doc: &mut Document) -> lopdf::ObjectId {
    let encoding = differences_encoding(
        None,
        true,
        vec![
            Object::Integer(5),
            Object::Name(b"space".to_vec()),
            Object::Integer(97),
            Object::Name(b"a".to_vec()),
            Object::Name(b"b".to_vec()),
        ],
    );
    let widths: Vec<i64> = (5i64..=98)
        .map(|code| match code {
            5 => 250,
            97 | 98 => 500,
            _ => 0,
        })
        .collect();
    subset_type1_font(doc, 5, &widths, Some(encoding), None)
}

/// A one-page document whose page has **no `/Resources`**: the `/Pages` node carries them
/// (`/Font /F1` Helvetica and `/XObject /Im1`), and the page inherits them.
pub(crate) fn inherited_resources_pdf(content: &[u8]) -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font = helvetica_font(&mut doc);
    let form = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(10),
                Object::Integer(10),
            ]),
        },
        Vec::new(),
    ));
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "MediaBox" => Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => 1,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font },
                "XObject" => dictionary! { "Im1" => form },
            },
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    doc
}

/// One page set in the TeX-style font `/F1`; `content` is the raw content stream.
pub(crate) fn tex_style_pdf(space_glyph: bool, content: &[u8]) -> Document {
    custom_page_pdf(
        |d| vec![("F1", tex_style_font(d, space_glyph))],
        &[content],
        false,
    )
}

/// The standard one-page fixture: `content_ops` content, `/F1` Helvetica, 612×792.
pub(crate) fn build_one_page_pdf(content_ops: &[u8]) -> Document {
    one_page_pdf(PageFixture::for_content(content_ops))
}

/// Same fixture serialized to bytes (for save-path tests that need on-disk form).
#[cfg(feature = "edit")]
pub(crate) fn build_one_page_pdf_bytes(content_ops: &[u8]) -> Vec<u8> {
    pdf_bytes(&build_one_page_pdf(content_ops))
}

/// Serialize a fixture document the way every caller previously inlined.
#[cfg(feature = "edit")]
pub(crate) fn pdf_bytes(doc: &Document) -> Vec<u8> {
    let mut doc = doc.clone();
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("fixture PDF must save");
    out
}

/// Assemble an sfnt binary from `(tag, table-bytes)` pairs. Records are sorted by tag: readers
/// that binary-search the directory (e.g. `subsetter::Face::table`, unlike the more lenient
/// `ttf-parser`) silently miss an out-of-order table — this was the source of a real bug where
/// `subsetter::subset` saw no `glyf`/`CFF`/`CFF2` table and returned `UnknownKind`.
#[cfg(feature = "fonts")]
pub(crate) fn build_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut tables = tables.to_vec();
    tables.sort_by_key(|(tag, _)| *tag);
    let tables = &tables[..];

    let num_tables = tables.len() as u16;
    let entry_selector = (num_tables as f64).log2().floor() as u16;
    let search_range = 16u16.saturating_mul(1u16 << entry_selector.min(15));
    let range_shift = (num_tables * 16).saturating_sub(search_range);

    let header_len = 12 + tables.len() * 16;
    let mut offset = header_len as u32;
    let mut records = Vec::new();
    let mut body = Vec::new();
    for (tag, data) in tables {
        records.extend(*tag);
        records.extend(be32(0)); // checksum (unchecked by ttf-parser; left as 0)
        records.extend(be32(offset));
        records.extend(be32(data.len() as u32));
        body.extend_from_slice(data);
        let padded_len = (data.len() + 3) & !3;
        body.resize(body.len() + (padded_len - data.len()), 0);
        offset += padded_len as u32;
    }

    let mut out = Vec::new();
    out.extend(be32(0x0001_0000)); // sfnt version (TrueType)
    out.extend(be16(num_tables));
    out.extend(be16(search_range));
    out.extend(be16(entry_selector));
    out.extend(be16(range_shift));
    out.extend(records);
    out.extend(body);
    out
}

/// Build a minimal TrueType font mapping each `(char, glyph_id)` in `chars` to a real glyph
/// (glyph 0 is always `.notdef`), every glyph advancing by `advance` font units, with the OS/2
/// `fsType` field set to `fs_type` so embedding/subsetting permission tests can control it.
///
/// Produces just enough of a valid sfnt (`head`, `hhea`, `maxp`, `hmtx`, `cmap` format 4,
/// `glyf`/`loca` with zero-length outlines, `OS/2` version 2) for `ttf_parser::Face::parse` to
/// succeed and for `glyph_index`/`glyph_hor_advance`/`permissions`/`is_outline_embedding_allowed`/
/// `is_subsetting_allowed` to behave correctly. OS/2 is version 2 because real `ttf-parser` only
/// honors the `fsType` restriction bits when `version >= 2`, and `Table::parse` requires
/// `data.len() >= 96` — a shorter or v0 table silently reports everything allowed.
#[cfg(feature = "fonts")]
pub(crate) fn build_test_ttf(fs_type: u16, chars: &[(char, u16)], advance: u16) -> Vec<u8> {
    let num_glyphs = chars.iter().map(|(_, g)| *g).max().unwrap_or(0) + 1;

    // --- head (54 bytes) ---
    let mut head = Vec::new();
    head.extend(be16(1)); // majorVersion
    head.extend(be16(0)); // minorVersion
    head.extend(be32(0)); // fontRevision
    head.extend(be32(0)); // checkSumAdjustment
    head.extend(be32(0x5F0F3CF5)); // magicNumber
    head.extend(be16(0)); // flags
    head.extend(be16(1000)); // unitsPerEm
    head.extend(be32(0)); // created (hi)
    head.extend(be32(0)); // created (lo)
    head.extend(be32(0)); // modified (hi)
    head.extend(be32(0)); // modified (lo)
    head.extend(be16(0)); // xMin (as i16 via be16 bit pattern)
    head.extend(be16(0)); // yMin
    head.extend(be16(0)); // xMax
    head.extend(be16(0)); // yMax
    head.extend(be16(0)); // macStyle
    head.extend(be16(8)); // lowestRecPPEM
    head.extend(be16(2)); // fontDirectionHint
    head.extend(be16(0)); // indexToLocFormat (0 = short/Offset16)
    head.extend(be16(0)); // glyphDataFormat
    assert_eq!(head.len(), 54);

    // --- hhea (36 bytes) ---
    let mut hhea = Vec::new();
    hhea.extend(be16(1));
    hhea.extend(be16(0));
    hhea.extend(be16(800)); // ascender
    hhea.extend(be16((-200i16) as u16)); // descender
    hhea.extend(be16(0)); // lineGap
    hhea.extend(be16(advance)); // advanceWidthMax
    hhea.extend(be16(0)); // minLeftSideBearing
    hhea.extend(be16(0)); // minRightSideBearing
    hhea.extend(be16(advance)); // xMaxExtent
    hhea.extend(be16(1)); // caretSlopeRise
    hhea.extend(be16(0)); // caretSlopeRun
    hhea.extend(be16(0)); // caretOffset
    hhea.extend(be16(0)); // reserved
    hhea.extend(be16(0)); // reserved
    hhea.extend(be16(0)); // reserved
    hhea.extend(be16(0)); // reserved
    hhea.extend(be16(0)); // metricDataFormat
    hhea.extend(be16(num_glyphs)); // numberOfHMetrics
    assert_eq!(hhea.len(), 36);

    // --- maxp (version 1.0, 32 bytes) ---
    let mut maxp = Vec::new();
    maxp.extend(be32(0x0001_0000));
    maxp.extend(be16(num_glyphs));
    maxp.extend(be16(0)); // maxPoints
    maxp.extend(be16(0)); // maxContours
    maxp.extend(be16(0)); // maxCompositePoints
    maxp.extend(be16(0)); // maxCompositeContours
    maxp.extend(be16(2)); // maxZones
    maxp.extend(be16(0)); // maxTwilightPoints
    maxp.extend(be16(0)); // maxStorage
    maxp.extend(be16(0)); // maxFunctionDefs
    maxp.extend(be16(0)); // maxInstructionDefs
    maxp.extend(be16(0)); // maxStackElements
    maxp.extend(be16(0)); // maxSizeOfInstructions
    maxp.extend(be16(0)); // maxComponentElements
    maxp.extend(be16(0)); // maxComponentDepth
    assert_eq!(maxp.len(), 32);

    // --- hmtx (numGlyphs * 4 bytes: advanceWidth u16 + lsb i16) ---
    let mut hmtx = Vec::new();
    for _ in 0..num_glyphs {
        hmtx.extend(be16(advance));
        hmtx.extend(be16(0));
    }

    // --- loca (short format: (numGlyphs+1) u16 offsets, in units of 2 bytes) + glyf (all
    // glyphs zero-length outlines, so every loca entry is 0) ---
    let mut loca = Vec::new();
    for _ in 0..=num_glyphs {
        loca.extend(be16(0));
    }
    let glyf: Vec<u8> = Vec::new();

    // --- cmap: one format-4 subtable, Windows/Unicode BMP, mapping `chars` ---
    let mut mappings: Vec<(u16, u16)> = chars.iter().map(|(c, g)| (*c as u16, *g)).collect();
    mappings.sort_by_key(|(code, _)| *code);
    let seg_count = mappings.len() + 1; // +1 terminator segment
    let seg_count_x2 = (seg_count * 2) as u16;
    let mut end_codes = Vec::new();
    let mut start_codes = Vec::new();
    let mut id_deltas = Vec::new();
    let mut id_range_offsets = Vec::new();
    for (code, gid) in &mappings {
        end_codes.push(*code);
        start_codes.push(*code);
        id_deltas.push(gid.wrapping_sub(*code));
        id_range_offsets.push(0u16);
    }
    end_codes.push(0xFFFF);
    start_codes.push(0xFFFF);
    id_deltas.push(1);
    id_range_offsets.push(0);

    let entry_selector = (seg_count as f64).log2().floor() as u16;
    let search_range = 2u16.saturating_mul(1u16 << entry_selector.min(15));
    let range_shift = seg_count_x2.saturating_sub(search_range);

    let mut subtable = Vec::new();
    subtable.extend(be16(4)); // format
    subtable.extend(be16(0)); // length placeholder, filled below once the full size is known
    subtable.extend(be16(0)); // language
    subtable.extend(be16(seg_count_x2));
    subtable.extend(be16(search_range));
    subtable.extend(be16(entry_selector));
    subtable.extend(be16(range_shift));
    for v in &end_codes {
        subtable.extend(be16(*v));
    }
    subtable.extend(be16(0)); // reservedPad
    for v in &start_codes {
        subtable.extend(be16(*v));
    }
    for v in &id_deltas {
        subtable.extend(be16(*v));
    }
    for v in &id_range_offsets {
        subtable.extend(be16(*v));
    }
    // `subtable` already includes the 2-byte `length` placeholder pushed above, so its current
    // byte count IS the subtable's total self-inclusive length per the format-4 spec — no `+2`.
    let length = subtable.len() as u16;
    let length_bytes = be16(length);
    subtable[2] = length_bytes[0];
    subtable[3] = length_bytes[1];

    let mut cmap = Vec::new();
    cmap.extend(be16(0)); // version
    cmap.extend(be16(1)); // numTables
    cmap.extend(be16(3)); // platformID (Windows)
    cmap.extend(be16(1)); // encodingID (Unicode BMP)
    cmap.extend(be32(12)); // offset to subtable (4 header + 8 record bytes)
    cmap.extend(subtable);

    // --- OS/2 version 2 (96 bytes) ---
    let mut os2 = Vec::new();
    os2.extend(be16(2)); // version
    os2.extend(be16(0)); // xAvgCharWidth
    os2.extend(be16(400)); // usWeightClass
    os2.extend(be16(5)); // usWidthClass
    os2.extend(be16(fs_type)); // fsType
    os2.extend(be16(0)); // ySubscriptXSize
    os2.extend(be16(0)); // ySubscriptYSize
    os2.extend(be16(0)); // ySubscriptXOffset
    os2.extend(be16(0)); // ySubscriptYOffset
    os2.extend(be16(0)); // ySuperscriptXSize
    os2.extend(be16(0)); // ySuperscriptYSize
    os2.extend(be16(0)); // ySuperscriptXOffset
    os2.extend(be16(0)); // ySuperscriptYOffset
    os2.extend(be16(0)); // yStrikeoutSize
    os2.extend(be16(0)); // yStrikeoutPosition
    os2.extend(be16(0)); // sFamilyClass
    os2.extend([0u8; 10]); // panose
    os2.extend(be32(0)); // ulUnicodeRange1
    os2.extend(be32(0)); // ulUnicodeRange2
    os2.extend(be32(0)); // ulUnicodeRange3
    os2.extend(be32(0)); // ulUnicodeRange4
    os2.extend(*b"TEST"); // achVendID
    os2.extend(be16(0x0040)); // fsSelection (REGULAR)
    os2.extend(be16(0)); // usFirstCharIndex
    os2.extend(be16(0xFFFF)); // usLastCharIndex
    os2.extend(be16(800)); // sTypoAscender
    os2.extend(be16((-200i16) as u16)); // sTypoDescender
    os2.extend(be16(0)); // sTypoLineGap
    os2.extend(be16(1000)); // usWinAscent
    os2.extend(be16(200)); // usWinDescent
    os2.extend(be32(0)); // ulCodePageRange1 (version >= 1)
    os2.extend(be32(0)); // ulCodePageRange2 (version >= 1)
    os2.extend(be16(500)); // sxHeight (version >= 2)
    os2.extend(be16(700)); // sCapHeight (version >= 2)
    os2.extend(be16(0)); // usDefaultChar (version >= 2)
    os2.extend(be16(32)); // usBreakChar (version >= 2)
    os2.extend(be16(1)); // usMaxContext (version >= 2)
    assert_eq!(os2.len(), 96);

    build_sfnt(&[
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"maxp", maxp),
        (*b"hmtx", hmtx),
        (*b"loca", loca),
        (*b"glyf", glyf),
        (*b"cmap", cmap),
        (*b"OS/2", os2),
    ])
}
