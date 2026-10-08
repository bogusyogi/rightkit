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

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame timestamps for `count` frames at `source_fps`, starting at `start_ns`.
    fn ideal(source_fps: i64, count: i64, start_ns: i64) -> Vec<i64> {
        (0..count)
            .map(|k| start_ns + k * NS_PER_SEC / source_fps)
            .collect()
    }

    fn kept_count(fps: u32, times: &[i64]) -> usize {
        let mut pacer = Pacer::new(fps);
        times
            .iter()
            .filter(|&&t| pacer.offer(t) == Decision::Keep)
            .count()
    }

    #[test]
    fn first_frame_is_always_kept() {
        let mut pacer = Pacer::new(30);
        assert_eq!(pacer.offer(123_456_789), Decision::Keep);
    }

    #[test]
    fn sixty_source_paced_to_thirty_keeps_every_other_frame() {
        assert_eq!(kept_count(30, &ideal(60, 60, 0)), 30);
    }

    #[test]
    fn sixty_source_at_sixty_keeps_all() {
        assert_eq!(kept_count(60, &ideal(60, 60, 0)), 60);
    }

    #[test]
    fn thirty_source_at_thirty_keeps_all() {
        assert_eq!(kept_count(30, &ideal(30, 30, 0)), 30);
    }

    #[test]
    fn thirty_source_never_duplicates_for_sixty_target() {
        // The pacer only drops; a slower source is kept as delivered.
        assert_eq!(kept_count(60, &ideal(30, 30, 0)), 30);
    }

    #[test]
    fn small_jitter_does_not_change_the_thirty_fps_count() {
        // Alternating +-1 ms on a 60 Hz source.
        let times: Vec<i64> = ideal(60, 60, 0)
            .into_iter()
            .enumerate()
            .map(|(k, t)| {
                if k % 2 == 0 {
                    t + 1_000_000
                } else {
                    t - 1_000_000
                }
            })
            .collect();
        let kept = kept_count(30, &times);
        assert!((29..=31).contains(&kept), "kept {kept}");
    }

    #[test]
    fn stall_reanchors_without_bursting() {
        // One frame, a 1 s stall, then 60 Hz resumes. The 60 fps grid must
        // keep about one frame per 16.7 ms in the following second, not a burst.
        let mut times = vec![0];
        times.extend(ideal(60, 60, 1_000_000_000));
        let kept = kept_count(60, &times);
        assert!((59..=61).contains(&kept), "kept {kept}");
    }

    #[test]
    fn drops_are_reported_as_drop_not_error() {
        let mut pacer = Pacer::new(30);
        assert_eq!(pacer.offer(0), Decision::Keep);
        // 16 ms after the first frame is half an interval early for a 30 fps grid.
        assert_eq!(pacer.offer(16_000_000), Decision::Drop);
    }
}

#[cfg(test)]
mod aggregation_tests {
    use super::*;

    /// Host time of arrival `i` on a `fps` source, in ns.
    fn at(i: i64, fps: i64) -> i64 {
        i * NS_PER_SEC / fps
    }

    fn run(fps: u32, arrivals: &[(i64, bool, Losses)]) -> Vec<Closed> {
        let mut agg = DropAggregator::new(fps);
        let mut closed = Vec::new();
        for &(_host, kept, losses) in arrivals {
            agg.arrival(kept, losses, &mut closed);
        }
        agg.finish(&mut closed);
        closed
    }

    fn assert_interval(actual: f64, frames: u32, fps: u32) {
        let expected = f64::from(frames) * 1000.0 / f64::from(fps);
        assert!(
            (actual - expected).abs() < 1e-9,
            "interval {actual} vs {expected}"
        );
    }

    const NONE: Losses = Losses {
        backpressure: 0,
        unknown: 0,
    };

    #[test]
    fn consecutive_losses_aggregate_into_one_event() {
        let one = Losses {
            backpressure: 1,
            unknown: 0,
        };
        let closed = run(
            60,
            &[
                (at(0, 60), true, NONE),
                (at(2, 60), false, one),
                (at(4, 60), false, one),
                (at(6, 60), true, one),
            ],
        );
        assert_eq!(closed.len(), 1, "{closed:?}");
        assert_eq!(closed[0].reason, DropReason::Backpressure);
        assert_eq!(closed[0].count, 3);
        assert_interval(closed[0].interval_ms, 3, 60);
    }

    #[test]
    fn pacer_decimation_is_not_a_drop() {
        // A 60 Hz source paced to 30 fps: keep, skip, keep, skip, keep.
        let arrivals: Vec<_> = (0..5).map(|i| (at(i, 60), i % 2 == 0, NONE)).collect();
        assert!(run(60, &arrivals).is_empty());
    }

    #[test]
    fn a_kept_frame_with_no_drops_produces_no_event() {
        let arrivals: Vec<_> = (0..10).map(|i| (at(i, 60), true, NONE)).collect();
        assert!(run(60, &arrivals).is_empty());
    }

    #[test]
    fn silence_is_not_a_drop() {
        // Static content: a long arrival gap with no frames lost.
        let closed = run(60, &[(at(0, 60), true, NONE), (at(300, 60), true, NONE)]);
        assert!(closed.is_empty(), "{closed:?}");
    }

    #[test]
    fn arrival_stats_summarize_raw_intervals() {
        let mut stats = ArrivalStats::default();
        for ms in [0, 16, 33, 50, 150] {
            stats.record(ms * 1_000_000);
        }
        let s = stats.summary();
        assert_eq!(s["count"], 4);
        assert_eq!(s["maxMs"], 100.0);
        assert_eq!(s["p50Ms"], 17.0);
    }

    #[test]
    fn callback_losses_are_backpressure_and_are_not_double_counted_as_gap() {
        // Three intervals between arrivals with two frames lost in the callback.
        let losses = Losses {
            backpressure: 2,
            unknown: 0,
        };
        let closed = run(60, &[(at(0, 60), true, NONE), (at(3, 60), true, losses)]);
        assert_eq!(closed.len(), 1, "{closed:?}");
        assert_eq!(closed[0].reason, DropReason::Backpressure);
        assert_eq!(closed[0].count, 2);
    }

    #[test]
    fn a_gap_reports_only_the_frames_actually_lost() {
        // Five intervals between arrivals with one frame lost to backpressure: one drop, no gap.
        let losses = Losses {
            backpressure: 1,
            unknown: 0,
        };
        let closed = run(60, &[(at(0, 60), true, NONE), (at(5, 60), true, losses)]);
        let reasons: Vec<_> = closed.iter().map(|c| (c.reason, c.count)).collect();
        assert_eq!(reasons, vec![(DropReason::Backpressure, 1)]);
    }

    #[test]
    fn a_change_of_reason_closes_the_run() {
        // A copy loss followed by a backpressure loss: two events, not one.
        let copy = Losses {
            backpressure: 0,
            unknown: 1,
        };
        let losses = Losses {
            backpressure: 1,
            unknown: 0,
        };
        let closed = run(
            60,
            &[
                (at(0, 60), true, NONE),
                (at(1, 60), false, copy),
                (at(3, 60), true, losses),
            ],
        );
        let reasons: Vec<_> = closed.iter().map(|c| (c.reason, c.count)).collect();
        assert_eq!(
            reasons,
            vec![(DropReason::Unknown, 1), (DropReason::Backpressure, 1)]
        );
    }

    #[test]
    fn size_and_copy_losses_are_unknown() {
        let losses = Losses {
            backpressure: 0,
            unknown: 2,
        };
        let closed = run(30, &[(at(0, 30), true, NONE), (at(1, 30), true, losses)]);
        assert_eq!(closed.len(), 1);
        assert_eq!(
            (closed[0].reason, closed[0].count),
            (DropReason::Unknown, 2)
        );
        assert_interval(closed[0].interval_ms, 2, 30);
    }

    #[test]
    fn first_arrival_has_no_gap_to_report() {
        let closed = run(60, &[(at(100, 60), true, NONE)]);
        assert!(closed.is_empty());
    }
}
