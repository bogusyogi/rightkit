// Settings — pure blob shape, defaults, and the allowlist patch-merge.
//
// Disk persistence, the global singleton, and the typed accessors live in
// `src-tauri/src/settings.rs`. The PATCH allowlist is the IPC attack surface
// (untrusted renderer input), so its validation is the highest-value thing to
// unit-test — and it's pure (`apply_patch_to` mutates a borrowed blob, no IO).

use rightkit_transcript_library::history::HistoryRetention;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

// ---- Defaults (per APP_DESIGN §4.3) ----

pub const DEFAULT_CLOSE_BEHAVIOR: &str = "destroy"; // world-class ephemeral hub default
pub const DEFAULT_SOUND_EFFECTS_ON: bool = false;
pub const DEFAULT_SOUND_EFFECTS_VOLUME: u8 = 50;
pub const DEFAULT_MEDIA_DUCKING_ON: bool = false;
// Recognition model — a registry alias the Rust engine resolves to an on-disk
// model directory. Default = Unified English, the locked Windows product lane.
// Explicit `parakeet-tdt` remains available as a rollback/control lane.
// `whisper` is the Pro multilingual lane; platform-specific runtimes resolve
// its concrete artifact.
pub const DEFAULT_ASR_BACKEND: &str = "parakeet-tdt";
pub const DEFAULT_AUDIO_CONDITIONING: bool = true; // asr_simple_gain_hpf on by default
pub const DEFAULT_DIAGNOSTICS_ENABLED: bool = true;
pub const DEFAULT_HISTORY_RETENTION: HistoryRetention = HistoryRetention::Forever;
pub const DEFAULT_TELEMETRY_USAGE: bool = false; // §12 — disabled by default
pub const DEFAULT_APP_THEME: &str = "ember";
pub const DEFAULT_APP_FONT: &str = "system";
pub const DEFAULT_HOTKEY_TOGGLE: &str = "Ctrl+Space"; // production lock
pub const DEFAULT_HOTKEY_CANCEL: &str = "Ctrl+Esc";
pub const DEFAULT_HOTKEY_OPEN_HUB: &str = "Ctrl+,";
pub const DEFAULT_PILL_VISIBILITY: &str = "always";
pub const DEFAULT_TRANSCRIPT_CLEANUP: &str = "aggressive";
pub const DEFAULT_AI_POLISH_ENABLED: bool = false;
pub const DEFAULT_AI_POLISH_MODE: &str = "app";
// Text delivery method into the focused app. `paste` = clipboard + ⌘V (default,
// works everywhere incl. Chromium/Electron). `keystroke` = synthetic unicode
// typing (no clipboard touch) for apps that mishandle programmatic paste.
pub const DEFAULT_DELIVERY_METHOD: &str = "paste";
// Voice command: "screenshot". Default clipboard-only because the most common
// workflow is pasting into chat/tools. Users can switch to disk-only or both
// from Commands.
pub const DEFAULT_SCREENSHOT_DESTINATION: &str = "clipboard";
// Push-to-talk key. `"lalt"` = Left Alt, `"ralt"` = Right Alt (AltGr on EU
// layouts; easy reach, next to Space), `"rctrl"` = Right Ctrl (quieter — fewer
// system shortcuts collide, harder to reach on TKL/60%). All are single-key,
// non-suppressing
// triggers so `windows+...` commands dispatch cleanly mid-PTT.
pub const DEFAULT_PUSH_TO_TALK_KEY: &str = "ralt";
pub const DEFAULT_MAC_PUSH_TO_TALK_KEY: &str = "ctrl";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegalContract {
    acceptance_version: String,
    material_sha256: String,
}

static LEGAL_CONTRACT: OnceLock<LegalContract> = OnceLock::new();

/// The host app's generated legal acceptance contract (its `src/generated/legal-contract.json`, produced from
/// `legal/legal-manifest.json`). A published crate cannot include a file of the app that depends on it, so the host
/// hands it over once at startup. Until it does, no legal acceptance is current and no acceptance patch is admitted.
/// A second call is refused unless it carries the same contract.
pub fn configure_legal_contract(json: &str) -> Result<(), String> {
    let contract: LegalContract =
        serde_json::from_str(json).map_err(|e| format!("legal contract is invalid: {e}"))?;
    if contract.acceptance_version.trim().is_empty() || contract.material_sha256.trim().is_empty() {
        return Err("legal contract needs acceptanceVersion and materialSha256".to_string());
    }
    match LEGAL_CONTRACT.set(contract) {
        Ok(()) => Ok(()),
        Err(again) => {
            let current = LEGAL_CONTRACT.get().expect("set");
            if current.acceptance_version == again.acceptance_version
                && current.material_sha256 == again.material_sha256
            {
                Ok(())
            } else {
                Err("a different legal contract is already configured".to_string())
            }
        }
    }
}

/// The acceptance version the host's contract requires; `None` until [`configure_legal_contract`].
pub fn required_legal_acceptance_version() -> Option<&'static str> {
    LEGAL_CONTRACT.get().map(|c| c.acceptance_version.as_str())
}

/// Digest over only the MATERIAL legal documents (license/eula/acceptable_use/
/// product_schedule) — see `@rightkit/legal`'s `materialAcceptanceDigest`.
/// Deliberately NOT a hash of the whole manifest: privacy_notice and
/// third_party_notices regenerate on every dependency bump and must never
/// force a re-prompt for a byte-identical EULA.
pub fn required_legal_material_sha256() -> Option<&'static str> {
    LEGAL_CONTRACT.get().map(|c| c.material_sha256.as_str())
}

/// The renderer records `Date::toISOString()`. Accept only that canonical UTC
/// RFC3339 subset (`YYYY-MM-DDTHH:MM:SS.mmmZ`) and reject impossible calendar dates
/// before persisting it. Calendar validation is `rightkit_time::DateTime::parse_rfc3339`;
/// the canonical shape and the no-leap-second / no-year-zero rules stay here.
pub fn legal_accepted_at_is_canonical_utc(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 24 || bytes[10] != b'T' || bytes[19] != b'.' || bytes[23] != b'Z' {
        return false;
    }
    rightkit_time::DateTime::parse_rfc3339(value)
        .is_some_and(|time| time.year != 0 && time.second <= 59)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsBlob {
    // Application
    #[serde(default)]
    pub close_behavior: Option<String>,
    #[serde(default)]
    pub app_theme: Option<String>,
    #[serde(default)]
    pub app_font: Option<String>,
    // Keyboard
    #[serde(default)]
    pub hotkey_toggle: Option<String>,
    #[serde(default)]
    pub hotkey_cancel: Option<String>,
    #[serde(default)]
    pub hotkey_open_hub: Option<String>,
    // Mic & audio
    #[serde(default)]
    pub input_device: Option<String>,
    #[serde(default)]
    pub sound_effects_on: Option<bool>,
    #[serde(default)]
    pub sound_effects_volume: Option<u8>,
    #[serde(default)]
    pub media_ducking_on: Option<bool>,
    #[serde(default)]
    pub pill_visibility: Option<String>,
    // Engine
    #[serde(default)]
    pub asr_backend: Option<String>,
    // Recognition language. `None`/`"auto"` = Parakeet auto-detect across its 25
    // European languages (free). A specific lang code (e.g. "fr", "ja") = lock to
    // that language via Whisper (Pro) — Parakeet has no language input, so forcing
    // a language requires the Whisper path. The shell sanitizes this to "auto" for
    // non-Pro users before pushing to the engine.
    #[serde(default)]
    pub dictation_language: Option<String>,
    #[serde(default)]
    pub transcript_cleanup: Option<String>,
    #[serde(default)]
    pub ai_polish_enabled: Option<bool>,
    #[serde(default)]
    pub ai_polish_mode: Option<String>,
    #[serde(default)]
    pub delivery_method: Option<String>,
    #[serde(default)]
    pub screenshot_destination: Option<String>,
    // Whether the user has made the first-run screenshot destination choice.
    // This intentionally lives in native settings, not renderer localStorage:
    // WebView storage can survive install-over flows in ways that make a
    // wiped settings profile behave as if the prompt had already been seen.
    #[serde(default)]
    pub screenshot_prompt_selected: Option<bool>,
    // Whether the one-time "cancel saved to history" pill hint has been shown
    // yet. Same claim-once shape as `screenshot_prompt_selected`: native
    // settings state (not renderer localStorage) so a wiped settings profile
    // can't make the app think the hint was already seen.
    #[serde(default)]
    pub cancel_hint_seen: Option<bool>,
    // Pro entitlement, derived from the keyring license and pushed by the shell
    // at engine-config sync time (see src-tauri sync_sidecar_engine_config). It
    // gates Pro-only voice commands (the Shortcuts bridge) in the engine.
    // rides the existing SettingsBlob push instead of a bespoke IPC.
    // NOT in the apply_patch allowlist (users can't set it) and never persisted
    // by the shell — always recomputed from the license on each push.
    #[serde(default)]
    pub is_pro: Option<bool>,
    // Audio conditioning (asr_simple_gain_hpf: DC removal + HPF + quiet gain).
    // Default-on = matches the shipped daemon. Disabling this developer-level
    // setting sends raw 16k audio to the engine for controlled A/B checks.
    #[serde(default)]
    pub audio_conditioning: Option<bool>,
    // Local diagnostics. Privacy is the baseline: ordinary diagnostics are
    // always content-redacted and never uploaded automatically. This switch
    // controls whether those local files are written at all.
    #[serde(default)]
    pub diagnostics_enabled: Option<bool>,
    // Developer controls are visible to every user but collapsed by default.
    // Sensitive capture options remain independently opt-in.
    #[serde(default)]
    pub developer_mode: Option<bool>,
    // One sync provider per account: "icloud" | "google" | "off". Absent means
    // "not chosen yet"; resolve through `resolve_sync_provider`, which maps an
    // existing iCloud-consented user to "icloud" (rightkit#162, issue #17).
    #[serde(default)]
    pub sync_provider: Option<String>,
    #[serde(default)]
    pub asr_compute_profile: Option<String>,
    #[serde(default)]
    pub asr_detailed_telemetry: Option<bool>,
    #[serde(default)]
    pub diagnostic_audio_capture: Option<bool>,
    #[serde(default)]
    pub diagnostic_unredacted_logs: Option<bool>,
    /// Developer-mode experiment: type stable words into the focused field
    /// while recording. Effective only while `developer_mode` is on.
    #[serde(default)]
    pub streaming_insertion: Option<bool>,
    // Local encrypted history lifecycle. Missing means Forever so existing
    // users never lose records merely by upgrading.
    #[serde(default)]
    pub history_retention: Option<HistoryRetention>,
    // Auto-install signed updates after the background check. Default OFF:
    // the app may auto-check and notify, but downloading/installing should be
    // user-initiated unless this opt-in is set.
    #[serde(default)]
    pub auto_update_install: Option<bool>,
    // Snippets / text expansion. Legacy shape: { trigger: replacement }.
    #[serde(default)]
    pub snippets: Option<HashMap<String, String>>,
    // Deterministic text replacements applied at the LOCAL polish layer:
    // { "mishear or phrase": "replacement" }, case-insensitive whole-word.
    // Distinct from vocabulary (spelling/casing guidance) and snippets
    // (trigger expansion) — this is the "always fix X to Y" lever.
    #[serde(default)]
    pub replacements: Option<HashMap<String, String>>,
    // Controls
    #[serde(default)]
    pub push_to_talk: Option<bool>,
    // Single-key PTT key. Values: "lalt", "ralt" (default, ergonomic), "rctrl"
    // (quieter, harder reach). Stored as a string so the IPC allowlist stays
    // trivial and future keys (e.g. "scroll_lock") don't need a schema bump.
    #[serde(default)]
    pub push_to_talk_key: Option<String>,
    // macOS keeps its own key because existing installs used Control while
    // Windows persisted Right Alt by default. Values: ctrl, lalt, ralt.
    #[serde(default)]
    pub mac_push_to_talk_key: Option<String>,
    #[serde(default)]
    pub middle_mouse_recording: Option<bool>,
    // Misc
    #[serde(default)]
    pub onboarding_complete: Option<bool>,
    // Version of the onboarding flow the user last completed. Install-over and
    // drag-to-Trash uninstall both leave Application Support behind, so bumping
    // the shell's required version is the only way to walk existing installs
    // through a redesigned wizard once.
    #[serde(default)]
    pub onboarding_version: Option<u32>,
    // Versioned, local proof of affirmative assent. This is deliberately
    // separate from the paid-feature key: legal permission and product
    // entitlement are independent contracts.
    #[serde(default)]
    pub legal_acceptance_version: Option<String>,
    #[serde(default)]
    pub legal_accepted_at: Option<String>,
    #[serde(default)]
    pub legal_eligibility_basis: Option<String>,
    #[serde(default)]
    pub legal_material_sha256: Option<String>,
    // Schema version of the persisted blob. `None` = pre-versioning (v0).
    // Written on every save so future upgrades can migrate deterministically
    // instead of silently resetting. Not user-patchable (not in the allowlist).
    #[serde(default)]
    pub schema_version: Option<u32>,
    // Forward-compat
    #[serde(flatten, default)]
    pub extras: HashMap<String, Value>,
}

/// Current on-disk settings schema version. Bump when the blob shape changes in
/// a way that needs a transform, and add a matching arm to [`migrate`].
pub const CURRENT_SCHEMA_VERSION: u32 = 13;

/// Current onboarding wizard generation. New completions stamp this value;
/// existing completed users keep their completed state & can rerun recognition
/// calibration from Developer Mode.
pub const REQUIRED_ONBOARDING_VERSION: u32 = 3;

/// Bring a loaded blob up to [`CURRENT_SCHEMA_VERSION`]. Runs versioned
/// transforms in order. Returns `true` if anything changed (so the caller can
/// persist the upgraded blob). Pure — no lock, no disk.
pub fn migrate(blob: &mut SettingsBlob) -> bool {
    let from = blob.schema_version.unwrap_or(0);
    if from >= CURRENT_SCHEMA_VERSION {
        return false;
    }
    // v0 -> v1: introduce the version tag. No field transforms.
    // v1 -> v2: retire persisted keys that never drove runtime behavior.
    if from < 2 {
        for key in RETIRED_SETTINGS_KEYS {
            blob.extras.remove(*key);
        }
    }
    // v2 -> v3: the bare `parakeet` alias used to mean the old RNN-T lane.
    // Unpinned/default installs always move to the current default product
    // alias, whatever the latest lock is.
    if from < 3 && blob.asr_backend.as_deref() == Some("parakeet") {
        blob.asr_backend = Some(DEFAULT_ASR_BACKEND.to_string());
    }
    // v3 -> v4: hub-lifecycle world-class default. Existing users keep
    // their stored `close_behavior` (minimize / quit); first-run installs
    // (where the field is still `None` after the upgrade) get `destroy`
    // — the new ephemeral-hub default. The hub is created on demand from
    // the shell and destroyed on close; the engine is the only long-lived
    // resident.
    if from < 4 && blob.close_behavior.is_none() {
        blob.close_behavior = Some("destroy".to_string());
    }
    // v3 -> v4: pill visibility default for new installs is `always` so the
    // app has an obvious resident anchor after onboarding. Existing users keep
    // whatever they had stored (legacy `during_dictation` users land on
    // `recording_only`; `always` users stay on `always`).
    if from < 4 && blob.pill_visibility.is_none() {
        blob.pill_visibility = Some("always".to_string());
    }
    // v4 -> v5: retire the old RNNT shell lane. Any persisted alias now
    // resolves to the current product backend.
    if from < 5
        && matches!(
            blob.asr_backend.as_deref(),
            Some("parakeet") | Some("parakeet-rnnt") | Some("rnnt")
        )
    {
        blob.asr_backend = Some(DEFAULT_ASR_BACKEND.to_string());
    }
    // v5 -> v6: old "L2 app-aware formatting" is now folded into product L1.
    // Existing unset or old-default "simple" AI polish should use app-aware L1.
    if from < 6 && matches!(blob.ai_polish_mode.as_deref(), None | Some("simple")) {
        blob.ai_polish_mode = Some(DEFAULT_AI_POLISH_MODE.to_string());
    }
    // v6 -> v7: Unified English replaces TDT as the default product ASR lane.
    // Legacy/default Parakeet aliases move forward; explicit Whisper stays Pro.
    if from < 7
        && matches!(
            blob.asr_backend.as_deref(),
            None | Some("parakeet") | Some("parakeet-tdt") | Some("parakeet-rnnt") | Some("rnnt")
        )
    {
        blob.asr_backend = Some(DEFAULT_ASR_BACKEND.to_string());
    }
    // v7 -> v8: AI polish has one persistent mode, app-aware L1. Prompt and
    // summarize are tail-triggered transforms, not selectable settings.
    if from < 8 && !matches!(blob.ai_polish_mode.as_deref(), Some("app")) {
        blob.ai_polish_mode = Some(DEFAULT_AI_POLISH_MODE.to_string());
    }
    // v8 -> v9: preserve completed users & advance their generation stamp.
    // Recognition calibration is available on demand in Developer Mode; an
    // app update must never force an existing user back through onboarding.
    if from < 9
        && blob.onboarding_complete == Some(true)
        && blob.onboarding_version.unwrap_or(0) < REQUIRED_ONBOARDING_VERSION
    {
        blob.onboarding_version = Some(REQUIRED_ONBOARDING_VERSION);
    }
    // v9 -> v10: PTT became a single user-selectable key
    // (ralt/rctrl). Installations that previously held Ctrl+Win via the now-
    // removed PTT plumbing get RAlt as the default (matches the new
    // ergonomic-first stance documented at DEFAULT_PUSH_TO_TALK_KEY).
    // Users who want RCtrl can pick it in Settings → Controls.
    if from < 10 && blob.push_to_talk_key.is_none() {
        blob.push_to_talk_key = Some(DEFAULT_PUSH_TO_TALK_KEY.to_string());
    }
    // v10 -> v11: reserve version 11 for the legal-receipt fields. Existing
    // users intentionally receive no synthetic assent; first run of this build
    // presents the legal screen and records their affirmative choice.
    // v11 -> v12: retire the legacy privacy toggle. A user who had explicitly
    // enabled it keeps the conservative logs-off preference, but history and
    // every other app feature are no longer coupled to that legacy setting.
    if from < 12 {
        let legacy_privacy_enabled = blob
            .extras
            .remove("privacy_mode")
            .and_then(|value| value.as_bool());
        if blob.diagnostics_enabled.is_none() {
            blob.diagnostics_enabled = Some(!legacy_privacy_enabled.unwrap_or(false));
        }
    }
    // v12 -> v13: add explicit history retention. None intentionally resolves
    // to Forever; no synthetic destructive preference is written.
    blob.schema_version = Some(CURRENT_SCHEMA_VERSION);
    true
}

pub const SYNC_PROVIDERS: [&str; 3] = ["icloud", "google", "off"];
pub const DEFAULT_SYNC_PROVIDER: &str = "off";

/// Effective sync provider. An explicit valid setting wins; absent or invalid
/// falls back to migration: a user who already consented to iCloud sync keeps
/// "icloud", everyone else is "off". Pure.
pub fn resolve_sync_provider(setting: Option<&str>, icloud_sync_enabled: bool) -> &'static str {
    if let Some(v) = setting {
        if let Some(p) = SYNC_PROVIDERS.iter().find(|p| **p == v) {
            return p;
        }
    }
    if icloud_sync_enabled {
        "icloud"
    } else {
        DEFAULT_SYNC_PROVIDER
    }
}

impl SettingsBlob {
    pub fn claim_screenshot_prompt(&mut self) -> bool {
        if self.screenshot_prompt_selected.unwrap_or(false) {
            return false;
        }
        self.screenshot_prompt_selected = Some(true);
        true
    }

    pub fn claim_cancel_hint(&mut self) -> bool {
        if self.cancel_hint_seen.unwrap_or(false) {
            return false;
        }
        self.cancel_hint_seen = Some(true);
        true
    }

    pub fn legal_acceptance_current(&self) -> bool {
        required_legal_acceptance_version().is_some()
            && self.legal_acceptance_version.as_deref() == required_legal_acceptance_version()
            && self
                .legal_accepted_at
                .as_deref()
                .is_some_and(legal_accepted_at_is_canonical_utc)
            && matches!(
                self.legal_eligibility_basis.as_deref(),
                Some("individual") | Some("enterprise")
            )
            && required_legal_material_sha256().is_some()
            && self.legal_material_sha256.as_deref() == required_legal_material_sha256()
    }
}

const RETIRED_SETTINGS_KEYS: &[&str] = &[
    "always_show_recording",
    "auto_check_updates",
    "encoder_mode",
    "error_logging",
    "keep_recordings",
    "launch_on_login",
    "playback_when_recording",
    "preshape_dml_warmup",
    "recording_style",
    "show_engine_mode_in_footer",
    "telemetry_wake_samples",
];
