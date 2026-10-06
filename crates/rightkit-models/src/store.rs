//! On-disk layout, durable state, verification, rollback and uninstall.
//!
//! ```text
//! <root>/<model-id>/state.json              active revision, history, per-revision file records
//! <root>/<model-id>/revisions/<key>/<files> one immutable dir per (revision, quant)
//! <root>/<model-id>/.staging/<key>/         resumable in-flight install (swapped in atomically)
//! <root>/<model-id>.lock                    cross-process operation lock
//! ```
//! Activation is a single atomic write of `state.json`; revision directories
//! are never edited in place, so rollback is a pointer flip.

use crate::error::{Error, ErrorCode, Result};
use crate::hash::sha256_file;
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

    pub fn model_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    pub fn revision_dir(&self, id: &str, key: &str) -> PathBuf {
        self.model_dir(id).join("revisions").join(key)
    }
    pub(crate) fn staging_dir(&self, id: &str, key: &str) -> PathBuf {
        self.model_dir(id).join(".staging").join(key)
    }

    pub fn state(&self, id: &str) -> Result<State> {
        let path = self.model_dir(id).join("state.json");
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::io(path.display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State { id: id.into(), ..State::default() }),
            Err(e) => Err(Error::io(path.display(), e)),
        }
    }

    pub(crate) fn save_state(&self, state: &State) -> Result<()> {
        let path = self.model_dir(&state.id).join("state.json");
        let json = serde_json::to_vec_pretty(state).map_err(|e| Error::io("serialize state", e))?;
        write_atomic(&path, &json)
    }

    pub(crate) fn acquire(&self, id: &str, wait: Duration) -> Result<StoreLock> {
        fs::create_dir_all(&self.root).map_err(|e| Error::io(self.root.display(), e))?;
        lock::lock_timeout(&self.model_dir(id), &self.app, wait)
            .map_err(|e| Error::new(ErrorCode::Io, e))?
            .ok_or_else(|| Error::new(ErrorCode::Busy, format!("another operation holds {id}")))
    }

    /// True when every recorded file of `key` passes `mode`.
    pub fn verify(&self, id: &str, key: &str, mode: VerifyMode) -> bool {
        let Ok(state) = self.state(id) else { return false };
        let Some(rec) = state.revisions.get(key) else { return false };
        let dir = self.revision_dir(id, key);
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

    /// Directory of the active revision if it passes a size check.
    pub fn active_dir(&self, id: &str) -> Option<PathBuf> {
        let key = self.state(id).ok()?.active?;
        self.verify(id, &key, VerifyMode::Size).then(|| self.revision_dir(id, &key))
    }

    /// Re-point `active` at an earlier revision (default: the most recent
    /// history entry that still verifies). Returns the new active key.
    pub fn rollback(&self, id: &str, target: Option<&str>) -> Result<String> {
        let _lock = self.acquire(id, Duration::ZERO)?;
        let mut state = self.state(id)?;
        let current = state.active.clone();
        let target = match target {
            Some(t) => t.to_string(),
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
        let _lock = self.acquire(id, Duration::ZERO)?;
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
        let dir = self.revision_dir(id, key);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| Error::io(dir.display(), e))?;
        }
        let _ = fs::remove_dir_all(self.staging_dir(id, key));
        Ok(())
    }

    fn verify_record(&self, state: &State, id: &str, key: &str) -> bool {
        state.revisions.get(key).is_some_and(|r| {
            let dir = self.revision_dir(id, key);
            !r.files.is_empty()
                && r.files.iter().all(|f| fs::metadata(dir.join(&f.path)).is_ok_and(|m| m.len() == f.bytes))
        })
    }

    /// Remove the whole model: every revision, staging, and state.
    pub fn uninstall(&self, id: &str) -> Result<()> {
        let _lock = self.acquire(id, Duration::ZERO)?;
        let dir = self.model_dir(id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| Error::io(dir.display(), e))?;
        }
        Ok(())
    }
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
