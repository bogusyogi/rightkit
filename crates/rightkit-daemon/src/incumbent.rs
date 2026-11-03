//! Replace a live but unresponsive incumbent host: ask it to stop, wait, then signal only a
//! process whose identity matches the record (rightkit#1121).
//!
//! Takeover by rank ([`publish_takeover`](crate::publish_takeover)) is cooperative: the incumbent
//! yields. A hung or old-version host never reads the request. [`stop_incumbent`] is the next
//! step for a new host that is entitled to replace it: the caller's own `ask` (for a loopback HTTP
//! daemon, `POST /internal/takeover`), a bounded wait, a graceful signal, a bounded wait and only
//! then a forced stop of the tree. Every signal goes through
//! [`AdoptedIdentity`](rightkit_child_process::AdoptedIdentity), which refuses a pid whose start
//! time differs from the one recorded, so a foreign or recycled pid is never signalled. A record
//! without a process start time ([`ServiceRecord::process_started_ms`]) cannot name one process:
//! the incumbent is asked and waited for, and never signalled.

use crate::record::ServiceRecord;
use rightkit_child_process::AdoptedIdentity;
use std::io;
use std::num::NonZeroU32;
use std::time::Duration;

/// How far the recorded start time may differ from the live process's start time. The record
/// stores milliseconds; the process table is exact.
const START_TOLERANCE: Duration = Duration::from_secs(2);

/// How the incumbent ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IncumbentStop {
    /// No process had the recorded pid already; nothing was asked or signalled.
    AlreadyGone,
    /// It stopped after being asked, within the grace.
    Asked,
    /// It stopped after one graceful signal.
    Signalled,
    /// It stopped after the forced stop of its tree.
    Forced,
}

/// Why the incumbent was not replaced. Nothing was signalled for `NotIncumbent`, `NoIdentity`
/// and `OwnProcess`.
#[derive(Debug)]
#[non_exhaustive]
pub enum IncumbentError {
    /// The pid is alive but is not the recorded process (a recycled pid): never signalled.
    NotIncumbent { pid: u32 },
    /// The record carries no process start time, so no signal may be sent; the incumbent was
    /// asked and did not stop within the grace.
    NoIdentity { pid: u32 },
    /// The record names this process.
    OwnProcess,
    /// The incumbent is still running after the forced stop.
    Survived { pid: u32 },
    /// The OS refused a query or a signal.
    Io(io::Error),
}

impl std::fmt::Display for IncumbentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotIncumbent { pid } => {
                write!(
                    f,
                    "pid {pid} is not the recorded host (recycled); not signalled"
                )
            }
            Self::NoIdentity { pid } => write!(
                f,
                "the record of pid {pid} has no start time, so it was asked but never signalled"
            ),
            Self::OwnProcess => write!(f, "the record names this process"),
            Self::Survived { pid } => write!(f, "pid {pid} survived the forced stop"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IncumbentError {}

impl From<io::Error> for IncumbentError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Stop the host `record` describes. `ask` is the app's polite request (its result is ignored: a
/// hung host fails it, and the wait below decides), `grace` bounds each of the three waits
/// (after the ask, after the graceful signal, after the forced stop).
pub fn stop_incumbent(
    record: &ServiceRecord,
    grace: Duration,
    ask: impl FnOnce() -> io::Result<()>,
) -> Result<IncumbentStop, IncumbentError> {
    let pid = record.pid;
    if pid == std::process::id() {
        return Err(IncumbentError::OwnProcess);
    }
    let Some(nz) = NonZeroU32::new(pid) else {
        return Ok(IncumbentStop::AlreadyGone);
    };
    if !crate::transport::pid_alive(pid) {
        return Ok(IncumbentStop::AlreadyGone);
    }
    if record.process_started_ms == 0 {
        // No identity: ask and wait, never signal.
        let _ = ask();
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if !crate::transport::pid_alive(pid) {
                return Ok(IncumbentStop::Asked);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        return Err(IncumbentError::NoIdentity { pid });
    }
    let identity =
        AdoptedIdentity::from_recorded_unix_ms(nz, record.process_started_ms, START_TOLERANCE)?;
    let Some(identity) = identity else {
        // Gone between the liveness probe and the identity read, or a recycled pid.
        return if crate::transport::pid_alive(pid) {
            Err(IncumbentError::NotIncumbent { pid })
        } else {
            Ok(IncumbentStop::AlreadyGone)
        };
    };
    let _ = ask();
    if identity.wait_confirmed_gone(grace)? {
        return Ok(IncumbentStop::Asked);
    }
    if identity.quit_gracefully(grace)? {
        return Ok(IncumbentStop::Signalled);
    }
    let report = identity.terminate_tree(grace)?;
    if report.survivors.is_empty() && report.failed.is_empty() && identity.confirmed_gone()? {
        Ok(IncumbentStop::Forced)
    } else {
        Err(IncumbentError::Survived { pid })
    }
}
