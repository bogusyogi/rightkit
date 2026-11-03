fn openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/commands") => Some(crate::api_commands::list_commands_response_schema()),
        ("post", "/commands/execute") => {
            Some(crate::api_commands::execute_command_response_schema())
        }
        ("get", "/health") => Some(crate::api_health::health_response_schema()),
        ("get", "/health/diagnostics") => {
            Some(crate::api_health::health_diagnostics_response_schema())
        }
        ("get", "/health/readiness") => {
            Some(crate::api_health::readiness_snapshot_response_schema())
        }
        ("get", "/machine/status") => Some(crate::api_health::machine_status_response_schema()),
        ("get", "/machine/info") => Some(crate::api_health::machine_info_response_schema()),
        ("get", "/sandbox/settings") | ("put", "/sandbox/settings") => {
            Some(crate::api_health::sandbox_settings_response_schema())
        }
        ("post", "/diagnostics/bundle") => {
            Some(crate::api_health::diagnostics_bundle_response_schema())
        }
        ("get", "/diagnostics/bundles/{id}") => {
            Some(crate::api_health::diagnostics_bundle_download_response_schema())
        }
        ("get", "/provider-keys/{provider}") => {
            Some(crate::api_settings::provider_key_status_response_schema())
        }
        ("get", "/provider-auth/{provider}") => Some(json!({
            "type": "object", "required": ["provider_id", "configured", "subscription_eligibility"],
            "properties": {"provider_id": {"type": "string"}, "configured": {"type": "boolean"}, "source": {"type": ["string", "null"]}, "subscription_auth_state": {}, "account_fingerprint": {"type": ["string", "null"]}, "available_model_ids": {"type": "array", "items": {"type": "string"}}, "subscription_eligibility": {"type": "string"}}
        })),
        ("post", "/provider-auth/{provider}/login") => Some(
            json!({"type": "object", "required": ["challengeId", "state"], "properties": {"challengeId": {"type": "string"}, "state": {"type": "string"}}}),
        ),
        ("post", "/provider-auth/{provider}/login/{challenge}/submit") => {
            Some(json!({"type": "object"}))
        }
        ("delete", "/provider-auth/{provider}") => Some(json!({"type": "object"})),
        ("get", "/inference/connections") => {
            Some(crate::api_settings::inference_connections_response_schema())
        }
        ("post", "/inference/connections/direct")
        | ("put", "/inference/connections/direct/{id}")
        | ("delete", "/inference/connections/direct/{id}") => {
            Some(crate::api_settings::inference_connections_response_schema())
        }
        ("put", "/inference/connections/direct/{id}/secret")
        | ("delete", "/inference/connections/direct/{id}/secret") => {
            Some(crate::api_settings::inference_secret_response_schema())
        }
        ("post", "/inference/connections/{authority}/{id}/probe") => {
            Some(crate::api_settings::inference_probe_response_schema())
        }
        ("get", "/inference/catalog") => {
            Some(crate::api_settings::inference_catalog_response_schema())
        }
        ("get", "/inference/load") => Some(crate::api_settings::inference_load_response_schema()),
        ("put", "/provider-keys/{provider}") | ("delete", "/provider-keys/{provider}") => {
            Some(crate::api_settings::provider_key_status_response_schema())
        }
        ("get", "/settings/toolsets")
        | ("put", "/settings/toolsets/{id}")
        | ("delete", "/settings/toolsets/{id}") => {
            crate::api_settings::settings_openapi_response_schema(method, path)
        }
        ("get", "/teams") | ("delete", "/teams/{name}") | ("get", "/messages") => {
            crate::api_coordination::coordination_openapi_response_schema(method, path)
        }
        ("get", "/memory") | ("post", "/memory") => {
            crate::api_memory::memory_openapi_response_schema(method, path)
        }
        ("get", "/tool-schemas") => {
            Some(crate::api_tool_schemas::tool_schemas_list_response_schema())
        }
        ("get", "/toolsets") => Some(crate::api_toolsets::toolsets_list_response_schema()),
        ("get", "/toolsets/gateways") => {
            Some(crate::api_toolsets::toolset_gateways_response_schema())
        }
        ("get", "/execution/backends") => {
            Some(crate::api_execution::execution_backend_registry_response_schema())
        }
        ("patch", "/execution/backends/default") => {
            Some(crate::api_execution::execution_backend_registry_response_schema())
        }
        ("patch", "/execution/backends/{id}") => {
            Some(crate::api_execution::execution_backend_registry_response_schema())
        }
        ("put", "/execution/network-profiles/{id}") => {
            Some(crate::api_execution::execution_backend_registry_response_schema())
        }
        ("delete", "/execution/network-profiles/{id}") => {
            Some(crate::api_execution::execution_backend_registry_response_schema())
        }
        ("post", "/execution/backends/{id}/doctor") => {
            Some(crate::api_execution::backend_doctor_report_response_schema())
        }
        ("get", "/execution/workspaces") => {
            Some(crate::api_execution::execution_workspaces_response_schema())
        }
        ("post", "/execution/workspaces") => {
            Some(crate::api_execution::create_execution_workspace_response_schema())
        }
        ("get", "/execution/processes") => {
            Some(crate::api_execution::execution_processes_response_schema())
        }
        ("post", "/execution/processes") => {
            Some(crate::api_execution::execution_process_response_schema())
        }
        ("get", "/execution/processes/{id}") => {
            Some(crate::api_execution::execution_process_response_schema())
        }
        ("get", "/execution/processes/{id}/logs") => {
            Some(crate::api_execution::execution_process_logs_response_schema())
        }
        ("post", "/execution/processes/{id}/stdin") => {
            Some(crate::api_execution::execution_process_response_schema())
        }
        ("post", "/execution/processes/{id}/resize") => {
            Some(crate::api_execution::execution_process_response_schema())
        }
        ("post", "/execution/processes/{id}/kill") => {
            Some(crate::api_execution::execution_process_response_schema())
        }
        ("get", "/sessions/{id}/tool-invocations") => {
            Some(crate::api_tool_invocations::tool_invocations_list_response_schema())
        }
        ("get", "/sessions/{id}/handoff") => {
            Some(crate::api_session_handoff::session_handoff_response_schema())
        }
        ("post", "/sessions/{id}/answer-question") => {
            Some(crate::api::answer_question_response_schema())
        }
        ("post", "/sessions/{id}/resolve-plan") => Some(crate::api::resolve_plan_response_schema()),
        ("post", "/sessions/{id}/resume") => Some(crate::api::resume_session_response_schema()),
        ("post", "/sessions/{id}/interrupt") => {
            Some(crate::api::interrupt_session_response_schema())
        }
        ("post", "/sessions/{id}/steer") => Some(crate::api::steer_session_response_schema()),
        ("get", "/sessions/{id}/compaction-archives") => {
            Some(crate::api_compaction_archives::compaction_archive_list_response_schema())
        }
        ("get", "/sessions/{id}/compaction-archives/{archive_id}") => {
            Some(crate::api_compaction_archives::compaction_archive_response_schema())
        }
        ("get", "/sessions/{id}/goal") => Some(crate::api::get_goal_response_schema()),
        ("post", "/sessions/{id}/goal") => Some(crate::api::set_goal_response_schema()),
        ("delete", "/sessions/{id}/goal") => Some(crate::api::clear_goal_response_schema()),
        ("post", "/sessions/{id}/goal/pause") => Some(crate::api::pause_goal_response_schema()),
        ("post", "/sessions/{id}/goal/resume") => Some(crate::api::resume_goal_response_schema()),
        ("post", "/sessions/{id}/goal/edit") => Some(crate::api::edit_goal_response_schema()),
        ("post", "/sessions/{id}/rewind") => Some(crate::api::rewind_session_response_schema()),
        ("post", "/sessions/{id}/orchestrate") => {
            Some(crate::api::orchestrate_session_response_schema())
        }
        ("post", "/sessions/{id}/driver-capabilities/leases") => {
            Some(crate::api_drivers::issue_capability_lease_response_schema())
        }
        ("get", "/drivers/{kind}/discovery") => {
            Some(crate::api_drivers::driver_discovery_response_schema())
        }
        ("get", "/drivers/{kind}/models") => {
            Some(crate::api_drivers::driver_models_response_schema())
        }
        ("get", "/sessions/{id}/driver-capabilities/tools") => {
            Some(crate::api_drivers::driver_capabilities_response_schema())
        }
        ("post", "/sessions/{id}/driver-capabilities/call") => {
            Some(crate::api_drivers::capability_call_response_schema())
        }
        ("post", "/sessions/{id}/driver/start") => {
            Some(crate::api_drivers::start_driver_response_schema())
        }
        ("post", "/sessions/{id}/driver/prompt")
        | ("post", "/sessions/{id}/driver/interrupt")
        | ("post", "/sessions/{id}/driver/resume") => {
            Some(crate::api_drivers::driver_request_response_schema())
        }
        ("post", "/sessions/{id}/driver/stop") => {
            Some(crate::api_drivers::stop_driver_response_schema())
        }
        ("get", "/sessions/{id}/driver/status") => {
            Some(crate::api_drivers::driver_status_response_schema())
        }
        ("get", "/tool-invocations/{id}") => {
            Some(crate::api_tool_invocations::tool_invocation_response_schema())
        }
        ("post", "/tool-invocations/{id}/cancel") => {
            Some(crate::api_tool_invocations::cancel_tool_invocation_response_schema())
        }
        ("get", "/tool-invocations/{id}/events") => {
            Some(crate::api_tool_invocations::tool_invocation_events_response_schema())
        }
        ("post", "/tool-schemas/validate") => {
            Some(crate::api_tool_schemas::validate_tool_schema_response_schema())
        }
        ("get", "/api/contract") => Some(api_contract_response_schema()),
        ("get", "/api/openapi.json") => Some(openapi_document_response_schema()),
        ("get", "/settings/orchestration") => {
            Some(crate::api_settings::orchestration_settings_response_schema())
        }
        ("put", "/settings/orchestration") => {
            Some(crate::api_settings::save_settings_response_schema())
        }
        ("get", "/settings/config-status") => {
            Some(crate::api_settings::config_status_response_schema())
        }
        ("post", "/settings/reload") => {
            Some(crate::api_settings::reload_settings_response_schema())
        }
        ("get", "/runtime/middleware") => {
            Some(crate::api_runtime::runtime_middleware_list_response_schema())
        }
        ("put", "/runtime/middleware/{id}/enabled") => {
            Some(crate::api_runtime::runtime_middleware_update_response_schema())
        }
        ("get", "/runtime/spans") => Some(crate::api_runtime::runtime_spans_response_schema()),
        ("get", "/runtime/spans/{id}") => Some(crate::api_runtime::runtime_span_response_schema()),
        ("get", "/runtime/observations") => {
            Some(crate::api_runtime::runtime_observations_response_schema())
        }
        ("post", "/sessions/{id}/permission-mode") => {
            Some(crate::api_permissions::set_permission_mode_response_schema())
        }
        ("get", "/sessions/{id}/permission-rules") => {
            Some(crate::api_permissions::permission_rules_response_schema())
        }
        ("post", "/sessions/{id}/permission-rules") => {
            Some(crate::api_permissions::permission_rule_mutation_response_schema())
        }
        ("delete", "/sessions/{id}/permission-rules/{index}") => {
            Some(crate::api_permissions::delete_permission_rule_response_schema())
        }
        ("get", "/permission-rules") => {
            Some(crate::api_permissions::persistent_permission_rules_response_schema())
        }
        ("post", "/permission-rules") => {
            Some(crate::api_permissions::persistent_permission_rule_mutation_response_schema())
        }
        ("delete", "/permission-rules/{index}") => {
            Some(crate::api_permissions::delete_persistent_permission_rule_response_schema())
        }
        ("get", "/permission-rules/export") => {
            Some(crate::api_permissions::permission_rule_store_schema())
        }
        ("post", "/permission-rules/import") => {
            Some(crate::api_permissions::import_permission_rules_response_schema())
        }
        ("get", "/workspace/trust") | ("patch", "/workspace/trust") => {
            Some(crate::api_permissions::workspace_trust_response_schema())
        }
        ("get", "/permission-denials") => {
            Some(crate::api_permissions::permission_denials_response_schema())
        }
        ("get", "/local-models/hardware") => {
            Some(crate::api_local_models::local_model_hardware_response_schema())
        }
        ("get", "/local-models/catalog") => {
            Some(crate::api_local_models::local_model_catalog_response_schema())
        }
        ("post", "/local-models/probe") => {
            Some(crate::api_local_models::probe_local_model_response_schema())
        }
        ("post", "/local-models/chat-smoke") => {
            Some(crate::api_local_models::chat_smoke_local_model_response_schema())
        }
        ("post", "/local-models/add-provider") => {
            Some(crate::api_local_models::add_local_model_provider_response_schema())
        }
        ("get", "/hooks/status") => Some(crate::hooks::hook_status_response_schema()),
        ("get", "/hooks/list") => Some(crate::hooks::hook_list_response_schema()),
        ("post", "/hooks/test") => Some(crate::hooks::hook_test_response_schema()),
        ("post", "/hooks/revoke") => Some(crate::hooks::hook_revoke_response_schema()),
        ("get", "/recipes") => Some(crate::api_recipes::recipes_list_response_schema()),
        ("post", "/recipes/install")
        | ("post", "/recipes/install-file")
        | ("post", "/recipes/install-url") => {
            Some(crate::api_recipes::installed_recipe_response_schema())
        }
        ("post", "/recipes/validate") => {
            Some(crate::api_recipes::validate_recipe_response_schema())
        }
        ("post", "/recipes/render") => Some(crate::api_recipes::render_recipe_response_schema()),
        ("post", "/recipes/deeplink/parse") | ("post", "/recipes/{id}/deeplink") => {
            Some(crate::api_recipes::recipe_deeplink_response_schema())
        }
        ("post", "/recipes/{id}/run") => Some(crate::api_recipes::recipe_run_response_schema()),
        ("post", "/recipes/{id}/schedule") => {
            Some(crate::api_recipes::schedule_recipe_response_schema())
        }
        ("get", "/recipes/{id}/runs") => Some(crate::api_recipes::recipe_runs_response_schema()),
        ("get", "/notifications") => {
            Some(crate::api_notifications::notifications_response_schema())
        }
        ("post", "/notifications/{id}/dismiss") => {
            Some(crate::api_notifications::dismiss_notification_response_schema())
        }
        ("get", "/instructions") => Some(crate::api::instruction_status_response_schema()),
        ("get", "/remote/channels") => Some(crate::api_remote::remote_channels_response_schema()),
        ("post", "/remote/channels/{id}/profile") => {
            Some(crate::api_remote::remote_channel_profile_response_schema())
        }
        ("post", "/remote/channels/{id}/fanout") => {
            Some(crate::api_remote::remote_channel_fanout_response_schema())
        }
        ("post", "/remote/channels/{id}/outbound") => {
            Some(crate::api_remote::remote_channel_outbound_response_schema())
        }
        ("get", "/remote/telegram/status") => {
            Some(crate::api_remote::telegram_status_response_schema())
        }
        ("get", "/remote/telegram/test") => {
            Some(crate::api_remote::telegram_test_response_schema())
        }
        ("get", "/remote/telegram/users") => {
            Some(crate::api_remote::telegram_users_response_schema())
        }
        ("post", "/remote/bindings") => Some(crate::api_remote::remote_binding_response_schema()),
        ("post", "/skills/provenance-review") => {
            Some(crate::api_skills::skill_provenance_review_response_schema())
        }
        _ => crate::api_triggers::trigger_openapi_response_schema(method, path)
            .or_else(|| crate::api_runtime::runtime_openapi_response_schema(method, path))
            .or_else(|| crate::api_benchmarks::benchmark_openapi_response_schema(method, path))
            .or_else(|| crate::api::session_openapi_response_schema(method, path))
            .or_else(|| crate::api_plugins::plugins_openapi_response_schema(method, path))
            .or_else(|| crate::api_skills::skills_openapi_response_schema(method, path))
            .or_else(|| crate::api_mcp::mcp_openapi_response_schema(method, path))
            .or_else(|| crate::api_lsp::lsp_openapi_response_schema(method, path))
            .or_else(|| crate::api_specs::specs_openapi_response_schema(method, path))
            .or_else(|| crate::api_schedules::schedule_openapi_response_schema(method, path))
            .or_else(|| crate::api_workspace::workspace_openapi_response_schema(method, path))
            .or_else(|| {
                crate::api_browser_workspace::browser_workspace_openapi_response_schema(
                    method, path,
                )
            })
            .or_else(|| crate::api_delivery::delivery_openapi_response_schema(method, path))
            .or_else(|| crate::api_environments::environments_openapi_response_schema(method, path))
            .or_else(|| {
                rightkit_agent_engine_attention::attention_openapi_response_schema(method, path)
            })
            .or_else(|| run_changes_contract_response_schema(method, path)),
    }
}
