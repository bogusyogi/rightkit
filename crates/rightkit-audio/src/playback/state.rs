//! State shared between the control side (caller threads) and the output callback.
//!
//! Every field the callback touches is an atomic or the lock-free ring. There is no mutex,
//! no channel and no heap allocation on the callback side.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::time::Duration;

use super::ring::SampleRing;

pub(crate) const FAULT_NONE: u8 = 0;
/// A stream error that does not need a reopen (underrun reported by the host, route change
/// the host handled itself, permission, and so on). Counted, not acted on.
pub(crate) const FAULT_ADVISORY: u8 = 1;
/// The device is gone or the stream was invalidated: reopen the default device.
pub(crate) const FAULT_REOPEN: u8 = 2;

/// Upper bound on the reported device latency. Hosts occasionally report nonsense; the clock
/// must not be wrecked by it.
const MAX_LATENCY: Duration = Duration::from_secs(2);

pub(crate) struct Shared {
    pub(crate) ring: SampleRing,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,

    paused: AtomicBool,
    muted: AtomicBool,
    volume_bits: AtomicU32,

    /// Bumped by `flush`. The callback discards up to `flush_target` when it changes.
    flush_gen: AtomicU64,
    flush_target: AtomicU64,
    /// Ring index (in samples) that counts as clock zero. `flush` sets it to the write index.
    clock_base: AtomicU64,
    /// Device latency from the last callback, in interleaved samples.
    latency_samples: AtomicU64,

    underruns: AtomicU64,
    underrun_samples: AtomicU64,
    overruns: AtomicU64,
    overrun_samples: AtomicU64,
    stream_errors: AtomicU64,
    recoveries: AtomicU64,
    fault: AtomicU8,
}

/// Snapshot of the playback counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputStats {
    /// Callbacks that had to substitute silence because the ring ran dry (not counted while
    /// paused).
    pub underruns: u64,
    /// Total samples of silence substituted on underrun.
    pub underrun_samples: u64,
    /// `push_samples` calls that could not accept everything.
    pub overruns: u64,
    /// Total samples refused on overrun.
    pub overrun_samples: u64,
    /// Stream errors reported by the host.
    pub stream_errors: u64,
    /// Successful reopens of the default device after a device loss.
    pub recoveries: u64,
    /// Samples queued right now.
    pub queued_samples: usize,
    /// A device loss has been seen and the default device has not been reopened yet.
    pub needs_recovery: bool,
}

impl Shared {
    pub(crate) fn new(sample_rate: u32, channels: u16, capacity_frames: usize) -> Self {
        let channels = channels.max(1);
        Self {
            ring: SampleRing::new(capacity_frames.max(1) * channels as usize),
            sample_rate,
            channels,
            paused: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            volume_bits: AtomicU32::new(1.0f32.to_bits()),
            flush_gen: AtomicU64::new(0),
            flush_target: AtomicU64::new(0),
            clock_base: AtomicU64::new(0),
            latency_samples: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            underrun_samples: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            overrun_samples: AtomicU64::new(0),
            stream_errors: AtomicU64::new(0),
            recoveries: AtomicU64::new(0),
            fault: AtomicU8::new(FAULT_NONE),
        }
    }

    // ---- control side -------------------------------------------------------------------

    /// Producer. Counts an overrun when anything was refused.
    pub(crate) fn push(&self, samples: &[f32]) -> usize {
        let accepted = self.ring.push(samples, self.channels as usize);
        if accepted < samples.len() {
            self.overruns.fetch_add(1, Ordering::Relaxed);
            self.overrun_samples
                .fetch_add((samples.len() - accepted) as u64, Ordering::Relaxed);
        }
        accepted
    }

    /// Drop everything queued so far and reset the clock to zero. Safe from any thread: it
    /// only reads the write index, so samples pushed after it are kept.
    pub(crate) fn flush(&self) {
        let target = self.ring.tail();
        self.flush_target.store(target, Ordering::Relaxed);
        // The clock reads zero immediately; the callback discards the queue on its next run.
        self.clock_base.store(target, Ordering::Relaxed);
        self.flush_gen.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub(crate) fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        self.volume_bits.store(volume.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    /// Samples (interleaved) the device has taken from the queue since the last flush, before
    /// latency adjustment. Reaches the pushed count once the queue has drained.
    pub(crate) fn consumed_samples(&self) -> u64 {
        self.ring
            .head()
            .saturating_sub(self.clock_base.load(Ordering::Relaxed))
    }

    /// Samples (interleaved) that have been played and are audible: consumed since the last
    /// flush, minus what the device still holds.
    pub(crate) fn played_samples(&self) -> u64 {
        self.consumed_samples()
            .saturating_sub(self.latency_samples.load(Ordering::Relaxed))
    }

    pub(crate) fn position(&self) -> Duration {
        let frames = self.played_samples() / self.channels as u64;
        let nanos = frames as u128 * 1_000_000_000 / self.sample_rate.max(1) as u128;
        Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
    }

    pub(crate) fn latency(&self) -> Duration {
        let frames = self.latency_samples.load(Ordering::Relaxed) / self.channels as u64;
        let nanos = frames as u128 * 1_000_000_000 / self.sample_rate.max(1) as u128;
        Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
    }

    pub(crate) fn fault(&self) -> u8 {
        self.fault.load(Ordering::Relaxed)
    }

    pub(crate) fn clear_fault(&self) {
        self.fault.store(FAULT_NONE, Ordering::Relaxed);
    }

    pub(crate) fn note_recovery(&self) {
        self.recoveries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn stats(&self) -> OutputStats {
        OutputStats {
            underruns: self.underruns.load(Ordering::Relaxed),
            underrun_samples: self.underrun_samples.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            overrun_samples: self.overrun_samples.load(Ordering::Relaxed),
            stream_errors: self.stream_errors.load(Ordering::Relaxed),
            recoveries: self.recoveries.load(Ordering::Relaxed),
            queued_samples: self.ring.len(),
            needs_recovery: self.fault() == FAULT_REOPEN,
        }
    }

    // ---- callback side (atomics only) ---------------------------------------------------

    pub(crate) fn current_flush_gen(&self) -> u64 {
        self.flush_gen.load(Ordering::Acquire)
    }

    /// Consumer. Apply a pending flush, if any, by discarding up to its snapshot.
    pub(crate) fn process_flush(&self, seen: &mut u64) {
        let generation = self.flush_gen.load(Ordering::Acquire);
        if generation != *seen {
            *seen = generation;
            self.ring
                .discard_to(self.flush_target.load(Ordering::Relaxed));
        }
    }

    /// Gain the callback should be at: 0 when muted, otherwise the volume.
    pub(crate) fn target_gain(&self) -> f32 {
        if self.is_muted() {
            0.0
        } else {
            self.volume()
        }
    }

    pub(crate) fn set_latency(&self, latency: Duration) {
        let latency = latency.min(MAX_LATENCY);
        let frames = latency.as_nanos() * self.sample_rate as u128 / 1_000_000_000;
        self.latency_samples
            .store(frames as u64 * self.channels as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_underrun(&self, silent_samples: usize) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
        self.underrun_samples
            .fetch_add(silent_samples as u64, Ordering::Relaxed);
    }

    /// Error callback. `reopen` marks a lost device; anything else is advisory.
    pub(crate) fn record_stream_error(&self, reopen: bool) {
        self.stream_errors.fetch_add(1, Ordering::Relaxed);
        let level = if reopen { FAULT_REOPEN } else { FAULT_ADVISORY };
        self.fault.fetch_max(level, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_resets_the_clock_immediately_and_drops_the_queue() {
        let shared = Shared::new(48_000, 2, 1_000);
        assert_eq!(shared.push(&vec![0.1; 40]), 40);

        // Simulated callback consumes 20 samples.
        let mut out = [0.0f32; 20];
        assert_eq!(shared.ring.pop_into(&mut out), 20);
        assert_eq!(shared.played_samples(), 20);

        shared.flush();
        // Clock is zero before the callback has even run.
        assert_eq!(shared.played_samples(), 0);
        assert_eq!(shared.position(), Duration::ZERO);

        // Callback applies the flush: the 20 remaining pre-flush samples are dropped.
        let mut seen = 0;
        shared.process_flush(&mut seen);
        assert_eq!(shared.ring.len(), 0);
        assert_eq!(shared.played_samples(), 0);

        // Post-flush data counts from zero.
        assert_eq!(shared.push(&vec![0.2; 10]), 10);
        let mut out = [0.0f32; 10];
        assert_eq!(shared.ring.pop_into(&mut out), 10);
        assert_eq!(shared.played_samples(), 10);
    }

    #[test]
    fn samples_pushed_after_flush_survive_a_late_callback() {
        let shared = Shared::new(48_000, 1, 100);
        shared.push(&[1.0; 10]);
        shared.flush();
        // Pushed before the callback has processed the flush.
        shared.push(&[2.0; 5]);
        let mut seen = 0;
        shared.process_flush(&mut seen);
        let mut out = [0.0f32; 8];
        assert_eq!(shared.ring.pop_into(&mut out), 5);
        assert_eq!(&out[..5], &[2.0; 5]);
    }

    #[test]
    fn position_is_latency_adjusted() {
        let shared = Shared::new(1_000, 2, 10_000);
        shared.push(&vec![0.0; 4_000]);
        let mut out = vec![0.0f32; 4_000];
        shared.ring.pop_into(&mut out);
        // 2000 frames at 1 kHz = 2 s handed over.
        assert_eq!(shared.position(), Duration::from_secs(2));
        shared.set_latency(Duration::from_millis(500));
        assert_eq!(shared.latency(), Duration::from_millis(500));
        assert_eq!(shared.position(), Duration::from_millis(1_500));
    }

    #[test]
    fn consumed_reaches_pushed_count_while_played_stays_behind_latency() {
        let shared = Shared::new(1_000, 1, 1_000);
        shared.push(&[0.5; 100]);
        shared.set_latency(Duration::from_millis(20));
        let mut out = [0.0f32; 100];
        assert_eq!(shared.ring.pop_into(&mut out), 100);
        // The queue is empty, so every pushed sample has been taken by the device.
        assert_eq!(shared.consumed_samples(), 100);
        // Played still excludes the 20 samples the device holds.
        assert_eq!(shared.played_samples(), 80);
    }

    #[test]
    fn overrun_is_counted_with_dropped_samples() {
        let shared = Shared::new(48_000, 2, 4); // 8 samples
        assert_eq!(shared.push(&[0.0; 12]), 8);
        let stats = shared.stats();
        assert_eq!(stats.overruns, 1);
        assert_eq!(stats.overrun_samples, 4);
        assert_eq!(stats.queued_samples, 8);
    }

    #[test]
    fn device_loss_sets_reopen_and_advisory_does_not_downgrade_it() {
        let shared = Shared::new(48_000, 2, 4);
        shared.record_stream_error(false);
        assert_eq!(shared.fault(), FAULT_ADVISORY);
        shared.record_stream_error(true);
        assert_eq!(shared.fault(), FAULT_REOPEN);
        shared.record_stream_error(false);
        assert_eq!(shared.fault(), FAULT_REOPEN);
        assert_eq!(shared.stats().stream_errors, 3);
    }
}
