//! Generation core: provider adapters, durable jobs (§4.3), the
//! spend ledger (§4.2), credentials (OS keychain), and result descriptors
//! (§4.4). The engine CLI and the app sidecar both drive it through [`api`].

/// The OS temp dir with symlinks resolved (macOS `/var` is a link, which the safe-filesystem owner refuses).
#[cfg(test)]
pub(crate) fn test_temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    if cfg!(windows) {
        dir
    } else {
        dir.canonicalize().unwrap_or(dir)
    }
}

pub mod api;
pub mod avatars;
pub mod balances;
pub mod capabilities;
pub mod catalogue;
pub mod credentials;
pub mod descriptor;
pub mod entitlement;
pub mod fal_storage;
pub mod host;
pub mod import;
pub mod jobs;
pub mod lease;
pub mod ledger;
pub mod library;
pub mod media;
pub mod provenance;
pub mod providers;
pub mod qts;
pub mod routing;
pub mod runner;
pub mod search;
pub mod stock;
pub mod store;

/// ONNX Runtime readiness for the engine's `doctor` (resolve + load probe).
pub use crate::qts::models::{onnxruntime_report, OnnxRuntimeReport};

use std::path::PathBuf;

/// Default file name of the job store database.
pub const DEFAULT_DB_FILE_NAME: &str = "media-gen.sqlite3";

/// Where the host keeps jobs, the ledger, and its library. Tests point it at
/// a temp dir; production uses [`crate::host::app_data_root`].
#[derive(Debug, Clone)]
pub struct Context {
    pub data_dir: PathBuf,
    /// Injected key source; `None` means the OS keychain.
    pub credentials: Option<std::sync::Arc<dyn credentials::CredentialSource>>,
    /// Library folder when it is not `<data_dir>/library` (a moved library).
    pub library_override: Option<PathBuf>,
    /// File name of the job store database under `data_dir`.
    pub db_file_name: String,
}

impl Context {
    pub fn production() -> Self {
        Self {
            data_dir: crate::host::app_data_root(),
            credentials: None,
            library_override: None,
            db_file_name: DEFAULT_DB_FILE_NAME.to_string(),
        }
    }
    /// A context rooted at `data_dir` using the OS keychain.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            credentials: None,
            library_override: None,
            db_file_name: DEFAULT_DB_FILE_NAME.to_string(),
        }
    }
    /// Replace the key source (tests use an in-memory store).
    pub fn with_credentials(
        mut self,
        source: std::sync::Arc<dyn credentials::CredentialSource>,
    ) -> Self {
        self.credentials = Some(source);
        self
    }
    /// Keep media in `dir` instead of `<data_dir>/library`.
    pub fn with_library_dir(mut self, dir: PathBuf) -> Self {
        self.library_override = Some(dir);
        self
    }
    /// The job store database: `<data_dir>/<db file name>` (see [`Context::with_db_file_name`]).
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join(&self.db_file_name)
    }
    /// Name the job store database file (default `media-gen.sqlite3`); a host with an existing
    /// database passes its file name here.
    pub fn with_db_file_name(mut self, name: impl Into<String>) -> Self {
        self.db_file_name = name.into();
        self
    }
    pub fn library_dir(&self) -> PathBuf {
        self.library_override
            .clone()
            .unwrap_or_else(|| self.data_dir.join("library"))
    }
    /// Per-job NDJSON logs of detached runners: `jobs/<job_id>.ndjson`.
    pub fn job_logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs").join("jobs")
    }
    pub fn locks_dir(&self) -> PathBuf {
        self.data_dir.join("locks")
    }
}

/// Typed error returned by every command; serialises as `{code, message}`
/// plus `details` when the error carries structured data for the UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct GenError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl GenError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
    /// Attach structured detail (for example the local files an upload would send).
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for GenError {}

pub type GenResult<T> = Result<T, GenError>;

// Test binaries are unsigned ad-hoc artifacts; touching the real credential
// store from them raises interactive prompts. This runs before any test in
// this binary, so Context::new can never reach the OS store.
#[cfg(test)]
mod no_keychain_in_tests {
    #[ctor::ctor(unsafe)]
    fn guard() {
        std::env::set_var("RIGHTKIT_NO_KEYCHAIN", "1");
        // A test process never defaults to the user's real data folder: with no folder set,
        // voice-model checks ran against the installed models and rewrote their cache file there.
        if std::env::var_os("RIGHTKIT_APP_DATA")
            .filter(|v| !v.is_empty())
            .is_none()
        {
            let dir = std::env::temp_dir()
                .join(format!("rightkit-media-gen-unit-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("isolated test data folder");
            std::env::set_var("RIGHTKIT_APP_DATA", dir);
        }
    }
}
