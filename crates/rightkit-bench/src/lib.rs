//! `rightkit-bench`: small, dependency-light building blocks for benchmark harnesses.
//!
//! - [`timing`]: warmup-then-repeat latency measurement with p50/p95/max.
//! - `sqlite` (feature `sqlite`): `EXPLAIN QUERY PLAN` probes and throwaway database paths for perf proofs.
//! - [`article`]: article-quality corpus scoring (main content kept, boilerplate dropped)
//!   with a reference heuristic extractor.
//!
//! Corpora, seeded databases and pass thresholds belong to the calling app.

pub mod article;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod timing;

pub use timing::{measure_many, percentile_ms, BenchMetric};
