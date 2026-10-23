# rightkit-docs

Product-neutral read and write for DOCX, XLSX and PDF, behind one typed API, for apps and agents that must not trust the files they open. Pure Rust: no Word, no Excel, no LibreOffice, no network.

**Use it** to read a document into typed content with caps, to create a DOCX or XLSX from typed blocks and cells, or to edit an existing file with typed ops and get a receipt that says what the edit could not keep.

**Do not use it** to render pages, to recalculate formulas, to open password-protected or macro-enabled files, or for PDF page graphs and font work (that is `rightkit-pdf`, which this crate wraps for PDF).

Capability: `document-io`; `rustright owns docx|xlsx|document|spreadsheet` resolve to this crate and `rustright owns pdf` resolves to `rightkit-pdf`. Choice of engine and evidence: [BAKEOFF.md](BAKEOFF.md).

## API

```rust
use rightkit_docs::{read, write, Block, Op, ReadOptions, Source, WriteOptions};

let path = std::path::Path::new("report.docx");
write(path, &[Op::CreateDocx { blocks: vec![Block::Heading { level: 1, text: "Q3".into() }] }],
      &WriteOptions::default())?;
let doc = read(Source::Path(path), &ReadOptions::default())?;          // format from content
write(path, &[Op::ReplaceText { find: "Q3".into(), replace: "Q4".into() }],
      &WriteOptions { expect_sha256: Some(doc.sha256), ..Default::default() })?;
```

`read(source, &ReadOptions) -> DocumentRead` and `write(target, &[Op], &WriteOptions) -> WriteReceipt`. Every type is serde (camelCase, `kind`/`type` discriminators) and every error is a `DocsError` with a stable snake_case `code()` and a `{ code, message }` serialisation, like `rightkit-pdf`'s `PdfEditError`.

### Reading

| Format | Content | Renderings |
|---|---|---|
| DOCX | paragraphs with style display names (`heading 1`) and a list flag, tables, headers, footers | plain text, Markdown (headings from style names, `-` for list items, pipe tables) |
| XLSX | sheets with state (`visible`, `hidden`, `veryHidden`), used range, non-empty cells as `text`, `number`, `bool`, `dateTime` (serial + ISO 8601), `duration`, `error`, with the formula and its **cached** result | tab-separated text, Markdown tables per sheet |
| PDF | page count and text per page (`rightkit-pdf`'s `extract`) | plain text, Markdown with `## Page N` |

Selection and caps: `pages` (PDF, 1-based), `sheets` and an A1 `range` (XLSX), `max_chars` and `max_cells`; any cap that cuts the result sets `truncated`. `sha256` is the hash of the exact bytes read.

`omitted` names everything the file holds that `content` does not: comments, tracked changes (inserted text is included, deleted text is not), footnotes and endnotes, images, charts, pivot tables, external links (never followed), drawings, list numbers, scanned PDF pages. Nothing is dropped without being named.

### Writing

A call applies its ops in order to one in-memory document and publishes once, through `rightkit_fs::AtomicWriter`.

| Op | Format | What it does |
|---|---|---|
| `CreateDocx { blocks }` | DOCX | new file from `Heading`, `Paragraph`, `Bullet`, `Numbered` (each run restarts at 1), `Table`, `PageBreak`; first op only |
| `CreateXlsx { sheets }` | XLSX | new workbook; cells are `Text`, `Number`, `Bool`, `Formula`, `Blank`; first op only |
| `ReplaceText { find, replace }` | DOCX, PDF | DOCX: body, headers and footers, across run boundaries, special characters escaped; PDF: source text through `rightkit-pdf`'s compose; zero matches is `not_found`, never a silent no-op |
| `AppendBlocks { blocks }` | DOCX | before the body's final section properties |
| `SetCells { sheet, cells }` | XLSX | value and formula kept explicit; an overwritten cell keeps its style |
| `AppendRows { sheet, rows }` | XLSX | after the last row that holds a value |
| `AddPdfText { page, x, y, text, size_pt }` | PDF | text in the bundled Source Serif 4 |
| `SelectPdfPages { pages }` | PDF | keep these pages, in this order, repeats allowed |

PDF create-from-nothing is not in v1.

`WriteOptions`: `expect_sha256` (an edit refuses with `source_changed` unless the target's current hash matches), `overwrite` (a create over an existing file), `limits`.

**Publishing.** The result is re-detected as the same format before anything is written. The write goes to a temp sibling, is flushed, and a last source-hash recheck runs immediately before the rename; a file changed meanwhile is never overwritten (`source_changed`), and a create never clobbers a file that appears first (`target_exists`). Permission bits of the replaced file are kept; a symlink target is replaced where it points and the link survives. A failure before the rename leaves the original and no temp file. The recheck narrows, it does not exclude, another program replacing the file between the recheck and the rename.

### Honest losses

An OOXML edit does not rebuild the document from a model. It patches exact byte ranges of the parts it touches and copies every other part raw (same bytes, same compression), so comments, tracked changes, footnotes, charts, notes, data validation, merged ranges, defined names, custom XML, extension parts and anything else this crate does not model survive unchanged. `WriteReceipt.touched_parts` lists the parts rewritten; everything else is byte-identical. What an edit does change is reported:

| Where | Change | Reported as |
|---|---|---|
| XLSX | workbook flagged to recalculate on open; existing formulas keep their stored (now possibly stale) results until a spreadsheet application opens the file | warning |
| XLSX | derived `calcChain.xml` dropped (the application rebuilds it) | loss `derived_part_dropped` |
| XLSX | rewritten rows lose the optional `spans` hint; sheet `dimension` updated | none (cosmetic) |
| XLSX | appended rows are not added to a table object's range | warning |
| XLSX | a formula cell written by an op has no stored result until recalculated | read returns `empty` value + `formula` |
| DOCX | replacement inside a document with tracked changes is itself not tracked | warning |
| DOCX | occurrences in footnotes, endnotes and comments are not edited | warning with the count |
| DOCX | appended heading style missing from the document, or no numbering definitions | warning; direct bold formatting or a text marker is used |
| any OOXML | a digital signature is invalidated by any edit | loss `signature_invalidated` |
| PDF | text the font cannot re-encode is not replaced | loss `not_replaced` per page |
| PDF | page selection drops unselected pages and document-level catalog entries | loss `pages_dropped`, warning |

A formula cell that is the master of a shared formula, or an array formula, is refused (`invalid_op`) rather than overwritten.

## Safety

All of it is inside this crate, per call, before a parser sees a byte. Defaults are `Limits::default()`; raise them per call, never process-wide.

| Guard | Default | Error |
|---|---|---|
| compressed input size | 64 MiB | `too_large` |
| entries in a package | 10 000 | `archive_limit` |
| decompressed size, per part / total | 64 MiB / 256 MiB | `archive_limit` |
| compression ratio (parts over 1 MiB; package over 4 MiB) | 200:1 | `archive_limit` |
| XML nesting depth | 256 | `malformed` |
| PDF pages / stream decode while loading | 5 000 / 64 MiB per stream | `archive_limit` / `malformed` |
| unsafe entry names (`..`, absolute, drive letters), case-insensitive duplicates | refused | `malformed` |
| DOCTYPE, entity declarations, UTF-16 XML (so no XXE and no entity expansion) | refused in **every** XML part | `forbidden_xml` |
| `.docm` `.xlsm` `.dotm` `.xltm`, any VBA project, macro content types | refused for read **and** edit; never opened, run, or stripped | `macro_enabled` |
| password-protected OOXML (OLE `EncryptedPackage`), zip-level encryption, encrypted PDF | refused | `encrypted` |
| embedded OLE objects (`embeddings/` parts, `oleObject` parts) and ActiveX controls (`activeX/` parts) | refused before any part is parsed | `active_content` |
| legacy `.doc` `.xls` `.ppt`, PPTX, XLSB, anything else | refused, never guessed | `unsupported_format` |
| external relationships (linked images, templates, workbook links, hyperlinks) | never followed; preserved as written; counted in `omitted` | none |
| formulas that reach outside the workbook: external workbook references, DDE (`\|`), `WEBSERVICE`, `FILTERXML`, `CALL`, `REGISTER`, `EXEC`, file or network locations | refused on create and edit | `unsafe_formula` |
| a cell value starting `=` `+` `-` `@` | `Text` is always a string cell; only `Formula` is a formula | none |
| parser panic on hostile input (XLSX, PDF) | caught and reported | `malformed` |

Control characters XML cannot carry are dropped from written text.

## Limits and gaps

- PDF decompression is bounded while the document loads (`rightkit-pdf`'s loader bound, per stream). Page content streams are decoded later by `pdf-extract` (its own `lopdf` 0.42), which this crate cannot bound further than the 64 MiB input cap.
- XLSX read goes through `calamine`, which reads the whole sheet into memory; the per-part cap bounds that.
- The recheck-before-rename narrows but does not exclude a concurrent writer from another program.
- XLSX dates are numbers plus ISO 8601; no date-typed writes in v1. 1900 serials below 61 have no ISO form.
- Tracked-change, comment and chart authoring are out of scope; they are preserved, not created.
- Hidden-state and 1904-date-system information is reported on read; edits keep it.

## Platform support

Pure Rust, macOS and Windows. The only platform-specific behaviour is `rightkit-fs`'s atomic write (and `rightkit-pdf`'s commit code, which this crate does not use). Proofs: see CHANGELOG.

## Runtime budget

Class `continuous = false`: bounded per call, no thread, timer, listener or watcher. The one process-wide object is `rightkit-pdf`'s font catalog, built once on the first PDF text replace or add.
