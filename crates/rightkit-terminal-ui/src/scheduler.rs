//! Frame scheduling.
//!
//! Two bounded lanes feed one renderer.
//!
//! The **coalescing lane** carries model-driven invalidation. It stores only
//! the earliest pending deadline plus one `Notify` permit: a million token
//! updates consume the same memory as one update.
//!
//! The **immediate lane** carries keystrokes/cancel-class input through a tiny
//! bounded channel. A full channel means an immediate wake is already pending,
//! so another signal can be safely coalesced without allocating.
//!
//! Both lanes share one frame-rate limiter. An immediate frame never clears a
//! pending coalesced deadline; durable settlement/final repaint therefore
//! remains fair even when input arrives during streaming.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use tokio::sync::{Notify, broadcast, mpsc};
use tokio::time::{Instant, sleep_until};

/// Minimum spacing between emitted frames: 120 frames per second.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);

/// Content-free renderer pressure counters. No model/tool payload enters this
/// structure, so it is safe to export as diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderMetricsSnapshot {
    pub redraws: u64,
    pub reflows: u64,
    pub coalesced_requests: u64,
    pub immediate_requests: u64,
    pub slow_frames: u64,
    pub input_latency_samples: u64,
    pub input_latency_micros: u64,
}

#[derive(Debug, Default)]
pub struct RenderMetrics {
    redraws: AtomicU64,
    reflows: AtomicU64,
    coalesced_requests: AtomicU64,
    immediate_requests: AtomicU64,
    slow_frames: AtomicU64,
    input_latency_samples: AtomicU64,
    input_latency_micros: AtomicU64,
}

impl RenderMetrics {
    pub fn snapshot(&self) -> RenderMetricsSnapshot {
        RenderMetricsSnapshot {
            redraws: self.redraws.load(Ordering::Relaxed),
            reflows: self.reflows.load(Ordering::Relaxed),
            coalesced_requests: self.coalesced_requests.load(Ordering::Relaxed),
            immediate_requests: self.immediate_requests.load(Ordering::Relaxed),
            slow_frames: self.slow_frames.load(Ordering::Relaxed),
            input_latency_samples: self.input_latency_samples.load(Ordering::Relaxed),
            input_latency_micros: self.input_latency_micros.load(Ordering::Relaxed),
        }
    }

    pub fn record_frame(&self, elapsed: Duration, slow_threshold: Duration) {
        self.redraws.fetch_add(1, Ordering::Relaxed);
        if elapsed >= slow_threshold {
            self.slow_frames.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_reflow(&self) {
        self.reflows.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_input_latency(&self, latency: Duration) {
        self.input_latency_samples.fetch_add(1, Ordering::Relaxed);
        self.input_latency_micros.fetch_add(
            latency.as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }
}

/// Tiny bounded priority-signal queue. Payloads carry no content; when full,
/// at least one immediate repaint is already pending and another signal adds
/// no semantic information.
const IMMEDIATE_QUEUE_CAPACITY: usize = 8;

/// A deadline far enough out to mean "nothing pending" without a separate
/// timer allocation.
const NEVER: Duration = Duration::from_secs(60 * 60 * 24 * 365);

#[derive(Debug, Default)]
struct CoalescedFrameSignal {
    earliest: Mutex<Option<Instant>>,
    notify: Notify,
}

impl CoalescedFrameSignal {
    fn request(&self, deadline: Instant) {
        {
            let mut pending = self
                .earliest
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *pending = Some(match *pending {
                Some(current) => current.min(deadline),
                None => deadline,
            });
        }
        // `notify_one` stores at most one permit when nobody is waiting, so
        // producer rate cannot turn into queue growth.
        self.notify.notify_one();
    }

    fn take(&self) -> Option<Instant> {
        self.earliest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[cfg(test)]
    fn pending(&self) -> Option<Instant> {
        *self
            .earliest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Handle used to ask for a frame. Cheap to clone; hand one to every component
/// that can invalidate the display.
#[derive(Debug, Clone)]
pub struct FrameRequester {
    coalesce: Arc<CoalescedFrameSignal>,
    immediate: mpsc::Sender<()>,
    metrics: Arc<RenderMetrics>,
}

impl FrameRequester {
    /// Ask for a frame as soon as the rate limiter permits. Coalesces with any
    /// other pending request in constant space.
    pub fn schedule_frame(&self) {
        self.metrics
            .coalesced_requests
            .fetch_add(1, Ordering::Relaxed);
        self.coalesce.request(Instant::now());
    }

    /// Ask for a frame at a point in the future -- a spinner tick, a timed
    /// status change. Still coalesces; an earlier request wins.
    pub fn schedule_frame_in(&self, delay: Duration) {
        self.metrics
            .coalesced_requests
            .fetch_add(1, Ordering::Relaxed);
        self.coalesce.request(Instant::now() + delay);
    }

    /// Ask for a frame *now*, bypassing any pending coalesced deadline.
    ///
    /// Reserved for input/cancel-class responsiveness. `try_send` is
    /// deliberate: a full tiny queue already represents pending immediate
    /// work, so blocking or allocating another entry would add no value.
    pub fn schedule_immediate(&self) {
        self.metrics
            .immediate_requests
            .fetch_add(1, Ordering::Relaxed);
        let _ = self.immediate.try_send(());
    }

    pub fn metrics(&self) -> Arc<RenderMetrics> {
        self.metrics.clone()
    }

    pub fn record_frame(&self, elapsed: Duration, slow_threshold: Duration) {
        self.metrics.record_frame(elapsed, slow_threshold);
    }

    pub fn record_reflow(&self) {
        self.metrics.record_reflow();
    }

    pub fn record_input_latency(&self, latency: Duration) {
        self.metrics.record_input_latency(latency);
    }
}

/// Enforces the minimum spacing between frames. Shared by both lanes so they
/// cannot conspire to exceed the frame budget.
#[derive(Debug, Default)]
pub struct FrameRateLimiter {
    last_emitted: Option<Instant>,
}

impl FrameRateLimiter {
    /// Push a requested deadline out far enough to respect the frame budget.
    pub fn clamp_deadline(&self, requested: Instant) -> Instant {
        match self.last_emitted {
            Some(last) => requested.max(last + MIN_FRAME_INTERVAL),
            None => requested,
        }
    }

    /// Whether a frame may be emitted at `now` without waiting.
    pub fn may_emit_at(&self, now: Instant) -> bool {
        match self.last_emitted {
            Some(last) => now >= last + MIN_FRAME_INTERVAL,
            None => true,
        }
    }

    /// Record that a frame went out.
    pub fn mark_emitted(&mut self, at: Instant) {
        self.last_emitted = Some(at);
    }
}

/// Spawn the scheduler actor. Returns the handle used to request frames.
///
/// `draw_tx` is broadcast so several consumers can observe frames, but it is
/// fire-and-forget: a lagging receiver misses frames rather than stalling the
/// scheduler. Missing a frame is harmless because every frame renders current
/// state in full rather than a delta.
pub fn spawn(draw_tx: broadcast::Sender<()>) -> FrameRequester {
    let coalesce = Arc::new(CoalescedFrameSignal::default());
    let (immediate_tx, immediate_rx) = mpsc::channel(IMMEDIATE_QUEUE_CAPACITY);
    let metrics = Arc::new(RenderMetrics::default());

    tokio::spawn(run(coalesce.clone(), immediate_rx, draw_tx));

    FrameRequester {
        coalesce,
        immediate: immediate_tx,
        metrics,
    }
}

async fn run(
    coalesce: Arc<CoalescedFrameSignal>,
    mut immediate_rx: mpsc::Receiver<()>,
    draw_tx: broadcast::Sender<()>,
) {
    let mut limiter = FrameRateLimiter::default();
    let mut next_deadline: Option<Instant> = None;

    loop {
        // The immediate lane may have emitted since this batch's deadline was
        // first clamped. Re-clamp at consumption time: keep the pending final
        // repaint, but never emit it inside the shared frame-budget window.
        let target = next_deadline
            .map(|deadline| limiter.clamp_deadline(deadline))
            .unwrap_or_else(|| Instant::now() + NEVER);

        tokio::select! {
            biased;

            // A due coalesced deadline must win over ready ingress. Without
            // this first branch, a continuously refilled immediate or notify
            // lane can keep the biased select ready forever and starve the
            // durable settlement/final repaint.
            () = sleep_until(target) => {
                if next_deadline.take().is_some() {
                    let now = Instant::now();
                    limiter.mark_emitted(now);
                    let _ = draw_tx.send(());
                }
            }

            // Immediate lane. Emit now if the budget allows; otherwise fold it
            // into the same pending deadline. The bounded queue makes input
            // priority finite rather than allowing producer backlog to starve
            // durable settlement forever. This branch intentionally precedes
            // coalesced model ingress: a continuously refreshed model signal
            // must not delay cancellation or keystroke handling.
            received = immediate_rx.recv() => {
                if received.is_none() { return; }
                let now = Instant::now();
                if limiter.may_emit_at(now) {
                    limiter.mark_emitted(now);
                    let _ = draw_tx.send(());
                    // Deliberately NOT clearing next_deadline: this immediate
                    // repaint does not discharge model/terminal invalidation.
                } else {
                    let clamped = limiter.clamp_deadline(now);
                    next_deadline = Some(match next_deadline {
                        Some(current) => current.min(clamped),
                        None => clamped,
                    });
                }
                continue;
            }

            // One notification represents any number of model-driven
            // invalidations. Pull the shared earliest deadline and merge it
            // with the scheduler's already-pending batch.
            _ = coalesce.notify.notified() => {
                if let Some(requested) = coalesce.take() {
                    let clamped = limiter.clamp_deadline(requested);
                    next_deadline = Some(match next_deadline {
                        Some(current) => current.min(clamped),
                        None => clamped,
                    });
                }
                continue;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> (broadcast::Sender<()>, broadcast::Receiver<()>) {
        broadcast::channel(16)
    }

    fn drain(rx: &mut broadcast::Receiver<()>) -> usize {
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        n
    }

    #[test]
    fn due_repaint_branch_precedes_continuously_ready_ingress() {
        // `run` deliberately uses a biased select. Keep the source-level
        // priority contract executable so a later branch reorder cannot
        // silently reintroduce final-repaint starvation under sustained
        // immediate/model pressure.
        let source = include_str!("scheduler.rs");
        let biased = source
            .find("biased;")
            .expect("scheduler must explicitly prioritize ready branches");
        let due = source
            .find("() = sleep_until(target)")
            .expect("scheduler must retain a due repaint branch");
        let coalesced = source
            .find("_ = coalesce.notify.notified()")
            .expect("scheduler must retain coalesced ingress");
        let immediate = source
            .find("received = immediate_rx.recv()")
            .expect("scheduler must retain immediate ingress");
        assert!(
            biased < due && due < immediate && immediate < coalesced,
            "biased due repaint must win before ingress, then immediate input must win over model ingress"
        );
    }

    #[test]
    fn clamp_pushes_a_request_out_to_respect_the_frame_budget() {
        let mut limiter = FrameRateLimiter::default();
        let t0 = Instant::now();
        assert_eq!(limiter.clamp_deadline(t0), t0);
        limiter.mark_emitted(t0);
        assert_eq!(limiter.clamp_deadline(t0), t0 + MIN_FRAME_INTERVAL);
        let far = t0 + Duration::from_millis(500);
        assert_eq!(limiter.clamp_deadline(far), far);
    }

    #[test]
    fn may_emit_is_false_only_inside_the_budget_window() {
        let mut limiter = FrameRateLimiter::default();
        let t0 = Instant::now();
        assert!(limiter.may_emit_at(t0));
        limiter.mark_emitted(t0);
        assert!(!limiter.may_emit_at(t0));
        assert!(limiter.may_emit_at(t0 + MIN_FRAME_INTERVAL));
    }

    #[test]
    fn a_million_coalesced_requests_store_one_deadline() {
        let signal = CoalescedFrameSignal::default();
        let base = Instant::now();
        for offset in 0..1_000_000u64 {
            signal.request(base + Duration::from_nanos(offset));
        }
        assert_eq!(signal.pending(), Some(base));
        assert_eq!(signal.take(), Some(base));
        assert_eq!(signal.pending(), None);
    }

    #[test]
    fn metrics_are_content_free_and_record_pressure_only() {
        let metrics = RenderMetrics::default();
        metrics.record_reflow();
        metrics.record_frame(Duration::from_millis(20), Duration::from_millis(16));
        metrics.record_input_latency(Duration::from_micros(42));
        assert_eq!(
            metrics.snapshot(),
            RenderMetricsSnapshot {
                redraws: 1,
                reflows: 1,
                coalesced_requests: 0,
                immediate_requests: 0,
                slow_frames: 1,
                input_latency_samples: 1,
                input_latency_micros: 42,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_is_bounded_by_the_frame_budget_not_the_request_count() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        for _ in 0..500 {
            requester.schedule_frame();
        }
        let intervals = 4;
        tokio::time::advance(MIN_FRAME_INTERVAL * intervals).await;
        tokio::task::yield_now().await;
        let frames = drain(&mut rx);
        assert!(frames >= 1, "the burst produced no frame at all");
        assert!(frames <= intervals as usize + 1);
    }

    #[tokio::test(start_paused = true)]
    async fn million_request_producer_has_bounded_ingress_and_final_repaint() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        for _ in 0..1_000_000 {
            requester.schedule_frame();
        }
        tokio::task::yield_now().await;
        tokio::time::advance(MIN_FRAME_INTERVAL * 4).await;
        tokio::task::yield_now().await;
        assert!(drain(&mut rx) >= 1, "coalesced flood lost final repaint");
    }

    #[tokio::test(start_paused = true)]
    async fn frame_count_scales_with_elapsed_time_not_request_volume() {
        let intervals = 4u32;
        let frames_for = |requests: usize| async move {
            let (tx, mut rx) = channel();
            let requester = spawn(tx);
            for _ in 0..requests {
                requester.schedule_frame();
            }
            tokio::time::advance(MIN_FRAME_INTERVAL * intervals).await;
            tokio::task::yield_now().await;
            drain(&mut rx)
        };
        let ceiling = intervals as usize + 1;
        for requests in [50usize, 5_000] {
            let frames = frames_for(requests).await;
            assert!(frames >= 1);
            assert!(frames <= ceiling);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_immediate_request_does_not_discharge_a_pending_coalesced_batch() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        requester.schedule_frame_in(Duration::from_millis(200));
        tokio::task::yield_now().await;
        requester.schedule_immediate();
        tokio::task::yield_now().await;
        assert_eq!(drain(&mut rx), 1, "keystroke should draw immediately");
        tokio::time::advance(Duration::from_millis(400)).await;
        tokio::task::yield_now().await;
        assert_eq!(drain(&mut rx), 1, "pending coalesced batch was swallowed");
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_frame_reclamps_an_already_pending_deadline() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        requester.schedule_frame_in(Duration::from_millis(20));
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_millis(19)).await;
        requester.schedule_immediate();
        tokio::task::yield_now().await;
        assert_eq!(
            drain(&mut rx),
            1,
            "input should draw before the batch deadline"
        );

        tokio::time::advance(Duration::from_millis(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            drain(&mut rx),
            0,
            "old deadline bypassed the shared frame limit"
        );

        tokio::time::advance(Duration::from_millis(9)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            drain(&mut rx),
            1,
            "re-clamping must preserve the final repaint"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_earlier_request_wins_over_a_later_pending_one() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        requester.schedule_frame_in(Duration::from_millis(500));
        tokio::task::yield_now().await;
        requester.schedule_frame();
        tokio::task::yield_now().await;
        tokio::time::advance(MIN_FRAME_INTERVAL * 4).await;
        tokio::task::yield_now().await;
        assert_eq!(drain(&mut rx), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn back_to_back_immediate_requests_respect_the_frame_budget() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        requester.schedule_immediate();
        tokio::task::yield_now().await;
        requester.schedule_immediate();
        tokio::task::yield_now().await;
        assert_eq!(drain(&mut rx), 1);
        tokio::time::advance(MIN_FRAME_INTERVAL * 2).await;
        tokio::task::yield_now().await;
        assert_eq!(drain(&mut rx), 1, "deferred keystroke frame never fired");
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_flood_cannot_starve_terminal_coalesced_repaint() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        // Model/terminal state needs one final repaint.
        requester.schedule_frame();
        // Input flood is much larger than the bounded immediate queue.
        for _ in 0..10_000 {
            requester.schedule_immediate();
        }
        tokio::task::yield_now().await;
        tokio::time::advance(MIN_FRAME_INTERVAL * 8).await;
        tokio::task::yield_now().await;
        assert!(
            drain(&mut rx) >= 1,
            "terminal repaint was starved by input flood"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ren002_immediate_input_preempts_continuously_ready_model_ingress() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        let model_requester = requester.clone();
        let model_flood = tokio::spawn(async move {
            loop {
                // Keep model ingress ready without making its repaint due;
                // any draw observed before the deadline must be input-driven.
                model_requester.schedule_frame_in(Duration::from_secs(10));
                tokio::task::yield_now().await;
            }
        });

        tokio::task::yield_now().await;
        requester.schedule_immediate();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        assert!(
            rx.try_recv().is_ok(),
            "continuous model invalidation must not delay cancellation/input repaint"
        );
        model_flood.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn ren002_simultaneous_flood_preserves_durable_settlement_repaint() {
        let (tx, mut rx) = channel();
        let requester = spawn(tx);
        let model_requester = requester.clone();
        let model_flood = tokio::spawn(async move {
            loop {
                model_requester.schedule_frame();
                tokio::task::yield_now().await;
            }
        });

        requester.schedule_frame();
        for _ in 0..10_000 {
            requester.schedule_immediate();
        }
        tokio::task::yield_now().await;
        tokio::time::advance(MIN_FRAME_INTERVAL * 4).await;
        tokio::task::yield_now().await;

        assert!(
            rx.try_recv().is_ok(),
            "durable settlement/final repaint must survive simultaneous model and input flood"
        );
        model_flood.abort();
    }
}
