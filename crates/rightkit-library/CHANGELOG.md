# Changelog

## 0.3.0

Breaking (one new field on a struct built by literal).

- `EnrichmentInput` gains `domain: SourceDomain`. `SourceDomain::FromUrl` is the 0.2.0 behaviour (the library's own http(s) rule). `SourceDomain::Given(Option<&str>)` records the host's own value as given, for a host whose shipped rule differs: ScrapeRight passes `rightkit_media_fetch::detect::host(url)` filtered to hosts with a dot, which also reads a scheme-less `example.com/page`, so its stored domain tags do not change. Migration: add `domain: SourceDomain::FromUrl` to keep 0.2.0 output.

## 0.2.0

Breaking. The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-content-ingest-mail.md, `rightkit-library` section) found the crate holding ScrapeRight's artifact index with a fixed social `Kind` enum, ScrapeRight's tier rules in its documentation, a dependency on `rightkit-media-fetch` for two URL helpers, a duplicate of the atomic-replace helper, and two dead files from GenRight. Taken on the recommended option, pending the owner's confirmation. Design note: docs/audits/2026-11-11-shape-matrix/reports/library-and-swift-mapping-redraw.md.

- Host-supplied kind: `history_types::Kind` is deleted (it duplicated `rightkit_media_fetch::protocol::Kind`). `HistoryRow::kind` and `TranscriptDetail::kind` are the stored tag as a `String`; the JSON is unchanged for every tag the enum knew. A tag the enum did not know now reads back as written instead of `"unknown"`. Migration: a host that matched on `Kind` keeps its own enum (ScrapeRight: `rightkit_media_fetch::protocol::Kind::from_tag(&row.kind)`).
- Enrichment: `EnrichmentInput` gains `shortcode: Option<&str>`, which the host fills (ScrapeRight: `rightkit_media_fetch::detect::instagram_shortcode` for its Instagram kinds); the library no longer decides which kinds carry a short code. `SourceMetadata::domain` comes from the crate's own link normaliser: an http(s) URL's host without `www.`, credentials or port. A source with no scheme (a local file name such as `clip.mp4`) no longer reads as a domain.
- Dropped the `rightkit-media-fetch` dependency (yt-dlp, ffmpeg and scraper came with it for two URL helpers).
- `history::publish_replacing` is removed; the attach publish calls `rightkit_fs::publish_replacing` (needs `rightkit-fs` 0.4.13), the one copy.
- Tier rules out of the library: the documentation of `srt_text`, `truncated_at_s` and the detail reader no longer speaks of Free and Pro rows; which records get timed files, and whether a transcript is capped, is the host's rule. No behaviour change.
- Removed `src/artifacts.rs` and `src/path_policy.rs`: GenRight's gallery index, never declared in `lib.rs` and never compiled (it needed `serde_json`), but shipped in the 0.1.x archives. It now lives, compiled, in `rightkit-media-gen` 0.1.2 as `gallery`.

## Unreleased (folded into 0.2.0)

- Doc comments name `rightkit-media-gen-approvals` (where the eyes gate moved from `rightkit-approvals` 0.1.0). No code change.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1

- Added `history` and `history_types`: the history reader, delete, merge, import and attach-to-media logic over `Store`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from ScrapeRight (scraperight-store, engine enrichment modules).
