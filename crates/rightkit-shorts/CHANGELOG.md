# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Breaking: requires `rightkit-autoedit` 0.2.0; with feature `engine-commands`, `rightkit-engine-host` 0.2.0 (the `Env` the commands take) and `rightkit-video-project` 0.1.3, whose `engine` module holds the shared project-method helpers (were `rightkit_engine_host::project`).

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- New, additive (feature `engine-commands`, off by default): `cmd::moments`, the `moments.ocr_index` and `moments.search` engine handlers (on-screen-text moment search over `rightkit-ocr` and `rightkit-search`), moved from CutRight's `cutright-engine`. The OCR model directory is the `model_dir` argument only (no environment variable, no test-mode feature). Uses `rightkit-fs` for the atomic index write and source hash.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-shorts` (CutRight).
