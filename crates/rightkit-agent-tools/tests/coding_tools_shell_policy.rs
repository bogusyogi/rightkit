use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use rightkit_agent_tools::coding_tools::shell::{PowerShellTool, ShellTool};
use rightkit_agent_tools::coding_tools::{Tool, ToolContext};
use serde_json::json;
use tempfile::tempdir;

#[cfg(not(windows))]
#[tokio::test]
async fn autosafe_runs_a_routine_posix_pipeline() {
    let workspace = tempdir().unwrap();
    let context = ToolContext::new(
        "shell-policy",
        workspace.path(),
        PermissionMode::AutoSafe,
        WorkspaceTrust::Trusted,
    );

    let result = ShellTool
        .execute(json!({"command": "printf hello | wc -c"}), &context)
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains('5'), "{}", result.content);
}

#[tokio::test]
async fn autosafe_rejects_a_nonleading_risky_segment_before_spawn() {
    let workspace = tempdir().unwrap();
    let sentinel = workspace.path().join("sentinel");
    std::fs::create_dir(&sentinel).unwrap();
    let context = ToolContext::new(
        "shell-policy",
        workspace.path(),
        PermissionMode::AutoSafe,
        WorkspaceTrust::Trusted,
    );

    let result = ShellTool
        .execute(json!({"command": "true && rm -rf sentinel"}), &context)
        .await;

    assert!(result.is_error);
    assert!(
        result.content.contains("requires approval"),
        "{}",
        result.content
    );
    assert!(sentinel.exists(), "command must be rejected before spawn");
}

#[tokio::test]
async fn powershell_autosafe_fails_closed_before_spawn() {
    let workspace = tempdir().unwrap();
    let context = ToolContext::new(
        "shell-policy",
        workspace.path(),
        PermissionMode::AutoSafe,
        WorkspaceTrust::Trusted,
    );

    let result = PowerShellTool
        .execute(
            json!({"command": "Get-ChildItem; Remove-Item -Recurse target"}),
            &context,
        )
        .await;

    assert!(result.is_error);
    assert!(
        result.content.contains("powershell-fail-closed"),
        "{}",
        result.content
    );
}
