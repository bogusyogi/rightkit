# rightkit-typeset

Typst rendering for titles, captions and math, with LaTeX input through mitex. Separate from `rightkit-motion` so only title, caption and math users pay for Typst's dependency tree.

Catalog capability: `typeset`. Version `0.2.0`. Decision record: `docs/design/motion-owner.md` (rightkit#264).

## API

- `Typesetter::new(FontSource, ppi)` then `render(&Request) -> Result<Rendered, TypesetError>`.
- `Request` source is Typst markup, Typst math (`Source::Math`), or LaTeX (`Source::Latex`, feature `latex`). The page is auto-sized and transparent; text size, family and margin are fields.
- Output is SVG by default, PNG with feature `png`.
- `latex_to_typst(&str)` (feature `latex`) converts LaTeX math to Typst math.

The Typst world has no file access and no package fetching. Fonts are explicit: `FontSource::Bytes` for your own files, or `FontSource::Embedded` (feature `embedded-fonts`). Without fonts, `render` returns `TypesetError::NoFonts`. Callers pass caption strings; there is no transcript dependency.

## Font advance measurement

`FontFace` reads TrueType/OpenType sfnt bytes independently of rendering. Public types are available at crate root & under `rightkit_typeset::font`.

```rust,no_run
use rightkit_typeset::{FontError, FontFace};

fn measure_caption(bytes: Vec<u8>, text: &str, font_px: u32) -> Result<u64, FontError> {
    let face = FontFace::parse(bytes)?;
    let measured = face.measure(text)?;
    // measured.advance_units: summed hmtx advances
    // measured.units_per_em: head.unitsPerEm
    // measured.missing: unique missing characters, in first-seen order
    Ok(measured.width_px(font_px))
}
```

Parse once & reuse `FontFace`. `units_per_em()`, `glyph_id(char)` & `advance_units(glyph)` expose individual metrics. Missing characters use glyph 0 (`.notdef`) advance; control characters contribute zero width. Glyphs beyond `numberOfHMetrics` share final advance. `width_px(u32)` scales summed units & rounds to nearest pixel.

Supports Unicode cmap formats 4 & 12, preferring format 12, plus first face of TTC collections. Kerning, ligatures, complex-script shaping & other cmap formats remain outside this API. `FontError` reports malformed reads, missing required tables or unsupported cmaps. No additional dependencies, fonts or I/O are introduced.

## Cargo features

`latex` (mitex 0.2.4), `png` (typst-render 0.15.1), `embedded-fonts` (typst-assets). None is on by default. Base dependencies: `typst` and `typst-svg` 0.15.1.

## Licences

This crate is MIT OR Apache-2.0. Typst, typst-svg, typst-render and mitex are Apache-2.0. No fonts are bundled by default; the `embedded-fonts` feature brings Typst's fonts under their own licences, listed in [THIRD-PARTY.md](THIRD-PARTY.md). A cargo-deny licence run on CI over Typst's full dependency tree is a precondition of the first publish.

Apps should not depend on `typst`, `typst-render` or `mitex` directly; the ownership catalog advises on those dependencies.
