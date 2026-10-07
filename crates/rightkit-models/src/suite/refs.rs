//! Per-app reference registry under one cross-process registry guard.
//!
//! A malformed or unreadable ref is an error. Callers that delete store
//! entries must treat that error as "do not delete".

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REGISTRY_LOCK: &str = ".registry.lock";
const LOCK_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefFile {
    pub app: String,
    #[serde(default)]
    pub models: BTreeSet<String>,
    /// Free-form provenance (e.g. first-used version); not load-bearing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn ref_path(refs_dir: &Path, app_id: &str) -> Option<PathBuf> {
    if app_id.is_empty()
        || app_id
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        || app_id.starts_with('.')
    {
        return None;
    }
    Some(refs_dir.join(format!("{app_id}.json")))
}

/// Ref ids are model ids, or `<kind>@<version>` for shared runtimes in the
/// tools registry (hence `@`). Never used as a path, only as a set member.
fn valid_model_id(model_id: &str) -> bool {
    !model_id.is_empty()
        && model_id != "."
        && model_id != ".."
        && model_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'))
}

/// Held across a complete ref read-modify-write or GC operation.
pub struct RegistryGuard {
    file: File,
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Acquire refs registry's cross-process guard.
pub fn acquire_registry(refs_dir: &Path) -> Result<RegistryGuard, String> {
    fs::create_dir_all(refs_dir)
        .map_err(|e| format!("create refs dir {}: {e}", refs_dir.display()))?;
    let path = refs_dir.join(REGISTRY_LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("open refs registry lock: {e}"))?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(RegistryGuard { file }),
            Err(TryLockError::WouldBlock) => std::thread::sleep(LOCK_POLL),
            Err(TryLockError::Error(e)) => return Err(format!("lock refs registry: {e}")),
        }
    }
}

/// Run an operation while holding registry guard.
pub fn with_registry<T>(
    refs_dir: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _guard = acquire_registry(refs_dir)?;
    operation()
}

fn read_ref_unlocked(refs_dir: &Path, app_id: &str) -> Result<Option<RefFile>, String> {
    let Some(path) = ref_path(refs_dir, app_id) else {
        return Err(format!("invalid app id for refs: {app_id:?}"));
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let rf: RefFile =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if rf.app != app_id || rf.models.iter().any(|id| !valid_model_id(id)) {
        return Err(format!("invalid app or model id in {}", path.display()));
    }
    Ok(Some(rf))
}

/// Read one app's ref file; malformed content is an error so GC cannot
/// mistake unreadable ref for an unreferenced model.
pub fn read_ref(refs_dir: &Path, app_id: &str) -> Result<Option<RefFile>, String> {
    with_registry(refs_dir, || read_ref_unlocked(refs_dir, app_id))
}

fn write_ref_unlocked(refs_dir: &Path, app_id: &str, rf: &RefFile) -> Result<(), String> {
    let Some(path) = ref_path(refs_dir, app_id) else {
        return Err(format!("invalid app id for refs: {app_id:?}"));
    };
    fs::create_dir_all(refs_dir).map_err(|e| format!("create {}: {e}", refs_dir.display()))?;
    let json = serde_json::to_vec_pretty(rf).map_err(|e| format!("serialize refs: {e}"))?;
    let tmp = refs_dir.join(format!(
        ".{}.json.{}.{}.tmp",
        app_id,
        std::process::id(),
        unique_suffix()
    ));
    let write_result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("create {}: {e}", tmp.display()))?;
        use std::io::Write;
        file.write_all(&json)
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| format!("commit {}: {e}", path.display()))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_else(|_| std::process::id() as u128)
}

/// Record that `app_id` uses `model_id` (idempotent).
pub fn register(refs_dir: &Path, app_id: &str, model_id: &str) -> Result<(), String> {
    with_registry(refs_dir, || register_locked(refs_dir, app_id, model_id))
}

/// Register while caller already holds registry guard.
pub fn register_locked(refs_dir: &Path, app_id: &str, model_id: &str) -> Result<(), String> {
    if !valid_model_id(model_id) {
        return Err(format!("invalid model id for refs: {model_id:?}"));
    }
    let mut rf = read_ref_unlocked(refs_dir, app_id)?.unwrap_or(RefFile {
        app: app_id.to_string(),
        models: BTreeSet::new(),
        note: None,
    });
    if rf.models.insert(model_id.to_string()) {
        write_ref_unlocked(refs_dir, app_id, &rf)
    } else {
        Ok(())
    }
}

/// Remove `app_id`'s ref file. Returns ids now unreferenced by any app.
pub fn unregister(refs_dir: &Path, app_id: &str) -> Result<Vec<String>, String> {
    with_registry(refs_dir, || {
        let Some(path) = ref_path(refs_dir, app_id) else {
            return Err(format!("invalid app id for refs: {app_id:?}"));
        };
        let had = read_ref_unlocked(refs_dir, app_id)?
            .map(|rf| rf.models)
            .unwrap_or_default();
        // Validate every ref before removing this app's file. Uninstall must
        // surface corruption without changing registry state.
        let _ = referenced_ids_unlocked(refs_dir)?;
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        }
        let still_referenced = referenced_ids_unlocked(refs_dir)?;
        Ok(had
            .into_iter()
            .filter(|id| !still_referenced.contains(id))
            .collect())
    })
}

fn referenced_ids_unlocked(refs_dir: &Path) -> Result<BTreeSet<String>, String> {
    let mut ids = BTreeSet::new();
    for rf in ref_files_unlocked(refs_dir)? {
        ids.extend(rf.models);
    }
    Ok(ids)
}

fn ref_files_unlocked(refs_dir: &Path) -> Result<Vec<RefFile>, String> {
    let mut refs = Vec::new();
    let entries = match fs::read_dir(refs_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(refs),
        Err(e) => return Err(format!("read refs dir {}: {e}", refs_dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("read refs entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text =
            fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let rf: RefFile =
            serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        let expected_app = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("invalid ref filename {}", path.display()))?;
        if rf.app != expected_app || rf.models.iter().any(|id| !valid_model_id(id)) {
            return Err(format!("invalid app or model id in {}", path.display()));
        }
        refs.push(rf);
    }
    Ok(refs)
}

/// Carry every legacy app ref, including notes and unknown model ids, into a
/// new registry. Existing destination refs are merged rather than replaced.
pub fn migrate_root_refs(old_models: &Path, new_models: &Path) -> Result<(), String> {
    migrate_root_refs_with_map(old_models, new_models, &[])
}

/// As [`migrate_root_refs`], while retaining old ids and adding trusted mapped
/// ids (for example a generic Mac id to an immutable catalog id).
pub fn migrate_root_refs_with_map(
    old_models: &Path,
    new_models: &Path,
    mappings: &[(&str, &str)],
) -> Result<(), String> {
    let old_refs = old_models.join("refs");
    let new_refs = new_models.join("refs");
    if old_refs == new_refs {
        return Ok(());
    }
    let (first, second) = if old_refs < new_refs {
        (&old_refs, &new_refs)
    } else {
        (&new_refs, &old_refs)
    };
    let _first_guard = acquire_registry(first)?;
    let _second_guard = acquire_registry(second)?;
    let legacy = ref_files_unlocked(&old_refs)?;
    for mut rf in legacy {
        for (old_id, new_id) in mappings {
            if rf.models.contains(*old_id) {
                rf.models.insert((*new_id).to_string());
            }
        }
        let existing = read_ref_unlocked(&new_refs, &rf.app)?;
        if let Some(existing) = existing {
            rf.models.extend(existing.models);
            rf.note = match (rf.note.take(), existing.note) {
                (Some(old), Some(new)) if old == new => Some(old),
                (Some(old), Some(new)) => Some(format!("{old}\n{new}")),
                (Some(old), None) => Some(old),
                (None, Some(new)) => Some(new),
                (None, None) => None,
            };
        }
        write_ref_unlocked(&new_refs, &rf.app, &rf)?;
    }
    Ok(())
}

/// Remove app refs from both current and legacy registries while holding both
/// guards. Other apps' refs remain intact; malformed refs abort before change.
pub fn unregister_across_roots(
    old_models: &Path,
    new_models: &Path,
    app_id: &str,
) -> Result<Vec<String>, String> {
    let old_refs = old_models.join("refs");
    let new_refs = new_models.join("refs");
    if old_refs == new_refs {
        return unregister(&new_refs, app_id);
    }
    let refs_dirs: [&Path; 2] = [old_refs.as_path(), new_refs.as_path()];
    let (_paths, _guards) = acquire_registries(&refs_dirs)?;
    let mut removed_models = BTreeSet::new();
    for refs_dir in refs_dirs {
        removed_models.extend(
            read_ref_unlocked(refs_dir, app_id)?
                .map(|rf| rf.models)
                .unwrap_or_default(),
        );
    }
    for refs_dir in refs_dirs {
        let _ = referenced_ids_unlocked(refs_dir)?;
    }
    for refs_dir in refs_dirs {
        if let Some(path) = ref_path(refs_dir, app_id) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("remove app ref: {error}")),
            }
        }
    }
    let mut still = BTreeSet::new();
    for refs_dir in refs_dirs {
        still.extend(referenced_ids_unlocked(refs_dir)?);
    }
    Ok(removed_models
        .into_iter()
        .filter(|id| !still.contains(id))
        .collect())
}

/// Every model id referenced by at least one app. Errors return empty set for
/// compatibility; deletion paths must use [`referenced_ids_checked`].
pub fn referenced_ids(refs_dir: &Path) -> BTreeSet<String> {
    referenced_ids_checked(refs_dir).unwrap_or_default()
}

/// Checked ref scan. Any corrupt or unreadable ref blocks deletion callers.
pub fn referenced_ids_checked(refs_dir: &Path) -> Result<BTreeSet<String>, String> {
    with_registry(refs_dir, || referenced_ids_unlocked(refs_dir))
}

/// Read refs while caller already holds registry guard. Used by store GC.
pub fn referenced_ids_locked(refs_dir: &Path) -> Result<BTreeSet<String>, String> {
    referenced_ids_unlocked(refs_dir)
}

/// Scan multiple registries while holding all guards in lexical order.
pub fn referenced_ids_across_roots(refs_dirs: &[&Path]) -> Result<BTreeSet<String>, String> {
    let (paths, _guards) = acquire_registries(refs_dirs)?;
    let mut ids = BTreeSet::new();
    for path in &paths {
        ids.extend(referenced_ids_unlocked(path)?);
    }
    Ok(ids)
}

/// Acquire multiple registry guards in lexical order; returned guards remain
/// held for caller's complete operation.
pub fn acquire_registries(
    refs_dirs: &[&Path],
) -> Result<(Vec<PathBuf>, Vec<RegistryGuard>), String> {
    let mut paths: Vec<PathBuf> = refs_dirs.iter().map(|path| path.to_path_buf()).collect();
    paths.sort();
    paths.dedup();
    let mut guards = Vec::new();
    for path in &paths {
        guards.push(acquire_registry(path)?);
    }
    Ok((paths, guards))
}
