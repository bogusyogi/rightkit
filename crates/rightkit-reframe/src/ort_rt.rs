//! ONNX Runtime for reframe, on RightKit's shared runtime (`rightkit-onnx`).
//!
//! RightKit owns dylib resolution (absolute paths only, the Windows `System32`
//! copy rejected — it hangs at session init), environment setup and
//! execution-provider sessions with an explicit CPU fallback. CutRight keeps
//! only its product inputs: the `RIGHTKIT_ORT_DYLIB` / `RIGHTKIT_REFRAME_EP`
//! overrides and where its bundles and dev builds keep the library.

use crate::error::{ReframeError, Result};
use rightkit_onnx::ort::session::Session;
use rightkit_onnx::{
    installed_runtime_candidate, runtime_filename, CandidateSource, RuntimeCandidate,
    SessionOptions,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DYLIB_ENV: &str = "RIGHTKIT_ORT_DYLIB";
#[cfg(feature = "test-mode")]
pub const EP_ENV: &str = "RIGHTKIT_REFRAME_EP";

/// Where the runtime library is looked for, in priority order: the explicit
/// override (debug builds only), then the app's own bundle (`Resources/runtime`, beside the exe).
/// Every app ships its own copy; loading another app's bundle is a bug. Debug
/// builds and tests also get the workspace staging directory, which
/// `runtime/manifest.json` describes.
pub fn dylib_candidates() -> Vec<RuntimeCandidate> {
    let name = runtime_filename();
    let mut out: Vec<RuntimeCandidate> = Vec::new();
    // A library named by the environment is loaded as code, so the override exists only in
    // debug builds; a shipped engine loads the copy in its own bundle.
    #[cfg(feature = "test-mode")]
    if let Some(v) = std::env::var_os(DYLIB_ENV).filter(|v| !v.is_empty()) {
        out.push(RuntimeCandidate::new(v, CandidateSource::Explicit));
    }
    if let Ok(exe) = std::env::current_exe() {
        out.extend(installed_runtime_candidate(&exe, "runtime"));
        if let Some(dir) = exe.parent() {
            for p in [
                dir.join(name),
                dir.join("runtime").join(name),
                dir.join("resources").join("runtime").join(name),
            ] {
                out.push(RuntimeCandidate::new(p, CandidateSource::Bundled));
            }
        }
    }
    #[cfg(feature = "test-mode")]
    if let Some(dir) = std::env::var_os("RIGHTKIT_ORT_DIR") {
        out.push(RuntimeCandidate::new(
            PathBuf::from(dir).join(name),
            CandidateSource::Explicit,
        ));
    }
    out.retain(|c| c.path.is_absolute());
    out
}

static CONFIGURED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Resolve and configure the library once per process.
pub fn configure() -> Result<PathBuf> {
    let mut guard = CONFIGURED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = guard.as_ref() {
        return Ok(p.clone());
    }
    let candidates = dylib_candidates();
    #[cfg(feature = "test-mode")]
    let dev_override = std::env::var_os(DYLIB_ENV).filter(|v| !v.is_empty());
    #[cfg(feature = "test-mode")]
    let unverified_override = candidates.first().is_some_and(|candidate| {
        dev_override.as_deref() == Some(candidate.path.as_os_str()) && candidate.path.is_file()
    });
    #[cfg(not(feature = "test-mode"))]
    let unverified_override = false;
    let selection = if unverified_override {
        rightkit_onnx::configure_runtime(candidates)
    } else {
        rightkit_onnx::configure_runtime_verified(candidates)
    }
    .map_err(|e| ReframeError::Ort(format!("{e}; set {DYLIB_ENV} to an absolute path")))?;
    rightkit_onnx::init_environment(&selection, &Default::default()).map_err(ReframeError::Ort)?;
    *guard = Some(selection.path.clone());
    Ok(selection.path)
}

/// Execution provider actually bound to a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProvider {
    CoreMl,
    DirectMl,
    Cpu,
}

impl ExecutionProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionProvider::CoreMl => "coreml",
            ExecutionProvider::DirectMl => "directml",
            ExecutionProvider::Cpu => "cpu",
        }
    }
    fn to_rightkit(self) -> rightkit_onnx::ExecutionProvider {
        match self {
            ExecutionProvider::CoreMl => rightkit_onnx::ExecutionProvider::CoreMl,
            ExecutionProvider::DirectMl => rightkit_onnx::ExecutionProvider::DirectMl,
            ExecutionProvider::Cpu => rightkit_onnx::ExecutionProvider::Cpu,
        }
    }
    fn from_rightkit(ep: rightkit_onnx::ExecutionProvider) -> Self {
        match ep {
            rightkit_onnx::ExecutionProvider::CoreMl => ExecutionProvider::CoreMl,
            rightkit_onnx::ExecutionProvider::DirectMl => ExecutionProvider::DirectMl,
            rightkit_onnx::ExecutionProvider::Cpu => ExecutionProvider::Cpu,
        }
    }
}

/// What was asked for versus what bound, for reports and the plan artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpReport {
    pub requested: ExecutionProvider,
    pub used: ExecutionProvider,
    /// Why the requested provider did not bind (empty when it did).
    pub fallback_reason: Option<String>,
    pub ort_dylib: String,
}

/// The platform's accelerator; `test-mode` builds also honour `RIGHTKIT_REFRAME_EP`
/// (`cpu`, `coreml`, `directml`). A shipped build never reads the variable.
pub fn preferred_provider() -> ExecutionProvider {
    #[cfg(feature = "test-mode")]
    match std::env::var(EP_ENV).ok().as_deref().map(str::trim) {
        Some("cpu") => return ExecutionProvider::Cpu,
        Some("coreml") => return ExecutionProvider::CoreMl,
        Some("directml") => return ExecutionProvider::DirectMl,
        _ => {}
    }
    ExecutionProvider::from_rightkit(rightkit_onnx::ExecutionProvider::platform_accelerator())
}

/// Build a session on the preferred provider, falling back to CPU and saying so.
pub fn build_session(model: &Path) -> Result<(Session, EpReport)> {
    let dylib = configure()?;
    let requested = preferred_provider();
    let built = SessionOptions::new(requested.to_rightkit())
        .with_cpu_fallback(true)
        .build_from_file_reported(model)
        .map_err(|e| ReframeError::Ort(e.to_string()))?;
    let report = EpReport {
        requested,
        used: ExecutionProvider::from_rightkit(built.provider),
        fallback_reason: built.fallback,
        ort_dylib: dylib.display().to_string(),
    };
    Ok((built.session, report))
}

#[cfg(test)]
mod tests {
    // Unit tests on purpose: a tampered library must be refused before anything loads it, and
    // an end-to-end run cannot swap the bundled library without breaking the real bundle.
    use super::*;

    fn bundle(lib_bytes: &[u8], manifest_sha: &str, manifest_len: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("libonnxruntime.dylib"), lib_bytes).unwrap();
        let manifest = serde_json::json!({
            "schema": 1,
            "files": [{
                "path": "libonnxruntime.dylib",
                "sha256": manifest_sha,
                "bytes": manifest_len,
            }]
        });
        std::fs::write(dir.path().join("manifest.json"), manifest.to_string()).unwrap();
        dir
    }

    fn sha(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn matching_library_is_accepted() {
        let good = b"genuine runtime";
        let dir = bundle(good, &sha(good), good.len());
        rightkit_onnx::verify_library_in_manifest(&dir.path().join("libonnxruntime.dylib"))
            .unwrap();
    }

    #[test]
    fn tampered_library_is_refused() {
        let good = b"genuine runtime";
        let dir = bundle(good, &sha(good), good.len());
        let lib = dir.path().join("libonnxruntime.dylib");
        let evil = b"evil    runtime";
        assert_eq!(
            evil.len(),
            good.len(),
            "the tamper must keep the manifest length"
        );
        std::fs::write(&lib, evil).unwrap(); // same length, different bytes
        let err = rightkit_onnx::verify_library_in_manifest(&lib).unwrap_err();
        assert!(matches!(
            err,
            rightkit_onnx::RuntimeError::BundleFileHash { .. }
        ));
        // the load path refuses it too, instead of falling through to a later candidate
        let refused = rightkit_onnx::configure_runtime_verified(vec![RuntimeCandidate::new(
            &lib,
            CandidateSource::Explicit,
        )]);
        assert!(refused.is_err());
    }

    #[test]
    fn resized_library_is_refused() {
        let good = b"genuine runtime";
        let dir = bundle(good, &sha(good), good.len());
        let lib = dir.path().join("libonnxruntime.dylib");
        std::fs::write(&lib, b"genuine runtime plus payload").unwrap();
        assert!(matches!(
            rightkit_onnx::verify_library_in_manifest(&lib),
            Err(rightkit_onnx::RuntimeError::BundleFileSize { .. })
        ));
    }

    #[test]
    fn missing_manifest_or_entry_fails_closed() {
        let good = b"genuine runtime";
        let dir = bundle(good, &sha(good), good.len());
        let lib = dir.path().join("libonnxruntime.dylib");
        std::fs::remove_file(dir.path().join("manifest.json")).unwrap();
        assert!(rightkit_onnx::verify_library_in_manifest(&lib).is_err());
        std::fs::write(dir.path().join("manifest.json"), r#"{"files":[]}"#).unwrap();
        assert!(rightkit_onnx::verify_library_in_manifest(&lib)
            .unwrap_err()
            .to_string()
            .contains("not listed"));
    }

    #[test]
    fn staged_bundle_library_matches_its_manifest() {
        let Some(dir) = std::env::var_os("RIGHTKIT_ORT_DIR") else {
            eprintln!("SKIP staged_bundle_library_matches_its_manifest: no ONNX Runtime (looked at RIGHTKIT_ORT_DIR)");
            return;
        };
        let lib = PathBuf::from(dir).join(runtime_filename());
        if !lib.is_file() {
            eprintln!("SKIP: no staged runtime at {}", lib.display());
            return;
        }
        rightkit_onnx::verify_library_in_manifest(&lib).unwrap();
    }
}
