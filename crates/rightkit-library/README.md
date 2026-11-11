# rightkit-library

Local artifact library: SQLite-backed history/search index (files stay the source of truth) and a deterministic non-LLM enrichment pass (keywords, links, tags, segmentation).

Moved into RightKit from ScrapeRight (scraperight-store, engine enrichment modules) on 2026-11-01.

## History reader

`history` holds the library reader and cleanup logic over `Store`: cursored and text-searched history pages (`get_history_page`), transcript variants and details, delete with artifact cleanup, `merge_history` between output folders, `import_transcript`, and the attach-to-media publish with crash recovery. `history_types` holds the serde shapes (`HistoryRow`, `HistoryPage`, `TranscriptDetail`, ...) and `HistorySettings`, which the host fills from its own settings.

## Shape (0.2.0)

An artifact index for extraction apps: ScrapeRight today; HeardRight's file transcription and CutRight's imports are the plausible second users. The host supplies the kind tag and any short code, and owns its product rules (which records get timed files, transcript caps). Not the home of dictation history (`rightkit-transcript-library`), GenRight's job and asset store or gallery (`rightkit-media-gen`), or PostRight's reusable content records (`rightkit-content-plan`).
