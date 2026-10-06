//! Single-load lease: at most one process on the machine holds a model family
//! loaded at a time.
//!
//! Two Right Suite apps each loading the same ~700 MB speech model would double
//! memory and thrash the accelerator, so a process takes the family's lease
//! before it loads the model and drops it when it unloads. A second process
//! waits (or gives up) instead of loading a second copy.
//!
//! The lease is an OS advisory lock on `<suite>/runtime/model-use/<family>.lock`.
//! The OS releases it when the holder exits or crashes, so no stale-file
//! cleanup is needed. A sidecar `.holder.json` names the holder for status messages.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(250);

/// Production Parakeet runtime family. Other families are still supported by
/// the root-injected APIs below.
pub const PARAKEET_MODEL_FAMILY: &str = "parakeet-tdt-v3";
pub const PP_OCR_MODEL_FAMILY: &str = "pp-ocrv6";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Holder {
    pub app: String,
    pub pid: u32,
}

/// Held while this process has the model family loaded. Dropping releases it.
pub struct ModelLease {
    files: Vec<File>,
    infos: Vec<PathBuf>,
}

impl Drop for ModelLease {
    fn drop(&mut self) {
        // Remove holder metadata while the advisory lock is still held. This
        // prevents a successor from writing its metadata before our cleanup.
        for info in &self.infos {
            let _ = std::fs::remove_file(info);
        }
        for file in &self.files {
            let _ = file.unlock();
        }
    }
}

fn lease_path(family: &str) -> PathBuf {
    lease_path_at(&crate::suite::paths::suite_root(), family)
}

fn lease_path_at(root: &Path, family: &str) -> PathBuf {
    root.join("runtime")
        .join("model-use")
        .join(format!("{family}.lock"))
}

fn open(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create lease dir: {e}"))?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("open model lease: {e}"))
}

/// Who holds `family` right now, if anyone (best effort, for status text).
pub fn current_holder(family: &str) -> Option<Holder> {
    holder_at(&lease_path(family))
}

/// Holder details beneath an explicit suite root.
pub fn current_holder_at_root(root: &Path, family: &str) -> Option<Holder> {
    holder_at(&lease_path_at(root, family))
}

/// Try to hold family lease beneath an explicit suite root. Migration and
/// store GC use this to avoid process-global root assumptions.
pub fn try_acquire_at_root(
    root: &Path,
    family: &str,
    app: &str,
) -> Result<Option<ModelLease>, String> {
    ModelLease::try_acquire_at(&lease_path_at(root, family), app)
}

/// Acquire corresponding family leases under multiple roots in lexical order,
/// preventing old/new root deadlocks during compatibility migration.
pub fn try_acquire_at_roots(
    roots: &[&Path],
    family: &str,
    app: &str,
) -> Result<Option<ModelLease>, String> {
    let mut roots: Vec<PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
    roots.sort();
    roots.dedup();
    let mut files: Vec<File> = Vec::new();
    let mut infos: Vec<PathBuf> = Vec::new();
    for root in roots {
        let path = lease_path_at(&root, family);
        let Some(mut lease) = ModelLease::try_acquire_at(&path, app)? else {
            for info in &infos {
                let _ = std::fs::remove_file(info);
            }
            for file in &files {
                let _ = file.unlock();
            }
            return Ok(None);
        };
        files.append(&mut lease.files);
        infos.append(&mut lease.infos);
    }
    Ok(Some(ModelLease { files, infos }))
}

pub fn try_acquire_families_at_roots(
    roots: &[&Path],
    families: &[String],
    app: &str,
) -> Result<Option<ModelLease>, String> {
    let mut names = families.to_vec();
    names.sort();
    names.dedup();
    let mut files: Vec<File> = Vec::new();
    let mut infos: Vec<PathBuf> = Vec::new();
    for family in names {
        let Some(mut lease) = try_acquire_at_roots(roots, &family, app)? else {
            for info in &infos {
                let _ = std::fs::remove_file(info);
            }
            for file in &files {
                let _ = file.unlock();
            }
            return Ok(None);
        };
        files.append(&mut lease.files);
        infos.append(&mut lease.infos);
    }
    Ok(Some(ModelLease { files, infos }))
}

/// Compatibility lease guarding current Orthic Labs root plus legacy root.
pub fn try_acquire_compat(family: &str, app: &str) -> Result<Option<ModelLease>, String> {
    let current = crate::suite::paths::suite_root();
    let legacy = crate::suite::paths::legacy_suite_root();
    try_acquire_at_roots(&[&current, &legacy], family, app)
}

/// Wait for family lease beneath an explicit suite root.
pub fn acquire_at_root(
    root: &Path,
    family: &str,
    app: &str,
    waiting: &dyn Fn(Option<Holder>),
    give_up: &dyn Fn() -> bool,
) -> Result<ModelLease, String> {
    ModelLease::acquire_at(&lease_path_at(root, family), app, waiting, give_up)
}

/// Attempt to guard model GC. Unknown runtime lock files block conservatively;
/// returned guard remains held through the caller's deletion.
pub fn try_acquire_for_gc(root: &Path, app: &str) -> Result<Option<ModelLease>, String> {
    let current = crate::suite::paths::suite_root();
    let legacy = crate::suite::paths::legacy_suite_root();
    if root == current && legacy != current {
        return try_acquire_for_gc_at_roots(&[root, &legacy], app);
    }
    try_acquire_for_gc_at_roots(&[root], app)
}

fn runtime_families(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("runtime").join("model-use");
    let mut families = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|e| format!("read runtime lease entry: {e}"))?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(family) = name.strip_suffix(".lock") {
                    if family.is_empty()
                        || !family
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
                    {
                        return Err(format!("invalid runtime lease filename: {name}"));
                    }
                    families.push(family.to_string());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("read runtime lease dir: {error}")),
    }
    Ok(families)
}

/// Guard all known roots before deleting a model, including legacy runtime
/// leases that older releases may still hold.
pub fn try_acquire_for_gc_at_roots(
    roots: &[&Path],
    app: &str,
) -> Result<Option<ModelLease>, String> {
    let mut families = vec![
        PARAKEET_MODEL_FAMILY.to_string(),
        PP_OCR_MODEL_FAMILY.to_string(),
    ];
    for root in roots {
        families.extend(runtime_families(root)?);
    }
    try_acquire_families_at_roots(roots, &families, app)
}

/// Holder details live beside the lock file: on Windows the OS lock blocks
/// other handles from reading the locked file itself.
fn info_path(lock: &Path) -> PathBuf {
    lock.with_extension("holder.json")
}

fn holder_at(path: &Path) -> Option<Holder> {
    let body = std::fs::read_to_string(info_path(path)).ok()?;
    serde_json::from_str(&body).ok()
}

impl ModelLease {
    /// Take the lease if it is free; `Ok(None)` when another process holds it.
    pub fn try_acquire(family: &str, app: &str) -> Result<Option<Self>, String> {
        try_acquire_compat(family, app)
    }

    fn try_acquire_at(path: &Path, app: &str) -> Result<Option<Self>, String> {
        let file = open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(format!("model lease lock: {e}")),
        }
        let body = serde_json::to_vec(&Holder {
            app: app.to_string(),
            pid: std::process::id(),
        })
        .map_err(|e| e.to_string())?;
        let info = info_path(path);
        if let Err(error) = std::fs::write(&info, body) {
            let _ = file.unlock();
            return Err(format!("write model lease holder: {error}"));
        }
        Ok(Some(Self {
            files: vec![file],
            infos: vec![info],
        }))
    }

    /// Wait for the lease. `waiting` is called each poll with the current
    /// holder so the caller can tell the user who has the model; `give_up`
    /// returns true to stop waiting (cancel).
    pub fn acquire(
        family: &str,
        app: &str,
        waiting: &dyn Fn(Option<Holder>),
        give_up: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        let mut request: Option<YieldRequest> = None;
        loop {
            if let Some(lease) = try_acquire_compat(family, app)? {
                return Ok(lease);
            }
            if give_up() {
                return Err("cancelled".to_string());
            }
            if request.is_none() {
                let current = lease_path(family);
                let legacy = lease_path_at(&crate::suite::paths::legacy_suite_root(), family);
                request = Some(YieldRequest::post(&[current, legacy], app));
            }
            waiting(current_holder(family));
            std::thread::sleep(POLL);
        }
    }

    fn acquire_at(
        path: &Path,
        app: &str,
        waiting: &dyn Fn(Option<Holder>),
        give_up: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        let mut request: Option<YieldRequest> = None;
        loop {
            if let Some(lease) = Self::try_acquire_at(path, app)? {
                return Ok(lease);
            }
            if give_up() {
                return Err("cancelled".to_string());
            }
            if request.is_none() {
                request = Some(YieldRequest::post(&[path.to_path_buf()], app));
            }
            waiting(holder_at(path));
            std::thread::sleep(POLL);
        }
    }
}

/// `<family>.yield-request.json` beside a lease lock: tells an idle holder
/// (e.g. an always-on dictation app) that another app is waiting, so it can
/// unload and release. Removed when the waiter acquires or gives up.
struct YieldRequest {
    paths: Vec<PathBuf>,
}

/// Path of the yield request for the lease lock at `lock`.
pub fn yield_request_path(lock: &Path) -> PathBuf {
    let name = lock
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".lock"))
        .unwrap_or("model");
    lock.with_file_name(format!("{name}.yield-request.json"))
}

impl YieldRequest {
    fn post(locks: &[PathBuf], app: &str) -> Self {
        let body = format!(
            "{{\"app\":{},\"pid\":{},\"requested_at_ms\":{}}}",
            serde_json::Value::String(app.to_string()),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let mut paths = Vec::new();
        for lock in locks {
            let p = yield_request_path(lock);
            if p.parent().is_some_and(Path::is_dir) && std::fs::write(&p, &body).is_ok() {
                paths.push(p);
            }
        }
        Self { paths }
    }
}

impl Drop for YieldRequest {
    fn drop(&mut self) {
        for p in &self.paths {
            // Only remove our own request: another waiter may have replaced it.
            if std::fs::read_to_string(p)
                .is_ok_and(|b| b.contains(&format!("\"pid\":{},", std::process::id())))
            {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}
