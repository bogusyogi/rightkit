use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Codex,
    Claude,
    /// The Antigravity CLI (`agy`) run as a turn runtime via `agy_harness`.
    /// Shares the normalized event vocabulary; its stable event keys keep
    /// their own vendor namespace so dedup never collides with Claude.
    Antigravity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverVersionRecord {
    pub driver: DriverKind,
    pub executable: PathBuf,
    pub binary_version: String,
    pub protocol_version: String,
    pub schema_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityRecord {
    pub id: String,
    pub read_only: bool,
    pub requires_approval: bool,
    pub workspace_root: PathBuf,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverBindingStatus {
    Starting,
    Ready,
    Running,
    WaitingForApproval,
    Interrupted,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DriverGatewayState {
    Starting,
    Ready,
    Failed { message: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverCursor {
    pub input_offset: u64,
    pub output_offset: u64,
    pub last_event_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverBinding {
    pub id: String,
    pub host_session_id: String,
    pub driver: DriverKind,
    pub vendor_session_id: Option<String>,
    pub vendor_turn_id: Option<String>,
    pub workspace_root: PathBuf,
    pub status: DriverBindingStatus,
    pub version: DriverVersionRecord,
    pub capabilities: Vec<CapabilityRecord>,
    pub cursor: DriverCursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Environment variable names removed from the spawned process before `env` is applied. A
    /// vendor subprocess is a different trust domain and must not inherit the daemon's ambient
    /// secrets; see `DENIED_VENDOR_SUBPROCESS_ENV_VARS`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub env_remove: BTreeSet<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NormalizedEventKind {
    SessionStarted {
        vendor_session_id: String,
        /// Typed `system/init` payload (Claude only; every other driver sets `None`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        init: Option<Box<crate::driver_host::ClaudeSessionInit>>,
    },
    TurnStarted {
        vendor_turn_id: String,
    },
    AssistantDelta {
        text: String,
    },
    AssistantCompleted {
        text: String,
    },
    ToolCallStarted {
        call_id: String,
        tool_name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        is_error: bool,
        content: Value,
    },
    ApprovalRequested {
        request_id: String,
        action: String,
        details: Value,
    },
    TurnCompleted,
    /// Terminal turn plus provider-reported token split. Kept in the
    /// normalized event so long-lived CLI sessions retain cache hit/write
    /// accounting just like native provider sessions.
    TurnCompletedWithUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    },
    TurnInterrupted,
    TurnFailed {
        message: String,
    },
    GatewayReady,
    /// Vendor rate-limit telemetry (Claude `rate_limit_event`). `account` is
    /// `None` straight out of the stream parser (the line carries no
    /// identity); the caller that owns the managed profile stamps it.
    RateLimit {
        account: Option<crate::driver_host::ClaudeAccountIdentity>,
        /// `allowed`, `allowed_warning`, `rejected`; unknown values preserved.
        status: String,
        rate_limit_type: Option<String>,
        /// Unix seconds at which the window resets.
        resets_at: Option<u64>,
        utilization: Option<f64>,
        overage_status: Option<String>,
        error_code: Option<String>,
    },
    /// Per-turn cost and per-model usage from the vendor `result` summary,
    /// attributed to `account` when the caller knows it.
    TurnCostReported {
        account: Option<crate::driver_host::ClaudeAccountIdentity>,
        total_cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u64>,
        model_usage: Vec<crate::driver_host::ClaudeModelUsage>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedEvent {
    pub event_key: String,
    pub driver: DriverKind,
    pub source_offset: u64,
    pub vendor_method: String,
    pub kind: NormalizedEventKind,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DriverHostError {
    #[error("invalid {driver:?} message: {message}")]
    InvalidMessage { driver: DriverKind, message: String },
    #[error("unknown required {driver:?} event '{method}'")]
    UnknownRequiredEvent { driver: DriverKind, method: String },
}

pub(crate) fn event_key(driver: DriverKind, method: &str, source_offset: u64) -> String {
    let driver = match driver {
        DriverKind::Codex => "codex",
        DriverKind::Claude => "claude",
        DriverKind::Antigravity => "agy",
    };
    format!("{driver}:{source_offset}:{method}")
}

/// Produce the durable identity used for vendor-event replay suppression.
///
/// `source_offset` deliberately is *not* part of this key. It is only a local
/// reader cursor: after a process restart a vendor is free to replay its
/// output from a different offset. The canonical payload hash is consequently
/// the fallback identity when the protocol does not provide a vendor UUID.
pub(crate) fn stable_event_key(
    driver: DriverKind,
    method: &str,
    source: &Value,
    _source_offset: u64,
) -> String {
    let driver_label = match driver {
        DriverKind::Codex => "codex",
        DriverKind::Claude => "claude",
        DriverKind::Antigravity => "agy",
    };
    if let Some(uuid) = source.get("uuid").and_then(Value::as_str) {
        return format!("{driver_label}:uuid:{uuid}");
    }

    let mut canonical = String::new();
    canonical.push_str(method);
    canonical.push('\0');
    write_canonical_json(source, &mut canonical);
    let digest = rightkit_crypto::sha256(canonical.as_bytes());
    format!(
        "{driver_label}:sha256:{}",
        rightkit_crypto::hex_encode(&digest)
    )
}

fn write_canonical_json(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&value.to_string()),
        Value::String(value) => {
            output.push_str(&serde_json::to_string(value).expect("string serialization"))
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical_json(value, output);
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values
                .iter()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .enumerate()
            {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).expect("object key serialization"));
                output.push(':');
                write_canonical_json(value, output);
            }
            output.push('}');
        }
    }
}

pub(crate) fn invalid(driver: DriverKind, message: impl Into<String>) -> DriverHostError {
    DriverHostError::InvalidMessage {
        driver,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_cursor_round_trips_without_losing_identity() {
        let cursor = DriverCursor {
            input_offset: 17,
            output_offset: 42,
            last_event_key: Some("codex:42:turn/completed".into()),
        };
        let encoded = serde_json::to_string(&cursor).unwrap();
        let decoded: DriverCursor = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn vendor_identity_survives_replay_at_a_different_offset() {
        let claude = serde_json::json!({"uuid": "event-1", "type": "assistant"});
        assert_eq!(
            stable_event_key(DriverKind::Claude, "assistant", &claude, 1),
            stable_event_key(DriverKind::Claude, "assistant", &claude, 99)
        );
        let codex = serde_json::json!({
            "method": "thread/started",
            "params": {"thread": {"id": "thread-1"}}
        });
        assert_eq!(
            stable_event_key(DriverKind::Codex, "thread/started", &codex, 1),
            stable_event_key(DriverKind::Codex, "thread/started", &codex, 99)
        );
    }

    #[test]
    fn fallback_identity_is_canonical_and_never_uses_reader_offset() {
        let first = serde_json::json!({
            "method": "item/agentMessage/delta",
            "params": {"delta": "same", "meta": {"a": 1, "b": 2}}
        });
        let reordered = serde_json::json!({
            "params": {"meta": {"b": 2, "a": 1}, "delta": "same"},
            "method": "item/agentMessage/delta"
        });
        let first_key = stable_event_key(DriverKind::Codex, "item/agentMessage/delta", &first, 2);
        assert_eq!(
            first_key,
            stable_event_key(
                DriverKind::Codex,
                "item/agentMessage/delta",
                &reordered,
                999,
            )
        );
        assert!(first_key.starts_with("codex:sha256:"));
    }
}
