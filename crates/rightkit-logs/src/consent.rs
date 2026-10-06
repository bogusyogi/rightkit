//! Persistent, file-backed upload consent.
//!
//! Nothing leaves the device unless the user has granted consent. The state is
//! re-read from disk on every check, so a revocation made by one process (the
//! settings UI, a CLI) is honored immediately by every other process.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
}

impl ConsentStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current state. A missing, unreadable or corrupt file is `Unset`:
    /// failure always resolves to "do not upload".
    pub fn status(&self) -> Consent {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Consent::Unset)
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
