use super::super::ReplTool;
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::Tool;
use rightkit_agent_policy::PermissionMode;
use rightkit_agent_policy::WorkspaceTrust;
use serde_json::json;
use std::path::PathBuf;

pub(super) fn test_workspace(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("rightkit-agent-test-tmp")
        .join(format!("repl-{label}-{nanos}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[tokio::test]
async fn test_start_python_session() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let ctx = ToolContext::new(
        "test",
        test_workspace("python-start"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let result = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("python"));
}

#[tokio::test]
async fn test_start_node_session() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let ctx = ToolContext::new(
        "test",
        test_workspace("node-start"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let result = tool
        .execute(json!({"operation": "start", "language": "node"}), &ctx)
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("node"));
}

#[tokio::test]
async fn repl_start_rejects_nonlocal_execution_backend_before_host_spawn() {
    let tool = ReplTool::new();
    let ctx = ToolContext::new(
        "test",
        test_workspace("nonlocal-backend"),
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    )
    .with_execution_backend("missing-backend");

    let result = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;

    assert!(result.is_error);
    assert!(
        result
            .content
            .contains("repl executionBackend 'missing-backend'"),
        "{}",
        result.content
    );
    assert!(
        tool.sessions.lock().await.is_empty(),
        "non-local REPL backend must not create a host process session"
    );
}

#[tokio::test]
async fn repl_start_rejects_restricted_network_profile_before_host_spawn() {
    let tool = ReplTool::new();
    let ctx = ToolContext::new(
        "test",
        test_workspace("restricted-network"),
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    )
    .with_execution_backend("local")
    .with_execution_network_profile("egress-restricted");

    let result = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;

    assert!(result.is_error);
    assert!(
        result
            .content
            .contains("repl networkProfile 'egress-restricted'"),
        "{}",
        result.content
    );
    assert!(
        tool.sessions.lock().await.is_empty(),
        "restricted network REPL must not create a host process session"
    );
}

#[tokio::test]
async fn test_unsupported_language() {
    let tool = ReplTool::new();
    let ctx = ToolContext::new(
        "test",
        test_workspace("unsupported"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let result = tool
        .execute(json!({"operation": "start", "language": "rust"}), &ctx)
        .await;

    assert!(result.is_error);
}

#[tokio::test]
async fn test_eval_nonexistent_session() {
    let tool = ReplTool::new();
    let ctx = ToolContext::new(
        "test",
        test_workspace("missing-session"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let result = tool
        .execute(
            json!({"operation": "eval", "session_id": "nonexistent", "code": "print('hi')"}),
            &ctx,
        )
        .await;

    assert!(result.is_error);
}

#[tokio::test]
async fn test_python_eval_returns_real_stdout_and_keeps_state() {
    let tool = ReplTool::new_with_launcher(std::sync::Arc::new(
        crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter,
    ));
    let ctx = ToolContext::new(
        "test",
        test_workspace("close"),
        PermissionMode::Plan,
        WorkspaceTrust::Trusted,
    );

    let start = tool
        .execute(json!({"operation": "start", "language": "python"}), &ctx)
        .await;
    assert!(!start.is_error);
    let session_id = start.content.split("session: ").nth(1).unwrap();

    let first = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "x = 41\nprint(x + 1)"}),
            &ctx,
        )
        .await;
    assert!(!first.is_error);
    assert!(first.content.contains("42"));

    let second = tool
        .execute(
            json!({"operation": "eval", "session_id": session_id, "code": "x + 1"}),
            &ctx,
        )
        .await;
    assert!(!second.is_error);
    assert!(second.content.contains("42"));
}
