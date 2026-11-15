//! Generation core: provider adapters, durable jobs (§4.3), the
//! spend ledger (§4.2), credentials (OS keychain), and result descriptors
//! (§4.4). A Rust host drives it through the typed API in [`typed`]
//! ([`typed::GenerateRequest`] to a job to [`typed::Asset`]s); an engine CLI or app sidecar
//! answers JSON commands through [`api`]. Both are the same path.
// Provider adapters are ported as written; these style lints would only churn the moved code.
#![allow(
    clippy::too_many_arguments,
    clippy::needless_range_loop,
    clippy::slow_vector_initialization,
    clippy::repeat_vec_with_capacity,
    clippy::large_enum_variant,
    clippy::type_complexity,
    // JobStore read/write closures return rightkit-job-queue's JobResult and GenError carries structured details by design;
    // both crates allow this lint for the same reason (rightkit-job-queue lib.rs).
    clippy::result_large_err
)]

pub mod api;
pub mod avatars;
pub mod balances;
pub mod capabilities;
pub mod catalogue;
pub mod credentials;
pub mod descriptor;
pub mod entitlement;
pub mod fal_storage;
pub mod gallery;
pub mod host;
pub mod import;
pub mod jobs;
pub mod lease;
pub mod ledger;
pub mod library;
pub mod media_commands;
pub mod provenance;
pub mod providers;
pub mod qts;
pub mod reference;
pub mod routing;
pub mod runner;
pub mod search;
pub mod stock;
pub mod store;
pub mod typed;
pub mod voices;

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

static CONTEXT_FACTORY: std::sync::OnceLock<fn() -> Context> = std::sync::OnceLock::new();

/// Register the host's production [`Context`] once at startup (its job database file name, its
/// own credential store). [`Context::production`], and with it the detached runner, the job
/// resume at start, the lease watcher and the local providers that open the job store, build
/// their context through it. Returns the rejected factory when one is already registered.
pub fn set_context_factory(factory: fn() -> Context) -> Result<(), fn() -> Context> {
    CONTEXT_FACTORY.set(factory)
}

impl Context {
    /// The host's context: the one [`set_context_factory`] registered, else the app data root,
    /// `media-gen.sqlite3` and the guarded OS keychain.
    pub fn production() -> Self {
        if let Some(factory) = CONTEXT_FACTORY.get() {
            return factory();
        }
        Self {
            data_dir: crate::host::app_data_root(),
            credentials: None,
            library_override: None,
            db_file_name: DEFAULT_DB_FILE_NAME.to_string(),
        }
    }
    /// The host's context rooted at `data_dir`: [`Context::production`] (the registered
    /// [`set_context_factory`] context, so the host's job database file name and credential
    /// store) with `data_dir` replaced and any library override cleared (a moved library belongs
    /// to the host's own root). With no factory it is [`Context::new`]`(data_dir)`. For code that
    /// is handed a data folder (an install under a chosen folder, a CLI's `--user-root`) and must
    /// still open the host's database.
    pub fn production_in(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            library_override: None,
            ..Self::production()
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
