fn openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/provider-auth/{provider}/login") => Some(serde_json::json!({"type": "object"})),
        ("post", "/provider-auth/{provider}/login/{challenge}/submit") => Some(serde_json::json!({
            "type": "object", "required": ["value"], "properties": {"value": {"type": "string"}}
        })),
        ("post", "/commands/execute") => {
            Some(crate::api_commands::execute_command_request_schema())
        }
        ("post", "/hooks/test") => Some(crate::hooks::hook_test_request_schema()),
        ("post", "/hooks/revoke") => Some(crate::hooks::hook_revoke_request_schema()),
        ("post", "/tool-schemas/validate") => {
            Some(crate::api_tool_schemas::validate_tool_schema_request_schema())
        }
        ("patch", "/execution/backends/default") => {
            Some(crate::api_execution::set_execution_backend_default_request_schema())
        }
        ("patch", "/execution/backends/{id}") => {
            Some(crate::api_execution::update_execution_backend_request_schema())
        }
        ("put", "/execution/network-profiles/{id}") => {
            Some(crate::api_execution::save_execution_network_profile_request_schema())
        }
        ("post", "/execution/workspaces") => {
            Some(crate::api_execution::create_execution_workspace_request_schema())
        }
        ("post", "/execution/processes") => {
            Some(crate::api_execution::create_execution_process_request_schema())
        }
        ("post", "/execution/processes/{id}/stdin") => {
            Some(crate::api_execution::write_execution_process_stdin_request_schema())
        }
        ("post", "/execution/processes/{id}/resize") => {
            Some(crate::api_execution::resize_execution_process_request_schema())
        }
        ("put", "/settings/orchestration") => {
            Some(crate::api_settings::save_settings_request_schema())
        }
        ("put", "/settings/toolsets/{id}") => {
            crate::api_settings::settings_openapi_request_schema(method, path)
        }
        ("put", "/provider-keys/{provider}") => {
            Some(crate::api_settings::save_provider_key_request_schema())
        }
        ("post", "/inference/connections/direct")
        | ("put", "/inference/connections/direct/{id}") => {
            Some(crate::api_settings::inference_connection_request_schema())
        }
        ("put", "/inference/connections/direct/{id}/secret") => {
            Some(crate::api_settings::inference_secret_request_schema())
        }
        ("post", "/inference/connections/{authority}/{id}/probe") => {
            Some(crate::api_settings::inference_probe_request_schema())
        }
        ("post", "/memory") => crate::api_memory::memory_openapi_request_schema(method, path),
        ("put", "/runtime/middleware/{id}/enabled") => {
            Some(crate::api_runtime::set_runtime_middleware_enabled_request_schema())
        }
        ("put", "/sandbox/settings") => {
            Some(crate::api_health::save_sandbox_settings_request_schema())
        }
        ("post", "/settings/reload") => Some(crate::api_settings::reload_settings_request_schema()),
        ("post", "/sessions/{id}/permission-mode") => {
            Some(crate::api_permissions::set_permission_mode_request_schema())
        }
        ("post", "/sessions/{id}/permission-rules") => {
            Some(crate::api_permissions::add_permission_rule_request_schema())
        }
        ("post", "/sessions/{id}/answer-question") => {
            Some(crate::api::answer_question_request_schema())
        }
        ("post", "/sessions/{id}/resolve-plan") => Some(crate::api::resolve_plan_request_schema()),
        ("post", "/sessions/{id}/steer") => Some(crate::api::steer_session_request_schema()),
        ("post", "/sessions/{id}/goal") => Some(crate::api::set_goal_request_schema()),
        ("post", "/sessions/{id}/goal/edit") => Some(crate::api::edit_goal_request_schema()),
        ("post", "/sessions/{id}/orchestrate") => {
            Some(crate::api::orchestrate_session_request_schema())
        }
        ("post", "/sessions/{id}/driver-capabilities/leases") => {
            Some(crate::api_drivers::issue_capability_lease_request_schema())
        }
        ("post", "/sessions/{id}/driver-capabilities/call") => {
            Some(crate::api_drivers::capability_call_request_schema())
        }
        ("post", "/sessions/{id}/driver/start") => {
            Some(crate::api_drivers::start_driver_request_schema())
        }
        ("post", "/sessions/{id}/driver/prompt") => {
            Some(crate::api_drivers::driver_prompt_request_schema())
        }
        ("post", "/sessions/{id}/driver/interrupt") => {
            Some(crate::api_drivers::driver_interrupt_request_schema())
        }
        ("post", "/sessions/{id}/driver/resume") | ("post", "/sessions/{id}/driver/stop") => {
            Some(crate::api_drivers::driver_binding_request_schema())
        }
        ("post", "/permission-rules") => {
            Some(crate::api_permissions::add_persistent_permission_rule_request_schema())
        }
        ("post", "/permission-rules/import") => {
            Some(crate::api_permissions::permission_rule_store_schema())
        }
        ("patch", "/workspace/trust") => {
            Some(crate::api_permissions::workspace_trust_request_schema())
        }
        ("post", "/local-models/probe") => {
            Some(crate::api_local_models::probe_local_model_request_schema())
        }
        ("post", "/local-models/chat-smoke") => {
            Some(crate::api_local_models::chat_smoke_local_model_request_schema())
        }
        ("post", "/local-models/add-provider") => {
            Some(crate::api_local_models::add_local_model_provider_request_schema())
        }
        ("post", "/remote/channels/{id}/profile") => {
            Some(crate::api_remote::remote_channel_profile_request_schema())
        }
        ("post", "/remote/channels/{id}/fanout") => {
            Some(crate::api_remote::remote_channel_fanout_request_schema())
        }
        ("post", "/remote/channels/{id}/outbound") => {
            Some(crate::api_remote::remote_channel_outbound_request_schema())
        }
        ("post", "/remote/bindings") => Some(crate::api_remote::remote_binding_request_schema()),
        ("post", "/recipes/install") => Some(crate::api_recipes::install_recipe_request_schema()),
        ("post", "/recipes/install-file") => {
            Some(crate::api_recipes::install_recipe_file_request_schema())
        }
        ("post", "/recipes/install-url") => {
            Some(crate::api_recipes::install_recipe_url_request_schema())
        }
        ("post", "/recipes/validate") => Some(crate::api_recipes::validate_recipe_request_schema()),
        ("post", "/recipes/render") => Some(crate::api_recipes::render_recipe_request_schema()),
        ("post", "/recipes/deeplink/parse") => {
            Some(crate::api_recipes::parse_recipe_deeplink_request_schema())
        }
        ("post", "/recipes/{id}/run") => Some(crate::api_recipes::run_recipe_request_schema()),
        ("post", "/recipes/{id}/schedule") => {
            Some(crate::api_recipes::schedule_recipe_request_schema())
        }
        ("post", "/recipes/{id}/deeplink") => {
            Some(crate::api_recipes::create_recipe_deeplink_request_schema())
        }
        ("post", "/tool-invocations/{id}/cancel") => {
            Some(crate::api_tool_invocations::cancel_tool_invocation_request_schema())
        }
        ("post", "/execution/backends/{id}/doctor")
        | ("post", "/execution/processes/{id}/kill") => {
            Some(crate::api_execution::execution_action_request_schema())
        }
        ("post", "/notifications/{id}/dismiss") => {
            Some(crate::api_notifications::dismiss_notification_request_schema())
        }
        ("post", "/skills/provenance-review") => {
            Some(crate::api_skills::skill_provenance_review_request_schema())
        }
        _ => crate::api_triggers::trigger_openapi_request_schema(method, path)
            .or_else(|| crate::api_runtime::runtime_openapi_request_schema(method, path))
            .or_else(|| crate::api::session_openapi_request_schema(method, path))
            .or_else(|| crate::api_benchmarks::benchmark_openapi_request_schema(method, path))
            .or_else(|| crate::api_settings::settings_openapi_request_schema(method, path))
            .or_else(|| crate::api_plugins::plugins_openapi_request_schema(method, path))
            .or_else(|| crate::api_skills::skills_openapi_request_schema(method, path))
            .or_else(|| crate::api_mcp::mcp_openapi_request_schema(method, path))
            .or_else(|| crate::api_lsp::lsp_openapi_request_schema(method, path))
            .or_else(|| crate::api_specs::specs_openapi_request_schema(method, path))
            .or_else(|| crate::api_schedules::schedule_openapi_request_schema(method, path))
            .or_else(|| crate::api_workspace::workspace_openapi_request_schema(method, path))
            .or_else(|| {
                crate::api_browser_workspace::browser_workspace_openapi_request_schema(method, path)
            })
            .or_else(|| crate::api_delivery::delivery_openapi_request_schema(method, path))
            .or_else(|| crate::api_environments::environments_openapi_request_schema(method, path))
            .or_else(|| {
                rightkit_agent_engine_attention::attention_openapi_request_schema(method, path)
            })
            .or_else(|| run_changes_contract_request_schema(method, path)),
    }
}
