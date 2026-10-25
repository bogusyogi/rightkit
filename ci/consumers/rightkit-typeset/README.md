# rightkit-typeset 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-typeset =0.1.0` exactly with its
default features, which is the feature set CutRight uses. The default set is empty: no `latex`, no
`png` and no `embedded-fonts`. It runs on Windows and macOS in hosted qualification and uses no font
file from the host.

The program builds its own font. `render_font()` writes a 96-glyph TrueType face byte by byte
(`head`, `hhea`, `maxp`, `hmtx`, `cmap` format 12, `loca`, `glyf`, `name`): U+0020 to U+007E map to
glyphs 1 to 95, every glyph is 600 units wide, and only the letter A (glyph 34) has an outline, a
box. Typst chooses its math font from its own default family names, so the face carries the name
`Libertinus Serif`, the fallback Typst looks for. It is a fixture and is not that typeface.

What it proves, in plain sentences, through the public API only:

- `Request`: the defaults (24 pt, 4 pt margin, SVG output, no family), the exact Typst document
  `to_typst` produces for markup and for math, and the escaping of a family name containing a quote
  and a backslash.
- `Typesetter::new`: non-finite and non-positive `ppi` values fall back to 144, a positive one is
  kept; `render` without fonts, and with an empty font list, returns `TypesetError::NoFonts`; bytes
  that are not a font return `UnreadableFonts`; the `Display` text of every `TypesetError` variant.
- `FontFace` and `Measurement`: `parse`, `units_per_em`, `glyph_id`, `advance_units`, `measure`
  (listing each missing character once, ignoring control characters, using the `.notdef` advance)
  and `width_px` (one rounding after summing), for a format 4 cmap, a format 12 cmap reaching beyond
  the BMP, both orders of a mixed cmap, the first face of a collection, and glyphs past
  `numberOfHMetrics`. Each `FontError` is checked by value: `Malformed` (empty, junk and truncated
  input), `MissingTable` and `NoUsableCmap`, with their `Display` text.
- A real Typst compile and SVG render through the built font: the page auto-sizes to the text plus
  both margins (five A at 24 pt is 80 pt wide, ten is 152 pt, five at 12 pt with no margin is 36
  pt), the A outline is drawn, output is identical on a second render, markup headings and math
  compile, and `#image`, `#read`, a package `#import`, `#include` and bad syntax each return
  `TypesetError::Compile`.

What it deliberately does not prove: rendering with a real typeface, LaTeX input (`latex`), PNG
output (`png`), Typst's embedded fonts (`embedded-fonts`), kerning, ligatures or complex-script
shaping, or the `NoPages` and `Encode` errors, which this API surface does not reach with a valid
font. The Typst dependency tree is large, so the first build of this consumer is slow; the run itself
takes well under a second.

State: in-memory values only. No network, no files, no process spawn, no user data. Run only by the
CI consumer lane.
