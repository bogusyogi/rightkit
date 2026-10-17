//! Membrane's hook wire types: the module status, dispatch result and host response that the
//! shipped `membrane.hook.status` JSON and HookHost responses carry.
//!
//! These are data shapes only. Membrane's module bodies, normalization and transport stay with
//! Membrane; this module lets a consumer read and write the same JSON. Event names use
//! [`EventKind`], whose serde form is the host event name, so the strings match Membrane's
//! `HookEvent` exactly. Membrane's `HookInputEnvelopeV1` is not repeated here: [`HookEvent`]
//! already normalizes the same fields.
//!
//! [`HookHostResponseV1`] serializes `hookSpecificOutput` only for events whose host schema takes
//! it, and emits `decision` and `reason` only on `Stop`. Emitting an unsupported variant makes
//! Claude reject the whole response, so this follows Membrane's projection exactly.
//!
//! [`HookEvent`]: crate::event::HookEvent

use crate::event::EventKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Schema version of every Membrane hook wire type.
pub const HOOK_SCHEMA_VERSION: u32 = 1;
/// Per-module wall budget Membrane registers for its modules.
pub const HOOK_MODULE_DEADLINE_MS: u64 = 3_000;

/// One Membrane hook module. The serde names are the shipped `membrane.*` ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookModuleId {
    #[serde(rename = "membrane.cortex-status")]
    CortexStatus,
    #[serde(rename = "membrane.memory-rearm")]
    MemoryRearm,
    #[serde(rename = "membrane.memory-recall")]
    MemoryRecall,
    #[serde(rename = "membrane.memory-pre-compact")]
    MemoryPreCompact,
    #[serde(rename = "membrane.memory-post-compact")]
    MemoryPostCompact,
    #[serde(rename = "membrane.memory-bump")]
    MemoryBump,
    #[serde(rename = "membrane.diagnostics-fence")]
    DiagnosticsFence,
    #[serde(rename = "membrane.memory-conflict")]
    MemoryConflict,
    #[serde(rename = "membrane.tool-observer")]
    ToolObserver,
    #[serde(rename = "membrane.memory-ingest")]
    MemoryIngest,
    #[serde(rename = "membrane.diagnostics-observe")]
    DiagnosticsObserve,
    #[serde(rename = "membrane.diagnostics-completion-fence")]
    DiagnosticsCompletionFence,
    #[serde(rename = "membrane.memory-nag")]
    MemoryNag,
    #[serde(rename = "membrane.memory-failure")]
    MemoryFailure,
    #[serde(rename = "membrane.memory-episode")]
    MemoryEpisode,
    #[serde(rename = "membrane.memory-session-end")]
    MemorySessionEnd,
}

impl HookModuleId {
    /// Every module, in Membrane's dispatch order.
    pub const ORDERED: [Self; 16] = [
        Self::CortexStatus,
        Self::MemoryRearm,
        Self::MemoryRecall,
        Self::MemoryPreCompact,
        Self::MemoryPostCompact,
        Self::MemoryBump,
        Self::DiagnosticsFence,
        Self::MemoryConflict,
        Self::ToolObserver,
        Self::MemoryIngest,
        Self::DiagnosticsObserve,
        Self::DiagnosticsCompletionFence,
        Self::MemoryNag,
        Self::MemoryFailure,
        Self::MemoryEpisode,
        Self::MemorySessionEnd,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookModuleState {
    Available,
    Unavailable,
    Skipped,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookInvocationStatus {
    Ok,
    Error,
}

/// A module's typed status, matching `membrane.hook.status` JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookModuleOutputV1 {
    pub schema_version: u32,
    pub kind: String,
    pub state: HookModuleState,
    pub reason: String,
    pub detail: Value,
}

impl HookModuleOutputV1 {
    pub fn status(state: HookModuleState, reason: impl Into<String>, detail: Value) -> Self {
        Self {
            schema_version: HOOK_SCHEMA_VERSION,
            kind: "membrane.hook.status".to_owned(),
            state,
            reason: reason.into(),
            detail,
        }
    }

    pub fn skipped() -> Self {
        Self::status(
            HookModuleState::Skipped,
            "event_not_applicable",
            Value::Null,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookModuleResultV1 {
    pub id: HookModuleId,
    pub status: HookInvocationStatus,
    pub output: Option<HookModuleOutputV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HookModuleResultV1 {
    pub fn ok(id: HookModuleId, output: HookModuleOutputV1) -> Self {
        Self {
            id,
            status: HookInvocationStatus::Ok,
            output: Some(output),
            error: None,
        }
    }

    pub fn skipped(id: HookModuleId) -> Self {
        Self::ok(id, HookModuleOutputV1::skipped())
    }

    pub fn error(id: HookModuleId, error: impl Into<String>) -> Self {
        Self {
            id,
            status: HookInvocationStatus::Error,
            output: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookDispatchResultV1 {
    pub schema_version: u32,
    pub event: EventKind,
    pub status: HookInvocationStatus,
    pub results: Vec<HookModuleResultV1>,
}

impl HookDispatchResultV1 {
    pub fn new(event: EventKind, results: Vec<HookModuleResultV1>) -> Self {
        let status = aggregate_hook_status(&results);
        Self {
            schema_version: HOOK_SCHEMA_VERSION,
            event,
            status,
            results,
        }
    }
}

/// `Error` when any module failed, otherwise `Ok`.
pub fn aggregate_hook_status(results: &[HookModuleResultV1]) -> HookInvocationStatus {
    if results
        .iter()
        .any(|result| result.status == HookInvocationStatus::Error)
    {
        HookInvocationStatus::Error
    } else {
        HookInvocationStatus::Ok
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookHostDecision {
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPermissionDecision {
    Deny,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookSpecificOutputV1 {
    pub hook_event_name: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision: Option<HookPermissionDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_decision_reason: Option<String>,
    pub additional_context: String,
}

/// The HookHost response: the host's own control fields plus the full `membraneHook` record.
///
/// Deserialization is custom (see the `Deserialize` impl below): `hookSpecificOutput` is absent on
/// the wire for events without a host projection, so it is rebuilt from `membraneHook.event`.
#[derive(Clone, Debug, PartialEq)]
pub struct HookHostResponseV1 {
    pub decision: Option<HookHostDecision>,
    pub reason: Option<String>,
    pub hook_specific_output: HookSpecificOutputV1,
    pub membrane_hook: HookDispatchResultV1,
}

/// Wire shape read by [`HookHostResponseV1`]'s `Deserialize` impl.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HookHostResponseWireV1 {
    decision: Option<HookHostDecision>,
    reason: Option<String>,
    hook_specific_output: Option<HookSpecificOutputV1>,
    membrane_hook: HookDispatchResultV1,
}

impl<'de> Deserialize<'de> for HookHostResponseV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = HookHostResponseWireV1::deserialize(deserializer)?;
        let event = &wire.membrane_hook.event;
        let hook_specific_output = match wire.hook_specific_output {
            Some(output) => output,
            // Events without a host projection never carry hookSpecificOutput on the wire; the
            // projection's values for them are not transmitted, so the carrier starts empty.
            None if !takes_host_projection(event) => HookSpecificOutputV1 {
                hook_event_name: event.clone(),
                permission_decision: None,
                permission_decision_reason: None,
                additional_context: String::new(),
            },
            None => {
                return Err(<D::Error as serde::de::Error>::missing_field(
                    "hookSpecificOutput",
                ));
            }
        };
        Ok(Self {
            decision: wire.decision,
            reason: wire.reason,
            hook_specific_output,
            membrane_hook: wire.membrane_hook,
        })
    }
}

/// Events on which a blocked module denies the action.
fn is_deny_boundary(event: &EventKind) -> bool {
    matches!(event, EventKind::PreToolUse | EventKind::Stop)
}

/// Events whose host schema takes `hookSpecificOutput`.
fn takes_host_projection(event: &EventKind) -> bool {
    matches!(
        event,
        EventKind::SessionStart
            | EventKind::UserPromptSubmit
            | EventKind::PreToolUse
            | EventKind::PostToolUse
            | EventKind::PostToolUseFailure
    )
}

// Only events with a native context projection receive hookSpecificOutput. Other lifecycle
// callbacks keep their typed receipts, with top-level Stop control. Emitting unsupported event
// variants makes Claude reject the entire response.
impl Serialize for HookHostResponseV1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let event = &self.hook_specific_output.hook_event_name;
        let omit_host_projection = !takes_host_projection(event);
        let count = 1
            + usize::from(!omit_host_projection)
            + usize::from(self.decision.is_some())
            + usize::from(self.reason.is_some());
        let mut map = serializer.serialize_map(Some(count))?;
        if let Some(decision) = &self.decision {
            map.serialize_entry("decision", decision)?;
        }
        if let Some(reason) = &self.reason {
            map.serialize_entry("reason", reason)?;
        }
        if !omit_host_projection {
            map.serialize_entry("hookSpecificOutput", &self.hook_specific_output)?;
        }
        map.serialize_entry("membraneHook", &self.membrane_hook)?;
        map.end()
    }
}

/// Project native module results into the deterministic HookHost response.
pub fn project_hook_host_response(result: HookDispatchResultV1) -> HookHostResponseV1 {
    let additional_context = result
        .results
        .iter()
        .filter_map(|entry| {
            entry
                .output
                .as_ref()
                .and_then(|output| output.detail.get("additionalContext"))
                .and_then(Value::as_str)
        })
        .filter(|context| !context.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let blocked = result.results.iter().find(|entry| {
        entry
            .output
            .as_ref()
            .is_some_and(|output| output.state == HookModuleState::Blocked)
    });
    let deny = is_deny_boundary(&result.event) && blocked.is_some();
    let reason = blocked
        .and_then(|entry| entry.output.as_ref())
        .and_then(|output| {
            output
                .detail
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| Some(output.reason.clone()))
        })
        .or_else(|| deny.then(|| "semantic edit fence not cleared".to_owned()));
    let is_stop = result.event == EventKind::Stop;
    let is_pre_tool = result.event == EventKind::PreToolUse;
    HookHostResponseV1 {
        decision: (deny && is_stop).then_some(HookHostDecision::Block),
        reason: (deny && is_stop).then(|| reason.clone()).flatten(),
        hook_specific_output: HookSpecificOutputV1 {
            hook_event_name: result.event.clone(),
            permission_decision: (deny && is_pre_tool).then_some(HookPermissionDecision::Deny),
            permission_decision_reason: (deny && is_pre_tool).then(|| reason.clone()).flatten(),
            additional_context,
        },
        membrane_hook: result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn blocked(event: EventKind) -> HookDispatchResultV1 {
        let output = HookModuleOutputV1::status(
            HookModuleState::Blocked,
            "fence_open",
            json!({"detail": "tests not run", "additionalContext": "ctx"}),
        );
        HookDispatchResultV1::new(
            event,
            vec![HookModuleResultV1::ok(
                HookModuleId::DiagnosticsFence,
                output,
            )],
        )
    }

    #[test]
    fn module_ids_use_membrane_names_in_order() {
        assert_eq!(
            serde_json::to_value(HookModuleId::MemoryRecall).unwrap(),
            json!("membrane.memory-recall")
        );
        assert_eq!(HookModuleId::ORDERED.len(), 16);
        assert_eq!(HookModuleId::ORDERED[0], HookModuleId::CortexStatus);
    }

    #[test]
    fn stop_block_projects_top_level_decision_only() {
        let response = project_hook_host_response(blocked(EventKind::Stop));
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["decision"], json!("block"));
        assert_eq!(value["reason"], json!("tests not run"));
        assert!(value.get("hookSpecificOutput").is_none());
        assert_eq!(value["membraneHook"]["event"], json!("Stop"));
    }

    #[test]
    fn pre_tool_use_block_projects_deny() {
        let response = project_hook_host_response(blocked(EventKind::PreToolUse));
        let value = serde_json::to_value(&response).unwrap();
        assert!(value.get("decision").is_none());
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecisionReason"],
            json!("tests not run")
        );
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"],
            json!("ctx")
        );
    }

    #[test]
    fn unsupported_event_omits_host_projection() {
        let response = project_hook_host_response(blocked(EventKind::PreCompact));
        let value = serde_json::to_value(&response).unwrap();
        assert!(value.get("hookSpecificOutput").is_none());
        assert!(value.get("decision").is_none());
        assert_eq!(value["membraneHook"]["event"], json!("PreCompact"));
    }

    #[test]
    fn response_round_trips_and_rejects_unknown_fields() {
        let response = project_hook_host_response(blocked(EventKind::SessionStart));
        let text = serde_json::to_string(&response).unwrap();
        let back: HookHostResponseV1 = serde_json::from_str(&text).unwrap();
        assert_eq!(back.membrane_hook, response.membrane_hook);
        let extra = text.replacen('{', "{\"unexpected\":1,", 1);
        assert!(serde_json::from_str::<HookHostResponseV1>(&extra).is_err());
    }

    #[test]
    fn response_round_trips_for_every_event_kind() {
        let kinds = [
            EventKind::SessionStart,
            EventKind::UserPromptSubmit,
            EventKind::PreToolUse,
            EventKind::PostToolUse,
            EventKind::PostToolUseFailure,
            EventKind::Stop,
            EventKind::SubagentStart,
            EventKind::SubagentStop,
            EventKind::PreCompact,
            EventKind::PostCompact,
            EventKind::SessionEnd,
            EventKind::Notification,
            EventKind::Other("CustomEvent".to_owned()),
        ];
        for kind in kinds {
            let response = project_hook_host_response(blocked(kind.clone()));
            let wire = serde_json::to_string(&response).unwrap();
            let back: HookHostResponseV1 = serde_json::from_str(&wire)
                .unwrap_or_else(|error| panic!("{kind:?} failed to read: {error}"));
            assert_eq!(back.membrane_hook, response.membrane_hook, "{kind:?}");
            assert_eq!(back.decision, response.decision, "{kind:?}");
            assert_eq!(back.reason, response.reason, "{kind:?}");
            assert_eq!(back.hook_specific_output.hook_event_name, kind);
            // The wire is the contract: re-serializing the parsed value must reproduce it exactly.
            assert_eq!(serde_json::to_string(&back).unwrap(), wire, "{kind:?}");
            if takes_host_projection(&kind) {
                assert_eq!(back, response, "{kind:?}");
            } else {
                let value = serde_json::to_value(&back).unwrap();
                assert!(value.get("hookSpecificOutput").is_none(), "{kind:?}");
            }
        }
    }

    #[test]
    fn projection_events_require_hook_specific_output_on_read() {
        let response = project_hook_host_response(blocked(EventKind::PreToolUse));
        let mut value = serde_json::to_value(&response).unwrap();
        value.as_object_mut().unwrap().remove("hookSpecificOutput");
        let text = value.to_string();
        assert!(serde_json::from_str::<HookHostResponseV1>(&text).is_err());
    }

    #[test]
    fn aggregate_status_reports_any_error() {
        let ok = HookModuleResultV1::skipped(HookModuleId::MemoryBump);
        let err = HookModuleResultV1::error(HookModuleId::MemoryNag, "boom");
        assert_eq!(
            aggregate_hook_status(std::slice::from_ref(&ok)),
            HookInvocationStatus::Ok
        );
        assert_eq!(
            aggregate_hook_status(&[ok, err]),
            HookInvocationStatus::Error
        );
    }
}
