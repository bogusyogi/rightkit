//! Repeat-and-percentile timing for latency benchmarks.

use serde::Serialize;
use std::time::Instant;

/// Latency of one measured operation over `n` runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BenchMetric {
    /// Operation name.
    pub name: String,
    /// Median, in milliseconds.
    pub p50_ms: u128,
    /// 95th percentile, in milliseconds.
    pub p95_ms: u128,
    /// Slowest run, in milliseconds.
    pub max_ms: u128,
    /// Rows (or items) the last run returned.
    pub rows: usize,
    /// Number of timed runs.
    pub n: usize,
}

/// The `p` quantile (0.0 to 1.0, nearest rank) of `samples`; sorts the slice. 0 when empty.
pub fn percentile_ms(samples: &mut [u128], p: f64) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let idx = ((samples.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize;
    samples[idx]
}

/// Run `f` once as a warmup, then `n` timed times, and report its latency.
/// `f` returns the number of rows it produced. The first error stops the run.
pub fn measure_many<F>(name: &str, n: usize, mut f: F) -> Result<BenchMetric, String>
where
    F: FnMut() -> Result<usize, String>,
{
    // One warmup pass so the first cold-cache sample doesn't dominate p95/max.
    f().map_err(|e| format!("{name} warmup failed: {e}"))?;
    let mut samples: Vec<u128> = Vec::with_capacity(n);
    let mut rows = 0;
    for _ in 0..n {
        let started = Instant::now();
        rows = f().map_err(|e| format!("{name} failed: {e}"))?;
        samples.push(started.elapsed().as_millis());
    }
    Ok(BenchMetric {
        name: name.to_string(),
        p50_ms: percentile_ms(&mut samples.clone(), 0.50),
        p95_ms: percentile_ms(&mut samples.clone(), 0.95),
        max_ms: samples.iter().copied().max().unwrap_or(0),
        rows,
        n,
    })
}
