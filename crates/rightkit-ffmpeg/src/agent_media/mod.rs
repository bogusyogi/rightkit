//! Native media tool command adapters.
//!
//! These tools intentionally delegate to installed system binaries instead of
//! embedding OCR, transcription, scraping, download, or media codecs in the
//! daemon process.

use rightkit_agent_policy::{
    catastrophic_reason, CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
    WorkspaceTrust,
};
use serde_json::{json, Value};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

const DEFAULT_TIMEOUT_MS: u64 = 300_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
const MAX_OUTPUT_BYTES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaToolKind {
    /// Shell-out to the system tesseract binary. No model bundle; always available.
    Ocr,
    /// In-process OS-native OCR (rightkit-ocr: Apple Vision / Windows.Media.Ocr, no
    /// models). Falls back to Ocr (tesseract) only when the tool is unavailable.
    OcrOnnx,
    Transcribe,
    WebExtract,
    MediaFetch,
    FfmpegExtract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeMediaTool {
    kind: MediaToolKind,
}

#[derive(Debug, Clone)]
pub struct MediaExecutionContext {
    pub workspace_root: PathBuf,
    pub permission_mode: PermissionMode,
    pub workspace_trust: WorkspaceTrust,
    pub execution_backend: Option<String>,
    pub execution_network_profile: Option<String>,
    pub cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaToolResult {
    pub content: String,
    pub is_error: bool,
    pub process_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

pub struct ManagedProcessOutput {
    pub output: std::process::Output,
    pub process_id: Option<String>,
}

pub trait ProcessLaunchAdapter: Send + Sync {
    fn launch<'a>(
        &'a self,
        spec: &'a CommandSpec,
        workspace_root: &'a std::path::Path,
        cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Pin<Box<dyn Future<Output = Result<ManagedProcessOutput, String>> + Send + 'a>>;
}

#[derive(Debug, Default, Clone, Copy)]
struct MissingProcessLaunchAdapter;

impl ProcessLaunchAdapter for MissingProcessLaunchAdapter {
    fn launch<'a>(
        &'a self,
        _spec: &'a CommandSpec,
        _workspace_root: &'a std::path::Path,
        _cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Pin<Box<dyn Future<Output = Result<ManagedProcessOutput, String>> + Send + 'a>> {
        Box::pin(async { Err("governed process launch adapter required".to_string()) })
    }
}

impl NativeMediaTool {
    pub const fn new(kind: MediaToolKind) -> Self {
        Self { kind }
    }

    pub const fn kind(&self) -> MediaToolKind {
        self.kind
    }

    pub fn name(&self) -> &'static str {
        self.kind.name()
    }

    pub fn description(&self) -> &'static str {
        self.kind.description()
    }

    pub fn input_schema(&self) -> Value {
        self.kind.input_schema()
    }

    pub fn command_spec(&self, args: &Value) -> Result<CommandSpec, MediaToolResult> {
        self.kind.command_spec(args)
    }

    pub async fn execute(&self, args: Value, ctx: &MediaExecutionContext) -> MediaToolResult {
        self.execute_with_adapter(args, ctx, &MissingProcessLaunchAdapter)
            .await
    }

    pub async fn execute_with_adapter(
        &self,
        args: Value,
        ctx: &MediaExecutionContext,
        adapter: &dyn ProcessLaunchAdapter,
    ) -> MediaToolResult {
        execute_native_media_tool(self.kind, args, ctx, adapter).await
    }
}

impl MediaToolKind {
    fn name(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::OcrOnnx => "ocr_onnx",
            Self::Transcribe => "transcribe",
            Self::WebExtract => "web_extract",
            Self::MediaFetch => "media_fetch",
            Self::FfmpegExtract => "ffmpeg_extract",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Ocr => {
                "Run Tesseract OCR on an image/PDF page via the installed tesseract binary."
            },
            Self::OcrOnnx => "In-process OCR using the operating system's own text-recognition framework (Apple Vision on macOS, Windows.Media.Ocr on Windows). No models to download or bundle. This is the default OCR backend; use the ocr tool only if you specifically need Tesseract. Name retained for tool-contract stability — it no longer uses ONNX.",
            Self::Transcribe => {
                "Run a local speech-to-text binary such as whisper or parakeet on an audio/video file."
            }
            Self::WebExtract => {
                "Run an installed web extraction/scraping CLI against a URL and return its output."
            }
            Self::MediaFetch => "Run yt-dlp to fetch media or media metadata from a URL.",
            Self::FfmpegExtract => {
                "Run ffmpeg to extract a clip, audio stream, frames, or another derived media file."
            }
        }
    }

    fn input_schema(self) -> Value {
        match self {
            Self::Ocr => json!({
                "type": "object",
                "required": ["image_path"],
                "properties": {
                    "image_path": { "type": "string", "description": "Image or page file to OCR, relative to the workspace or absolute." },
                    "language": { "type": "string", "description": "Tesseract language code such as eng." },
                    "psm": { "type": "integer", "description": "Optional Tesseract page segmentation mode." },
                    "binary": { "type": "string", "description": "Override binary path. Defaults to tesseract." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds, max 3600000." },
                    "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Additional argv tokens appended to the binary invocation." }
                }
            }),
            Self::OcrOnnx => json!({
                "type": "object",
                "required": ["image_path"],
                "properties": {
                    "image_path": { "type": "string", "description": "Image or page file to OCR, relative to the workspace or absolute." },
                    "model_dir": { "type": "string", "description": "Deprecated and ignored: OCR is the OS framework and loads no models. Accepted so existing callers do not break." },
                    "language": { "type": "string", "description": "Reserved for future multilingual support; currently ignored." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds, max 3600000." },
                    "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Reserved for future CLI flags." }
                }
            }),
            Self::Transcribe => json!({
                "type": "object",
                "required": ["media_path"],
                "properties": {
                    "media_path": { "type": "string", "description": "Audio or video file to transcribe." },
                    "engine": { "type": "string", "enum": ["whisper", "parakeet"], "description": "Default binary family when binary is not provided." },
                    "binary": { "type": "string", "description": "Override binary path. Defaults to engine, then whisper." },
                    "model": { "type": "string", "description": "Optional model name/path passed as --model." },
                    "language": { "type": "string", "description": "Optional language code passed as --language." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds, max 3600000." },
                    "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Additional argv tokens appended to the binary invocation." }
                }
            }),
            Self::WebExtract => json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "description": "URL to extract or scrape." },
                    "binary": { "type": "string", "description": "Override binary path. Defaults to scrape." },
                    "format": { "type": "string", "description": "Optional output format passed as --format." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds, max 3600000." },
                    "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Additional argv tokens appended to the binary invocation." }
                }
            }),
            Self::MediaFetch => json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "description": "Media URL for yt-dlp." },
                    "output_path": { "type": "string", "description": "Optional output template/path passed with -o. If omitted, metadata JSON is returned." },
                    "format": { "type": "string", "description": "Optional yt-dlp format selector passed with -f." },
                    "binary": { "type": "string", "description": "Override binary path. Defaults to yt-dlp." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds, max 3600000." },
                    "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Additional argv tokens appended to the binary invocation." }
                }
            }),
            Self::FfmpegExtract => json!({
                "type": "object",
                "required": ["input_path", "output_path"],
                "properties": {
                    "input_path": { "type": "string", "description": "Input media path." },
                    "output_path": { "type": "string", "description": "Output media path." },
                    "start": { "type": "string", "description": "Optional start timestamp passed with -ss." },
                    "duration": { "type": "string", "description": "Optional duration passed with -t." },
                    "mode": { "type": "string", "enum": ["copy", "audio"], "description": "audio adds -vn; copy adds -c copy." },
                    "binary": { "type": "string", "description": "Override binary path. Defaults to ffmpeg." },
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds, max 3600000." },
                    "extra_args": { "type": "array", "items": { "type": "string" }, "description": "Additional argv tokens inserted before output_path." }
                }
            }),
        }
    }

    fn command_spec(self, args: &Value) -> Result<CommandSpec, MediaToolResult> {
        let timeout_ms = args["timeout_ms"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);
        let extra_args = optional_string_array(args, "extra_args")?;

        let spec = match self {
            Self::Ocr => {
                let image_path = required_string(args, "image_path")?;
                let mut argv = vec![image_path, "stdout".to_string()];
                if let Some(language) = optional_string(args, "language") {
                    argv.extend(["-l".to_string(), language]);
                }
                if let Some(psm) = args["psm"].as_u64() {
                    argv.extend(["--psm".to_string(), psm.to_string()]);
                }
                argv.extend(extra_args);
                CommandSpec {
                    program: optional_string(args, "binary").unwrap_or_else(|| "tesseract".into()),
                    args: argv,
                    timeout_ms,
                }
            }
            Self::OcrOnnx => {
                return Err(MediaToolResult::error(
                    "ocr_onnx is an in-process backend and has no CommandSpec; use NativeMediaTool::execute instead.",
                ));
            }
            Self::Transcribe => {
                let media_path = required_string(args, "media_path")?;
                let engine = optional_string(args, "engine").unwrap_or_else(|| "whisper".into());
                if engine != "whisper" && engine != "parakeet" {
                    return Err(MediaToolResult::error(
                        "Invalid engine: expected 'whisper' or 'parakeet'.",
                    ));
                }
                let mut argv = vec![media_path];
                if let Some(model) = optional_string(args, "model") {
                    argv.extend(["--model".to_string(), model]);
                }
                if let Some(language) = optional_string(args, "language") {
                    argv.extend(["--language".to_string(), language]);
                }
                argv.extend(extra_args);
                CommandSpec {
                    program: optional_string(args, "binary").unwrap_or(engine),
                    args: argv,
                    timeout_ms,
                }
            }
            Self::WebExtract => {
                let url = required_string(args, "url")?;
                let mut argv = Vec::new();
                if let Some(format) = optional_string(args, "format") {
                    argv.extend(["--format".to_string(), format]);
                }
                argv.extend(extra_args);
                argv.push(url);
                CommandSpec {
                    program: optional_string(args, "binary").unwrap_or_else(|| "scrape".into()),
                    args: argv,
                    timeout_ms,
                }
            }
            Self::MediaFetch => {
                let url = required_string(args, "url")?;
                let mut argv = Vec::new();
                if let Some(format) = optional_string(args, "format") {
                    argv.extend(["-f".to_string(), format]);
                }
                if let Some(output_path) = optional_string(args, "output_path") {
                    argv.extend(["-o".to_string(), output_path]);
                } else {
                    argv.push("--dump-json".to_string());
                }
                argv.extend(extra_args);
                argv.push(url);
                CommandSpec {
                    program: optional_string(args, "binary").unwrap_or_else(|| "yt-dlp".into()),
                    args: argv,
                    timeout_ms,
                }
            }
            Self::FfmpegExtract => {
                let input_path = required_string(args, "input_path")?;
                let output_path = required_string(args, "output_path")?;
                let mode = match optional_string(args, "mode").as_deref() {
                    Some("audio") => Some(crate::ExtractMode::AudioOnly),
                    Some("copy") => Some(crate::ExtractMode::Copy),
                    Some(other) => {
                        return Err(MediaToolResult::error(format!(
                            "Invalid mode: expected 'audio' or 'copy', got '{other}'."
                        )))
                    }
                    None => None,
                };
                let options = crate::ExtractOptions {
                    start: optional_string(args, "start")
                        .map(|value| parse_timestamp("start", &value))
                        .transpose()?,
                    duration: optional_string(args, "duration")
                        .map(|value| parse_timestamp("duration", &value))
                        .transpose()?,
                    mode,
                    extra: extra_args
                        .into_iter()
                        .map(std::ffi::OsString::from)
                        .collect(),
                };
                let argv: Vec<String> = crate::extract_args(
                    std::path::Path::new(&input_path),
                    std::path::Path::new(&output_path),
                    &options,
                )
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
                CommandSpec {
                    program: optional_string(args, "binary").unwrap_or_else(|| "ffmpeg".into()),
                    args: argv,
                    timeout_ms,
                }
            }
        };

        Ok(spec)
    }
}

pub fn native_media_tools() -> Vec<NativeMediaTool> {
    vec![
        NativeMediaTool::new(MediaToolKind::Ocr),
        NativeMediaTool::new(MediaToolKind::OcrOnnx),
        NativeMediaTool::new(MediaToolKind::Transcribe),
        NativeMediaTool::new(MediaToolKind::WebExtract),
        NativeMediaTool::new(MediaToolKind::MediaFetch),
        NativeMediaTool::new(MediaToolKind::FfmpegExtract),
    ]
}

impl MediaExecutionContext {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        permission_mode: PermissionMode,
        workspace_trust: WorkspaceTrust,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            permission_mode,
            workspace_trust,
            execution_backend: None,
            execution_network_profile: None,
            cancel_rx: None,
        }
    }
}

impl MediaToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            process_id: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            process_id: None,
        }
    }

    fn with_process_id(mut self, pid: Option<String>) -> Self {
        self.process_id = pid;
        self
    }
}

async fn execute_native_media_tool(
    kind: MediaToolKind,
    args: Value,
    ctx: &MediaExecutionContext,
    adapter: &dyn ProcessLaunchAdapter,
) -> MediaToolResult {
    if ctx.workspace_root.as_os_str().is_empty() {
        return MediaToolResult::error(
            "This session has no workspace root; refusing to run a media command \
             against the daemon's working directory. Open a folder for the session first.",
        );
    }

    // In-process backends bypass the shell-out path entirely.
    if matches!(kind, MediaToolKind::OcrOnnx) {
        return execute_ocr_onnx(&args, ctx).await;
    }
    let spec = match kind.command_spec(&args) {
        Ok(spec) => spec,
        Err(result) => return result,
    };
    let display = display_command(&spec.program, &spec.args);

    if let Some(reason) = catastrophic_reason(&display) {
        return MediaToolResult::error(format!(
            "Blocked by destructive guard ({reason}): this media command is refused in any mode."
        ));
    }
    if let Some(message) = nonlocal_execution_backend_error(kind.name(), ctx) {
        return MediaToolResult::error(message);
    }
    if let Some(message) = unenforced_network_profile_error(kind.name(), ctx) {
        return MediaToolResult::error(message);
    }

    let verdict = PermissionEngine::new().evaluate(
        CapabilityClass::Shell,
        ctx.permission_mode,
        ctx.workspace_trust,
        None,
        false,
    );
    match verdict.decision {
        PermissionDecision::Deny => {
            return MediaToolResult::error(format!(
                "Permission denied: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ))
        }
        PermissionDecision::Ask => {
            return MediaToolResult::error(format!(
                "Tool requires approval: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ))
        }
        PermissionDecision::Allow => {}
    }

    let launched = match adapter
        .launch(&spec, &ctx.workspace_root, ctx.cancel_rx.clone())
        .await
    {
        Ok(value) => value,
        Err(error) if error.starts_with("timeout:") => {
            return MediaToolResult::error(format!(
                "{} command timed out after {}ms: {}",
                kind.name(),
                spec.timeout_ms,
                display
            ))
            .with_process_id(error.strip_prefix("timeout:").map(str::to_owned))
        }
        Err(error) if error.starts_with("cancelled:") => {
            return MediaToolResult::error(format!(
                "{} command cancelled by operator.",
                kind.name()
            ))
            .with_process_id(error.strip_prefix("cancelled:").map(str::to_owned))
        }
        Err(error) => {
            return MediaToolResult::error(format!(
                "Failed to spawn {} binary '{}': {}",
                kind.name(),
                spec.program,
                error
            ))
        }
    };
    process_output(launched.output, launched.process_id)
}

/// OS-native OCR through RightKit, native backend only (no ONNX fallback).
/// Output is the newline-joined line text, the same shape the host app always returned.
fn ocr_image_native(path: &std::path::Path) -> Result<String, String> {
    let mut ocr = rightkit_ocr::Ocr::new(rightkit_ocr::OcrOptions::new(
        rightkit_ocr::BackendPreference::Native,
    ));
    ocr.recognize_file(path)
        .map(|result| result.text())
        .map_err(|e| e.to_string())
}

/// In-process OS-native OCR backend (Apple Vision / Windows.Media.Ocr).
///
/// Calls rightkit-ocr directly -- no shell-out, no subprocess, no permission-gate
/// shell check (reading an image is read-only). Since 2026-07-18 there are no models and no
/// ONNX Runtime in this path, so nothing needs resolving before the call; the `model_dir`
/// argument is accepted and ignored for tool-contract stability.
async fn execute_ocr_onnx(args: &Value, ctx: &MediaExecutionContext) -> MediaToolResult {
    use std::path::PathBuf;

    let image_path = match args.get("image_path").and_then(|v| v.as_str()) {
        Some(p) => PathBuf::from(p),
        None => return MediaToolResult::error("ocr_onnx requires an image_path argument."),
    };
    let image_path = if image_path.is_absolute() {
        image_path
    } else {
        ctx.workspace_root.join(&image_path)
    };
    if !image_path.exists() {
        return MediaToolResult::error(format!(
            "ocr_onnx: image not found at {}",
            image_path.display()
        ));
    }

    // OCR is CPU-bound in the OS framework. Run in spawn_blocking so we do not stall the
    // executor. The engine handle is free to construct now, so no caching is warranted.
    let path_for_blocking = image_path.clone();
    let join = tokio::task::spawn_blocking(move || ocr_image_native(&path_for_blocking)).await;
    match join {
        Ok(Ok(text)) => MediaToolResult::ok(text),
        Ok(Err(e)) => MediaToolResult::error(format!("ocr_onnx failed: {e}")),
        Err(e) => MediaToolResult::error(format!("ocr_onnx worker panicked: {e}")),
    }
}

fn process_output(output: std::process::Output, process_id: Option<String>) -> MediaToolResult {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut combined = String::new();
    combined.push_str(&stdout);
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("[stderr]\n");
        combined.push_str(&stderr);
    }
    truncate_output(&mut combined);
    let exit_code = output.status.code().unwrap_or(-1);
    if output.status.success() {
        MediaToolResult::ok(combined).with_process_id(process_id)
    } else {
        MediaToolResult::error(format!("[exit {exit_code}]\n{combined}"))
            .with_process_id(process_id)
    }
}

fn truncate_output(output: &mut String) {
    if output.len() <= MAX_OUTPUT_BYTES {
        return;
    }
    while !output.is_char_boundary(MAX_OUTPUT_BYTES) {
        output.pop();
    }
    output.truncate(MAX_OUTPUT_BYTES);
    output.push_str("\n[truncated]");
}

fn nonlocal_execution_backend_error(surface: &str, ctx: &MediaExecutionContext) -> Option<String> {
    let backend = ctx
        .execution_backend
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if backend.eq_ignore_ascii_case("local") {
        return None;
    }
    Some(format!(
        "{surface} executionBackend '{backend}' is not available for command execution; only 'local' is currently supported"
    ))
}

fn unenforced_network_profile_error(surface: &str, ctx: &MediaExecutionContext) -> Option<String> {
    let profile = ctx
        .execution_network_profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if profile.eq_ignore_ascii_case("local-host") {
        return None;
    }
    Some(format!(
        "{surface} networkProfile '{profile}' is not enforced by local host execution; refusing to run until backend routing applies the selected profile"
    ))
}

fn required_string(args: &Value, key: &str) -> Result<String, MediaToolResult> {
    optional_string(args, key)
        .ok_or_else(|| MediaToolResult::error(format!("Missing required argument: {key}")))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn optional_string_array(args: &Value, key: &str) -> Result<Vec<String>, MediaToolResult> {
    let Some(value) = args.get(key) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = value.as_array() else {
        return Err(MediaToolResult::error(format!(
            "{key} must be an array of strings."
        )));
    };
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(text) = item.as_str() else {
            return Err(MediaToolResult::error(format!(
                "{key}[{index}] must be a string."
            )));
        };
        out.push(text.to_string());
    }
    Ok(out)
}

/// Parse an ffmpeg-style timestamp (`SS[.frac]`, `MM:SS[.frac]` or `HH:MM:SS[.frac]`) into a
/// `Duration` for `crate::extract_args`.
fn parse_timestamp(field: &str, value: &str) -> Result<std::time::Duration, MediaToolResult> {
    let invalid = || {
        MediaToolResult::error(format!(
            "Invalid {field}: expected seconds or [HH:]MM:SS[.fraction], got '{value}'."
        ))
    };
    let mut seconds = 0.0_f64;
    for part in value.trim().split(':') {
        let n: f64 = part.parse().map_err(|_| invalid())?;
        if !n.is_finite() || n < 0.0 {
            return Err(invalid());
        }
        seconds = seconds * 60.0 + n;
    }
    std::time::Duration::try_from_secs_f64(seconds).map_err(|_| invalid())
}

fn display_command(program: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(program.to_string());
    parts.extend(args.iter().cloned());
    parts.join(" ")
}
