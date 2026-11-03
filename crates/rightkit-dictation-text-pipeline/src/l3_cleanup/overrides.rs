//! Environment overrides for the cloud-cleanup lanes: test and `test-mode` builds only.
//!
//! Release switch rule (`@rightkit/release`): a release binary must contain no environment
//! selector for an endpoint, model, routing preference, provider key, test activation or
//! tuning of these lanes, and must not contain the NAME strings either (the binary scan
//! searches for them). This module is the only place in the crate that names those variables.
//! The `test_mode` half reads each one with a literal `dev_var!("NAME")`; the `release` half
//! has the same functions and returns "no override", so callers need no `cfg` of their own and
//! no name literal exists outside the gated half.
//!
//! What the product sets (cleanup enabled, cloud consent, provider state) is NOT read from the
//! environment in a release build: it arrives with the pushed runtime config (`policy`).
//! Here that exists only as the test fallback `policy_*` for a harness that pushes nothing.

pub(crate) use release::*;

mod release {
    //! Release: no environment is consulted. Every override is "absent".
    use crate::provider_keys::ProviderKey;

    pub(crate) fn max_input_chars() -> Option<u64> {
        None
    }

    pub(crate) fn total_timeout_ms() -> Option<u64> {
        None
    }

    pub(crate) fn provider_timeout_ms() -> Option<u64> {
        None
    }

    pub(crate) fn circuit_fails() -> Option<u64> {
        None
    }

    pub(crate) fn circuit_cooldown_ms() -> Option<u64> {
        None
    }

    pub(crate) fn groq_base_url() -> Option<String> {
        None
    }

    pub(crate) fn groq_primary_model(_prompt_version: &str) -> Option<String> {
        None
    }

    pub(crate) fn groq_fallback_model(_prompt_version: &str) -> Option<String> {
        None
    }

    pub(crate) fn nvidia_base_url() -> Option<String> {
        None
    }

    pub(crate) fn nvidia_model(_prompt_version: &str) -> Option<String> {
        None
    }

    pub(crate) fn openrouter_model() -> Option<String> {
        None
    }

    pub(crate) fn openrouter_base_url() -> Option<String> {
        None
    }

    pub(crate) fn openrouter_validated_for() -> Option<String> {
        None
    }

    pub(crate) fn openrouter_provider_only() -> Vec<String> {
        Vec::new()
    }

    pub(crate) fn openrouter_provider_order() -> Vec<String> {
        Vec::new()
    }

    pub(crate) fn openrouter_provider_sort() -> Option<String> {
        None
    }

    pub(crate) fn roster() -> Vec<String> {
        Vec::new()
    }

    pub(crate) fn env_api_key(_which: ProviderKey) -> Option<String> {
        None
    }

    pub(crate) fn policy_cleanup() -> bool {
        false
    }

    pub(crate) fn policy_consent() -> bool {
        false
    }

    pub(crate) fn policy_provider_state() -> Option<String> {
        None
    }
}
