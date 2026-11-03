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

/// Where the runtime library is looked for, in priority order: the app's own bundle
/// (`Resources/runtime`, beside the exe). Every app ships its own copy; loading another app's
/// bundle is a bug.
pub fn dylib_candidates() -> Vec<RuntimeCandidate> {
    let name = runtime_filename();
    let mut out: Vec<RuntimeCandidate> = Vec::new();
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
    let selection = rightkit_onnx::configure_runtime_verified(candidates)
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

/// The platform's accelerator.
pub fn preferred_provider() -> ExecutionProvider {
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
