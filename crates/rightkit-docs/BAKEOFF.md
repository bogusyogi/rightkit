# Engine bake-off (2026-10-16)

Question: which libraries read, create and edit DOCX and XLSX for `rightkit-docs`? Method: one throwaway binary (not in the repo) over a small rich corpus, run on the Mac in release mode; then the crate's own tests on the same corpus shapes. Versions and licences were read from crates.io on 2026-10-16.

| Crate | Version | Licence | Released | MSRV | Role tried |
|---|---|---|---|---|---|
| docx-rs | 0.4.22 | MIT | 2026-07-21 | n/a | DOCX read, create, edit |
| calamine | 0.36.1 | MIT | 2026-07-27 | 1.88 | XLSX read |
| rust_xlsxwriter | 0.99.1 | MIT OR Apache-2.0 | 2026-09-14 | 1.88 | XLSX create |
| umya-spreadsheet | 3.1.1 | MIT | 2026-10-12 | 1.89 | XLSX edit |
| office_oxide | 0.1.15 | MIT OR Apache-2.0 | 2026-10-15 | 1.88 | DOCX and XLSX read, create, edit |
| zip / quick-xml | 8.6.0 (9.0.3 current) / 0.41.0 (0.42.0 current) | MIT | | | skew check |

Corpus: a hand-built DOCX (style, header, footer, comments, footnotes, a tracked insertion and deletion, a custom XML part, a table, a bold run splitting a phrase), and an XLSX from rust_xlsxwriter (chart, cell note, data validation, merged range, defined name, formula with cached result, two sheets).

## Results

| Test | docx-rs | office_oxide | umya-spreadsheet | calamine | in-house (chosen) |
|---|---|---|---|---|---|
| DOCX read round trip keeps parts | parts 10 -> 22; **drops** `customXml/item1.xml` and the footnote body | parts 10 -> 10, all markers kept | n/a | n/a | parts 10 -> 10, only touched parts rewritten |
| DOCX replace across two runs | not offered (model rewrite) | works, 1 replacement | n/a | n/a | works; receipt lists parts touched |
| DOCX read | structure only (3 children, no text helper) | text and Markdown incl. header, footer, footnote, comment | n/a | n/a | blocks with style names, tables, headers, footers; omissions named |
| XLSX edit keeps chart, note, defined name, merge | n/a | all kept | all kept; **lost one of four `dataValidation` entries** | n/a | all kept, parts untouched are raw copies |
| XLSX read typed cells and formula + cached result | n/a | text only through `plain_text`, typed through its model | n/a | typed values; formula text via `worksheet_formula` | uses calamine |
| XLSX create | n/a | works | n/a | n/a | uses rust_xlsxwriter |
| DOCTYPE with external entity in `document.xml` | rejected | **accepted**, entity left unexpanded in the text | not tested | not tested | refused (`forbidden_xml`) in every XML part |
| 40 MiB-of-spaces part, 962:1 ratio | **accepted** (22 ms) | rejected as truncated XML (an accident of the fixture, not a ratio guard) | not tested | not tested | refused (`archive_limit`) before parsing |
| Limits | none | process-wide statics (`set_max_package_bytes`, `set_max_text_chars`) | none | none | per-call `Limits` |
| Macro-enabled, encrypted | no handling | encrypted OOXML is an `UnsupportedFormat` error (read from its source, not run); macros not examined | n/a | n/a | typed `macro_enabled`, `encrypted` (tested) |
| Transitive weight added | zip 8, quick-xml 0.41, crc32fast, base64 | zip **9**, atoi_simd, fast-float2, encoding_rs, cfb reader | 21 required deps (crates.io metadata) incl. aes, cbc, hmac, chrono, jiff, rand | zip 8, quick-xml 0.41, encoding_rs | zip 8 and quick-xml 0.41 shared with calamine and rust_xlsxwriter |

## Choice

- **XLSX read: calamine 0.36.1.** Typed cells, shared strings, dates, formula text, cached results, in the same zip 8 and quick-xml 0.41 line as rust_xlsxwriter.
- **XLSX create: rust_xlsxwriter 0.99.1**, with the `<v>0</v>` placeholder it writes for a formula given no result removed, so a reader never sees a computed 0 nobody computed.
- **XLSX edit and every DOCX operation: in-house byte-range patches over `zip` 8.6 and `quick-xml` 0.41.** umya-spreadsheet was dropped: it lost data validation in a plain set-cell round trip and brings 21 dependencies including crypto. docx-rs was dropped for editing: it re-serialises through a model and loses parts. It was also dropped for creation, which in-house does in about 150 lines with full control of styles and numbering.
- **office_oxide was the strongest third-party candidate and edited as losslessly as the in-house code on this corpus**, including the cross-run replace. It was not adopted as the engine because (1) it is under six months old, a single author, 0.1.x; (2) its limits are process-wide statics, which a shared crate used by several products in one process cannot set per call; (3) it accepts a DOCTYPE part and has no ratio or entry guard, so every guard would still be ours, leaving it only the parts we would rewrite anyway; (4) it is on zip 9 while calamine and rust_xlsxwriter are on zip 8, so adopting it duplicates the archive stack; (5) it gives no receipt of what an edit changed. Revisit it if it reaches 1.0 with per-call limits.
- **PDF: rightkit-pdf**, unchanged.

Version skew to watch: crates.io has `zip` 9.0.3 and `quick-xml` 0.42.0; calamine 0.36.1 and rust_xlsxwriter 0.99.1 pin the 8.x and 0.41 lines, and this crate follows them so one copy of each links. `pdf-extract` still pulls `lopdf` 0.42 beside `rightkit-pdf`'s 0.44. When calamine moves to zip 9 and quick-xml 0.42, move this crate in the same release.
