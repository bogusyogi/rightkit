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
use rightkit_child_process::{AdoptedIdentity, IdentityCheck};
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
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
    /// The pid runs an executable other than the expected one, or its executable cannot be read.
    /// Nothing was signalled.
    ExecutableMismatch { pid: u32 },
    /// The stop was asked and signalled, but the caller's `released` condition did not hold within
    /// the grace. `process_gone` says whether the process had exited by then.
    NotReleased { pid: u32, process_gone: bool },
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
            Self::ExecutableMismatch { pid } => write!(
                f,
                "pid {pid} does not run the expected executable (or its executable cannot be read); not signalled"
            ),
            Self::NotReleased { pid, process_gone } => write!(
                f,
                "pid {pid} did not release the host's resource within the grace (process gone: {process_gone})"
            ),
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

/// Options for [`stop_identity`]: the grace for each wait, and the executable the pid must run.
#[derive(Debug, Clone)]
pub struct StopOptions {
    grace: Duration,
    expected_exe: Option<PathBuf>,
}

impl StopOptions {
    /// `grace` bounds each of the three waits, as in [`stop_incumbent`]. No executable pin.
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            expected_exe: None,
        }
    }

    /// Require the pid to run `exe` (compared after canonicalizing both paths). Checked before the
    /// ask and again before each signal.
    #[must_use]
    pub fn expected_exe(mut self, exe: impl Into<PathBuf>) -> Self {
        self.expected_exe = Some(exe.into());
        self
    }
}

/// Stop the host an [`AdoptedIdentity`] names, with the caller deciding what "stopped" means.
///
/// The same ladder as [`stop_incumbent`] (ask, wait, graceful signal, wait, forced tree stop, wait),
/// with three differences: the identity is given directly instead of through a [`ServiceRecord`];
/// the executable is re-read from the OS and compared with [`StopOptions::expected_exe`] before the
/// ask and again before each signal (the identity's stored executable is never used); and success is
/// the caller's `released` condition, not the pid's death. A process that dies without `released`
/// turning true is [`IncumbentError::NotReleased`].
///
/// `ask` is the app's polite request; its result is ignored. `released` is polled every 25 ms on the
/// calling thread only. Nothing is signalled when the pid is not the same process (`NotIncumbent`)
/// or runs another executable (`ExecutableMismatch`).
pub fn stop_identity(
    identity: &AdoptedIdentity,
    options: &StopOptions,
    ask: impl FnOnce() -> io::Result<()>,
    mut released: impl FnMut() -> bool,
) -> Result<IncumbentStop, IncumbentError> {
    let pid = identity.pid();
    if pid == std::process::id() {
        return Err(IncumbentError::OwnProcess);
    }
    if !confirm_before_signal(identity, options)? {
        return Ok(IncumbentStop::AlreadyGone);
    }
    let _ = ask();
    if wait_released(&mut released, options.grace) {
        return Ok(IncumbentStop::Asked);
    }
    // Graceful stage. A process that has already gone is not signalled again.
    if confirm_before_signal(identity, options)? {
        identity.quit_gracefully(options.grace)?;
        if wait_released(&mut released, options.grace) {
            return Ok(IncumbentStop::Signalled);
        }
    }
    // Forced stage, under the same re-check.
    if confirm_before_signal(identity, options)? {
        let _ = identity.terminate_tree(options.grace)?;
        if wait_released(&mut released, options.grace) {
            return Ok(IncumbentStop::Forced);
        }
    }
    Err(IncumbentError::NotReleased {
        pid,
        process_gone: identity.confirmed_gone()?,
    })
}

/// Steps 2 and 3 of [`stop_identity`], run immediately before each signal. `Ok(true)`: the pid is
/// still the same process and runs the expected executable, so it may be signalled. `Ok(false)`: no
/// process has the pid any more, so there is nothing to signal. `Err`: the pid names another process
/// (`NotIncumbent`) or another executable (`ExecutableMismatch`); nothing is signalled.
fn confirm_before_signal(
    identity: &AdoptedIdentity,
    options: &StopOptions,
) -> Result<bool, IncumbentError> {
    let pid = identity.pid();
    match identity.verify()? {
        IdentityCheck::Same => {}
        IdentityCheck::Gone => return Ok(false),
        IdentityCheck::Replaced => return Err(IncumbentError::NotIncumbent { pid }),
    }
    if let Some(expected) = options.expected_exe.as_deref() {
        let nz = NonZeroU32::new(pid).ok_or(IncumbentError::NotIncumbent { pid })?;
        if !exe_matches(nz, expected) {
            return Err(IncumbentError::ExecutableMismatch { pid });
        }
    }
    Ok(true)
}

/// Whether the process that now has `pid` runs `expected`. The executable is read afresh from the
/// OS, never taken from a stored identity. Unreadable, gone or different is `false`.
fn exe_matches(pid: NonZeroU32, expected: &Path) -> bool {
    let Ok(Some(fresh)) = AdoptedIdentity::capture(pid) else {
        return false;
    };
    let Some(actual) = fresh.exe() else {
        return false;
    };
    match (std::fs::canonicalize(actual), std::fs::canonicalize(expected)) {
        (Ok(a), Ok(e)) => a == e,
        _ => false,
    }
}

/// Poll `released` every 25 ms for up to `grace`. True as soon as it holds.
fn wait_released(released: &mut impl FnMut() -> bool, grace: Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    loop {
        if released() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
