use super::*;

#[test]
fn spawn_subagent_only_requires_process_for_authenticated_cli_runtime() {
    let registry = ToolRegistry::build_default();
    let tool = registry
        .get("spawn_subagent")
        .expect("spawn_subagent must be registered");

    assert_eq!(tool.process_resource_kind_for(&serde_json::json!({})), None);
    assert_eq!(
        tool.process_resource_kind_for(&serde_json::json!({
            "runtime": {"kind": "rightkit_native", "modelId": "m"}
        })),
        None
    );
    assert_eq!(
        tool.process_resource_kind_for(&serde_json::json!({
            "runtime": {
                "kind": "authenticated_cli",
                "providerId": "codex",
                "accountProfileId": "codex/default"
            }
        })),
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::ChildProcess)
    );
}

#[test]
fn default_registry_preserves_lsp_process_requirement() {
    let registry = ToolRegistry::build_default();
    let tool = registry.get("lsp").expect("lsp must be registered");

    assert_eq!(
        tool.process_resource_kind_for(&serde_json::json!({
            "operation": "diagnostics",
            "path": "src/lib.rs"
        })),
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Lsp)
    );
}
