//! Checked migration primitives shared by desktop controllers.
//!
//! This module deliberately does not move files. The store/controller owns
//! transaction sequencing; these types keep journal phases and source checks
//! serializable across RPC boundaries.

use crate::suite::catalog::ModelSpec;
use crate::suite::manifest::{self, VerifyMode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
pub const JOURNAL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JournalPhase {
    Planned,
    SourceVerified,
    Staged,
    DestinationVerified,
    SourceAside,
    AliasCreated,
    Complete,
    Rollback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationJournal {
    pub version: u32,
    pub model_id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    #[serde(default)]
    pub staging: Option<PathBuf>,
    #[serde(default)]
    pub aside: Option<PathBuf>,
    #[serde(default)]
    pub backup: Option<PathBuf>,
    #[serde(default)]
    pub destination_created: bool,
    /// Write-ahead intent: source payload will be renamed into staging/destination.
    /// Missing in older copy-only journals.
    #[serde(default)]
    pub source_moved: bool,
    pub phase: JournalPhase,
}

pub fn journal_path(models_root: &Path, model_id: &str) -> PathBuf {
    models_root
        .join(".migration")
        .join(format!("{model_id}.json"))
}

pub fn write_journal(path: &Path, journal: &MigrationJournal) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(journal)
        .map_err(|error| format!("serialize journal: {error}"))?;
    manifest::write_atomic(path, &bytes)
}

pub fn read_journal(path: &Path) -> Result<MigrationJournal, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("read journal {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse journal {}: {error}", path.display()))
}

/// Validate a legacy source against immutable full catalog pins before any
/// move. Extra files are intentionally allowed and remain part of the copy.
pub fn verify_trusted_source(source: &Path, spec: &ModelSpec) -> Result<(), String> {
    if !source.is_dir() {
        return Err(format!(
            "migration source is not a directory: {}",
            source.display()
        ));
    }
    let report = manifest::verify_dir(source, &spec.expected_manifest(), VerifyMode::Full);
    if report.is_ready() {
        Ok(())
    } else {
        Err(format!(
            "migration source failed trusted verification: {}",
            report
                .needed()
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

/// Validate source pins, then let controller perform an actual native load
/// check against its published destination path.
pub fn verify_source_checked(
    source: &Path,
    spec: &ModelSpec,
    published_destination: &Path,
    load_check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    verify_trusted_source(source, spec)?;
    load_check(published_destination)
}
