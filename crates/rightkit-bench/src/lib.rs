//! `rightkit-bench`: small, dependency-light building blocks for benchmark harnesses.
//!
//! - [`timing`]: warmup-then-repeat latency measurement with p50/p95/max.
//! - `sqlite` (feature `sqlite`): `EXPLAIN QUERY PLAN` probes and throwaway database paths for perf proofs.
//! - [`article`]: article-quality corpus scoring (main content kept, boilerplate dropped)
//!   with a reference heuristic extractor.
//!
//! - [`load`]: concurrent load driver writing JSONL latency samples.
//! - `owner` (feature `sqlite`): probes, plan checks and table counts on the owner's **real** app
//!   database, opened read-only; writes need a `WriteGate` from an armed `rightkit-qa` snapshot
//!   (feature `qa-snapshot`). No synthetic corpus exists in this crate (rightkit#1481).
//!
//! Queries, workloads and pass thresholds belong to the calling app.

pub mod article;
pub mod load;
#[cfg(feature = "sqlite")]
pub mod owner;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod timing;

pub use timing::{measure_many, percentile_ms, BenchMetric};
