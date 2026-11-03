//! Provider roster selection: which providers a lane may attempt, in what
//! order, with which model. The product ladder is Groq-primary with a
//! same-provider fallback.

use super::policy;
use super::prompts::{
    is_app_prompt_version, DEFAULT_GROQ_MODEL, L1_GROQ_PRIMARY, L1_GROQ_SAME_PROVIDER_FALLBACK,
    L2_GROQ_PRIMARY, L2_GROQ_SAME_PROVIDER_FALLBACK, L3_GROQ_PRIMARY,
    L3_GROQ_SAME_PROVIDER_FALLBACK, PROMPT_POLISH_VERSION, SUMMARY_PROMPT_VERSION,
};
use super::state::{Provider, ProviderSpec};
use crate::provider_keys::{self, ProviderKey};

/// BYOK key lookup. Keys arrive over IPC into `provider_keys`; the environment is never read.
fn byok_key(which: ProviderKey) -> Option<String> {
    provider_keys::get(which)
}

pub(crate) fn provider_specs(prompt_version: &str) -> Vec<ProviderSpec> {
    // BYOK cloud cleanup is Pro. Free (and a lapsed trial) get NO cloud providers, so
    // every L3 path returns "no_provider" and falls back to local on-device polish
    // (which is free). is_pro is pushed by the shell from the keyring license — the
    // same gate command_classify (shortcuts) and check_duration_limit (file length) use.
    if !crate::settings::is_pro() {
        tracing::warn!(
            reason = "not_pro",
            prompt_version,
            "l3_provider_roster_empty"
        );
        return Vec::new();
    }

    // Supported product ladder. We never silently route through a provider the
    // user has not supplied a key for.
    //   Groq -> primary model, then same-provider fallback
    //   none -> Vec::new() — every lane returns no_provider and falls back local
    let groq_key = byok_key(ProviderKey::Groq);

    let mut specs = Vec::new();
    if let Some(groq_key) = groq_key.as_deref() {
        let primary = default_groq_primary_model(prompt_version).to_string();
        specs.push(groq_spec(prompt_version, primary, groq_key));
        let same_provider = default_groq_same_provider_fallback(prompt_version).to_string();
        // Avoid duplicating the primary route when the same-provider fallback
        // resolves to the same model — would burn an extra attempt and an
        // extra circuit failure on the same lane.
        if !specs.iter().any(|spec| {
            spec.provider == Provider::Groq
                && spec.base_url == GROQ_BASE_URL
                && spec.model == same_provider
        }) {
            specs.push(groq_spec(prompt_version, same_provider, groq_key));
        }
    }

    if specs.is_empty() {
        let reason = policy::empty_roster_reason();
        tracing::warn!(reason, prompt_version, "l3_provider_roster_empty");
    }
    specs
}

/// The built-in Groq endpoint.
const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

fn groq_spec(_prompt_version: &str, model: String, api_key: &str) -> ProviderSpec {
    ProviderSpec {
        provider: Provider::Groq,
        base_url: GROQ_BASE_URL.to_string(),
        api_key: api_key.to_string(),
        model,
    }
}

pub(crate) fn default_groq_primary_model(prompt_version: &str) -> &'static str {
    if is_app_prompt_version(prompt_version) {
        L1_GROQ_PRIMARY
    } else {
        match prompt_version {
            PROMPT_POLISH_VERSION => L2_GROQ_PRIMARY,
            SUMMARY_PROMPT_VERSION => L3_GROQ_PRIMARY,
            _ => DEFAULT_GROQ_MODEL,
        }
    }
}

pub(crate) fn default_groq_same_provider_fallback(prompt_version: &str) -> &'static str {
    if is_app_prompt_version(prompt_version) {
        L1_GROQ_SAME_PROVIDER_FALLBACK
    } else {
        match prompt_version {
            PROMPT_POLISH_VERSION => L2_GROQ_SAME_PROVIDER_FALLBACK,
            SUMMARY_PROMPT_VERSION => L3_GROQ_SAME_PROVIDER_FALLBACK,
            _ => L1_GROQ_SAME_PROVIDER_FALLBACK,
        }
    }
}
