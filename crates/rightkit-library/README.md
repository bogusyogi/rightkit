# rightkit-library

Local artifact library: SQLite-backed history/search index (files stay the source of truth) and a deterministic non-LLM enrichment pass (keywords, links, tags, segmentation).

Moved into RightKit from ScrapeRight (scraperight-store, engine enrichment modules) on 2026-11-01.

## History reader (0.1.1)

`history` holds the library reader and cleanup logic over `Store`: cursored and text-searched history pages (`get_history_page`), transcript variants and details, delete with artifact cleanup, `merge_history` between output folders, `import_transcript`, and the attach-to-media publish with crash recovery. `history_types` holds the serde shapes (`HistoryRow`, `HistoryPage`, `TranscriptDetail`, ...) and `HistorySettings`, which the host fills from its own settings.
