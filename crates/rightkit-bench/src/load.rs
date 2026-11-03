//! Concurrent load driver: run one or more workloads on a fixed interval against the owner's
//! real data and record every call's latency as JSON lines.
//!
//! The driver never opens a database or chooses a query: the caller passes closures that already
//! hold a read-only connection (`owner::open_owner_read_only`) or, for a write workload, a gated
//! writer (`owner::WriteGate`). It generates no data.

use crate::timing::percentile_ms;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How a load run is paced and bounded.
#[derive(Debug, Clone)]
pub struct LoadConfig {
    /// Delay between the end of one call and the start of the next, per workload.
    pub interval: Duration,
    /// JSONL file the samples are appended to (created if missing).
    pub out: PathBuf,
    /// Stop after this long. `None` runs until the stop flag is set.
    pub duration: Option<Duration>,
    /// Stop each workload after this many calls. `None` is unbounded.
    pub max_samples: Option<usize>,
}

/// One named workload: a closure returning the number of rows (or items) the call produced.
pub struct Workload<'a> {
    /// Name written to every sample line.
    pub name: String,
    /// The operation; called repeatedly from its own thread.
    pub call: Box<dyn FnMut() -> Result<usize, String> + Send + 'a>,
}

impl<'a> Workload<'a> {
    /// A workload from a closure.
    pub fn new(
        name: impl Into<String>,
        call: impl FnMut() -> Result<usize, String> + Send + 'a,
    ) -> Self {
        Self {
            name: name.into(),
            call: Box::new(call),
        }
    }
}

/// One line of the output file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    /// Wall-clock time the call started, milliseconds since the Unix epoch.
    pub ts_ms: u128,
    /// Workload name.
    pub workload: String,
    /// Call latency in milliseconds.
    pub ms: u128,
    /// Rows the call returned (0 on error).
    pub rows: usize,
    /// Error text, if the call failed.
    pub err: Option<String>,
}

/// Latency summary of one workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoadSummary {
    /// Workload name.
    pub workload: String,
    /// Calls made.
    pub calls: usize,
    /// Calls that failed.
    pub errors: usize,
    /// Median latency of the successful calls, milliseconds.
    pub p50_ms: u128,
    /// 95th percentile latency, milliseconds.
    pub p95_ms: u128,
    /// Slowest successful call, milliseconds.
    pub max_ms: u128,
}

fn summarize(workload: &str, samples: &[Sample]) -> LoadSummary {
    let mut ok: Vec<u128> = samples
        .iter()
        .filter(|s| s.err.is_none())
        .map(|s| s.ms)
        .collect();
    let p50_ms = percentile_ms(&mut ok, 0.50);
    let p95_ms = percentile_ms(&mut ok, 0.95);
    LoadSummary {
        workload: workload.to_string(),
        calls: samples.len(),
        errors: samples.iter().filter(|s| s.err.is_some()).count(),
        p50_ms,
        p95_ms,
        // `percentile_ms` sorted `ok`.
        max_ms: ok.last().copied().unwrap_or(0),
    }
}

/// Run every workload concurrently, each on its own thread, until `cfg.duration` passes, every
/// workload reached `cfg.max_samples`, or `stop` is set. Every call is appended to `cfg.out` as it
/// finishes (so a killed run keeps its samples). Returns one summary per workload, in order.
pub fn run_concurrent(
    cfg: &LoadConfig,
    stop: &AtomicBool,
    workloads: Vec<Workload<'_>>,
) -> Result<Vec<LoadSummary>, String> {
    if workloads.is_empty() {
        return Err("run_concurrent needs at least one workload".to_string());
    }
    if cfg.duration.is_none() && cfg.max_samples.is_none() {
        return Err("a load run needs a duration or a sample bound, or it never ends".to_string());
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.out)
        .map_err(|e| format!("open {}: {e}", cfg.out.display()))?;
    let out = Mutex::new(file);
    let deadline = cfg.duration.map(|d| Instant::now() + d);
    let results: Vec<Result<LoadSummary, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = workloads
            .into_iter()
            .map(|mut w| {
                let out = &out;
                scope.spawn(move || -> Result<LoadSummary, String> {
                    let mut samples: Vec<Sample> = Vec::new();
                    loop {
                        if stop.load(Ordering::Relaxed)
                            || deadline.is_some_and(|d| Instant::now() >= d)
                            || cfg.max_samples.is_some_and(|m| samples.len() >= m)
                        {
                            break;
                        }
                        let ts_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or(0);
                        let started = Instant::now();
                        let res = (w.call)();
                        let ms = started.elapsed().as_millis();
                        let sample = match res {
                            Ok(rows) => Sample {
                                ts_ms,
                                workload: w.name.clone(),
                                ms,
                                rows,
                                err: None,
                            },
                            Err(e) => Sample {
                                ts_ms,
                                workload: w.name.clone(),
                                ms,
                                rows: 0,
                                err: Some(e),
                            },
                        };
                        let line = serde_json::to_string(&sample).map_err(|e| e.to_string())?;
                        {
                            let mut f = out
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            writeln!(f, "{line}")
                                .and_then(|()| f.flush())
                                .map_err(|e| format!("write samples: {e}"))?;
                        }
                        samples.push(sample);
                        std::thread::sleep(cfg.interval);
                    }
                    Ok(summarize(&w.name, &samples))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("workload thread panicked".to_string()))
            })
            .collect()
    });
    results.into_iter().collect()
}

/// Summarise a JSONL sample file written by [`run_concurrent`], one summary per workload name in
/// first-seen order. For a file a separate long-running process produced.
pub fn summarize_file(path: &Path) -> Result<Vec<LoadSummary>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut order: Vec<String> = Vec::new();
    let mut by: Vec<Vec<Sample>> = Vec::new();
    for (i, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let s: Sample =
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
        match order.iter().position(|n| *n == s.workload) {
            Some(idx) => by[idx].push(s),
            None => {
                order.push(s.workload.clone());
                by.push(vec![s]);
            }
        }
    }
    Ok(order
        .iter()
        .zip(by.iter())
        .map(|(n, s)| summarize(n, s))
        .collect())
}
