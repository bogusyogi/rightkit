# Changelog

## 0.1.1 (unreleased)

- New, additive (feature `engine-commands`, off by default): `cmd::moments`, the `moments.ocr_index` and `moments.search` engine handlers (on-screen-text moment search over `rightkit-ocr` and `rightkit-search`), moved from CutRight's `cutright-engine`. The OCR model directory is the `model_dir` argument only (no environment variable, no test-mode feature). Uses `rightkit-fs` for the atomic index write and source hash.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-shorts` (CutRight).
