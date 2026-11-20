//! Shared-model activation and model install orchestration. Moved from HeardRight
//! `commands/models.rs` (`prepare_shared_tdt_for_download`, `download_shared_tdt`,
//! `download_local_asr_model_blocking`, `tdt_post_prepare_decision`, `expected_sha`,
//! `model_download_platform`).
//!
//! No model id, route, label, worker URL, sentence or app id is fixed here: the host supplies
//! them in [`ModelInstall`] and [`InstallText`]; the engine, the runtime lock and the settings
//! switch are reached through [`ActivationHost`] / [`InstallHost`].

use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::model_flow::{self, ArchiveModel, FlowText, SharedModel};
use crate::{shared, ultra_model, whisper_model};

/// Host event sink: `(event, detail)`.
pub type EventSink = fn(&str, &str);

/// Renderer progress: wire phase name, bytes received, total bytes (0 when unknown), message.
pub type EmitProgress<'a> = &'a dyn Fn(&str, u64, u64, &str);

/// The platform tag a model download grant carries.
pub fn download_platform() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "windows"
    }
}

/// The compiled-in archive checksum when it is a real SHA-256, else `None`.
pub fn expected_sha(pin: Option<&str>) -> Option<String> {
    pin.map(str::to_string)
        .filter(|s| ultra_model::is_sha256_hex(s.trim()))
}

/// The decision after a shared-model prepare. `Ok(None)`: the model is ready and the prepare
/// succeeded. `Ok(Some(error))`: the model is ready although the prepare failed with `error` (the
/// host logs it; the engine continues on the recorded copy). `Err(message)`: no model to load;
/// `message` is the prepare error, or `not_installed_message` when the prepare reported success.
pub fn post_prepare_decision(
    model_ready: bool,
    prepared: Result<bool, String>,
    not_installed_message: &str,
) -> Result<Option<String>, String> {
    if model_ready {
        return Ok(prepared.err());
    }
    Err(match prepared {
        Err(error) => error,
        Ok(_) => not_installed_message.to_string(),
    })
}

/// The idle resident engine held under the host's runtime lock for the duration of an activation.
pub trait EngineGuard {
    /// Release every resident backend session.
    fn shutdown_resident(&mut self);
    /// Load the model the engine will run.
    fn prewarm(&mut self) -> Result<(), String>;
}

/// What activation needs from the app.
pub trait ActivationHost {
    /// True when the engine launches on the shared model's backend for this entitlement.
    fn launch_backend_is_shared(&self) -> bool;
    /// Take the runtime lock and return the engine: `Ok(None)` when no engine supervisor exists,
    /// `Err(message)` when a dictation is in progress (the host's wording).
    fn acquire_idle_engine(&self) -> Result<Option<Box<dyn EngineGuard + '_>>, String>;
    /// Persist the switch off the retired backend, called only after the replacement loaded.
    fn finalize_retired_switch(&self) -> Result<(), String>;
    /// True when the model the engine would run now resolves on disk.
    fn fallback_model_present(&self) -> bool;
    /// Reclaim the app's superseded model directories (called after a successful activation).
    fn reclaim_superseded(&self);
}

/// Activate the shared model under the host's idle guard. `Ok(true)` when it was activated or
/// already active, `Ok(false)` when there was nothing to do (not the shared backend, or no engine).
pub fn prepare_shared_model(
    host: &dyn ActivationHost,
    replace_retired: bool,
    sink: EventSink,
) -> Result<bool, String> {
    if !host.launch_backend_is_shared() && !replace_retired {
        return Ok(false);
    }
    let Some(mut engine) = host.acquire_idle_engine()? else {
        return Ok(false);
    };
    engine.shutdown_resident();
    let engine = RefCell::new(engine);
    let started = Instant::now();
    let mut result = shared::prepare(&|dir| {
        shared::with_load_check(dir, || {
            let mut engine = engine.borrow_mut();
            let result = engine.prewarm();
            // Release all path-keyed sessions before store retirement/rollback.
            engine.shutdown_resident();
            result
        })
    });
    // Persist the retired backend switch only after the pinned replacement
    // loads successfully, while the same idle guard still owns activation.
    if matches!(&result, Ok(Some(_))) && replace_retired {
        if let Err(error) = host.finalize_retired_switch() {
            shared::deactivate();
            result = Err(error);
        }
    }
    // A stable tag only: the success value is the model directory, a path under the profile.
    let outcome = match &result {
        Ok(Some(_)) => "activated",
        Ok(None) => "unchanged",
        Err(_) => "error",
    };
    sink(
        "shared_model_activation",
        &format!(
            "elapsed_ms={} result={outcome}",
            started.elapsed().as_millis()
        ),
    );
    // Successful selection or rollback both restart through normal resolution.
    if host.fallback_model_present() {
        if let Err(error) = engine.borrow_mut().prewarm() {
            if result.is_ok() {
                return Err(error);
            }
            sink("shared_model_fallback_load_failed", &error);
        }
    }
    // Superseded models are reclaimed only now: the replacement was activated by the store
    // (pinned entry, native load check) AND just loaded as the resident model above. Any
    // failure returned or logged before this point leaves them.
    if matches!(&result, Ok(Some(_))) {
        host.reclaim_superseded();
        // Shared-store copies: references released now; the prune itself runs at the next
        // launch before the engine takes its runtime lease.
        if let Err(error) = shared::reclaim_retired_store_entries() {
            sink("retired_store_entries_reclaim_failed", &error);
        }
    }
    result.map(|dir| dir.is_some())
}

/// [`ActivationHost`] plus the dictation-idle wait a shared-store install needs.
pub trait InstallHost: ActivationHost {
    fn dictation_idle(&self) -> bool;
    /// Block until idle or `budget` elapses; true when idle.
    fn wait_for_dictation_idle(&self, budget: Duration) -> bool;
}

/// One model to install. Every field is a host value.
#[derive(Clone, Copy, Debug)]
pub struct ModelInstall<'a> {
    /// Backend name: the worker grant's model and the name progress events carry.
    pub backend: &'a str,
    /// True for the shared-store copy (activated through [`prepare_shared_model`]), false for an
    /// archive install.
    pub shared_store: bool,
    pub route: &'a str,
    pub archive_top_level: &'a str,
    pub install_subdir: &'a str,
    /// Compiled-in archive checksum (archive installs).
    pub sha_pin: Option<&'a str>,
    /// Owner id recorded in the shared store's refs (shared installs).
    pub app_id: &'a str,
    pub worker_base: &'a str,
    pub device_id: &'a str,
    pub timeout: Duration,
    pub progress_step_bytes: u64,
    pub max_archive_bytes: u64,
    pub max_installed_bytes: u64,
    /// Longest wait for the per-model operation gate.
    pub operation_gate_wait: Duration,
    /// Longest wait for a dictation to finish before a shared install.
    pub idle_wait: Duration,
}

/// Host-owned sentences of the install flow.
#[derive(Clone, Copy, Debug)]
pub struct InstallText<'a> {
    pub shared: FlowText<'a>,
    pub archive: FlowText<'a>,
    /// Progress message while waiting for a dictation to finish.
    pub waiting_for_dictation: &'a str,
    /// Error when the dictation did not finish in time.
    pub dictation_still_running: &'a str,
    /// Error when the shared copy was fetched but activation was deferred.
    pub activation_deferred: &'a str,
    /// Final progress message of a shared install.
    pub ready: &'a str,
}

/// Install `model`. The caller builds its own status afterwards.
pub fn install_model(
    host: &dyn InstallHost,
    model: &ModelInstall<'_>,
    text: &InstallText<'_>,
    emit: EmitProgress<'_>,
    sink: EventSink,
) -> Result<(), String> {
    let operation_lock = whisper_model::model_operation_lock(model.install_subdir);
    // Bounded wait for the per-model operation gate: another download or preparation of this
    // model may hold it for a long time, but never without limit.
    let _operation_guard =
        whisper_model::lock_model_operation_within(&operation_lock, model.operation_gate_wait)
            .map_err(|_| "model_operation_busy".to_string())?;
    if model.shared_store {
        // Never fail because the user is mid-dictation: wait for it to finish.
        if !host.dictation_idle() {
            emit("waiting", 0, 0, text.waiting_for_dictation);
            sink("model_download", "waiting_for_idle");
            if !host.wait_for_dictation_idle(model.idle_wait) {
                return Err(text.dictation_still_running.to_string());
            }
        }
        return download_shared(host, model, text, emit, sink);
    }
    let expected_sha = expected_sha(model.sha_pin);
    model_flow::ensure_archive_model(
        &ArchiveModel {
            grant_model: model.backend,
            route: model.route,
            archive_top_level: model.archive_top_level,
            install_subdir: model.install_subdir,
            sha256: expected_sha.as_deref(),
            worker_base: model.worker_base,
            platform: download_platform(),
            device_id: model.device_id,
            timeout: model.timeout,
            progress_step_bytes: model.progress_step_bytes,
            max_archive_bytes: model.max_archive_bytes,
            max_installed_bytes: model.max_installed_bytes,
        },
        &text.archive,
        &|phase, received, total, message| emit(phase.wire(), received, total, message),
    )
}

fn download_shared(
    host: &dyn ActivationHost,
    model: &ModelInstall<'_>,
    text: &InstallText<'_>,
    emit: EmitProgress<'_>,
    sink: EventSink,
) -> Result<(), String> {
    if prepare_shared_model(host, true, sink)? {
        return Ok(());
    }
    model_flow::fetch_shared_speech_model(
        &SharedModel {
            app_id: model.app_id,
            grant_model: model.backend,
            route: model.route,
            worker_base: model.worker_base,
            platform: download_platform(),
            device_id: model.device_id,
            timeout: model.timeout,
        },
        &text.shared,
        &|phase, received, total, message| emit(phase.wire(), received, total, message),
    )?;
    if !prepare_shared_model(host, true, sink)? {
        return Err(text.activation_deferred.to_string());
    }
    emit("installed", 1, 1, text.ready);
    Ok(())
}

/// The runtime lock held continuously across resident shutdown, cache clear & restart.
pub trait EngineLock {
    fn lock(&self) -> Result<Box<dyn LockedEngine + '_>, String>;
}

/// The engine under the lock: it reports whether a dictation is in progress.
pub trait LockedEngine: EngineGuard {
    fn dictation_active(&self) -> bool;
}

/// Drop every resident backend under the idle guard, run `clear` (a cache or model directory
/// reset), then restart the engine. Moved from HeardRight `commands/compute.rs`
/// (`recompile_coreml_cache`). Keep one idle runtime guard throughout shutdown, clear & restart;
/// `clear` must not reacquire that runtime lock. `busy_message` is returned when a dictation is in progress;
/// `restart_failed` words a failed restart (the `clear` result is discarded in that case, as
/// before).
pub fn rebuild_resident<T>(
    host: &dyn EngineLock,
    busy_message: &str,
    clear: impl FnOnce() -> Result<T, String>,
    restart_failed: &dyn Fn(&str) -> String,
) -> Result<T, String> {
    let mut engine = host.lock()?;
    if engine.dictation_active() {
        return Err(busy_message.to_string());
    }
    engine.shutdown_resident();
    let cleared = clear();
    if let Err(error) = engine.prewarm() {
        return Err(restart_failed(&error));
    }
    cleared
}

/// Prune the shared-store models the current one superseded while no engine holds the runtime
/// lease (the store refuses to prune under any live lease). A no-op unless the current model is
/// Ready. Logs the count only (the entries are paths under the user's profile). Moved from
/// HeardRight `app_run.rs` (`reclaim_retired_shared_models`).
pub fn reclaim_retired_store_logged(sink: EventSink) {
    match shared::reclaim_retired_store_entries() {
        Ok(report) if !report.removed.is_empty() => {
            sink(
                "retired_store_entries_reclaimed",
                &format!("count={}", report.removed.len()),
            );
        }
        Ok(_) => {}
        Err(error) => sink("retired_store_entries_reclaim_failed", &error),
    }
}
