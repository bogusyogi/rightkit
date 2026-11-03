//! Crash-safe daemon reload: replace a running daemon with a new build without dropping the
//! endpoint or losing recovery state (rightkit#666).
//!
//! One serialized saga, one coordinator for every caller (a CLI, a TUI, the desktop shell):
//!
//! 1. write the durable marker `daemon.reload.json` (`request_id`, `hash`, `phase`, `pid`, time,
//!    `endpoint`, `detail`) in phase `starting`; the request id and hash bind the replacement to
//!    the update that asked, so a stale marker is never taken for readiness;
//! 2. host hook [`ReloadHost::persist_recovery`] (snapshot the sessions and state);
//! 3. host hook [`ReloadHost::fence_intake`] (refuse new mutations);
//! 4. marker `draining`, host hook [`ReloadHost::drain`] (finish or stop in-flight work);
//! 5. marker `handoff`; when a [`StagedBinary`] was given, swap it onto its target path under an
//!    [`ActivationFence`] (journalled, so a crash in the middle is undone by
//!    [`ActivationFence::recover`]);
//! 6. host hook [`ReloadHost::launch_replacement`] (spawn the new process with the same endpoint
//!    and the exact launch arguments; the host stamps [`ENV_RELOAD_REQUEST`] and
//!    [`ENV_RELOAD_HASH`] so the child knows it is a reload replacement);
//! 7. host hook [`ReloadHost::wait_ready`]: poll [`read_marker`] and the live endpoint until the
//!    replacement calls [`publish_socket_ready`] (marker `socket_ready`) for exactly that request
//!    id and hash; a timeout is a failure;
//! 8. host hook [`ReloadHost::resume`], then the activation is committed (backup removed).
//!
//! Rollback: any stage failure writes marker phase `failed` with `"stage: error"`, rolls the
//! activation fence back (restores the previous binary), then calls [`ReloadHost::rollback`] (which
//! terminates the never-ready replacement by pid, best effort, and restores the old runtime).
//! Errors compose: the stage error, the rollback error and a failed marker write are all reported
//! ([`ReloadError::StageAndRollback`]). The caller never silently continues after an incomplete
//! reload. A second request while one runs is [`ReloadError::InFlight`], never a second saga.

use crate::record::write_atomic;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The marker's file name, in the directory the host chooses.
pub const MARKER_FILE: &str = "daemon.reload.json";
/// Environment variable the replacement receives: the reload request id.
pub const ENV_RELOAD_REQUEST: &str = "RIGHTKIT_RELOAD_REQUEST";
/// Environment variable the replacement receives: the hash of the update.
pub const ENV_RELOAD_HASH: &str = "RIGHTKIT_RELOAD_HASH";

/// Where a reload is, as persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReloadPhase {
    Starting,
    Draining,
    Handoff,
    /// The replacement published its endpoint for this request.
    SocketReady,
    Failed,
}

/// The durable record of a reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadMarker {
    pub request_id: String,
    pub hash: String,
    pub phase: ReloadPhase,
    /// The pid of the process that wrote this phase (the old daemon, then the replacement).
    pub pid: u32,
    pub timestamp_ms: u64,
    pub endpoint: String,
    #[serde(default)]
    pub detail: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Read the marker in `dir`. `Ok(None)` when there is none.
pub fn read_marker(dir: &Path) -> io::Result<Option<ReloadMarker>> {
    match std::fs::read(dir.join(MARKER_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn write_marker(dir: &Path, marker: &ReloadMarker) -> io::Result<()> {
    let body = serde_json::to_vec_pretty(marker)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_atomic(&dir.join(MARKER_FILE), &body)
}

/// Called by the REPLACEMENT once its endpoint is live: marker `socket_ready` for exactly
/// `request_id` and `hash`. Refused when the marker is missing or names another request or hash,
/// so a stale marker, or a process that was not launched by this reload, can never claim
/// readiness.
pub fn publish_socket_ready(
    dir: &Path,
    request_id: &str,
    hash: &str,
    endpoint: &str,
) -> io::Result<()> {
    let Some(mut marker) = read_marker(dir)? else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no reload is in progress",
        ));
    };
    if marker.request_id != request_id || marker.hash != hash || marker.phase == ReloadPhase::Failed
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the reload marker is for another request, or the reload failed",
        ));
    }
    marker.phase = ReloadPhase::SocketReady;
    marker.pid = std::process::id();
    marker.timestamp_ms = now_ms();
    marker.endpoint = endpoint.to_string();
    write_marker(dir, &marker)
}

/// What the replacement is told, and what the hooks receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadRequest {
    pub request_id: String,
    pub hash: String,
    /// The endpoint the replacement must serve (the same one).
    pub endpoint: String,
}

/// A new binary staged next to the one that runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedBinary {
    pub staged: PathBuf,
    pub target: PathBuf,
}

/// The host's side of the saga. Every hook returns `Err(text)` to fail its stage.
pub trait ReloadHost {
    fn persist_recovery(&self, request: &ReloadRequest) -> Result<(), String>;
    fn fence_intake(&self, request: &ReloadRequest) -> Result<(), String>;
    fn drain(&self, request: &ReloadRequest) -> Result<(), String>;
    /// Spawn the replacement, detached, with the same endpoint and launch arguments, and with
    /// [`ENV_RELOAD_REQUEST`] and [`ENV_RELOAD_HASH`] set. Returns its pid.
    fn launch_replacement(&self, request: &ReloadRequest) -> Result<u32, String>;
    /// Wait until the replacement is ready: normally `poll_ready(dir, request)` and a live
    /// endpoint probe, until `timeout`. `Err` is a failure (a timeout is one).
    fn wait_ready(
        &self,
        request: &ReloadRequest,
        replacement_pid: u32,
        timeout: Duration,
    ) -> Result<(), String>;
    /// Reopen for work (the old daemon was replaced; this is the resume of the host's state).
    fn resume(&self, request: &ReloadRequest) -> Result<(), String>;
    /// Undo a failed reload: terminate the never-ready replacement (`replacement_pid`, when one
    /// was launched) and restore the old runtime.
    fn rollback(&self, request: &ReloadRequest, replacement_pid: Option<u32>)
        -> Result<(), String>;
}

/// True once the marker in `dir` is `socket_ready` for exactly this request (the check a
/// [`ReloadHost::wait_ready`] polls).
pub fn poll_ready(dir: &Path, request: &ReloadRequest) -> bool {
    matches!(
        read_marker(dir),
        Ok(Some(m)) if m.phase == ReloadPhase::SocketReady
            && m.request_id == request.request_id
            && m.hash == request.hash
    )
}

/// Why a reload did not complete.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReloadError {
    /// Another reload is running in this process.
    InFlight,
    /// The marker could not be written at the start: nothing was changed.
    Marker(io::Error),
    /// A stage failed and the rollback succeeded.
    Stage { stage: &'static str, error: String },
    /// A stage failed and the rollback, or the failed-marker write, also failed. Every error is
    /// kept: the host must not continue as if the old daemon were intact.
    StageAndRollback {
        stage: &'static str,
        error: String,
        rollback: Option<String>,
        marker: Option<String>,
    },
}

impl std::fmt::Display for ReloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InFlight => write!(f, "a reload is already running"),
            Self::Marker(e) => write!(f, "the reload marker could not be written: {e}"),
            Self::Stage { stage, error } => write!(f, "reload failed at {stage}: {error}"),
            Self::StageAndRollback {
                stage,
                error,
                rollback,
                marker,
            } => {
                write!(f, "reload failed at {stage}: {error}")?;
                if let Some(r) = rollback {
                    write!(f, "; rollback failed: {r}")?;
                }
                if let Some(m) = marker {
                    write!(f, "; the failed marker was not written: {m}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ReloadError {}

/// The journalled swap of a staged binary onto its target path. The journal
/// (`<target>.reload-journal`) names the target, the backup of the previous binary and the staged
/// file, so [`recover`](Self::recover) can undo a swap that a crash cut in the middle.
#[derive(Debug)]
pub struct ActivationFence {
    target: PathBuf,
    backup: PathBuf,
    journal: PathBuf,
    staged: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    target: PathBuf,
    backup: PathBuf,
    staged: PathBuf,
    swapped: bool,
}

fn side(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

impl ActivationFence {
    pub fn new(binary: &StagedBinary) -> Self {
        Self {
            target: binary.target.clone(),
            backup: side(&binary.target, ".reload-previous"),
            journal: side(&binary.target, ".reload-journal"),
            staged: binary.staged.clone(),
        }
    }

    fn write_journal(&self, swapped: bool) -> io::Result<()> {
        let body = serde_json::to_vec(&Journal {
            target: self.target.clone(),
            backup: self.backup.clone(),
            staged: self.staged.clone(),
            swapped,
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(&self.journal, &body)
    }

    /// Swap: journal, target to backup, staged onto target, journal marks it done.
    pub fn activate(&self) -> io::Result<()> {
        if !self.staged.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the staged binary is missing",
            ));
        }
        let _ = std::fs::remove_file(&self.backup);
        self.write_journal(false)?;
        std::fs::rename(&self.target, &self.backup)?;
        if let Err(e) = std::fs::rename(&self.staged, &self.target) {
            let _ = std::fs::rename(&self.backup, &self.target);
            let _ = std::fs::remove_file(&self.journal);
            return Err(e);
        }
        self.write_journal(true)
    }

    /// Put the previous binary back and drop the journal.
    pub fn rollback(&self) -> io::Result<()> {
        if self.backup.is_file() {
            // The new binary goes back to its staged name, the previous one to the target.
            let _ = std::fs::rename(&self.target, &self.staged);
            std::fs::rename(&self.backup, &self.target)?;
        }
        match std::fs::remove_file(&self.journal) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The reload succeeded: drop the backup and the journal.
    pub fn commit(&self) -> io::Result<()> {
        let _ = std::fs::remove_file(&self.backup);
        match std::fs::remove_file(&self.journal) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// At startup: a journal that was never committed means a reload was cut by a crash. When the
    /// marker in `dir` is not `socket_ready` for the journalled swap, the previous binary is
    /// restored. Returns whether a rollback ran.
    pub fn recover(binary: &StagedBinary, dir: &Path) -> io::Result<bool> {
        let fence = Self::new(binary);
        let Ok(bytes) = std::fs::read(&fence.journal) else {
            return Ok(false);
        };
        let Ok(journal) = serde_json::from_slice::<Journal>(&bytes) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unreadable reload journal",
            ));
        };
        let ready = matches!(read_marker(dir), Ok(Some(m)) if m.phase == ReloadPhase::SocketReady);
        if journal.swapped && ready {
            fence.commit()?;
            return Ok(false);
        }
        fence.rollback()?;
        Ok(true)
    }
}

/// Runs reloads, one at a time.
pub struct ReloadCoordinator {
    dir: PathBuf,
    ready_timeout: Duration,
    running: Mutex<()>,
}

impl ReloadCoordinator {
    /// `dir` holds the marker (the host's runtime directory).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            ready_timeout: Duration::from_secs(30),
            running: Mutex::new(()),
        }
    }

    /// How long the replacement gets to become ready. Default 30 s.
    #[must_use]
    pub fn ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Run the saga. `Ok` only when the replacement answered ready and the host resumed.
    pub fn run(
        &self,
        host: &dyn ReloadHost,
        request: &ReloadRequest,
        staged: Option<&StagedBinary>,
    ) -> Result<(), ReloadError> {
        let Ok(_one) = self.running.try_lock() else {
            return Err(ReloadError::InFlight);
        };
        let mut marker = ReloadMarker {
            request_id: request.request_id.clone(),
            hash: request.hash.clone(),
            phase: ReloadPhase::Starting,
            pid: std::process::id(),
            timestamp_ms: now_ms(),
            endpoint: request.endpoint.clone(),
            detail: String::new(),
        };
        write_marker(&self.dir, &marker).map_err(ReloadError::Marker)?;
        let fence = staged.map(ActivationFence::new);
        let mut replacement = None;
        let outcome = self.stages(host, request, &mut marker, fence.as_ref(), &mut replacement);
        match outcome {
            Ok(()) => {
                if let Some(fence) = &fence {
                    let _ = fence.commit();
                }
                Ok(())
            }
            Err((stage, error)) => {
                marker.phase = ReloadPhase::Failed;
                marker.detail = format!("{stage}: {error}");
                marker.timestamp_ms = now_ms();
                let marker_error = write_marker(&self.dir, &marker)
                    .err()
                    .map(|e| e.to_string());
                let mut rollback_error = None;
                if let Some(fence) = &fence {
                    if let Err(e) = fence.rollback() {
                        rollback_error = Some(format!("activation: {e}"));
                    }
                }
                if let Err(e) = host.rollback(request, replacement) {
                    rollback_error = Some(match rollback_error {
                        Some(prior) => format!("{prior}; host: {e}"),
                        None => format!("host: {e}"),
                    });
                }
                if rollback_error.is_none() && marker_error.is_none() {
                    Err(ReloadError::Stage { stage, error })
                } else {
                    Err(ReloadError::StageAndRollback {
                        stage,
                        error,
                        rollback: rollback_error,
                        marker: marker_error,
                    })
                }
            }
        }
    }

    fn stages(
        &self,
        host: &dyn ReloadHost,
        request: &ReloadRequest,
        marker: &mut ReloadMarker,
        fence: Option<&ActivationFence>,
        replacement: &mut Option<u32>,
    ) -> Result<(), (&'static str, String)> {
        host.persist_recovery(request)
            .map_err(|e| ("persist_recovery", e))?;
        host.fence_intake(request)
            .map_err(|e| ("fence_intake", e))?;
        self.advance(marker, ReloadPhase::Draining)?;
        host.drain(request).map_err(|e| ("drain", e))?;
        self.advance(marker, ReloadPhase::Handoff)?;
        if let Some(fence) = fence {
            fence.activate().map_err(|e| ("activate", e.to_string()))?;
        }
        let pid = host
            .launch_replacement(request)
            .map_err(|e| ("launch_replacement", e))?;
        *replacement = Some(pid);
        host.wait_ready(request, pid, self.ready_timeout)
            .map_err(|e| ("wait_ready", e))?;
        if !poll_ready(&self.dir, request) {
            return Err((
                "wait_ready",
                "the replacement did not publish readiness for this request".to_string(),
            ));
        }
        host.resume(request).map_err(|e| ("resume", e))
    }

    fn advance(
        &self,
        marker: &mut ReloadMarker,
        phase: ReloadPhase,
    ) -> Result<(), (&'static str, String)> {
        marker.phase = phase;
        marker.timestamp_ms = now_ms();
        write_marker(&self.dir, marker).map_err(|e| ("marker", e.to_string()))
    }
}
