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

#[cfg(any(test, feature = "test-mode"))]
pub(crate) use test_mode::*;

#[cfg(not(any(test, feature = "test-mode")))]
pub(crate) use release::*;

/// Dynamic key read for unit tests only (`with_env` fixtures name keys at run time). It does not
/// exist in a `test-mode` or release build.
#[cfg(test)]
pub(crate) fn env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(any(test, feature = "test-mode"))]
mod test_mode {
    use super::super::prompts::SUMMARY_PROMPT_VERSION;
    use crate::provider_keys::ProviderKey;

    fn norm(value: Result<String, std::env::VarError>) -> Option<String> {
        value
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    fn norm_u64(value: Result<String, std::env::VarError>) -> Option<u64> {
        norm(value).and_then(|v| v.parse::<u64>().ok())
    }

    fn norm_csv(value: Result<String, std::env::VarError>) -> Vec<String> {
        norm(value)
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn norm_true(value: Result<String, std::env::VarError>) -> bool {
        norm(value)
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"))
            .unwrap_or(false)
    }

    // Tuning (S6 shape, test-mode only: no product path sets any of these).

    pub(crate) fn max_input_chars() -> Option<u64> {
        norm_u64(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_MAX_INPUT_CHARS"
        ))
    }

    pub(crate) fn total_timeout_ms() -> Option<u64> {
        norm_u64(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_TOTAL_TIMEOUT_MS"
        ))
    }

    pub(crate) fn provider_timeout_ms() -> Option<u64> {
        norm_u64(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_PROVIDER_TIMEOUT_MS"
        ))
    }

    pub(crate) fn circuit_fails() -> Option<u64> {
        norm_u64(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_CIRCUIT_FAILS"
        ))
    }

    pub(crate) fn circuit_cooldown_ms() -> Option<u64> {
        norm_u64(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_CIRCUIT_COOLDOWN_MS"
        ))
    }

    // Endpoint, model, routing overrides (S3).

    pub(crate) fn groq_base_url() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_GROQ_BASE_URL"
        ))
    }

    pub(crate) fn groq_primary_model(prompt_version: &str) -> Option<String> {
        let specific = if prompt_version == SUMMARY_PROMPT_VERSION {
            norm(rightkit_dictation_core::dev_var!(
                "RIGHTKIT_L3_GROQ_SUMMARY_MODEL"
            ))
        } else {
            norm(rightkit_dictation_core::dev_var!(
                "RIGHTKIT_L3_GROQ_CLEANUP_MODEL"
            ))
        };
        specific.or_else(|| norm(rightkit_dictation_core::dev_var!("RIGHTKIT_L3_GROQ_MODEL")))
    }

    pub(crate) fn groq_fallback_model(prompt_version: &str) -> Option<String> {
        if prompt_version == SUMMARY_PROMPT_VERSION {
            norm(rightkit_dictation_core::dev_var!(
                "RIGHTKIT_L3_GROQ_SUMMARY_SAME_PROVIDER_FALLBACK_MODEL"
            ))
        } else {
            norm(rightkit_dictation_core::dev_var!(
                "RIGHTKIT_L3_GROQ_CLEANUP_SAME_PROVIDER_FALLBACK_MODEL"
            ))
        }
    }

    pub(crate) fn nvidia_base_url() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_NVIDIA_BASE_URL"
        ))
    }

    pub(crate) fn nvidia_model(prompt_version: &str) -> Option<String> {
        let specific = if prompt_version == SUMMARY_PROMPT_VERSION {
            norm(rightkit_dictation_core::dev_var!(
                "RIGHTKIT_L3_NVIDIA_SUMMARY_MODEL"
            ))
        } else {
            norm(rightkit_dictation_core::dev_var!(
                "RIGHTKIT_L3_NVIDIA_CLEANUP_MODEL"
            ))
        };
        specific.or_else(|| {
            norm(rightkit_dictation_core::dev_var!(
                "RIGHTKIT_L3_NVIDIA_MODEL"
            ))
        })
    }

    pub(crate) fn openrouter_model() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_OPENROUTER_MODEL"
        ))
    }

    pub(crate) fn openrouter_base_url() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_OPENROUTER_BASE_URL"
        ))
    }

    pub(crate) fn openrouter_validated_for() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR"
        ))
    }

    pub(crate) fn openrouter_provider_only() -> Vec<String> {
        norm_csv(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_OPENROUTER_PROVIDER_ONLY"
        ))
    }

    pub(crate) fn openrouter_provider_order() -> Vec<String> {
        norm_csv(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_OPENROUTER_PROVIDER_ORDER"
        ))
    }

    pub(crate) fn openrouter_provider_sort() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_OPENROUTER_PROVIDER_SORT"
        ))
    }

    /// Opt-in extra providers appended after the Groq ladder (the shell always sent an empty
    /// roster, so the product never uses extras).
    pub(crate) fn roster() -> Vec<String> {
        norm_csv(rightkit_dictation_core::dev_var!("RIGHTKIT_L3_ROSTER"))
    }

    /// Legacy provider-key environment variables, as a fallback behind the key store (S3).
    pub(crate) fn env_api_key(which: ProviderKey) -> Option<String> {
        match which {
            ProviderKey::Groq => norm(rightkit_dictation_core::dev_var!("GROQ_API_KEY")),
            ProviderKey::Nvidia => norm(rightkit_dictation_core::dev_var!("NVIDIA_API_KEY"))
                .or_else(|| norm(rightkit_dictation_core::dev_var!("NIM_API_KEY"))),
            ProviderKey::OpenRouter => {
                norm(rightkit_dictation_core::dev_var!("OPENROUTER_API_KEY"))
            }
        }
    }

    // Product-set values: environment fallback for a harness that pushes no `l3_policy`.

    pub(crate) fn policy_cleanup() -> bool {
        norm_true(rightkit_dictation_core::dev_var!("RIGHTKIT_L3_CLEANUP"))
    }

    pub(crate) fn policy_consent() -> bool {
        norm_true(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_CLOUD_CONSENT"
        ))
    }

    pub(crate) fn policy_provider_state() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!(
            "RIGHTKIT_L3_PROVIDER_STATE"
        ))
    }

    // Live evaluation harness (S1/S3), used only by `live_eval`, which is itself test-mode only.

    pub(crate) fn ai_eval_mode() -> bool {
        norm_true(rightkit_dictation_core::dev_var!("RIGHTKIT_AI_EVAL_MODE"))
    }

    pub(crate) fn ai_eval_config() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!("RIGHTKIT_AI_EVAL_CONFIG"))
    }

    pub(crate) fn ai_eval_output() -> Option<String> {
        norm(rightkit_dictation_core::dev_var!("RIGHTKIT_AI_EVAL_OUTPUT"))
    }

    /// The key a live-eval candidate names. Config validation already restricts `key_env` to
    /// these two; anything else reads nothing.
    pub(crate) fn ai_eval_api_key(key_env: &str) -> Option<String> {
        match key_env {
            "GROQ_API_KEY" => norm(rightkit_dictation_core::dev_var!("GROQ_API_KEY")),
            "CEREBRAS_API_KEY" => norm(rightkit_dictation_core::dev_var!("CEREBRAS_API_KEY")),
            _ => None,
        }
    }
}

#[cfg(not(any(test, feature = "test-mode")))]
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
