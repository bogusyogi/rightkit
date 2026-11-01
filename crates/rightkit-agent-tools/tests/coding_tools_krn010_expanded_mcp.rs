#![cfg(windows)]

use rightkit_agent_engine_runtime_resources::{ResourceKind, ResourceProfile, ServiceScope};
use rightkit_agent_tools::coding_tools::process_resource_authority::{
    install_process_resource_profile, GovernedProcessLaunchAdapter,
};
use rightkit_mcp::agent_mcp::{McpTransport, StdioTransport};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "current_thread")]
#[ignore = "explicitly launches one governed stdio MCP fixture process"]
async fn expanded_real_stdio_mcp_lists_calls_and_releases() {
    let root = tempfile::tempdir().expect("create MCP workspace");
    let test_executable = std::env::current_exe().expect("resolve test executable");
    let fixture = test_executable
        .parent()
        .expect("test deps directory")
        .parent()
        .expect("test profile directory")
        .join("examples")
        .join("krn010_mcp_fixture.exe");
    assert!(
        fixture.is_file(),
        "MCP fixture executable must exist: {}",
        fixture.display()
    );

    let governor = install_process_resource_profile(ResourceProfile::Expanded)
        .unwrap_or_else(|existing| panic!("resource profile must be Expanded, found {existing:?}"));
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: root.path().to_string_lossy().into_owned(),
        sandbox_profile: Some("workspace-write".to_string()),
        sandbox_required: true,
        network_required: false,
    };
    let adapter = Arc::new(GovernedProcessLaunchAdapter::new(
        rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter,
        governor.clone(),
        ResourceKind::Mcp,
        true,
    ));
    let transport = StdioTransport::new(
        fixture.to_string_lossy().into_owned(),
        Vec::new(),
        HashMap::new(),
    )
    .with_process_launch_adapter(adapter)
    .with_launch_context(context);

    let tools = transport.list_tools().await.expect("list real MCP tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "krn010_echo");

    let scope = ServiceScope::Workspace(root.path().to_string_lossy().into_owned());
    assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 1);
    assert_eq!(governor.usage(ResourceKind::Mcp, &scope), 1);

    let result = transport
        .call_tool("krn010_echo", serde_json::json!({}), Some("krn010"))
        .await
        .expect("call real MCP tool");
    assert!(!result.is_error);
    assert_eq!(result.content, "KRN010 MCP OK");
    drop(transport);

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && (governor.usage(ResourceKind::ExternalExecution, &scope) != 0
            || governor.usage(ResourceKind::Mcp, &scope) != 0)
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 0);
    assert_eq!(governor.usage(ResourceKind::Mcp, &scope), 0);
}
