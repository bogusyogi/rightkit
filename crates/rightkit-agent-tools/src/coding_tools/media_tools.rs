//! `media_tools` — registers the `host_media` crate's native media
//! tools (OCR / transcribe / web-extract / media-fetch / ffmpeg-extract)
//! as first-class agent tools in the [`ToolRegistry`].
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

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::shell::NativeCommandSandboxAdapter;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_sandbox::execution::{
    CommandLaunchSpec, ProcessLaunchAdapter as _, ProcessLaunchContext,
};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::AsyncReadExt;

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
            let stdout_task = tokio::spawn(async move {
                let mut bytes = Vec::new();
                if let Some(mut stream) = stdout {
                    stream
                        .read_to_end(&mut bytes)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(bytes)
            });
            let stderr_task = tokio::spawn(async move {
                let mut bytes = Vec::new();
                if let Some(mut stream) = stderr {
                    stream
                        .read_to_end(&mut bytes)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(bytes)
            });
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
            let stdout = stdout_task.await.map_err(|error| error.to_string())??;
            let stderr = stderr_task.await.map_err(|error| error.to_string())??;
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
    "transcribe",
    "web_extract",
    "media_fetch",
    "ffmpeg_extract",
];

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
            #[cfg(feature = "coding_tools-host")]
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

#[cfg(test)]
#[path = "media_tools_tests.rs"]
mod media_tools_tests;
