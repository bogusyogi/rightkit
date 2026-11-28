//! Graceful signal shutdown budget for the daemon (DL-011).
//!
//! The ordered, single-flight shutdown itself is RightKit's `rightkit_daemon::OrderedShutdown`:
//! named phases in order, each on its own thread with its own deadline, failures and timeouts
//! recorded without stopping later phases, and a watchdog that ends the process at the hard
//! deadline. The phases (runtime, API, persistence, sandbox) are wired in
//! `main_runtime_server.rs::shutdown_phases`; this module keeps the host's budget.

use std::time::Duration;

/// How long a shutdown may take before the process is ended by force: long enough for an
/// ordered runtime/API/persistence/lock-release drain, short enough that a stuck shutdown does
/// not block a supervised restart indefinitely.
pub(crate) const DEFAULT_SHUTDOWN_EXIT_GRACE: Duration = Duration::from_secs(15);

/// The most one phase may take before it is given up on and the next phase runs.
pub(crate) const SHUTDOWN_PHASE_DEADLINE: Duration = Duration::from_secs(5);

/// A shutdown phase slower than this is named in the daemon log.
pub(crate) const SLOW_SHUTDOWN_STEP: Duration = Duration::from_secs(1);
