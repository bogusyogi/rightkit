//! The face-detector model, fetched into the shared Orthic Labs model store.
//!
//! YuNet `face_detection_yunet_2023mar.onnx` from OpenCV Zoo (MIT licence, Shiqi Yu;
//! 232,589 bytes) — pinned by commit URL and SHA-256. Stored as
//! `<shared>/models/<id>/{face_detection_yunet_2023mar.onnx,manifest.json}` by the shared
//! store, so other Orthic Labs apps can reuse it.

use crate::error::{ReframeError, Result};
use rightkit_models::http::http_client;
use rightkit_models::suite::catalog::{ModelSpec, ModelTier, RemoteFile, SpecFile, SpecSource};
use rightkit_models::suite::{ensure_ready, resolve_ready_dir, EnsureOptions, EntryState};
use std::path::{Path, PathBuf};

pub const YUNET_ID: &str = "yunet-face-detection-2023mar";
pub const YUNET_FILE: &str = "face_detection_yunet_2023mar.onnx";
pub const YUNET_SHA256: &str = "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4";
pub const YUNET_SIZE: u64 = 232_589;
pub const YUNET_URL: &str = "https://github.com/opencv/opencv_zoo/raw/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/face_detection_yunet_2023mar.onnx";
pub const YUNET_LICENSE: &str = "MIT (OpenCV Zoo, face_detection_yunet)";

const FILES: &[SpecFile] = &[SpecFile {
    path: YUNET_FILE,
    sha256: Some(YUNET_SHA256),
    size: Some(YUNET_SIZE),
}];
const REMOTE: &[RemoteFile] = &[RemoteFile {
    path: YUNET_FILE,
    url: Some(YUNET_URL),
    sha256: Some(YUNET_SHA256),
    size: Some(YUNET_SIZE),
}];

pub fn yunet_spec() -> ModelSpec {
    ModelSpec {
        id: YUNET_ID,
        label: "YuNet face detector",
        // On-demand (reframe only), not part of the base install; no entitlement gate.
        tier: ModelTier::AddOn,
        entitlement: false,
        version: "2023mar",
        size_hint_bytes: YUNET_SIZE,
        files: FILES,
        source: SpecSource::Files(REMOTE),
        // Only ever installed in the shared store: nothing to migrate or retire.
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

/// Ensure the verified model is in the shared store (downloads once, verifies SHA-256,
/// registers the host app's ref, `app_id`) and return the `.onnx` path.
pub fn ensure_yunet(models_root: &Path, app_id: &str) -> Result<PathBuf> {
    let spec = yunet_spec();
    let status = ensure_ready(
        &http_client(),
        models_root,
        &spec,
        &EnsureOptions {
            seed_dir: None,
            authorization: None,
            app_id,
        },
        |_| {},
    )
    .map_err(ReframeError::Model)?;
    if status.state != EntryState::Ready {
        return Err(ReframeError::Model(format!(
            "{} is not ready ({:?})",
            spec.label, status.state
        )));
    }
    Ok(status.dir.join(YUNET_FILE))
}

/// Resolve the model without downloading (None if absent or unverified).
pub fn resolve_yunet(models_root: &Path) -> Option<PathBuf> {
    resolve_ready_dir(models_root, &yunet_spec()).map(|d| d.join(YUNET_FILE))
}
