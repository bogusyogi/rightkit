use super::super::helpers::{available_languages, executable_on_path, node_runtime_available};
use super::super::ReplTool;
use super::part_01::test_workspace;
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::Tool;
use rightkit_agent_policy::PermissionMode;
use rightkit_agent_policy::WorkspaceTrust;
use rightkit_sandbox::execution::{ManagedChild, ProcessLaunchAdapter};
use serde_json::json;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

#[tokio::test]
async fn test_python_eval_reports_generated_workspace_artifacts() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let workspace = test_workspace("artifacts");
    let ctx = ToolContext::new(
        "test",
        workspace.clone(),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error);
    let session_id = start.content.split("session: ").nth(1).unwrap();

    let result = tool
            .execute(
                json!({
                    "operation": "eval",
                    "session_id": session_id,
                    "code": "from pathlib import Path\nPath('plots').mkdir(exist_ok=True)\nPath('plots/chart.txt').write_text('points')"
                }),
                &ctx,
            )
            .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(workspace.join("plots").join("chart.txt").exists());
    assert!(result.content.contains("artifacts:"), "{}", result.content);
    assert!(
        result.content.contains("plots/chart.txt"),
        "{}",
        result.content
    );
    assert!(result.content.contains("new"), "{}", result.content);
}

#[tokio::test]
async fn test_python_eval_reports_image_artifact_metadata() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let workspace = test_workspace("image-artifact");
    let ctx = ToolContext::new(
        "test",
        workspace.clone(),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error);
    let session_id = start.content.split("session: ").nth(1).unwrap();

    let result = tool
            .execute(
                json!({
                    "operation": "eval",
                    "session_id": session_id,
                    "code": "from pathlib import Path\nimport base64\nPath('plots').mkdir(exist_ok=True)\nPath('plots/pixel.png').write_bytes(base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII='))"
                }),
                &ctx,
            )
            .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(workspace.join("plots").join("pixel.png").exists());
    assert!(
        result.content.contains("plots/pixel.png"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("image/png 1x1"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn test_python_eval_surfaces_traceback() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let ctx = ToolContext::new(
        "test",
        test_workspace("python-eval"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error);
    let session_id = start.content.split("session: ").nth(1).unwrap();

    let result = tool
            .execute(
                json!({"operation": "eval", "session_id": session_id, "code": "raise RuntimeError('boom')"}),
                &ctx,
            )
            .await;

    assert!(result.is_error);
    assert!(result.content.contains("RuntimeError"));
    assert!(result.content.contains("boom"));
}

#[tokio::test]
async fn test_status_lists_active_sessions() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let ctx = ToolContext::new(
        "test",
        test_workspace("python-traceback"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let empty = tool.execute(json!({"operation": "status"}), &ctx).await;
    assert!(!empty.is_error);
    assert!(empty.content.contains("No active REPL sessions"));

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error);
    let session_id = start.content.split("session: ").nth(1).unwrap();

    let status = tool.execute(json!({"operation": "status"}), &ctx).await;
    assert!(!status.is_error);
    assert!(status.content.contains(session_id));
    assert!(status.content.contains("language=python"));

    let one = tool
        .execute(
            json!({"operation": "status", "session_id": session_id}),
            &ctx,
        )
        .await;
    assert!(!one.is_error);
    assert!(one.content.contains(session_id));
}

#[tokio::test]
async fn test_reset_keeps_session_id_and_clears_python_state() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let ctx = ToolContext::new(
        "test",
        test_workspace("reset"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error);
    let session_id = start.content.split("session: ").nth(1).unwrap();

    let set_value = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "x = 41"}),
            &ctx,
        )
        .await;
    assert!(!set_value.is_error);

    let reset = tool
        .execute(
            json!({"operation": "reset", "session_id": session_id}),
            &ctx,
        )
        .await;
    assert!(!reset.is_error);
    assert!(reset.content.contains(session_id));

    let after = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "x"}),
            &ctx,
        )
        .await;
    assert!(after.is_error);
    assert!(after.content.contains("NameError"));
}

#[tokio::test]
async fn test_close_session() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let ctx = ToolContext::new(
        "test",
        test_workspace("status"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    // Start a session
    let start_result = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start_result.is_error);

    // Extract session ID from result
    let session_id = start_result
        .content
        .split("session: ")
        .nth(1)
        .unwrap_or("test_session");

    // Close the session
    let close_result = tool
        .execute(
            json!({"operation": "close", "session_id": session_id}),
            &ctx,
        )
        .await;

    assert!(!close_result.is_error);
}

/// A scripted launch adapter whose child replays canned stdout frames —
/// for tests that need a deterministic protocol stream instead of a real
/// interpreter.
struct ScriptedLaunchAdapter {
    canned_stdout: std::sync::Mutex<Option<String>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct ScriptedChild {
    stdout: Option<Pin<Box<dyn AsyncRead + Send>>>,
    // Holding the writer keeps the stream open after the canned bytes,
    // so a script that stops producing output exercises the eval timeout.
    _writer_hold: Option<tokio::io::DuplexStream>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl ProcessLaunchAdapter for ScriptedLaunchAdapter {
    async fn spawn(
        &self,
        _spec: rightkit_sandbox::execution::CommandLaunchSpec,
    ) -> Result<Box<dyn ManagedChild>, rightkit_sandbox::execution::EffectExecutionError> {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let canned = self
            .canned_stdout
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(canned) = canned {
            writer
                .write_all(canned.as_bytes())
                .await
                .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::SpawnFailed)?;
        }
        Ok(Box::new(ScriptedChild {
            stdout: Some(Box::pin(reader)),
            _writer_hold: Some(writer),
            cancelled: self.cancelled.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl ManagedChild for ScriptedChild {
    fn pid(&self) -> Option<u32> {
        Some(4321)
    }
    fn take_stdin(&mut self) -> Option<Pin<Box<dyn AsyncWrite + Send>>> {
        Some(Box::pin(tokio::io::sink()))
    }
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        self.stdout.take()
    }
    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        None
    }
    async fn wait(&mut self) -> Result<i32, rightkit_sandbox::execution::EffectExecutionError> {
        Ok(0)
    }
    async fn cancel(&mut self) -> Result<(), rightkit_sandbox::execution::EffectExecutionError> {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn repl_start_fails_closed_without_injected_process_authority() {
    let tool = ReplTool::new();
    let ctx = ToolContext::new(
        "test",
        test_workspace("no-authority"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let result = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;

    assert!(result.is_error);
    assert!(
        result.content.contains("resource_limits_missing"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn repl_start_fails_closed_when_governed_launch_is_unconfined() {
    // Mirrors the Windows Job-Object fallback: an adapter that claims a
    // sandboxed launch but returns an unconfined child must not run
    // model-authored code.
    let tool = ReplTool::new();
    let ctx = ToolContext::new(
        "test",
        test_workspace("unconfined-governed"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    )
    .with_process_launch(
        std::sync::Arc::new(crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter),
        rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: String::new(),
            sandbox_profile: None,
            sandbox_required: false,
            network_required: false,
        },
    );

    let result = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;

    assert!(result.is_error);
    assert!(
        result.content.contains("requires confinement"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn eval_uncorrelated_response_terminates_session() {
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(ScriptedLaunchAdapter {
        canned_stdout: std::sync::Mutex::new(Some(
            "{\"id\":42,\"ok\":true,\"stdout\":\"\",\"stderr\":\"\",\"result\":\"\"}\n".to_string(),
        )),
        cancelled: cancelled.clone(),
    }));
    let ctx = ToolContext::new(
        "test",
        test_workspace("uncorrelated"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error, "{}", start.content);
    let session_id = start.content.split("session: ").nth(1).unwrap().to_string();

    let result = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "1 + 1"}),
            &ctx,
        )
        .await;
    assert!(result.is_error);
    assert!(
        result.content.contains("uncorrelated"),
        "{}",
        result.content
    );
    assert!(
        cancelled.load(std::sync::atomic::Ordering::SeqCst),
        "a desynced child must be cancelled so no stale frame survives"
    );

    let again = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "2 + 2"}),
            &ctx,
        )
        .await;
    assert!(again.is_error);
    assert!(again.content.contains("not found"), "{}", again.content);
}

#[tokio::test]
async fn eval_timeout_settles_and_terminates_session() {
    // The scripted child keeps stdout open but never produces a frame:
    // the eval deadline must settle the request by killing the child so
    // a late response can never be consumed by a subsequent evaluation.
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(ScriptedLaunchAdapter {
        canned_stdout: std::sync::Mutex::new(None),
        cancelled: cancelled.clone(),
    }));
    let ctx = ToolContext::new(
        "test",
        test_workspace("eval-timeout"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error, "{}", start.content);
    let session_id = start.content.split("session: ").nth(1).unwrap().to_string();

    let result = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "while True: pass"}),
            &ctx,
        )
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("timed out"), "{}", result.content);
    assert!(
        cancelled.load(std::sync::atomic::Ordering::SeqCst),
        "the timed-out child must be cancelled — the request is settled, not pending"
    );

    let again = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "1 + 1"}),
            &ctx,
        )
        .await;
    assert!(again.is_error);
    assert!(again.content.contains("not found"), "{}", again.content);
}

#[test]
fn language_enum_advertises_only_resolvable_runtimes() {
    // CQ-16: the schema's `language` enum is the model-facing
    // advertisement — it may only contain runtimes whose executable
    // resolves on this host, and the probe output is what it serves.
    let langs = available_languages();
    for lang in langs {
        match *lang {
            "python" => assert!(executable_on_path(if cfg!(windows) {
                "python"
            } else {
                "python3"
            })),
            "node" => assert!(node_runtime_available()),
            other => panic!("unexpected advertised language {other}"),
        }
    }

    let schema = ReplTool::new().input_schema();
    let advertised: Vec<&str> = schema["properties"]["language"]["enum"]
        .as_array()
        .expect("language enum")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(advertised, langs, "schema must serve the probe result");
}
