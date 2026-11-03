# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1

- Added `protocol`, `web`, `source_auth`, `x_post`, `tiktok`, `playlist_batch`, `profile_batch`, `carousel` and `extraction` (platform extraction logic from ScrapeRight) behind `PublicWeb`, `BrowserHost` and closures for the host queue. Additive. New dependencies: `rightkit-web-extract`, `rightkit-fs`, `scraper`, `serde_json`, `tracing`.

## 0.1.0 (unreleased)

- First RightKit release, moved from ScrapeRight (scraperight-audio, scraperight-detect, scraperight-protocol ytdlp).
