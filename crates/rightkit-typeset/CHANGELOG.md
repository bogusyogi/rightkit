# Changelog

## 0.1.0 (2026-10-17)

- Added `font` module (rightkit#461, re-exported at the crate root): `FontFace::parse`, `units_per_em`, `glyph_id`, `advance_units`, `measure` and `Measurement::width_px`. Reads `hmtx` advances and `cmap` formats 4 and 12 from TrueType and OpenType sfnt bytes, plus the first face of a TTC, with no new dependency. Kerning, ligatures, shaping and other cmap formats are not covered; malformed or unsupported input is `FontError`.

## 0.1.0 - 2026-10-17

Initial version (rightkit#264).

- `Typesetter`, `Request`, `Rendered`: Typst rendering to SVG (and PNG with feature `png`) with no file access and no package fetching.
- `latex_to_typst` and `Source::Latex` through mitex 0.2.4 (feature `latex`).
- Feature `embedded-fonts` for Typst's embedded fonts; see THIRD-PARTY.md.
- Written without a compile against typst 0.15.1; CI is the first compiler.
