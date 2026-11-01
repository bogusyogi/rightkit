//! ENG-07: serve `ReconfigureL3` live instead of a sidecar restart (test-mode lever
//! `RIGHTKIT_L3_LIVE_RECONFIGURE=1`; off by default, and the capability is advertised only when on).
//!
//! The polish policy reaches the engine inside the pushed config (`extras["l3_policy"]`). The
//! shell records that value once per sidecar spawn, so after a live reconfigure its later config
//! pushes still carry the spawn-time policy. While a live policy is in force, a push whose
//! `l3_policy` equals the one in force when the live change arrived keeps the live policy; a push
//! that carries any other policy is newer than the live change and wins. A restarted engine
//! starts with no live state.
//!
//! Not covered (needs a public reset in `rightkit-dictation-text-pipeline`): a restart also resets the L3
//! circuit breaker; a live reconfigure keeps it.

use rightkit_dictation_core::settings::SettingsBlob;
use rightkit_dictation_text_pipeline::settings::{replace_runtime_config, L3_POLICY_KEY};
use serde_json::Value;

#[derive(Default)]
struct LiveL3 {
    /// The last config the shell pushed, as pushed.
    last_pushed: Option<SettingsBlob>,
    /// `(pushed policy when the live change arrived, live policy)`.
    live: Option<(Option<Value>, Value)>,
}

static STATE: std::sync::Mutex<Option<LiveL3>> = std::sync::Mutex::new(None);

fn with_state<T>(f: impl FnOnce(&mut LiveL3) -> T) -> T {
    let mut guard = STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(guard.get_or_insert_with(LiveL3::default))
}

/// The config to install for a `ReplaceEngineConfig` push. Unchanged unless the lever is on and a
/// live policy is in force.
pub(crate) fn apply_pushed_config(mut config: SettingsBlob) -> SettingsBlob {
    if !crate::perf_levers::l3_live_reconfigure() {
        return config;
    }
    with_state(|state| {
        state.last_pushed = Some(config.clone());
        let pushed = config.extras.get(L3_POLICY_KEY).cloned();
        match state.live.as_ref() {
            Some((base, live)) if *base == pushed => {
                config
                    .extras
                    .insert(L3_POLICY_KEY.to_string(), live.clone());
            }
            Some(_) => state.live = None,
            None => {}
        }
    });
    config
}

/// The policy the shell's `l3_policy_for` derives from the same three values.
pub(crate) fn policy_value(
    ai_polish_enabled: bool,
    provider_state: &str,
    provider_ready: bool,
) -> Value {
    let ready = ai_polish_enabled && provider_ready;
    serde_json::json!({
        "cleanup_enabled": ai_polish_enabled && (cfg!(target_os = "macos") || ready),
        "cloud_consent": ready,
        "provider_state": if ai_polish_enabled { provider_state } else { "" },
    })
}

/// Apply a `ReconfigureL3`. Keys arrive separately (`SetProviderKeys`) before it.
pub(crate) fn reconfigure(
    ai_polish_enabled: bool,
    provider_state: &str,
    provider_ready: bool,
) -> Result<(), String> {
    if !crate::perf_levers::l3_live_reconfigure() {
        return Err("live L3 reconfigure is not enabled in this engine".into());
    }
    let live = policy_value(ai_polish_enabled, provider_state, provider_ready);
    let config = with_state(|state| {
        let mut config = state.last_pushed.clone().unwrap_or_default();
        let base = config.extras.get(L3_POLICY_KEY).cloned();
        config
            .extras
            .insert(L3_POLICY_KEY.to_string(), live.clone());
        state.live = Some((base, live));
        config
    });
    replace_runtime_config(config);
    tracing::info!(
        event = "l3_live_reconfigured",
        ai_polish_enabled,
        provider_ready,
        "L3 policy applied without a restart"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_matches_the_shell_derivation() {
        let off = policy_value(false, "ready", true);
        assert_eq!(off["cleanup_enabled"], false);
        assert_eq!(off["cloud_consent"], false);
        assert_eq!(off["provider_state"], "");
        let on = policy_value(true, "ready", true);
        assert_eq!(on["cleanup_enabled"], true);
        assert_eq!(on["cloud_consent"], true);
        assert_eq!(on["provider_state"], "ready");
        let no_key = policy_value(true, "groq_key_absent", false);
        assert_eq!(no_key["cloud_consent"], false);
        assert_eq!(no_key["cleanup_enabled"], cfg!(target_os = "macos"));
    }
}
