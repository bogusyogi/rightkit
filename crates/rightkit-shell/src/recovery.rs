//! Setup recovery (crash-safe session snapshot) and startup cache repair.
//!
//! Donor: ViewRight `setup_recovery.rs` and `startup_cache.rs`.
//!
//! **Recovery.** The app periodically saves a JSON snapshot of its open work; after a crash it loads
//! it back. The write is atomic (`.new` written and synced, previous file kept as `.old` until the
//! swap succeeds), validated (`version`, size cap) and *authority-checked*: every file path named by
//! the snapshot must already be granted by the [`PathBroker`], so a snapshot can never smuggle in a
//! path the person did not open. Loading re-grants the recovered paths (they were authorised when
//! saved). `untitled:` entries have no file and are skipped.
//!
//! **Cache repair.** A dangling symlink or a stray file where a cache directory belongs makes
//! `create_dir_all` fail with `AlreadyExists`, which aborts plugins that expect the directory
//! (`tauri-plugin-http` cookie cache). Cache content is disposable, so [`ensure_cache_dir`] replaces
//! the bad entry with a real directory. The shell runs it for the app cache dir at startup.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::broker::PathBroker;
use crate::error::ShellError;

/// What a recovery store accepts and how it finds file paths inside a snapshot.
#[derive(Clone)]
pub struct RecoverySpec {
    /// File stem, e.g. `setup-recovery`; the file is `<state>/recovery/<name>-v<version>.json`.
    pub name: String,
    pub version: u64,
    pub max_bytes: usize,
    /// File paths named by a parsed snapshot (anything starting `untitled:` is ignored).
    pub paths: Arc<dyn Fn(&Value) -> Vec<String> + Send + Sync>,
}

impl RecoverySpec {
    /// ViewRight's snapshot shape: `{"version":1,"documents":[{"document":{"path":..}}]}`.
    pub fn documents() -> Self {
        Self {
            name: "setup-recovery".into(),
            version: 1,
            max_bytes: 256 * 1024 * 1024,
            paths: Arc::new(|v| {
                v["documents"]
                    .as_array()
                    .map(|docs| {
                        docs.iter()
                            .filter_map(|d| d["document"]["path"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            }),
        }
    }
}

pub struct RecoveryStore {
    spec: RecoverySpec,
    path: PathBuf,
}

impl RecoveryStore {
    pub fn new(spec: RecoverySpec, state_dir: &Path) -> Self {
        let path = state_dir
            .join("recovery")
            .join(format!("{}-v{}.json", spec.name, spec.version));
        Self { spec, path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn validate(&self, snapshot: &str) -> Result<Value, ShellError> {
        if snapshot.len() > self.spec.max_bytes {
            return Err(ShellError::DocumentTooLarge(
                "setup recovery is too large".into(),
            ));
        }
        let parsed: Value = serde_json::from_str(snapshot)
            .map_err(|e| ShellError::Invalid(format!("invalid setup recovery: {e}")))?;
        if parsed["version"].as_u64() != Some(self.spec.version) {
            return Err(ShellError::Invalid(
                "unsupported setup recovery version".into(),
            ));
        }
        Ok(parsed)
    }

    fn file_paths(&self, parsed: &Value) -> Vec<String> {
        (self.spec.paths)(parsed)
            .into_iter()
            .filter(|p| !p.starts_with("untitled:"))
            .collect()
    }

    pub fn save(&self, snapshot: &str, broker: &PathBroker) -> Result<(), ShellError> {
        let parsed = self.validate(snapshot)?;
        for p in self.file_paths(&parsed) {
            let normalized = crate::broker::normalize_existing_file(&p)?;
            if !broker.is_granted_normalized(&normalized)? {
                return Err(ShellError::BrokerDenied(
                    "setup recovery path is not authorized".into(),
                ));
            }
        }
        write_atomic(&self.path, snapshot)
    }

    /// The saved snapshot, if any. Recovered file paths are re-granted (best effort: a file deleted
    /// since the crash simply is not granted).
    pub fn load(&self, broker: &PathBroker) -> Result<Option<String>, ShellError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let parsed = self.validate(&raw)?;
        for p in self.file_paths(&parsed) {
            let _ = broker.grant_file(&p);
        }
        Ok(Some(raw))
    }

    pub fn clear(&self) -> Result<(), ShellError> {
        for candidate in [
            self.path.clone(),
            self.path.with_extension("json.new"),
            self.path.with_extension("json.old"),
        ] {
            match fs::remove_file(candidate) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

fn write_atomic(path: &Path, content: &str) -> Result<(), ShellError> {
    let parent = path
        .parent()
        .ok_or_else(|| ShellError::Invalid("setup recovery path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let (new, old) = (
        path.with_extension("json.new"),
        path.with_extension("json.old"),
    );
    let _ = fs::remove_file(&new);
    let _ = fs::remove_file(&old);
    let mut f = OpenOptions::new().create_new(true).write(true).open(&new)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()?;
    drop(f);
    if path.exists() {
        fs::rename(path, &old)?;
    }
    if let Err(e) = fs::rename(&new, path) {
        if old.exists() {
            let _ = fs::rename(&old, path);
        }
        return Err(e.into());
    }
    let _ = fs::remove_file(old);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDirOutcome {
    Existing,
    Created,
    Repaired,
}

/// Make `path` a usable directory, replacing a dangling symlink or a regular file.
pub fn ensure_cache_dir(path: &Path) -> std::io::Result<CacheDirOutcome> {
    let mut repaired = false;
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => return Ok(CacheDirOutcome::Existing),
        Ok(m) if m.file_type().is_symlink() => {
            if fs::metadata(path).is_ok_and(|t| t.is_dir()) {
                return Ok(CacheDirOutcome::Existing);
            }
            fs::remove_file(path)?;
            repaired = true;
        }
        Ok(_) => {
            fs::remove_file(path)?;
            repaired = true;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::create_dir_all(path)?;
    Ok(if repaired {
        CacheDirOutcome::Repaired
    } else {
        CacheDirOutcome::Created
    })
}
