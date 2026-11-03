//! Ordered shutdown: named phases run in order, each with its own deadline, under a watchdog that
//! ends the process when a step hangs (rightkit#1122).
//!
//! A daemon stops in a fixed order (refuse new work, drain in-flight turns, stop workers, flush
//! storage). [`OrderedShutdown`] runs the phases the host registered, one after another, each on
//! its own thread so a hung step can be given up on at its deadline. A failed, panicked or
//! timed-out phase is recorded and the next phase still runs: a stuck drain must not keep the
//! storage from flushing. The shutdown is single-flight: the first [`run`](OrderedShutdown::run)
//! executes the phases, every other call (a signal handler and the window close arriving together)
//! waits for that run and gets the same [`ShutdownReport`]. The watchdog is a separate thread
//! started with the run; when the whole shutdown has not finished by its hard deadline it calls
//! the exit function, by default [`std::process::exit`] with the configured code.

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How one phase ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseOutcome {
    Completed,
    /// The phase returned an error.
    Failed(String),
    /// The phase did not finish within its deadline; its thread was left running.
    TimedOut,
    /// The phase panicked.
    Panicked,
}

/// One phase's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseReport {
    pub name: String,
    pub outcome: PhaseOutcome,
    pub elapsed: Duration,
}

/// What a shutdown did, in phase order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShutdownReport {
    pub phases: Vec<PhaseReport>,
}

impl ShutdownReport {
    /// Every phase completed.
    pub fn ok(&self) -> bool {
        self.phases
            .iter()
            .all(|p| p.outcome == PhaseOutcome::Completed)
    }
}

type PhaseFn = Box<dyn FnOnce() -> Result<(), String> + Send>;
type ExitFn = Arc<dyn Fn(i32) + Send + Sync>;

struct Phase {
    name: String,
    deadline: Duration,
    run: PhaseFn,
}

struct Inner {
    phases: Mutex<Option<Vec<Phase>>>,
    watchdog: Mutex<Option<(Duration, i32)>>,
    exit: Mutex<ExitFn>,
    report: Mutex<Option<ShutdownReport>>,
    done: Condvar,
}

/// The shutdown coordinator. Clones share one shutdown.
#[derive(Clone)]
pub struct OrderedShutdown {
    inner: Arc<Inner>,
}

impl Default for OrderedShutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderedShutdown {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                phases: Mutex::new(Some(Vec::new())),
                watchdog: Mutex::new(None),
                exit: Mutex::new(Arc::new(|code| std::process::exit(code))),
                report: Mutex::new(None),
                done: Condvar::new(),
            }),
        }
    }

    /// Append a phase. Phases run in the order added. `deadline` bounds this phase (a zero
    /// deadline is raised to 1 ms). Ignored once the shutdown has started.
    #[must_use]
    pub fn phase(
        self,
        name: impl Into<String>,
        deadline: Duration,
        run: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Self {
        if let Some(phases) = self
            .inner
            .phases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            phases.push(Phase {
                name: name.into(),
                deadline: deadline.max(Duration::from_millis(1)),
                run: Box::new(run),
            });
        }
        self
    }

    /// End the process with `exit_code` when the whole shutdown has not finished `hard_deadline`
    /// after it started. Without it there is no watchdog.
    #[must_use]
    pub fn watchdog(self, hard_deadline: Duration, exit_code: i32) -> Self {
        *self
            .inner
            .watchdog
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((hard_deadline, exit_code));
        self
    }

    /// Replace what the watchdog does (default: `std::process::exit(code)`), for a host that must
    /// do its own last write first.
    #[must_use]
    pub fn on_watchdog(self, exit: impl Fn(i32) + Send + Sync + 'static) -> Self {
        *self.inner.exit.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(exit);
        self
    }

    /// Run the shutdown, or wait for the run already in flight and return its report.
    pub fn run(&self) -> ShutdownReport {
        let phases = self
            .inner
            .phases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let Some(phases) = phases else {
            let mut report = self.inner.report.lock().unwrap_or_else(|e| e.into_inner());
            while report.is_none() {
                report = self
                    .inner
                    .done
                    .wait(report)
                    .unwrap_or_else(|e| e.into_inner());
            }
            return report.clone().unwrap_or_default();
        };

        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let watchdog = *self
            .inner
            .watchdog
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((hard, code)) = watchdog {
            let finished = finished.clone();
            let exit = self
                .inner
                .exit
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let _ = std::thread::Builder::new()
                .name("rk-shutdown-watchdog".into())
                .spawn(move || {
                    let (flag, cv) = &*finished;
                    let guard = flag.lock().unwrap_or_else(|e| e.into_inner());
                    let (guard, _) = cv
                        .wait_timeout_while(guard, hard, |done| !*done)
                        .unwrap_or_else(|e| e.into_inner());
                    if !*guard {
                        exit(code);
                    }
                });
        }

        let mut report = ShutdownReport::default();
        for phase in phases {
            let started = Instant::now();
            let (tx, rx) = mpsc::channel();
            let run = phase.run;
            let spawned = std::thread::Builder::new()
                .name(format!("rk-shutdown-{}", phase.name))
                .spawn(move || {
                    let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                    {
                        Ok(Ok(())) => PhaseOutcome::Completed,
                        Ok(Err(e)) => PhaseOutcome::Failed(e),
                        Err(_) => PhaseOutcome::Panicked,
                    };
                    let _ = tx.send(outcome);
                });
            let outcome = match spawned {
                Err(e) => PhaseOutcome::Failed(format!("could not start the phase: {e}")),
                Ok(_) => rx
                    .recv_timeout(phase.deadline)
                    .unwrap_or(PhaseOutcome::TimedOut),
            };
            report.phases.push(PhaseReport {
                name: phase.name,
                outcome,
                elapsed: started.elapsed(),
            });
        }

        {
            let (flag, cv) = &*finished;
            *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        }
        *self.inner.report.lock().unwrap_or_else(|e| e.into_inner()) = Some(report.clone());
        self.inner.done.notify_all();
        report
    }
}
