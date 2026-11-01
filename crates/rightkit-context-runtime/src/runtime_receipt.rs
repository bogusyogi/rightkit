//! Durable, content-free identity receipt for one resident startup.

use crate::installation_identity::{InstallationIdentity, StartupClaim};
use rightkit_fs::{atomic_write_with_options, AtomicWriteOptions, FsError, ReplaceMode};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const RUNTIME_RECEIPT_SCHEMA_VERSION: u32 = 2;
pub const TELEMETRY_OUTBOX_TABLE: &str = "membrane_event_outbox";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReceiptV2 {
    pub schema_version: u32,
    pub workspace_root: PathBuf,
    pub cortex_db: PathBuf,
    pub catalog_db: PathBuf,
    pub telemetry_event_db: PathBuf,
    pub telemetry_outbox_db: PathBuf,
    pub telemetry_outbox_table: String,
    pub installation_id: String,
    pub startup_generation: u64,
    pub service_instance_id: String,
    pub claimed_at: String,
    /// Additive installed/development provenance. Older receipts remain
    /// readable while current health can identify stable-current resolution.
    #[serde(default)]
    pub runtime_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_install_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_version_root: Option<PathBuf>,
    #[serde(default)]
    pub release_generation: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeReceiptError {
    #[error("runtime binding {binding} must be absolute: {value}")]
    Relative {
        binding: &'static str,
        value: String,
    },
    #[error("workspace root cannot be derived from the Cortex database path: {0}")]
    WorkspaceUnbound(PathBuf),
    #[error("runtime identity and startup claim do not match")]
    IdentityMismatch,
    #[error("runtime receipt {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime receipt conflict: {0}")]
    Conflict(PathBuf),
    #[error("runtime receipt path is not a regular owned path: {0}")]
    UnsafePath(PathBuf),
    #[error("encode runtime receipt: {0}")]
    Encode(String),
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> RuntimeReceiptError {
    RuntimeReceiptError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn require_absolute(
    binding: &'static str,
    path: impl AsRef<Path>,
) -> Result<PathBuf, RuntimeReceiptError> {
    let path = path.as_ref();
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Err(RuntimeReceiptError::Relative {
            binding,
            value: path.to_string_lossy().into_owned(),
        })
    }
}

/// Resolve installed-code provenance from validated executable ancestry. Data
/// may intentionally remain under legacy Membrane state; workspace_root must
/// never be used as install identity in absorbed CodeRight layouts.
fn installed_code_roots_for_executable(executable: &Path) -> Result<(PathBuf, PathBuf), String> {
    let runtime = crate::service::runtime_from_installed_exe(executable)?;
    Ok((
        runtime
            .stable_current
            .ok_or_else(|| "installed stable root unavailable".to_string())?,
        runtime
            .version_root
            .ok_or_else(|| "installed version root unavailable".to_string())?,
    ))
}

fn installed_code_roots() -> Option<(PathBuf, PathBuf)> {
    let executable = std::env::current_exe().ok()?;
    installed_code_roots_for_executable(&executable).ok()
}

pub fn resolve_workspace_root(
    configured: Option<std::ffi::OsString>,
    cortex_db: &Path,
) -> Result<PathBuf, RuntimeReceiptError> {
    if let Some(configured) = configured.filter(|value| !value.is_empty()) {
        return require_absolute("workspace root", PathBuf::from(configured));
    }
    let cortex_db = require_absolute("cortex database", cortex_db)?;
    let memory = cortex_db.parent();
    let cache = memory.and_then(Path::parent);
    let tools = cache.and_then(Path::parent);
    if memory
        .and_then(Path::file_name)
        .is_some_and(|name| name == "memory")
        && cache
            .and_then(Path::file_name)
            .is_some_and(|name| name == ".cache")
        && tools
            .and_then(Path::file_name)
            .is_some_and(|name| name == "tools")
    {
        return tools
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or(RuntimeReceiptError::WorkspaceUnbound(cortex_db));
    }
    Err(RuntimeReceiptError::WorkspaceUnbound(cortex_db))
}

pub fn validate_telemetry_event_binding(
    configured: Option<std::ffi::OsString>,
) -> Result<Option<PathBuf>, RuntimeReceiptError> {
    configured
        .filter(|value| !value.is_empty())
        .map(|value| require_absolute("telemetry event database", PathBuf::from(value)))
        .transpose()
}

impl RuntimeReceiptV2 {
    pub fn new(
        workspace_root: &Path,
        cortex_db: &Path,
        catalog_db: &Path,
        telemetry_event_db: &Path,
        identity: &InstallationIdentity,
        claim: &StartupClaim,
    ) -> Result<Self, RuntimeReceiptError> {
        if identity.installation_id != claim.installation_id
            || identity.startup_generation != claim.startup_generation
            || identity.current_service_instance_id.as_deref()
                != Some(claim.service_instance_id.as_str())
            || identity.current_claimed_at.as_deref() != Some(claim.claimed_at.as_str())
            || claim.startup_generation == 0
        {
            return Err(RuntimeReceiptError::IdentityMismatch);
        }
        let cortex_db = require_absolute("cortex database", cortex_db)?;
        let runtime_origin = match crate::qa_env::runtime_origin()
            .and_then(|value| value.into_string().ok())
            .as_deref()
        {
            Some("installed") => "installed",
            Some("development") => "development",
            _ if workspace_root
                .file_name()
                .is_some_and(|name| name == "state")
                && workspace_root
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "Membrane") =>
            {
                "installed"
            }
            // A release build only ever runs from the installed layout (`runtime_from_installed_exe`).
            _ if !cfg!(debug_assertions) => "installed",
            _ => "development",
        };
        let (stable_install_root, resolved_version_root) = if runtime_origin == "installed" {
            let (stable, version) = installed_code_roots().ok_or_else(|| {
                RuntimeReceiptError::WorkspaceUnbound(workspace_root.to_path_buf())
            })?;
            (Some(stable), Some(version))
        } else {
            (None, None)
        };
        Ok(Self {
            schema_version: RUNTIME_RECEIPT_SCHEMA_VERSION,
            workspace_root: require_absolute("workspace root", workspace_root)?,
            cortex_db: cortex_db.clone(),
            catalog_db: require_absolute("catalog database", catalog_db)?,
            telemetry_event_db: require_absolute("telemetry event database", telemetry_event_db)?,
            telemetry_outbox_db: cortex_db,
            telemetry_outbox_table: TELEMETRY_OUTBOX_TABLE.to_string(),
            installation_id: identity.installation_id.clone(),
            startup_generation: claim.startup_generation,
            service_instance_id: claim.service_instance_id.clone(),
            claimed_at: claim.claimed_at.clone(),
            runtime_origin: runtime_origin.to_string(),
            stable_install_root,
            resolved_version_root,
            release_generation: crate::release_identity::release_generation(),
        })
    }

    pub fn path(&self) -> PathBuf {
        self.workspace_root
            .join("tools/.cache/memory/runtime-receipts")
            .join(format!(
                "{:020}-{}.json",
                self.startup_generation, self.service_instance_id
            ))
    }

    pub fn persist(&self) -> Result<PathBuf, RuntimeReceiptError> {
        let final_path = self.path();
        let parent = final_path
            .parent()
            .expect("runtime receipt path always has a parent");
        fs::create_dir_all(parent).map_err(|error| io_error("create directory", parent, error))?;
        let parent_metadata = fs::symlink_metadata(parent)
            .map_err(|error| io_error("inspect directory", parent, error))?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(RuntimeReceiptError::UnsafePath(parent.to_path_buf()));
        }
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| RuntimeReceiptError::Encode(error.to_string()))?;
        bytes.push(b'\n');
        if let Ok(metadata) = fs::symlink_metadata(&final_path) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(RuntimeReceiptError::UnsafePath(final_path));
            }
            let existing =
                fs::read(&final_path).map_err(|error| io_error("read", &final_path, error))?;
            return if existing == bytes {
                Ok(final_path)
            } else {
                Err(RuntimeReceiptError::Conflict(final_path))
            };
        }
        let options = AtomicWriteOptions {
            replace: ReplaceMode::NoReplace,
            durable: true,
            ..AtomicWriteOptions::default()
        };
        match atomic_write_with_options(&final_path, options, |file| file.write_all(&bytes)) {
            Ok(()) => Ok(final_path),
            Err(FsError::AlreadyExists(_)) => {
                let metadata = fs::symlink_metadata(&final_path)
                    .map_err(|error| io_error("inspect", &final_path, error))?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(RuntimeReceiptError::UnsafePath(final_path));
                }
                let existing =
                    fs::read(&final_path).map_err(|error| io_error("read", &final_path, error))?;
                if existing == bytes {
                    Ok(final_path)
                } else {
                    Err(RuntimeReceiptError::Conflict(final_path))
                }
            }
            Err(error) => Err(io_error("publish", &final_path, error.into())),
        }
    }
}

static CURRENT_RECEIPT: Mutex<Option<RuntimeReceiptV2>> = Mutex::new(None);

pub fn publish_current(receipt: RuntimeReceiptV2) {
    *CURRENT_RECEIPT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(receipt);
}

pub fn current_snapshot() -> Option<RuntimeReceiptV2> {
    CURRENT_RECEIPT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}
