//! Model install bridge over `rightkit-models` (cargo feature `models`).
//!
//! This is only the UniFFI surface. The HTTPS-pinned grant request, the resumable pinned download, the
//! bounded tar.gz extraction, atomic activation with the previous entry kept aside during replacement,
//! and crash recovery are all `rightkit-models`' (`suite::grant`, `Store::install`,
//! `Store::recover`). Nothing here downloads, hashes, extracts or activates anything itself.
//!
//! Layout: the store root is the bridge's `models_dir` (`<filesDir>/models`), the entry is
//! `<root>/<model_id>/`, and the archive's contents land in `<root>/<model_id>/<payload_dir>/`. The
//! Kotlin wrapper is `com.orthiclabs.rightkit.android.models.ModelDownloadManager`.
//!
//! Blocking: [`install_model`] runs on the calling thread until the install ends (Kotlin calls it from
//! `Dispatchers.IO`). This crate spawns no thread. Cancel with [`ModelInstallCancel::cancel`] from any thread.
//!
//! UNVERIFIED: reviewed against `rightkit-models` 0.7.2 and UniFFI 0.32 source
//! without being compiled or run on a device.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rightkit_models::http::ModelHttpOptions;
use rightkit_models::suite::grant::{try_acquire_grant_pinned, GrantError, ModelGrantRequest};
use rightkit_models::{
    ErrorCode, Event, Extract, FileSpec, InstallOptions, ModelId, ModelSpec, Platform, RevisionSpec,
    Store, VerifyMode,
};

use crate::context::{AndroidContext, ContextHandle};
use crate::error::AndroidError;

/// What to install. The host owns the catalogue: every value here comes from the app's configuration and the
/// model Worker's published pins, never from this crate.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ModelInstallRequest {
    /// The host app named in the store's reference registry.
    pub app_id: String,
    /// Store entry id: the directory under `<filesDir>/models`.
    pub model_id: String,
    pub label: String,
    /// Revision key written into the entry's manifest; use the archive digest.
    pub revision: String,
    /// Directory (relative, forward slashes) under the entry that receives the archive's contents.
    pub payload_dir: String,
    /// The Worker origin, `https` only, no credentials, no path.
    pub worker_base: String,
    /// Published archive route on that origin; also the grant's route.
    pub route: String,
    /// Published grant model name.
    pub grant_model: String,
    /// The grant's `platform` (`android`).
    pub platform: String,
    /// The stable per-install device id sent with the grant request.
    pub device_id: String,
    /// `User-Agent` of the download (validated by `rightkit-models`).
    pub user_agent: String,
    /// SHA-256 (64 lowercase hex) of the whole archive, from the Worker's manifest.
    pub archive_sha256: String,
    /// Size in bytes of the whole archive.
    pub archive_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ModelInstallStage {
    /// Waiting for the grant or the response, or counting bytes already staged.
    Downloading,
    /// A downloaded file was hashed against its pin.
    Verifying,
    /// Extracting and activating.
    Installing,
}

/// Progress sink implemented in Kotlin. Called on the installing thread; keep it short and never block it.
#[uniffi::export(with_foreign)]
pub trait ModelInstallListener: Send + Sync {
    fn on_progress(&self, stage: ModelInstallStage, done: u64, total: u64);
}

/// Cancels one [`install_model`] call. Create one per call.
#[derive(uniffi::Object)]
pub struct ModelInstallCancel {
    flag: Arc<AtomicBool>,
}

#[uniffi::export]
impl ModelInstallCancel {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            flag: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Requests cooperative cancellation. If activation already won, installation succeeds.
    /// Download cancellation discards staging; extraction/activation are not interruptible.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

/// Errors of the model bridge. Messages never contain a path, a URL, a token or a provider message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum ModelInstallError {
    #[error("rightkit-android is not initialised")]
    NotInitialised,
    #[error("invalid request: {reason}")]
    Invalid { reason: String },
    #[error("model install cancelled")]
    Cancelled,
    /// The Worker answered 429; `retry_after_secs` is its `Retry-After` when it sent one.
    #[error("model download is rate limited")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("model download grant failed")]
    Grant,
    #[error("not enough free space for the model")]
    DiskSpace,
    #[error("model integrity check failed")]
    Integrity,
    #[error("another process is installing this model")]
    Busy,
    /// Any other `rightkit-models` failure; `code` is its `ErrorCode` name.
    #[error("model install failed ({code})")]
    Failed { code: String },
}

impl From<AndroidError> for ModelInstallError {
    fn from(error: AndroidError) -> Self {
        match error {
            AndroidError::NotInitialised => ModelInstallError::NotInitialised,
            AndroidError::Invalid { reason } => ModelInstallError::Invalid { reason },
            AndroidError::Jni { .. } => ModelInstallError::Failed {
                code: "Jni".to_string(),
            },
        }
    }
}

fn from_models_error(error: &rightkit_models::Error) -> ModelInstallError {
    match &error.code {
        ErrorCode::Cancelled | ErrorCode::Paused => ModelInstallError::Cancelled,
        ErrorCode::DiskSpace => ModelInstallError::DiskSpace,
        ErrorCode::HashMismatch | ErrorCode::SizeMismatch => ModelInstallError::Integrity,
        ErrorCode::Busy => ModelInstallError::Busy,
        ErrorCode::InvalidSpec | ErrorCode::NoFiles => ModelInstallError::Invalid {
            reason: format!("{:?}", error.code),
        },
        code => ModelInstallError::Failed {
            code: format!("{code:?}"),
        },
    }
}

fn invalid(reason: &str) -> ModelInstallError {
    ModelInstallError::Invalid {
        reason: reason.to_string(),
    }
}

/// `https://host[:port]` with a host and nothing else; returns the origin and its host name.
fn https_origin(base: &str) -> Result<(String, String), ModelInstallError> {
    let parsed = url::Url::parse(base).map_err(|_| invalid("invalid worker origin"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || base.bytes().any(|b| b.is_ascii_whitespace() || b == b'\\')
    {
        return Err(invalid("worker_base must be an https origin"));
    }
    // Use the same canonical origin for grants and downloads.
    Ok((parsed.origin().ascii_serialization(), parsed.host_str().unwrap().to_string()))
}

fn checked_route(route: &str) -> Result<&str, ModelInstallError> {
    if !route.starts_with('/')
        || route.len() <= 1
        || !route.bytes().all(|b| b.is_ascii_alphanumeric() || b"/-._~".contains(&b))
        || route[1..].split('/').any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid("route must be a plain absolute path"));
    }
    Ok(route)
}

fn validate(request: &ModelInstallRequest) -> Result<(), ModelInstallError> {
    ModelId::parse(&request.app_id).map_err(|_| invalid("invalid app_id"))?;
    ModelId::parse(&request.model_id).map_err(|_| invalid("invalid model_id"))?;
    validate_payload_dir(&request.payload_dir)?;
    if request.archive_sha256.len() != 64
        || !request.archive_sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        || request.archive_bytes == 0 || request.archive_bytes > i64::MAX as u64
        || request.revision != request.archive_sha256
    {
        return Err(invalid("revision must equal the pinned archive sha256; positive byte size required"));
    }
    if request.platform != "android" || request.grant_model.trim().is_empty()
        || request.device_id.trim().is_empty() || request.user_agent.trim().is_empty()
    {
        return Err(invalid("android platform, grant_model, device_id and user_agent are required"));
    }
    Ok(())
}

fn validate_payload_dir(payload_dir: &str) -> Result<(), ModelInstallError> {
    if payload_dir.split('/').any(|part| {
        part.is_empty() || part == "." || part == ".."
            || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
    }) {
        return Err(invalid("payload_dir must be a relative directory path"));
    }
    let root = payload_dir.split('/').next().unwrap_or_default();
    if matches!(root, ".dl" | ".shared" | "manifest.json") {
        return Err(invalid("payload_dir uses a reserved model store name"));
    }
    Ok(())
}

fn spec_for(request: &ModelInstallRequest, archive_url: String) -> Result<ModelSpec, ModelInstallError> {
    let hex = request.archive_sha256.len() == 64
        && request
            .archive_sha256
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !hex || request.archive_bytes == 0 {
        return Err(invalid("archive_sha256 and archive_bytes must be the pinned values"));
    }
    if request.revision.trim().is_empty() || request.payload_dir.trim().is_empty() {
        return Err(invalid("revision and payload_dir are required"));
    }
    let file = FileSpec {
        path: request.payload_dir.clone(),
        url: archive_url,
        bytes: Some(request.archive_bytes),
        sha256: Some(request.archive_sha256.clone()),
        extract: Some(Extract::TarGz),
        ..Default::default()
    };
    let revision = RevisionSpec {
        revision: request.revision.clone(),
        files: vec![file],
        ..Default::default()
    };
    Ok(ModelSpec::new(request.model_id.clone(), vec![revision]).with_label(request.label.clone()))
}

fn store_for(request: &ModelInstallRequest) -> Result<Store, ModelInstallError> {
    validate(request)?;
    let root = AndroidContext::get()?.paths().models_dir();
    let mut store = Store::new(root, request.app_id.clone());
    if !request.user_agent.trim().is_empty() {
        store = store.with_http_options(ModelHttpOptions {
            user_agent: Some(request.user_agent.clone()),
            ..Default::default()
        });
    }
    Ok(store)
}

fn grant(request: &ModelInstallRequest, origin: &str) -> Result<String, ModelInstallError> {
    // Pinned to the Worker host over HTTPS by `rightkit-models`; a 429 is typed and never replayed.
    try_acquire_grant_pinned(ModelGrantRequest {
        worker_base: origin,
        route: &request.route,
        model: &request.grant_model,
        platform: &request.platform,
        sha256: &request.archive_sha256,
        device_id: &request.device_id,
    })
    .map(|grant| grant.bearer())
    .map_err(|error| match error {
        GrantError::RateLimited { retry_after, .. } => ModelInstallError::RateLimited {
            retry_after_secs: retry_after.map(|delay| delay.as_secs()),
        },
        GrantError::Failed(_) => ModelInstallError::Grant,
    })
}

/// The installed payload directory for `request` when exactly that revision is installed and verified, else
/// `None`. Full file verification; `Store::resolve` may recover a legacy layout. No network.
#[uniffi::export]
pub fn installed_model_dir(request: ModelInstallRequest) -> Result<Option<String>, ModelInstallError> {
    let store = store_for(&request)?;
    let key = RevisionSpec {
        revision: request.revision.clone(),
        ..Default::default()
    }
    .key();
    let Some(resolved) = store.resolve(&request.model_id).filter(|resolved| resolved.key == key) else {
        return Ok(None);
    };
    if !store.verify(&request.model_id, &key, VerifyMode::Full) {
        return Ok(None);
    }
    Ok(verified_payload_dir(&resolved.dir, &request.payload_dir))
}

fn verified_payload_dir(model_dir: &Path, payload_dir: &str) -> Option<String> {
    let entry = model_dir.canonicalize().ok()?;
    let payload = entry.join(payload_dir).canonicalize().ok()?;
    (payload.is_dir() && payload != entry && payload.starts_with(&entry))
        .then(|| payload.to_string_lossy().into_owned())
}

/// Resolves an install that a crash or kill interrupted (the previous entry is put back, or the finished one
/// kept). Call it once at startup. Returns whether anything was recovered.
#[uniffi::export]
pub fn recover_model(request: ModelInstallRequest) -> Result<bool, ModelInstallError> {
    store_for(&request)?
        .recover(&request.model_id)
        .map_err(|error| from_models_error(&error))
}

/// Makes the requested revision installed and returns its payload directory. Does nothing (and asks for no
/// grant) when exactly that revision is already installed. Otherwise: a grant from the Worker (HTTPS pinned),
/// the pinned resumable download of the archive, bounded tar.gz extraction, and atomic activation with the
/// previous entry kept aside during replacement. Cancellation is cooperative; activation wins a late cancel.
/// One automatic grant refresh if the Worker rejects
/// the first.
#[uniffi::export]
pub fn install_model(
    request: ModelInstallRequest,
    cancel: Arc<ModelInstallCancel>,
    listener: Arc<dyn ModelInstallListener>,
) -> Result<String, ModelInstallError> {
    validate(&request)?;
    if cancel.flag.load(Ordering::SeqCst) { return Err(ModelInstallError::Cancelled); }
    let (origin, host) = https_origin(&request.worker_base)?;
    let route = checked_route(&request.route)?;
    if request.device_id.trim().is_empty() || request.grant_model.trim().is_empty() {
        return Err(invalid("device_id and grant_model are required"));
    }
    let spec = spec_for(&request, format!("{origin}{route}"))?;
    if let Some(dir) = installed_model_dir(request.clone())? {
        return Ok(dir);
    }
    let store = store_for(&request)?;
    let mut refreshed = false;
    loop {
        if cancel.flag.load(Ordering::SeqCst) {
            return Err(ModelInstallError::Cancelled);
        }
        listener.on_progress(ModelInstallStage::Downloading, 0, request.archive_bytes);
        let authorization = grant(&request, &origin)?;
        let options = InstallOptions {
            revision: Some(request.revision.clone()),
            platform: Platform::current(),
            authorization: Some(authorization),
            // The bearer goes to the Worker host only, and no redirect is followed with it.
            authorization_host: Some(host.clone()),
            max_download_redirects: 0,
            cancel: Arc::clone(&cancel.flag),
            require_pinned: true,
            ..Default::default()
        };
        let sink = |event: Event| match event {
            Event::Preflight { needed_bytes, .. } => {
                listener.on_progress(ModelInstallStage::Downloading, 0, needed_bytes)
            }
            Event::Progress {
                overall_done,
                overall_total,
                ..
            } => listener.on_progress(ModelInstallStage::Downloading, overall_done, overall_total),
            Event::FileVerified { .. } => {
                listener.on_progress(ModelInstallStage::Verifying, request.archive_bytes, request.archive_bytes)
            }
            Event::Extracted { .. } | Event::Activated { .. } => {
                listener.on_progress(ModelInstallStage::Installing, request.archive_bytes, request.archive_bytes)
            }
            _ => {}
        };
        match store.install(&spec, &options, sink) {
            Ok(outcome) => {
                return verified_payload_dir(&outcome.dir, &request.payload_dir)
                    .ok_or_else(|| invalid("installed revision does not contain the requested payload layout"));
            }
            Err(error) if !refreshed && error.message.contains("download authorization rejected") => {
                refreshed = true;
            }
            Err(error) => return Err(from_models_error(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_reserved_store_paths() {
        for path in [".dl", ".dl/model", ".shared", ".shared/model", "manifest.json"] {
            assert!(validate_payload_dir(path).is_err(), "{path}");
        }
        assert!(validate_payload_dir("model/weights").is_ok());
    }

    #[test]
    fn missing_payload_cannot_be_reported_ready() {
        let entry = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(verified_payload_dir(entry, "src").is_some());
        assert!(verified_payload_dir(entry, "missing-layout").is_none());
        assert!(verified_payload_dir(entry, "Cargo.toml").is_none());
        assert!(verified_payload_dir(entry, ".").is_none());
    }
}
