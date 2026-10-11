//! Shared Whisper route: resolve or install the catalogued large-v3-turbo model
//! under the Suite root (`<suite>/models/whisper-turbo/`).
//!
//! Only the ggml entry is installable. The CoreML and iOS bundles are pinned by
//! archive digest in [`crate::suite::catalog::WHISPER_COREML_ARCHIVE`] and
//! [`crate::suite::catalog::WHISPER_IOS_ARCHIVE`]; their per-file pins are not
//! recorded, so [`resolve_whisper_model`] and [`ensure_whisper_model`] refuse
//! them rather than install an entry that can never verify.
//!
//! [`resolve_whisper_model`] is a read-only lookup at size depth (the engine
//! start path: no multi-GB hash per start). It does not register an app ref;
//! [`ensure_whisper_model`] does, through the store's locked, verified,
//! atomic install.

use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};
use crate::http::http_client;
use crate::suite::catalog::{whisper_turbo_ggml_spec, WHISPER_COREML_ARCHIVE, WHISPER_GGML_FILE};
use crate::suite::manifest::VerifyMode;
use crate::suite::paths;
use crate::suite::store::{
    ensure_ready_cancellable, status_at, EnsureOptions, EntryState, Progress,
};

/// Which Whisper artifact the caller needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperKind {
    /// ggml `whisper_turbo_q5_k.bin` for whisper.cpp 1.9.3 (Windows and macOS).
    Ggml,
    /// macOS CoreML `whisper-multi` bundle. Not installable yet.
    CoreMl,
}

/// Path to the verified ggml model file in the shared store (size-depth check).
pub fn resolve_whisper_model(kind: WhisperKind) -> Result<PathBuf> {
    resolve_whisper_model_at(&paths::models_root(), kind)
}

/// As [`resolve_whisper_model`], under an explicit `models` root.
pub fn resolve_whisper_model_at(models_root: &Path, kind: WhisperKind) -> Result<PathBuf> {
    match kind {
        WhisperKind::Ggml => {
            let spec = whisper_turbo_ggml_spec();
            let status = status_at(models_root, &spec, VerifyMode::Size);
            match status.state {
                EntryState::Ready => Ok(status.dir.join(WHISPER_GGML_FILE)),
                EntryState::Missing => Err(Error::new(
                    ErrorCode::NotFound,
                    format!(
                        "{} is not installed in {}; install it with ensure_whisper_model",
                        spec.id,
                        status.dir.display()
                    ),
                )),
                EntryState::InProgress => Err(Error::new(
                    ErrorCode::Busy,
                    format!("{} is being installed by another process", spec.id),
                )),
                EntryState::Partial => Err(Error::new(
                    ErrorCode::NotFound,
                    format!(
                        "{} does not match trusted revision {} (files to repair: {})",
                        spec.id,
                        spec.version,
                        status.needed.join(", ")
                    ),
                )),
            }
        }
        WhisperKind::CoreMl => Err(coreml_not_installable()),
    }
}

/// Download (verified, resumable) and register the ggml entry for `app_id` when the store
/// has no verified copy; returns the model file. An installed entry returns without
/// touching the network.
///
/// The model is an entitlement-gated add-on: `bearer` is the app's signed Pro entitlement
/// (`Authorization` header value). No worker grant exists for it, so none is requested.
/// Without a bearer a missing model fails before any network call.
pub fn ensure_whisper_model(
    kind: WhisperKind,
    app_id: &str,
    bearer: Option<&str>,
    cancelled: &dyn Fn() -> bool,
    progress: impl FnMut(Progress),
) -> Result<PathBuf> {
    ensure_whisper_model_at(
        &paths::models_root(),
        kind,
        app_id,
        bearer,
        cancelled,
        progress,
    )
}

/// As [`ensure_whisper_model`], under an explicit `models` root.
pub fn ensure_whisper_model_at(
    models_root: &Path,
    kind: WhisperKind,
    app_id: &str,
    bearer: Option<&str>,
    cancelled: &dyn Fn() -> bool,
    progress: impl FnMut(Progress),
) -> Result<PathBuf> {
    match kind {
        WhisperKind::Ggml => {
            let spec = whisper_turbo_ggml_spec();
            if bearer.is_none()
                && status_at(models_root, &spec, VerifyMode::Size).state != EntryState::Ready
            {
                return Err(Error::new(
                    ErrorCode::Download,
                    format!(
                        "{} is an entitlement-gated add-on: the app's signed Pro entitlement is required as the bearer",
                        spec.label
                    ),
                ));
            }
            let status = ensure_ready_cancellable(
                &http_client(),
                models_root,
                &spec,
                &EnsureOptions {
                    seed_dir: None,
                    authorization: bearer,
                    app_id,
                },
                cancelled,
                progress,
            )
            .map_err(|message| Error::new(ErrorCode::Download, message))?;
            Ok(status.dir.join(WHISPER_GGML_FILE))
        }
        WhisperKind::CoreMl => Err(coreml_not_installable()),
    }
}

fn coreml_not_installable() -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!(
            "Whisper CoreML bundle ({}) has no installable store entry: archive {} is pinned, \
             per-file size and SHA-256 pins are not recorded",
            WHISPER_COREML_ARCHIVE.platform, WHISPER_COREML_ARCHIVE.sha256
        ),
    )
}
