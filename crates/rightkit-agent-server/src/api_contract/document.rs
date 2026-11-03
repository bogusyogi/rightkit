fn openapi_document() -> Value {
    let mut paths = Map::new();
    for endpoint in DAEMON_API_ENDPOINTS {
        let openapi_path = openapi_path(endpoint.path);
        let method = endpoint.method.to_ascii_lowercase();
        let path_item = paths
            .entry(openapi_path.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(methods) = path_item {
            methods.insert(
                method.clone(),
                openapi_operation(endpoint, &openapi_path, &method),
            );
        }
    }

    let mut schemas = crate::api_tool_schemas::tool_schemas_openapi_components();
    schemas.extend(crate::api_health::health_openapi_components());
    schemas.extend(crate::api_coordination::coordination_openapi_components());
    schemas.extend(crate::api_memory::memory_openapi_components());
    schemas.extend(crate::api::session_openapi_components());
    schemas.extend(crate::api_compaction_archives::compaction_archive_openapi_components());
    schemas.extend(crate::api_tool_invocations::tool_invocation_openapi_components());
    schemas.extend(crate::api_commands::command_openapi_components());
    schemas.extend(crate::api_toolsets::toolset_openapi_components());
    schemas.extend(crate::api_execution::execution_openapi_components());
    schemas.extend(crate::api_settings::settings_openapi_components());
    schemas.extend(crate::api_runtime::runtime_openapi_components());
    schemas.extend(crate::api_benchmarks::benchmark_openapi_components());
    schemas.extend(crate::api_permissions::permission_openapi_components());
    schemas.extend(crate::api_local_models::local_models_openapi_components());
    schemas.extend(crate::api_recipes::recipes_openapi_components());
    schemas.extend(crate::api_schedules::schedules_openapi_components());
    schemas.extend(crate::api_specs::specs_openapi_components());
    schemas.extend(crate::api_notifications::notifications_openapi_components());
    schemas.extend(crate::api_remote::remote_openapi_components());
    schemas.extend(crate::api_skills::skills_openapi_components());
    schemas.extend(crate::api_plugins::plugin_openapi_components());
    schemas.extend(crate::api_mcp::mcp_openapi_components());
    schemas.extend(crate::api_lsp::lsp_openapi_components());
    schemas.extend(crate::api_triggers::trigger_openapi_components());
    schemas.extend(crate::api_session_handoff::session_handoff_openapi_components());
    schemas.extend(crate::hooks::hooks_openapi_components());
    schemas.extend(api_contract_openapi_components());
    schemas.extend(run_changes_contract_components());
    schemas.extend(crate::api_workspace::workspace_openapi_components());
    schemas.extend(crate::api_browser_workspace::browser_workspace_openapi_components());
    schemas.extend(crate::api_delivery::delivery_openapi_components());
    schemas.extend(crate::api_environments::environments_openapi_components());
    schemas.extend(rightkit_agent_engine_attention::attention_openapi_components());
    let security_schemes = json!({
        "daemonBearer": {
            "type": "http",
            "scheme": "bearer",
            "description": "Canonical daemon authentication carrier."
        }
    });
    let mut components = Map::new();
    components.insert("securitySchemes".to_string(), security_schemes);
    components.insert("schemas".to_string(), Value::Object(schemas));

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "RightKit Daemon API",
            "version": "1",
            "description": "Generated from the daemon endpoint manifest. Response schemas are intentionally generic until schema-level contracts are promoted."
        },
        "paths": paths,
        "components": components,
        "security": [{"daemonBearer": []}]
    })
}
