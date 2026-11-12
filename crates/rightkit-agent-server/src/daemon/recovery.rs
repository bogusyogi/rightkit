//! Crash detection, recovery, stale cleanup & orphan prevention for the
//! RightKit daemon (DL-013).
//!
//! Translated from `prime-agent`
//! `packages/coding-agent/src/core/orphan-process-journal.ts`
//! (`recordOrphanProcessState`/`readActiveOrphanProcesses`/
//! `isOrphanProcessIdentityCurrent`/`clearOrphanProcessJournal`): an
//! append-only, `fsync`'d journal of owned-worker liveness records, tolerant
//! of a crash truncating only the final append (each line parses
//! independently; a malformed trailing line is skipped, not fatal), scoped
//! to RightKit's primary journal so a restarted daemon can inspect prior
//! owner records, and gated on exact-process identity
//! (RightKit's own `process_start_epoch_ms`, replacing the donor's
//! `getProcessStartId`) so a record is never "recovered" onto a pid that has
//! since been reused by an unrelated process.
//!
//! RightKit seam: `record_worker_state`/`read_active_workers` are the
//! journal primitives; `recover_orphans` is the startup adoption sweep the
//! daemon runs once, per atom (grok-build + prime-agent) — replace or
//! conservatively terminate a record whose identity no longer matches
//! (closing the "conservative replacement" half of DL-013), and adopt (keep)
//! the ones that do.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkerRecord {
    version: u32,
    pid: u32,
    owner_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    boot_epoch_ms: Option<u128>,
    active: bool,
    recorded_at: String,
}

const RECORD_VERSION: u32 = 1;

/// A worker this daemon's journal claims is still active, along with the
/// exact-process identity to re-verify before adoption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWorker {
    pub pid: u32,
    pub boot_epoch_ms: u128,
}

/// Default worker-journal path: `~/.{app}/worker-journal.jsonl`, a
/// sibling of the single-instance lock written by
/// [`super::ownership::default_lockfile_path`] and of the `daemon.port`
/// discovery file.
///
/// Deliberately resolves the home directory through the same helper the
/// lockfile uses, so both daemon-owned files land in one directory and honour
/// the same `rightkit_agent_config::state_root()` (in QA, the isolated `RIGHTKIT_SUITE_ROOT`). Callers that run against a non-default
/// database (QA, dev snapshots) should pass an explicit path instead — an
/// auxiliary daemon must not sweep the primary daemon's journal.
pub fn default_journal_path() -> Option<PathBuf> {
    let root = rightkit_agent_config::state_root()?;
    Some(root.join("worker-journal.jsonl"))
}

/// Append one worker liveness record to the journal at `path`, owned by the
/// current process (`std::process::id()`). Never fails the caller's
/// spawn/reap path — journal I/O errors are swallowed, matching the donor's
/// "process tracking must not make a successfully spawned command fail"
/// invariant.
pub fn record_worker_state(path: &Path, pid: u32, active: bool) {
    if pid == 0 {
        return;
    }
    let boot_epoch_ms = if active {
        crate::main_runtime::process_start_epoch_ms(pid)
    } else {
        None
    };
    let record = WorkerRecord {
        version: RECORD_VERSION,
        pid,
        owner_pid: std::process::id(),
        boot_epoch_ms,
        active,
        recorded_at: now_rfc3339(),
    };
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    let _ = append_line(path, &line);
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut file = daemon_journal_options()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn daemon_journal_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = OpenOptions::new();
    opts.mode(0o600);
    opts
}

#[cfg(not(unix))]
fn daemon_journal_options() -> OpenOptions {
    OpenOptions::new()
}

/// Read the journal at `path` and return the latest still-active record per
/// pid, restricted to records owned by `owner_pid`. A crash mid-append can
/// truncate only the final line — each line is parsed independently and a
/// malformed one is skipped rather than aborting the whole read.
pub fn read_active_workers(path: &Path, owner_pid: u32) -> Vec<ActiveWorker> {
    read_active_workers_for_owner(path, Some(owner_pid))
}

/// Read active records from every prior owner in this journal. Startup
/// recovery uses this rather than current daemon PID: process IDs change on
/// restart, so filtering by current PID would silently skip every crash-era
/// worker before identity/adoption can run.
fn read_active_workers_for_recovery(path: &Path) -> Vec<ActiveWorker> {
    read_active_workers_for_owner(path, None)
}

fn read_active_workers_for_owner(path: &Path, owner_pid: Option<u32>) -> Vec<ActiveWorker> {
    let mut contents = String::new();
    match File::open(path).and_then(|mut f| f.read_to_string(&mut contents)) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    }

    let mut latest: std::collections::HashMap<u32, WorkerRecord> = std::collections::HashMap::new();
    for line in contents.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<WorkerRecord>(line) else {
            continue;
        };
        if record.version != RECORD_VERSION
            || record.pid == 0
            || owner_pid.is_some_and(|owner_pid| record.owner_pid != owner_pid)
        {
            continue;
        }
        latest.insert(record.pid, record);
    }

    latest
        .into_values()
        .filter(|r| r.active)
        .filter_map(|r| {
            r.boot_epoch_ms.map(|boot_epoch_ms| ActiveWorker {
                pid: r.pid,
                boot_epoch_ms,
            })
        })
        .collect()
}

/// True if `worker`'s pid still resolves to a process with the same start
/// time recorded in the journal — i.e. the pid was not reused since the
/// record was written.
pub fn is_worker_identity_current(worker: &ActiveWorker) -> bool {
    // The owner's recorded-identity check: the pid must name a process whose start time is within
    // the owner tolerance of the recorded epoch milliseconds (coarse unit, so exact equality is
    // not a sound comparison).
    let Some(pid) = std::num::NonZeroU32::new(worker.pid) else {
        return false;
    };
    let recorded_ms = u64::try_from(worker.boot_epoch_ms).unwrap_or(u64::MAX);
    matches!(
        rightkit_child_process::AdoptedIdentity::from_recorded_unix_ms(
            pid,
            recorded_ms,
            super::kill_tree::PIN_TOLERANCE,
        ),
        Ok(Some(_))
    )
}

/// Remove the journal file entirely (called after a clean shutdown so a
/// fresh daemon start does not sweep records for workers it deliberately
/// reaped).
pub fn clear_journal(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Startup recovery sweep: for every active record a prior daemon instance
/// left in this primary journal, either adopt it (identity still
/// matches — the worker survived a daemon crash/restart) or conservatively
/// terminate it (identity no longer matches or the process is gone) via
/// `kill_tree::terminate_pid`, closing the orphan-leak gap a crashed
/// supervisor would otherwise leave. Returns the adopted workers.
pub fn recover_orphans(path: &Path, _owner_pid: u32) -> Vec<ActiveWorker> {
    let candidates = read_active_workers_for_recovery(path);
    let mut adopted = Vec::new();
    for worker in candidates {
        if is_worker_identity_current(&worker) {
            adopted.push(worker);
        } else {
            // Identity mismatch: either already dead (nothing to do) or the
            // pid was reused by an unrelated process (must not touch it).
            // Re-verifying inside terminate_pid_pinned via kill_tree closes
            // this without a second identity primitive here.
            let _ = super::kill_tree::terminate_pid_pinned(worker.pid, worker.boot_epoch_ms, false);
        }
    }
    adopted
}

fn now_rfc3339() -> String {
    // No chrono dependency in this crate's allowlisted scope; a monotonic-ish
    // wall-clock stamp is sufficient since recovery keys off boot_epoch_ms
    // identity, not this timestamp — kept only for diagnostics.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}Z", now.as_secs(), now.subsec_millis())
}
