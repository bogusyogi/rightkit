//! Product-set cloud-cleanup policy: whether AI polish is on, whether cloud use is consented, and
//! the provider readiness label.
//!
//! The native shell derives these from the user's settings and key readiness and pushes them with
//! the runtime config it already sends (`replace_runtime_config`, `L3Policy` under
//! `settings::L3_POLICY_KEY`), over the private stdin channel. Nothing in a release build reads
//! them from the environment. Until a policy has been pushed the answer is the fail-closed one:
//! disabled, no consent, state unknown. A malformed pushed value counts as not pushed.
//!
//! Test and `test-mode` builds fall back to the old environment variables when no policy was
//! pushed (`overrides::policy_*`), so harnesses that push no config keep working. A pushed policy
//! always wins.

use super::overrides;
use crate::settings::pushed_l3_policy;

/// Whether AI polish is enabled at all: the privacy gate for capturing any machine-read context.
pub(crate) fn cleanup_enabled() -> bool {
    match pushed_l3_policy() {
        Some(policy) => policy.cleanup_enabled,
        None => overrides::policy_cleanup(),
    }
}

/// Whether the user's consent to send text to the cloud provider is in force.
pub(crate) fn cloud_consent() -> bool {
    match pushed_l3_policy() {
        Some(policy) => policy.cloud_consent,
        None => overrides::policy_consent(),
    }
}

/// The telemetry reason for an empty provider roster, from the pushed provider state.
pub(crate) fn empty_roster_reason() -> &'static str {
    let state = match pushed_l3_policy() {
        Some(policy) => Some(policy.provider_state),
        None => overrides::policy_provider_state(),
    };
    match state.as_deref() {
        Some("groq_key_read_error") => "groq_key_read_error",
        Some("groq_key_absent") => "groq_key_absent",
        Some("ready") => "eligible_roster_empty",
        _ => "provider_state_unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{replace_runtime_config, L3Policy, L3_POLICY_KEY};
    use rightkit_dictation_core::settings::SettingsBlob;

    fn push(policy: Option<L3Policy>) {
        let mut blob = SettingsBlob::default();
        if let Some(policy) = policy {
            blob.extras
                .insert(L3_POLICY_KEY.to_string(), policy.to_extra());
        }
        replace_runtime_config(blob);
    }

    #[test]
    fn pushed_policy_wins_over_the_environment_and_fails_closed_when_malformed() {
        let _guard = crate::test_support::test_env_guard();
        let previous: Vec<_> = [
            "RIGHTKIT_L3_CLEANUP",
            "RIGHTKIT_L3_CLOUD_CONSENT",
            "RIGHTKIT_L3_PROVIDER_STATE",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        std::env::set_var("RIGHTKIT_L3_CLEANUP", "1");
        std::env::set_var("RIGHTKIT_L3_CLOUD_CONSENT", "1");
        std::env::set_var("RIGHTKIT_L3_PROVIDER_STATE", "ready");

        // Nothing pushed: the test fallback reads the environment.
        push(None);
        assert!(cleanup_enabled());
        assert!(cloud_consent());
        assert_eq!(empty_roster_reason(), "eligible_roster_empty");

        // A pushed policy is authoritative, including "off".
        push(Some(L3Policy {
            cleanup_enabled: false,
            cloud_consent: false,
            provider_state: "groq_key_absent".to_string(),
        }));
        assert!(!cleanup_enabled());
        assert!(!cloud_consent());
        assert_eq!(empty_roster_reason(), "groq_key_absent");

        push(Some(L3Policy {
            cleanup_enabled: true,
            cloud_consent: true,
            provider_state: "ready".to_string(),
        }));
        assert!(cleanup_enabled());
        assert!(cloud_consent());

        // A malformed value is "not pushed", never "enabled".
        let mut blob = SettingsBlob::default();
        blob.extras
            .insert(L3_POLICY_KEY.to_string(), serde_json::json!("on"));
        replace_runtime_config(blob);
        std::env::remove_var("RIGHTKIT_L3_CLEANUP");
        std::env::remove_var("RIGHTKIT_L3_CLOUD_CONSENT");
        std::env::remove_var("RIGHTKIT_L3_PROVIDER_STATE");
        assert!(!cleanup_enabled());
        assert!(!cloud_consent());
        assert_eq!(empty_roster_reason(), "provider_state_unknown");

        push(None);
        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
