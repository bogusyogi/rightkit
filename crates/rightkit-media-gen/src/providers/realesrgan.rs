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
        let better = best.as_ref().map_or(true, |b: &Installed| {
            version_key(&version) > version_key(&b.version)
        });
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
            root: super::orthic_root(),
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
        let scratch =
            std::env::temp_dir().join(format!("rightkit-realesrgan-{}", std::process::id()));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn req(model: &str, params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::Upscale,
            provider: ID.into(),
            model: model.into(),
            prompt: "upscale".into(),
            params,
        }
    }

    /// A tools root laid out like an install: manifest + empty binary file +
    /// models dir. `runtime_ready` is metadata-only so a stub file proves it.
    fn install_dir() -> (tempfile_root::Root, PathBuf) {
        let root = tempfile_root::Root::new();
        let home = tool_dir(&root.0).join("20220424");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::create_dir_all(home.join("models")).unwrap();
        std::fs::write(home.join("bin").join(binary_name()), b"stub").unwrap();
        std::fs::write(
            home.join("manifest.json"),
            serde_json::json!({"version":"20220424","complete":true,"protocol":1}).to_string(),
        )
        .unwrap();
        (root, home)
    }

    mod tempfile_root {
        pub struct Root(pub std::path::PathBuf);
        impl Root {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!(
                    "gr-realesrgan-test-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                std::fs::create_dir_all(&p).unwrap();
                Root(p)
            }
        }
        impl Drop for Root {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn discovery_is_metadata_only() {
        let (root, home) = install_dir();
        let inst = discover(&root.0).expect("a complete install is found");
        assert_eq!(inst.version, "20220424");
        assert!(inst.binary.ends_with(binary_name()));
        assert_eq!(inst.models, home.join("models"));
        // Incomplete installs are skipped.
        let other = tool_dir(&root.0).join("20000101");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("manifest.json"),
            r#"{"version":"20000101","complete":false}"#,
        )
        .unwrap();
        assert_eq!(discover(&root.0).unwrap().version, "20220424");
    }

    #[test]
    fn missing_install_reports_not_installed_without_running() {
        let root = tempfile_root::Root::new();
        let p = RealEsrgan::with_root(Some(root.0.clone()));
        let e = p.runtime_ready().unwrap().unwrap_err();
        assert_eq!(e.code, "not_installed");
        assert!(e.message.contains("realesrgan-ncnn-vulkan"));
        let e = p
            .submit(
                &req(X4, serde_json::json!({"image_path": "/tmp/x.png"})),
                "",
            )
            .unwrap_err();
        assert_eq!(e.code, "not_installed");
    }

    /// Native-output check, opt-in: run only when RIGHTKIT_REALESRGAN_TOOLS
    /// points at a tools root holding a real install. Exercises the actual
    /// official binary end to end (submit -> PNG bytes).
    #[test]
    fn real_binary_upscales_a_png_when_installed() {
        let Some(root) = std::env::var_os("RIGHTKIT_REALESRGAN_TOOLS").map(PathBuf::from) else {
            eprintln!("SKIP: RIGHTKIT_REALESRGAN_TOOLS is not set (no Real-ESRGAN install)");
            return;
        };
        let dir = root.join("__test-inputs");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.png");
        std::fs::write(&input, PNG_1X1).unwrap();
        let p = RealEsrgan::with_root(Some(root.clone()));
        let out = p
            .submit(
                &req(
                    X4,
                    serde_json::json!({"image_path": input.to_string_lossy(), "scale": 4}),
                ),
                "",
            )
            .unwrap();
        let SubmitOutcome::Completed { outputs } = out else {
            panic!()
        };
        let (bytes, mime) = p.fetch(&outputs[0]).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR width: byte offset 16..20 big-endian; must be 4x the 1x1 input.
        let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        assert_eq!(w, 4, "output width should be 4x the 1x1 input");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 1x1 opaque red PNG, fixed bytes (PNG signature + IHDR/IDAT/IEND).
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn validation_rejects_bad_input_before_launch() {
        let (root, _home) = install_dir();
        let p = RealEsrgan::with_root(Some(root.0.clone()));
        assert_eq!(
            p.estimate_usd(&req(X4, serde_json::json!({}))).unwrap(),
            0.0
        );
        let mut wrong = req(X4, serde_json::json!({}));
        wrong.kind = Kind::Image;
        assert_eq!(p.estimate_usd(&wrong).unwrap_err().code, "invalid_request");
        assert_eq!(
            p.estimate_usd(&req("nope", serde_json::json!({})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            p.estimate_usd(&req(X4, serde_json::json!({"scale": 8})))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            p.submit(
                &req(
                    X4,
                    serde_json::json!({"image_path": "/definitely/missing.png"})
                ),
                ""
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
    }
}
