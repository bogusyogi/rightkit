# Changelog

## 0.1.0 - 2026-10-17

Initial version (rightkit#264).

- `Typesetter`, `Request`, `Rendered`: Typst rendering to SVG (and PNG with feature `png`) with no file access and no package fetching.
- `latex_to_typst` and `Source::Latex` through mitex 0.2.4 (feature `latex`).
- Feature `embedded-fonts` for Typst's embedded fonts; see THIRD-PARTY.md.
- Written without a compile against typst 0.15.1; CI is the first compiler.
