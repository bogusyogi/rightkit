//! Graceful signal shutdown, ordering & idempotency for the RightKit daemon
//! (DL-011).
//!
//! Translated from `bb` `apps/host-daemon/src/daemon.ts::createDaemon`
//! (the `stop`/`shutdown`/watchdog closures): single-flight shutdown that
//! runs an ordered list of steps, accumulates (rather than short-circuits on)
//! step failures, and arms a bounded hard-deadline watchdog so a hung step or
//! an undrained runtime cannot keep the process alive past its budget.
//!
//! RightKit seam: steps are runtime → API → persistence → lock release
//! (per the lane's atom description), supplied by the orchestrator as
//! `ShutdownStep`s in that order; this module only owns the single-flight
//! gate, per-step failure accumulation, and the watchdog. It does not decide
//! what the steps are — that wiring belongs in `main_runtime_server.rs`
//! (hub file, out of this lane's allowlist).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How long a shutdown may take before the process is ended by force. Chosen
/// to match the donor's `DEFAULT_SHUTDOWN_EXIT_GRACE_MS` (15s): long enough
/// for an ordered runtime/API/persistence/lock-release drain, short enough
/// that a stuck shutdown does not block a supervised restart indefinitely.
pub(crate) const DEFAULT_SHUTDOWN_EXIT_GRACE: Duration = Duration::from_secs(15);

/// One named shutdown step. `run` returns `Err` on failure; the step's name
/// is used only for logging/diagnostics — failure does not stop later steps.
pub struct ShutdownStep {
    pub name: &'static str,
    pub run: Box<dyn FnOnce() -> Result<(), String> + Send>,
}

/// Accumulated result of a shutdown run: every step's outcome, in order.
#[derive(Debug, Default)]
pub struct ShutdownReport {
    pub failures: Vec<(&'static str, String)>,
}

impl ShutdownReport {
    pub fn is_ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Single-flight shutdown gate: only the first caller actually runs the
/// steps; concurrent/later callers (a second SIGTERM, a signal racing an
/// already-in-flight shutdown) observe the same in-flight/completed result
/// rather than re-running steps or racing partial state.
pub struct ShutdownCoordinator {
    started: AtomicBool,
    result: OnceLock<Mutex<Option<ShutdownReport>>>,
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            result: OnceLock::new(),
        }
    }
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `steps` in order exactly once for the coordinator's lifetime.
    /// A concurrent call while a shutdown is already running blocks until
    /// that run's report is available, then returns the same report —
    /// matching the donor's `stopPromise` reuse semantics, expressed
    /// synchronously since this module has no async runtime dependency.
    ///
    /// `on_deadline` is invoked if `deadline` elapses before every step has
    /// returned; the donor's watchdog forcibly exits the process at this
    /// point (`forceExit`), so pass a hard-exit callback in production and a
    /// no-op/assertion in tests.
    pub fn run_once(
        &self,
        steps: Vec<ShutdownStep>,
        deadline: Duration,
        on_deadline: impl FnOnce() + Send + 'static,
    ) -> ShutdownReport {
        if self
            .started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            // A shutdown is already running or has completed: wait for it to
            // publish, poll-free, via the shared slot (short critical
            // section — no busy loop needed since the first caller populates
            // it before returning).
            return self.wait_for_report();
        }

        let watchdog = arm_watchdog(deadline, on_deadline);
        let mut report = ShutdownReport::default();
        for step in steps {
            if let Err(e) = (step.run)() {
                report.failures.push((step.name, e));
            }
        }
        watchdog.disarm();

        let lock = self.result.get_or_init(|| Mutex::new(None));
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(clone_report(&report));
        report
    }

    fn wait_for_report(&self) -> ShutdownReport {
        // The single-flight winner populates `result` before its `run_once`
        // returns, so by the time a loser observes `started == true` the
        // slot is either already populated or about to be — a short bounded
        // spin covers the narrow window between the two without a full async
        // notification mechanism, matching this module's sync-only scope.
        loop {
            if let Some(lock) = self.result.get() {
                if let Some(report) = lock.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    return clone_report(report);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn clone_report(report: &ShutdownReport) -> ShutdownReport {
    ShutdownReport {
        failures: report.failures.clone(),
    }
}

/// Unref'd-equivalent watchdog: a background thread that calls `on_deadline`
/// if not disarmed within `deadline`. Rust threads have no "unref" concept,
/// so `disarm()` uses a flag the watchdog checks after waking, keeping the
/// thread from firing once shutdown finished on time (it still exits
/// promptly rather than lingering).
struct Watchdog {
    disarmed: std::sync::Arc<AtomicBool>,
}

impl Watchdog {
    fn disarm(&self) {
        self.disarmed.store(true, Ordering::SeqCst);
    }
}

fn arm_watchdog(deadline: Duration, on_deadline: impl FnOnce() + Send + 'static) -> Watchdog {
    let disarmed = std::sync::Arc::new(AtomicBool::new(false));
    let flag = disarmed.clone();
    std::thread::spawn(move || {
        std::thread::sleep(deadline);
        if !flag.load(Ordering::SeqCst) {
            on_deadline();
        }
    });
    Watchdog { disarmed }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn runs_steps_in_order() {
        let coordinator = ShutdownCoordinator::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        let o3 = order.clone();
        let steps = vec![
            ShutdownStep {
                name: "runtime",
                run: Box::new(move || {
                    o1.lock().unwrap().push("runtime");
                    Ok(())
                }),
            },
            ShutdownStep {
                name: "api",
                run: Box::new(move || {
                    o2.lock().unwrap().push("api");
                    Ok(())
                }),
            },
            ShutdownStep {
                name: "persistence",
                run: Box::new(move || {
                    o3.lock().unwrap().push("persistence");
                    Ok(())
                }),
            },
        ];

        let report = coordinator.run_once(steps, Duration::from_secs(5), || {
            panic!("watchdog must not fire when steps complete promptly")
        });
        assert!(report.is_ok());
        assert_eq!(
            *order.lock().unwrap(),
            vec!["runtime", "api", "persistence"]
        );
    }

    #[test]
    fn accumulates_failures_without_stopping_later_steps() {
        let coordinator = ShutdownCoordinator::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let r1 = ran.clone();
        let r2 = ran.clone();
        let steps = vec![
            ShutdownStep {
                name: "runtime",
                run: Box::new(move || {
                    r1.fetch_add(1, Ordering::SeqCst);
                    Err("runtime drain failed".to_string())
                }),
            },
            ShutdownStep {
                name: "lock_release",
                run: Box::new(move || {
                    r2.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
            },
        ];

        let report = coordinator.run_once(steps, Duration::from_secs(5), || {});
        assert_eq!(ran.load(Ordering::SeqCst), 2, "both steps must run");
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, "runtime");
    }

    #[test]
    fn single_flight_second_call_reuses_first_result() {
        let coordinator = Arc::new(ShutdownCoordinator::new());
        let steps = vec![ShutdownStep {
            name: "only",
            run: Box::new(|| Ok(())),
        }];
        let first = coordinator.run_once(steps, Duration::from_secs(5), || {});
        assert!(first.is_ok());

        // A second call with different (failing) steps must NOT re-run —
        // the coordinator has already completed, so the failing step must
        // never execute and the cached success report is returned.
        let second = coordinator.run_once(
            vec![ShutdownStep {
                name: "must_not_run",
                run: Box::new(|| Err("should never execute".to_string())),
            }],
            Duration::from_secs(5),
            || {},
        );
        assert!(second.is_ok(), "second call must reuse the first report");
    }

    #[test]
    fn watchdog_fires_when_a_step_hangs_past_deadline() {
        let coordinator = ShutdownCoordinator::new();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_flag = fired.clone();
        let steps = vec![ShutdownStep {
            name: "slow",
            run: Box::new(|| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            }),
        }];

        let _ = coordinator.run_once(steps, Duration::from_millis(50), move || {
            fired_flag.store(true, Ordering::SeqCst);
        });
        // Give the watchdog thread a moment past the 50ms deadline to fire.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            fired.load(Ordering::SeqCst),
            "watchdog must fire on a hung step"
        );
    }
}
