//! The always-listening wake-word harness: an owner-access marker, a saved threshold, the listen
//! start and stop on the engine, and the start at launch when the owner left it on.
//!
//! Mechanism only. The wake model, the threshold default and bounds, the wording of this harness's
//! errors and both file locations come from the host ([`crate::host::WakeLabProfile`],
//! `DictationHost::wake_lab_marker_path` and `wake_lab_config_path`). The Tauri entry points that
//! call these functions stay in the application, which runs each call on a blocking worker.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::engine_events::start_engine_event_consumer;
use crate::engine_process::WakeListenParams;
use crate::host::{host, WakeLabProfile};
use crate::runtime_lock::{runtime_lock, with_backend_transition};
use crate::shell::Shell;
use crate::telemetry;

// Serialize file/listener transactions; never acquire this mutex from runtime callbacks.
static MUTATION: Mutex<()> = Mutex::new(());

/// The saved settings. The keys are the on-disk format; do not rename them.
#[derive(Clone, Deserialize, Serialize)]
struct WakeLabConfig {
    enabled: bool,
    threshold: f32,
}

/// What the Wake Lab panel reads.
#[derive(Serialize)]
pub struct WakeLabStatus {
    pub marker_present: bool,
    pub enabled: bool,
    pub threshold: f32,
}

fn default_config(profile: &WakeLabProfile) -> WakeLabConfig {
    WakeLabConfig {
        enabled: false,
        threshold: profile.default_threshold,
    }
}

fn clamp_threshold(profile: &WakeLabProfile, threshold: f32) -> f32 {
    let threshold = if threshold.is_finite() {
        threshold
    } else {
        profile.default_threshold
    };
    threshold.clamp(profile.threshold_min, profile.threshold_max)
}

fn marker_path() -> PathBuf {
    host().wake_lab_marker_path()
}

fn load_config() -> WakeLabConfig {
    let profile = host().wake_lab_profile();
    std::fs::read(host().wake_lab_config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| default_config(&profile))
}

fn save_config(config: &WakeLabConfig) -> Result<(), String> {
    let profile = host().wake_lab_profile();
    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|error| format!("{}: {error}", profile.config_serialize_failed))?;
    host()
        .replace_file_atomically(&host().wake_lab_config_path(), &bytes)
        .map_err(|error| format!("{}: {error}", profile.config_write_failed))
}

/// The panel's status: whether the marker exists, whether the listener is on (only with the marker),
/// and the threshold clamped to the profile's bounds.
pub fn status() -> WakeLabStatus {
    let profile = host().wake_lab_profile();
    let marker_present = marker_path().is_file();
    let config = load_config();
    WakeLabStatus {
        marker_present,
        enabled: marker_present && config.enabled,
        threshold: clamp_threshold(&profile, config.threshold),
    }
}

/// Start or stop the listener on the engine, under the runtime lock. The lock is released before
/// the event consumer is started.
fn listen_on_runtime(
    profile: &WakeLabProfile,
    enabled: bool,
    threshold: f32,
) -> Result<(), String> {
    with_backend_transition(|| {
        let mut runtime = runtime_lock()?;
        if enabled {
            runtime.start_wake_listen(WakeListenParams {
                model: Some(profile.model.into()),
                threshold: Some(threshold),
                ..Default::default()
            })
        } else {
            runtime.stop_wake_listen()
        }
    })
}

/// Start or stop the listener. Starting needs the marker and consumes the saved threshold; a
/// successful start makes sure the engine event consumer is running.
pub fn set_listen_enabled(shell: &Shell, enabled: bool) -> Result<(), String> {
    let _mutation = MUTATION.lock().map_err(|error| error.to_string())?;
    set_listen_enabled_inner(shell, enabled)
}

fn set_listen_enabled_inner(shell: &Shell, enabled: bool) -> Result<(), String> {
    let profile = host().wake_lab_profile();
    if enabled && !marker_path().is_file() {
        return Err(profile.marker_absent.into());
    }
    let threshold = clamp_threshold(&profile, load_config().threshold);
    let result = listen_on_runtime(&profile, enabled, threshold);
    if result.is_ok() && enabled {
        start_engine_event_consumer(shell.clone());
    }
    result
}

/// Turn the harness on or off. Turning it on creates the marker first; any later failure rolls the
/// marker and the listener back.
pub fn set_enabled(shell: &Shell, enabled: bool) -> Result<(), String> {
    let _mutation = MUTATION.lock().map_err(|error| error.to_string())?;
    let profile = host().wake_lab_profile();
    let marker = marker_path();
    let marker_present = marker.is_file();
    let created_marker = enabled && !marker_present;
    if created_marker {
        host()
            .replace_file_atomically(&marker, b"")
            .map_err(|error| format!("{}: {error}", profile.enable_access_failed))?;
    }
    let previous = load_config();
    let mut config = previous.clone();
    config.enabled = enabled;
    if let Err(error) = set_listen_enabled_inner(shell, enabled) {
        // A failed/timed-out request may still have reached the engine; restore prior intent.
        if let Err(rollback) = listen_on_runtime(
            &profile,
            previous.enabled && marker_present,
            clamp_threshold(&profile, previous.threshold),
        ) {
            telemetry::log_message("wake_lab_rollback_failed", &rollback);
        }
        if created_marker {
            let _ = std::fs::remove_file(&marker);
        }
        telemetry::log_message("wake_lab_state_failed", &error);
        return Err(error);
    }
    if let Err(error) = save_config(&config) {
        if let Err(rollback) = set_listen_enabled_inner(shell, previous.enabled && marker_present) {
            telemetry::log_message("wake_lab_rollback_failed", &rollback);
        }
        if created_marker {
            let _ = std::fs::remove_file(&marker);
        }
        telemetry::log_message("wake_lab_state_failed", &error);
        return Err(error);
    }
    telemetry::log_message(
        "wake_lab_state",
        if enabled { "enabled" } else { "disabled" },
    );
    Ok(())
}

/// Save a new threshold. When the listener is on, it is restarted with the new value; a failed
/// restart puts the previous threshold back and restarts the listener with it.
pub fn set_threshold(shell: &Shell, threshold: f32) -> Result<(), String> {
    let _mutation = MUTATION.lock().map_err(|error| error.to_string())?;
    let profile = host().wake_lab_profile();
    if !marker_path().is_file() {
        return Err(profile.marker_absent.into());
    }
    let previous = load_config();
    let mut config = previous.clone();
    config.threshold = clamp_threshold(&profile, threshold);
    save_config(&config)?;
    if config.enabled {
        let restart = set_listen_enabled_inner(shell, false)
            .and_then(|()| set_listen_enabled_inner(shell, true));
        if let Err(error) = restart {
            let _ = save_config(&previous);
            let _ = set_listen_enabled_inner(shell, true);
            return Err(error);
        }
    }
    Ok(())
}

/// Start the listener at launch when the owner left it on. Runs as a background job, so the
/// caller's runtime lock is not held while it starts.
pub fn start_configured(shell: Shell) {
    schedule_configured_listener(Some(shell));
}

/// Restore the configured listener when the resident sidecar was restarted. The app's engine
/// event receiver survives a sidecar restart.
pub(crate) fn restore_configured_listener() {
    schedule_configured_listener(None);
}

fn schedule_configured_listener(shell: Option<Shell>) {
    let config = load_config();
    if !marker_path().is_file() || !config.enabled {
        return;
    }
    host().spawn_blocking(Box::new(move || {
        let _mutation = match MUTATION.lock() {
            Ok(guard) => guard,
            Err(error) => {
                telemetry::log_message("wake_lab_autostart_error", &error.to_string());
                return;
            }
        };
        // A queued launch job must not undo a disable completed while it waited.
        let config = load_config();
        if !marker_path().is_file() || !config.enabled {
            return;
        }
        let profile = host().wake_lab_profile();
        match listen_on_runtime(&profile, true, clamp_threshold(&profile, config.threshold)) {
            Ok(()) => {
                if let Some(shell) = shell {
                    start_engine_event_consumer(shell);
                }
            }
            Err(error) => telemetry::log_message("wake_lab_autostart_error", &error),
        }
    }));
}
