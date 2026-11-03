# Changelog

## 0.2.0 (unpublished)

- rightkit#1481: real-data benchmarking, MailRight's perf_proof/benchmarks/load_driver measurement logic made app-generic (source d690ecd). `load` module: `run_concurrent` (one thread per workload, fixed interval, JSONL samples) and `summarize_file`. `owner` module (feature `sqlite`): `open_owner_read_only`, `run_probes`, `check_plans`, `table_counts` against the owner's real database, opened read-only with `query_only`. Writes (`open_owner_writer`, `rekey_round_trip` with feature `sqlcipher`/`sqlcipher-system`) require a `WriteGate`, obtainable only from an armed `rightkit-qa` snapshot (feature `qa-snapshot`, `require_armed`). No synthetic corpus, no seeder, no environment switch (Adrian, 2026-11-03: "no dummy data ... real data. snapshot and restore.").

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from ScrapeRight (benchmarks/article-quality) and MailRight (benchmarks.rs, perf_proof harness helpers).
