//! Persistent, file-backed upload consent.
//!
//! Nothing leaves the device unless the user has granted consent. The state is
//! re-read from disk on every check, so a revocation made by one process (the
//! settings UI, a CLI) is honored immediately by every other process.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Consent {
    /// The user has not been asked or has not answered. Treated as "no".
    Unset,
    Granted {
        at_ms: i64,
        /// App-defined version of the wording the user agreed to. A newer
        /// required version makes an older grant invalid via `is_granted_for`.
        policy_version: String,
    },
    Revoked {
        at_ms: i64,
    },
}

impl Consent {
    pub fn is_granted(&self) -> bool {
        matches!(self, Consent::Granted { .. })
    }

    pub fn is_granted_for(&self, required_policy_version: &str) -> bool {
        matches!(self, Consent::Granted { policy_version, .. } if policy_version == required_policy_version)
    }
}

#[derive(Debug, Clone)]
pub struct ConsentStore {
    path: PathBuf,
    /// Message of the last `status` read that failed for a reason other than the file being
    /// absent; cleared by the next successful read. Shared between clones.
    last_read_error: Arc<Mutex<Option<String>>>,
}

impl ConsentStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_read_error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current state. A missing, unreadable or corrupt file is `Unset`:
    /// failure always resolves to "do not upload".
    ///
    /// An absent file is the ordinary "user has not chosen" case. Any other failure (the file
    /// could not be read, or holds no valid state) still returns `Unset` but is no longer
    /// silent: its message is kept for `last_read_error`, and `try_status` returns it directly.
    pub fn status(&self) -> Consent {
        let (consent, error) = match self.try_status() {
            Ok(consent) => (consent, None),
            Err(error) => (Consent::Unset, Some(error.to_string())),
        };
        *self
            .last_read_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = error;
        consent
    }

    /// Like `status`, but a file that cannot be read or parsed is an error instead of `Unset`.
    /// An absent file (`NotFound`) is `Ok(Consent::Unset)`. A file that was read but holds no
    /// valid state is `ErrorKind::InvalidData`. Callers that treat an error as "no consent" get
    /// the same result as `status`.
    pub fn try_status(&self) -> io::Result<Consent> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Consent::Unset),
            Err(error) => return Err(error),
        };
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// Why the most recent `status` call returned `Unset` for a reason other than the file
    /// being absent (a read error, or content that is not valid consent state). `None` when
    /// that call read the file, or found it absent. This tells "could not read consent" apart
    /// from "the user never chose".
    pub fn last_read_error(&self) -> Option<String> {
        self.last_read_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn is_granted(&self) -> bool {
        self.status().is_granted()
    }

    pub fn grant(&self, policy_version: &str) -> io::Result<()> {
        self.write(&Consent::Granted {
            at_ms: crate::now_ms(),
            policy_version: policy_version.to_string(),
        })
    }

    pub fn revoke(&self) -> io::Result<()> {
        self.write(&Consent::Revoked {
            at_ms: crate::now_ms(),
        })
    }

    fn write(&self, state: &Consent) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        crate::atomic::write_atomic(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_file_is_unset_without_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConsentStore::new(dir.path().join("consent.json"));
        assert_eq!(store.try_status().unwrap(), Consent::Unset);
        assert_eq!(store.status(), Consent::Unset);
        assert_eq!(store.last_read_error(), None);
    }

    #[test]
    fn granted_state_reads_back_and_clears_a_prior_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        fs::write(&path, b"{").unwrap();
        let store = ConsentStore::new(&path);
        assert_eq!(store.status(), Consent::Unset);
        assert!(store.last_read_error().is_some());
        store.grant("2026-10").unwrap();
        assert!(store.status().is_granted_for("2026-10"));
        assert_eq!(store.last_read_error(), None);
    }

    /// A directory at the path makes the read fail with an error that is not `NotFound`
    /// (`IsADirectory` on Unix, `PermissionDenied` on Windows) on every platform.
    #[test]
    fn unreadable_file_stays_unset_but_is_observable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        fs::create_dir(&path).unwrap();
        let store = ConsentStore::new(&path);
        let error = store
            .try_status()
            .expect_err("a read error must be an error");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(store.status(), Consent::Unset);
        assert!(!store.is_granted());
        assert!(store.last_read_error().is_some());
        assert!(store.clone().last_read_error().is_some(), "clones share it");
    }

    #[test]
    fn corrupt_file_is_invalid_data_and_unset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        fs::write(&path, b"not json").unwrap();
        let store = ConsentStore::new(&path);
        assert_eq!(
            store.try_status().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(store.status(), Consent::Unset);
    }
}
