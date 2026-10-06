//! On-disk layout, durable state, verification, rollback and uninstall.
//!
//! ```text
//! <root>/<model-id>/state.json              active revision, history, per-revision file records
//! <root>/<model-id>/revisions/<key>/<files> one immutable dir per (revision, quant)
//! <root>/<model-id>/.staging/<key>/         resumable in-flight install (swapped in atomically)
//! <root>/<model-id>/.aside/<key>/          previous same-key revision while a replacement commits (crash-recoverable)
//! <root>/<model-id>.lock                    cross-process operation lock
//! ```
//! Activation is a single atomic write of `state.json`; revision directories
//! are never edited in place, so rollback is a pointer flip.

use crate::error::{Error, ErrorCode, Result};
use crate::hash::sha256_file;
use crate::ids::{ModelId, RevisionKey};
use crate::lock::{self, StoreLock};
use crate::spec::safe_rel_path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRecord {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionRecord {
    pub revision: String,
    pub quant: Option<String>,
    pub installed_at_unix: u64,
    pub files: Vec<FileRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub active: Option<String>,
    /// Previously active keys, most recent last.
    pub history: Vec<String>,
    pub revisions: BTreeMap<String, RevisionRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Exists and non-empty.
    Presence,
    /// Presence plus recorded size (fast path for every launch).
    Size,
    /// Size plus sha256 (status / repair).
    Full,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
    /// Identifies the lock holder (an app name).
    pub app: String,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>, app: impl Into<String>) -> Self {
        Self { root: root.into(), app: app.into() }
    }

    /// `<root>/<id>`; fails for any id that is not a single safe path component.
    pub fn model_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.root.join(ModelId::parse(id)?.as_str()))
    }
    pub fn revision_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        Ok(self.model_dir(id)?.join("revisions").join(RevisionKey::parse(key)?.as_str()))
    }
    pub(crate) fn staging_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        Ok(self.model_dir(id)?.join(".staging").join(RevisionKey::parse(key)?.as_str()))
    }
    /// Where the previous same-key revision waits while a replacement commits.
    pub(crate) fn aside_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        Ok(self.model_dir(id)?.join(".aside").join(RevisionKey::parse(key)?.as_str()))
    }

    pub fn state(&self, id: &str) -> Result<State> {
        let path = self.model_dir(id)?.join("state.json");
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::io(path.display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State { id: id.into(), ..State::default() }),
            Err(e) => Err(Error::io(path.display(), e)),
        }
    }

    pub(crate) fn save_state(&self, state: &State) -> Result<()> {
        self.reject_links(&state.id)?;
        let path = self.model_dir(&state.id)?.join("state.json");
        let json = serde_json::to_vec_pretty(state).map_err(|e| Error::io("serialize state", e))?;
        write_atomic(&path, &json)
    }

    /// Take the cross-process lock for `id`, then finish any interrupted
    /// same-key swap before the caller mutates anything.
    pub(crate) fn acquire(&self, id: &str, wait: Duration) -> Result<StoreLock> {
        let lock = self.acquire_raw(id, wait)?;
        self.recover_locked(id)?;
        Ok(lock)
    }

    pub(crate) fn acquire_raw(&self, id: &str, wait: Duration) -> Result<StoreLock> {
        let dir = self.model_dir(id)?;
        fs::create_dir_all(&self.root).map_err(|e| Error::io(self.root.display(), e))?;
        let lock = lock::lock_timeout(&dir, &self.app, wait)
            .map_err(|e| Error::new(ErrorCode::Io, e))?
            .ok_or_else(|| Error::new(ErrorCode::Busy, format!("another operation holds {id}")))?;
        Ok(lock)
    }

    /// Refuse to operate through symlinks planted at any directory the store
    /// owns under `<root>/<id>` (a link there would redirect writes/deletes).
    pub(crate) fn reject_links(&self, id: &str) -> Result<()> {
        let dir = self.model_dir(id)?;
        for p in [
            dir.clone(),
            dir.join("revisions"),
            dir.join(".staging"),
            dir.join(".aside"),
            dir.join("state.json"),
        ] {
            if is_link(&p) {
                return Err(Error::new(ErrorCode::Io, format!("refusing to follow symlink at {}", p.display())));
            }
        }
        Ok(())
    }

    /// `<root>/refs`: the per-app reference registry shared with the suite
    /// layout ([`crate::suite::refs`]).
    pub fn refs_dir(&self) -> PathBuf {
        crate::suite::paths::model_refs_dir_at(&self.root)
    }

    /// Record that `app` uses model `id` so [`Store::gc`] and other apps'
    /// uninstalls leave it alone. Idempotent.
    pub fn register_app(&self, app: &str, id: &str) -> Result<()> {
        let id = ModelId::parse(id)?;
        crate::suite::refs::register(&self.refs_dir(), app, id.as_str())
            .map_err(|e| Error::new(ErrorCode::Io, e))
    }

    /// Drop `app`'s references; returns model ids no app references any more.
    /// A corrupt ref file aborts without changing anything.
    pub fn unregister_app(&self, app: &str) -> Result<Vec<String>> {
        crate::suite::refs::unregister(&self.refs_dir(), app)
            .map_err(|e| Error::new(ErrorCode::Io, e))
    }

    /// Remove every model no app references, skipping `keep`, anything under a
    /// live install lock, and everything while any runtime-family lease is
    /// held. A corrupt ref blocks deletion (nothing is removed).
    pub fn gc(&self, keep: &[&str]) -> Result<Vec<String>> {
        crate::suite::store::prune_unref_entries_with(&self.root, keep, &[])
            .map_err(|e| Error::new(ErrorCode::Io, e))
    }

    /// As [`Store::uninstall`], but refuses with `Busy` while any app still
    /// references the model.
    pub fn uninstall_unreferenced(&self, id: &str) -> Result<()> {
        let id = ModelId::parse(id)?;
        let _guard = crate::suite::refs::acquire_registry(&self.refs_dir())
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        let referenced = crate::suite::refs::referenced_ids_locked(&self.refs_dir())
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        if referenced.contains(id.as_str()) {
            return Err(Error::new(ErrorCode::Busy, format!("{id} is referenced by another app")));
        }
        self.uninstall(id.as_str())
    }

    /// Public entry for crash recovery: finishes an interrupted same-key swap
    /// for `id` (see [`Store::install`]). Safe to call at startup.
    pub fn recover(&self, id: &str) -> Result<bool> {
        let _lock = self.acquire_raw(id, Duration::ZERO)?;
        self.recover_locked(id)
    }

    /// True when every recorded file of `key` passes `mode`.
    pub fn verify(&self, id: &str, key: &str, mode: VerifyMode) -> bool {
        let Ok(state) = self.state(id) else { return false };
        let Ok(dir) = self.revision_dir(id, key) else { return false };
        let Some(rec) = state.revisions.get(key) else { return false };
        record_matches(rec, &dir, mode)
    }

    /// Directory of the active revision if it passes a size check.
    pub fn active_dir(&self, id: &str) -> Option<PathBuf> {
        let key = self.state(id).ok()?.active?;
        if self.reject_links(id).is_err() {
            return None;
        }
        self.verify(id, &key, VerifyMode::Size).then(|| self.revision_dir(id, &key).ok()).flatten()
    }

    /// Re-point `active` at an earlier revision (default: the most recent
    /// history entry that still verifies). Returns the new active key.
    pub fn rollback(&self, id: &str, target: Option<&str>) -> Result<String> {
        let _lock = self.acquire(id, Duration::ZERO)?;
        self.reject_links(id)?;
        let mut state = self.state(id)?;
        let current = state.active.clone();
        let target = match target {
            Some(t) => RevisionKey::parse(t)?.as_str().to_string(),
            None => state
                .history
                .iter()
                .rev()
                .find(|k| Some(*k) != current.as_ref() && self.verify(id, k, VerifyMode::Size))
                .cloned()
                .ok_or_else(|| Error::new(ErrorCode::NoRollback, "no earlier verified revision"))?,
        };
        if !self.verify(id, &target, VerifyMode::Size) {
            return Err(Error::new(ErrorCode::NoRollback, format!("revision {target} is not installed or fails verification")));
        }
        if let Some(c) = current.filter(|c| *c != target) {
            state.history.retain(|k| *k != target);
            state.history.push(c);
        }
        state.active = Some(target.clone());
        self.save_state(&state)?;
        Ok(target)
    }

    /// Remove one revision. If it was active, fall back to the newest
    /// verified history entry (or none).
    pub fn uninstall_revision(&self, id: &str, key: &str) -> Result<()> {
        let key = RevisionKey::parse(key)?.as_str().to_string();
        let key = key.as_str();
        let _lock = self.acquire(id, Duration::ZERO)?;
        self.reject_links(id)?;
        let mut state = self.state(id)?;
        if state.revisions.remove(key).is_none() {
            return Err(Error::new(ErrorCode::NotFound, format!("{id} has no revision {key}")));
        }
        state.history.retain(|k| k != key);
        if state.active.as_deref() == Some(key) {
            state.active = state.history.iter().rev().find(|k| self.verify_record(&state, id, k)).cloned();
            if let Some(a) = &state.active {
                state.history.retain(|k| k != a);
            }
        }
        self.save_state(&state)?;
        remove_tree(&self.revision_dir(id, key)?)?;
        let _ = remove_tree(&self.staging_dir(id, key)?);
        Ok(())
    }

    fn verify_record(&self, state: &State, id: &str, key: &str) -> bool {
        let Ok(dir) = self.revision_dir(id, key) else { return false };
        state.revisions.get(key).is_some_and(|r| record_matches(r, &dir, VerifyMode::Size))
    }

    /// Remove the whole model: every revision, staging, and state. A symlink at
    /// `<root>/<id>` is unlinked, never followed.
    pub fn uninstall(&self, id: &str) -> Result<()> {
        let _lock = self.acquire_raw(id, Duration::ZERO)?;
        remove_tree(&self.model_dir(id)?)
    }

    /// Finish an interrupted same-key swap (see `install`): the previous
    /// revision directory waits in `.aside/<key>` while the replacement commits.
    /// - `revisions/<key>` missing: the swap died between the two renames; put
    ///   the previous revision back.
    /// - present and matching the recorded files: the swap committed; drop the aside.
    /// - present but not matching while the aside does: the replacement was
    ///   renamed in but never recorded; restore the aside the ledger still names.
    ///
    /// Caller holds the model lock. Returns whether anything was restored.
    pub(crate) fn recover_locked(&self, id: &str) -> Result<bool> {
        self.reject_links(id)?;
        let aside_root = self.model_dir(id)?.join(".aside");
        let Ok(entries) = fs::read_dir(&aside_root) else { return Ok(false) };
        let state = self.state(id)?;
        let mut restored = false;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(key) = name.to_str().and_then(|n| RevisionKey::parse(n).ok()) else {
                let _ = remove_tree(&entry.path());
                continue;
            };
            let aside = entry.path();
            let dest = self.revision_dir(id, key.as_str())?;
            let rec = state.revisions.get(key.as_str());
            let restore = if !dest.exists() {
                true
            } else {
                let dest_ok = rec.is_some_and(|r| record_matches(r, &dest, VerifyMode::Size));
                let aside_ok = rec.is_some_and(|r| record_matches(r, &aside, VerifyMode::Size));
                !dest_ok && aside_ok
            };
            if restore {
                remove_tree(&dest)?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
                }
                fs::rename(&aside, &dest).map_err(|e| Error::io(format!("restore {}", dest.display()), e))?;
                restored = true;
            } else {
                remove_tree(&aside)?;
            }
        }
        let _ = fs::remove_dir(&aside_root);
        Ok(restored)
    }
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// Delete `path` without ever following a symlink: a link (at the root of the
/// removal) is unlinked; real trees go through `remove_dir_all`, which does
/// not traverse links either. Missing paths are fine.
pub(crate) fn remove_tree(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::io(path.display(), e)),
    };
    let result = if meta.file_type().is_symlink() {
        fs::remove_file(path).or_else(|_| fs::remove_dir(path))
    } else if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|e| Error::io(path.display(), e))
}

/// Do the files recorded in `rec` exist under `dir` and pass `mode`?
pub(crate) fn record_matches(rec: &RevisionRecord, dir: &Path, mode: VerifyMode) -> bool {
    !rec.files.is_empty()
        && rec.files.iter().all(|f| {
            let Some(rel) = safe_rel_path(&f.path) else { return false };
            let p = dir.join(rel);
            let Ok(meta) = fs::metadata(&p) else { return false };
            if !meta.is_file() || meta.len() == 0 {
                return false;
            }
            if mode != VerifyMode::Presence && meta.len() != f.bytes {
                return false;
            }
            mode != VerifyMode::Full || sha256_file(&p).is_ok_and(|h| h.eq_ignore_ascii_case(&f.sha256))
        })
}

/// Durable replacement: private temp file + fsync + rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| Error::new(ErrorCode::Io, "atomic write needs a parent"))?;
    fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if cfg!(windows) && path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(path.display(), e));
    }
    Ok(())
}
