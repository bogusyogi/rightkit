//! Provider roster selection: which providers a lane may attempt, in what
//! order, with which model. The product ladder is Groq-primary with a
//! same-provider fallback; Cerebras is test-only, NVIDIA opt-in via roster,
//! and OpenRouter dormant behind a validation marker.

use super::overrides;
use super::policy;
use super::prompts::{
    is_app_prompt_version, DEFAULT_GROQ_MODEL, DEFAULT_NVIDIA_MODEL, L1_GROQ_PRIMARY,
    L1_GROQ_SAME_PROVIDER_FALLBACK, L1_OPENROUTER_FALLBACK, L2_GROQ_PRIMARY,
    L2_GROQ_SAME_PROVIDER_FALLBACK, L2_OPENROUTER_FALLBACK, L3_GROQ_PRIMARY,
    L3_GROQ_SAME_PROVIDER_FALLBACK, PROMPT_POLISH_VERSION, SUMMARY_PROMPT_VERSION,
};
use super::state::{Provider, ProviderSpec};
use crate::provider_keys::{self, ProviderKey};

/// BYOK key lookup. Keys arrive over IPC into `provider_keys`. Only test and `test-mode`
/// builds fall back to the legacy environment variables (`overrides::env_api_key`); a release
/// build neither reads them nor contains their names.
fn byok_key(which: ProviderKey) -> Option<String> {
    provider_keys::get(which).or_else(|| overrides::env_api_key(which))
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
    // user has not supplied a key for. Cerebras remains available only to the
    // explicit legacy live-eval/test harness below; it is not product routing.
    //   Groq -> primary model, then same-provider fallback
    //   none -> Vec::new() — every lane returns no_provider and falls back local
    // OpenRouter and NVIDIA are dormant: they are reachable only through the opt-in roster
    // (`overrides::roster`), which exists in test and `test-mode` builds only. The product
    // roster was always empty, so a release build runs the Groq ladder alone.
    let groq_key = byok_key(ProviderKey::Groq);
    // Keep old provider-spec fixtures executable for unit-test coverage while
    // excluding Cerebras from every release build. Live evaluation has its
    // own explicit candidate path and does not use this product ladder.
    let has_groq = groq_key.is_some();

    let mut specs = Vec::new();
    if has_groq {
        let primary = overrides::groq_primary_model(prompt_version)
            .unwrap_or_else(|| default_groq_primary_model(prompt_version).to_string());
        specs.push(groq_spec(
            prompt_version,
            primary,
            groq_key.as_ref().unwrap(),
        ));
    }
    if has_groq {
        let same_provider = overrides::groq_fallback_model(prompt_version)
            .unwrap_or_else(|| default_groq_same_provider_fallback(prompt_version).to_string());
        // Avoid duplicating the primary route when the same-provider fallback
        // resolves to the same model — would burn an extra attempt and an
        // extra circuit failure on the same lane.
        if !specs.iter().any(|spec| {
            spec.provider == Provider::Groq
                && spec.base_url
                    == overrides::groq_base_url().unwrap_or_else(|| GROQ_BASE_URL.to_string())
                && spec.model == same_provider
        }) {
            specs.push(groq_spec(
                prompt_version,
                same_provider,
                groq_key.as_ref().unwrap(),
            ));
        }
    }

    // Opt-in extras for eval harnesses and one-off routing overrides. These
    // never replace the conditional ladder above; they only append additional
    // providers when explicitly listed in the roster (test and `test-mode` builds only).
    let extras = overrides::roster();
    for name in extras {
        match name.trim().to_ascii_lowercase().as_str() {
            "groq" | "cerebras" => {} // Groq is already handled; Cerebras is retired from product routing.
            "nvidia" | "nim" => {
                if let Some(api_key) = byok_key(ProviderKey::Nvidia) {
                    if !specs.iter().any(|spec| spec.provider == Provider::Nvidia) {
                        specs.push(ProviderSpec {
                            provider: Provider::Nvidia,
                            base_url: overrides::nvidia_base_url().unwrap_or_else(|| {
                                "https://integrate.api.nvidia.com/v1".to_string()
                            }),
                            api_key,
                            model: overrides::nvidia_model(prompt_version)
                                .unwrap_or_else(|| DEFAULT_NVIDIA_MODEL.to_string()),
                        });
                    }
                }
            }
            "openrouter" | "or" => {
                let model = overrides::openrouter_model()
                    .unwrap_or_else(|| default_openrouter_model(prompt_version).to_string());
                if let Some(api_key) = byok_key(ProviderKey::OpenRouter) {
                    if openrouter_validated_for(&model, prompt_version) {
                        if !specs.iter().any(|spec| {
                            spec.provider == Provider::OpenRouter && spec.model == model
                        }) {
                            specs.push(ProviderSpec {
                                provider: Provider::OpenRouter,
                                base_url: overrides::openrouter_base_url()
                                    .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string()),
                                api_key,
                                model,
                            });
                        }
                    } else {
                        tracing::warn!(
                            provider = "openrouter",
                            model = %model,
                            prompt_version,
                            "l3_cleanup_provider_skipped_unvalidated"
                        );
                    }
                }
            }
            _ => {}
        }
    }

    if specs.is_empty() {
        let reason = policy::empty_roster_reason();
        tracing::warn!(reason, prompt_version, "l3_provider_roster_empty");
    }
    specs
}

/// The built-in Groq endpoint. A release build always uses it.
const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

fn groq_spec(_prompt_version: &str, model: String, api_key: &str) -> ProviderSpec {
    ProviderSpec {
        provider: Provider::Groq,
        base_url: overrides::groq_base_url().unwrap_or_else(|| GROQ_BASE_URL.to_string()),
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

fn default_openrouter_model(prompt_version: &str) -> &'static str {
    if is_app_prompt_version(prompt_version) {
        L1_OPENROUTER_FALLBACK
    } else {
        match prompt_version {
            PROMPT_POLISH_VERSION => L2_OPENROUTER_FALLBACK,
            SUMMARY_PROMPT_VERSION => L3_GROQ_PRIMARY,
            _ => L1_OPENROUTER_FALLBACK,
        }
    }
}

pub(crate) fn openrouter_validated_for(model: &str, prompt_version: &str) -> bool {
    let marker = format!("{model}|{prompt_version}");
    let validated_for = overrides::openrouter_validated_for();
    validated_for.as_deref() == Some(&marker)
        || legacy_prompt_versions(prompt_version)
            .iter()
            .any(|legacy| validated_for.as_deref() == Some(format!("{model}|{legacy}").as_str()))
        || (model == L1_OPENROUTER_FALLBACK && is_app_prompt_version(prompt_version))
}

fn legacy_prompt_versions(prompt_version: &str) -> &'static [&'static str] {
    if is_app_prompt_version(prompt_version) {
        &[
            "l1_app_polish_prompt_v8",
            "l3_app_polish_prompt_v2",
            "l1_app_polish_prompt_v4",
        ]
    } else {
        match prompt_version {
            PROMPT_POLISH_VERSION => &["l2_prompt_polish_prompt_v6", "l2_prompt_polish_prompt_v5"],
            SUMMARY_PROMPT_VERSION => &["l3_summarize_prompt_v3"],
            _ => &[],
        }
    }
}
