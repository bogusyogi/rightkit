# rightkit-typeset

Typst rendering for titles, captions and math, with LaTeX input through mitex. Separate from `rightkit-motion` so only title, caption and math users pay for Typst's dependency tree.

Catalog capability: `typeset`. Version `0.1.0`. Decision record: `docs/design/motion-owner.md` (rightkit#264).

## API

- `Typesetter::new(FontSource, ppi)` then `render(&Request) -> Result<Rendered, TypesetError>`.
- `Request` source is Typst markup, Typst math (`Source::Math`), or LaTeX (`Source::Latex`, feature `latex`). The page is auto-sized and transparent; text size, family and margin are fields.
- Output is SVG by default, PNG with feature `png`.
- `latex_to_typst(&str)` (feature `latex`) converts LaTeX math to Typst math.

The Typst world has no file access and no package fetching. Fonts are explicit: `FontSource::Bytes` for your own files, or `FontSource::Embedded` (feature `embedded-fonts`). Without fonts, `render` returns `TypesetError::NoFonts`. Callers pass caption strings; there is no transcript dependency.

## Cargo features

`latex` (mitex 0.2.4), `png` (typst-render 0.15.1), `embedded-fonts` (typst-assets). None is on by default. Base dependencies: `typst` and `typst-svg` 0.15.1.

## Licences

This crate is MIT OR Apache-2.0. Typst, typst-svg, typst-render and mitex are Apache-2.0. No fonts are bundled by default; the `embedded-fonts` feature brings Typst's fonts under their own licences, listed in [THIRD-PARTY.md](THIRD-PARTY.md). A cargo-deny licence run on CI over Typst's full dependency tree is a precondition of the first publish.

Apps should not depend on `typst`, `typst-render` or `mitex` directly; the ownership catalog advises on those dependencies.
