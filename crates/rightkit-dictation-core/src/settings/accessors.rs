//! Typed accessors over [`SettingsBlob`]: every "stored value or documented default" rule, the
//! audio-policy mapping, the pill-visibility normalizer and the dynamic `extras` helpers.
//!
//! These are pure functions of a blob. The host owns persistence (load, transact, save) and
//! calls them on a snapshot or inside its own mutation closure; nothing here reads a file.

use serde_json::Value;
use std::collections::HashMap;

use super::blob::{
    SettingsBlob, DEFAULT_AI_POLISH_ENABLED, DEFAULT_CLOSE_BEHAVIOR, DEFAULT_DELIVERY_METHOD,
    DEFAULT_DIAGNOSTICS_ENABLED, DEFAULT_HISTORY_RETENTION, DEFAULT_MAC_PUSH_TO_TALK_KEY,
    DEFAULT_PILL_VISIBILITY, DEFAULT_PUSH_TO_TALK_KEY,
};
use rightkit_transcript_library::history::HistoryRetention;

/// Close behaviours the settings patch allowlist accepts, in the order the patch lists them.
pub const CLOSE_BEHAVIORS: [&str; 3] = ["minimize", "quit", "destroy"];

/// Audio-conditioning policy name used when the `audio_conditioning` setting is on.
pub const AUDIO_POLICY_CONDITIONED: &str = "asr_simple_gain_hpf";

/// Audio-conditioning policy name used when the `audio_conditioning` setting is off.
pub const AUDIO_POLICY_RAW: &str = "raw";

/// Settings `extras` key holding the model/execution-provider choice.
pub const MODEL_CHOICE_KEY: &str = "model_choice";

/// Checks a close behaviour against the same allowlist `apply_patch_to` uses, so a typed setter
/// and the renderer patch path reject the same values with the same message.
pub fn validate_close_behavior(value: &str) -> Result<(), String> {
    if CLOSE_BEHAVIORS.contains(&value) {
        Ok(())
    } else {
        Err(format!("invalid close_behavior: {value}"))
    }
}

/// Normalizes a stored pill-visibility string to the two values a renderer and the gating logic
/// handle: `"recording_only"` (the legacy `"during_dictation"` alias maps here) or `"always"`.
pub fn normalize_pill_visibility(stored: &str) -> &'static str {
    match stored {
        "recording_only" | "during_dictation" => "recording_only",
        _ => "always",
    }
}

/// Settings `extras` key for a monitor's saved pill position: `{prefix}{monitor_id}`.
/// The host supplies `prefix` (for example `"pill_position_"`) so the persisted key stays its own.
pub fn pill_position_key(prefix: &str, monitor_id: &str) -> String {
    format!("{prefix}{monitor_id}")
}

/// The settings patch a screenshot-destination choice makes: the destination plus the flag that
/// the first-run choice was made. Feed it to `apply_patch_to` so a native pill and a renderer
/// write the same keys.
pub fn screenshot_destination_patch(destination: &str) -> Value {
    serde_json::json!({
        "screenshot_destination": destination,
        "screenshot_prompt_selected": true,
    })
}

impl SettingsBlob {
    /// Stored close behaviour, or [`DEFAULT_CLOSE_BEHAVIOR`].
    pub fn close_behavior_or_default(&self) -> String {
        self.close_behavior
            .clone()
            .unwrap_or_else(|| DEFAULT_CLOSE_BEHAVIOR.to_string())
    }

    /// Stored delivery method, or [`DEFAULT_DELIVERY_METHOD`].
    pub fn delivery_method_or_default(&self) -> String {
        self.delivery_method
            .clone()
            .unwrap_or_else(|| DEFAULT_DELIVERY_METHOD.to_string())
    }

    /// Whether local diagnostics may be written: the stored consent, else the default.
    pub fn diagnostics_enabled_or_default(&self) -> bool {
        self.diagnostics_enabled
            .unwrap_or(DEFAULT_DIAGNOSTICS_ENABLED)
    }

    /// Stored history retention, or [`DEFAULT_HISTORY_RETENTION`].
    pub fn history_retention_or_default(&self) -> HistoryRetention {
        self.history_retention.unwrap_or(DEFAULT_HISTORY_RETENTION)
    }

    /// Whether AI polish is on: the stored flag, else the default.
    pub fn ai_polish_enabled_or_default(&self) -> bool {
        self.ai_polish_enabled.unwrap_or(DEFAULT_AI_POLISH_ENABLED)
    }

    /// Raw onboarding flag, independent of legal state. Use it to choose which surface to show.
    /// Gating ("is the app usable") is `onboarding_done() && legal_acceptance_current()`.
    pub fn onboarding_done(&self) -> bool {
        self.onboarding_complete.unwrap_or(false)
    }

    /// Onboarding finished AND the current legal terms accepted: the gate for anything that must
    /// not run on an unconfigured or unaccepted install.
    pub fn onboarding_gate_open(&self) -> bool {
        self.onboarding_done() && self.legal_acceptance_current()
    }

    /// Push-to-talk on? Defaults to on.
    pub fn push_to_talk_or_default(&self) -> bool {
        self.push_to_talk.unwrap_or(true)
    }

    /// Selected push-to-talk key, or [`DEFAULT_PUSH_TO_TALK_KEY`].
    pub fn push_to_talk_key_or_default(&self) -> String {
        self.push_to_talk_key
            .clone()
            .unwrap_or_else(|| DEFAULT_PUSH_TO_TALK_KEY.to_string())
    }

    /// Selected macOS push-to-talk key, or [`DEFAULT_MAC_PUSH_TO_TALK_KEY`].
    pub fn mac_push_to_talk_key_or_default(&self) -> String {
        self.mac_push_to_talk_key
            .clone()
            .unwrap_or_else(|| DEFAULT_MAC_PUSH_TO_TALK_KEY.to_string())
    }

    /// Middle-mouse recording on? Defaults to on.
    pub fn middle_mouse_recording_or_default(&self) -> bool {
        self.middle_mouse_recording.unwrap_or(true)
    }

    /// Saved snippets, empty when none.
    pub fn snippets_or_empty(&self) -> HashMap<String, String> {
        self.snippets.clone().unwrap_or_default()
    }

    /// Audio-conditioning policy name for the engine: [`AUDIO_POLICY_CONDITIONED`] when the
    /// `audio_conditioning` setting is on (default on), else [`AUDIO_POLICY_RAW`]. The engine
    /// feeds it to `audio_conditioning::condition_for_asr`, the only conditioning authority.
    pub fn audio_policy(&self) -> &'static str {
        if self
            .audio_conditioning
            .unwrap_or(super::blob::DEFAULT_AUDIO_CONDITIONING)
        {
            AUDIO_POLICY_CONDITIONED
        } else {
            AUDIO_POLICY_RAW
        }
    }

    /// Stored pill visibility, or [`DEFAULT_PILL_VISIBILITY`].
    pub fn pill_visibility_or_default(&self) -> String {
        self.pill_visibility
            .clone()
            .unwrap_or_else(|| DEFAULT_PILL_VISIBILITY.to_string())
    }

    /// Pill visibility reduced to `"recording_only"` or `"always"` ([`normalize_pill_visibility`]).
    pub fn pill_visibility_normalized(&self) -> &'static str {
        normalize_pill_visibility(&self.pill_visibility_or_default())
    }

    /// The model/execution-provider choice kept in `extras` under [`MODEL_CHOICE_KEY`], if any.
    pub fn model_choice(&self) -> Option<String> {
        self.extras
            .get(MODEL_CHOICE_KEY)
            .and_then(|v| v.as_str().map(str::to_string))
    }

    /// Sets the model choice in `extras`. Typed host-internal write: it bypasses the renderer
    /// patch allowlist by design.
    pub fn set_model_choice(&mut self, value: &str) {
        self.extras.insert(
            MODEL_CHOICE_KEY.to_string(),
            Value::String(value.to_string()),
        );
    }

    /// Sets the close behaviour after validating it against the patch allowlist.
    pub fn set_close_behavior(&mut self, value: &str) -> Result<(), String> {
        validate_close_behavior(value)?;
        self.close_behavior = Some(value.to_string());
        Ok(())
    }

    /// Saved pill position for a monitor as physical pixels `(x, y)`, stored in `extras` under
    /// `{key_prefix}{monitor_id}` as `{"x": f64, "y": f64}`. `None` when never saved or malformed.
    pub fn pill_position_for_monitor(
        &self,
        key_prefix: &str,
        monitor_id: &str,
    ) -> Option<(f64, f64)> {
        let object = self
            .extras
            .get(&pill_position_key(key_prefix, monitor_id))?
            .as_object()?;
        Some((object.get("x")?.as_f64()?, object.get("y")?.as_f64()?))
    }

    /// Saves a pill position for a monitor under `{key_prefix}{monitor_id}`. Typed host-internal
    /// write into `extras`; it bypasses the renderer patch allowlist by design.
    pub fn set_pill_position_for_monitor(
        &mut self,
        key_prefix: &str,
        monitor_id: &str,
        x: f64,
        y: f64,
    ) {
        self.extras.insert(
            pill_position_key(key_prefix, monitor_id),
            serde_json::json!({ "x": x, "y": y }),
        );
    }

    /// Replaces a persisted `asr_backend` equal to `retired` with `replacement`, once the host
    /// has verified the replacement model on disk. Returns whether it changed. Any other
    /// selection is left alone.
    pub fn replace_retired_asr_backend(&mut self, retired: &str, replacement: &str) -> bool {
        if self.asr_backend.as_deref() == Some(retired) {
            self.asr_backend = Some(replacement.to_string());
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_audio_policy() {
        let mut b = SettingsBlob::default();
        assert_eq!(b.close_behavior_or_default(), DEFAULT_CLOSE_BEHAVIOR);
        assert!(b.push_to_talk_or_default());
        assert_eq!(b.audio_policy(), AUDIO_POLICY_CONDITIONED);
        b.audio_conditioning = Some(false);
        assert_eq!(b.audio_policy(), AUDIO_POLICY_RAW);
    }

    #[test]
    fn pill_visibility_and_positions() {
        let mut b = SettingsBlob::default();
        b.pill_visibility = Some("during_dictation".into());
        assert_eq!(b.pill_visibility_normalized(), "recording_only");
        b.set_pill_position_for_monitor("p_", "m0", 1.0, 2.0);
        assert_eq!(b.pill_position_for_monitor("p_", "m0"), Some((1.0, 2.0)));
        assert_eq!(b.pill_position_for_monitor("p_", "m1"), None);
    }

    #[test]
    fn close_behavior_validates_and_retired_switch() {
        let mut b = SettingsBlob::default();
        assert!(b.set_close_behavior("nope").is_err());
        assert!(b.set_close_behavior("quit").is_ok());
        b.asr_backend = Some("old".into());
        assert!(b.replace_retired_asr_backend("old", "new"));
        assert!(!b.replace_retired_asr_backend("old", "new"));
    }
}
