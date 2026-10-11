# rightkit-pdf

Product-neutral PDF engine for Right Suite apps. It owns PDF inspection, composition (replace source text, add text, OCR correction), font subsetting (Source Serif 4 is bundled), staged atomic save, and, behind features, page merge and selection and whole-document or per-page text extraction. It is pure Rust: no Tauri, no PDFium.

**Use it** to read, edit, merge, or extract text from PDF files in a Right Suite app, when the output must be written atomically.

**Do not use it** to render PDFs to images, to run PDFium-based rendering, or to handle PDFs that need a full layout engine. Extraction (`extract`) uses `pdf-extract`, and its quality depends on the source file.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `fonts` | on | `fontdb`, `ttf-parser`, `subsetter`, `wuff` | Font catalog, parsing and subsetting (`src/font.rs`). |
| `edit` | on | `getrandom`, implies `fonts` | Compose, staged save and atomic commit (`src/compose.rs`, `src/save.rs`, `src/commit.rs`). |
| `merge` | on | none (built on `lopdf`, which is always a dependency) | Page merge and selection (`src/merge.rs`). |
| `extract` | off | `pdf-extract` | Whole-document and per-page text extraction (`src/extract.rs`). |

Inspection (`src/inspect.rs`, `src/model.rs`) is always compiled. `--no-default-features` gives inspection only.

## Platform support

Pure Rust with no platform-specific code (no `cfg(target_os)` gates in `src/`). It is expected to build wherever its dependencies build. The bundled font is `assets/SourceSerif4-Variable.woff2` (SIL OFL 1.1).

Published version: see `INDEX.md` at the repository root. Changes: `CHANGELOG.md`.

## Example

Illustrative. For compiled round trips on real files, see `tests/e2e.rs` and `tests/corpus_qa.rs`.

```rust,ignore
// Run with the default feature set. The public functions are in src/inspect.rs,
// src/compose.rs, src/merge.rs, src/extract.rs, and src/save.rs.
```

## Editability and replacement contract

`PdfTextIsland::editable` answers one question: can a replacement be written through this island's own font (same appearance)? The page graph and `compose_candidate` read the same per-font table (`inspect::ReverseTable`), so they cannot disagree. When it is `false`, `rejection` is `unsupported_encoding` and the additive `editBlocker` names the reason:

| `editBlocker` | Meaning |
|---|---|
| `unsupported_font_type` | Type3, unknown subtype, a Type0 font whose CMap is not `Identity-H`, or no font resource |
| `no_reversible_encoding` | No code-to-character table the file defines exactly (embedded font with a built-in encoding, unreadable `/Encoding` dictionary, unknown base encoding, no code with a positive explicit width) |
| `missing_metrics` | A code has no width where the font requires authoritative metrics |
| `mixed_text_state` | The island mixes fonts, sizes, scales or spacing |
| `unmapped_glyphs` | A glyph has no Unicode value, so replacing the text would silently drop it |
| `spans_across_streams` | The island's source operands sit in more than one content stream |
| `positioned_text_run` | A word gap inside the island comes from a position move (`Td`, `Tm`, ...), or a positioning/state operator sits between two of its show operators, so an edit touching that seam could not be rebuilt faithfully |

`substitutionEditable` stays `true` for every non-rejected island: its glyphs can still be removed and the text redrawn in a catalog font by changing the appearance. Only `editable` promises an in-place write.

An editable island can still refuse a particular text: characters its font has no usable code for fail with `PdfEditError::MissingGlyphs`, wire code `missing_glyph`, message naming the characters (`PdfEditError::missing_chars()` returns them). Nothing is ever substituted.

Re-encoding rules (`inspect::ReverseTable`): a code is usable only when it decodes to exactly one character through the font's own table, the file defines that decode (a named `WinAnsi`/`MacRoman`/`Standard` base, `/Differences` entries, a `ToUnicode` CMap, or the Standard encoding of a non-embedded Standard-14 text face), and the file gives it a positive explicit width (`/Widths` or `/W`). For `Identity-H` fonts the character must map to exactly one CID. `/Differences` without a `/BaseEncoding` defines only the codes it lists. An `/Encoding` dictionary the PDF library cannot parse (it then falls back to StandardEncoding) is refused, not guessed.

Word gaps. TeX-class producers write no space glyphs: a word gap is a `TJ` adjustment. A gap whose pen distance (beyond what `Tc` adds) exceeds `GAP_SPACE_EM` = 0.2 em, with no real space beside it, appears in `text` as a synthesised space and in `gaps` (`charIndex`, `widthPt`). In-place writes diff the island text, widen the changed middle to whole glyphs, and rewrite only that middle: every other word, kern and gap stays byte for byte. An edit that keeps the glyph count overwrites each glyph in place, so the kerns between them stay where they were. Otherwise the middle is written as one run: a changed number of spaces regenerates `TJ` gaps with the same numbers as the gap it replaces (else the island's first gap), or uses a real space glyph when the font has one; with neither, the edit is refused (`missing_glyph` naming the space) rather than dropping the gap. Unchanged glyphs keep their pen positions when the replaced run keeps its advance, and move together by the exact difference otherwise (kerns inside a flattened word are not re-created).

## Fit check

A replacement is measured with the source font's own widths and the walker's pen rules (`Tc`, `Tw`, `Tz`, text-matrix and CTM scale), the same quantities the island box is built from. It must fit the available bounds within 0.05 pt, so a replacement no wider than the original never overflows its own box.

## Page content

`/Contents` may be a stream, an array, or an indirect reference to an array of streams. Text state flows from one stream into the next. Composition re-parses a stream strictly and refuses (`malformed_content`) one the parser cannot read to the end, rather than rewriting it with the tail dropped.

## Limits

- Glyph presence in a subset font is inferred from a positive explicit width, not read from the font program. A producer that writes real widths for glyphs it left out of the subset would let an unavailable code through.
- `ToUnicode` cannot say which of two CIDs is the glyph variant the author sees, so an `Identity-H` character mapped by more than one CID is left unencodable (`missing_glyph`).
- Layout is measured, not reflowed: text after an edited run on the same line keeps its position.
- A `ToUnicode` CMap is read for its `bfchar` and `bfrange` forms (scalar and array destinations). Any other form, an unbalanced entry, `usecmap`, or a CMap that exhausts the reader's work budget makes the font non-editable rather than partly decoded.
- Hostile files: CID widths and `ToUnicode` codes are limited to the two-byte code space, `/Widths` is bounded by its own length, `/Parent` chains stop at a cycle or depth 64, and streams decode under 256 MiB. This bounds the crate's own loops and allocations; it does not audit `lopdf` or the font libraries it calls.
- An edit is read back before it is returned: the island at the same position must read as exactly the requested text and fit the bounds as the walker now measures it. Read-back uses the same decoder as inspection, so it proves structure and geometry, not that a code is the glyph the author sees.
- `compose_candidate` redraws in a catalog font only when the appearance changes; a non-editable island with unchanged appearance is refused. An island whose source spans live in several content streams cannot be redrawn at all (`substitutionEditable: false`).

## Measuring coverage

`tests/corpus_coverage.rs` (ignored) rotates the first 5+ letter word of editable islands on page 1 of every PDF in `RIGHTKIT_PDF_CORPUS_DIR` and prints islands / editable / tried / ok / refusals per file, plus the `editBlocker` histogram of refused islands:

`RIGHTKIT_PDF_CORPUS_DIR=<dir> rightkit cargo test -p rightkit-pdf --test corpus_coverage -- --ignored --nocapture`

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-pdf`, and `rightkit cargo test -p rightkit-pdf --features extract` for extraction.
