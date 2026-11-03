//! Turns arrivals into the helper events the core consumes: pacing to the
//! requested fps, drop runs for frames that were received and then lost, and the
//! readiness marker and frame events for kept frames. Nothing here touches a
//! window or a device, so the integration tests drive it with synthetic frames.
//!
//! Silence is not a drop. Static content delivers no frames, and a long arrival
//! gap with no loss reports nothing.

use serde_json::Value;

use crate::backend::{FrameSample, HelperEvent};
use crate::pacer::{ArrivalStats, Closed, Decision, DropAggregator, Losses, Pacer};

/// Pacing, drop aggregation and arrival statistics for one take.
pub struct Emitter {
    pacer: Pacer,
    drops: DropAggregator,
    arrivals: ArrivalStats,
    closed: Vec<Closed>,
    last_host_ns: u64,
    first_host_ns: Option<u64>,
    kept: u64,
}

/// A frame the pacer kept, before its pixels are read and hashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptFrame {
    pub index: u64,
    pub host_ns: u64,
    pub first_host_ns: u64,
}

impl KeptFrame {
    /// Sample time relative to the first kept frame, in nanoseconds.
    pub fn sample_pts_ns(&self) -> u64 {
        self.host_ns.saturating_sub(self.first_host_ns)
    }

    /// The readiness marker for frame 0, a plain frame event afterwards.
    pub fn event(&self, raw_hash: String) -> HelperEvent {
        let sample = FrameSample {
            index: self.index,
            host_ns: self.host_ns,
            sample_pts_ns: self.sample_pts_ns(),
            raw_hash,
        };
        if self.index == 0 {
            HelperEvent::Ready(sample)
        } else {
            HelperEvent::Frame(sample)
        }
    }
}

impl Emitter {
    pub fn new(fps: u32) -> Self {
        Self {
            pacer: Pacer::new(fps),
            drops: DropAggregator::new(fps),
            arrivals: ArrivalStats::default(),
            closed: Vec::new(),
            last_host_ns: 0,
            first_host_ns: None,
            kept: 0,
        }
    }

    /// Offers one arrival at `host_ns`, with the callback losses since the
    /// previous arrival. Returns true when the pacer keeps the frame. A drop run
    /// that this arrival closes is queued for [`Emitter::drain`].
    pub fn offer(&mut self, host_ns: u64, losses: Losses) -> bool {
        self.last_host_ns = host_ns;
        self.arrivals.record(host_ns as i64);
        let kept = self.pacer.offer(host_ns as i64) == Decision::Keep;
        self.drops.arrival(kept, losses, &mut self.closed);
        kept
    }

    /// Claims the next index for a frame that was offered and kept.
    pub fn keep(&mut self, host_ns: u64) -> KeptFrame {
        let first_host_ns = *self.first_host_ns.get_or_insert(host_ns);
        let frame = KeptFrame {
            index: self.kept,
            host_ns,
            first_host_ns,
        };
        self.kept += 1;
        frame
    }

    /// Appends the queued drop runs as `Dropped` events, stamped at the last arrival.
    pub fn drain(&mut self, out: &mut Vec<HelperEvent>) {
        for run in self.closed.drain(..) {
            out.push(HelperEvent::Dropped {
                host_ns: self.last_host_ns,
                count: run.count,
                interval_ms: run.interval_ms,
                reason: run.reason,
            });
        }
    }

    /// Closes the run still open at the end of the take and drains everything queued.
    pub fn finish(&mut self, out: &mut Vec<HelperEvent>) {
        self.drops.finish(&mut self.closed);
        self.drain(out);
    }

    /// Frames kept so far.
    pub fn kept(&self) -> u64 {
        self.kept
    }

    /// Raw arrival-interval statistics, for the helper's summary.
    pub fn arrival_summary(&self) -> Value {
        self.arrivals.summary()
    }
}
