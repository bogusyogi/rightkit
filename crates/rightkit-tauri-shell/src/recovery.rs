//! Setup recovery: a crash-safe session snapshot.
//!
//! Donor: ViewRight `setup_recovery.rs`. The app periodically saves a JSON snapshot of its open
//! work; after a crash it loads it back. The write is atomic (`.new` written and synced, previous
//! file kept as `.old` until the swap succeeds), validated (`version`, size cap) and
//! *authority-checked*: every file path the snapshot names must already be granted by the
//! [`PathBroker`], so a snapshot can never smuggle in a path the person did not open. Loading
//! re-grants the recovered paths. `untitled:` entries have no file and are skipped.
//!
//! The snapshot file is `<state>/recovery/<name>-v<version>.json` unless the spec pins an exact
//! path with [`RecoverySpec::at_path`]. An app that moved its snapshot can migrate the old file once
//! with [`RecoveryStore::migrate_legacy`].
//!
//! Cache-directory repair moved to [`crate::startup_cache`]; it is re-exported here so existing
//! paths keep working.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::broker::PathBroker;
use crate::error::ShellError;

pub use crate::startup_cache::{ensure_cache_dir, CacheDirOutcome};

/// Lists the file paths a parsed snapshot names.
pub type PathExtractor = Arc<dyn Fn(&Value) -> Vec<String> + Send + Sync>;

/// What a recovery store accepts and how it finds file paths inside a snapshot.
#[derive(Clone)]
pub struct RecoverySpec {
    /// File stem, e.g. `setup-recovery`; the default file is `<state>/recovery/<name>-v<version>.json`.
    pub name: String,
    pub version: u64,
    pub max_bytes: usize,
    /// File paths named by a parsed snapshot (anything starting `untitled:` is ignored).
    pub paths: PathExtractor,
    /// Exact snapshot file, used instead of the default location when set.
    pub exact_path: Option<PathBuf>,
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
            exact_path: None,
        }
    }

    /// Store the snapshot at exactly `path` instead of the default location.
    pub fn at_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.exact_path = Some(path.into());
        self
    }
}

/// Result of [`RecoveryStore::migrate_legacy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationOutcome {
    /// No legacy snapshot exists; nothing to do.
    NoLegacy,
    /// The current snapshot already exists, so the legacy file was left untouched.
    AlreadyCurrent,
    /// The legacy snapshot was validated, written to the current path and removed.
    Migrated,
}

pub struct RecoveryStore {
    spec: RecoverySpec,
    path: PathBuf,
}

impl RecoveryStore {
    pub fn new(spec: RecoverySpec, state_dir: &Path) -> Self {
        let path = match &spec.exact_path {
            Some(exact) => exact.clone(),
            None => state_dir
                .join("recovery")
                .join(format!("{}-v{}.json", spec.name, spec.version)),
        };
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

    /// One-time migration of a snapshot an earlier version wrote to `legacy`.
    ///
    /// The legacy file is validated (size and version) and copied atomically to the current path,
    /// then removed. Authority is not re-checked here: [`RecoveryStore::save`] and
    /// [`RecoveryStore::load`] still apply it. An invalid legacy file is an error and is left in
    /// place. If the current snapshot already exists, the legacy file is left alone.
    pub fn migrate_legacy(&self, legacy: &Path) -> Result<MigrationOutcome, ShellError> {
        let raw = match fs::read_to_string(legacy) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MigrationOutcome::NoLegacy)
            }
            Err(e) => return Err(e.into()),
        };
        if self.path.exists() {
            return Ok(MigrationOutcome::AlreadyCurrent);
        }
        self.validate(&raw)?;
        write_atomic(&self.path, &raw)?;
        let _ = fs::remove_file(legacy);
        Ok(MigrationOutcome::Migrated)
    }

    pub fn clear(&self) -> Result<(), ShellError> {
        for candidate in [
            self.path.clone(),
            sidecar(&self.path, "new"),
            sidecar(&self.path, "old"),
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

/// `<path>.<suffix>`, appended to the whole file name (so `x.json` gives `x.json.new`).
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

fn write_atomic(path: &Path, content: &str) -> Result<(), ShellError> {
    let parent = path
        .parent()
        .ok_or_else(|| ShellError::Invalid("setup recovery path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let (new, old) = (sidecar(path, "new"), sidecar(path, "old"));
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
