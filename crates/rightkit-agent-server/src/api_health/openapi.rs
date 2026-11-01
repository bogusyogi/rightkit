pub(crate) use rightkit_agent_engine_api_platform::openapi::{
    diagnostics_bundle_download_response_schema, diagnostics_bundle_response_schema,
    health_diagnostics_response_schema, health_response_schema, machine_info_response_schema,
    machine_status_response_schema, readiness_snapshot_response_schema,
    sandbox_settings_response_schema, save_sandbox_settings_request_schema,
};

pub(crate) fn health_openapi_components() -> Map<String, Value> {
    rightkit_agent_engine_api_platform::openapi::health_openapi_components(allowed_sandbox_tiers())
}
