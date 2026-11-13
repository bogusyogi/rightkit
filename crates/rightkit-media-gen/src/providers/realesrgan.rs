//! Real-ESRGAN ncnn Vulkan — local, free, native image upscale route.
//!
//! The provider consumes an installed copy of the official
//! `realesrgan-ncnn-vulkan` release binary (xinntao/Real-ESRGAN GitHub
//! releases — MIT-licensed code and models, free download). Nothing is
//! Python or a scripted worker: it is one compiled binary plus its `models/`
//! folder. No network call happens at generation time.
//!
//! Install layout (shared tools under the organisation root):
//!
//! ```text
//! <organisation root>/shared/tools/realesrgan-ncnn-vulkan/<version>/
//!     manifest.json      {"version","complete":true,"protocol":1}
//!     bin/realesrgan-ncnn-vulkan[.exe]
//!     models/            (realesrgan-x4plus.* etc., shipped in the zip)
//! ```
//!
//! `runtime_ready` is metadata-only: it checks the manifest and binary on
//! disk, never launches the binary. `test_key` runs `-h` to prove the binary
//! actually executes (still local, still free). `runtime_is_external` is
//! false — this is a compiled native binary, not a scripted worker.
//!
//! Request: `kind:"upscale"`, `model` `realesrgan-x4plus` |
//! `realesrgan-x4plus-anime`, `params.image_path` (a local file the engine
//! can read — the engine runs on the same machine), `params.scale` 2|3|4
//! (default 4). Prompt is unused by the model and may be empty text
//! placeholders like "upscale".

use super::{
    Durations, GenRequest, Kind, ModelInfo, Output, PollOutcome, Provider, ProviderError,
    SubmitOutcome,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ID: &str = "realesrgan";
/// Model dir name inside the tool home (matches the release zip layout).
const MODELS_DIR: &str = "models";
const PROTOCOL: u64 = 1;
/// Per-image wall clock limit; a hung binary must not block the job runner.
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

const X4: &str = "realesrgan-x4plus";
const X4_ANIME: &str = "realesrgan-x4plus-anime";

static MODELS: [ModelInfo; 2] = [
    ModelInfo {
        provider: ID,
        model: X4,
        kind: Kind::Upscale,
        label:
            "Real-ESRGAN x4 general (local ncnn Vulkan, free, needs the official release binary)",
        est_usd: 0.0,
        durations: Durations::NONE,
    },
    ModelInfo {
        provider: ID,
        model: X4_ANIME,
        kind: Kind::Upscale,
        label: "Real-ESRGAN x4 anime/illustration (local ncnn Vulkan, free)",
        est_usd: 0.0,
        durations: Durations::NONE,
    },
];

pub struct RealEsrgan {
    root: Option<PathBuf>,
}

/// Where the tool home lives; `shared/tools` under the organisation root
/// (same convention as the local worker).
pub fn tool_dir(root: &Path) -> PathBuf {
    root.join("shared")
        .join("tools")
        .join("realesrgan-ncnn-vulkan")
}

pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "realesrgan-ncnn-vulkan.exe"
    } else {
        "realesrgan-ncnn-vulkan"
    }
}

#[derive(Debug, Clone)]
pub struct Installed {
    pub home: PathBuf,
    pub binary: PathBuf,
    pub models: PathBuf,
    pub version: String,
}

/// Highest-version complete install under `root`, manifest + binary on disk
/// only — never executes anything.
pub fn discover(root: &Path) -> Option<Installed> {
    let dir = tool_dir(root);
    let mut best: Option<Installed> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let home = entry.path();
        let Ok(text) = std::fs::read_to_string(home.join("manifest.json")) else {
            continue;
        };
        let Ok(m) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if m["complete"] != Value::Bool(true) || m["protocol"].as_u64() != Some(PROTOCOL) {
            continue;
        }
        let binary = home.join("bin").join(binary_name());
        let models = home.join(MODELS_DIR);
        if !binary.is_file() || !models.is_dir() {
            continue;
        }
        let version = m["version"].as_str().unwrap_or("").to_string();
        let better = best
            .as_ref()
            .is_none_or(|b: &Installed| version_key(&version) > version_key(&b.version));
        if better {
            best = Some(Installed {
                home,
                binary,
                models,
                version,
            });
        }
    }
    best
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

impl RealEsrgan {
    pub fn new() -> Self {
        Self {
            root: super::tools_root(),
        }
    }

    /// Tests and fixtures point the provider at an explicit tools root.
    pub fn with_root(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    fn installed(&self) -> Result<Installed, ProviderError> {
        let root = self.root.clone().ok_or_else(|| {
            ProviderError::new(
                "not_installed",
                "no organisation data root for this OS",
                false,
            )
        })?;
        discover(&root).ok_or_else(|| {
            ProviderError::new(
                "not_installed",
                format!(
                    "Real-ESRGAN ncnn binary not installed: download the official \
                     realesrgan-ncnn-vulkan release (free, from the Real-ESRGAN \
                     GitHub releases page) into {}",
                    tool_dir(&root).display()
                ),
                false,
            )
        })
    }

    fn check(&self, req: &GenRequest) -> Result<(), ProviderError> {
        if req.kind != Kind::Upscale {
            return Err(ProviderError::new(
                "invalid_request",
                "realesrgan only upscales images",
                false,
            ));
        }
        if req.model != X4 && req.model != X4_ANIME {
            return Err(ProviderError::new(
                "invalid_request",
                format!("unknown realesrgan model {}", req.model),
                false,
            ));
        }
        Ok(())
    }
}

impl Default for RealEsrgan {
    fn default() -> Self {
        Self::new()
    }
}

fn scale_of(req: &GenRequest) -> Result<u32, ProviderError> {
    let Some(v) = req.params.get("scale") else {
        return Ok(4);
    };
    let n = v
        .as_u64()
        .or_else(|| v.as_str()?.parse().ok())
        .ok_or_else(|| {
            ProviderError::new("invalid_request", "params.scale must be 2, 3 or 4", false)
        })?;
    if (2..=4).contains(&n) {
        Ok(n as u32)
    } else {
        Err(ProviderError::new(
            "invalid_request",
            "params.scale must be 2, 3 or 4",
            false,
        ))
    }
}

impl Provider for RealEsrgan {
    fn id(&self) -> &'static str {
        ID
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    /// `submit`: `params.image_path`, a local file; `scale_of` takes 2, 3 or 4 (default 4). No aspect, resolution or seed.
    fn capabilities(&self, _model: &str) -> crate::capabilities::Capabilities {
        crate::capabilities::Capabilities::default()
            .mode("image")
            .input("image", crate::capabilities::io("image_path", "path", true))
    }

    fn needs_key(&self) -> bool {
        false
    }
    /// Local engine: a submit has no remote charge, so a replay is free.
    fn submit_is_idempotent(&self) -> bool {
        true
    }

    /// Metadata-only: manifest + binary + models dir on disk. Never launches
    /// the binary and never touches the network.
    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        Some(self.installed().map(|_| ()))
    }

    /// Compiled native binary, not a scripted worker.
    fn runtime_is_external(&self) -> bool {
        false
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.check(req)?;
        scale_of(req)?;
        Ok(0.0)
    }

    fn submit(&self, req: &GenRequest, _api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.check(req)?;
        let scale = scale_of(req)?;
        let inst = self.installed()?;
        let input = req
            .params
            .get("image_path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProviderError::new(
                    "invalid_request",
                    "realesrgan needs params.image_path (a local file)",
                    false,
                )
            })?;
        let input = PathBuf::from(input);
        if !input.is_file() {
            return Err(ProviderError::new(
                "invalid_request",
                format!(
                    "params.image_path is not a readable file: {}",
                    input.display()
                ),
                false,
            ));
        }
        // Owner-only scratch folder: the output file is not created in the shared temp folder.
        let scratch = std::env::temp_dir().join(format!(
            "{}-realesrgan-{}",
            crate::host::config().machine_dir_name,
            std::process::id()
        ));
        rightkit_fs::ensure_private_dir(&scratch).map_err(|e| {
            ProviderError::new(
                "unavailable",
                format!("could not prepare the realesrgan scratch folder: {e}"),
                true,
            )
        })?;
        let out = scratch.join(format!(
            "out-{}.png",
            rightkit_logs::identity::random_uuid()
        ));
        let args: Vec<std::ffi::OsString> = vec![
            "-i".into(),
            input.into(),
            "-o".into(),
            out.clone().into(),
            "-s".into(),
            scale.to_string().into(),
            "-n".into(),
            req.model.as_str().into(),
            "-m".into(),
            inst.models.clone().into(),
            "-f".into(),
            "png".into(),
        ];
        // One bounded owned run: deadline with tree kill, no console window, stdin null, stderr kept (tail only).
        let options = rightkit_child_process::RunOptions::new(RUN_TIMEOUT)
            .stdout_cap(0)
            .stderr_cap(64 * 1024);
        let output = rightkit_child_process::run(&inst.binary, &args, &options).map_err(|e| {
            let _ = std::fs::remove_file(&out);
            ProviderError::new(
                "not_installed",
                format!("could not run {}: {e}", inst.binary.display()),
                false,
            )
        })?;
        if output.timed_out {
            let _ = std::fs::remove_file(&out);
            return Err(ProviderError::new(
                "timeout",
                "realesrgan exceeded its run limit",
                true,
            ));
        }
        if !output.status.success() {
            let _ = std::fs::remove_file(&out);
            let text = String::from_utf8_lossy(&output.stderr.bytes);
            let tail: String = text
                .chars()
                .rev()
                .take(400)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            return Err(ProviderError::new(
                "generation_failed",
                format!("realesrgan exited {}: {}", output.status, tail.trim()),
                false,
            ));
        }
        let bytes = std::fs::read(&out).map_err(|e| {
            ProviderError::new(
                "bad_response",
                format!("realesrgan produced no readable output: {e}"),
                true,
            )
        })?;
        let _ = std::fs::remove_file(&out);
        if bytes.len() < 16 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
            return Err(ProviderError::new(
                "bad_response",
                "realesrgan output was not a PNG",
                true,
            ));
        }
        Ok(SubmitOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes,
                mime: "image/png".into(),
            }],
        })
    }

    fn poll(&self, _req: &GenRequest, _id: &str, _key: &str) -> Result<PollOutcome, ProviderError> {
        Err(ProviderError::new(
            "invalid_request",
            "realesrgan completes synchronously; nothing to poll",
            false,
        ))
    }

    /// Deeper check for `media.modalities {"probe":true}`: run `-h` and see
    /// the binary actually execute. Local process only, still free.
    fn test_key(&self, _api_key: &str) -> Result<(), ProviderError> {
        let inst = self.installed()?;
        match rightkit_child_process::run(
            &inst.binary,
            ["-h"],
            &rightkit_child_process::RunOptions::new(Duration::from_secs(30)),
        ) {
            Ok(out) if out.timed_out => Err(ProviderError::new(
                "not_installed",
                format!(
                    "{} did not execute: child exceeded its time limit and was terminated",
                    inst.binary.display()
                ),
                false,
            )),
            // ncnn tools answer -h with a nonzero exit but they did run.
            Ok(_) => Ok(()),
            Err(e) => Err(ProviderError::new(
                "not_installed",
                format!("{} did not execute: {e}", inst.binary.display()),
                false,
            )),
        }
    }

    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        match output {
            Output::Bytes { bytes, mime } => Ok((bytes.clone(), mime.clone())),
            Output::Url { .. } => Err(ProviderError::new(
                "invalid_request",
                "realesrgan returns bytes, not URLs",
                false,
            )),
            Output::Path { .. } | Output::Finalized { .. } => Err(ProviderError::new(
                "unsupported",
                "path outputs are adopted by the library",
                false,
            )),
        }
    }
}
