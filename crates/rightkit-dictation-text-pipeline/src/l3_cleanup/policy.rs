//! Product-set cloud-cleanup policy: whether AI polish is on, whether cloud use is consented, and
//! the provider readiness label.
//!
//! The native shell derives these from the user's settings and key readiness and pushes them with
//! the runtime config it already sends (`replace_runtime_config`, `L3Policy` under
//! `settings::L3_POLICY_KEY`), over the private stdin channel. Nothing in a release build reads
//! them from the environment. Until a policy has been pushed the answer is the fail-closed one:
//! disabled, no consent, state unknown. A malformed pushed value counts as not pushed.

use crate::settings::pushed_l3_policy;

/// Whether AI polish is enabled at all: the privacy gate for capturing any machine-read context.
pub(crate) fn cleanup_enabled() -> bool {
    pushed_l3_policy().is_some_and(|policy| policy.cleanup_enabled)
}

/// Whether the user's consent to send text to the cloud provider is in force.
pub(crate) fn cloud_consent() -> bool {
    pushed_l3_policy().is_some_and(|policy| policy.cloud_consent)
}

/// The telemetry reason for an empty provider roster, from the pushed provider state.
pub(crate) fn empty_roster_reason() -> &'static str {
    let state = pushed_l3_policy().map(|policy| policy.provider_state);
    match state.as_deref() {
        Some("groq_key_read_error") => "groq_key_read_error",
        Some("groq_key_absent") => "groq_key_absent",
        Some("ready") => "eligible_roster_empty",
        _ => "provider_state_unknown",
    }
}
