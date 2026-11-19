//! The visibility guardian's worker thread (Windows and macOS).
//!
//! The loop blocks on a channel with no timeout while idle and wakes only when an OS event source
//! reports a change that can alter what is on screen; each coalesced batch is handed to the
//! `apply` closure, which the host implements (fullscreen update, visibility invariant). One
//! settle pass repeats `GuardianTimings::settle_pass_ms` after the last focus-class event. Only if
//! no OS event source could be installed does it fall back to a pass every
//! `GuardianTimings::degraded_repair_interval_ms`. Every period, including how long it waits for
//! the OS source to come up (`os_source_start_timeout_ms`) and how long one Accessibility request
//! may block (`ax_messaging_timeout_ms`), is the host's: the crate has no default.
//!
//! Lifecycle: the worker is a generation behind a [`GuardianHandle`]. [`start`] returns the
//! handle, or says why it could not start ([`GuardianStartError`]); [`GuardianHandle::stop`] wakes
//! the worker, which drops its OS event sources and exits, and waits for that for a bounded time.
//! Dropping the handle also asks the worker to stop (it does not wait). After a stop a new
//! guardian can be started. A panic in the host's `ready` or `apply` is caught and reported by type; it never
//! ends the worker. The OS event sources report to their own generation only
//! ([`crate::guardian::notify_generation`]), so a late callback from a stopped guardian cannot
//! reach a newer worker.
//!
//! What stop does and does not prove: it waits for the worker thread. On Windows the WinEvent pump
//! thread is a separate thread that dropping the sources only asks to quit (`WM_QUIT`); it is not
//! joined, ends on its own, and until it does its callbacks are dropped by the generation check.
//!
//! Statics: `NEXT_GENERATION` below (this worker's generation numbers, separate from
//! `lifecycle::NEXT_GENERATION`), and `guardian::EVENT_TX` (the one installed worker channel).
//!
//! UNPROVEN RECOVERY: the Windows "pill buried in the z-order" self-heal runs only on those
//! events, so a pill buried with no foreground/minimise event stays buried until the next one or
//! an explicit visibility pass by the host.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::guardian::{self, Due, GuardianEvent, GuardianTimings};
use crate::lifecycle::panic_type;

#[cfg(target_os = "macos")]
use crate::guardian_mac::GuardianSources;
#[cfg(target_os = "windows")]
use crate::guardian_win::GuardianSources;

/// The OS event sources of this platform. Other platforms have none: the worker then runs in its
/// degraded mode (a pass every 30 s), which is also what a failed hook installation gives.
#[cfg(any(target_os = "macos", target_os = "windows"))]
type PlatformSources = GuardianSources;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
type PlatformSources = NoSources;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Host-supplied OS-visible names for the guardian's threads and diagnostics. Each is non-empty
/// and at most 48 bytes without control characters (`event_prefix`: lower-case ASCII letters,
/// digits and `_`, at most 32 bytes).
#[derive(Clone, Copy, Debug)]
pub struct GuardianNames {
    /// The worker thread.
    pub worker: &'static str,
    /// The Windows WinEvent pump thread (unused on macOS).
    pub os_events: &'static str,
    /// Prefix of the guardian's diagnostics: `<event_prefix>_degraded`,
    /// `<event_prefix>_ready_panic`, `<event_prefix>_apply_panic`, `<event_prefix>_worker_panic`.
    pub event_prefix: &'static str,
}

impl GuardianNames {
    fn is_valid(&self) -> bool {
        let thread = |name: &str| {
            !name.is_empty() && name.len() <= 48 && !name.chars().any(char::is_control)
        };
        thread(self.worker)
            && thread(self.os_events)
            && !self.event_prefix.is_empty()
            && self.event_prefix.len() <= 32
            && self
                .event_prefix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }
}

/// Why [`start`] returned no guardian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardianStartError {
    /// The names or the timings are outside their documented ranges.
    InvalidInput,
    /// Another guardian still owns the event slot (it was stopped but has not finished). Start
    /// again once it has; the host reports this, it is not silent.
    SlotOccupied,
    /// The OS refused a thread.
    ThreadSpawn,
}

impl GuardianStartError {
    /// A fixed code for diagnostics.
    pub fn code(self) -> &'static str {
        match self {
            GuardianStartError::InvalidInput => "invalid_input",
            GuardianStartError::SlotOccupied => "slot_occupied",
            GuardianStartError::ThreadSpawn => "thread_spawn",
        }
    }
}

/// An OS event source the worker owns for its whole life.
pub(crate) trait EventSources {
    /// Called before each evaluation (follow the frontmost app, re-register what failed).
    fn refresh(&mut self);
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl EventSources for GuardianSources {
    fn refresh(&mut self) {
        GuardianSources::refresh(self);
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) struct NoSources;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl EventSources for NoSources {
    fn refresh(&mut self) {}
}

/// A running guardian. Dropping the handle asks the worker to stop (without waiting); call
/// [`GuardianHandle::stop`] to wait for it.
pub struct GuardianHandle {
    generation: u64,
    stop: Arc<AtomicBool>,
    done: Arc<(Mutex<bool>, Condvar)>,
}

impl GuardianHandle {
    fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        guardian::notify_generation(self.generation, GuardianEvent::Stop);
    }

    /// Wake the worker so it exits and drops its OS event sources, then wait up to `timeout` for
    /// it. Returns whether it finished in time (when it did not, it still exits and drops them
    /// once the host's `apply` returns).
    pub fn stop(&self, timeout: Duration) -> bool {
        self.request_stop();
        let (finished, changed) = &*self.done;
        let guard = finished.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = changed
            .wait_timeout_while(guard, timeout, |done| !*done)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

impl Drop for GuardianHandle {
    /// Ask the worker to stop. It never waits: a worker inside the host's `apply` finishes that
    /// batch first, then exits and drops its OS event sources.
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// The one guardian a host runs, kept where start and stop can both reach it. Keep it in a
/// `static`. [`GuardianSlot::start_once`] starts a guardian unless one is already held;
/// [`GuardianSlot::stop`] takes it out and waits (bounded) for the worker, after which a new one
/// can be started. The slot holds its lock for the whole of a start, so two racing starts make one
/// guardian.
pub struct GuardianSlot {
    handle: Mutex<Option<GuardianHandle>>,
}

impl Default for GuardianSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl GuardianSlot {
    pub const fn new() -> GuardianSlot {
        GuardianSlot {
            handle: Mutex::new(None),
        }
    }

    /// Whether a guardian is held.
    pub fn is_running(&self) -> bool {
        self.handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// [`start`], unless a guardian is already held. `Ok(true)` when this call started one,
    /// `Ok(false)` when one already ran. The error says why none runs; the host reports it.
    pub fn start_once(
        &self,
        names: GuardianNames,
        timings: GuardianTimings,
        ready: impl Fn() -> bool + Send + 'static,
        apply: impl Fn(Due) + Send + 'static,
        log: fn(&str, &str),
    ) -> Result<bool, GuardianStartError> {
        let mut slot = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Ok(false);
        }
        *slot = Some(start(names, timings, ready, apply, log)?);
        Ok(true)
    }

    /// Take the guardian out and [`GuardianHandle::stop`] it, waiting at most `timeout`. `None`
    /// when none was held; otherwise whether the worker finished in time (when it did not, it
    /// still exits once the host's `apply` returns). The lock is not held while waiting.
    pub fn stop(&self, timeout: Duration) -> Option<bool> {
        let handle = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take();
        handle.map(|handle| handle.stop(timeout))
    }
}

/// Start the worker. `ready` says whether the host can run a batch yet; `apply` runs one coalesced
/// batch on the worker thread; `log` is the host event sink (`(event, detail)`, the detail made
/// only of crate-controlled values). Errors say why no guardian runs; the host reports them.
pub fn start(
    names: GuardianNames,
    timings: GuardianTimings,
    ready: impl Fn() -> bool + Send + 'static,
    apply: impl Fn(Due) + Send + 'static,
    log: fn(&str, &str),
) -> Result<GuardianHandle, GuardianStartError> {
    if !names.is_valid() || !timings.is_valid() {
        return Err(GuardianStartError::InvalidInput);
    }
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::sync_channel::<GuardianEvent>(guardian::EVENT_QUEUE_CAPACITY);
    if !guardian::install_sender(generation, tx) {
        return Err(GuardianStartError::SlotOccupied);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_stop = Arc::clone(&stop);
    let worker_done = Arc::clone(&done);
    let spawned = std::thread::Builder::new()
        .name(names.worker.to_string())
        .spawn(move || {
            // Contain startup, source teardown and host diagnostics too, so any unwind reaches
            // slot cleanup and the exit signal. OS sources stay owned by this worker.
            let result = catch_unwind(AssertUnwindSafe(|| {
                let sources = start_sources(generation, names.os_events, &timings);
                worker_loop(
                    rx,
                    sources,
                    &timings,
                    names.event_prefix,
                    ready,
                    apply,
                    log,
                    &worker_stop,
                );
            }));
            // Clearing the slot makes every later callback of this generation a no-op: a callback
            // names its generation, and no slot holds it any more.
            guardian::clear_sender(generation);
            let (finished, changed) = &*worker_done;
            *finished.lock().unwrap_or_else(|e| e.into_inner()) = true;
            changed.notify_all();
            if let Err(payload) = result {
                log(
                    &format!("{}_worker_panic", names.event_prefix),
                    &format!("type={}", panic_type(payload.as_ref())),
                );
            }
        });
    if spawned.is_err() {
        guardian::clear_sender(generation);
        return Err(GuardianStartError::ThreadSpawn);
    }
    Ok(GuardianHandle {
        generation,
        stop,
        done,
    })
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn start_sources(
    generation: u64,
    os_events: &str,
    timings: &GuardianTimings,
) -> Option<PlatformSources> {
    GuardianSources::start(generation, os_events, timings)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn start_sources(
    _generation: u64,
    _os_events: &str,
    _timings: &GuardianTimings,
) -> Option<PlatformSources> {
    None
}

/// The worker loop: `sources` is dropped when it returns.
#[allow(clippy::too_many_arguments)]
pub(crate) fn worker_loop<S: EventSources>(
    rx: Receiver<GuardianEvent>,
    mut sources: Option<S>,
    timings: &GuardianTimings,
    event_prefix: &str,
    ready: impl Fn() -> bool,
    apply: impl Fn(Due),
    log: fn(&str, &str),
    stop_flag: &AtomicBool,
) {
    let degraded = sources.is_none();
    if degraded {
        log(
            &format!("{event_prefix}_degraded"),
            "reason=no_os_event_source",
        );
    }
    let mut coalescer = guardian::Coalescer::new(*timings);
    // Establish the initial state, as the poll's first tick used to.
    coalescer.note(GuardianEvent::ForegroundChanged, Instant::now());
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        let received =
            match guardian::next_wait(coalescer.next_deadline(), Instant::now(), degraded, timings)
            {
                // Idle: block until the OS sends an event. No periodic wake.
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(wait) => rx.recv_timeout(wait),
            };
        match received {
            Ok(GuardianEvent::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(event) => coalescer.note(event, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {
                // Reached only in degraded mode with nothing scheduled.
                if degraded && coalescer.is_idle() {
                    coalescer.note(GuardianEvent::ForegroundChanged, Instant::now());
                }
            }
        }
        // Bound each drain even if producers refill the queue continuously, then service due
        // work and recheck stop before receiving again. Idle still blocks without a timer.
        let mut stop = false;
        for _ in 0..guardian::EVENT_QUEUE_CAPACITY {
            if stop_flag.load(Ordering::SeqCst) {
                stop = true;
                break;
            }
            let Ok(event) = rx.try_recv() else { break };
            if event == GuardianEvent::Stop {
                stop = true;
                break;
            }
            coalescer.note(event, Instant::now());
        }
        if stop || stop_flag.load(Ordering::SeqCst) {
            break;
        }
        let due = coalescer.take_due(Instant::now());
        if !due.any() {
            continue;
        }
        // A plain std::thread has no autorelease pool: drain one around every batch of AppKit /
        // AX work on the macOS worker.
        let mut run_batch = || {
            let mut phase = "ready";
            let result = catch_unwind(AssertUnwindSafe(|| {
                if !ready() {
                    return;
                }
                phase = "apply";
                if let Some(sources) = sources.as_mut() {
                    sources.refresh();
                }
                apply(due);
            }));
            if let Err(payload) = result {
                log(
                    &format!("{event_prefix}_{phase}_panic"),
                    &format!("type={}", panic_type(payload.as_ref())),
                );
            }
        };
        #[cfg(target_os = "macos")]
        objc2::rc::autoreleasepool(|_| run_batch());
        #[cfg(not(target_os = "macos"))]
        run_batch();
    }
    // Dropping the sources removes the AppKit observers (macOS) or posts WM_QUIT to the WinEvent
    // pump (Windows).
    #[cfg(target_os = "macos")]
    objc2::rc::autoreleasepool(|_| drop(sources));
    #[cfg(not(target_os = "macos"))]
    drop(sources);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Fake {
        refreshes: Rc<Cell<u32>>,
        dropped: Rc<Cell<bool>>,
    }

    impl EventSources for Fake {
        fn refresh(&mut self) {
            self.refreshes.set(self.refreshes.get() + 1);
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    fn quiet(_: &str, _: &str) {}

    const T: GuardianTimings = GuardianTimings {
        focus_coalesce_ms: 40,
        geometry_coalesce_ms: 150,
        settle_pass_ms: 500,
        degraded_repair_interval_ms: 30_000,
        probe_min_interval_ms: 30_000,
        ax_messaging_timeout_ms: 100,
        os_source_start_timeout_ms: 3_000,
    };

    #[test]
    fn a_burst_is_one_batch_then_stop_drops_the_sources() {
        let (tx, rx) = mpsc::channel();
        let refreshes = Rc::new(Cell::new(0));
        let dropped = Rc::new(Cell::new(false));
        let sources = Fake {
            refreshes: Rc::clone(&refreshes),
            dropped: Rc::clone(&dropped),
        };
        let applied = Rc::new(Cell::new(0));
        let applied_in = Rc::clone(&applied);
        // The initial note plus these coalesce into batches; Stop ends the loop.
        tx.send(GuardianEvent::ForegroundChanged).unwrap();
        tx.send(GuardianEvent::GeometryChanged).unwrap();
        // After the settle pass the worker blocks again; Stop arrives from a helper thread.
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            tx.send(GuardianEvent::Stop).unwrap();
        });
        let flag = AtomicBool::new(false);
        worker_loop(
            rx,
            Some(sources),
            &T,
            "t",
            || true,
            move |_| applied_in.set(applied_in.get() + 1),
            quiet,
            &flag,
        );
        stopper.join().unwrap();
        assert!(applied.get() >= 1, "at least one batch ran");
        assert_eq!(refreshes.get(), applied.get());
        assert!(
            dropped.get(),
            "the OS sources are dropped when the worker ends"
        );
    }

    #[test]
    fn a_host_panic_is_caught_and_the_worker_keeps_running() {
        let (tx, rx) = mpsc::channel();
        let applied = Rc::new(Cell::new(0));
        let applied_in = Rc::clone(&applied);
        tx.send(GuardianEvent::ForegroundChanged).unwrap();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            tx.send(GuardianEvent::Stop).unwrap();
        });
        let flag = AtomicBool::new(false);
        worker_loop::<Fake>(
            rx,
            None,
            &T,
            "t",
            || true,
            move |_| {
                applied_in.set(applied_in.get() + 1);
                panic!("host fault");
            },
            quiet,
            &flag,
        );
        stopper.join().unwrap();
        // The initial pass and the settle pass both ran despite the panics.
        assert!(applied.get() >= 2, "ran {} batches", applied.get());
    }

    #[test]
    fn a_batch_waits_while_the_host_is_not_ready() {
        let (tx, rx) = mpsc::channel();
        let applied = Rc::new(Cell::new(0));
        let applied_in = Rc::clone(&applied);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            tx.send(GuardianEvent::Stop).unwrap();
        });
        let flag = AtomicBool::new(false);
        worker_loop::<Fake>(
            rx,
            None,
            &T,
            "t",
            || false,
            move |_| applied_in.set(applied_in.get() + 1),
            quiet,
            &flag,
        );
        stopper.join().unwrap();
        assert_eq!(applied.get(), 0);
    }

    #[test]
    fn a_set_stop_flag_ends_the_worker_without_an_event() {
        let (_tx, rx) = mpsc::channel::<GuardianEvent>();
        let flag = AtomicBool::new(true);
        worker_loop::<Fake>(rx, None, &T, "t", || true, |_| {}, quiet, &flag);
    }
}
