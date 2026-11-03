# rightkit-bench

Benchmark harness building blocks: warmup-and-percentile timing, SQLite EXPLAIN QUERY PLAN probes, and article-quality corpus scoring with a reference extractor.

Moved into RightKit from ScrapeRight (benchmarks/article-quality) and MailRight (benchmarks.rs, perf_proof harness helpers) on 2026-11-01.

## Real data only (0.2.0)

`rightkit_bench::owner` runs probes and `EXPLAIN QUERY PLAN` checks on the owner's real app database, opened read-only. `rightkit_bench::load` drives concurrent workloads and records JSONL latencies. Anything that writes (a rekey round trip, a sync workload) needs a `WriteGate`, which exists only while a `rightkit-qa` snapshot is armed (feature `qa-snapshot`); `rightkit-qa` restores the data afterwards. The crate generates no corpus.
