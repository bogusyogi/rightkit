//! The dictation commands of a shell as plain functions: start, stop, cancel, toggle, repaste,
//! terminal-error recovery, and the history commands over the runtime.
//!
//! A shell keeps its `#[tauri::command]` wrappers (the command names and the `generate_handler!`
//! registration stay in the application) and calls these. Functions that need the application
//! handle take the shell's [`DictationShell`] object; the rest use the installed
//! [`crate::host::DictationHost`].

use rightkit_transcript_library::delivery::{DeliveryOutcome, DeliveryRecord, TargetSnapshot};
use rightkit_transcript_library::export::{format_history, HistoryExportFormat};
use rightkit_transcript_library::history::{
    HistoryFilter, HistoryPage, HistoryQuery, HistoryRetention, MAX_HISTORY_PAGE_SIZE,
};

use crate::host::host;
use crate::hotkey_wiring::emit_onboarding_shortcut;
use crate::lifecycle::{
    emit_delivery_recorded, emit_delivery_recorded_from_id, emit_observed_states, emit_pill_state,
    emit_runtime_state, emit_state, observe_state, send_prepared_stop,
};
use crate::runtime::{self, CancelSource};
use crate::runtime_lock::{runtime_lock, with_backend_transition};
use crate::shell::{DictationShell, PillCue, Shell};
use crate::start_sequence::{
    await_capture_live, capture_liveness, run_start_sequence, StartSteps, CAPTURE_LIVE_POLL,
    CAPTURE_LIVE_WINDOW,
};
use crate::state::AppState;
use crate::telemetry;

fn show_error_pill(message: &str) {
    let host = host();
    if host.onboarding_surfaces_enabled() {
        host.show_pill_cue(PillCue::Error {
            message: message.to_string(),
        });
    }
}

/// Onboarding gates background shell surfaces such as the resting pill, tray hotkeys and the mini
/// clipboard. The onboarding UI itself may still invoke dictation commands explicitly for its
/// voice-test step.
pub fn require_onboarded() -> Result<(), String> {
    if host().onboarding_complete() {
        Ok(())
    } else {
        Err("onboarding is not complete".to_string())
    }
}

/// Stop the recording started from a hotkey, the tray or the app.
pub fn stop_dictation_inner(shell: &dyn DictationShell) -> Result<AppState, String> {
    telemetry::utterance_timeline::begin_stop("stop");
    let mut observed = Vec::new();
    let result = with_backend_transition(|| {
        let prepared = {
            let mut runtime = runtime_lock()?;
            runtime.prepare_stop_dictation(|state| observe_state(&mut observed, state))
        }?;
        send_prepared_stop(prepared, &mut observed)
    });
    emit_observed_states(shell, &observed);
    telemetry::utterance_timeline::mark(|t| &mut t.pill_processing_ms);
    let state = result?;
    emit_delivery_recorded(shell, &state);
    Ok(state)
}

/// Stop the recording from the pill's own buttons; `send_enter` also presses Enter after the text.
pub fn stop_dictation_from_pill_inner(
    shell: &dyn DictationShell,
    send_enter: bool,
) -> Result<AppState, String> {
    telemetry::utterance_timeline::begin_stop(if send_enter {
        "pill_stop_send"
    } else {
        "pill_stop"
    });
    let overlay_handles = host().pill_overlay_handles();
    let mut observed = Vec::new();
    let result = with_backend_transition(|| {
        let prepared = {
            let mut runtime = runtime_lock()?;
            runtime.prepare_stop_from_pill(send_enter, overlay_handles, |state| {
                observe_state(&mut observed, state)
            })
        }?;
        send_prepared_stop(prepared, &mut observed)
    });
    emit_observed_states(shell, &observed);
    telemetry::utterance_timeline::mark(|t| &mut t.pill_processing_ms);
    let state = result?;
    emit_delivery_recorded(shell, &state);
    Ok(state)
}

/// Copy text to the clipboard on the user's explicit request.
pub fn copy_text_to_clipboard_inner(text: String) -> Result<(), String> {
    telemetry::log_clipboard_copy(text.chars().count());
    host().copy_text_to_clipboard(&text)
}

/// Terminal-pill expiry only. `generation: None` is the native pill (its timer has no token): a
/// state check only. This must never cancel a newly started recording after an older Error
/// visual's timer fires.
pub fn recover_terminal_error_inner(
    shell: &dyn DictationShell,
    generation: Option<u64>,
) -> Result<AppState, String> {
    let state = runtime_lock()?.recover_terminal_error(generation)?;
    emit_runtime_state(shell, &state);
    Ok(state)
}

/// Cancel (any source) FINALIZES the recording and saves it to encrypted History marked
/// `Cancelled`, storing only the raw transcript: no clipboard write, no paste, no Enter, no
/// delivery. Uniform for every `CancelSource`: no per-source behaviour branch; `source` is used only
/// for the telemetry string. Mirrors [`stop_dictation_inner`] exactly: it reuses the stop path
/// (`prepare_cancel_to_history` builds a `PreparedStop` the same way `prepare_stop_dictation` does;
/// `send_prepared_stop` sends it) so the runtime lock is never held across the backend send or UI
/// emission.
pub fn cancel_dictation_inner(
    shell: &dyn DictationShell,
    source: CancelSource,
) -> Result<AppState, String> {
    if host().onboarding_shortcut_test_hotkeys_enabled() {
        emit_onboarding_shortcut(shell, "cancel");
        return Ok(AppState::Armed);
    }
    emit_onboarding_shortcut(shell, "cancel");
    telemetry::log_message(
        "cancel_to_history_lifecycle",
        &format!("phase=requested source={}", source.as_str()),
    );
    telemetry::utterance_timeline::begin_stop("cancel");
    let mut observed = Vec::new();
    let result = with_backend_transition(|| {
        let prepared = {
            let mut runtime = runtime_lock()?;
            runtime.prepare_cancel_to_history(|state| observe_state(&mut observed, state))
        }?;
        send_prepared_stop(prepared, &mut observed)
    });
    emit_observed_states(shell, &observed);
    let state = result?;
    emit_delivery_recorded(shell, &state);
    Ok(state)
}

/// Paste the last delivery again. Takes the runtime lock and performs the real OS paste
/// synchronously, so it must not run on a UI thread.
pub fn repaste_last_delivery_inner(shell: &dyn DictationShell) -> Result<DeliveryRecord, String> {
    require_onboarded()?;
    let (result, state, recovery) = {
        let mut runtime = runtime_lock()?;
        let previous_id = runtime.last_delivery_id();
        let result = runtime.repaste_last_delivery();
        let recovery = if result.is_err() {
            match &*runtime {
                runtime::RuntimeApp::Engine(engine) => engine
                    .last_delivery
                    .as_ref()
                    .filter(|record| {
                        previous_id.as_deref() != Some(record.delivery_id.as_str())
                            && engine
                                .recent_cache
                                .iter()
                                .any(|saved| saved.delivery_id == record.delivery_id)
                    })
                    .cloned(),
            }
        } else {
            None
        };
        (result, runtime.app_state(), recovery)
    };
    if let Some(record) = recovery {
        telemetry::log_delivery(&record, None, Some(false));
        emit_delivery_recorded_from_id(shell, &record.delivery_id, Some(&record.target));
        // Explicit recovery only: no second delivery, clipboard write or Return.
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if !host().onboarding_dictation_test_active() && !record.transcript.trim().is_empty() {
            if let Err(err) = shell.show_clipboard_recovery(&record.transcript) {
                telemetry::log_message(
                    "pill_clipboard_fallback_error",
                    crate::dictation_error::error_log_code(&err),
                );
            }
        }
    }
    let record = result?;
    host().show_pill_cue(PillCue::Pasted);
    telemetry::log_delivery(&record, None, None);
    emit_state(shell, &state);
    emit_delivery_recorded_from_id(shell, &record.delivery_id, Some(&record.target));
    Ok(record)
}

/// Result of the idle pill's one-click "copy last transcript". Carries no transcript text, so it is
/// safe to log and to hand back across a bridge.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyLastOutcome {
    Copied,
    Empty,
    Failed,
}

#[cfg(target_os = "macos")]
impl CopyLastOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Copied => "copied",
            Self::Empty => "empty",
            Self::Failed => "failed",
        }
    }
}

/// Copies the newest delivered transcript to the clipboard through the engine's own copy-last path
/// and logs `pill_copy_last outcome=...` (outcome only, never the transcript).
#[cfg(target_os = "macos")]
pub fn copy_last_transcript_from_pill() -> CopyLastOutcome {
    let result = match runtime_lock() {
        Ok(mut runtime) => runtime.copy_last_delivery_to_clipboard(),
        Err(error) => Err(error),
    };
    let outcome = match result {
        Ok(_) => CopyLastOutcome::Copied,
        Err(error) if error.contains("no last delivery") => CopyLastOutcome::Empty,
        Err(error) => {
            telemetry::log_message("pill_native_copy_error", &error);
            CopyLastOutcome::Failed
        }
    };
    telemetry::log_message("pill_copy_last", &format!("outcome={}", outcome.as_str()));
    outcome
}

/// The lock-free state read of the pill's state query: it must not queue behind prewarm's model
/// load or config sync. Fake mode and pre-init fall back to the runtime lock.
pub fn get_app_state() -> Result<AppState, String> {
    if let Some(state) = runtime::app_state_lock_free() {
        return Ok(state);
    }
    Ok(runtime_lock()?.app_state())
}

/// Recent history rows. `cancelled_only` is optional for back-compat: an omitted argument means
/// `false` (cancelled and draft rows hidden), the same as `HistoryFilter::cancelled_only`'s serde
/// default.
pub fn get_recent_history(cancelled_only: Option<bool>) -> Result<Vec<DeliveryRecord>, String> {
    let cancelled_only = cancelled_only.unwrap_or(false);
    let records: Vec<DeliveryRecord> = runtime_lock()?
        .recent_history()?
        .iter()
        .filter(|record| {
            cancelled_only
                || !matches!(
                    record.outcome,
                    DeliveryOutcome::Cancelled | DeliveryOutcome::Draft
                )
        })
        .cloned()
        .collect();
    telemetry::log_message(
        "ipc_get_recent_history",
        &format!(
            "count={} first_id={:?} cancelled_only={cancelled_only}",
            records.len(),
            records.first().map(|r| r.delivery_id.clone()),
        ),
    );
    Ok(records)
}

/// One page of history. Logs unconditionally, success or failure, so a silently empty page is
/// diagnosable from the log.
pub fn query_history(query: HistoryQuery) -> Result<HistoryPage, String> {
    let has_filter = !query.filter.search.is_empty()
        || query.filter.from_ms.is_some()
        || query.filter.until_ms.is_some()
        || !query.filter.app.is_empty();
    let result = runtime_lock()?.query_history(&query);
    match &result {
        Ok(page) => telemetry::log_message(
            "ipc_query_history",
            &format!(
                "offset={} limit={} has_filter={} total={} items={}",
                query.offset,
                query.limit,
                has_filter,
                page.total,
                page.items.len(),
            ),
        ),
        Err(err) => telemetry::log_message(
            "ipc_query_history_error",
            &format!(
                "offset={} limit={} has_filter={} code={}",
                query.offset,
                query.limit,
                has_filter,
                crate::dictation_error::history_error_log_code(err)
            ),
        ),
    }
    result
}

/// Delete every history row matching `filter`.
pub fn delete_history_matching(filter: HistoryFilter) -> Result<usize, String> {
    runtime_lock()?.delete_history_matching(&filter)
}

/// Explicit user action: rotate the history encryption key on the live store.
pub fn rotate_history_key() -> Result<(), String> {
    runtime_lock()?.rotate_history_key()
}

/// Persist a new retention choice and prune what it already excludes; returns the pruned count.
pub fn set_history_retention(retention: HistoryRetention) -> Result<usize, String> {
    host().set_history_retention(retention)?;
    let Some(cutoff_ms) = retention.cutoff_ms(rightkit_transcript_library::delivery::now_ms())
    else {
        return Ok(0);
    };
    runtime_lock()?.prune_history_before(cutoff_ms)
}

/// Export all history rows to `path` (absolute, no `..`, an existing parent, a supported
/// extension); returns the row count. The wording of the export is the host's
/// [`crate::host::DictationHost::history_export_labels`].
pub fn export_history(path: String) -> Result<usize, String> {
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        return Err("export path must be absolute (use the OS save dialog)".to_string());
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("export path must not contain '..' components".to_string());
    }
    let format = HistoryExportFormat::from_path(&path)?;
    let parent = path
        .parent()
        .ok_or_else(|| "export path has no parent directory".to_string())?;
    if !parent.is_dir() {
        return Err(format!(
            "export path parent does not exist: {}",
            parent.display()
        ));
    }

    let mut runtime = runtime_lock()?;
    let mut items = Vec::new();
    let mut query = HistoryQuery {
        limit: MAX_HISTORY_PAGE_SIZE,
        ..Default::default()
    };
    loop {
        let page = runtime.query_history(&query)?;
        items.extend(page.items);
        if !page.has_more {
            break;
        }
        query.offset = u32::try_from(items.len())
            .map_err(|_| "history export exceeds supported row count".to_string())?;
    }
    let contents = format_history(&items, format, &host().history_export_labels())?;
    rightkit_fs::atomic::atomic_write(&path, contents.as_bytes())
        .map_err(|err| format!("write history export {}: {err}", path.display()))?;
    Ok(items.len())
}

/// Write recovery diagnostics (no key, no history content) to a user-chosen `.txt` path. Never
/// touches the encrypted backup.
pub fn export_history_recovery_diagnostics(path: String) -> Result<(), String> {
    let path = std::path::PathBuf::from(path);
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
    {
        return Err("diagnostics export must use a .txt file".to_string());
    }
    rightkit_fs::atomic::atomic_write(&path, host().history_recovery_diagnostics().as_bytes())
        .map_err(|err| format!("write diagnostics: {err}"))
}

/// Delete one history row.
pub fn delete_history_item(delivery_id: String) -> Result<bool, String> {
    runtime_lock()?.delete_history_item(&delivery_id)
}

/// Edit one history row. The sentinel id `"current"` is the pill popup's row: it resolves to the
/// last delivery's real id and enforces origin authority BEFORE the popup override changes, so the
/// correction also reaches History.
pub fn update_history_item_inner(delivery_id: String, transcript: String) -> Result<bool, String> {
    if delivery_id == "current" {
        let mut runtime = runtime_lock()?;
        if let Some(real_id) = runtime.last_delivery_id() {
            let changed = runtime.update_history_item(&real_id, transcript.clone())?;
            if changed {
                host().set_pill_clipboard_override(Some(transcript))?;
            }
            return Ok(changed);
        }
        return Ok(false);
    }
    runtime_lock()?.update_history_item(&delivery_id, transcript)
}

/// Delete all history rows.
pub fn clear_history() -> Result<(), String> {
    runtime_lock()?.clear_history()
}

/// Re-push entitlement-derived engine config after a licence change so Pro features take effect
/// without a restart.
pub fn refresh_entitlements() -> Result<(), String> {
    let mut runtime = runtime_lock()?;
    runtime.sync_sidecar_runtime_snapshot_if_running()
}

/// Production implementation of the start sequence. The order is fixed by `run_start_sequence`;
/// each step here is one stage of it.
struct ProductionStart {
    observed: Vec<AppState>,
    initial_target: TargetSnapshot,
    prepared: Option<runtime::PreparedStart>,
    dispatched: Option<runtime::PreparedStartResult>,
}

impl StartSteps for ProductionStart {
    fn observe_target(&mut self) {
        self.initial_target = host().observe_start_target();
    }

    fn allocate_interaction(&mut self) -> Result<(), String> {
        let mut runtime = runtime_lock()?;
        let observed = &mut self.observed;
        let prepared = runtime.prepare_start_dictation(self.initial_target.clone(), |state| {
            observe_state(observed, state)
        })?;
        self.prepared = Some(prepared);
        Ok(())
    }

    fn show_optimistic_recording(&mut self) {
        // Visual only: Recording look now, no spinner. The Recording STATE is committed by
        // `acknowledge_recording`; rejection falls to the Error visual in `start_dictation_inner`.
        let host = host();
        if host.onboarding_surfaces_enabled() {
            host.show_pill_cue(PillCue::Recording);
        }
    }

    fn backend_start(&mut self) -> Result<(), String> {
        let prepared = self.prepared.take().ok_or("start not allocated")?;
        match prepared.send() {
            Ok(dispatched) => {
                self.dispatched = Some(dispatched);
                Ok(())
            }
            Err(error) => {
                if let Ok(mut runtime) = runtime_lock() {
                    let observed = &mut self.observed;
                    runtime.fail_prepared_start(
                        &error.session_id,
                        error.message.clone(),
                        |state| observe_state(observed, state),
                    );
                }
                Err(error.message)
            }
        }
    }

    fn confirm_capture_live(&mut self) -> Result<(), String> {
        // The sidecar acknowledges StartRecording before the worker opens or resumes capture, so
        // acceptance alone does not prove a live mic. Require first frames inside a bounded window.
        let Some(runtime::PreparedStartResult::Engine { session_id }) = self.dispatched.as_ref()
        else {
            return Ok(());
        };
        let session_id = session_id.clone();
        let Err(message) = await_capture_live(
            || capture_liveness(&session_id),
            std::thread::sleep,
            CAPTURE_LIVE_WINDOW,
            CAPTURE_LIVE_POLL,
        ) else {
            return Ok(());
        };
        telemetry::log_message(
            "start_capture_not_live",
            &format!(
                "session={session_id} code={}",
                crate::dictation_error::error_log_code(&message)
            ),
        );
        self.dispatched = None;
        // Stop any capture the worker left running, then close the session. Cancel clears the
        // active session, so the start-failure telemetry that `fail_prepared_start` would emit is
        // recorded here.
        let cancel =
            runtime_lock().map(|mut runtime| runtime.prepare_cancel(CancelSource::Watchdog));
        if let Ok(prepared) = cancel {
            let _ = prepared.send();
        }
        Err(message)
    }

    fn acknowledge_recording(&mut self) -> Result<AppState, String> {
        let dispatched = self.dispatched.take().ok_or("start not dispatched")?;
        let mut runtime = runtime_lock()?;
        let observed = &mut self.observed;
        runtime.finalize_prepared_start(dispatched, |state| observe_state(observed, state))
    }
}

/// Start a recording: the voice-controls check, the engine consumer, focus coverage, the start
/// sequence, then state and pill emission.
pub fn start_dictation_inner(shell: &Shell) -> Result<AppState, String> {
    host().require_voice_controls()?;
    // A process resumed mid-onboarding has not activated the shared model (the boot that does it
    // waits for onboarding to finish), and the onboarding shortcut test is its first engine spawn.
    // Activate before the transition gate and the runtime lock are taken: the activation takes the
    // runtime lock itself and is a no-op once this process has verified the copy.
    if !host().onboarding_complete() {
        host().ensure_shared_model_active()?;
    }
    telemetry::log_message("start_dictation_invoked", "phase=requested");
    let mut steps = ProductionStart {
        observed: Vec::new(),
        initial_target: TargetSnapshot::empty(),
        prepared: None,
        dispatched: None,
    };
    // The consumer delivers the first-frames signal the start now waits on, so it must run before
    // the wait (idempotent).
    crate::engine_events::start_engine_event_consumer(shell.clone());
    // macOS focus continuity is on demand: arm it BEFORE the hotkey anchor is read (the first start
    // step), keep it until a terminal state ends it.
    #[cfg(target_os = "macos")]
    host().focus_coverage_start_begin();
    let result = run_start_sequence(&mut steps, |op| with_backend_transition(op));
    #[cfg(target_os = "macos")]
    host().focus_coverage_start_finish(result.as_ref().ok());
    let observed = std::mem::take(&mut steps.observed);
    emit_observed_states(shell.as_ref(), &observed);
    match result {
        // `finalize_prepared_start` produces Recording only after it confirms the prepared session
        // remains active. A terminal async sidecar event (for example a capture-open failure
        // delivered by the engine consumer on its own thread) can supersede that snapshot before
        // this emission is serialized; `emit_pill_state` emits only while the state is still
        // authoritative, so a stale Recording can never land after the terminal state that
        // replaced it.
        Ok(state @ AppState::Recording { .. }) => {
            emit_pill_state(&state);
            crate::engine_events::start_engine_event_consumer(shell.clone());
            Ok(state)
        }
        Ok(state) => {
            emit_pill_state(&state);
            Ok(state)
        }
        Err(error) => {
            #[cfg(target_os = "windows")]
            shell.note_start_error(&error);
            show_error_pill(&error);
            Err(error)
        }
    }
}

/// Stop when recording, start otherwise. Every failure path reverts the pill out of Recording and
/// shows an error instead of silently propagating with a stale recording pill on screen.
pub fn toggle_dictation_inner(shell: &Shell) -> Result<AppState, String> {
    let stopping_active_recording =
        matches!(runtime_lock()?.app_state(), AppState::Recording { .. });
    let result = if stopping_active_recording {
        stop_dictation_inner(shell.as_ref())
    } else {
        start_dictation_inner(shell)
    };
    match result {
        Ok(state) => Ok(state),
        Err(err) => {
            show_error_pill(&err);
            Err(err)
        }
    }
}

/// Stop and press Enter once the paste lands. Async stop: the worker transcribes off-thread; the
/// Enter keystroke is sent by finalize once the paste lands. This call only transitions to
/// `transcribing` and returns; it never blocks on the transcribe. A failure also reverts the pill
/// instead of leaving stale feedback.
pub fn stop_dictation_and_send_inner(shell: &dyn DictationShell) -> Result<AppState, String> {
    telemetry::utterance_timeline::begin_stop("stop_send");
    let mut observed = Vec::new();
    let result = with_backend_transition(|| {
        let prepared = {
            let mut runtime = runtime_lock()?;
            runtime.prepare_stop_and_send(|state| observe_state(&mut observed, state))
        }?;
        send_prepared_stop(prepared, &mut observed)
    });
    emit_observed_states(shell, &observed);
    telemetry::utterance_timeline::mark(|t| &mut t.pill_processing_ms);
    match result {
        Ok(state) => {
            emit_delivery_recorded(shell, &state);
            Ok(state)
        }
        Err(err) => {
            show_error_pill(&err);
            Err(err)
        }
    }
}
