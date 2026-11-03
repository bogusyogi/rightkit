//! CG-A08 — `session/cancel` handling for the ACP stdio bridge.
//!
//! Split into its own leaf module (rather than staying folded into
//! `prompt.rs` as it was in the pre-existing `acp_server_prompt.rs`) so the
//! cancel lifecycle leg is independently addressable per the canon's
//! "session, prompt, cancel & config lifecycle" framing for CG-A08. Behavior
//! is unchanged: `session/cancel` is a JSON-RPC *notification* (no `id`),
//! never a request — the mod-level dispatcher is responsible for routing it
//! here before the request/response machinery runs, so a failed cancel never
//! produces a stray response line.

use super::session::{require_session_id, DaemonClient};
use rightkit_agents::agent_acp as acp;
use serde_json::Value;

pub async fn handle_session_cancel(
    params: Value,
    client: &DaemonClient,
) -> Result<(), acp::AcpError> {
    let request: acp::CancelNotification =
        serde_json::from_value(params).map_err(|e| acp::AcpError::invalid_params(e.to_string()))?;
    require_session_id(&request.session_id)?;
    client
        .interrupt_session(&request.session_id)
        .await
        .map_err(|e| acp::AcpError::internal_error(format!("failed to cancel session: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn missing_session_id_is_invalid_params() {
        let client = DaemonClient::new(0, None);
        let err = handle_session_cancel(json!({}), &client).await.unwrap_err();
        assert_eq!(err.code, acp::AcpErrorCode::InvalidParams.code());
    }
}
