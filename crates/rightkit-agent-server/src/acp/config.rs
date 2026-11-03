//! CG-A08 — ACP agent configuration/negotiation lifecycle: process options,
//! `initialize`, and `authenticate`.
//!
//! NOTICE: relocated from the config-negotiation half of the pre-existing
//! `acp_server.rs` (RightKit-original, RETAIN action per canon CG-A08);
//! logic carried forward unchanged aside from module paths.

use rightkit_agents::agent_acp as acp;
use serde_json::{json, Value};

/// Requires an already-running daemon (`--port`) — the ACP stdio process is
/// a thin protocol adapter over the daemon's existing session HTTP API
/// (`super::session::DaemonClient`), not a second copy of the engine.
pub struct AcpServerOptions {
    pub port: u16,
    pub token: Option<String>,
}

pub fn success_response(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
}

pub fn error_response(id: Option<Value>, error: acp::AcpError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": error })
}

pub fn handle_initialize(params: Value) -> Result<Value, acp::AcpError> {
    let request: acp::InitializeRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    if request.protocol_version != acp::ACP_PROTOCOL_VERSION {
        return Err(acp::AcpError::invalid_params(format!(
            "unsupported ACP protocol version {}; RightKit supports only {}",
            request.protocol_version,
            acp::ACP_PROTOCOL_VERSION
        ))
        .with_data(json!({
            "requestedProtocolVersion": request.protocol_version,
            "supportedProtocolVersions": [acp::ACP_PROTOCOL_VERSION],
        })));
    }
    let mut response = acp::default_initialize_response();
    response.protocol_version = acp::negotiate_protocol_version(request.protocol_version);
    let mut value =
        serde_json::to_value(response).map_err(|e| acp::AcpError::internal_error(e.to_string()))?;
    // ACP covers the portable prompt/session subset. Publish the RightKit
    // lifecycle controls that deliberately remain on the versioned daemon
    // API so clients do not infer their absence or fail late.
    value["coderightCapabilityGaps"] = json!({
        "transport": "daemonApi",
        "controls": [
            "goalLifecycle",
            "subagentLifecycle",
            "approvalHistory",
            "sessionForkArchiveExport",
            "scheduleLifecycle"
        ]
    });
    Ok(value)
}

pub fn handle_authenticate(params: Value) -> Result<Value, acp::AcpError> {
    let _request: acp::AuthenticateRequest =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    // CR's daemon token already gates every call this bridge makes; there is
    // no additional per-client login step, so any advertised method id
    // succeeds trivially (see AGENTS.md doc-comment on AuthenticateResponse).
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_rejects_unsupported_protocol_version() {
        let params = json!({ "protocolVersion": acp::ACP_PROTOCOL_VERSION + 999 });
        let err = handle_initialize(params).unwrap_err();
        assert_eq!(err.code, acp::AcpErrorCode::InvalidParams.code());
    }

    #[test]
    fn success_response_uses_null_id_when_absent() {
        let response = success_response(None, json!({}));
        assert_eq!(response["id"], Value::Null);
    }
}
