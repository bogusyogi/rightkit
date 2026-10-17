# Third-party notices: rightkit-typeset

DRAFT. Licences below were read from crates.io metadata and the Typst project's documentation, not from a cargo-deny run. Verify against the exact build before publishing, and carry these into each app's generated `THIRD_PARTY_NOTICES.md` (see `legal/THIRD_PARTY_NOTICES_REQUIREMENTS.md`).

## Code dependencies

| Crate | Version | Licence |
| --- | --- | --- |
| typst | 0.15.1 | Apache-2.0 |
| typst-svg | 0.15.1 | Apache-2.0 |
| typst-render (feature `png`) | 0.15.1 | Apache-2.0 |
| mitex (feature `latex`) | 0.2.4 | Apache-2.0 |
| typst-assets (feature `embedded-fonts`) | 0.15 (to confirm) | Apache-2.0 for the crate; fonts below |

Typst's full transitive dependency tree is unchecked.

## Embedded fonts (feature `embedded-fonts` only)

No fonts are bundled by default. With `embedded-fonts`, typst-assets embeds the font files Typst ships. To confirm against the exact typst-assets version, they are:

| Font family | Licence |
| --- | --- |
| Libertinus Serif | SIL Open Font License 1.1 |
| New Computer Modern, New Computer Modern Math | SIL Open Font License 1.1 (GUST Font License lineage; confirm) |
| DejaVu Sans Mono | Bitstream Vera licence with DejaVu changes (permissive) |

The OFL requires the copyright notice and licence text to accompany redistributed font files, and forbids selling the fonts on their own. Add the exact notices from the typst-assets release to the app's notices file when the feature ships.

Never bundle ManimGX or x264.
