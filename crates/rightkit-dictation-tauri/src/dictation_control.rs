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
use std::sync::Arc;

use crate::runtime::{self, CancelSource};
use crate::runtime_lock::{runtime_lock, with_backend_transition};
use crate::shell::{DictationShell, PillCue, Shell};
use crate::start_sequence::{
    await_capture_live, capture_liveness, run_start_sequence, CaptureLiveness, StartSteps,
    CAPTURE_LIVE_POLL, CAPTURE_LIVE_WINDOW,
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
    stop_dictation_inner_at(shell, runtime::OriginalReleaseMark::for_current_action())
}

/// Stop with a marker sampled at the native release before asynchronous dispatch.
pub fn stop_dictation_inner_at(
    shell: &dyn DictationShell,
    released: runtime::OriginalReleaseMark,
) -> Result<AppState, String> {
    // The release is the end of the audio, however late the rest of this stop runs: seal the
    // press's recording first, synchronously (bounded), before the gate, the start or a key
    // delivery can delay anything. The key-gated stop below is sent only after the engine
    // acknowledged the seal (`runtime::seal_capture_for_stop`).
    if !runtime::seal_capture_for_stop_at(released) {
        return Err("Capture release could not be sealed; recording discarded.".into());
    }
    // A stop that reaches a press whose start has not committed a session yet is that press's
    // STOP intent, not a cancel: the start completes (adopting the warm-up audio), commits
    // `Recording` and then performs this stop itself, so a quick press-and-release is finalised
    // and delivered. Only a cancel discards.
    if runtime::defer_stop_to_pending_start(runtime::DeferredStop::Stop) {
        return get_app_state();
    }
    stop_dictation_for_session(shell, None)
}

/// `stop_dictation_inner` for a caller that must return promptly: the Windows push-to-talk release
/// callback. Its dispatch thread is serial, and a key-gated stop (the transition gate, a provider
/// key delivery in flight) held it back from the next press, whose warm-up and start are then
/// late. On the calling thread this does only what must not wait: the synchronous capture seal
/// (bounded by `CAPTURE_SEAL_WAIT`), then either records the stop as the pending start's intent
/// (the start performs it for its own session, see `run_reserved_start`) or binds the active
/// session; the key-gated stop itself runs on a `dictation-stop` thread, for that session only.
pub fn stop_dictation_detached(shell: &Shell) -> Result<(), String> {
    stop_dictation_detached_at(shell, runtime::OriginalReleaseMark::legacy_receipt())
}

/// `stop_dictation_and_send_inner` for the same caller (the push-to-talk send chord): see
/// [`stop_dictation_detached`].
pub fn send_dictation_detached(shell: &Shell) -> Result<(), String> {
    send_dictation_detached_at(shell, runtime::OriginalReleaseMark::legacy_receipt())
}

/// Queue a stop while retaining the key hook's original release marker.
pub fn stop_dictation_detached_at(
    shell: &Shell,
    released: runtime::OriginalReleaseMark,
) -> Result<(), String> {
    detached_stop(shell, runtime::DeferredStop::Stop, released)
}

pub fn send_dictation_detached_at(
    shell: &Shell,
    released: runtime::OriginalReleaseMark,
) -> Result<(), String> {
    detached_stop(shell, runtime::DeferredStop::StopAndSend, released)
}

fn detached_stop(
    shell: &Shell,
    kind: runtime::DeferredStop,
    released: runtime::OriginalReleaseMark,
) -> Result<(), String> {
    if !runtime::seal_capture_for_stop_at(released) {
        return Err("Capture release could not be sealed; recording discarded.".into());
    }
    if runtime::defer_stop_to_pending_start(kind) {
        return Ok(());
    }
    // Bind the session this release belongs to before anything else can change it: the next press
    // may start a new recording while the stop below is still waiting on the gate or on keys, and
    // the stop must end the recording that was released, not that one.
    let Some(session_id) = runtime::active_session_lock_free() else {
        telemetry::log_message("stop_detached", "phase=no_active_session");
        return Ok(());
    };
    let shell = shell.clone();
    std::thread::Builder::new()
        .name("dictation-stop".to_string())
        .spawn(move || {
            if let Err(error) = run_deferred_stop(shell.as_ref(), kind, &session_id) {
                telemetry::log_message(
                    "stop_detached_error",
                    &format!("code={}", crate::dictation_error::error_log_code(&error)),
                );
            }
        })
        .map(|_| ())
        .map_err(|error| format!("stop thread: {error}"))
}

/// Whether `expected` (the session a deferred stop was bound to) is the runtime's active session.
/// `None` is an ordinary stop: it acts on whatever is recording.
fn expected_session_is_active(runtime: &runtime::RuntimeApp, expected: Option<&str>) -> bool {
    expected.is_none_or(|id| runtime.active_session_id().as_deref() == Some(id))
}

/// The stop body of `stop_dictation_inner`. With `expected` it is a deferred stop and acts only
/// when that exact session is still the active recording, checked under the backend transition
/// gate and the runtime lock that the stop itself then uses; otherwise it changes nothing.
fn stop_dictation_for_session(
    shell: &dyn DictationShell,
    expected: Option<&str>,
) -> Result<AppState, String> {
    telemetry::utterance_timeline::begin_stop("stop");
    let mut observed = Vec::new();
    let result = with_backend_transition(|| {
        let prepared = {
            let mut runtime = runtime_lock()?;
            if expected_session_is_active(&runtime, expected) {
                runtime.prepare_stop_dictation(|state| observe_state(&mut observed, state))
            } else {
                telemetry::log_message("start_stop_deferred", "phase=session_mismatch_skipped");
                Ok(runtime::PreparedStop::without_command(runtime.app_state()))
            }
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
    // Release seal and stop intent of a pending start (see `stop_dictation_inner`).
    if !runtime::seal_capture_for_stop() {
        return Err("Capture release could not be sealed; recording discarded.".into());
    }
    if runtime::defer_stop_to_pending_start(runtime::DeferredStop::PillStop { send_enter }) {
        return get_app_state();
    }
    stop_dictation_from_pill_for_session(shell, send_enter, None)
}

/// The stop body of `stop_dictation_from_pill_inner` (see `stop_dictation_for_session`).
fn stop_dictation_from_pill_for_session(
    shell: &dyn DictationShell,
    send_enter: bool,
    expected: Option<&str>,
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
            if expected_session_is_active(&runtime, expected) {
                runtime.prepare_stop_from_pill(send_enter, overlay_handles, |state| {
                    observe_state(&mut observed, state)
                })
            } else {
                telemetry::log_message("start_stop_deferred", "phase=session_mismatch_skipped");
                Ok(runtime::PreparedStop::without_command(runtime.app_state()))
            }
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
    // A CANCEL before the press's start committed a session has no session to cancel: it marks the
    // pending start cancelled (discarding any stop that was waiting on it) and revokes its warm-up
    // before it queues on the gate, so the start unwinds instead of resuming the microphone after
    // the cancel. Only a cancel discards.
    runtime::cancel_pending_start();
    // Same capture seal as an ordinary Stop, before any gate or provider-key wait: audio after the
    // cancel never enters the clip that is saved to History.
    if !runtime::seal_capture_for_stop() {
        return Err("Capture release could not be sealed; recording discarded.".into());
    }
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

/// Cancel, by session and attempt, a start whose RPC ended in an error or a timeout and may
/// therefore already be recording in the engine. A no-op when the session is no longer the shell's
/// active one or no engine is running; an engine that is running but does not accept the cancel is
/// shut down (`PreparedEngineCancel::send_possibly_started`).
fn cancel_possibly_started(session_id: &str) {
    let prepared = runtime_lock()
        .ok()
        .and_then(|runtime| runtime.prepare_possibly_started_cancel(session_id));
    if let Some(prepared) = prepared {
        prepared.send_possibly_started();
    }
}

/// The error a start unwinds with when its press was cancelled (see `runtime::cancel_pending_start`).
/// It never reaches the user: `start_dictation_after_press` turns any failure of a cancelled press
/// into the current state, not an error pill.
const START_CANCELLED: &str = "start cancelled before recording";

/// Production implementation of the start sequence. The order is fixed by `run_start_sequence`;
/// each step here is one stage of it.
///
/// The press's pending token (`token`) is checked at every point a cancel could otherwise be lost:
/// before the session is allocated, before the backend send, while waiting for live capture, and
/// at the `Recording` commit (a compare-exchange, so a cancel and the commit cannot both win).
struct ProductionStart {
    observed: Vec<AppState>,
    initial_target: TargetSnapshot,
    prepared: Option<runtime::PreparedStart>,
    dispatched: Option<runtime::PreparedStartResult>,
    /// The session `allocate_interaction` claimed, until it is dispatched or unwound.
    session_id: Option<String>,
    /// This press's pending start: its attempt id (owned by this start and passed down
    /// explicitly; no shared slot carries it) and whether a cancel or stop reached it first.
    token: Arc<runtime::PendingStart>,
}

impl ProductionStart {
    /// The press was cancelled after the session was allocated and before anything was sent to
    /// the engine: release the session and return to Armed. The engine never saw it.
    fn abort_unsent(&mut self) -> String {
        if let Some(session_id) = self.session_id.take() {
            if let Ok(mut runtime) = runtime_lock() {
                let observed = &mut self.observed;
                runtime.abort_prepared_start(&session_id, |state| observe_state(observed, state));
            }
        }
        START_CANCELLED.to_string()
    }

    /// The press was cancelled after the start reached the engine: cancel the session it started
    /// (stops capture and closes the session; also what the not-live path does).
    fn cancel_dispatched(&mut self, source: CancelSource) {
        self.dispatched = None;
        self.session_id = None;
        let cancel = runtime_lock().map(|mut runtime| runtime.prepare_cancel(source));
        if let Ok(prepared) = cancel {
            let _ = prepared.send();
        }
    }
}

impl StartSteps for ProductionStart {
    fn observe_target(&mut self) {
        // A press already cancelled has no use for the target; the start unwinds at the next step.
        if self.token.is_cancelled() {
            return;
        }
        let began = std::time::Instant::now();
        self.initial_target = host().observe_start_target();
        // Content-free timing so installed logs show what the hotkey-time observation costs now
        // that the microphone no longer waits for it.
        telemetry::log_message(
            "start_observe_target",
            &format!("elapsed_ms={}", began.elapsed().as_millis()),
        );
    }

    fn allocate_interaction(&mut self) -> Result<(), String> {
        // Cancelled during the target observation or while queued on the gate: no session yet,
        // nothing to unwind.
        if self.token.is_cancelled() {
            return Err(START_CANCELLED.to_string());
        }
        let mut runtime = runtime_lock()?;
        let observed = &mut self.observed;
        let prepared = runtime.prepare_start_dictation_for_attempt(
            self.initial_target.clone(),
            self.token.capture_attempt(),
            |state| observe_state(observed, state),
        )?;
        self.session_id = runtime.active_session_id();
        self.prepared = Some(prepared);
        Ok(())
    }

    fn show_optimistic_recording(&mut self) {
        // Visual only: Recording look now, no spinner. The Recording STATE is committed by
        // `acknowledge_recording`; rejection falls to the Error visual in `start_dictation_inner`.
        // A press already cancelled, or released before any capture existed, shows nothing.
        if self.token.is_cancelled() || self.token.refuse_if_released_before_capture() {
            return;
        }
        let host = host();
        if host.onboarding_surfaces_enabled() {
            host.show_pill_cue(PillCue::Recording);
        }
    }

    fn backend_start(&mut self) -> Result<(), String> {
        let prepared = self.prepared.take().ok_or("start not allocated")?;
        // Last check before the backend send: a cancel that arrived while the session was being
        // allocated unwinds here, and nothing reaches the engine. No warm-up is awaited: the
        // engine adopts this press's warm-up if it already got there, and otherwise this start
        // cold starts at once (a late warm-up is fenced by attempt id).
        if self.token.is_cancelled() {
            return Err(self.abort_unsent());
        }
        // Capture begins with this send. A press released while no capture existed (no engine could
        // seal it) is cancelled here instead, so nothing is recorded after the release; the
        // ordinary cancelled unwind follows (`start_dictation_after_press`).
        if !self.token.begin_capture() {
            return Err(self.abort_unsent());
        }
        match prepared.send() {
            Ok(dispatched) => {
                self.dispatched = Some(dispatched);
                Ok(())
            }
            Err(error) => {
                // The send may have failed AFTER its frame reached the engine (an RPC timeout, a
                // lost reply, a key delivery that failed after the start was accepted): the engine
                // can already be recording a session the shell is about to forget. Treat it as
                // possibly started: send the session-correlated cancel (session id plus this
                // press's attempt id) FIRST. The engine cancels the session if it started and
                // fences the session and attempt if it did not, so a start that arrives later does
                // not record. The shell state is cleared only after that.
                cancel_possibly_started(&error.session_id);
                if self.token.is_cancelled() {
                    // Cancelled while the send was in flight: it is a cancel, not a failed start.
                    return Err(self.abort_unsent());
                }
                if let Ok(mut runtime) = runtime_lock() {
                    let observed = &mut self.observed;
                    runtime.fail_prepared_start(
                        &error.session_id,
                        error.message.clone(),
                        |state| observe_state(observed, state),
                    );
                }
                self.session_id = None;
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
        let token = Arc::clone(&self.token);
        let Err(message) = await_capture_live(
            || {
                // A cancel ends the wait at the next poll instead of after the whole window.
                if token.is_cancelled() {
                    CaptureLiveness::Failed(START_CANCELLED.to_string())
                } else {
                    capture_liveness(&session_id)
                }
            },
            std::thread::sleep,
            CAPTURE_LIVE_WINDOW,
            CAPTURE_LIVE_POLL,
        ) else {
            return Ok(());
        };
        if message == START_CANCELLED {
            // The engine holds the session this press started: cancel it.
            self.cancel_dispatched(CancelSource::ShortcutIntent);
            return Err(message);
        }
        telemetry::log_message(
            "start_capture_not_live",
            &format!(
                "session={session_id} code={}",
                crate::dictation_error::error_log_code(&message)
            ),
        );
        // Stop any capture the worker left running, then close the session. Cancel clears the
        // active session, so the start-failure telemetry that `fail_prepared_start` would emit is
        // recorded here.
        self.cancel_dispatched(CancelSource::Watchdog);
        Err(message)
    }

    fn acknowledge_recording(&mut self) -> Result<AppState, String> {
        let dispatched = self.dispatched.take().ok_or("start not dispatched")?;
        // The commit point. A cancel that won the compare-exchange before this line wins: the
        // session the engine started is cancelled and no `Recording` is ever committed. A cancel
        // after it finds no pending start and takes the ordinary path against the session, which
        // it reaches only after this gate section ends.
        if !self.token.commit() {
            drop(dispatched);
            self.cancel_dispatched(CancelSource::ShortcutIntent);
            return Err(START_CANCELLED.to_string());
        }
        self.session_id = None;
        // Bind the committed session to the press before anything can take its deferred stop.
        if let runtime::PreparedStartResult::Engine { session_id } = &dispatched {
            self.token.bind_session(session_id.clone());
        }
        let mut runtime = runtime_lock()?;
        let observed = &mut self.observed;
        runtime.finalize_prepared_start(dispatched, |state| observe_state(observed, state))
    }
}

/// Whether this press's warm-up can lead to a recording. A press made while a previous dictation
/// is still in flight (Recording, Transcribing or Pasting) is refused by the start ("still
/// finishing the previous dictation"), so it must not resume the microphone at all.
fn warmup_can_lead_to_recording() -> bool {
    !runtime::app_state_lock_free().is_some_and(|state| {
        matches!(
            state,
            AppState::Recording { .. } | AppState::Transcribing { .. } | AppState::Pasting { .. }
        )
    })
}

/// Start a recording: the voice-controls check, the engine consumer, focus coverage, the start
/// sequence, then state and pill emission.
///
/// The whole start is reserved first (`StartReservation`). A press that cannot reserve it (another
/// press's start call has not fully ended) returns at once with the authoritative current state:
/// it observes no target, shows no pill, takes no backend gate, reaches no engine, sends no
/// warm-up and revokes nothing, so it can neither start a second session, claim the first press's
/// warm-up, nor retire it. The reservation is held until this call returns.
/// Explicit wake/developer recording: no precise capture epoch or native release requirement.
pub fn start_dictation_legacy_inner(shell: &Shell) -> Result<AppState, String> {
    let Some(reservation) = runtime::StartReservation::claim_legacy() else {
        return get_app_state();
    };
    run_reserved_start(shell, reservation)
}

pub fn start_dictation_inner(shell: &Shell) -> Result<AppState, String> {
    let Some(reservation) = runtime::StartReservation::claim() else {
        telemetry::log_message("start_coalesced", "reason=start_in_flight");
        return get_app_state();
    };
    run_reserved_start(shell, reservation)
}

/// `start_dictation_inner` for a caller that must return at once: the Windows push-to-talk press
/// callback. The hook's dispatch thread is serial, so a press callback that stays in the start
/// (100-300 ms of target observation, runtime lock and engine round trips) holds the same
/// press's release callback back until the start returns, and the release would be seen late.
/// This claims the press's reservation here, synchronously and cheaply (so a release that follows
/// at once already finds the pending start and records its stop intent), then runs the whole start
/// on its own thread and returns. A press that cannot reserve is coalesced exactly as
/// `start_dictation_inner` does. Failures are reported the same way (error pill) and logged; there
/// is no result to hand back.
pub fn start_dictation_detached(shell: &Shell) -> Result<(), String> {
    start_dictation_detached_route(shell, true)
}

pub fn start_dictation_legacy_detached(shell: &Shell) -> Result<(), String> {
    start_dictation_detached_route(shell, false)
}

fn start_dictation_detached_route(shell: &Shell, exact: bool) -> Result<(), String> {
    let reservation = if exact {
        runtime::StartReservation::claim()
    } else {
        runtime::StartReservation::claim_legacy()
    };
    let Some(reservation) = reservation else {
        telemetry::log_message("start_coalesced", "reason=start_in_flight");
        return Ok(());
    };
    let shell = shell.clone();
    // If the thread cannot be created the closure is dropped with the reservation inside it, which
    // releases the reservation.
    std::thread::Builder::new()
        .name("dictation-start".to_string())
        .spawn(move || {
            if let Err(error) = run_reserved_start(&shell, reservation) {
                telemetry::log_message(
                    "start_detached_error",
                    &format!("code={}", crate::dictation_error::error_log_code(&error)),
                );
            }
        })
        .map(|_| ())
        .map_err(|error| format!("start thread: {error}"))
}

/// The start of the press that holds `reservation`, to the end of its deferred stop (if any).
fn run_reserved_start(
    shell: &Shell,
    mut reservation: runtime::StartReservation,
) -> Result<AppState, String> {
    host().require_voice_controls()?;
    // A claim made during an engine retirement has no hold yet; take it here (off the dispatch
    // thread, no slot held), so the start runs on the fresh generation.
    reservation.ensure_engine_hold();
    // The mic starts now, before any shell work: the engine keeps its stream paused while idle
    // and resumes it only on a start or this warm-up, and the target observation, pill, runtime
    // lock and engine snapshot below cost 100-300 ms that would otherwise be lost speech. Nothing
    // waits for it. The engine adopts this audio at the start frame of the same attempt when the
    // warm-up got there first; otherwise the start cold starts, as before, and the late warm-up is
    // fenced. The attempt id is this press's: the start, stop and cancel frames repeat it, and a
    // press that ends without a recording revokes it so the mic pauses at once.
    let token = reservation.token();
    let attempt = token.attempt();
    // The user released this press while no engine could seal it (it was retiring or down) and no
    // capture of the press existed: everything a capture started now would hold was spoken after
    // the release. End the press as a cancelled, empty one: no warm-up, no recording state, no
    // audio, no deferred stop to replay on a new recording. A cancel that arrived first reads the
    // same way; the reservation drops here, so the next press gets a fresh attempt.
    if token.refuse_if_released_before_capture() {
        let state = get_app_state();
        drop(reservation);
        return state;
    }
    let warmed = token.capture_attempt().is_some() && warmup_can_lead_to_recording();
    if warmed {
        // Takes the press's `begin_capture` (refused after a release) and only reaches a running
        // engine.
        runtime::send_capture_warmup(&token);
    }
    let result = start_dictation_after_press(shell, Arc::clone(&token));
    if warmed && !matches!(result, Ok(AppState::Recording { .. })) {
        runtime::revoke_capture_warmup(attempt);
    }
    // A stop (not a cancel) that reached this press before it committed was recorded as its
    // intent: now that `Recording` is committed, perform it, so a quick press-and-release is
    // transcribed and delivered. The stop is bound to this press: it acts only if the session this
    // press committed (`PendingStart::committed_session`) is still the runtime's active session,
    // and it is dispatched BEFORE the reservation is released, so no other press can start (or
    // stop) in between. A start that did not end in `Recording` has nothing to stop.
    let deferred_stop = match &result {
        Ok(AppState::Recording { .. }) => token.take_deferred_stop().zip(token.committed_session()),
        _ => None,
    };
    let outcome = match deferred_stop {
        Some((stop, session_id)) => {
            telemetry::log_message("start_stop_deferred", "phase=performing_after_commit");
            // Capture had begun, but the release could not reach the engine when it happened (a
            // retirement, a missed ack): apply the ORIGINAL release instant as the seal cutoff
            // first, so the stop below finalizes audio that ends at the release, not at this stop.
            if runtime::seal_capture_at_recorded_release(&token) {
                run_deferred_stop(shell.as_ref(), stop, &session_id)
            } else {
                Err("Capture release could not be sealed; recording discarded.".into())
            }
        }
        None => result,
    };
    drop(reservation);
    outcome
}

/// Perform the stop a user asked for during the press's start, against the session that start
/// committed and no other. Runs under the backend transition gate and the runtime lock like every
/// stop (`stop_dictation_for_session` and its siblings), and does nothing when `session_id` is no
/// longer the active session (it ended, was cancelled or replaced meanwhile).
fn run_deferred_stop(
    shell: &dyn DictationShell,
    stop: runtime::DeferredStop,
    session_id: &str,
) -> Result<AppState, String> {
    match stop {
        runtime::DeferredStop::Stop => stop_dictation_for_session(shell, Some(session_id)),
        runtime::DeferredStop::PillStop { send_enter } => {
            stop_dictation_from_pill_for_session(shell, send_enter, Some(session_id))
        }
        runtime::DeferredStop::StopAndSend => {
            stop_dictation_and_send_for_session(shell, Some(session_id))
        }
    }
}

/// `start_dictation_inner` after the shortcut-press warm-up has been sent. The press's pending
/// token is this call's own; it reaches the start frame through `ProductionStart`, so concurrent
/// presses never share it.
fn start_dictation_after_press(
    shell: &Shell,
    token: Arc<runtime::PendingStart>,
) -> Result<AppState, String> {
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
        session_id: None,
        token,
    };
    // The consumer delivers the first-frames signal the start now waits on, so it must run before
    // the wait (idempotent).
    crate::engine_events::start_engine_event_consumer(shell.clone());
    // macOS focus continuity is on demand: arm it BEFORE the hotkey anchor is read (the first start
    // step), keep it until a terminal state ends it.
    #[cfg(target_os = "macos")]
    host().focus_coverage_start_begin();
    let result = run_start_sequence(&mut steps, |op| with_backend_transition(op));
    // A press cancelled before it committed unwinds with an error of its own making (or with the
    // backend's, if the cancel overtook a failure). That is a cancel, not a failed start: report
    // the authoritative state, so the optimistic Recording look is repainted and no error pill
    // shows.
    let result = match result {
        Err(_) if steps.token.is_cancelled() => {
            telemetry::log_message("start_cancelled", "phase=unwound");
            get_app_state()
        }
        other => other,
    };
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
    stop_dictation_and_send_inner_at(shell, runtime::OriginalReleaseMark::for_current_action())
}

pub fn stop_dictation_and_send_inner_at(
    shell: &dyn DictationShell,
    released: runtime::OriginalReleaseMark,
) -> Result<AppState, String> {
    // Release seal and stop intent of a pending start (see `stop_dictation_inner`): the start
    // performs the stop-and-send (Enter after the paste) once it is recording.
    if !runtime::seal_capture_for_stop_at(released) {
        return Err("Capture release could not be sealed; recording discarded.".into());
    }
    if runtime::defer_stop_to_pending_start(runtime::DeferredStop::StopAndSend) {
        return get_app_state();
    }
    stop_dictation_and_send_for_session(shell, None)
}

/// The stop body of `stop_dictation_and_send_inner` (see `stop_dictation_for_session`).
fn stop_dictation_and_send_for_session(
    shell: &dyn DictationShell,
    expected: Option<&str>,
) -> Result<AppState, String> {
    telemetry::utterance_timeline::begin_stop("stop_send");
    let mut observed = Vec::new();
    let result = with_backend_transition(|| {
        let prepared = {
            let mut runtime = runtime_lock()?;
            if expected_session_is_active(&runtime, expected) {
                runtime.prepare_stop_and_send(|state| observe_state(&mut observed, state))
            } else {
                telemetry::log_message("start_stop_deferred", "phase=session_mismatch_skipped");
                Ok(runtime::PreparedStop::without_command(runtime.app_state()))
            }
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
