//! Caller control over a Whisper run: cancellation, progress, deadlines and the
//! typed interruption result.
//!
//! Everything here is plain Rust apart from [`AbortState`], which is the one
//! object whose address is handed to whisper.cpp as callback user data. Its
//! lifetime rules are documented on the type and exercised without a model by
//! the tests at the bottom of this file.

use super::WhisperTranscript;
use std::ffi::c_void;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cooperative cancellation flag the caller can trip from any thread. Clones
/// share one flag. Checked inside whisper.cpp's abort callback (so an in-flight
/// window stops promptly) and between windows.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap an existing shared flag (for callers that already own an `Arc<AtomicBool>`).
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// Trip the flag. Idempotent.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// The shared flag, for callers that want to keep using it as an `AtomicBool`.
    pub fn flag(&self) -> &Arc<AtomicBool> {
        &self.0
    }

    fn raw(&self) -> *const AtomicBool {
        Arc::as_ptr(&self.0)
    }
}

impl From<Arc<AtomicBool>> for CancelToken {
    fn from(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }
}

/// Why a run stopped before finishing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptReason {
    /// The caller's [`CancelToken`] was tripped.
    Cancelled,
    /// A deadline passed: `window_deadline`, `total_deadline` or `detect_deadline`.
    DeadlineExceeded,
}

/// Payload of [`crate::Error::Interrupted`]: the reason plus everything that
/// had finished when the run stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperInterrupted {
    pub reason: InterruptReason,
    /// Windows that completed before the stop, assembled exactly like a full
    /// transcript. The window that was running when the stop hit is not included.
    /// `language` is empty when no window finished and none was forced.
    pub partial: WhisperTranscript,
    /// Number of windows that completed (the partial covers these).
    pub windows_completed: usize,
    /// Windows the audio needs at the nominal 30 s (an estimate: cut points move).
    pub windows_estimated: usize,
}

impl WhisperInterrupted {
    pub(crate) fn empty(reason: InterruptReason) -> Self {
        Self {
            reason,
            partial: WhisperTranscript {
                text: String::new(),
                raw_text: String::new(),
                language: String::new(),
                detection: None,
                segments: Vec::new(),
                words: Vec::new(),
            },
            windows_completed: 0,
            windows_estimated: 0,
        }
    }
}

/// Progress of a run. `fraction` is the share of the audio's samples decoded,
/// in `[0, 1]`, and never decreases within a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WhisperProgress {
    /// Windows fully decoded so far.
    pub windows_completed: usize,
    /// Windows needed at the nominal 30 s; windows cut early make the true
    /// count larger. Equals `windows_completed` once the last window is done.
    pub windows_estimated: usize,
    /// Samples decoded so far. Inside a window this includes whisper.cpp's own
    /// within-window progress.
    pub samples_completed: usize,
    pub samples_total: usize,
    /// `samples_completed / samples_total`.
    pub fraction: f32,
}

/// Caller progress callback. Called on the thread that runs the decode, only
/// while a run is in progress, and never after the run returns.
#[derive(Clone)]
pub struct ProgressCallback(Arc<dyn Fn(&WhisperProgress) + Send + Sync>);

impl ProgressCallback {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&WhisperProgress) + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    pub(crate) fn call(&self, p: &WhisperProgress) {
        (self.0)(p)
    }
}

impl fmt::Debug for ProgressCallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProgressCallback(..)")
    }
}

/// Polled progress for a run, for callers that cannot or do not want a callback.
///
/// Create one, store a clone in [`WhisperOptions::progress_probe`], and read it from
/// any thread while the run is in progress. The decode loop updates it with atomics,
/// so the caller's code needs no `Send`, `Sync` or `'static` bound. Clones share one
/// set of counters, and each run resets them when it starts.
///
/// Each getter reads one atomic, so values read in a row may come from different
/// moments. `fraction` never decreases within a run, as with [`WhisperProgress::fraction`].
///
/// [`WhisperOptions::progress_probe`]: super::WhisperOptions::progress_probe
#[derive(Debug, Clone, Default)]
pub struct ProgressProbe(Arc<ProgressCounters>);

#[derive(Debug, Default)]
struct ProgressCounters {
    windows_completed: AtomicUsize,
    windows_estimated: AtomicUsize,
    samples_completed: AtomicUsize,
    samples_total: AtomicUsize,
    /// `f32` bits of the fraction; non-negative floats order like their bits, so `fetch_max` keeps the largest.
    fraction_bits: AtomicU32,
}

impl ProgressProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Windows fully decoded so far in the current or most recent run.
    pub fn completed_windows(&self) -> usize {
        self.0.windows_completed.load(Ordering::SeqCst)
    }

    /// Windows needed at the nominal 30 s (see [`WhisperProgress::windows_estimated`]).
    pub fn total_windows(&self) -> usize {
        self.0.windows_estimated.load(Ordering::SeqCst)
    }

    /// Samples decoded so far.
    pub fn samples_completed(&self) -> usize {
        self.0.samples_completed.load(Ordering::SeqCst)
    }

    /// Samples in the audio of the current or most recent run.
    pub fn samples_total(&self) -> usize {
        self.0.samples_total.load(Ordering::SeqCst)
    }

    /// Share of the audio decoded, in `[0, 1]`.
    pub fn fraction(&self) -> f32 {
        f32::from_bits(self.0.fraction_bits.load(Ordering::SeqCst))
    }

    /// Reset for a run that will decode `samples_total` samples in about `windows_estimated` windows.
    pub(crate) fn begin(&self, windows_estimated: usize, samples_total: usize) {
        let c = &self.0;
        c.windows_completed.store(0, Ordering::SeqCst);
        c.windows_estimated.store(windows_estimated, Ordering::SeqCst);
        c.samples_completed.store(0, Ordering::SeqCst);
        c.samples_total.store(samples_total, Ordering::SeqCst);
        c.fraction_bits.store(0f32.to_bits(), Ordering::SeqCst);
    }

    /// Record one progress event from the decode loop.
    pub(crate) fn publish(&self, p: &WhisperProgress) {
        let c = &self.0;
        c.windows_completed.store(p.windows_completed, Ordering::SeqCst);
        c.windows_estimated.store(p.windows_estimated, Ordering::SeqCst);
        c.samples_completed.store(p.samples_completed, Ordering::SeqCst);
        c.samples_total.store(p.samples_total, Ordering::SeqCst);
        c.fraction_bits.fetch_max(p.fraction.to_bits(), Ordering::SeqCst);
    }
}

/// Fixed part of the whole-recording bound the pre-0.2 route used: 120 s.
pub const RECORDING_DEADLINE_BASE: Duration = Duration::from_secs(120);
/// Per-audio-second part of that bound: 4x the audio duration.
pub const RECORDING_DEADLINE_AUDIO_FACTOR: u32 = 4;

/// The whole-recording bound the spawned-CLI route used: 120 s plus 4x the
/// audio duration. Pass the result as [`WhisperOptions::total_deadline`].
/// A policy helper only; the crate default stays "no whole-recording bound".
///
/// [`WhisperOptions::total_deadline`]: super::WhisperOptions::total_deadline
pub fn recording_deadline(audio: Duration) -> Duration {
    RECORDING_DEADLINE_BASE.saturating_add(audio.saturating_mul(RECORDING_DEADLINE_AUDIO_FACTOR))
}

/// Duration of `samples` mono samples at 16 kHz (exact: 62.5 us per sample).
pub fn audio_duration(samples: usize) -> Duration {
    let rate = super::WHISPER_SAMPLE_RATE as u64;
    let n = samples as u64;
    Duration::new(n / rate, ((n % rate) * (1_000_000_000 / rate)) as u32)
}

/// `start + d`, or `None` when there is no bound or the sum is not representable.
pub(crate) fn deadline_after(start: Instant, d: Option<Duration>) -> Option<Instant> {
    d.and_then(|d| start.checked_add(d))
}

/// The earlier of two optional deadlines (`None` = unbounded).
pub(crate) fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Plain (non-FFI) check used between windows and around uninterruptible native calls.
pub(crate) fn check_interrupt(cancel: Option<&CancelToken>, deadline: Option<Instant>) -> Option<InterruptReason> {
    if cancel.is_some_and(CancelToken::is_cancelled) {
        return Some(InterruptReason::Cancelled);
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Some(InterruptReason::DeadlineExceeded);
    }
    None
}

/// Placeholder error a decoder returns; the windowing layer fills in the partial result.
pub(crate) fn interrupted_error(reason: InterruptReason) -> crate::Error {
    crate::Error::Interrupted(Box::new(WhisperInterrupted::empty(reason)))
}

const NO_DEADLINE: u64 = u64::MAX;
const REASON_NONE: u8 = 0;
const REASON_CANCELLED: u8 = 1;
const REASON_DEADLINE: u8 = 2;

/// One within-window progress sink: receives whisper.cpp's own progress as a
/// fraction of the current window in `[0, 1]`.
pub(crate) struct ProgressSlot<'a> {
    pub f: &'a (dyn Fn(f32) + Sync),
}

/// State behind whisper.cpp's abort and progress callbacks.
///
/// Lifetime contract (the reason this type exists):
/// - The address handed to whisper.cpp as user data is the address of an
///   `Arc<AbortState>` owned by the `WhisperCpp` context. It stays valid for the
///   whole life of the context, so no call, including ggml backends that keep the
///   last-set abort pointer after `whisper_full` returns, can ever see freed state.
/// - The per-call borrowed data (the caller's cancel flag, the progress closure)
///   is not reached through that address directly: it is published through raw
///   pointers that [`AbortState::arm`] sets and the returned [`ArmGuard`] clears
///   when it drops, before the borrowed data can go away. Outside an armed
///   window both pointers are null, so a stale callback reads nothing freed.
/// - Everything is atomics, so the callbacks (which ggml may call from several
///   worker threads) need no lock.
pub(crate) struct AbortState {
    origin: Instant,
    deadline_ns: AtomicU64,
    cancel: AtomicPtr<AtomicBool>,
    progress: AtomicPtr<c_void>,
    reason: AtomicU8,
}

/// Disarms the state when dropped (also on unwind).
pub(crate) struct ArmGuard<'a> {
    state: &'a AbortState,
}

impl Drop for ArmGuard<'_> {
    fn drop(&mut self) {
        self.state.cancel.store(std::ptr::null_mut(), Ordering::SeqCst);
        self.state.progress.store(std::ptr::null_mut(), Ordering::SeqCst);
        self.state.deadline_ns.store(NO_DEADLINE, Ordering::SeqCst);
    }
}

impl AbortState {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
            deadline_ns: AtomicU64::new(NO_DEADLINE),
            cancel: AtomicPtr::new(std::ptr::null_mut()),
            progress: AtomicPtr::new(std::ptr::null_mut()),
            reason: AtomicU8::new(REASON_NONE),
        }
    }

    fn ns_since_origin(&self, t: Instant) -> u64 {
        u64::try_from(t.saturating_duration_since(self.origin).as_nanos())
            .unwrap_or(NO_DEADLINE - 1)
            .min(NO_DEADLINE - 1)
    }

    /// Publish this call's cancel flag, deadline and progress sink. The guard
    /// borrows all three for `'a`, so none can be dropped while armed. Clears
    /// the previous stop reason.
    pub fn arm<'a>(
        &'a self,
        cancel: Option<&'a CancelToken>,
        deadline: Option<Instant>,
        progress: Option<&'a ProgressSlot<'a>>,
    ) -> ArmGuard<'a> {
        self.reason.store(REASON_NONE, Ordering::SeqCst);
        self.deadline_ns
            .store(deadline.map_or(NO_DEADLINE, |d| self.ns_since_origin(d)), Ordering::SeqCst);
        self.cancel.store(
            cancel.map_or(std::ptr::null_mut(), |c| c.raw() as *mut AtomicBool),
            Ordering::SeqCst,
        );
        self.progress.store(
            progress.map_or(std::ptr::null_mut(), |s| s as *const ProgressSlot<'a> as *mut c_void),
            Ordering::SeqCst,
        );
        ArmGuard { state: self }
    }

    /// Should whisper.cpp stop? Sticky once true (until the next `arm`).
    pub fn poll(&self) -> bool {
        if self.reason.load(Ordering::SeqCst) != REASON_NONE {
            return true;
        }
        let c = self.cancel.load(Ordering::SeqCst);
        // SAFETY: non-null only between `arm` and the guard's drop, during which
        // the borrowed `CancelToken` (and so its `Arc` allocation) is alive.
        if !c.is_null() && unsafe { &*c }.load(Ordering::SeqCst) {
            self.set_reason(REASON_CANCELLED);
            return true;
        }
        let d = self.deadline_ns.load(Ordering::SeqCst);
        if d != NO_DEADLINE && self.ns_since_origin(Instant::now()) >= d {
            self.set_reason(REASON_DEADLINE);
            return true;
        }
        false
    }

    fn set_reason(&self, r: u8) {
        let _ = self.reason.compare_exchange(REASON_NONE, r, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// What stopped the last armed call, if anything.
    pub fn reason(&self) -> Option<InterruptReason> {
        match self.reason.load(Ordering::SeqCst) {
            REASON_CANCELLED => Some(InterruptReason::Cancelled),
            REASON_DEADLINE => Some(InterruptReason::DeadlineExceeded),
            _ => None,
        }
    }

    /// Forward whisper.cpp's within-window percent (0..=100) to the armed sink.
    /// A no-op when nothing is armed. Panics from the sink are the caller's
    /// problem to contain (the FFI trampoline wraps this in `catch_unwind`).
    pub fn report(&self, percent: i32) {
        let p = self.progress.load(Ordering::SeqCst);
        if p.is_null() {
            return;
        }
        // SAFETY: non-null only while armed; the slot lives on the arming
        // frame's stack until the guard drops.
        let slot = unsafe { &*(p as *const ProgressSlot<'_>) };
        (slot.f)(percent.clamp(0, 100) as f32 / 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn cancel_token_clones_share_one_flag() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
        let shared = Arc::new(AtomicBool::new(false));
        let c = CancelToken::from(Arc::clone(&shared));
        shared.store(true, Ordering::SeqCst);
        assert!(c.is_cancelled());
    }

    #[test]
    fn recording_deadline_is_120s_plus_4x_audio() {
        assert_eq!(recording_deadline(Duration::ZERO), Duration::from_secs(120));
        assert_eq!(recording_deadline(Duration::from_secs(30)), Duration::from_secs(240));
        assert_eq!(recording_deadline(Duration::from_millis(2_500)), Duration::from_secs(130));
        assert_eq!(recording_deadline(Duration::MAX), Duration::MAX, "saturates, never panics");
    }

    #[test]
    fn audio_duration_is_exact_at_16k() {
        assert_eq!(audio_duration(0), Duration::ZERO);
        assert_eq!(audio_duration(16_000), Duration::from_secs(1));
        assert_eq!(audio_duration(8_000), Duration::from_millis(500));
        assert_eq!(audio_duration(16_000 * 90 + 160), Duration::new(90, 10_000_000));
    }

    #[test]
    fn earliest_and_deadline_after() {
        let now = Instant::now();
        let later = now + Duration::from_secs(5);
        assert_eq!(earliest(None, None), None);
        assert_eq!(earliest(Some(later), None), Some(later));
        assert_eq!(earliest(None, Some(now)), Some(now));
        assert_eq!(earliest(Some(later), Some(now)), Some(now));
        assert_eq!(deadline_after(now, None), None);
        assert_eq!(deadline_after(now, Some(Duration::from_secs(5))), Some(later));
        assert_eq!(deadline_after(now, Some(Duration::MAX)), None, "overflow means unbounded");
    }

    #[test]
    fn progress_probe_is_send_sync_and_clones_share_counters() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<ProgressProbe>();
        let a = ProgressProbe::new();
        let b = a.clone();
        assert_eq!(b.completed_windows(), 0);
        assert_eq!(b.fraction(), 0.0);
        a.begin(3, 480_000);
        assert_eq!(b.total_windows(), 3);
        assert_eq!(b.samples_total(), 480_000);
        let from_thread = std::thread::spawn(move || b.total_windows()).join().unwrap();
        assert_eq!(from_thread, 3, "readable from another thread");
    }

    #[test]
    fn progress_probe_fraction_is_monotonic_and_begin_resets() {
        let probe = ProgressProbe::new();
        probe.begin(3, 100);
        probe.publish(&WhisperProgress {
            windows_completed: 1,
            windows_estimated: 3,
            samples_completed: 40,
            samples_total: 100,
            fraction: 0.4,
        });
        probe.publish(&WhisperProgress {
            windows_completed: 1,
            windows_estimated: 3,
            samples_completed: 30,
            samples_total: 100,
            fraction: 0.3,
        });
        assert_eq!(probe.completed_windows(), 1);
        assert_eq!(probe.samples_completed(), 30, "counts are the latest event");
        assert_eq!(probe.fraction(), 0.4, "fraction never goes back within a run");
        probe.begin(1, 10);
        assert_eq!(probe.completed_windows(), 0);
        assert_eq!(probe.fraction(), 0.0, "a new run starts from zero");
    }

    #[test]
    fn abort_state_trips_on_cancel_mid_call_and_disarms() {
        let state = AbortState::new();
        let token = CancelToken::new();
        {
            let _guard = state.arm(Some(&token), None, None);
            assert!(!state.poll());
            assert_eq!(state.reason(), None);
            token.cancel(); // tripped while "whisper_full" is running
            assert!(state.poll());
            assert_eq!(state.reason(), Some(InterruptReason::Cancelled));
            assert!(state.poll(), "sticky");
        }
        // Disarmed: the token pointer is gone, so a stale poll reads nothing and does not trip.
        assert!(state.poll(), "reason stays until the next arm (read by the caller after return)");
        let _guard = state.arm(None, None, None);
        assert!(!state.poll(), "re-arming clears the reason and the old token is not consulted");
    }

    #[test]
    fn abort_state_trips_on_deadline() {
        let state = AbortState::new();
        let _g = state.arm(None, Some(Instant::now() + Duration::from_secs(3_600)), None);
        assert!(!state.poll());
        drop(_g);
        let _g = state.arm(None, Some(Instant::now()), None);
        std::thread::sleep(Duration::from_millis(2));
        assert!(state.poll());
        assert_eq!(state.reason(), Some(InterruptReason::DeadlineExceeded));
    }

    #[test]
    fn cancel_wins_the_reason_when_both_hold() {
        let state = AbortState::new();
        let token = CancelToken::new();
        token.cancel();
        let _g = state.arm(Some(&token), Some(Instant::now()), None);
        assert!(state.poll());
        assert_eq!(state.reason(), Some(InterruptReason::Cancelled));
    }

    #[test]
    fn progress_is_forwarded_only_while_armed_and_clamped() {
        let state = AbortState::new();
        let seen = Mutex::new(Vec::<f32>::new());
        let sink = |f: f32| seen.lock().unwrap().push(f);
        let slot = ProgressSlot { f: &sink };
        {
            let _g = state.arm(None, None, Some(&slot));
            state.report(0);
            state.report(50);
            state.report(250);
            state.report(-5);
        }
        state.report(70); // disarmed: dropped, not delivered
        assert_eq!(*seen.lock().unwrap(), vec![0.0, 0.5, 1.0, 0.0]);
    }

    #[test]
    fn check_interrupt_orders_cancel_before_deadline() {
        let t = CancelToken::new();
        assert_eq!(check_interrupt(Some(&t), None), None);
        assert_eq!(check_interrupt(None, Some(Instant::now())), Some(InterruptReason::DeadlineExceeded));
        t.cancel();
        assert_eq!(check_interrupt(Some(&t), Some(Instant::now())), Some(InterruptReason::Cancelled));
    }
}
