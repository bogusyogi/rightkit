//! Crash detection, recovery and orphan prevention for the daemon (DL-013).
//!
//! The journal is RightKit's `rightkit_child_process::WorkerJournal`: an append-only file of
//! worker and owner identities (pid plus start time), each record flushed before the call
//! returns, a torn final line dropped on read. `recover_journal` returns the workers still
//! running as the process they were recorded as whose owner is gone; a recycled pid is never
//! returned because its start time no longer matches. This module keeps the host's journal
//! location and the startup sweep's policy (adopt the survivors, bound the file).

use std::path::{Path, PathBuf};

use rightkit_child_process::{AdoptedIdentity, WorkerJournal};

/// Default worker-journal path: `<state root>/worker-journal.rkj`, beside the single-instance lock
/// and the `daemon.port` discovery file, under the same `rightkit_agent_config::state_root()` (in QA,
/// the isolated suite root). Callers against a non-default database must pass their own path:
/// an auxiliary daemon must not sweep the primary daemon's journal.
pub fn default_journal_path() -> Option<PathBuf> {
    let root = rightkit_agent_config::state_root()?;
    // The JSON-lines journal this replaced is retired: nothing reads it any more.
    let _ = std::fs::remove_file(root.join("worker-journal.jsonl"));
    Some(root.join("worker-journal.rkj"))
}

/// Record `pid` as a worker this process started (`active`) or one that ended. Never fails the
/// caller's spawn or reap path: process tracking must not make a successfully spawned command
/// fail, so journal errors are only logged.
pub fn record_worker_state(path: &Path, pid: u32, active: bool) {
    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return;
    };
    let Ok(Some(identity)) = AdoptedIdentity::capture(pid) else {
        return;
    };
    let result = WorkerJournal::open(path).and_then(|mut journal| {
        if active {
            journal.record(&identity)
        } else {
            journal.finish(&identity)
        }
    });
    if let Err(error) = result {
        eprintln!("[serve] worker journal write failed: {error}");
    }
}

/// Remove the journal (after a clean shutdown, so a fresh start does not sweep workers this
/// daemon deliberately reaped).
pub fn clear_journal(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Startup recovery sweep: every worker a previous daemon recorded that is still the same process
/// and whose owner is gone is adopted (it survived a daemon crash or a reload). Workers that are
/// gone or whose pid was recycled are dropped, never signalled. The journal is then rewritten to
/// hold exactly the adopted workers under this process, so it does not grow across restarts.
pub fn recover_orphans(path: &Path) -> Vec<AdoptedIdentity> {
    let recovery = match rightkit_child_process::recover_journal(path) {
        Ok(recovery) => recovery,
        Err(error) => {
            eprintln!("[serve] worker journal unreadable: {error}");
            return Vec::new();
        }
    };
    if recovery.corrupt_lines > 0 || recovery.torn_tail {
        eprintln!(
            "[serve] worker journal: skipped {} corrupt line(s){}",
            recovery.corrupt_lines,
            if recovery.torn_tail {
                ", dropped a torn final record"
            } else {
                ""
            }
        );
    }
    let own = std::process::id();
    let adopted: Vec<AdoptedIdentity> = recovery
        .orphans
        .into_iter()
        .filter(|worker| worker.pid() != own)
        .collect();
    if let Err(error) = WorkerJournal::open(path).and_then(|mut journal| journal.reset(&adopted)) {
        eprintln!("[serve] worker journal reset failed: {error}");
    }
    adopted
}
