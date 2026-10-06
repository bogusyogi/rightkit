//! PDF editor font system (Dispatch B, lane B2): catalog discovery, embedding/subsetting
//! permission checks, glyph subsetting, & Type0/CID/ToUnicode assembly — `docs/PDF-EDITOR-NOW.md`
//! §7.
//!
//! Owns everything the frozen `compose_candidate(source, commands, fonts: &dyn PdfFontResolver)`
//! seam (`docs/PDF-EDITOR-DISPATCH.md`) needs on the font side: a concrete `PdfFontResolver`
//! ([`FontCatalog`]) that `pdf_compose.rs` (lane C1) can construct and pass in, plus
//! [`subset_for_pdf`], which `pdf_compose.rs` calls once it has decided (§7.3) that a selected
//! catalog font must be embedded rather than reused from the source PDF. Wire/shared types
//! (`PdfFontCatalogEntry`, `PdfFontSource`, `ResolvedPdfFont`, `GlyphId`, `PdfFontResolver`,
//! `PdfEditError`) live in `pdf_model.rs` (lane A1); this file only implements behavior against
//! them.
//!
//! ## §7.1 dependency roles used here
//! - `fontdb` enumerates system fonts (family/style/source) without eagerly reading every font's
//!   bytes.
//! - `ttf-parser` reads metrics (`hmtx`), glyph indices (`cmap`), & the OS/2 `fsType` embedding/
//!   subsetting permission bits (`Face::permissions`/`is_outline_embedding_allowed`/
//!   `is_subsetting_allowed` — names verified against 0.25 docs per §7.1's note and used here
//!   exactly as `pdf_model.rs`'s doc comments and §7.4's `subset_for_pdf` example show them).
//! - `subsetter` (`subsetter::subset` + `GlyphRemapper`) produces the actual PDF-oriented TrueType
//!   glyph subset embedded as `FontFile2`.
//!
//! ## Bundled fonts
//! §7.2 says the catalog "includes bundled fonts such as Source Serif 4 plus eligible system
//! fonts." Source Serif 4's WOFF2 bytes are compiled into this crate and decoded in memory with
//! `wuff` before `ttf-parser`/`subsetter` inspect them. Installed apps therefore never depend on
//! repository paths or separately packaged native resources.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::model::{
    GlyphId, PdfEditError, PdfFontCatalogEntry, PdfFontResolver, PdfFontSource, ResolvedPdfFont,
};

// -------------------------------------------------------------------------------------------
// §7.2 — Catalog
// -------------------------------------------------------------------------------------------

/// One bundled font compiled into this crate (or injected directly for tests via
/// [`FontCatalog::from_parts`]).
#[derive(Clone, Debug)]
struct BundledFontEntry {
    id: String,
    family: String,
    postscript_name: Option<String>,
    style: String,
    bytes: Vec<u8>,
    face_index: u32,
}

/// Resolves the font catalog (§7.2): bundled fonts plus system fonts discovered via `fontdb`.
/// Implements [`PdfFontResolver`] so `pdf_compose.rs` (lane C1) can build the frozen
/// `compose_candidate` seam against a real resolver.
pub struct FontCatalog {
    db: fontdb::Database,
    bundled: Vec<BundledFontEntry>,
}

impl FontCatalog {
    /// Builds a catalog from embedded bundled fonts plus installed system fonts. System-font
    /// discovery is host-dependent; prefer [`FontCatalog::from_parts`] in deterministic tests.
    pub fn discover() -> FontCatalog {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let bundled = discover_bundled_fonts();
        FontCatalog { db, bundled }
    }

    /// Process-wide cached catalog for `list_pdf_fonts` — `fontdb::Database::load_system_fonts`
    /// walks the host font directories, so this only pays that cost once per process, matching the
    /// warm-cache convention `system_fonts.rs` already uses for `list_system_fonts`.
    pub fn shared() -> &'static FontCatalog {
        static CATALOG: OnceLock<FontCatalog> = OnceLock::new();
        CATALOG.get_or_init(FontCatalog::discover)
    }

    /// Builds a catalog from already-known bundled entries and an already-populated `fontdb`
    /// database, with no scanning of its own. Used by tests that need fully deterministic font
    /// bytes instead of whatever happens to be installed on the host running the test.
    #[cfg(test)]
    fn from_parts(db: fontdb::Database, bundled: Vec<BundledFontEntry>) -> FontCatalog {
        FontCatalog { db, bundled }
    }

    /// Every catalog entry (§7.2's `list_pdf_fonts` payload): bundled fonts first, then system
    /// fonts grouped by (family, style) so a family with multiple weights/styles yields one entry
    /// per style rather than one per physical font file.
    pub fn list_entries(&self) -> Vec<PdfFontCatalogEntry> {
        let mut entries: Vec<PdfFontCatalogEntry> = Vec::new();

        for bundled in &self.bundled {
            let (embeddable, subsettable) = self
                .parse_permissions(&bundled.bytes, bundled.face_index)
                .unwrap_or((false, false));
            entries.push(PdfFontCatalogEntry {
                id: bundled.id.clone(),
                family: bundled.family.clone(),
                postscript_name: bundled.postscript_name.clone(),
                styles: vec![bundled.style.clone()],
                embeddable,
                subsettable,
                source: PdfFontSource::Bundled,
            });
        }

        // Group system faces by family so `styles` aggregates every style fontdb reports for that
        // family instead of duplicating one catalog row per face.
        let mut by_family: BTreeMap<String, (String, Option<String>, Vec<String>)> =
            BTreeMap::new();
        let mut first_id_by_family: BTreeMap<String, fontdb::ID> = BTreeMap::new();

        for face in self.db.faces() {
            let Some((family, _)) = face
                .families
                .iter()
                .find(|(_, lang)| *lang == fontdb::Language::English_UnitedStates)
                .or_else(|| face.families.first())
                .cloned()
            else {
                continue;
            };
            let style = format_style(face.style, face.weight, face.stretch);
            let entry = by_family.entry(family.clone()).or_insert_with(|| {
                (
                    family.clone(),
                    Some(face.post_script_name.clone()),
                    Vec::new(),
                )
            });
            if !entry.2.contains(&style) {
                entry.2.push(style);
            }
            first_id_by_family.entry(family).or_insert(face.id);
        }

        for (family, (_, postscript_name, styles)) in by_family {
            let Some(&face_id) = first_id_by_family.get(&family) else {
                continue;
            };
            let (embeddable, subsettable) = self
                .parse_permissions_by_id(face_id)
                .unwrap_or((false, false));
            entries.push(PdfFontCatalogEntry {
                id: system_font_id(&family),
                family,
                postscript_name,
                styles,
                embeddable,
                subsettable,
                source: PdfFontSource::System,
            });
        }

        entries
    }

    fn parse_permissions(&self, bytes: &[u8], face_index: u32) -> Option<(bool, bool)> {
        let face = ttf_parser::Face::parse(bytes, face_index).ok()?;
        Some(face_embedding_permissions(&face))
    }

    fn parse_permissions_by_id(&self, id: fontdb::ID) -> Option<(bool, bool)> {
        self.db
            .with_face_data(id, |bytes, index| {
                ttf_parser::Face::parse(bytes, index)
                    .ok()
                    .map(|face| face_embedding_permissions(&face))
            })
            .flatten()
    }
}

impl PdfFontResolver for FontCatalog {
    fn resolve(&self, id: &str) -> Result<ResolvedPdfFont, PdfEditError> {
        if let Some(bundled) = self.bundled.iter().find(|entry| entry.id == id) {
            let (embeddable, subsettable) = self
                .parse_permissions(&bundled.bytes, bundled.face_index)
                .unwrap_or((false, false));
            return Ok(ResolvedPdfFont {
                id: bundled.id.clone(),
                family: bundled.family.clone(),
                bytes: bundled.bytes.clone(),
                face_index: bundled.face_index,
                embeddable,
                subsettable,
            });
        }

        // System lookup mirrors `list_entries`' id scheme (`system_font_id(family)`), so a catalog
        // id round-trips back to the first face fontdb reports for that family.
        for face in self.db.faces() {
            let Some((family, _)) = face.families.first().cloned() else {
                continue;
            };
            if system_font_id(&family) != id {
                continue;
            }
            let resolved = self.db.with_face_data(face.id, |bytes, face_index| {
                let (embeddable, subsettable) = ttf_parser::Face::parse(bytes, face_index)
                    .ok()
                    .map(|f| face_embedding_permissions(&f))
                    .unwrap_or((false, false));
                ResolvedPdfFont {
                    id: id.to_string(),
                    family: family.clone(),
                    bytes: bytes.to_vec(),
                    face_index,
                    embeddable,
                    subsettable,
                }
            });
            if let Some(resolved) = resolved {
                return Ok(resolved);
            }
        }

        Err(PdfEditError::UnsupportedEncoding)
    }

    fn glyphs_for_text(
        &self,
        font: &ResolvedPdfFont,
        text: &str,
    ) -> Result<Vec<GlyphId>, PdfEditError> {
        glyph_ids_for_text(&font.bytes, font.face_index, text)
    }
}

/// The one implementation of `text` → glyph ids through a font program's cmap: parse `bytes` at
/// `face_index`, then map every char, failing with [`PdfEditError::MissingGlyph`] on the first char
/// the font has no glyph for (the all-or-nothing contract §7.3 rule 3 relies on). `pub(crate)` so
/// `pdf_compose.rs`'s test-only `PdfFontResolver` mocks share the real algorithm instead of
/// reimplementing — and silently drifting from — it.
pub(crate) fn glyph_ids_for_text(
    bytes: &[u8],
    face_index: u32,
    text: &str,
) -> Result<Vec<GlyphId>, PdfEditError> {
    let face = ttf_parser::Face::parse(bytes, face_index)
        .map_err(|_| PdfEditError::UnsupportedEncoding)?;
    text.chars()
        .map(|c| {
            face.glyph_index(c)
                .map(|g| GlyphId(g.0))
                .ok_or(PdfEditError::MissingGlyph)
        })
        .collect()
}

/// §7.4's `is_outline_embedding_allowed`/`is_subsetting_allowed` + `permissions()` gate, factored
/// out so both catalog listing (§7.2's `embeddable`/`subsettable` columns) and [`subset_for_pdf`]
/// (§7.4's hard reject) agree on exactly one definition. Returns `(embeddable, subsettable)`.
fn face_embedding_permissions(face: &ttf_parser::Face) -> (bool, bool) {
    let restricted = matches!(
        face.permissions(),
        Some(ttf_parser::Permissions::Restricted)
    );
    let embeddable = !restricted && face.is_outline_embedding_allowed();
    let subsettable = !restricted && face.is_subsetting_allowed();
    (embeddable, subsettable)
}

fn format_style(style: fontdb::Style, weight: fontdb::Weight, stretch: fontdb::Stretch) -> String {
    let mut parts = Vec::new();
    if stretch != fontdb::Stretch::Normal {
        parts.push(format!("{stretch:?}"));
    }
    match weight.0 {
        400 => {}
        700 => parts.push("Bold".to_string()),
        other => parts.push(format!("{other}")),
    }
    match style {
        fontdb::Style::Normal => {}
        fontdb::Style::Italic => parts.push("Italic".to_string()),
        fontdb::Style::Oblique => parts.push("Oblique".to_string()),
    }
    if parts.is_empty() {
        "Regular".to_string()
    } else {
        parts.join(" ")
    }
}

/// Stable catalog id for a system family. Deliberately not the raw family string (§7.2: "Font ID,
/// not family label, crosses Save boundary") — this keeps ids filesystem/JSON-key friendly and
/// collision-free-in-practice for typical family names.
fn system_font_id(family: &str) -> String {
    let slug: String = family
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("system-{slug}")
}

/// Compressed web-font bytes become part of `rightkit-pdf` itself. `include_bytes!` also makes
/// Cargo track the font as an input, while `wuff` reconstructs SFNT bytes once per process when
/// [`FontCatalog::shared`] initializes.
const SOURCE_SERIF_4_WOFF2: &[u8] =
    include_bytes!("../assets/SourceSerif4-Variable.woff2");

fn bundled_font_id(family: &str, postscript_name: Option<&str>) -> String {
    // Frontend PDF edit state persists this exact id. Source Serif 4's variable WOFF2 metadata
    // uses a PostScript name that varies between upstream releases, so key it by stable family.
    if family.eq_ignore_ascii_case("Source Serif 4") {
        return "source-serif-4".to_string();
    }
    postscript_name
        .unwrap_or(family)
        .to_ascii_lowercase()
        .replace(' ', "-")
}

fn discover_bundled_fonts() -> Vec<BundledFontEntry> {
    let Some(bytes) = wuff::decompress_woff2(SOURCE_SERIF_4_WOFF2).ok() else {
        return Vec::new();
    };
    let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else {
        return Vec::new();
    };
    let family = face
        .names()
        .into_iter()
        .find(|name| name.name_id == ttf_parser::name_id::FAMILY && name.is_unicode())
        .and_then(|name| name.to_string())
        .unwrap_or_else(|| "Source Serif 4".to_string());
    let postscript_name = face
        .names()
        .into_iter()
        .find(|name| name.name_id == ttf_parser::name_id::POST_SCRIPT_NAME && name.is_unicode())
        .and_then(|name| name.to_string());
    let style = if face.is_bold() && face.is_italic() {
        "Bold Italic"
    } else if face.is_bold() {
        "Bold"
    } else if face.is_italic() {
        "Italic"
    } else {
        "Regular"
    }
    .to_string();

    vec![BundledFontEntry {
        id: bundled_font_id(&family, postscript_name.as_deref()),
        family,
        postscript_name,
        style,
        bytes,
        face_index: 0,
    }]
}

// -------------------------------------------------------------------------------------------
// §7.4 — Embedded Type0/CID/ToUnicode subset
// -------------------------------------------------------------------------------------------

/// One glyph's advance width for the embedded `/W` array, in `/1000` text-space units (matching
/// every other width convention in this codebase — see `pdf_model.rs::TextState::glyph_quad`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfCidWidth {
    pub cid: u32,
    pub width_1000: f64,
}

/// Result of [`subset_for_pdf`]: everything `pdf_compose.rs` (lane C1) needs to emit §7.4's
/// `Type0`/`CIDFontType2`/`FontFile2`/`ToUnicode` object graph for one embedded font subset.
///
/// This implementation assigns CIDs to equal the subset font's own new (post-subsetting) glyph
/// ids — i.e. `/CIDToGIDMap /Identity` is always correct for a subset this function produced,
/// so callers never need a separate CIDToGIDMap stream. `/Encoding /Identity-H` (§7.4) then maps
/// each 2-byte code directly to that CID/GID.
#[derive(Clone, Debug)]
pub struct EmbeddedPdfFontSubset {
    /// Subset TrueType font program bytes for the `FontFile2` stream.
    pub font_program: Vec<u8>,
    /// `/W` array entries, one per CID actually used by `text`. Order matches first-appearance
    /// order in `text`, not sorted — `pdf_compose.rs` is free to sort/compact before emitting.
    pub widths: Vec<PdfCidWidth>,
    /// `(cid, unicode)` pairs for the generated `ToUnicode` CMap — see [`build_to_unicode_cmap`].
    pub to_unicode: Vec<(u32, String)>,
    /// Number of glyphs in `font_program` (subset `maxp.numGlyphs`), for callers that want to
    /// sanity-check `/CIDToGIDMap /Identity` bounds before composing. Not yet read by any caller
    /// in this crate — part of the struct's documented API surface for a future consumer.
    #[allow(dead_code)]
    pub glyph_count: u16,
}

/// Subsets `bytes` (a resolved catalog font's program, `face_index` `0` for a single-face file) to
/// exactly the glyphs `text` needs, per §7.4's `subset_for_pdf`. Rejects up front (before touching
/// the subsetter) when the font's OS/2 `fsType` forbids outline embedding or subsetting — §7.3
/// rule 3: "If selected font cannot embed or lacks a glyph, fail before candidate creation."
pub fn subset_for_pdf(
    bytes: &[u8],
    face_index: u32,
    text: &str,
) -> Result<EmbeddedPdfFontSubset, PdfEditError> {
    let face = ttf_parser::Face::parse(bytes, face_index)
        .map_err(|_| PdfEditError::UnsupportedEncoding)?;

    let (embeddable, subsettable) = face_embedding_permissions(&face);
    if !embeddable || !subsettable {
        return Err(PdfEditError::FontEmbeddingDenied);
    }

    let units_per_em = face.units_per_em() as f64;
    if units_per_em <= 0.0 {
        return Err(PdfEditError::UnsupportedEncoding);
    }

    // First-appearance-ordered, de-duplicated (char, old_gid) pairs. Missing a glyph fails the
    // whole subset — §7.3 rule 3, and matches `PdfFontResolver::glyphs_for_text`'s all-or-nothing
    // contract so a caller that already validated glyph coverage via that trait method never hits
    // a surprise here.
    let mut seen_glyphs: Vec<u16> = Vec::new();
    let mut char_glyphs: Vec<(char, u16)> = Vec::new();
    for c in text.chars() {
        let gid = face.glyph_index(c).ok_or(PdfEditError::MissingGlyph)?.0;
        if !seen_glyphs.contains(&gid) {
            seen_glyphs.push(gid);
        }
        char_glyphs.push((c, gid));
    }

    let mut remapper = subsetter::GlyphRemapper::new();
    // `old_gid -> new_gid`, populated in the exact order we call `remap`, which is also the order
    // `subsetter::subset` uses to build the output font — safe to rely on regardless of whatever
    // internal storage `GlyphRemapper` uses, since we only ever read back values we ourselves
    // pushed.
    let mut old_to_new: BTreeMap<u16, u16> = BTreeMap::new();
    for &old_gid in &seen_glyphs {
        let new_gid = remapper.remap(old_gid);
        old_to_new.insert(old_gid, new_gid);
    }

    let font_program = subsetter::subset(bytes, face_index, &remapper)
        .map_err(|_| PdfEditError::UnsupportedEncoding)?;

    let mut widths = Vec::with_capacity(seen_glyphs.len());
    for &old_gid in &seen_glyphs {
        let new_gid = old_to_new[&old_gid];
        let advance = face
            .glyph_hor_advance(ttf_parser::GlyphId(old_gid))
            .unwrap_or(0);
        widths.push(PdfCidWidth {
            cid: new_gid as u32,
            width_1000: advance as f64 * 1000.0 / units_per_em,
        });
    }

    // ToUnicode: first char that mapped to each new CID wins (mirrors `widths`' first-appearance
    // convention above); later duplicate chars sharing a glyph don't need a second CMap entry.
    let mut to_unicode: Vec<(u32, String)> = Vec::new();
    let mut cid_has_unicode: BTreeMap<u32, ()> = BTreeMap::new();
    for (c, old_gid) in &char_glyphs {
        let new_gid = old_to_new[old_gid] as u32;
        if cid_has_unicode.insert(new_gid, ()).is_none() {
            to_unicode.push((new_gid, c.to_string()));
        }
    }

    Ok(EmbeddedPdfFontSubset {
        font_program,
        widths,
        to_unicode,
        glyph_count: seen_glyphs.len() as u16 + 1, // +1 for glyph 0 (.notdef), always present
    })
}

/// Builds a generated Unicode `ToUnicode` CMap (§7.4) as PDF stream text, using `bfchar` entries —
/// simplest correct form for a subset whose CIDs (per this module's `/CIDToGIDMap /Identity`
/// convention) are small, sparse, non-contiguous glyph ids, where `bfrange` compaction buys little.
/// `entries` need not be sorted; output is sorted by `cid` for a deterministic, diff-friendly
/// stream.
pub fn build_to_unicode_cmap(entries: &[(u32, String)]) -> String {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(cid, _)| *cid);

    let mut out = String::new();
    out.push_str("/CIDInit /ProcSet findresource begin\n");
    out.push_str("12 dict begin\nbegincmap\n");
    out.push_str("/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    out.push_str("/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n");
    out.push_str("1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");
    out.push_str(&format!("{} beginbfchar\n", sorted.len()));
    for (cid, unicode) in &sorted {
        out.push_str(&format!(
            "<{:04X}> <{}>\n",
            cid,
            unicode_to_utf16be_hex(unicode)
        ));
    }
    out.push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

fn unicode_to_utf16be_hex(text: &str) -> String {
    let mut hex = String::new();
    for unit in text.encode_utf16() {
        hex.push_str(&format!("{unit:04X}"));
    }
    hex
}

// -------------------------------------------------------------------------------------------
// Tests (§14.1 font fixtures)
// -------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::build_test_ttf;

    // ---------------------------------------------------------------------------------------
    // §14.1 fixtures
    // ---------------------------------------------------------------------------------------

    const FS_TYPE_INSTALLABLE: u16 = 0x0000;
    const FS_TYPE_RESTRICTED: u16 = 0x0002; // bit1: Restricted License embedding
    const FS_TYPE_NO_SUBSETTING: u16 = 0x0100; // bit8: No subsetting

    fn installable_font() -> Vec<u8> {
        build_test_ttf(FS_TYPE_INSTALLABLE, &[('A', 1), ('B', 2), ('C', 3)], 600)
    }

    #[test]
    fn installable_font_parses_and_reports_embeddable_subsettable() {
        let bytes = installable_font();
        let face = ttf_parser::Face::parse(&bytes, 0).expect("valid synthetic font");
        let (embeddable, subsettable) = face_embedding_permissions(&face);
        assert!(embeddable);
        assert!(subsettable);
    }

    #[test]
    fn restricted_font_is_not_embeddable() {
        let bytes = build_test_ttf(FS_TYPE_RESTRICTED, &[('A', 1)], 600);
        let face = ttf_parser::Face::parse(&bytes, 0).expect("valid synthetic font");
        let (embeddable, subsettable) = face_embedding_permissions(&face);
        assert!(!embeddable);
        assert!(!subsettable);
    }

    #[test]
    fn subset_for_pdf_rejects_embedding_restricted_font() {
        let bytes = build_test_ttf(FS_TYPE_RESTRICTED, &[('A', 1)], 600);
        let err = subset_for_pdf(&bytes, 0, "A").expect_err("restricted font must reject");
        assert!(matches!(err, PdfEditError::FontEmbeddingDenied));
    }

    #[test]
    fn subset_for_pdf_rejects_subset_disallowed_font() {
        let bytes = build_test_ttf(FS_TYPE_NO_SUBSETTING, &[('A', 1)], 600);
        let err = subset_for_pdf(&bytes, 0, "A").expect_err("no-subsetting font must reject");
        assert!(matches!(err, PdfEditError::FontEmbeddingDenied));
    }

    #[test]
    fn subset_for_pdf_reports_missing_glyph() {
        let bytes = installable_font(); // only maps A, B, C
        let err = subset_for_pdf(&bytes, 0, "AZ").expect_err("Z has no glyph");
        assert!(matches!(err, PdfEditError::MissingGlyph));
    }

    #[test]
    fn subset_for_pdf_succeeds_for_covered_text() {
        let bytes = installable_font();
        let subset = subset_for_pdf(&bytes, 0, "ABA").expect("A and B are covered");
        // "ABA" uses 2 distinct glyphs (A, B); each gets exactly one /W entry.
        assert_eq!(subset.widths.len(), 2);
        assert!(subset
            .widths
            .iter()
            .all(|w| (w.width_1000 - 600.0).abs() < 1e-6));
        // ToUnicode has exactly one entry per distinct CID, not per character occurrence.
        assert_eq!(subset.to_unicode.len(), 2);
        let mut chars: Vec<&str> = subset
            .to_unicode
            .iter()
            .map(|(_, unicode)| unicode.as_str())
            .collect();
        chars.sort();
        assert_eq!(chars, vec!["A", "B"]);
        // Subset font program itself must still be a valid, parseable sfnt.
        ttf_parser::Face::parse(&subset.font_program, 0).expect("subset output is valid TTF");
    }

    #[test]
    fn subset_for_pdf_glyphs_for_text_agrees_with_resolver_trait() {
        let bytes = installable_font();
        let resolved = ResolvedPdfFont {
            id: "test-font".to_string(),
            family: "Test Font".to_string(),
            bytes: bytes.clone(),
            face_index: 0,
            embeddable: true,
            subsettable: true,
        };
        let catalog = FontCatalog::from_parts(fontdb::Database::new(), Vec::new());
        let glyphs = catalog
            .glyphs_for_text(&resolved, "AB")
            .expect("A and B are covered");
        assert_eq!(glyphs, vec![GlyphId(1), GlyphId(2)]);

        let err = catalog
            .glyphs_for_text(&resolved, "Z")
            .expect_err("Z is not covered");
        assert!(matches!(err, PdfEditError::MissingGlyph));
    }

    #[test]
    fn build_to_unicode_cmap_round_trips_generated_mapping() {
        let subset = subset_for_pdf(&installable_font(), 0, "AB").expect("covered text");
        let cmap = build_to_unicode_cmap(&subset.to_unicode);

        assert!(cmap.contains("beginbfchar"));
        assert!(cmap.contains("endbfchar"));
        assert!(cmap.contains("/CMapType 2 def"));

        // Round-trip check: every (cid, "A"/"B") pair must appear as `<CID_HEX> <UTF16BE_HEX>`.
        for (cid, unicode) in &subset.to_unicode {
            let expected = format!("<{cid:04X}> <{}>", unicode_to_utf16be_hex(unicode));
            assert!(
                cmap.contains(&expected),
                "missing {expected} in generated CMap:\n{cmap}"
            );
        }
    }

    #[test]
    fn build_to_unicode_cmap_is_sorted_by_cid_and_deterministic() {
        let entries = vec![
            (5u32, "b".to_string()),
            (1u32, "a".to_string()),
            (3u32, "c".to_string()),
        ];
        let cmap_a = build_to_unicode_cmap(&entries);
        let cmap_b = build_to_unicode_cmap(&entries);
        assert_eq!(cmap_a, cmap_b);
        let pos_1 = cmap_a.find("<0001>").expect("cid 1 present");
        let pos_3 = cmap_a.find("<0003>").expect("cid 3 present");
        let pos_5 = cmap_a.find("<0005>").expect("cid 5 present");
        assert!(pos_1 < pos_3 && pos_3 < pos_5);
    }

    #[test]
    fn unicode_to_utf16be_hex_encodes_bmp_text() {
        assert_eq!(unicode_to_utf16be_hex("A"), "0041");
        assert_eq!(unicode_to_utf16be_hex("AB"), "00410042");
    }

    #[test]
    fn font_catalog_from_parts_lists_bundled_entry_with_permissions() {
        let bundled = BundledFontEntry {
            id: "test-serif".to_string(),
            family: "Test Serif".to_string(),
            postscript_name: Some("TestSerif-Regular".to_string()),
            style: "Regular".to_string(),
            bytes: installable_font(),
            face_index: 0,
        };
        let catalog = FontCatalog::from_parts(fontdb::Database::new(), vec![bundled]);
        let entries = catalog.list_entries();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, "test-serif");
        assert_eq!(entry.source, PdfFontSource::Bundled);
        assert!(entry.embeddable);
        assert!(entry.subsettable);
    }

    #[test]
    fn font_catalog_resolve_returns_bundled_font_bytes() {
        let bytes = installable_font();
        let bundled = BundledFontEntry {
            id: "test-serif".to_string(),
            family: "Test Serif".to_string(),
            postscript_name: None,
            style: "Regular".to_string(),
            bytes: bytes.clone(),
            face_index: 0,
        };
        let catalog = FontCatalog::from_parts(fontdb::Database::new(), vec![bundled]);
        let resolved = catalog
            .resolve("test-serif")
            .expect("bundled font resolves");
        assert_eq!(resolved.bytes, bytes);
        assert!(resolved.embeddable);
    }

    #[test]
    fn font_catalog_resolve_unknown_id_is_unsupported_encoding() {
        let catalog = FontCatalog::from_parts(fontdb::Database::new(), Vec::new());
        let err = catalog
            .resolve("does-not-exist")
            .expect_err("unknown id must fail");
        assert!(matches!(err, PdfEditError::UnsupportedEncoding));
    }

    #[test]
    fn bundled_fonts_are_available_without_runtime_files() {
        assert_eq!(discover_bundled_fonts().len(), 1);
    }

    #[test]
    fn source_serif_4_woff2_is_discovered_resolved_parseable_and_subsettable() {
        let bundled = discover_bundled_fonts()
            .into_iter()
            .find(|font| font.id == "source-serif-4")
            .expect("embedded Source Serif 4 WOFF2 is available");
        let face = ttf_parser::Face::parse(&bundled.bytes, bundled.face_index)
            .expect("decoded Source Serif 4 is parseable SFNT");
        assert!(face.glyph_index('ç').is_some());
        assert!(face.glyph_index('ã').is_some());

        let catalog = FontCatalog::from_parts(fontdb::Database::new(), vec![bundled.clone()]);
        let entry = catalog
            .list_entries()
            .into_iter()
            .find(|entry| entry.id == "source-serif-4")
            .expect("Source Serif 4 catalog entry");
        assert_eq!(entry.source, PdfFontSource::Bundled);
        assert!(entry.embeddable);
        assert!(entry.subsettable);

        let resolved = catalog
            .resolve("source-serif-4")
            .expect("stable frontend font id resolves");
        assert_eq!(resolved.bytes, bundled.bytes);

        let correction = "A correção mantém ação, informação e português.";
        catalog
            .glyphs_for_text(&resolved, correction)
            .expect("Portuguese correction text has complete glyph coverage");
        let subset = subset_for_pdf(&resolved.bytes, resolved.face_index, correction)
            .expect("Source Serif 4 subset is embeddable");
        ttf_parser::Face::parse(&subset.font_program, 0)
            .expect("Source Serif 4 subset output is parseable SFNT");
        assert!(subset.widths.len() > 10);
        assert!(subset.to_unicode.iter().any(|(_, unicode)| unicode == "ç"));
        assert!(subset.to_unicode.iter().any(|(_, unicode)| unicode == "ã"));
    }

    #[test]
    fn bundled_font_id_stabilizes_source_serif_metadata() {
        assert_eq!(
            bundled_font_id("Source Serif 4", Some("SourceSerif4-Roman")),
            "source-serif-4"
        );
        assert_eq!(
            bundled_font_id("Test Serif", Some("TestSerif-Regular")),
            "testserif-regular"
        );
    }

    #[test]
    fn system_font_id_is_stable_and_slugified() {
        assert_eq!(system_font_id("Arial"), "system-arial");
        assert_eq!(system_font_id("Source Serif 4"), "system-source-serif-4");
        assert_eq!(system_font_id("Arial"), system_font_id("Arial"));
    }
}
