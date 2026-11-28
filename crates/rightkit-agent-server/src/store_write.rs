//! One write for the host's small JSON state files (notifications, trust, execution stores, schedules, ...), and
//! the one way a damaged one is set aside.
//!
//! The bytes are published with the owner's `rightkit_fs::atomic_write`, which replaces the destination NAME with a
//! new file. That is not what the plain `std::fs::write` it replaced did for a path that is a link, so the link cases
//! are decided here rather than silently changed:
//! - a symlink is followed, as `std::fs::write` followed it: the file it points at is replaced atomically and the link
//!   stays a link (a state file kept in a dotfiles repo through a symlink is not turned into a stray copy). A DANGLING
//!   link is resolved the same way: the file it names is created (as `std::fs::write` created it), and the link stays a
//!   link; a link that cannot be resolved at all (a loop) is an error, never replaced.
//! - a hard-linked file (link count above 1; Unix `nlink`, Windows `nNumberOfLinks` from
//!   `GetFileInformationByHandle`) is written IN PLACE: replacing the name would leave the other names on the old
//!   content, so the link wins over atomicity there, as it did before adoption.
//!
//! The in-place path is NOT crash-atomic. It truncates, writes and then flushes (`sync_data`): a crash or a kill after
//! the truncate and before the flush completes leaves an empty or partial file, and a concurrent reader can see that
//! state. That is the same exposure the plain `std::fs::write` had, limited to files that have a second hard link. The
//! loaders therefore never treat a file they cannot parse as "no data": see [`set_aside_damaged`].

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Writes `bytes` to the state file at `path` (see the module notes for symlinks and hard links).
pub(crate) fn write_store_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = link_target(path)?;
    if hard_linked(&target) {
        return write_in_place(&target, bytes);
    }
    rightkit_fs::atomic_write(&target, bytes).map_err(io::Error::other)
}

/// How long one store update waits for another writer of the same file.
const STORE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Runs `update` while holding the cross-process lock beside a store file (`<file>.lock`).
/// Several processes of the host share one state root, so every change to a store is one
/// read-change-write of the FILE under this lock, never a write of a process's in-memory copy.
/// The lock also excludes other threads of this process and is not reentrant: `update` must not
/// take it again. It is held only for the file work. A lock still busy after
/// [`STORE_LOCK_WAIT`] is an error for the caller, never a skipped write.
pub(crate) fn with_store_lock<T>(
    path: &Path,
    label: &str,
    update: impl FnOnce() -> T,
) -> Result<T, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let mut options = rightkit_fs::lock::LockOptions::wait(STORE_LOCK_WAIT).labeled(label);
    // Holders keep the lock for one small file's read and write, so a short poll is cheap.
    options.poll = std::time::Duration::from_millis(10);
    rightkit_fs::lock::with_lock(Path::new(&lock_name), &options, update)
        .map_err(|error| format!("{} is locked: {error}", path.display()))
}

/// What this process last read from, or wrote to, each shared store file (as JSON): the state its
/// in-memory copy of that store was derived from. The difference between this and the copy being
/// saved is exactly what THIS process changed.
fn shared_store_bases(
) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, serde_json::Value>> {
    static BASES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, serde_json::Value>>,
    > = std::sync::OnceLock::new();
    BASES.get_or_init(Default::default)
}

fn remember_shared_store_base(path: &Path, base: Option<serde_json::Value>) {
    let mut bases = shared_store_bases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match base {
        Some(base) => {
            bases.insert(path.to_path_buf(), base);
        }
        None => {
            bases.remove(path);
        }
    }
}

/// [`load_or_set_aside`] for a store that several processes of the host write (see
/// [`write_shared_store`]): also remembers what was read, so a later save can tell what this
/// process changed.
pub(crate) fn load_shared_store<T>(path: &Path) -> Option<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let loaded = load_or_set_aside::<T>(path);
    remember_shared_store_base(
        path,
        loaded
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok()),
    );
    loaded
}

/// Saves a store that several processes of the host write, without losing the others' changes.
///
/// `value` is this process's whole in-memory copy, which holds nothing the other processes
/// wrote since it was loaded. So it is never written over the file as it is: under the
/// cross-process lock the file is read again, and only what THIS process changed since its last
/// load or save (the difference between the remembered base and `value`) is applied to what is
/// on disk. Records are matched by map key and, inside lists, by their `id`; where both sides
/// changed the same field, this process's value is written. When no other process wrote the
/// file in between, the bytes written are exactly `value`'s, as before.
///
/// A file that no longer parses is set aside first, as at load. A lock still busy after
/// [`STORE_LOCK_WAIT`] is an error and nothing is written.
pub(crate) fn write_shared_store<T>(path: &Path, label: &str, value: &T) -> Result<(), String>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let ours = serde_json::to_value(value).map_err(|e| e.to_string())?;
    with_store_lock(path, label, || {
        let base = shared_store_bases()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .cloned();
        // Read through `T`, so all three sides are in `T`'s own JSON form and compare exactly.
        let theirs =
            load_or_set_aside::<T>(path).and_then(|on_disk| serde_json::to_value(on_disk).ok());
        let merged = match theirs {
            Some(theirs) if base.as_ref() != Some(&theirs) && theirs != ours => {
                let merged = merge_own_changes(base.as_ref(), &ours, &theirs);
                match serde_json::from_value::<T>(merged) {
                    Ok(merged) => Some(merged),
                    Err(error) => {
                        tracing::warn!(file = %path.display(), %error, "changes of another process could not be merged into this store; saving this process's copy");
                        None
                    }
                }
            }
            _ => None,
        };
        let bytes = serde_json::to_vec_pretty(merged.as_ref().unwrap_or(value))
            .map_err(|e| e.to_string())?;
        write_store_file(path, &bytes).map_err(|e| e.to_string())?;
        // The in-memory copy stays as it is, so the next save's difference starts from it.
        remember_shared_store_base(path, Some(ours));
        Ok(())
    })?
}

/// Three-way merge: `theirs` (the file now) with the changes this process made (`base` to
/// `ours`) applied. A part this process did not change is taken from `theirs`; a part only this
/// process changed is taken from `ours`; objects are merged key by key and lists record by
/// record; where both changed the same leaf, `ours` is kept.
fn merge_own_changes(
    base: Option<&serde_json::Value>,
    ours: &serde_json::Value,
    theirs: &serde_json::Value,
) -> serde_json::Value {
    use serde_json::Value;
    if base == Some(ours) {
        return theirs.clone();
    }
    if base == Some(theirs) || ours == theirs {
        return ours.clone();
    }
    match (ours, theirs) {
        (Value::Object(ours), Value::Object(theirs)) => {
            let base = base.and_then(Value::as_object);
            let in_base = |key: &str| base.and_then(|base| base.get(key));
            let mut merged = serde_json::Map::new();
            for (key, our_value) in ours {
                match theirs.get(key) {
                    Some(their_value) => {
                        merged.insert(
                            key.clone(),
                            merge_own_changes(in_base(key), our_value, their_value),
                        );
                    }
                    // Not on disk. They removed it and this process left it alone: it stays
                    // removed. Otherwise this process added or changed it: it is written.
                    None => {
                        if in_base(key) != Some(our_value) {
                            merged.insert(key.clone(), our_value.clone());
                        }
                    }
                }
            }
            for (key, their_value) in theirs {
                // Only on disk: another process's entry, kept unless this process removed it.
                if !ours.contains_key(key) && in_base(key).is_none() {
                    merged.insert(key.clone(), their_value.clone());
                }
            }
            Value::Object(merged)
        }
        (Value::Array(ours), Value::Array(theirs)) => {
            let base = base
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            merge_own_list_changes(base, ours, theirs)
        }
        _ => ours.clone(),
    }
}

/// The `id` of every record in `list`, when each is an object with its own distinct string `id`.
fn record_ids(list: &[serde_json::Value]) -> Option<Vec<&str>> {
    let ids: Vec<&str> = list
        .iter()
        .map(|record| record.get("id").and_then(serde_json::Value::as_str))
        .collect::<Option<_>>()?;
    let distinct: std::collections::HashSet<&str> = ids.iter().copied().collect();
    (distinct.len() == ids.len()).then_some(ids)
}

/// [`merge_own_changes`] for lists. The result is the list on disk, in its order, without the
/// records this process removed, with the records this process changed merged in place, and with
/// the records this process added appended. Records are matched by `id` when every record has
/// one; otherwise whole elements are compared.
fn merge_own_list_changes(
    base: &[serde_json::Value],
    ours: &[serde_json::Value],
    theirs: &[serde_json::Value],
) -> serde_json::Value {
    let mut merged = Vec::with_capacity(theirs.len() + 1);
    if let (Some(base_ids), Some(our_ids), Some(their_ids)) =
        (record_ids(base), record_ids(ours), record_ids(theirs))
    {
        let by_id = |ids: &[&str], list: &[serde_json::Value], id: &str| {
            ids.iter()
                .position(|known| *known == id)
                .map(|at| list[at].clone())
        };
        for (their_record, id) in theirs.iter().zip(&their_ids) {
            let in_base = by_id(&base_ids, base, id);
            match by_id(&our_ids, ours, id) {
                Some(our_record) => {
                    merged.push(merge_own_changes(
                        in_base.as_ref(),
                        &our_record,
                        their_record,
                    ));
                }
                // Not held here: another process's record, kept unless this process removed it.
                None => {
                    if in_base.is_none() {
                        merged.push(their_record.clone());
                    }
                }
            }
        }
        for (our_record, id) in ours.iter().zip(&our_ids) {
            if their_ids.contains(id) {
                continue;
            }
            // Not on disk: written when this process added or changed it; left removed when
            // another process removed it and this one had not touched it.
            if by_id(&base_ids, base, id).as_ref() != Some(our_record) {
                merged.push(our_record.clone());
            }
        }
    } else {
        merged.extend(
            theirs
                .iter()
                .filter(|record| ours.contains(record) || !base.contains(record))
                .cloned(),
        );
        for record in ours {
            if !base.contains(record) && !merged.contains(record) {
                merged.push(record.clone());
            }
        }
    }
    serde_json::Value::Array(merged)
}

/// Truncate, write, flush. Not crash-atomic (module notes).
fn write_in_place(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(target)?;
    file.write_all(bytes)?;
    file.sync_data()
}

/// The file a write to `path` must replace: when `path` is a symlink, the file the chain of links names (even if it does
/// not exist yet, which is where `std::fs::write` created it); otherwise `path`.
fn link_target(path: &Path) -> io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::canonicalize(path) {
            Ok(resolved) => Ok(resolved),
            Err(_) => resolve_dangling(path),
        },
        _ => Ok(path.to_path_buf()),
    }
}

/// Follow a symlink chain whose end does not exist: the path it would create. A chain longer than the usual OS limit
/// (a loop) is an error.
fn resolve_dangling(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..40 {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let next = std::fs::read_link(&current)?;
                current = if next.is_absolute() {
                    next
                } else {
                    current
                        .parent()
                        .map_or(next.clone(), |parent| parent.join(&next))
                };
            }
            // The end of the chain: a regular path that does not exist (or a file that appeared meanwhile).
            _ => return Ok(current),
        }
    }
    Err(io::Error::other(format!(
        "too many levels of symbolic links: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn hard_linked(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.nlink() > 1)
}

/// The link count of a regular file, from `GetFileInformationByHandle` (`std` only has it on nightly).
#[cfg(windows)]
fn hard_linked(path: &Path) -> bool {
    if !std::fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    rightkit_fs::link_count(&file).is_ok_and(|count| count > 1)
}

#[cfg(not(any(unix, windows)))]
fn hard_linked(_path: &Path) -> bool {
    false
}

/// Moves a state file the app could not parse out of the way, so the next save cannot overwrite what an operator (or a
/// later build) could still repair: `<name>.damaged-<unix seconds>`, with `-1`, `-2`, ... when that name is taken (an
/// exclusive rename, so an earlier set-aside copy is never replaced). An empty or whitespace-only file is damaged too
/// (a torn write leaves one): it is set aside and logged like any other, never read as "no data" again and again
/// (storage contract rule 14). A missing path, or one that is not a regular file, is left alone. Returns the new path
/// when a file was set aside. Best effort: a failed rename is logged, and the caller carries on with its default.
pub(crate) fn set_aside_damaged(path: &Path) -> Option<PathBuf> {
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file()) {
        return None;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let base = path.file_name()?.to_string_lossy().into_owned();
    for attempt in 0..100u32 {
        let name = if attempt == 0 {
            format!("{base}.damaged-{stamp}")
        } else {
            format!("{base}.damaged-{stamp}-{attempt}")
        };
        let to = path.with_file_name(name);
        match rightkit_fs::rename_exclusive(path, &to) {
            Ok(()) => {
                tracing::warn!(file = %path.display(), kept_as = %to.display(), "state file could not be parsed; kept aside and starting empty");
                return Some(to);
            }
            Err(rightkit_fs::FsError::AlreadyExists(_)) => continue,
            Err(error) => {
                tracing::warn!(file = %path.display(), %error, "state file could not be parsed and could not be set aside");
                return None;
            }
        }
    }
    None
}

/// Reads and parses a state file for a loader whose fallback is a default value. `None` when the file is missing. A
/// file that exists but cannot be read, is empty, or does not parse is damaged: it is logged and set aside by
/// [`set_aside_damaged`] before `None` is returned, so the default the caller starts with is never saved over the
/// only copy of the data and is never silently empty.
pub(crate) fn load_or_set_aside<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(file = %path.display(), %error, "state file could not be read; using defaults");
            set_aside_damaged(path);
            return None;
        }
    };
    match serde_json::from_slice::<T>(&raw) {
        Ok(value) => Some(value),
        Err(_) => {
            set_aside_damaged(path);
            None
        }
    }
}

/// Removes the files the host keeps for one deleted session (storage rule 35): its
/// `<state root>/sessions/<id>` folder (shell-output recovery copies) and its
/// `<state root>/traces/<id>.jsonl` trace. Run-change records are removed through the run-change
/// store. A missing path is fine; any other failure is logged and does not undo the deletion.
/// The retained `pre-v*.bak` database backups are the documented exception.
pub(crate) fn remove_session_files(state_root: &Path, session_id: &str) {
    let folder = state_root
        .join("sessions")
        .join(rightkit_agent_coding_tools::output_compaction::session_store_dir_name(session_id));
    let trace = state_root
        .join("traces")
        .join(format!("{session_id}.jsonl"));
    for (path, is_dir) in [(folder, true), (trace, false)] {
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = result {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::warn!(file = %path.display(), %error, "file of a deleted session could not be removed");
            }
        }
    }
}

/// The instinct store at `path`: empty when the file is missing; when it exists but does not parse it is set aside
/// first (see [`set_aside_damaged`]), so the empty store is never saved over the only copy.
pub(crate) fn load_instincts(path: &Path) -> rightkit_agent_learning::InstinctStore {
    match rightkit_agent_learning::InstinctStore::load_from_json(&path.to_string_lossy()) {
        Ok(store) => store,
        Err(_) => {
            // Only a file we could read but not parse is damaged; a missing or unreadable one has nothing to keep.
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                set_aside_damaged(path);
            }
            rightkit_agent_learning::InstinctStore::default()
        }
    }
}
