//! `media_tools` — registers the `host_media` crate's native media
//! tools (OCR / web-extract / media-fetch / ffmpeg-extract)
//! as first-class agent tools in the
//! [`ToolRegistry`](crate::registry::ToolRegistry).
//!
//! WS-7, FEATURE-MATRIX `media / HR-SR tool coverage`. The
//! `host_media` crate already builds the `NativeMediaTool`
//! command-spec adapters (ffmpeg / tesseract / whisper / yt-dlp / scrape)
//! in `engine/crates/media/src/lib.rs`. This module is the bridge to the
//! registry's own [`Tool`] trait; the factories in
//! `registry/registry_impl.rs` (`builtin_tool_factories`, CQ-16) register
//! each media tool individually. The actual
//! subprocess execution — `CommandSpec` -> spawn -> capture stdout /
//! stderr / exit code — lives in `rightkit_ffmpeg::agent_media::execute_native_media_tool`
//! and is invoked through `NativeMediaTool::execute`.
//!
//! Permission gating is deliberately separate from arbitrary shell execution:
//! every media tool reports `CapabilityClass::Media`, allowing roles to use
//! OCR/transcription/extraction without receiving `shell` authority.
//! Media tools are filterable through `--allowed-tools` /
//! `--disallowed-tools` / `--toolset` like every other tool — no
//! special-casing.

use crate::context::ToolContext;
use crate::shell::NativeCommandSandboxAdapter;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_sandbox::execution::{
    CommandLaunchSpec, ProcessLaunchAdapter as _, ProcessLaunchContext,
};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

struct GovernedMediaProcessLaunchAdapter {
    network_required: bool,
}

impl rightkit_ffmpeg::agent_media::ProcessLaunchAdapter for GovernedMediaProcessLaunchAdapter {
    fn launch<'a>(
        &'a self,
        spec: &'a rightkit_ffmpeg::agent_media::CommandSpec,
        workspace_root: &'a std::path::Path,
        cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<rightkit_ffmpeg::agent_media::ManagedProcessOutput, String>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let launcher = NativeCommandSandboxAdapter;
            let mut child = launcher
                .spawn(CommandLaunchSpec {
                    program: spec.program.clone(),
                    argv: spec.args.clone(),
                    cwd: workspace_root.to_string_lossy().into_owned(),
                    context: ProcessLaunchContext {
                        workspace_root: workspace_root.to_string_lossy().into_owned(),
                        sandbox_profile: Some("media".to_string()),
                        sandbox_required: true,
                        network_required: self.network_required,
                    },
                    env: Vec::new(),
                })
                .await
                .map_err(|error| error.to_string())?;
            let process_id = child.pid().map(|pid| pid.to_string());
            let stdout = child.take_stdout();
            let stderr = child.take_stderr();
            // Bounded capture: output past the cap is drained and dropped, so a
            // process streaming gigabytes cannot exhaust the daemon's memory.
            let (stdout_task, stdout_kept) = spawn_capped_reader(stdout);
            let (stderr_task, stderr_kept) = spawn_capped_reader(stderr);
            let wait = async {
                if let Some(mut cancel_rx) = cancel_rx {
                    tokio::select! {
                        status = child.wait() => status.map_err(|error| error.to_string()),
                        _ = tokio::time::sleep(Duration::from_millis(spec.timeout_ms)) => Err("timeout".to_string()),
                        _ = cancel_rx.changed() => Err("cancelled".to_string()),
                    }
                } else {
                    tokio::time::timeout(Duration::from_millis(spec.timeout_ms), child.wait())
                        .await
                        .map_err(|_| "timeout".to_string())?
                        .map_err(|error| error.to_string())
                }
            }
            .await;
            let status = match wait {
                Ok(status) => status,
                Err(reason) => {
                    let _ = child.cancel().await;
                    // Reap before releasing command ACLs/workspace ownership.
                    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                    stdout_task.abort();
                    stderr_task.abort();
                    return Err(format!(
                        "{reason}:{}",
                        process_id.as_deref().unwrap_or("unknown")
                    ));
                }
            };
            // The child has exited, but a background grandchild can still hold its pipes open,
            // and then EOF never comes. The readers get a short grace, then the call returns with
            // what the child wrote.
            let stdout = finish_reader(stdout_task, &stdout_kept, READER_GRACE_AFTER_EXIT).await?;
            let stderr = finish_reader(stderr_task, &stderr_kept, READER_GRACE_AFTER_EXIT).await?;
            Ok(rightkit_ffmpeg::agent_media::ManagedProcessOutput {
                output: std::process::Output {
                    status: exit_status(status),
                    stdout,
                    stderr,
                },
                process_id,
            })
        })
    }
}

/// How long a media child's output readers may run on after the child exited.
const READER_GRACE_AFTER_EXIT: Duration = Duration::from_secs(2);

/// Most bytes kept from one stream of a media subprocess: twice the 50 000-byte limit
/// `rightkit_ffmpeg::agent_media` puts on the text it returns, which leaves room for its
/// truncation marker. The rest of the stream is read and discarded so the child never blocks.
const MAX_CAPTURED_STREAM_BYTES: usize = 100_000;

type CapturedBytes = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// Drain `stream` to EOF on its own task, keeping at most [`MAX_CAPTURED_STREAM_BYTES`] in a
/// buffer the caller can still read if the task has to be abandoned.
fn spawn_capped_reader<R>(
    stream: Option<R>,
) -> (tokio::task::JoinHandle<Result<(), String>>, CapturedBytes)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let kept = CapturedBytes::default();
    let sink = kept.clone();
    let task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let Some(mut stream) = stream else {
            return Ok(());
        };
        let cap = MAX_CAPTURED_STREAM_BYTES;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = stream
                .read(&mut buffer)
                .await
                .map_err(|error| error.to_string())?;
            if read == 0 {
                return Ok(());
            }
            let mut kept = sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let room = cap.saturating_sub(kept.len());
            kept.extend_from_slice(&buffer[..read.min(room)]);
        }
    });
    (task, kept)
}

/// Wait at most `grace` for a reader to reach EOF; past it, stop the reader and keep what it
/// captured. A read error still fails, as before.
async fn finish_reader(
    task: tokio::task::JoinHandle<Result<(), String>>,
    kept: &CapturedBytes,
    grace: Duration,
) -> Result<Vec<u8>, String> {
    let abort = task.abort_handle();
    match tokio::time::timeout(grace, task).await {
        Ok(joined) => joined.map_err(|error| error.to_string())??,
        Err(_) => abort.abort(),
    }
    Ok(std::mem::take(
        &mut *kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    ))
}

#[cfg(windows)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code as u32)
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code << 8)
}

/// The full set of `host_media` tool names, in stable order. The
/// names appear here (not in `rightkit_ffmpeg::agent_media::native_media_tools()`)
/// so the toolset gateway and tests have a single source of truth.
pub const MEDIA_TOOL_NAMES: &[&str] = &[
    "ocr",
    "ocr_onnx",
    "web_extract",
    "media_fetch",
    "ffmpeg_extract",
];

/// OS-native OCR through RightKit, native backend only (no ONNX fallback): the newline-joined line text.
fn native_ocr(path: &std::path::Path) -> Result<String, String> {
    let mut ocr = rightkit_ocr::Ocr::new(rightkit_ocr::OcrOptions::new(
        rightkit_ocr::BackendPreference::Native,
    ));
    ocr.recognize_file(path)
        .map(|result| result.text())
        .map_err(|e| e.to_string())
}

#[async_trait::async_trait]
impl Tool for rightkit_ffmpeg::agent_media::NativeMediaTool {
    fn name(&self) -> &str {
        rightkit_ffmpeg::agent_media::NativeMediaTool::name(self)
    }

    fn description(&self) -> &str {
        rightkit_ffmpeg::agent_media::NativeMediaTool::description(self)
    }

    fn input_schema(&self) -> serde_json::Value {
        rightkit_ffmpeg::agent_media::NativeMediaTool::input_schema(self)
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolResult {
        let media_ctx = rightkit_ffmpeg::agent_media::MediaExecutionContext {
            workspace_root: ctx.workspace_root.clone(),
            permission_mode: ctx.permission_mode,
            workspace_trust: ctx.workspace_trust,
            execution_backend: ctx.execution_backend.clone(),
            execution_network_profile: ctx.execution_network_profile.clone(),
            cancel_rx: ctx.cancel_rx.clone(),
        };
        // Live daemon owner supplies process lifecycle adapter explicitly;
        // media crate no longer owns an uninjectable raw spawn seam.
        rightkit_ffmpeg::agent_media::set_ocr_provider(native_ocr);
        let adapter = GovernedMediaProcessLaunchAdapter {
            network_required: ctx.execution_network_profile.is_some(),
        };
        let result = self.execute_with_adapter(args, &media_ctx, &adapter).await;
        ToolResult {
            content: result.content,
            is_error: result.is_error,
            process_id: result.process_id,
            media_type: None,
            mutations: Vec::new(),
            #[cfg(feature = "host")]
            sandbox_denial: None,
        }
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::Media)
    }
}

// Media tools register individually through the `builtin_tool_factories`
// table in `registry/registry_impl.rs` (one descriptor/factory authority,
// CQ-16); the `impl Tool for NativeMediaTool` bridge below is what those
// factories use. They are filterable through the toolset gateway
// (`--allowed-tools` / `--disallowed-tools` / `--toolset`) like every
// other tool — no hardcoded always-on / always-off list.
