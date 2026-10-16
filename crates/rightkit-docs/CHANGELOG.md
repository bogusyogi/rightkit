# Changelog

## 0.1.0 - unreleased

First version. Issue Orthic-Labs/rightkit#85 (requested by Pulse for the Zenon replacement; second consumer ViewRight).

- `read(Source, &ReadOptions) -> DocumentRead` and `write(&Path, &[Op], &WriteOptions) -> WriteReceipt` for DOCX, XLSX and PDF; format detected from content.
- DOCX: paragraphs with style names, tables, headers and footers, text and Markdown renderings; create from blocks, append blocks, replace text across runs, all as byte-range patches that keep every untouched part byte-identical.
- XLSX: sheets, used ranges, typed cells, formulas with cached results, selection by sheet and A1 range, `max_cells` and `max_chars` caps; create (rust_xlsxwriter); set cells and append rows by patching `<sheetData>` in place.
- PDF: read and per-page text through `rightkit-pdf` (`extract`), replace text, add text, select pages.
- Safety: size, entry, ratio and depth caps; no DOCTYPE or entities in any XML part; macro-enabled and encrypted files refused with typed errors; external references never followed; unsafe formulas refused; formula-looking text always a string.
- Writes: `rightkit_fs::AtomicWriter` with a last source-hash recheck before the rename; the receipt lists parts touched, losses and warnings.
- Engine choice and the third-party bake-off: BAKEOFF.md.

Proof on macOS (Apple silicon): `cargo test` runs one journey per format (create, read back, edit, re-read) and hands the result to the system's own consumer: `textutil` (Apple's Word importer) for DOCX, Quick Look for XLSX, CoreGraphics (`sips`) for PDF. LibreOffice and Excel were not used: LibreOffice is not installed on the Mac and driving Excel is blocked by the workspace's no-desktop-intrusion rule. Windows consumer proof: pending (Dell).
