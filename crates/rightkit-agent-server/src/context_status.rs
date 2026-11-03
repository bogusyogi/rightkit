//! Truthful projection of the Pulse Membrane CLI binding.
//!
//! This is presentation only. RightKit reaches Membrane solely through the
//! `coderight-membrane-cli` adapter; this module only checks whether the CLI can be
//! located (no process is spawned, no store is created) and never infers health.

use serde::{Deserialize, Serialize};
use serde_json::Value;

const COMPONENTS: [(&str, &str); 5] = [
    ("pull", "Pull"),
    ("memory", "Memory"),
    ("blueprint", "Blueprint"),
    ("transcript", "Transcript"),
    ("adapt", "Adapt"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComponentState {
    Available,
    Advertised,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextComponentStatus {
    pub(crate) name: String,
    pub(crate) state: ComponentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextStatus {
    pub(crate) state: ComponentState,
    pub(crate) binding: String,
    pub(crate) components: Vec<ContextComponentStatus>,
}

fn state_rank(state: ComponentState) -> u8 {
    match state {
        ComponentState::Available => 0,
        ComponentState::Advertised => 1,
        ComponentState::Degraded => 2,
        ComponentState::Unavailable => 3,
    }
}

fn aggregate(components: &[ContextComponentStatus]) -> ComponentState {
    components
        .iter()
        .map(|component| component.state)
        .max_by_key(|state| state_rank(*state))
        .unwrap_or(ComponentState::Unavailable)
}

fn component(name: &str, state: ComponentState, reason: Option<&str>) -> ContextComponentStatus {
    ContextComponentStatus {
        name: name.to_owned(),
        state,
        reason: reason.map(str::to_owned),
    }
}

/// `membraneBinding` object for `/runtime/resource-metrics`: whether Pulse's Membrane CLI
/// is installed. `bound` means the binary was located, not that it is healthy.
pub(crate) fn cli_binding_json() -> Value {
    match rightkit_memory::membrane::MembraneCli::locate() {
        Some(binary) => serde_json::json!({
            "bound": true,
            "mode": "cli",
            "binary": binary.display().to_string(),
            "subsystems": COMPONENTS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        }),
        None => serde_json::json!({
            "bound": false,
            "mode": "off",
            "note": rightkit_memory::membrane::OFF_REASON,
        }),
    }
}

/// Project the daemon's `/runtime/resource-metrics` response.
/// Missing binding data remains unavailable; it never becomes healthy by
/// virtue of the daemon being reachable.
pub(crate) fn from_metrics(value: &Value) -> ContextStatus {
    let binding = value.get("membraneBinding").unwrap_or(&Value::Null);
    let bound = binding.get("bound").and_then(Value::as_bool) == Some(true);
    if !bound {
        let reason = binding
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or("canonical context binding unavailable");
        let components = COMPONENTS
            .iter()
            .map(|(_, label)| component(label, ComponentState::Unavailable, Some(reason)))
            .collect::<Vec<_>>();
        return ContextStatus {
            state: ComponentState::Unavailable,
            binding: binding
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("unavailable")
                .to_owned(),
            components,
        };
    }

    let components = COMPONENTS
        .iter()
        .map(|(subsystem, label)| {
            let present = binding
                .get("subsystems")
                .and_then(Value::as_array)
                .is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value.as_str() == Some(*subsystem))
                });
            if !present {
                return component(
                    label,
                    ComponentState::Unavailable,
                    Some("subsystem not advertised"),
                );
            }
            if *subsystem == "blueprint"
                && binding.get("blueprintReady").and_then(Value::as_bool) == Some(false)
            {
                return component(
                    label,
                    ComponentState::Degraded,
                    Some("blueprint readiness reported false"),
                );
            }
            component(
                label,
                ComponentState::Advertised,
                Some("capability advertised; runtime health not exposed"),
            )
        })
        .collect::<Vec<_>>();
    ContextStatus {
        state: aggregate(&components),
        binding: binding
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("cli")
            .to_owned(),
        components,
    }
}

pub(crate) fn render_text(status: &ContextStatus) -> String {
    let mut output = format!("Context: {:?} ({})", status.state, status.binding);
    output.push_str("\nComponents:");
    for component in &status.components {
        output.push_str(&format!("\n- {}: {:?}", component.name, component.state));
        if let Some(reason) = &component.reason {
            output.push_str(&format!(" ({reason})"));
        }
    }
    output
}
