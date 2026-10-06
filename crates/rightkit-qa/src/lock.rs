//! Run lock: one QA run per lock directory. A lock is reclaimed only when its owner is
//! provably gone (pid dead, or the pid now belongs to a different process); a live owner is
//! never displaced however long its run takes. Age applies only to a lock directory that
//! never received an owner record (a crash between `mkdir` and the owner write).
use crate::process::is_alive;
use crate::util::{err, new_id, now_iso, now_ms, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOwner {
    pub label: String,
    pub pid: u32,
    pub token: String,
    pub started_at: String,
    pub started_at_ms: u128,
    /// Command line of the owner at acquisition; guards against pid reuse.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

pub struct RunOwnership {
    pub owner: RunOwner,
    dir: PathBuf,
    released: bool,
}

#[derive(Debug, Clone)]
pub struct LockOptions {
    pub timeout: Duration,
    /// Only for a lock directory with no readable owner record: how long before it counts as abandoned.
    /// Never applied to a lock whose owner is alive.
    pub stale: Duration,
    pub poll: Duration,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(120), stale: Duration::from_secs(60), poll: Duration::from_millis(250) }
    }
}

pub fn acquire(lock_dir: &Path, label: &str, opts: &LockOptions) -> Result<RunOwnership> {
    acquire_with(lock_dir, label, opts, &owner_alive)
}

/// The owner is alive when its pid runs and still has the command line recorded at acquisition.
pub fn owner_alive(o: &RunOwner) -> bool {
    if !is_alive(o.pid) {
        return false;
    }
    match (&o.fingerprint, crate::process::fingerprint(o.pid)) {
        (Some(want), Some(now)) => *want == now,
        _ => true,
    }
}

pub fn acquire_with(lock_dir: &Path, label: &str, opts: &LockOptions, alive: &dyn Fn(&RunOwner) -> bool) -> Result<RunOwnership> {
    let owner_path = lock_dir.join("owner.json");
    let waiting_since = std::time::Instant::now();
    let owner = RunOwner {
        label: if label.trim().is_empty() { "qa-run".into() } else { label.trim().into() },
        pid: std::process::id(),
        token: new_id(),
        started_at: now_iso(),
        started_at_ms: now_ms(),
        fingerprint: crate::process::fingerprint(std::process::id()),
    };
    if let Some(parent) = lock_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    loop {
        match fs::create_dir(lock_dir) {
            Ok(()) => {
                let tmp = lock_dir.join(format!("owner.json.tmp-{}", owner.token));
                fs::write(&tmp, serde_json::to_vec_pretty(&owner)?)?;
                fs::rename(&tmp, &owner_path)?;
                return Ok(RunOwnership { owner, dir: lock_dir.to_path_buf(), released: false });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let current = read_owner(&owner_path);
                let reclaim = match &current {
                    Some(o) => !alive(o),
                    None => {
                        let age = fs::metadata(lock_dir).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).unwrap_or(Duration::ZERO);
                        age > opts.stale
                    }
                };
                if reclaim {
                    reclaim_dir(lock_dir, current.as_ref().map(|o| o.token.as_str()));
                    continue;
                }
                if waiting_since.elapsed() >= opts.timeout {
                    return err(format!(
                        "timed out waiting for QA run lock held by {} pid {}",
                        current.as_ref().map(|o| o.label.as_str()).unwrap_or("unknown"),
                        current.as_ref().map(|o| o.pid.to_string()).unwrap_or_else(|| "unknown".into())
                    ));
                }
                std::thread::sleep(opts.poll);
            }
            Err(e) => {
                let _ = fs::remove_dir_all(lock_dir);
                return Err(e.into());
            }
        }
    }
}

/// Atomically move the observed-dead lock aside, then delete it. The rename has one
/// winner; if what moved turns out to be a different owner's fresh lock it is put back.
fn reclaim_dir(lock_dir: &Path, observed: Option<&str>) {
    let tomb = lock_dir.with_extension(format!("reclaimed-{}", new_id()));
    if fs::rename(lock_dir, &tomb).is_err() {
        return;
    }
    let moved = read_owner(&tomb.join("owner.json")).map(|o| o.token);
    if moved.is_some() && moved.as_deref() != observed {
        let _ = fs::rename(&tomb, lock_dir);
        return;
    }
    let _ = fs::remove_dir_all(&tomb);
}

fn read_owner(path: &Path) -> Option<RunOwner> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

impl RunOwnership {
    /// Remove the lock only if this run still owns it.
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if read_owner(&self.dir.join("owner.json")).map(|o| o.token == self.owner.token).unwrap_or(false) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

impl Drop for RunOwnership {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-lock-{name}-{}", new_id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn second_run_times_out_while_first_holds() {
        let d = tmp("hold").join("native.lock");
        let first = acquire(&d, "a", &LockOptions::default()).unwrap();
        let quick = LockOptions { timeout: Duration::from_millis(60), poll: Duration::from_millis(10), ..Default::default() };
        let e = acquire(&d, "b", &quick).err().expect("must time out");
        assert!(e.0.contains("held by a"), "{e}");
        drop(first);
        assert!(acquire(&d, "c", &quick).is_ok());
    }

    #[test]
    fn live_owner_is_never_reclaimed_by_age() {
        let d = tmp("live").join("native.lock");
        let _first = acquire(&d, "a", &LockOptions::default()).unwrap();
        // Age the owner record far past any limit; the owner (this process) is alive.
        let mut o: RunOwner = serde_json::from_slice(&fs::read(d.join("owner.json")).unwrap()).unwrap();
        o.started_at_ms = 1;
        fs::write(d.join("owner.json"), serde_json::to_vec(&o).unwrap()).unwrap();
        let tight = LockOptions { timeout: Duration::from_millis(80), poll: Duration::from_millis(10), stale: Duration::from_millis(1) };
        assert!(acquire(&d, "b", &tight).is_err(), "a live owner must not be displaced by age");
        assert_eq!(read_owner(&d.join("owner.json")).unwrap().label, "a");
    }

    #[test]
    fn dead_owner_is_reclaimed() {
        let d = tmp("dead").join("native.lock");
        let _first = acquire_with(&d, "a", &LockOptions::default(), &|_: &RunOwner| true).unwrap();
        let second = acquire_with(&d, "b", &LockOptions::default(), &|_: &RunOwner| false).unwrap();
        assert_eq!(second.owner.label, "b");
    }
}
