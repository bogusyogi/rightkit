// Sidecar environment whitelist.
//
// `start()` runs `env_clear` on the child `Command`, then re-adds only the
// keys the sidecar legitimately needs. This module owns the BYOK/L3 key
// roster that must be stripped before conditional re-population, plus the
// debug-build live-eval pass-through.

use std::process::Command;

// Test-mode only: in release `env_allowlist` (env_clear + a fixed passthrough list) already drops
// every one of these, and a release binary must not contain them (release switch rule).
#[cfg(feature = "test-mode")]
pub(crate) const L3_ENV_KEYS: &[&str] = &[
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
    "NVIDIA_API_KEY",
    "NIM_API_KEY",
    "OPENROUTER_API_KEY",
    "RIGHTKIT_L3_CLEANUP",
    "RIGHTKIT_L3_CLOUD_CONSENT",
    "RIGHTKIT_L3_ROSTER",
    "RIGHTKIT_L3_PROVIDER_STATE",
    "RIGHTKIT_L3_GROQ_BASE_URL",
    "RIGHTKIT_L3_GROQ_CLEANUP_MODEL",
    "RIGHTKIT_L3_GROQ_SUMMARY_MODEL",
    "RIGHTKIT_L3_GROQ_MODEL",
    "RIGHTKIT_L3_CEREBRAS_BASE_URL",
    "RIGHTKIT_L3_CEREBRAS_CLEANUP_MODEL",
    "RIGHTKIT_L3_CEREBRAS_SUMMARY_MODEL",
    "RIGHTKIT_L3_CEREBRAS_MODEL",
    "RIGHTKIT_L3_NVIDIA_BASE_URL",
    "RIGHTKIT_L3_NVIDIA_CLEANUP_MODEL",
    "RIGHTKIT_L3_NVIDIA_SUMMARY_MODEL",
    "RIGHTKIT_L3_NVIDIA_MODEL",
    "RIGHTKIT_L3_OPENROUTER_BASE_URL",
    "RIGHTKIT_L3_OPENROUTER_MODEL",
    "RIGHTKIT_L3_OPENROUTER_VALIDATED_FOR",
    "RIGHTKIT_L3_OPENROUTER_PROVIDER_ONLY",
    "RIGHTKIT_L3_OPENROUTER_PROVIDER_ORDER",
    "RIGHTKIT_L3_OPENROUTER_PROVIDER_SORT",
];
#[cfg(feature = "test-mode")]
const LIVE_EVAL_ENV_KEYS: &[&str] = &[
    "RIGHTKIT_AI_EVAL_MODE",
    "RIGHTKIT_AI_EVAL_CONFIG",
    "RIGHTKIT_AI_EVAL_OUTPUT",
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
];

pub(crate) fn strip_l3_env(command: &mut Command) {
    // Owner rule (rightkit-credentials `handoff`): an ambient copy of a provider key must not be
    // inherited, whatever the hand-off frame carries.
    rightkit_dictation_core::engine::provider_handoff::scrub_sidecar_env(command);
    #[cfg(feature = "test-mode")]
    for key in L3_ENV_KEYS {
        command.env_remove(key);
    }
}

#[cfg(feature = "test-mode")]
pub(crate) fn pass_live_eval_env(command: &mut Command) {
    if rightkit_dictation_core::dev_var!("RIGHTKIT_AI_EVAL_MODE").as_deref() != Ok("1") {
        return;
    }
    for key in LIVE_EVAL_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

/// Live lever testing (QA build only): every `HR_*` switch in the shell's own environment reaches
/// the engine, so the owner can A/B engine levers on the app-started engine by relaunching the QA
/// shell with different values. Release and normal dev builds compile none of this (and
/// `env_allowlist` drops every `HR_*`), so levers are inert there.
#[cfg(feature = "test-mode")]
const LEVER_PREFIX: &str = "HR_";

/// `HR_*` names the supervisor owns or that would break the app-to-engine contract, never forwarded
/// as levers: the data root and owner diagnostics are set explicitly (with their own checks) in
/// `build_sidecar_command`; `RIGHTKIT_ASR_BACKEND` makes the engine skip the launch frame (the model
/// choice must come from the shell); the calibration flag belongs to the shell's calibrate child.
#[cfg(feature = "test-mode")]
const LEVER_DENY: &[&str] = &[
    "RIGHTKIT_APP_DATA_DIR",
    "RIGHTKIT_ASR_BACKEND",
    "RIGHTKIT_OWNER_DIAGNOSTICS",
    "RIGHTKIT_OWNER_DIAGNOSTICS_DIR",
    "RIGHTKIT_CALIBRATION_COLLECT_DIAGNOSTICS",
];

/// The levers to forward from `vars` (a process environment), sorted by name. Non-UTF-8 names and
/// empty values are skipped.
#[cfg(feature = "test-mode")]
pub(crate) fn lever_env(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(String, std::ffi::OsString)> {
    let mut levers: Vec<(String, std::ffi::OsString)> = vars
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value)))
        .filter(|(name, value)| {
            name.starts_with(LEVER_PREFIX)
                && !LEVER_DENY.contains(&name.as_str())
                && !value.is_empty()
        })
        .collect();
    levers.sort_by(|a, b| a.0.cmp(&b.0));
    levers
}

/// Forward this process's levers to the engine. Call before the supervisor's own explicit `env`
/// sets so those always win.
#[cfg(feature = "test-mode")]
pub(crate) fn pass_lever_env(command: &mut Command) {
    for (name, value) in lever_env(std::env::vars_os()) {
        command.env(name, value);
    }
}

#[cfg(feature = "test-mode")]
#[doc(hidden)]
pub fn pass_live_eval_env_for_test(command: &mut Command) {
    pass_live_eval_env(command);
}

#[cfg(feature = "test-mode")]
#[doc(hidden)]
pub fn strip_l3_env_for_test(command: &mut Command) {
    strip_l3_env(command);
}

#[cfg(feature = "test-mode")]
#[doc(hidden)]
pub fn l3_env_keys_for_test() -> &'static [&'static str] {
    L3_ENV_KEYS
}
