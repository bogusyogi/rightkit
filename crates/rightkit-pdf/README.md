# rightkit-pdf

Product-neutral PDF engine for Right Suite apps. It owns PDF inspection, composition (replace source text, add text, OCR correction), font subsetting (Source Serif 4 is bundled), staged atomic save, and, behind features, page merge and selection and whole-document or per-page text extraction. It is pure Rust: no Tauri, no PDFium.

**Use it** to read, edit, merge, or extract text from PDF files in a Right Suite app, when the output must be written atomically.

**Do not use it** as a general PDF renderer (the one exception is the `raster` feature, which draws the page images of scanned PDFs for OCR and nothing else), to run PDFium-based rendering, or to handle PDFs that need a full layout engine. Extraction (`extract`) uses `pdf-extract`, and its quality depends on the source file.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `fonts` | on | `fontdb`, `ttf-parser`, `subsetter`, `wuff` | Font catalog, parsing and subsetting (`src/font.rs`). |
| `edit` | on | `getrandom`, `rightkit-fs` (its `fs` feature only), implies `fonts` | Compose, staged save and atomic commit (`src/compose.rs`, `src/save.rs`, `src/commit.rs`). |
| `merge` | on | none (built on `lopdf`, which is always a dependency) | Page merge and selection (`src/merge.rs`), page delete, reorder, rotate and insert with outline and page labels kept (`src/organize.rs`). |
| `extract` | off | `pdf-extract` | Whole-document and per-page text extraction (`src/extract.rs`). |
| `raster` | off | `image` (`png`, `jpeg` codecs only) | Render scanned pages to gray/RGBA/PNG for OCR (`src/raster.rs`). Detection (`src/scan.rs`) is always compiled. |
| `redact` | off | none | Remove text, images, vector paths and annotations under marked areas, fail closed, whole-file rewrite (`src/redact.rs`). |
| `forms` | off | none | AcroForm: list fields, set values, regenerate appearances, `/NeedAppearances` handling, typed refusals (`src/forms.rs`). |
| `optimize` | off | none | Size reduction: unused objects, stream compression, image downsampling by measured effective resolution, progress and cancel (`src/optimize.rs`). |

Inspection (`src/inspect.rs`, `src/model.rs`) is always compiled. `--no-default-features` gives inspection only.

## Scanned pages and rasterization

`scan::classify_page(bytes, page)` returns a `PageProfile`; `is_scanned()` is true when the page shows no text (an invisible OCR layer counts as text) and an image covers at least half of it. `scan::scanned_pages(bytes)` lists them. With feature `raster` (off by default; `image` with the `png` and `jpeg` codecs), `raster::rasterize_page(bytes, page, &RasterOptions { dpi, max_pixels, format })` returns a `RasterPage` (`Gray8` or `Rgba8`, top row first) and `to_png()` encodes it. The page is refused as `RasterError::TooLarge` before any allocation when it would exceed `max_pixels`. It draws image XObjects only, through the CTM, crop box and `/Rotate`; `RasterPage::limitations` names visible text, vector graphics or inline images it did not draw. Not decoded: CCITT, JBIG2 and JPEG 2000 images, CMYK JPEG, `Separation`/`DeviceN`/`Lab` colour, `/Mask`; these fail as `RasterError::UnsupportedImage` (or are ignored, for `/Mask`).

## Platform support

Pure Rust. The only platform-specific code is in `src/commit.rs`: the file identity a commit compares (`dev`/`ino` on Unix, `FILE_ID_INFO` from an opened handle on Windows through `windows-sys`) and the comparison of a replacement with the file it replaces (see "Saving"). The file replacement itself is `rightkit-fs`'s `AtomicWriter`; this crate makes no platform call to write a file. It is expected to build wherever its dependencies build. The bundled font is `assets/SourceSerif4-Variable.woff2`. The crate ships no licence file for it, so the SIL OFL 1.1 licence stated for the font is not verified in this repository.

Published version: see `INDEX.md` at the repository root. Changes: `CHANGELOG.md`.

## Example

Illustrative. For compiled round trips on real files, see `tests/e2e.rs` and `tests/corpus_qa.rs`.

```rust,ignore
// Run with the default feature set. The public functions are in src/inspect.rs,
// src/compose.rs, src/merge.rs, src/extract.rs, and src/save.rs.
```

## Editability and replacement contract

`PdfTextIsland::editable` answers one question: can a replacement be written through this island's own font (same appearance)? The page graph and `compose_candidate` read the same per-font table (`inspect::ReverseTable`, crate-private), so they cannot disagree. When it is `false`, `rejection` is `unsupported_encoding` and the additive `editBlocker` names the reason:

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

Word spacing follows the **encoded byte**, not the character: `Tw` applies to the single-byte character code 32 and to nothing else (ISO 32000-1 9.3.3). A font whose `/Differences` map code 32 to a letter still pays `Tw` for that letter; a space reached through any other code, and every multi-byte (`Identity-H`) code, pays none. Decoding, the replacement measure and the read-back all use that rule, and the pen jump `Tw` causes is not read back as a word gap.

## Graphics state

`q`/`Q` save and restore the text state parameters with the graphics state, as the specification defines: font and size (`Tf`), `Tc`, `Tw`, `Tz`, `TL`, `Ts`, `Tr`. The text matrix and text line matrix are not part of it and are left alone. `q` nesting is bounded at 65 536; a page that nests deeper is `malformed_content`.

## Resources

`AddText`, `CorrectOcrLine` and a catalog-font rewrite add a font to the page. When the page has no `/Resources` of its own the nearest ancestor `/Pages` node's `/Resources` is inherited, so the new page-local `/Resources` starts as a copy of that dictionary (never a merge of levels) and the page keeps every font, XObject and graphics state it resolved. `compose_candidate` compares what each page can resolve before and after and returns `verification_failed` if anything was hidden; `save::stage_prepared_save_against_base` makes the same comparison against the bytes a candidate was composed from. A `/Parent` cycle, or a chain deeper than 64, makes the add refuse with `malformed_content` rather than write over resources it cannot see.

Text appended by `AddText` and `CorrectOcrLine` is drawn in an isolated `q`/`Q` block that sets `Tr`, `Tc`, `Tw`, `Tz` and `Ts` explicitly, so it cannot inherit invisible (`3 Tr`), spaced, scaled or raised text state from the page's last text object.

## Saving

`save::stage_prepared_save` checks a candidate's structure and expected text but sees only digests, not the bytes it was composed from. Use `save::stage_prepared_save_against_base` (the same arguments plus those bytes) to also refuse a candidate in which a page lost a resource.

`commit::commit_pdf_save` decides "is the target the source?" by filesystem identity, not by comparing path strings: `dir/./memo.pdf`, a hard link, a symlink and (on a case-insensitive volume) `MEMO.PDF` are the source, and the source SHA recheck runs for all of them. On Unix identity is the device and file id; on Windows the volume serial and 128-bit file id from an opened handle; a destination that does not exist yet is its canonical directory plus its name. The check, the replacement and the invalidation of other tokens staged against the written file run under a per-destination lock, so two candidates staged against the same source cannot both commit: the second fails with `write_conflict`. A symlink destination is resolved once and the file it points at is replaced. The lock is in-process; another program replacing the file between the recheck and the rename is narrowed by the recheck, not excluded. `save::resolve_commit` keeps string-equality semantics; `save::resolve_commit_with` takes the comparison. The exported `commit::write_atomic` is the bare writer: it takes no lock and invalidates no token.

### What a save does to the file, per platform

The replacement is `rightkit_fs::AtomicWriter`: a temp sibling in the destination's directory, flushed, renamed over the destination, directory synced. This crate adds no write path of its own, and in particular never `ReplaceFileW`. A replacement is a **new file**, so it carries what a new file in that directory carries. Before any byte is written the new (empty) temp file is compared with the destination, and the save is **refused** (`CommitError::Refused(Unpreserved)`, original untouched, token spent) when a difference visible through `std` and `rightkit-fs` would weaken or change the file's protection.

| | Linux and other Unix | macOS | Windows |
|---|---|---|---|
| Content | the old file or the complete new one; temp flushed before the rename, directory synced after | the same, with the Apple flush ladder | the old file or the complete new one (`std::fs::rename`, POSIX semantics); the temp is flushed, but the swap itself can be lost in a power failure and the old version reappear (`rightkit-fs`: `FileFlushedOnly`) |
| A failed or interrupted replace | the original is untouched and the temp is removed; there is no state in which neither version exists | the same | the same |
| After the rename, a later step fails (directory sync) | `CommitError::Replaced`: the new bytes are in place, tokens are invalidated, durability is not established | the same | not reachable (no directory sync exists) |
| Permission bits (`0o7777`), owner, group | kept, else refused (`Unpreserved::Mode`, `Owner`, `Group`) | the same | not applicable |
| Extended ACL | not examined: POSIX ACLs and SELinux labels are lost | refused when the destination is owner-only (`mode & 0o077 == 0`) and it or the new file is not provably free of an ACL (`rightkit_fs::private::is_owner_only`; `Unpreserved::Acl`). Not examined for any wider mode: the ACL is lost and the new file takes the parent's inheritable entries | not examined: the destination's DACL (protected, explicit entries, owner) is **not kept**; the new file takes the parent directory's inherited ACL |
| Read-only or encrypted | not applicable | not applicable | refused (`Unpreserved::ReadOnly`, `Encrypted`) |
| Everything else | extended attributes, file flags, creation time are lost; a hard-linked sibling keeps the old bytes | the same, extended attributes included | alternate data streams, the hidden/system/compressed attributes and creation time are lost |

"Not examined" means exactly that: such a save succeeds and the property is gone. Closing the Windows DACL and the macOS wide-mode ACL cases needs a read of the destination's security descriptor or ACL, which neither `std` nor a public `rightkit-fs` function provides.

## Page content

`/Contents` may be a stream, an array, or an indirect reference to an array of streams. Text state flows from one stream into the next. Composition re-parses a stream strictly and refuses (`malformed_content`) one the parser cannot read to the end, rather than rewriting it with the tail dropped. A stream that is only read (the `q`/`Q` count behind Add Text and a catalog-font rewrite) may end in the operands of an operator that opens the next stream, which ISO 32000-1 7.8.2 allows; a string, array or dictionary left open there, or a byte no tokenizer reads, is still refused.

## Limits

- Glyph presence in a subset font is inferred from a positive explicit width, not read from the font program. A producer that writes real widths for glyphs it left out of the subset would let an unavailable code through.
- `ToUnicode` cannot say which of two CIDs is the glyph variant the author sees, so an `Identity-H` character mapped by more than one CID is left unencodable (`missing_glyph`).
- Layout is measured, not reflowed: text after an edited run on the same line keeps its position. A catalog-font replacement removes the old glyphs but keeps their pen movement (each removed glyph becomes the `TJ` number that moves the pen as it did; a `Tj` becomes a `TJ`), so the text after it does not slide left; glyphs shown by `'` or `"`, which carry a line move a `TJ` cannot, are refused. A same-font in-place edit changes the island's own length by the width difference and does not compensate the text that follows it in later operators. What the tests pin for a catalog-font replacement is the geometry of the glyphs after the edited run: in the one-line fixtures they build, the quads are the same bit for bit. It is not a promise for every file. A consumer's corpus probe, which is not in this repository, is recorded as still measuring a neighbouring island moving more than 0.5 pt in a share of changed-font runs (its figures are not reproduced here), with the cause not isolated.
- A catalog-font replacement is drawn from the placement of the run it replaces: the new block's `Tm` is the first glyph's `text matrix x CTM` (all six numbers, so rotation, skew and negative or non-uniform scale survive) and its `Tf` stays the source run's text-space size, because the block runs in the default graphics state and cannot see a `cm` the producer used. It is placed from the island, not from `availableBounds`. Published 0.2.0 (and the 0.2.1 tree, not yet published) emitted `1 0 0 1 <availableBounds.x> <availableBounds.y> Tm` with the raw text-space size, which draws the replacement at the wrong place and size on any page whose producer positions runs with `cm` outside the text object (`0.1 0 0 0.1 0 0 cm` pages) while still reading back as the requested text. **Not reproduced:** the block resets horizontal scaling (`Tz`, so the width of a condensed or expanded source run is lost, and the replacement is measured at 100 percent), text rise (`Ts`), character and word spacing (`Tc`, `Tw`) and the render mode (`Tr`: the replacement is always filled), and it draws the catalog font's own glyphs, not the source font's weight, width or style. `appearance.sizePt` of the replacement is in the island's text-space units, the value the island itself reports, not page points: on a page under a `0.1` scale an island reports 100 and is drawn 10 points high, and a caller that sends 10 gets a run one hundredth of the size. The fit checks extent only: the replacement's length along the island's own axis, in device space, against `availableBounds.width` and `.height`. Its position is not compared with `availableBounds`, so a rectangle that starts left of the island's origin lets the run overflow to the right by that offset.
- An edit that adds a stream (Add Text, `CorrectOcrLine`, a catalog-font replacement) puts one `q` stream in front of the page's `/Contents`, so every island's `contents_index`, and with it its `island_digest`, is one higher afterwards even though its quads and its stream, operator and operand are unchanged: a locator taken before the edit is stale, so inspect the page again before the next edit.
- A batch is applied against the pristine source: the `q`/`Q` wrappers and new streams are installed after the last command, and the `ReplaceSourceText` commands for one page run back to front by source address. Two islands that interleave their operators (one island has runs before and after the other's) can still invalidate each other's locators; the later one fails with `source_digest_mismatch`.
- Saving over an existing file keeps, or refuses to lose, its owner, group and permission bits on Unix and refuses a read-only or encrypted file on Windows. It does not keep a Windows DACL, extended attributes, file flags, POSIX ACLs, or a macOS ACL on a file whose mode is wider than owner-only (see "What a save does to the file, per platform").
- A `ToUnicode` CMap is read for its `bfchar` and `bfrange` forms (scalar and array destinations). Any other form, an unbalanced entry, `usecmap`, or a CMap that exhausts the reader's work budget makes the font non-editable rather than partly decoded.
- Hostile files: CID widths and `ToUnicode` codes are limited to the two-byte code space, `/Widths` is bounded by its own length, `/Parent` chains stop at a cycle or depth 64, and streams decode under 64 MiB while the document loads. A page's content is bounded in aggregate, across every `/Contents` entry as listed (a stream listed a thousand times is decoded and charged a thousand times): 64 MiB decoded (the decode buffer) and 2^23 tokens parsed per page, charged before the parse allocates (each byte starts at most one token, so the byte count bounds the token count; `src/inspect.rs`, `MAX_CONTENT_TOKENS_PER_PAGE`); a page whose decoded content is larger than 8 MiB is `malformed_content`); each page also bounds glyph records (reserved only after the operand's glyph count passes the page budget), the fonts it resolves (512) and the `ToUnicode` bytes (32 MiB) and `ToUnicode`/`/W` assignments (2^21 each) all its fonts may spend together. Work is also bounded across entries, not only within one: a `ToUnicode` CMap and a `/W` array each have a total assignment budget (2^20); a font whose `/W` ran out of budget is displayed but `editable: false` (`missing_metrics`) and rejects a code it has no width for. `TJ` gap provenance moves its numbers (never copies them per operand) and has a per-page budget of 2^20 numbers, past which a gap has no provenance and its island is `positioned_text_run`. This bounds the crate's own loops and allocations; it does not audit `lopdf` or the font libraries it calls.
- An in-place edit is read back before it is returned: the island at the same position must read as exactly the requested text and fit the bounds as the walker now measures it. A catalog-font replacement is read back too, by its own content stream (the join rule does not compare fonts, so the run may share an island with a neighbour): the walker's first glyph of that stream must have a `text matrix x CTM` origin within 0.5 pt of the source run's first glyph's, and one `Tf` size along text x and along text y, in device space, within 2 percent of the requested size, or the batch is refused with `verification_failed`. That comparison excludes text rise on both sides, so a source run with a non-zero `Ts` is redrawn lower or higher by the rise times the matrix scale and still passes; and it uses the size that was requested, so it cannot catch a caller that sent page points where the island's text-space units were meant (see above). The text of a catalog-font run is checked by the save-side recheck (any island of the page containing the expected text), which compares no geometry. Read-back uses the same decoder as inspection, so it proves structure and geometry, not that a code is the glyph the author sees.
- Grouping measures the gap between two glyphs along **page x** (the x extent of their quads) and the baseline offset along **page y** (`origin.1`), not along and across the baseline (`projected_gap`, `baseline_distance`, `has_intervening_painted_content` in `src/inspect.rs`). Text turned a quarter, or sheared steeply, is therefore measured across its length: in a 10 pt run turned 90 degrees, two consecutive glyphs overlap by one em in page x (a gap of -10 against a window of -0.25 em) and sit 6.7 pt apart in page y (against 2 pt), so each glyph becomes an island of its own. Left-to-right upright text is unaffected; text running right to left or downwards on the page (turned 180 or 270 degrees) is measured the wrong way round for the same reason. This is a limit of the grouping, not of the composer: a replacement of such a run still lands on the source glyph's placement. Before the em was measured in device space (below) a page under a scaled `cm` hid the limit, because the window was ten times too wide; now such a page behaves like an unscaled one.
- Runs are grouped into islands by device-space distances measured in device-space ems (`Tf` size times the length of the glyph's baseline axis): two glyphs join when they are within 1.5 em of each other along the baseline, 0.25 em of overlap and a baseline offset of 0.2 em (0.5 pt at least), with the same paint. Before this version the em was the text-space `Tf` size, so on a page under a scaled `cm` the window was too wide by the page's scale and neighbouring runs were merged into one island; locators taken on such a page with an older build do not match.
- `compose_candidate` redraws in a catalog font only when the appearance changes; a non-editable island with unchanged appearance is refused. An island whose source spans live in several content streams cannot be redrawn at all (`substitutionEditable: false`).

## Page merge and selection

`merge::merge_pdfs` and `merge::select_pages` set on each page the attributes it inherits from its `/Pages` ancestors (`Resources`, `MediaBox`, `CropBox`, `Rotate`) before the old page tree is discarded. A value an ancestor holds directly (a `/Resources` dictionary, a box array) becomes **one** indirect object shared by every page below that ancestor, so the output holds one copy however many pages inherit it; a value that is already a reference, or a number, is copied as it is. A page's own value is left alone. Selecting a page from a large document materializes every page in memory but copies nothing per page.

## Redaction

Feature `redact` (off by default). `redact::apply_marks(bytes, &[RedactMark], &RedactOptions)` removes the content under marked rectangles (PDF user space, before `/Rotate`) and returns new bytes plus a `RedactReport`; `redact::verify_redacted` re-checks any file against the same marks. It removes, it does not only cover: glyphs under a mark are cut from `Tj`/`TJ`/`'`/`"` and replaced by a `TJ` number of the same advance (the text after them does not move), so extraction cannot return them; an overlapping image `Do` is removed whole (partly covered images are dropped, not pixel-cleared); overlapping vector paths and annotations are removed; `/ActualText`, `/Alt` and `/E` in inline `BDC` lists are stripped; the file is written whole (no earlier revision, unreferenced objects pruned); with `sanitize` (default) `/Info`, `/Metadata`, `/PieceInfo`, page `/Thumb`, the structure tree and the embedded-file and JavaScript name trees are removed. A black box is painted last (`RedactOptions::fill`).

It fails closed with a typed `RedactError`: text it cannot position, a glyph whose advance cannot be written as a `TJ` number, a content stream that does not parse strictly or that two pages share, an inline image, a form XObject or a shading that overlaps a mark (`RedactOptions::remove_unsupported` removes the operator whole instead), an encrypted file, and any result whose second walk still finds content under a mark. Not covered: the insides of form XObjects (a form is cut whole or refused, never edited), content reached through `/Properties` names, and sub-glyph precision (a glyph goes when its bounding quad overlaps, not its ink). The full list is the module documentation of `src/redact.rs`. Stage the result through `save::stage_prepared_save` with no expected-text checks.

## Writing a new PDF

`write::Document` creates a PDF from nothing (always compiled, no feature). `Document::new("1.4")`, `add_icc_profile(bytes, components)`, `add_page(width_pt, height_pt)` and `place_jpeg(page, jpeg, icc)` (or `add_jpeg_page` for both), then `finish() -> Vec<u8>`; with feature `edit`, `write_to(path)` writes through `commit::write_atomic`. Each image is a full-page DCTDecode XObject drawn by `q w 0 0 h 0 0 cm /Im0 Do Q`, 8 bits per component, DeviceRGB, DeviceGray or `[/ICCBased n 0 R]` with the profile written once. The output is deterministic (no dates, no `/Info`, `/ID` is a hash of the body) and uses a classic cross-reference table, so `%PDF-1.4` is honest. JPEG bytes are copied, not re-encoded. Refused: CMYK, 12-bit, lossless and arithmetic JPEGs, an ICC profile whose component count differs from the JPEG's, ICC under PDF 1.2 or earlier, a second image on a page. Text (feature `fonts`): `add_font(ttf_bytes, face_index)`, `measure_text`, `draw_text(page, font, size, x, y, rgb, text)`. Each used font is subset to the glyphs drawn (`font::subset_for_pdf`, the `subsetter` crate) and embedded as Type0 / CIDFontType2 / `FontFile2` with `Identity-H`, `/CIDToGIDMap /Identity`, `/W` widths and a `ToUnicode` CMap, so CJK is selectable text. TrueType (glyf/loca) only; no shaping, kerning, bidi or wrapping; a missing glyph or a font whose `fsType` forbids embedding is refused. Not compiled or run yet; CI is the check.

## Page organise

With feature `merge`, `organize::{delete_pages, reorder_pages, rotate_pages, insert_pages}` take bytes and return bytes (keep the input as the undo snapshot) and `organize::verify_structure` is the check each result passes: page count, one flat `/Pages` node over `/Page` objects, outline destinations that name a result page, readable page labels. Page objects keep their ids, so outline items and destinations to surviving pages stay valid; an outline item whose page was deleted loses only its destination. Page labels are rebuilt for the new order (`/St` adjusted so each page keeps its label); inserted pages carry their source's labels. Deleted pages and everything only they reached are pruned. See `src/organize.rs` for what is kept and dropped.

## Forms

Feature `forms` (off by default). `forms::list_fields` lists terminal fields (qualified name, kind, value, options, flags, button states); `forms::set_values` and `forms::reset_fields` return new bytes and a `FillOutcome`. Text and combo-box fields get a generated appearance stream (`/MK` colours and border, clipping, `/Q`, auto font size, wrapping); a field it cannot draw (embedded Type 0 font, rotated widget, list box, no `/DA`) keeps its value, is named in `without_appearance`, and `/AcroForm /NeedAppearances` is set. Refusals are typed (`FormError`): read-only, too long, not an option, a character the font cannot show (never replaced), password, file-select and comb fields, XFA and signed documents unless `FormOptions` allows. The written file is read back and compared. Detecting fields in unlabelled printed forms is not built.

## Optimizing

Feature `optimize` (off by default). `optimize::optimize(bytes, &OptimizeOptions::standard() | mobile())` (and `optimize_with_progress`, which can cancel) removes unused objects, Flate-compresses unfiltered streams and downsamples 8-bit Gray/RGB images to 150 (or 96) ppi **measured where pages draw them**, through the CTM and forms up to 12 deep; an image whose resolution is unknown (a mask, listed by a form no page paints, a pattern or Type 3 font) is never resampled, and the report names why. Input is returned unchanged when the result is not smaller. Not done: page-printer strip merging, JPEG re-encoding, colour conversion.

## Extraction quality

`tests/extract_quality.rs` (feature `extract`, ignored) scores `extract` against poppler `pdftotext` on a sha256-pinned corpus: word F1 and a reading-order score, with a column-aware-ordering verdict. No network; a missing `pdftotext` fails. The baseline is not recorded yet. See `tests/fixtures/extract-quality/README.md`.

## Measuring coverage

`tests/corpus_coverage.rs` (ignored) rotates the first 5+ letter word of editable islands on page 1 of every PDF in `RK_PDF_CORPUS_DIR` and prints islands / editable / tried / ok / refusals per file, plus the `editBlocker` histogram of refused islands:

`RK_PDF_CORPUS_DIR=<dir> rightkit cargo test -p rightkit-pdf --test corpus_coverage -- --ignored --nocapture`

## Verification

Run through the RightKit wrapper: `rightkit cargo test -p rightkit-pdf`, and `rightkit cargo test -p rightkit-pdf --features extract` for extraction.
