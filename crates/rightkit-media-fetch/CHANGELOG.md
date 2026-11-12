# Changelog

## 0.2.0

- Requires `rightkit-http` 0.4.0 (a breaking release), so this release is breaking too.

## 0.1.3

- `extraction::publish_replacing` is now a re-export of `rightkit_fs::publish_replacing` (needs `rightkit-fs` 0.4.13), and the private copy in `carousel` is gone: one implementation, in `rightkit-fs`. Same signature and behaviour.
- `protocol::Kind` is now the only copy of the job-kind enum: `rightkit-library` 0.2.0 deleted its duplicate and stores the host's tag string (`Kind::as_str`).
- Shape review 2026-11-11 (`rightkit-library` row), taken on the recommended option, pending the owner's confirmation.

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1

- Added `protocol`, `web`, `source_auth`, `x_post`, `tiktok`, `playlist_batch`, `profile_batch`, `carousel` and `extraction` (platform extraction logic from ScrapeRight) behind `PublicWeb`, `BrowserHost` and closures for the host queue. Additive. New dependencies: `rightkit-web-extract`, `rightkit-fs`, `scraper`, `serde_json`, `tracing`.

## 0.1.0 (unreleased)

- First RightKit release, moved from ScrapeRight (scraperight-audio, scraperight-detect, scraperight-protocol ytdlp).
