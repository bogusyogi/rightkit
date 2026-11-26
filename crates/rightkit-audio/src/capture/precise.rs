//! Opt-in timestamped capture. Producer admission, cutoff publication & closure
//! share one epoch-fenced CAS word. Validation uses CPAL-reported timestamps;
//! CPAL 0.18.2 does not expose WASAPI TIMESTAMP_ERROR. ALSA fallback timestamps
//! can include scheduling jitter; discontinuity is explicit failure, never proof
//! of exact hardware visibility or an instruction to repair timestamps.
use super::{anyhow, CaptureError, Result};
use cpal::traits::StreamTrait;
use cpal::{Stream, StreamInstant};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Instant;

const CLOSED: u64 = 1;
const IN_FLIGHT: u64 = 2;
const PUBLISHING: u64 = 4;
const REQUESTED: u64 = 8;
const WATERMARK: u64 = 16;
const SHIFT: u32 = 5;

#[derive(Default)]
pub(super) struct ProducerGate {
    word: AtomicU64,
    cutoff_ns: AtomicU64,
    // A timed-out producer cannot be recycled: its storage remains owned until
    // stream destruction joins its callback, even if callback later completes.
    failed: AtomicBool,
    pub metadata_overflow: AtomicBool,
}
impl ProducerGate {
    pub fn enter(&self) -> Option<Admission<'_>> {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if word & (CLOSED | IN_FLIGHT) != 0 {
                return None;
            }
            match self.word.compare_exchange_weak(
                word,
                word | IN_FLIGHT,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Admission {
                        gate: self,
                        epoch: word >> SHIFT,
                    })
                }
                Err(actual) => word = actual,
            }
        }
    }
    pub fn check_epoch(&self, epoch: u64) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            return Err(anyhow!("precise capture timed out; reopen session"));
        }
        if self.current_epoch() != epoch {
            return Err(anyhow!("precise capture wrong epoch"));
        }
        Ok(())
    }
    pub fn request_cut(&self, epoch: u64, at: StreamInstant) -> Result<()> {
        self.check_epoch(epoch)?;
        let nanos =
            u64::try_from(at.as_nanos()).map_err(|_| anyhow!("precise capture clock overflow"))?;
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if word >> SHIFT != epoch {
                return Err(anyhow!("precise capture wrong epoch"));
            }
            if word & (CLOSED | PUBLISHING | REQUESTED) != 0 {
                return Err(anyhow!("precise capture cut already requested or closed"));
            }
            // Reserve publication in same epoch/admission CAS word. Epoch reset
            // must quiesce both IN_FLIGHT & PUBLISHING before touching cutoff.
            match self.word.compare_exchange_weak(
                word,
                word | PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => word = actual,
            }
        }
        self.cutoff_ns.store(nanos, Ordering::Relaxed);
        // Publish payload before REQUESTED. Preserve concurrent callback/close bits.
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            let next = (word & !PUBLISHING) | REQUESTED;
            match self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Ok(()),
                Err(actual) => word = actual,
            }
        }
    }
    // Called only after complete native packet + metadata have been committed.
    pub fn packet_committed(&self, epoch: u64, capture: StreamInstant) {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if word >> SHIFT != epoch || word & REQUESTED == 0 {
                return;
            }
            if capture.as_nanos() < u128::from(self.cutoff_ns.load(Ordering::Relaxed)) {
                return;
            }
            match self.word.compare_exchange_weak(
                word,
                word | CLOSED | WATERMARK,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => word = actual,
            }
        }
    }
    pub fn has_watermark(&self) -> bool {
        self.word.load(Ordering::Acquire) & WATERMARK != 0
    }
    pub fn close(&self, epoch: u64) -> Result<()> {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if word >> SHIFT != epoch {
                return Err(anyhow!("precise capture wrong epoch"));
            }
            match self.word.compare_exchange_weak(
                word,
                word | CLOSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => word = actual,
            }
        }
    }
    pub fn timeout(&self, epoch: u64) {
        self.failed.store(true, Ordering::Release);
        let _ = self.close(epoch);
    }
    pub fn current_epoch(&self) -> u64 {
        self.word.load(Ordering::Acquire) >> SHIFT
    }
    pub fn wait_closed_until(&self, deadline: Instant) -> Result<()> {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if word & CLOSED == 0 {
                return Err(anyhow!("precise capture is not closed"));
            }
            if word & (IN_FLIGHT | PUBLISHING) == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("precise capture quiesce timeout"));
            }
            std::thread::yield_now();
        }
    }
    pub fn open_next(&self) -> Result<u64> {
        self.check_epoch(self.current_epoch())?;
        let word = self.word.load(Ordering::Acquire);
        if word & CLOSED == 0 || word & (IN_FLIGHT | PUBLISHING) != 0 {
            return Err(anyhow!("precise capture producer is not quiescent"));
        }
        let epoch = (word >> SHIFT)
            .checked_add(1)
            .filter(|n| *n <= u64::MAX >> SHIFT)
            .ok_or_else(|| anyhow!("precise capture epoch exhausted"))?;
        self.metadata_overflow.store(false, Ordering::Release);
        self.cutoff_ns.store(0, Ordering::Relaxed);
        self.word.store(epoch << SHIFT, Ordering::Release);
        Ok(epoch)
    }
}
pub(super) struct Admission<'a> {
    gate: &'a ProducerGate,
    pub epoch: u64,
}
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.gate.word.fetch_and(!IN_FLIGHT, Ordering::Release);
    }
}

/// Versioned same-machine host clock marker, sampled before asynchronous IPC.
/// Unsupported stream-relative clock domains must use explicit legacy handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CaptureClock {
    WasapiQpc100nsV1,
    CoreAudioHostNsV1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaptureEdge {
    pub version: u8,
    pub clock: CaptureClock,
    pub seconds: u64,
    pub nanos: u32,
}
impl CaptureEdge {
    fn instant(self) -> Result<StreamInstant> {
        if self.version != 1 {
            return Err(anyhow!("unsupported capture edge version"));
        }
        if self.nanos >= 1_000_000_000 {
            return Err(anyhow!("invalid capture edge nanos"));
        }
        if Some(self.clock) != platform_clock() {
            return Err(anyhow!("unsupported capture edge clock domain"));
        }
        Ok(StreamInstant::new(self.seconds, self.nanos))
    }
}
fn platform_clock() -> Option<CaptureClock> {
    #[cfg(target_os = "windows")]
    {
        Some(CaptureClock::WasapiQpc100nsV1)
    }
    #[cfg(target_os = "macos")]
    {
        Some(CaptureClock::CoreAudioHostNsV1)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn edge_from_nanos(clock: CaptureClock, nanos: u128) -> Result<CaptureEdge> {
    let seconds =
        u64::try_from(nanos / 1_000_000_000).map_err(|_| anyhow!("capture edge clock overflow"))?;
    Ok(CaptureEdge {
        version: 1,
        clock,
        seconds,
        nanos: (nanos % 1_000_000_000) as u32,
    })
}
/// Sample native common clock without opening a capture stream. Matches CPAL
/// 0.18.2 integer mappings exactly; no elapsed-time or per-stream alignment.
pub fn capture_edge_now() -> Result<CaptureEdge> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let mut frequency = 0i64;
        let mut counter = 0i64;
        // SAFETY: valid writable i64 pointers; these process-independent QPC APIs
        // require no initialization & retain neither pointer after returning.
        unsafe {
            QueryPerformanceFrequency(&mut frequency)
                .map_err(|e| anyhow!("capture QPC frequency failed: {e}"))?;
            QueryPerformanceCounter(&mut counter)
                .map_err(|e| anyhow!("capture QPC read failed: {e}"))?;
        }
        if frequency <= 0 || counter < 0 {
            return Err(anyhow!("invalid capture QPC clock"));
        }
        let units_100ns = counter as u128 * 10_000_000 / frequency as u128;
        edge_from_nanos(CaptureClock::WasapiQpc100nsV1, units_100ns * 100)
    }
    #[cfg(target_os = "macos")]
    {
        #[repr(C)]
        struct Timebase {
            numer: u32,
            denom: u32,
        }
        #[link(name = "System")]
        unsafe extern "C" {
            fn mach_absolute_time() -> u64;
            fn mach_timebase_info(info: *mut Timebase) -> i32;
        }
        let mut info = Timebase { numer: 0, denom: 0 };
        // SAFETY: mach_absolute_time has no preconditions; timebase pointer is
        // valid & matches mach_timebase_info_data_t's two uint32_t fields.
        let (ticks, status) = unsafe { (mach_absolute_time(), mach_timebase_info(&mut info)) };
        if status != 0 || info.denom == 0 || info.numer == 0 {
            return Err(anyhow!("capture mach timebase unavailable: {status}"));
        }
        let nanos = ticks as u128 * u128::from(info.numer) / u128::from(info.denom);
        edge_from_nanos(CaptureClock::CoreAudioHostNsV1, nanos)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err(anyhow!(
            "capture edge common clock unsupported; use explicit legacy capture"
        ))
    }
}

/// Exact-marker failure. Every variant rejects this marker; callers must never
/// substitute receipt time, cut_now or legacy trimming after this error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureCutError {
    SessionDropped,
    EdgeBeforeEpoch,
    /// Invalid/version/clock/future markers, stale epochs & publication failures.
    Rejected(CaptureError),
}
impl std::fmt::Display for CaptureCutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionDropped => f.write_str("precise capture session dropped"),
            Self::EdgeBeforeEpoch => f.write_str("capture edge precedes session epoch"),
            Self::Rejected(error) => std::fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for CaptureCutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rejected(error) => Some(error),
            _ => None,
        }
    }
}
impl From<CaptureError> for CaptureCutError {
    fn from(error: CaptureError) -> Self {
        Self::Rejected(error)
    }
}

/// Synchronous hotkey edge; handles never extend capture stream lifetime.
#[derive(Clone)]
pub struct CaptureCutHandle {
    pub(super) stream: Weak<Stream>,
    pub(super) gate: Arc<ProducerGate>,
    pub(super) epoch: u64,
    pub(super) start: StreamInstant,
    pub(super) cut: Arc<OnceLock<Result<CaptureCutToken>>>,
}
impl CaptureCutHandle {
    /// Local-stream release edge; unsupported cross-process domains remain local.
    pub fn cut_now(&self) -> Result<CaptureCutToken> {
        let stream = self
            .stream
            .upgrade()
            .ok_or_else(|| anyhow!("precise capture session dropped"))?;
        self.gate.check_epoch(self.epoch)?;
        self.cut
            .get_or_init(|| {
                let at = stream.now();
                if at < self.start {
                    return Err(anyhow!("capture clock precedes session epoch"));
                }
                self.gate.request_cut(self.epoch, at)?;
                Ok(CaptureCutToken {
                    epoch: self.epoch,
                    at,
                    gate: Arc::clone(&self.gate),
                })
            })
            .clone()
    }
    /// Apply same-machine release marker sampled synchronously in shell before IPC.
    pub fn cut_at(&self, edge: CaptureEdge) -> Result<CaptureCutToken> {
        let stream = self
            .stream
            .upgrade()
            .ok_or_else(|| anyhow!("precise capture session dropped"))?;
        self.gate.check_epoch(self.epoch)?;
        let at = edge.instant()?;
        if at > stream.now() {
            return Err(anyhow!("capture edge is in future"));
        }
        if at < self.start {
            return Err(anyhow!("capture edge precedes session epoch"));
        }
        self.publish_at(at)
    }
    /// Typed exact-marker path. `cut_at` retains its existing signature.
    /// Explicit markers fail closed: no error permits receipt-age fallback.
    pub fn cut_at_exact(
        &self,
        edge: CaptureEdge,
    ) -> std::result::Result<CaptureCutToken, CaptureCutError> {
        let stream = self
            .stream
            .upgrade()
            .ok_or(CaptureCutError::SessionDropped)?;
        self.gate.check_epoch(self.epoch)?;
        let at = edge.instant()?;
        if at > stream.now() {
            return Err(CaptureCutError::Rejected(anyhow!(
                "capture edge is in future"
            )));
        }
        if at < self.start {
            return Err(CaptureCutError::EdgeBeforeEpoch);
        }
        self.publish_at(at).map_err(CaptureCutError::from)
    }
    fn publish_at(&self, at: StreamInstant) -> Result<CaptureCutToken> {
        let result = self
            .cut
            .get_or_init(|| {
                self.gate.request_cut(self.epoch, at)?;
                Ok(CaptureCutToken {
                    epoch: self.epoch,
                    at,
                    gate: Arc::clone(&self.gate),
                })
            })
            .clone();
        match result {
            Ok(token) if token.at != at => Err(anyhow!("precise capture cutoff already fixed")),
            other => other,
        }
    }
}
#[derive(Clone)]
pub struct CaptureCutToken {
    pub(super) epoch: u64,
    pub(super) at: StreamInstant,
    pub(super) gate: Arc<ProducerGate>,
}
#[derive(Clone, Copy)]
pub(super) struct CallbackStamp {
    pub epoch: u64,
    pub capture: StreamInstant,
    pub callback: StreamInstant,
    pub frames: usize,
}
pub(super) struct PreciseEpoch {
    pub epoch: u64,
    pub start: StreamInstant,
    pub native: Vec<f32>,
    pub stamps: Vec<CallbackStamp>,
    pub overflow_before: u64,
    pub errors_before: u64,
    pub cached: Option<Result<Vec<f32>>>,
}
impl PreciseEpoch {
    pub fn validate_integrity(
        &self,
        metadata_overflow: bool,
        overflows: u64,
        errors: u64,
    ) -> Result<()> {
        if metadata_overflow || overflows != self.overflow_before {
            return Err(anyhow!("precise capture overflow"));
        }
        if errors != self.errors_before {
            return Err(anyhow!("precise capture stream error during epoch"));
        }
        Ok(())
    }
    pub fn selected_native(&self, cut: &CaptureCutToken, rate: u32) -> Result<Vec<f32>> {
        let mut offset = 0usize;
        let mut out = Vec::new();
        let mut previous: Option<(StreamInstant, usize)> = None;
        if self.stamps.is_empty() {
            return Err(anyhow!("precise capture missing timestamps"));
        }
        for stamp in &self.stamps {
            if stamp.epoch != self.epoch || stamp.epoch != cut.epoch {
                return Err(anyhow!("precise capture wrong timestamp epoch"));
            }
            // `callback` is not an upper bound for `capture`. On WASAPI it is the QPC value of the
            // audio clock's last position update, which trails a packet's own capture time by up
            // to one device period (Dell built-in microphone array: 247 of 799 packets). Only the
            // capture time selects samples, so only it is required.
            if stamp.frames == 0 || stamp.capture == StreamInstant::ZERO {
                return Err(anyhow!("precise capture invalid or missing timestamp"));
            }
            if let Some((prior_capture, prior_frames)) = previous {
                // WASAPI Stop can leave queued old packets across warm reuse.
                // Compare packet intervals only where their discontinuity or
                // regression intersects requested [start,cut). Old idle gaps
                // ending at/before start carry no continuity into this epoch.
                let scale = u128::from(rate);
                let prior_start = prior_capture.as_nanos() * scale;
                let prior_end = prior_start + prior_frames as u128 * 1_000_000_000;
                let packet_start = stamp.capture.as_nanos() * scale;
                let packet_end = packet_start + stamp.frames as u128 * 1_000_000_000;
                let window_start = self.start.as_nanos() * scale;
                let window_end = cut.at.as_nanos() * scale;
                let intersects = |lo: u128, hi: u128| {
                    window_start < window_end && lo < window_end && hi > window_start
                };
                if packet_start < prior_start && intersects(packet_start, prior_end.max(packet_end))
                {
                    return Err(anyhow!("precise capture timestamp regression"));
                }
                // Half of the shorter packet tolerates the jitter of reported packet
                // timestamps: a driver stamps each packet from its own clock read, and on the
                // Dell built-in microphone array consecutive 480-frame packets differ from
                // their nominal spacing by up to 17 frames with nothing lost. A lost packet
                // moves the next one by at least a whole packet, which still fails. A
                // gap/overlap wholly before start or after cut is discarded.
                let tolerance = (prior_frames.min(stamp.frames) as u128 / 2).max(1) * 1_000_000_000;
                if packet_start.abs_diff(prior_end) > tolerance
                    && intersects(packet_start.min(prior_end), packet_start.max(prior_end))
                {
                    return Err(anyhow!("precise capture timestamp discontinuity"));
                }
            }
            previous = Some((stamp.capture, stamp.frames));
            let end = offset
                .checked_add(stamp.frames)
                .filter(|end| *end <= self.native.len())
                .ok_or_else(|| anyhow!("precise capture timestamp/sample mismatch"))?;
            // Each packet is intersected with half-open [epoch_start, cut).
            // ceil(delta * rate) selects sample instants at/after start & before cut.
            let index_at = |instant: StreamInstant| {
                instant
                    .checked_duration_since(stamp.capture)
                    .map(|d| {
                        (d.as_nanos() * u128::from(rate))
                            .div_ceil(1_000_000_000)
                            .min(stamp.frames as u128) as usize
                    })
                    .unwrap_or(0)
            };
            let first = index_at(self.start);
            let last = index_at(cut.at);
            if first < last {
                out.extend_from_slice(&self.native[offset + first..offset + last]);
            }
            offset = end;
        }
        if offset != self.native.len() {
            return Err(anyhow!("precise capture missing callback metadata"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::StreamResampler;
    use super::*;
    fn stamp(epoch: u64, millis: u64, frames: usize) -> CallbackStamp {
        CallbackStamp {
            epoch,
            capture: StreamInstant::from_millis(millis),
            callback: StreamInstant::from_millis(millis + 100),
            frames,
        }
    }
    fn epoch(native: Vec<f32>, stamps: Vec<CallbackStamp>) -> PreciseEpoch {
        PreciseEpoch {
            epoch: 1,
            start: StreamInstant::ZERO,
            native,
            stamps,
            overflow_before: 0,
            errors_before: 0,
            cached: None,
        }
    }
    fn cut(gate: &Arc<ProducerGate>, millis: u64) -> CaptureCutToken {
        CaptureCutToken {
            epoch: 1,
            at: StreamInstant::from_millis(millis),
            gate: Arc::clone(gate),
        }
    }
    #[test]
    fn delayed_pre_entry_straddle_and_watermark_keep_full_native_prefix() {
        let gate = Arc::new(ProducerGate::default());
        gate.close(0).unwrap();
        gate.wait_closed_until(Instant::now()).unwrap();
        assert_eq!(gate.open_next().unwrap(), 1);
        // Hotkey occurs before delayed native packets even enter callback.
        gate.request_cut(1, StreamInstant::from_millis(30)).unwrap();
        let delayed = gate.enter().unwrap();
        gate.packet_committed(delayed.epoch, StreamInstant::from_millis(10));
        assert!(!gate.has_watermark());
        drop(delayed);
        // This packet straddles cut: retain it, but do not close yet.
        let straddling = gate.enter().unwrap();
        gate.packet_committed(straddling.epoch, StreamInstant::from_millis(20));
        assert!(!gate.has_watermark());
        drop(straddling);
        let watermark = gate.enter().unwrap();
        gate.packet_committed(watermark.epoch, StreamInstant::from_millis(40));
        assert!(gate.has_watermark());
        assert!(gate.enter().is_none());
        assert!(gate.wait_closed_until(Instant::now()).is_err());
        drop(watermark);
        gate.wait_closed_until(Instant::now()).unwrap();
        let mut native = vec![0.0; 1920];
        native[960] = 1.0; // exactly at 30ms, inside straddling packet
        let clip = epoch(
            native,
            vec![stamp(1, 10, 480), stamp(1, 20, 960), stamp(1, 40, 480)],
        );
        let token = cut(&gate, 30);
        let selected = clip.selected_native(&token, 48000).unwrap();
        assert_eq!(selected, vec![0.0; 960]);
        assert_eq!(
            StreamResampler::resample_sealed(&selected, 48000, 16000).unwrap(),
            vec![0.0; 320]
        );
        assert_eq!(gate.open_next().unwrap(), 2);
        assert!(gate.request_cut(1, token.at).is_err());
        assert!(gate.enter().is_some()); // stale epoch did not close new admission
        assert!(!gate.has_watermark());
    }
    #[test]
    fn no_watermark_timeout_closes_admission_and_forbids_reuse() {
        let gate = ProducerGate::default();
        gate.request_cut(0, StreamInstant::from_millis(30)).unwrap();
        let admitted = gate.enter().unwrap();
        gate.packet_committed(0, StreamInstant::from_millis(20));
        assert!(!gate.has_watermark());
        gate.timeout(0);
        assert!(gate.enter().is_none());
        assert!(gate.wait_closed_until(Instant::now()).is_err());
        assert!(gate.open_next().is_err());
        drop(admitted);
        gate.wait_closed_until(Instant::now()).unwrap();
        assert!(gate.open_next().is_err()); // even late completion cannot recycle timed-out epoch
    }
    #[test]
    fn start_side_old_packet_is_trimmed_to_actual_epoch_instant() {
        let gate = Arc::new(ProducerGate::default());
        let mut clip = epoch(
            (0..1440).map(|n| n as f32).collect(),
            vec![stamp(1, 10, 480), stamp(1, 20, 480), stamp(1, 30, 480)],
        );
        clip.start = StreamInstant::from_millis(15);
        let selected = clip.selected_native(&cut(&gate, 30), 48000).unwrap();
        assert_eq!(selected, (240..960).map(|n| n as f32).collect::<Vec<_>>());
        clip.start = StreamInstant::from_millis(35);
        assert!(clip
            .selected_native(&cut(&gate, 30), 48000)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn warm_reuse_discards_queued_old_packets_and_idle_gap_before_start() {
        let gate = Arc::new(ProducerGate::default());
        let mut native = vec![0.75; 480]; // queued [10,20) ms
        native.extend(vec![0.25; 480]); // fresh [1000,1010) ms
        native.extend(vec![1.0; 480]); // post-cut watermark packet
        let mut clip = epoch(
            native,
            vec![stamp(1, 10, 480), stamp(1, 1000, 480), stamp(1, 1010, 480)],
        );
        clip.start = StreamInstant::from_millis(1000);
        assert_eq!(
            clip.selected_native(&cut(&gate, 1010), 48000).unwrap(),
            vec![0.25; 480]
        );
    }
    #[test]
    fn warm_reuse_retains_start_straddle_and_rejects_gap_intersecting_start() {
        let gate = Arc::new(ProducerGate::default());
        let mut native = vec![0.75; 480];
        native.extend(vec![0.25; 720]); // [990,1005), final 240 frames belong to epoch
        native.extend(vec![0.5; 480]); // [1005,1015), first 240 frames precede cut
        let mut clip = epoch(
            native,
            vec![stamp(1, 10, 480), stamp(1, 990, 720), stamp(1, 1005, 480)],
        );
        clip.start = StreamInstant::from_millis(1000);
        let mut expected = vec![0.25; 240];
        expected.extend(vec![0.5; 240]);
        assert_eq!(
            clip.selected_native(&cut(&gate, 1010), 48000).unwrap(),
            expected
        );
        let mut gap = epoch(vec![0.0; 960], vec![stamp(1, 10, 480), stamp(1, 1001, 480)]);
        gap.start = clip.start;
        assert!(gap.selected_native(&cut(&gate, 1010), 48000).is_err());
    }
    #[test]
    fn old_timestamp_regression_is_discarded_but_relevant_regression_fails() {
        let gate = Arc::new(ProducerGate::default());
        let mut old = epoch(
            vec![0.25; 1440],
            vec![stamp(1, 20, 480), stamp(1, 10, 480), stamp(1, 1000, 480)],
        );
        old.start = StreamInstant::from_millis(1000);
        assert_eq!(
            old.selected_native(&cut(&gate, 1010), 48000).unwrap(),
            vec![0.25; 480]
        );
        let mut relevant = epoch(
            vec![0.25; 960],
            vec![stamp(1, 1000, 480), stamp(1, 999, 480)],
        );
        relevant.start = old.start;
        assert!(relevant.selected_native(&cut(&gate, 1010), 48000).is_err());
    }
    #[test]
    fn discontinuity_and_regression_fail_instead_of_splicing_packets() {
        let gate = Arc::new(ProducerGate::default());
        let token = cut(&gate, 50);
        assert!(
            epoch(vec![0.0; 960], vec![stamp(1, 10, 480), stamp(1, 26, 480)])
                .selected_native(&token, 48000)
                .is_err()
        );
        assert!(
            epoch(vec![0.0; 960], vec![stamp(1, 20, 480), stamp(1, 10, 480)])
                .selected_native(&token, 48000)
                .is_err()
        );
    }
    #[test]
    fn callback_boundaries_and_delayed_worker_do_not_move_cut() {
        let gate = Arc::new(ProducerGate::default());
        let token = cut(&gate, 30);
        // Three callbacks buffered before worker runs; impulse in post-cut chunk.
        let mut native = vec![0.0; 480 * 3];
        native[960] = 1.0;
        let whole = epoch(native.clone(), vec![stamp(1, 10, 1440)]);
        let split = epoch(
            native,
            vec![stamp(1, 10, 480), stamp(1, 20, 480), stamp(1, 30, 480)],
        );
        let selected = whole.selected_native(&token, 48000).unwrap();
        assert_eq!(selected.len(), 960);
        assert_eq!(selected, split.selected_native(&token, 48000).unwrap());
        assert!(StreamResampler::resample_sealed(&selected, 48000, 16000)
            .unwrap()
            .iter()
            .all(|v| *v == 0.0));
    }
    #[test]
    fn sealed_short_and_boundary_lengths_include_delay_compensated_tail() {
        for rate in [16000, 44100, 48000] {
            let chunk = (rate as usize / 50).max(64);
            for n in [1, chunk - 1, chunk, chunk + 1, chunk * 2] {
                let mut input = vec![0.0; n];
                input[n - 1] = 0.5;
                let output = StreamResampler::resample_sealed(&input, rate, 16000).unwrap();
                let expected = (n as u128 * 16000).div_ceil(u128::from(rate)) as usize;
                assert_eq!(output.len(), expected, "rate={rate}, n={n}");
                assert!(
                    output.iter().any(|x| x.abs() > 0.00001),
                    "tail lost: rate={rate}, n={n}"
                );
                if rate == 16000 {
                    assert_eq!(output, input);
                }
            }
        }
    }
    #[test]
    fn dropped_session_handles_fail_without_retaining_any_stream() {
        let handle = CaptureCutHandle {
            stream: Weak::new(),
            gate: Arc::new(ProducerGate::default()),
            epoch: 0,
            start: StreamInstant::ZERO,
            cut: Arc::new(OnceLock::new()),
        };
        assert!(handle.cut_now().is_err());
        assert!(handle
            .cut_at(CaptureEdge {
                version: 1,
                clock: CaptureClock::WasapiQpc100nsV1,
                seconds: 0,
                nanos: 0
            })
            .is_err());
    }
    #[test]
    fn malformed_or_unsupported_edge_schema_fails() {
        let edge = CaptureEdge {
            version: 2,
            clock: CaptureClock::WasapiQpc100nsV1,
            seconds: 1,
            nanos: 0,
        };
        assert!(edge.instant().is_err());
        assert!(CaptureEdge {
            version: 1,
            nanos: 1_000_000_000,
            ..edge
        }
        .instant()
        .is_err());
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert!(capture_edge_now().is_err());
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn common_clock_marker_is_valid_monotonic_and_round_trips() {
        let first = capture_edge_now().unwrap();
        let next = capture_edge_now().unwrap();
        assert!(first.instant().unwrap() <= next.instant().unwrap());
        assert_eq!(Some(first.clock), platform_clock());
        #[cfg(target_os = "windows")]
        assert_eq!(first.nanos % 100, 0);
        let json = serde_json::to_string(&first).unwrap();
        assert_eq!(serde_json::from_str::<CaptureEdge>(&json).unwrap(), first);
    }
    #[test]
    fn overflow_and_async_errors_never_seal_successfully() {
        let clip = epoch(vec![0.0], vec![stamp(1, 10, 1)]);
        assert!(clip.validate_integrity(false, 0, 0).is_ok());
        assert!(clip.validate_integrity(true, 0, 0).is_err());
        assert!(clip.validate_integrity(false, 1, 0).is_err());
        assert!(clip.validate_integrity(false, 0, 1).is_err());
    }
    #[test]
    fn timestamp_errors_and_next_epoch_are_fail_closed() {
        let gate = Arc::new(ProducerGate::default());
        let token = cut(&gate, 30);
        assert!(epoch(vec![0.0], vec![])
            .selected_native(&token, 48000)
            .is_err());
        assert!(epoch(vec![0.0], vec![stamp(2, 10, 1)])
            .selected_native(&token, 48000)
            .is_err());
        assert!(epoch(vec![0.0], vec![stamp(1, 0, 1)])
            .selected_native(&token, 48000)
            .is_err());
        assert!(epoch(vec![0.0], vec![stamp(1, 10, 2)])
            .selected_native(&token, 48000)
            .is_err());
        assert!(epoch(vec![0.0; 2], vec![stamp(1, 10, 1)])
            .selected_native(&token, 48000)
            .is_err());
        let mut next = epoch(vec![0.25; 16], vec![stamp(2, 10, 16)]);
        next.epoch = 2;
        assert!(next.selected_native(&token, 48000).is_err());
        let next_token = CaptureCutToken { epoch: 2, ..token };
        assert_eq!(
            next.selected_native(&next_token, 48000).unwrap(),
            vec![0.25; 16]
        );
    }
}
