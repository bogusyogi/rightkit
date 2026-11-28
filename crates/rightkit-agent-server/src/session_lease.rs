//! One writer process per session, for in-process hosts that share an account (issue 510).
//!
//! Several in-process hosts (`{app} run`, the terminal app) may hold the account lock
//! `daemon.lock` at once, shared. What keeps them apart is this lease: an exclusive file lock per
//! session under `<state root>/inline-live/`, taken before a process creates or revives the
//! session and held until the process exits (the OS drops it however the process dies). The
//! storage layer's per-session event counter is cached in memory and is only right while one
//! process writes the session; the lease is that guarantee.
//!
//! A `serve` daemon holds the account lock exclusively, so no in-process host runs beside it and
//! it takes no leases: [`enable`] is called by the in-process host only.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);
static HELD: Mutex<Option<HashMap<String, rightkit_fs::FileLock>>> = Mutex::new(None);

/// How long [`hold`] keeps trying a busy lease. Another process's start-up recovery takes a free
/// lease for an instant to learn that it is free; a real holder keeps it for its whole life.
const BUSY_RETRY: Duration = Duration::from_millis(1500);

/// Turn leases on for this process. Called once by the in-process host.
pub(crate) fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

fn lease_path(session_id: &str) -> Option<PathBuf> {
    let name: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Some(
        rightkit_agent_config::state_root()?
            .join("inline-live")
            .join(format!("session-{name}.lock")),
    )
}

/// Take (or keep) this process's lease on `session_id`. `Err` carries a message that names the
/// session and says another {app} process is running it. With leases off, or with no state
/// root to put the lock in, this is `Ok`.
pub(crate) fn hold(session_id: &str) -> Result<(), String> {
    if !ENABLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let held = held.get_or_insert_with(HashMap::new);
    if held.contains_key(session_id) {
        return Ok(());
    }
    let Some(path) = lease_path(session_id) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return Err(format!(
                "cannot create the session lock folder {}: {error}",
                parent.display()
            ));
        }
    }
    let started = Instant::now();
    loop {
        match rightkit_fs::FileLock::try_acquire(&path) {
            Ok(lock) => {
                held.insert(session_id.to_string(), lock);
                return Ok(());
            }
            Err(rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. }) => {
                if started.elapsed() >= BUSY_RETRY {
                    let owner = rightkit_fs::probe_owner(&path)
                        .map(|owner| format!("process {}", owner.pid))
                        .unwrap_or_else(|| {
                            format!("another {} process", rightkit_agent_config::app_name())
                        });
                    return Err(format!(
                        "session {session_id} is being run by {owner}; one session has one writer at a time, so nothing was run. Wait for it to finish, or start a new session"
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(format!(
                    "cannot take the session lock {}: {error}",
                    path.display()
                ));
            }
        }
    }
}

/// Whether no live process holds the lease on `session_id` (this one included). Takes a free
/// lease for an instant and lets it go. With no state root the answer is `false`: without a lock
/// to look at, a session is never declared abandoned.
pub(crate) fn is_free(session_id: &str) -> bool {
    let held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if held
        .as_ref()
        .is_some_and(|held| held.contains_key(session_id))
    {
        return false;
    }
    let Some(path) = lease_path(session_id) else {
        return false;
    };
    if !path.exists() {
        // Never leased: a session written by a daemon, or by a build without leases. Those ran
        // under the exclusive account lock, which this process could not share while they lived.
        return true;
    }
    rightkit_fs::FileLock::try_acquire(&path).is_ok()
}

/// A lease file is left on disk when its holder exits (deleting a lock file on release lets two
/// later processes lock two different files of one name). Clear the ones nobody has held for a
/// week, and the per-process lock files of builds from before issue 510, so the folder does not
/// grow by two files per session for ever. Only a file whose lock is free is removed.
pub(crate) fn prune_stale() {
    const UNUSED_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
    let Some(dir) = rightkit_agent_config::state_root().map(|root| root.join("inline-live")) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "lock") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let old_enough = name.starts_with("session-")
            && entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= UNUSED_FOR);
        let legacy_process_lock = !name.starts_with("session-");
        if !(old_enough || legacy_process_lock) {
            continue;
        }
        let ours = held
            .as_ref()
            .is_some_and(|held| held.values().any(|lock| lock.path() == path.as_path()));
        if ours {
            continue;
        }
        if let Ok(lock) = rightkit_fs::FileLock::try_acquire(&path) {
            let _ = std::fs::remove_file(rightkit_fs::lock::owner_path_for(&path));
            let _ = std::fs::remove_file(&path);
            drop(lock);
        }
    }
}
