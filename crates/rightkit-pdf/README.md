# rightkit-pdf

Product-neutral PDF engine for Right Suite apps. It owns PDF inspection, composition (replace source text, add text, OCR correction), font subsetting (Source Serif 4 is bundled), staged atomic save, and, behind features, page merge and selection and whole-document or per-page text extraction. It is pure Rust: no Tauri, no PDFium.

**Use it** to read, edit, merge, or extract text from PDF files in a Right Suite app, when the output must be written atomically.

**Do not use it** to render PDFs to images, to run PDFium-based rendering, or to handle PDFs that need a full layout engine. Extraction (`extract`) uses `pdf-extract`, and its quality depends on the source file.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `merge` | on | none (built on `lopdf`, which is always a dependency) | Page merge and selection (`src/merge.rs`). |
| `extract` | off | `pdf-extract` | Whole-document and per-page text extraction (`src/extract.rs`). |

## Platform support

Pure Rust with no platform-specific code (no `cfg(target_os)` gates in `src/`). It is expected to build wherever its dependencies build. The bundled font is `assets/SourceSerif4-Variable.woff2` (SIL OFL 1.1).

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative. For compiled round trips on real files, see `tests/e2e.rs` and `tests/corpus_qa.rs`.

```rust,ignore
// Run with the default feature set. The public functions are in src/inspect.rs,
// src/compose.rs, src/merge.rs, src/extract.rs, and src/save.rs.
```

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-pdf`, and `rightkit cargo test -p rightkit-pdf --features extract` for extraction.
