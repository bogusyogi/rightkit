//! Daemon resource telemetry: turn latency, provider round-trips, wakeups.
//!
//! Companion to `prompt_overhead.rs` for the runtime-cost side of the daemon:
//! turn wall-clock latency percentiles, provider round-trips per task (kept
//! per task identity so benchmark lanes can correlate them), and a wakeup
//! counter with an observation-window rate. Gauges that are only meaningful
//! live (RSS, open streams, pool sizes) are read at request time by the
//! `/runtime/resource-metrics` endpoint instead of being recorded here.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

/// Cap on distinct task rows kept for round-trip accounting.
const MAX_DISTINCT_ROWS: usize = 512;

/// Bounded window of recent turn wall-clock samples kept for percentile math.
/// Oldest samples are evicted first; percentiles describe this window, which
/// is labelled alongside every surfaced value.
const MAX_LATENCY_SAMPLES: usize = 1024;

/// Bounded window of recent SQLite operation-latency samples (microseconds),
/// shared across operation kinds. OPS-003 requires SQLite latency as a
/// measured counter; percentiles describe this window only.
const MAX_SQLITE_LATENCY_SAMPLES: usize = 1024;

/// Bounded window of recent event-loop scheduling-delay samples
/// (microseconds). A periodic timer tick whose deadline has already passed
/// when the task is scheduled measures scheduler/event-loop stall without a
/// dedicated watchdog thread.
const MAX_EVENT_LOOP_DELAY_SAMPLES: usize = 1024;

/// The SQLite operation class a latency sample belongs to. Bounded and
/// closed — the same set the `/runtime/resource-metrics` endpoint reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SqliteOpKind {
    /// `Storage::append_event` — durable event admission.
    AppendEvent,
    /// `Storage::all_events` — full per-session replay read.
    ReadEvents,
}

impl SqliteOpKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::AppendEvent => "append_event",
            Self::ReadEvents => "read_events",
        }
    }
}

/// Per-kind aggregate over the bounded SQLite latency window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SqliteLatencySummary {
    pub samples: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
}

/// Event-loop scheduling-delay summary over the bounded window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventLoopDelaySummary {
    pub samples: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
}

/// One turn wall-clock latency observation, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnLatencyEvent {
    pub latency_ms: u64,
}

/// One provider round-trip observation attributed to a task identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundTripEvent {
    pub task: String,
}

/// Nearest-rank percentiles over the bounded latency window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TurnLatencyPercentiles {
    pub samples: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
}

/// Wakeup counter plus the rate over its observation window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WakeupSummary {
    pub total: u64,
    /// Wakeups per second between the first and the latest observation.
    /// `None` until at least two observations bracket a non-zero window.
    pub per_second: Option<f64>,
}

#[derive(Default)]
struct ResourceSink {
    latency_samples: VecDeque<u64>,
    round_trips: BTreeMap<String, u64>,
    dropped_rows: u64,
    wakeups_total: u64,
    wakeups_first_observed: Option<Instant>,
    wakeups_last_observed: Option<Instant>,
    sqlite_samples: BTreeMap<SqliteOpKind, VecDeque<u64>>,
    event_loop_delay_samples: VecDeque<u64>,
}

static SINK: Mutex<Option<ResourceSink>> = Mutex::new(None);

fn with_sink<R>(f: impl FnOnce(&mut ResourceSink) -> R) -> R {
    let mut guard = SINK.lock().expect("resource telemetry sink lock poisoned");
    let sink = guard.get_or_insert_with(ResourceSink::default);
    f(sink)
}

/// Record one turn wall-clock latency observation.
pub fn record_turn_latency(event: TurnLatencyEvent) {
    tracing::info!(
        operation = "resource",
        metric = "turn_latency",
        latency_ms = event.latency_ms,
        "turn wall-clock latency observed"
    );
    with_sink(|sink| {
        if sink.latency_samples.len() >= MAX_LATENCY_SAMPLES {
            sink.latency_samples.pop_front();
        }
        sink.latency_samples.push_back(event.latency_ms);
    });
}

/// Record one provider round-trip attributed to a task identity.
pub fn record_round_trip(event: RoundTripEvent) {
    tracing::info!(
        operation = "resource",
        metric = "round_trip",
        task = %event.task,
        "provider round-trip observed"
    );
    with_sink(|sink| {
        if !sink.round_trips.contains_key(&event.task)
            && sink.round_trips.len() >= MAX_DISTINCT_ROWS
        {
            sink.dropped_rows += 1;
            return;
        }
        *sink.round_trips.entry(event.task).or_default() += 1;
    });
}

/// Record one daemon wakeup. The rate is derived from the observation
/// window, not from a wall clock sampled at record time.
pub fn record_wakeup() {
    let total = with_sink(|sink| {
        let now = Instant::now();
        sink.wakeups_total += 1;
        sink.wakeups_first_observed.get_or_insert(now);
        sink.wakeups_last_observed = Some(now);
        sink.wakeups_total
    });
    tracing::info!(
        operation = "resource",
        metric = "wakeup",
        wakeups_total = total,
        "daemon wakeup observed"
    );
}

/// Record one SQLite operation's wall-clock duration (OPS-003). Recording is
/// done on the same thread that performed the operation, so the measurement
/// includes lock/commit wait inside the operation — the honest figure.
pub fn record_sqlite_op(kind: SqliteOpKind, duration: std::time::Duration) {
    let micros = duration.as_micros() as u64;
    tracing::trace!(
        operation = "resource",
        metric = "sqlite_op",
        op = kind.label(),
        micros,
        "sqlite operation observed"
    );
    with_sink(|sink| {
        let window = sink.sqlite_samples.entry(kind).or_default();
        if window.len() >= MAX_SQLITE_LATENCY_SAMPLES {
            window.pop_front();
        }
        window.push_back(micros);
    });
}

/// Record one event-loop scheduling delay (OPS-003). Callers measure this by
/// comparing a periodic tick's scheduled deadline to the time the task was
/// actually polled; the value is scheduler delay, not workload duration.
pub fn record_event_loop_delay(delay: std::time::Duration) {
    let micros = delay.as_micros() as u64;
    with_sink(|sink| {
        if sink.event_loop_delay_samples.len() >= MAX_EVENT_LOOP_DELAY_SAMPLES {
            sink.event_loop_delay_samples.pop_front();
        }
        sink.event_loop_delay_samples.push_back(micros);
    });
}

/// Snapshot the SQLite latency window for one operation kind. `None` until at
/// least one sample has been recorded for that kind.
pub fn sqlite_latency_summary(kind: SqliteOpKind) -> Option<SqliteLatencySummary> {
    with_sink(|sink| {
        let window = sink.sqlite_samples.get(&kind)?;
        if window.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = window.iter().copied().collect();
        sorted.sort_unstable();
        Some(SqliteLatencySummary {
            samples: sorted.len(),
            p50_us: nearest_rank(&sorted, 50),
            p95_us: nearest_rank(&sorted, 95),
            max_us: *sorted.last().unwrap_or(&0),
        })
    })
}

/// Snapshot the event-loop scheduling-delay window. `None` until at least one
/// delay sample has been recorded.
pub fn event_loop_delay_summary() -> Option<EventLoopDelaySummary> {
    with_sink(|sink| {
        if sink.event_loop_delay_samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = sink.event_loop_delay_samples.iter().copied().collect();
        sorted.sort_unstable();
        Some(EventLoopDelaySummary {
            samples: sorted.len(),
            p50_us: nearest_rank(&sorted, 50),
            p95_us: nearest_rank(&sorted, 95),
            max_us: *sorted.last().unwrap_or(&0),
        })
    })
}

/// Nearest-rank percentile over a pre-sorted sample slice.
fn nearest_rank(sorted: &[u64], percent: u64) -> u64 {
    let rank = (percent * sorted.len() as u64).div_ceil(100);
    let index = rank.saturating_sub(1).min(sorted.len() as u64 - 1) as usize;
    sorted[index]
}

/// Snapshot p50/p95 turn latency over the bounded recent-sample window.
/// `None` until at least one sample has been recorded.
pub fn turn_latency_percentiles() -> Option<TurnLatencyPercentiles> {
    with_sink(|sink| {
        if sink.latency_samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = sink.latency_samples.iter().copied().collect();
        sorted.sort_unstable();
        Some(TurnLatencyPercentiles {
            samples: sorted.len(),
            p50_ms: nearest_rank(&sorted, 50),
            p95_ms: nearest_rank(&sorted, 95),
        })
    })
}

/// Snapshot per-task round-trip counts, ordered by task identity.
pub fn round_trip_aggregates() -> Vec<(String, u64)> {
    with_sink(|sink| sink.round_trips.clone().into_iter().collect())
}

/// Rows dropped because the distinct-task cap was reached.
pub fn resource_dropped_rows() -> u64 {
    with_sink(|sink| sink.dropped_rows)
}

/// Snapshot the wakeup counter and its observation-window rate.
pub fn wakeup_summary() -> WakeupSummary {
    with_sink(|sink| {
        let per_second = match (sink.wakeups_first_observed, sink.wakeups_last_observed) {
            (Some(first), Some(last)) if sink.wakeups_total >= 2 => {
                let elapsed = last.saturating_duration_since(first).as_secs_f64();
                (elapsed > 0.0).then(|| sink.wakeups_total as f64 / elapsed)
            }
            _ => None,
        };
        WakeupSummary {
            total: sink.wakeups_total,
            per_second,
        }
    })
}

/// Raw latency samples currently in the bounded window, oldest first.
///
/// Bounded by `MAX_LATENCY_SAMPLES` (1024). The length is the window length,
/// not lifetime total — `turn_latency_percentiles().samples` and this vec have
/// the same bound. Callers needing a point-in-time view should take a
/// `snapshot_resource_telemetry` and derive percentiles from the snapshot's
/// `latency_samples` to avoid races between `latency_raw_samples` and
/// `turn_latency_percentiles`.
pub fn latency_raw_samples() -> Vec<u64> {
    with_sink(|sink| sink.latency_samples.iter().copied().collect())
}

/// Point-in-time snapshot of every bounded counter in the sink.
///
/// The snapshot is taken under one lock acquisition so latency window,
/// round-trip rows, `dropped_rows`, and wakeup totals are mutually
/// consistent. Counters remain bounded after the snapshot — the snapshot
/// copies the current window/rows, it does not reset them.
///
/// Reset/snapshot semantics (explicit):
/// - `reset_resource_telemetry()` clears all windows/rows/totals to zero.
///   Bounded invariants (`MAX_LATENCY_SAMPLES`, `MAX_DISTINCT_ROWS`) still
///   hold after reset — the next samples fill a fresh empty window.
/// - `snapshot_resource_telemetry()` reads without resetting. For
///   scenario-local attribution (P2-1), take a snapshot before the scenario,
///   run scenario-local traffic, take a second snapshot, and attribute the
///   delta — do not copy process-global totals to each scenario row.
/// - `dropped_rows` is monotonic until reset; a non-zero value means the
///   distinct-task cap was hit and some per-task rows were not retained.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSnapshot {
    pub latency_samples: Vec<u64>,
    pub round_trips: Vec<(String, u64)>,
    pub dropped_rows: u64,
    pub wakeups_total: u64,
    pub wakeups_per_second: Option<f64>,
    pub sqlite_samples: Vec<(SqliteOpKind, Vec<u64>)>,
    pub event_loop_delay_samples: Vec<u64>,
}

pub fn snapshot_resource_telemetry() -> ResourceSnapshot {
    with_sink(|sink| {
        let per_second = match (sink.wakeups_first_observed, sink.wakeups_last_observed) {
            (Some(first), Some(last)) if sink.wakeups_total >= 2 => {
                let elapsed = last.saturating_duration_since(first).as_secs_f64();
                (elapsed > 0.0).then(|| sink.wakeups_total as f64 / elapsed)
            }
            _ => None,
        };
        ResourceSnapshot {
            latency_samples: sink.latency_samples.iter().copied().collect(),
            round_trips: sink.round_trips.clone().into_iter().collect(),
            dropped_rows: sink.dropped_rows,
            wakeups_total: sink.wakeups_total,
            wakeups_per_second: per_second,
            sqlite_samples: sink
                .sqlite_samples
                .iter()
                .map(|(kind, window)| (*kind, window.iter().copied().collect()))
                .collect(),
            event_loop_delay_samples: sink.event_loop_delay_samples.iter().copied().collect(),
        }
    })
}

/// Reset the sink. Intended for tests, diagnostics, and explicit
/// scenario-isolation in the baseline harness (P2-1). The harness should
/// either reset before each scenario or capture before/after snapshots and
/// attribute the delta — both are explicit, bounded semantics. Do not call
/// reset from live engine turn paths.
pub fn reset_resource_telemetry() {
    with_sink(|sink| {
        sink.latency_samples.clear();
        sink.round_trips.clear();
        sink.dropped_rows = 0;
        sink.wakeups_total = 0;
        sink.wakeups_first_observed = None;
        sink.wakeups_last_observed = None;
        sink.sqlite_samples.clear();
        sink.event_loop_delay_samples.clear();
    });
}
