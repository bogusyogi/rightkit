//! Stall watchdog: detects that something that should keep answering has
//! stopped. This is the shared form of HeardRight's `hang_watchdog` (main
//! thread, 5 s) and `ui_stall_watchdog` (UI thread, 200 ms, one outstanding
//! ping, observer-gap guard, capture cooldown).
//!
//! Mechanism: a dedicated OS thread sends one *ping* at a time to the watched
//! party (a closure the app supplies: for the UI thread it is
//! `run_on_main_thread(move || ack.done())`; for a worker, a message on its
//! queue). The watched party calls [`Ack::done`] when it gets round to it. The
//! gap between sending and acknowledging is the stall. The watchdog thread
//! takes no application lock, so it can observe a deadlock without joining it.
//!
//! Semantics, all inherited from the donor and tested end to end:
//! - **Edge-triggered**: one [`StallEvent::Stalled`] per episode, then
//!   [`StallEvent::Recovered`] when the ack lands; the next stall reports again.
//! - **One outstanding ping**: a frozen target never accumulates queued pings.
//! - **Observer gap**: if the watchdog thread itself was not scheduled for
//!   longer than `observer_gap` (laptop sleep, SIGSTOP, heavy load) the interval
//!   proves nothing about the target, so its timing is discarded
//!   ([`StallEvent::ObserverGap`]) rather than reported as a stall.
//! - [`Cooldown`] rate-limits expensive captures (stack samples) per process.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Handed to the watched party; calling [`Ack::done`] proves it is alive.
#[derive(Clone, Debug)]
pub struct Ack {
    epoch: Instant,
    done_ms: Arc<AtomicU64>,
}

impl Ack {
    /// Mark the ping answered. Cheap, lock-free, callable from any thread.
    pub fn done(&self) {
        self.done_ms.store(self.epoch.elapsed().as_millis() as u64 + 1, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StallConfig {
    /// How often the watchdog wakes (HeardRight UI: 50 ms; main-thread: 1 s).
    pub poll: Duration,
    /// Ack latency at or above which an episode is reported (UI: 200 ms; hang: 5 s).
    pub threshold: Duration,
    /// A watchdog wake later than this since the previous one is an observer gap.
    pub observer_gap: Duration,
}

impl Default for StallConfig {
    fn default() -> Self {
        Self { poll: Duration::from_millis(50), threshold: Duration::from_millis(200), observer_gap: Duration::from_secs(1) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StallEvent {
    /// The target has not acknowledged for `gap`. `recovered` is true when the
    /// ack landed late (a stall seen only after the fact: nothing left to sample).
    Stalled { episode: u64, gap: Duration, recovered: bool },
    /// The target is answering again after a reported stall.
    Recovered { episode: u64, gap: Duration },
    /// The watchdog thread was not scheduled for `gap`; that interval's timing was discarded.
    ObserverGap { gap: Duration },
    /// The ping closure returned an error; the watchdog stopped.
    Stopped { reason: String },
}

/// Pure episode state: report once per stall, re-arm on recovery.
#[derive(Debug, Default)]
struct Episode {
    reported: bool,
}

impl Episode {
    fn detect(&mut self, gap: Duration, threshold: Duration) -> bool {
        if gap < threshold || self.reported {
            return false;
        }
        self.reported = true;
        true
    }
}

/// Minimum spacing between expensive actions (HeardRight samples stacks at
/// most once a minute). Pure; the caller supplies `now`.
#[derive(Debug, Clone, Copy)]
pub struct Cooldown {
    every: Duration,
    last: Option<Duration>,
}

impl Cooldown {
    pub fn new(every: Duration) -> Self {
        Self { every, last: None }
    }
    /// True (and records `now`) when the action may run.
    pub fn reserve(&mut self, now: Duration) -> bool {
        if self.last.is_some_and(|l| now.saturating_sub(l) < self.every) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// A running watchdog. Dropping it stops and joins the thread.
pub struct StallWatchdog {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StallWatchdog {
    /// Start watching. `ping` must hand the [`Ack`] to the target without
    /// blocking on it (enqueue and return); `on_event` runs on the watchdog
    /// thread, so keep it short and lock-free (log, flip an atomic, spawn work).
    pub fn spawn(
        name: &str,
        config: StallConfig,
        ping: impl Fn(Ack) -> Result<(), String> + Send + 'static,
        on_event: impl Fn(StallEvent) + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::Builder::new().name(name.to_string()).spawn(move || run(config, flag, ping, on_event))?;
        Ok(Self { stop, handle: Some(handle) })
    }

    /// Stop and join. Idempotent.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StallWatchdog {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    cfg: StallConfig,
    stop: Arc<AtomicBool>,
    ping: impl Fn(Ack) -> Result<(), String>,
    on_event: impl Fn(StallEvent),
) {
    let epoch = Instant::now();
    let ms = |at: Instant| at.saturating_duration_since(epoch).as_millis() as u64 + 1;
    let mut done = Arc::new(AtomicU64::new(0));
    let mut pending: Option<u64> = None;
    let mut episode = Episode::default();
    let mut episode_no = 0u64;
    let mut previous_poll = ms(Instant::now());
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(cfg.poll);
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let now = ms(Instant::now());
        let observer = Duration::from_millis(now.saturating_sub(previous_poll));
        previous_poll = now;
        if observer > cfg.observer_gap {
            on_event(StallEvent::ObserverGap { gap: observer });
            if pending.is_some() {
                pending = Some(now);
            }
            episode.reported = false;
        }
        if let Some(sent) = pending {
            let acked = done.load(Ordering::Acquire);
            let gap = Duration::from_millis(if acked != 0 { acked.saturating_sub(sent) } else { now.saturating_sub(sent) });
            if episode.detect(gap, cfg.threshold) {
                episode_no += 1;
                on_event(StallEvent::Stalled { episode: episode_no, gap, recovered: acked != 0 });
            }
            if acked == 0 {
                continue;
            }
            if episode.reported {
                on_event(StallEvent::Recovered { episode: episode_no, gap });
            }
            episode.reported = false;
            pending = None;
        }
        if pending.is_none() {
            done = Arc::new(AtomicU64::new(0));
            let ack = Ack { epoch, done_ms: Arc::clone(&done) };
            match ping(ack) {
                Ok(()) => pending = Some(ms(Instant::now())),
                Err(reason) => {
                    on_event(StallEvent::Stopped { reason });
                    return;
                }
            }
        }
    }
}
