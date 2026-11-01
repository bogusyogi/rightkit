use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use rightkit_dictation_core::settings::{
    migrate, SettingsBlob, DEFAULT_AI_POLISH_MODE, DEFAULT_ASR_BACKEND,
    DEFAULT_SCREENSHOT_DESTINATION, DEFAULT_TRANSCRIPT_CLEANUP,
};

static SETTINGS_OVERRIDE: OnceLock<Mutex<Option<SettingsBlob>>> = OnceLock::new();

const CALIBRATION_ROUTE_FILE: &str = "recognition-route.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedRecognitionRoute {
    pub id: String,
    pub encoder_compute: String,
    pub decoder_compute: String,
    pub provider: String,
    #[serde(default)]
    pub adapter_index: Option<i32>,
    #[serde(default)]
    pub adapter_name: Option<String>,
}

fn calibration_route_path() -> PathBuf {
    app_data_root().join(CALIBRATION_ROUTE_FILE)
}

pub fn persisted_recognition_route() -> Option<PersistedRecognitionRoute> {
    let bytes = fs::read(calibration_route_path()).ok()?;
    serde_json::from_slice(&bytes)
        .ok()
        .filter(valid_persisted_recognition_route)
}

fn valid_persisted_recognition_route(route: &PersistedRecognitionRoute) -> bool {
    let no_adapter = route.adapter_index.is_none() && route.adapter_name.is_none();
    match route.provider.as_str() {
        "dml" => {
            route
                .adapter_index
                .is_some_and(|index| route.id == format!("dml-{index}"))
                && route.encoder_compute == "dml"
                && route.decoder_compute == "dml"
                && route
                    .adapter_name
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
        }
        "cpu" => {
            no_adapter
                && route.id == "cpu-only"
                && route.encoder_compute == "cpu_only"
                && route.decoder_compute == "cpu_only"
        }
        "coreml" => {
            no_adapter
                && matches!(
                    (
                        route.id.as_str(),
                        route.encoder_compute.as_str(),
                        route.decoder_compute.as_str()
                    ),
                    ("ane-all", "neural_engine", "neural_engine")
                        | ("ane-encoder-cpu-decoder", "neural_engine", "cpu_only")
                        | ("gpu-encoder-ane-decoder", "cpu_gpu", "neural_engine")
                        | ("gpu-encoder-cpu-decoder", "cpu_gpu", "cpu_only")
                        | ("gpu-all", "cpu_gpu", "cpu_gpu")
                        | ("cpu-only", "cpu_only", "cpu_only")
                )
        }
        _ => false,
    }
}

/// Persist only a route that completed the fixed onboarding read. Write+rename
/// ensures normal dictation never sees a partially-written selection.
pub fn persist_recognition_route(route: &PersistedRecognitionRoute) -> Result<(), String> {
    if !valid_persisted_recognition_route(route) {
        return Err("invalid recognition route".to_string());
    }
    #[cfg(test)]
    if !app_data_root_is_explicit_override() {
        panic!(
            "test process must not write recognition route outside RIGHTKIT_APP_DATA_DIR override"
        );
    }
    let path = calibration_route_path();
    let bytes =
        serde_json::to_vec(route).map_err(|error| format!("encode recognition route: {error}"))?;
    let options = rightkit_fs::atomic::AtomicWriteOptions {
        create_parents: true,
        ..Default::default()
    };
    rightkit_fs::atomic::atomic_write_with_options(&path, options, |file| {
        std::io::Write::write_all(file, &bytes)
    })
    .map_err(|error| format!("write recognition route: {error}"))
}

/// Calibration candidate route for THIS process. Calibration runs in an isolated process; this
/// lets each candidate use its exact compute route without mutating normal dictation settings.
/// Process-local state, never the environment (release switch rule: no env selectors). The
/// DirectML adapter index lives in rightkit-dictation-inference (`calibration::set_active_route`).
static CALIBRATION_ROUTE: std::sync::RwLock<Option<(String, String)>> =
    std::sync::RwLock::new(None);

pub fn apply_calibration_route_environment(route: &PersistedRecognitionRoute) {
    if let Ok(mut slot) = CALIBRATION_ROUTE.write() {
        *slot = Some((route.encoder_compute.clone(), route.decoder_compute.clone()));
    }
}

/// Clears the calibration candidate (tests; a process never leaves calibration otherwise).
#[doc(hidden)]
pub fn clear_calibration_route() {
    if let Ok(mut slot) = CALIBRATION_ROUTE.write() {
        *slot = None;
    }
}

pub fn onboarding_calibration_active() -> bool {
    CALIBRATION_ROUTE
        .read()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

fn calibration_compute_override(role: &str) -> Option<String> {
    let slot = CALIBRATION_ROUTE.read().ok()?;
    let (encoder, decoder) = slot.as_ref()?;
    let value = match role {
        "encoder" => encoder,
        "decoder" => decoder,
        _ => return None,
    };
    (!value.trim().is_empty()).then(|| value.clone())
}

fn persisted_compute(role: &str) -> Option<String> {
    persisted_recognition_route().map(|route| match role {
        "encoder" => route.encoder_compute,
        "decoder" => route.decoder_compute,
        _ => unreachable!(),
    })
}

pub fn replace_runtime_config(config: SettingsBlob) {
    let mut blob = config;
    migrate(&mut blob);
    let cell = SETTINGS_OVERRIDE.get_or_init(|| Mutex::new(None));
    if let Ok(mut slot) = cell.lock() {
        *slot = Some(blob);
    }
}

pub use rightkit_dictation_core::app_paths::{
    app_data_root, app_data_root_is_explicit_override, test_binary_data_root,
};

pub fn wake_lab_marker_present() -> bool {
    app_data_root().join("owner-idle-wake.enabled").is_file()
}

/// The last settings read from disk, returned when a later read is refused.
static LAST_READ: Mutex<Option<SettingsBlob>> = Mutex::new(None);

fn settings_path() -> PathBuf {
    app_data_root().join("settings.json")
}

fn load_settings() -> SettingsBlob {
    if let Some(blob) = SETTINGS_OVERRIDE
        .get()
        .and_then(|cell| cell.lock().ok().and_then(|slot| slot.clone()))
    {
        return blob;
    }
    let path = settings_path();
    let bytes = match rightkit_fs::replaced::read_replaced(&path) {
        Ok(bytes) => bytes,
        // Only a missing file means "no settings". A refused read keeps the last settings
        // that were read; defaults are the fallback only when nothing was ever read.
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            if let Some(blob) = LAST_READ.lock().ok().and_then(|slot| slot.clone()) {
                return blob;
            }
            let mut blob = SettingsBlob::default();
            migrate(&mut blob);
            return blob;
        }
        Err(_) => {
            let mut blob = SettingsBlob::default();
            migrate(&mut blob);
            return blob;
        }
    };
    match serde_json::from_slice::<SettingsBlob>(&bytes) {
        Ok(mut blob) => {
            migrate(&mut blob);
            if let Ok(mut slot) = LAST_READ.lock() {
                *slot = Some(blob.clone());
            }
            blob
        }
        Err(_) => {
            let mut blob = SettingsBlob::default();
            migrate(&mut blob);
            blob
        }
    }
}

/// How a process that can verify the licence itself answers "is this a paid session". A native
/// authoriser (the owner's `NativeEngineEntitlements` reading the licence record) is installed once,
/// natively, by the process that owns it; it is the only source then and a pushed flag is ignored.
static PRO_AUTHORIZER: OnceLock<fn() -> bool> = OnceLock::new();

/// Install the native Pro authoriser for this process (first installation wins). It must verify the
/// signed licence on each call; it must never read a renderer value, a settings file or the
/// environment.
pub fn install_pro_authorizer(authorize: fn() -> bool) {
    let _ = PRO_AUTHORIZER.set(authorize);
}

/// The Pro flag the native shell pushed over the engine IPC (`replace_runtime_config`). Only that
/// push counts: the shell derives it on every config sync from the native licence verifier
/// (`license::has_pro`), and the IPC channel is not reachable from the renderer. The Pro flag in
/// the persisted settings file is deliberately NOT read, so a hand-edited or stale `is_pro` on disk
/// can never authorise anything.
fn pushed_pro_flag() -> Option<bool> {
    SETTINGS_OVERRIDE.get().and_then(|cell| {
        cell.lock()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(|blob| blob.is_pro))
    })
}

/// Key under `SettingsBlob::extras` that carries the product-set cloud-cleanup policy in the
/// runtime config push.
pub const L3_POLICY_KEY: &str = "l3_policy";

/// What the native shell decided about cloud cleanup, derived from the user's settings and key
/// readiness. It rides the pushed runtime config (private stdin channel), never the environment.
/// Like `is_pro`, only the push counts: the persisted settings file is never read for it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct L3Policy {
    /// AI polish is on (the privacy gate for capturing field context).
    #[serde(default)]
    pub cleanup_enabled: bool,
    /// The user's cloud consent is in force and a provider is ready.
    #[serde(default)]
    pub cloud_consent: bool,
    /// Provider readiness label for telemetry: `ready`, `groq_key_absent`, `groq_key_read_error`.
    #[serde(default)]
    pub provider_state: String,
}

impl L3Policy {
    /// The value to store under [`L3_POLICY_KEY`] in the pushed config's `extras`.
    pub fn to_extra(&self) -> serde_json::Value {
        serde_json::json!({
            "cleanup_enabled": self.cleanup_enabled,
            "cloud_consent": self.cloud_consent,
            "provider_state": self.provider_state,
        })
    }
}

/// The policy the native shell pushed, or `None` when none was pushed (or the value is malformed;
/// callers then fail closed).
pub fn pushed_l3_policy() -> Option<L3Policy> {
    SETTINGS_OVERRIDE.get().and_then(|cell| {
        cell.lock().ok().and_then(|slot| {
            slot.as_ref()
                .and_then(|blob| blob.extras.get(L3_POLICY_KEY))
                .and_then(|value| serde_json::from_value::<L3Policy>(value.clone()).ok())
        })
    })
}

/// Unit tests (`cfg(test)`) default to Pro so the engine command tests do not each need to push a
/// licence; a test that wants the Free path pushes `is_pro: Some(false)` via replace_runtime_config.
#[cfg(test)]
fn unpushed_default() -> bool {
    true
}

/// Engine test-mode builds (feature `test-mode`) default to Pro only when the harness asks for it
/// with `RIGHTKIT_ENGINE_TEST_MODE` (entitlement default only; the delivery and runtime
/// selectors of that variable are AP-HR-16). The read does not exist in any other build.
#[cfg(all(not(test), feature = "test-mode"))]
fn unpushed_default() -> bool {
    std::env::var("RIGHTKIT_ENGINE_TEST_MODE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// A shipped build fails closed: nothing pushed and no native authoriser means Free.
#[cfg(all(not(test), not(feature = "test-mode")))]
fn unpushed_default() -> bool {
    false
}

/// Pro entitlement for paid commands in this process: the installed native authoriser when there is
/// one, else the flag the native shell pushed, else the fail-closed default (Free).
pub fn is_pro() -> bool {
    if let Some(authorize) = PRO_AUTHORIZER.get() {
        return authorize();
    }
    pushed_pro_flag().unwrap_or_else(unpushed_default)
}

pub fn diagnostics_enabled() -> bool {
    true
}

fn compute_profile(value: Option<String>, fallback: &str, allow_inherit: bool) -> String {
    match value.as_deref() {
        Some("neural_engine") => "neural_engine",
        Some("cpu_gpu") => "cpu_gpu",
        Some("cpu_only") => "cpu_only",
        Some("inherit") if allow_inherit => "inherit",
        _ => fallback,
    }
    .to_string()
}

pub fn asr_compute_profile() -> String {
    compute_profile(load_settings().asr_compute_profile, "automatic", false)
}

pub fn asr_encoder_compute() -> String {
    asr_role_compute("encoder")
}

pub fn asr_decoder_compute() -> String {
    asr_role_compute("decoder")
}

/// Per-stage compute for the encoder/decoder: the calibration process's env
/// override, else the route chosen by onboarding or the Developer route test,
/// else `"inherit"` (the global profile).
fn asr_role_compute(role: &str) -> String {
    if let Some(value) = calibration_compute_override(role) {
        return compute_profile(Some(value), "inherit", true);
    }
    if let Some(value) = persisted_compute(role) {
        return compute_profile(Some(value), "inherit", true);
    }
    "inherit".to_string()
}

pub fn asr_detailed_telemetry() -> bool {
    !onboarding_calibration_active() && load_settings().asr_detailed_telemetry.unwrap_or(false)
}

pub fn diagnostic_audio_capture() -> bool {
    load_settings().diagnostic_audio_capture.unwrap_or(false)
}

/// Streaming insertion runs only while Developer mode is on.
pub fn streaming_insertion() -> bool {
    let settings = load_settings();
    settings.developer_mode.unwrap_or(false) && settings.streaming_insertion.unwrap_or(false)
}

pub fn diagnostic_unredacted_logs() -> bool {
    load_settings().diagnostic_unredacted_logs.unwrap_or(false)
}

pub fn screenshot_destination() -> String {
    match load_settings()
        .screenshot_destination
        .unwrap_or_else(|| DEFAULT_SCREENSHOT_DESTINATION.to_string())
        .as_str()
    {
        "disk" => "disk".to_string(),
        "both" => "both".to_string(),
        _ => DEFAULT_SCREENSHOT_DESTINATION.to_string(),
    }
}

pub fn snippets() -> HashMap<String, String> {
    load_settings().snippets.unwrap_or_default()
}

/// Deterministic text replacements ("always fix X to Y"), applied at the
/// local polish layer. Distinct from vocabulary and snippets.
pub fn replacements() -> HashMap<String, String> {
    load_settings().replacements.unwrap_or_default()
}

/// Saved mic selection string (e.g. `id:2:USB Mic`), or None for system default.
/// The worker resolves this to a device index via `resolve_mic_selection`.
pub fn input_device() -> Option<String> {
    load_settings().input_device
}

pub fn asr_backend() -> String {
    match load_settings()
        .asr_backend
        .unwrap_or_else(|| DEFAULT_ASR_BACKEND.to_string())
        .as_str()
    {
        "parakeet-unified" | "parakeet" | "parakeet-rnnt" | "rnnt" => {
            DEFAULT_ASR_BACKEND.to_string()
        }
        "parakeet-tdt" => "parakeet-tdt".to_string(),
        "whisper" | "whisper-cli" => "whisper".to_string(),
        _ => DEFAULT_ASR_BACKEND.to_string(),
    }
}

pub fn transcript_cleanup() -> String {
    match load_settings()
        .transcript_cleanup
        .unwrap_or_else(|| DEFAULT_TRANSCRIPT_CLEANUP.to_string())
        .as_str()
    {
        "clean" => "clean".to_string(),
        "aggressive" => "aggressive".to_string(),
        _ => DEFAULT_TRANSCRIPT_CLEANUP.to_string(),
    }
}

pub fn ai_polish_mode() -> String {
    match load_settings()
        .ai_polish_mode
        .unwrap_or_else(|| DEFAULT_AI_POLISH_MODE.to_string())
        .as_str()
    {
        "app" => "app".to_string(),
        _ => DEFAULT_AI_POLISH_MODE.to_string(),
    }
}

/// Recognition language. `"auto"` = Parakeet auto-detect (free). A specific code
/// = lock Whisper to that language (Pro; the shell already sanitized this to
/// "auto" for non-Pro before pushing). Precedence: pushed setting / on-disk
/// settings → `RIGHTKIT_DICTATION_LANG` env (test and `test-mode` builds only) → "auto".
pub fn dictation_language() -> String {
    // The live pushed config / on-disk settings WIN over a spawn-time env seed:
    // a language change in Settings (pushed via ReplaceEngineConfig) must reach
    // the running engine. The env is only a last-resort seed for a test harness
    // running the engine without a shell; a release build never reads it.
    if let Some(v) = load_settings()
        .dictation_language
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
    {
        return v;
    }
    rightkit_dictation_core::dev_var!("RIGHTKIT_DICTATION_LANG")
        .ok()
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "auto".to_string())
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
