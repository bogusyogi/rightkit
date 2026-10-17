# Changelog

## Unreleased

### Changed (rightkit#528, active content)

- OOXML packages holding embedded OLE objects or ActiveX controls are refused before any part is parsed, with the new `DocsError::ActiveContent` (wire code `active_content`). Refused parts: any `embeddings/` part, any `activeX/` part, and any part whose name starts with `oleObject`. Pinned by an inline test in `src/package.rs`.
- External relationships (hyperlinks, external targets) are still not followed and still listed as omitted; they are not refused.

## 0.1.1 - unreleased

- quick-xml 0.41 to 0.42. Names, attribute values and text now arrive as `&str` and `Cow<str>`, so the `decode()` calls are gone (event content is read through `Deref`, raw and un-normalised exactly as `decode()` returned it), `local` and the new `prefix_of` in `xmlutil` take `&str`, and every `local(e.name().as_ref())` call site is unchanged. The DOCTYPE and entity preflight, the 256-level depth cap, `check_end_names`, entity handling (the five predefined entities and numeric references, nothing else) and attribute normalisation are untouched.
- One deliberate difference: 0.42 validates UTF-8 while reading, so a part with invalid UTF-8 is now refused as `malformed` where 0.41 silently skipped the text or attribute it could not decode. Pinned by new tests.
- The namespace-binding cap change (256 to 128) does not apply: the walker uses `Reader`, not `NsReader`.
- calamine 0.36.1 still depends on quick-xml 0.41, so two copies link until calamine moves.

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
