# Changelog

## 0.1.3 (unreleased)

- Add `content` (reusable content records with occurrences, cooldowns, expiry, variants and experiment summaries over the host channel store) and its `content_protocol` shapes, moved from PostRight. The channel store is opened through `rightkit_content_proof::host`.

## 0.1.2 (unreleased)

- Declare the `artifacts` module (gallery indexing, sidecars, brand-code catalogue) that was present but not compiled; `serde_json` is now a normal dependency and `rightkit-fs` uses its `digest` feature.

## 0.1.1

- Added `history` and `history_types`: the history reader, delete, merge, import and attach-to-media logic over `Store`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from ScrapeRight (scraperight-store, engine enrichment modules).
