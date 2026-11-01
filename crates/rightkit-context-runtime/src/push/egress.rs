//! Final native serialization gate. The host still owns surrounding model
//! messages; this measurement is explicitly limited to the serialized result.
use super::recovery::RecoveryError;
use rightkit_context_protocol::host_observation::RemainingContextCeilingV1;
use serde_json::{json, Value};

/// The serialized form whose size and token count are measured: the result envelope as it
/// goes on the wire.
pub(crate) fn wire_form(envelope: &Value) -> String {
    envelope.to_string()
}

pub fn fit_native_response(
    mut envelope: Value,
    ceiling: &RemainingContextCeilingV1,
) -> Result<Value, RecoveryError> {
    ceiling.validate().map_err(|_| RecoveryError::Denied)?;
    if ceiling.remaining_tokens.basis.id != "o200k_base"
        || ceiling.remaining_tokens.basis.version != "1"
    {
        return Err(RecoveryError::Denied);
    }
    let limit = ceiling
        .remaining_tokens
        .estimate
        .value
        .ok_or(RecoveryError::Denied)?;
    let data = envelope
        .pointer_mut("/result/data")
        .and_then(Value::as_object_mut)
        .ok_or(RecoveryError::Corrupt)?;
    data.insert(
        "deliveryMeasurement".into(),
        json!({"schemaVersion":1,"scope":"serialized_mcp_tool_result",
        "basis":"o200k_base/1","ceilingId":ceiling.ceiling_id,"tokens":0,"bytes":0,
        "remainingTokens":limit,"providerBilledTokens":null}),
    );
    for _ in 0..16 {
        let wire = wire_form(&envelope);
        let tokens = rightkit_memory_core::ContextTokenAccounting::count_exact(&wire)
            .map_err(|_| RecoveryError::Unavailable)? as u64;
        let bytes = wire.len();
        if envelope
            .pointer("/result/data/deliveryMeasurement/tokens")
            .and_then(Value::as_u64)
            == Some(tokens)
            && envelope
                .pointer("/result/data/deliveryMeasurement/bytes")
                .and_then(Value::as_u64)
                == Some(bytes as u64)
        {
            // Leave space for the JSON-RPC id/envelope and HTTP headers within
            // the native client's 128 KiB read cap. Never truncate the content.
            return if tokens <= limit && bytes <= 120 * 1024 {
                Ok(envelope)
            } else {
                Err(RecoveryError::Limit)
            };
        }
        envelope["result"]["data"]["deliveryMeasurement"]["tokens"] = json!(tokens);
        envelope["result"]["data"]["deliveryMeasurement"]["bytes"] = json!(bytes);
    }
    Err(RecoveryError::Corrupt)
}

/// Bounded-response egress gate (implementation-contract §"Pull budget
/// contract"). Same serialized-MCP-tool-result measurement as
/// `fit_native_response`, but the capacity bound is the caller's declared
/// response budget rather than an H8 observation. Host capacity is reported
/// as unobserved — never invented — and an over-budget result is the typed
/// `RecoveryError::Limit` refusal, never a truncation.
pub fn fit_native_response_to_budget(
    mut envelope: Value,
    budget_tokens: u64,
) -> Result<Value, RecoveryError> {
    if budget_tokens == 0 {
        return Err(RecoveryError::Denied);
    }
    let data = envelope
        .pointer_mut("/result/data")
        .and_then(Value::as_object_mut)
        .ok_or(RecoveryError::Corrupt)?;
    data.insert("deliveryMeasurement".into(), json!({"schemaVersion":1,"scope":"serialized_mcp_tool_result",
        "basis":"o200k_base/1","responseBudget":budget_tokens,"hostCapacity":{"coverage":"unavailable"},
        "tokens":0,"bytes":0,"providerBilledTokens":null}));
    for _ in 0..16 {
        let wire = wire_form(&envelope);
        let tokens = rightkit_memory_core::ContextTokenAccounting::count_exact(&wire)
            .map_err(|_| RecoveryError::Unavailable)? as u64;
        let bytes = wire.len();
        if envelope
            .pointer("/result/data/deliveryMeasurement/tokens")
            .and_then(Value::as_u64)
            == Some(tokens)
            && envelope
                .pointer("/result/data/deliveryMeasurement/bytes")
                .and_then(Value::as_u64)
                == Some(bytes as u64)
        {
            // Same native-transport read cap as the H8 gate; never truncate.
            return if tokens <= budget_tokens && bytes <= 120 * 1024 {
                Ok(envelope)
            } else {
                Err(RecoveryError::Limit)
            };
        }
        envelope["result"]["data"]["deliveryMeasurement"]["tokens"] = json!(tokens);
        envelope["result"]["data"]["deliveryMeasurement"]["bytes"] = json!(bytes);
    }
    Err(RecoveryError::Corrupt)
}
