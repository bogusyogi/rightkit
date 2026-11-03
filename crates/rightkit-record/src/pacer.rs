//! Consumer-side pacing. The capture delivers at the content's rate and the
//! worker keeps frames on the requested fps grid, dropping the rest. Pure, so
//! it is unit tested with synthetic timestamps and needs no window.

use rightkit_contracts::types::record_event::DropReason;

pub const NS_PER_SEC: i64 = 1_000_000_000;

/// Frames the capture callback lost since the previous arrival. The callback
/// only counts these; the worker turns them into events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Losses {
    /// Staging slots or the channel were full.
    pub backpressure: u64,
    /// Size mismatch or copy failure, reported as unknown.
    pub unknown: u64,
}

/// One closed run of consecutive drops, ready to be written as a frame_dropped event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Closed {
    pub reason: DropReason,
    pub count: u32,
    /// Time the run spans: `count` frame intervals.
    pub interval_ms: f64,
}

/// Groups drops into runs. Consecutive drops with the same reason form one run.
/// A kept frame, or a change of reason, closes the run.
///
/// Reasons:
/// - `Backpressure`: callback losses that arrived with a frame.
/// - `Unknown`: size or copy losses.
///
/// Only frames that were received and then lost are drops. Silence is not: the
/// capture delivers frames only when content changes, so an arrival gap on static
/// content is normal. Raw arrival intervals go to the summary (`ArrivalStats`), and
/// missed state changes are judged later by the scenario runtime against the bundle.
///
/// Pacer decimation (the source is faster than the requested fps) is intentional
/// down-sampling, not a drop: it emits no event and does not close an open run.
pub struct DropAggregator {
    frame_interval_ms: f64,
    pending: Option<(DropReason, u32)>,
}

impl DropAggregator {
    pub fn new(fps: u32) -> Self {
        Self {
            frame_interval_ms: 1000.0 / f64::from(fps),
            pending: None,
        }
    }

    /// Records one arrival: whether the pacer kept it, and the callback losses
    /// since the previous arrival. Closed runs are pushed to `closed`.
    pub fn arrival(&mut self, kept: bool, losses: Losses, closed: &mut Vec<Closed>) {
        self.add(
            DropReason::Backpressure,
            u32::try_from(losses.backpressure).unwrap_or(u32::MAX),
            closed,
        );
        self.add(
            DropReason::Unknown,
            u32::try_from(losses.unknown).unwrap_or(u32::MAX),
            closed,
        );
        if kept {
            self.close(closed);
        }
    }

    /// Closes any run still open at the end of the capture.
    pub fn finish(&mut self, closed: &mut Vec<Closed>) {
        self.close(closed);
    }

    fn add(&mut self, reason: DropReason, count: u32, closed: &mut Vec<Closed>) {
        if count == 0 {
            return;
        }
        match &mut self.pending {
            Some((open, total)) if *open == reason => *total = total.saturating_add(count),
            _ => {
                self.close(closed);
                self.pending = Some((reason, count));
            }
        }
    }

    fn close(&mut self, closed: &mut Vec<Closed>) {
        if let Some((reason, count)) = self.pending.take() {
            closed.push(Closed {
                reason,
                count,
                interval_ms: f64::from(count) * self.frame_interval_ms,
            });
        }
    }
}

/// Raw inter-arrival intervals of delivered frames, summarized for the capture summary.
#[derive(Debug, Default)]
pub struct ArrivalStats {
    last_ns: Option<i64>,
    intervals_ms: Vec<f64>,
}

impl ArrivalStats {
    pub fn record(&mut self, host_ns: i64) {
        if let Some(prev) = self.last_ns {
            self.intervals_ms.push((host_ns - prev) as f64 / 1e6);
        }
        self.last_ns = Some(host_ns);
    }

    /// count, mean, p50, p95 and max of the intervals in milliseconds.
    pub fn summary(&self) -> serde_json::Value {
        let mut v = self.intervals_ms.clone();
        if v.is_empty() {
            return serde_json::json!({ "count": 0 });
        }
        v.sort_by(f64::total_cmp);
        let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        serde_json::json!({
            "count": v.len(),
            "meanMs": v.iter().sum::<f64>() / v.len() as f64,
            "p50Ms": pct(0.5),
            "p95Ms": pct(0.95),
            "maxMs": v[v.len() - 1],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Drop,
}

#[derive(Debug)]
pub struct Pacer {
    interval_ns: i64,
    tolerance_ns: i64,
    next_due_ns: Option<i64>,
}

impl Pacer {
    /// `fps` must be 30 or 60 (checked by the CLI).
    pub fn new(fps: u32) -> Self {
        let interval_ns = NS_PER_SEC / i64::from(fps);
        // A frame is accepted when it lands up to a third of an interval early.
        // That admits normal jitter but rejects the in-between frame of a 2:1 source.
        Self {
            interval_ns,
            tolerance_ns: interval_ns / 3,
            next_due_ns: None,
        }
    }

    /// Offers one frame timestamp (host ns, monotonic, non-decreasing).
    pub fn offer(&mut self, host_ns: i64) -> Decision {
        match self.next_due_ns {
            None => {
                self.next_due_ns = Some(host_ns + self.interval_ns);
                Decision::Keep
            }
            Some(due) if host_ns + self.tolerance_ns >= due => {
                // Advance the grid by one interval. If the source stalled, re-anchor
                // to the arriving frame so the output does not burst to catch up.
                let next =
                    (due + self.interval_ns).max(host_ns + self.interval_ns - self.tolerance_ns);
                self.next_due_ns = Some(next);
                Decision::Keep
            }
            Some(_) => Decision::Drop,
        }
    }
}
