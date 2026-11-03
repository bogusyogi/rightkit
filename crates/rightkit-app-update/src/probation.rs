//! Failed-boot detection after an update: one sentinel file and the BootStage probation state
//! machine (decision D1 of #771: no payload backup, no rollback helper, no `.previous` copy).
//!
//! The app calls [`BootProbation::begin`] right before it relaunches into an installed update,
//! [`BootProbation::evaluate_boot`] once at every start with the running version,
//! [`BootProbation::report_stage`] as the new version passes its boot stages (a `boot_stage`
//! command), and [`BootProbation::acknowledge_healthy`] once it is healthy (a `boot_ok`
//! command). A version that starts more than `max_boot_attempts` times without an
//! acknowledgement is declared failed: the sentinel is cleared, the decision is
//! [`BootDecision::ReinstallRequired`] and the app tells the person to reinstall the release
//! they came from. Nothing is restored here.
//!
//! The sentinel is one JSON file written atomically ([`rightkit_fs::atomic::atomic_write`]) with
//! camelCase fields, the same record `@rightkit/app-update`'s `createBootProbation` keeps, so a
//! renderer-side and a native owner read one file. The file holds versions, a counter, an
//! optional fixed install path and the last stage name, never a payload path or a secret.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a failed boot tells the person to do.
pub const GUIDANCE_REINSTALL: &str = "reinstall";

/// The suffix of the retired `.previous` rollback copy; a sentinel may not name one.
const PREVIOUS_SUFFIX: &str = ".previous";

/// Why a probation call failed.
#[derive(Debug, thiserror::Error)]
pub enum ProbationError {
    #[error("boot probation input refused: {0}")]
    Invalid(String),
    #[error("boot sentinel io: {0}")]
    Io(#[from] io::Error),
    #[error("boot sentinel write: {0}")]
    Write(#[from] rightkit_fs::FsError),
}

/// The sentinel record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sentinel {
    pub from_version: String,
    pub to_version: String,
    /// Starts of `to_version` seen without a healthy acknowledgement.
    pub boot_attempts: u32,
    /// The fixed install path the update replaced in place (context only, never restored from).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_path: Option<String>,
    /// The last boot stage the new version reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stage: Option<String>,
}

/// A version that never acknowledged within its allowed starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootFailure {
    pub failed_version: String,
    /// The last good version: the release to reinstall.
    pub from_version: String,
    pub boot_attempts: u32,
    pub install_path: Option<String>,
    /// How far the failed boot got, when it reported any stage.
    pub last_stage: Option<String>,
}

impl BootFailure {
    pub fn guidance(&self) -> &'static str {
        GUIDANCE_REINSTALL
    }
}

/// What [`BootProbation::evaluate_boot`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootDecision {
    /// No update is on probation.
    None,
    /// The running version is on probation; acknowledge once it is healthy.
    Probation {
        to_version: String,
        boot_attempts: u32,
    },
    /// The version never acknowledged within the allowed starts; show reinstall guidance.
    ReinstallRequired(BootFailure),
    /// The sentinel named another version (superseded) or could not be read; it was cleared.
    Stale,
}

/// The probation state machine over one sentinel file.
#[derive(Clone, Debug)]
pub struct BootProbation {
    path: PathBuf,
    max_boot_attempts: u32,
}

/// Default starts tolerated without an acknowledgement.
pub const DEFAULT_MAX_BOOT_ATTEMPTS: u32 = 2;

fn version_text(what: &str, value: &str) -> Result<(), ProbationError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ProbationError::Invalid(format!(
            "{what} is 1 to 256 characters with no control characters"
        )));
    }
    Ok(())
}

fn stage_text(stage: &str) -> Result<(), ProbationError> {
    if stage.is_empty()
        || stage.len() > 64
        || !stage
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(ProbationError::Invalid(
            "a boot stage is 1 to 64 letters, digits, `_`, `-` or `.`".into(),
        ));
    }
    Ok(())
}

/// The fixed install path rule of the retired rollback: non-empty, not a root, not a
/// `.previous` copy.
fn install_path_text(path: &str) -> Result<(), ProbationError> {
    version_text("install path", path)?;
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Err(ProbationError::Invalid(
            "install path must not be a filesystem root".into(),
        ));
    }
    if trimmed.ends_with(PREVIOUS_SUFFIX) {
        return Err(ProbationError::Invalid(
            "install path must be the fixed install path, not a .previous copy".into(),
        ));
    }
    Ok(())
}

impl BootProbation {
    /// Probation over the sentinel file at `path`, tolerating
    /// [`DEFAULT_MAX_BOOT_ATTEMPTS`] unacknowledged starts.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_boot_attempts: DEFAULT_MAX_BOOT_ATTEMPTS,
        }
    }

    /// Tolerate `attempts` (at least 1) unacknowledged starts before a boot is declared failed.
    pub fn max_boot_attempts(mut self, attempts: u32) -> Result<Self, ProbationError> {
        if attempts < 1 {
            return Err(ProbationError::Invalid(
                "max boot attempts must be a positive integer".into(),
            ));
        }
        self.max_boot_attempts = attempts;
        Ok(self)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<Option<Result<Sentinel, ()>>, ProbationError> {
        match rightkit_fs::replaced::read_replaced(&self.path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(|_| ()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, sentinel: &Sentinel) -> Result<(), ProbationError> {
        let bytes = serde_json::to_vec(sentinel)
            .map_err(|e| ProbationError::Invalid(format!("sentinel could not be encoded: {e}")))?;
        rightkit_fs::atomic::atomic_write(&self.path, &bytes)?;
        Ok(())
    }

    fn clear(&self) -> Result<(), ProbationError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Record an install that is about to take effect. Call right before the relaunch. Replaces
    /// any earlier sentinel and starts the counter at 0.
    pub fn begin(
        &self,
        from_version: &str,
        to_version: &str,
        install_path: Option<&str>,
    ) -> Result<(), ProbationError> {
        version_text("from version", from_version)?;
        version_text("to version", to_version)?;
        if let Some(path) = install_path {
            install_path_text(path)?;
        }
        self.write(&Sentinel {
            from_version: from_version.to_owned(),
            to_version: to_version.to_owned(),
            boot_attempts: 0,
            install_path: install_path.map(str::to_owned),
            last_stage: None,
        })
    }

    /// Call once at every app start with the running version.
    pub fn evaluate_boot(&self, current_version: &str) -> Result<BootDecision, ProbationError> {
        let record = match self.read()? {
            None => return Ok(BootDecision::None),
            Some(Err(())) => {
                // A sentinel that does not parse proves nothing; it must not block every start.
                self.clear()?;
                return Ok(BootDecision::Stale);
            }
            Some(Ok(record)) => record,
        };
        if record.to_version != current_version {
            self.clear()?;
            return Ok(BootDecision::Stale);
        }
        let boot_attempts = record.boot_attempts.saturating_add(1);
        if boot_attempts > self.max_boot_attempts {
            self.clear()?;
            return Ok(BootDecision::ReinstallRequired(BootFailure {
                failed_version: record.to_version,
                from_version: record.from_version,
                boot_attempts,
                install_path: record.install_path,
                last_stage: record.last_stage,
            }));
        }
        self.write(&Sentinel {
            boot_attempts,
            last_stage: None,
            ..record.clone()
        })?;
        Ok(BootDecision::Probation {
            to_version: record.to_version,
            boot_attempts,
        })
    }

    /// Record how far this boot got (a `boot_stage` command). Does nothing when no update is on
    /// probation; a failed boot reports the last stage it reached.
    pub fn report_stage(&self, stage: &str) -> Result<(), ProbationError> {
        stage_text(stage)?;
        if let Some(Ok(record)) = self.read()? {
            self.write(&Sentinel {
                last_stage: Some(stage.to_owned()),
                ..record
            })?;
        }
        Ok(())
    }

    /// The new version is healthy (a `boot_ok` command): clear the sentinel.
    pub fn acknowledge_healthy(&self) -> Result<(), ProbationError> {
        self.clear()
    }
}
