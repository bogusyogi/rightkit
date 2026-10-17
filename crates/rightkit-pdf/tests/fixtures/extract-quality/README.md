# Extraction quality harness (rightkit-pdf `extract`)

Test-only, opt-in, no network. `tests/extract_quality.rs` compares `rightkit_pdf::extract::extract_text_by_pages`
with poppler `pdftotext` on a pinned corpus and reports two scores per file:

- **word F1**: multiset overlap of lower-cased alphanumeric words (blind to order).
- **reading order**: longest common subsequence of the words both sides have, divided by the number of
  words both sides have (1.0 = the shared words come in the same order). A column read straight across
  into its neighbour keeps F1 at 1.0 and lowers this score.

## Run

Through the RightKit wrapper, package `rightkit-pdf`, feature `extract`, test target `extract_quality`, with
`--ignored --nocapture`. Set `RIGHTKIT_PDFTOTEXT` to the `pdftotext` binary, or leave it unset to use PATH.

A missing `pdftotext` is a **failure**, not a pass. Set `RIGHTKIT_PDF_QUALITY_ALLOW_SKIP=1` to turn it into a loudly
printed skip. The scoring functions have ordinary tests that need no oracle and run with the `extract` feature.

## Corpus pin

`manifest.json` lists files with their sha256; the harness refuses a file that is missing or differs. The default
manifest pins the four fixtures in `tests/fixtures`. For a larger corpus (the pdf.js test PDFs at a fixed commit)
fetch it yourself, then write its manifest offline with the ignored test `pin_corpus_manifest`, giving it
`RIGHTKIT_PDF_QUALITY_PIN_DIR=<dir>` and `RIGHTKIT_PDF_QUALITY_SOURCE="mozilla/pdf.js@<commit> test/pdfs"`, and run
the harness with `RIGHTKIT_PDF_QUALITY_CORPUS=<dir>`.

## Baseline

`baseline.json` holds per-file scores. It is **not yet recorded** (`"recorded": false`): recording needs a run with
poppler, which has not been done. Record with `RIGHTKIT_PDF_QUALITY_RECORD=<new-baseline.json>`, review, and copy it
over `baseline.json`. Later runs fail a file whose F1 or order score falls more than `tolerance` under its baseline.

## Decision rule for column-aware ordering

The run ends with a verdict: when the mean reading-order score is below 0.95, or any file is below 0.90, it prints
`COLUMN-AWARE ORDERING RECOMMENDED`. pdfcraft's reference is `crates/render/src/text.rs` (`arrange` at 1311, the
two-column test at 2050, tab-separated `column_text` at 272), MIT OR Apache-2.0.
