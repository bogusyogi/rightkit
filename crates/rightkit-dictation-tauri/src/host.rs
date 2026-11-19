//! The seam between the dictation runtime and the application that embeds it.
//!
//! The runtime owns the recording state machine, the engine sidecar bridge, history sequencing and
//! the transcribe watchdog. Everything that belongs to the application shell or to a platform
//! (settings storage, the history databases, focus capture and text insertion, media pause, sync
//! notifications, vocabulary) is reached through [`DictationHost`]. The application implements it
//! once and installs it with [`install_host`] before the first dictation call.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, OnceLock};

use rightkit_dictation_core::engine::{RecordingContext, RecordingContextState};
use rightkit_dictation_core::settings::SettingsBlob;
use rightkit_dictation_core::state::AppState;
use rightkit_text_polish::vocab_learn::Suggestion;
use rightkit_text_polish::vocabulary::VocabularyEntry;
use rightkit_transcript_library::delivery::{
    DeliveryBackend, DeliveryError, DeliveryRecord, TargetSnapshot,
};
use rightkit_transcript_library::export::ExportLabels;
use rightkit_transcript_library::history::{
    HistoryError, HistoryFilter, HistoryOrigin, HistoryPage, HistoryQuery, HistoryRetention,
    HistoryStore, ScopedDeleteOutcome, ScopedUpdateOutcome,
};

use crate::shell::PillCue;

/// A hotkey callback.
pub type HotkeyAction = Arc<dyn Fn() + Send + Sync + 'static>;
/// A hotkey callback that receives a pressed flag.
pub type HotkeyFnAction = Arc<dyn Fn(bool) + Send + Sync + 'static>;

/// An opaque platform focus pin (an accessibility element or window handle retained for one
/// recording). The host creates it in `capture_*_destination` and downcasts it in
/// [`DictationHost::deliver_final`].
pub type HostPin = Arc<dyn Any + Send + Sync>;

/// Everything the host needs to insert one final transcript.
pub struct FinalDelivery<'a> {
    /// Transcript text to insert, with any control tail already removed.
    pub transcript: &'a str,
    /// Press Enter after the text lands.
    pub want_enter: bool,
    /// Whether the start field context was readable.
    pub start_context_state: Option<RecordingContextState>,
    /// Whether that context reached a provider.
    pub context_request_attempted: Option<bool>,
    /// Whether the provider bound the transcript to the start field.
    pub context_original_binding: Option<bool>,
    /// Destination frozen when recording started.
    pub start_target: Option<&'a TargetSnapshot>,
    /// Destination frozen at the first stop boundary.
    pub stop_target: Option<&'a TargetSnapshot>,
    /// Focus pin captured at start.
    pub start_pin: Option<&'a HostPin>,
    /// Focus pin captured at stop.
    pub stop_pin: Option<&'a HostPin>,
    /// Fingerprint of the start field content.
    pub start_content_fingerprint: Option<u64>,
    /// Called once the text has been inserted.
    pub on_inserted: &'a mut dyn FnMut(),
}

/// What [`DictationHost::deliver_final`] did with the transcript.
pub enum FinalOutcome {
    /// The record the delivery produced.
    Delivered(DeliveryRecord),
    /// Delivery was refused and the record is a clipboard fallback; the runtime will not send Enter.
    Recovery(DeliveryRecord),
}

/// What streaming insertion left in the destination when the final transcript arrived. Closed, so
/// the runtime cannot read "unknown" as "nothing was typed".
pub enum StreamingFinal {
    /// This session typed nothing into any field. The runtime delivers the whole transcript once,
    /// through the normal gated delivery.
    NothingWritten,
    /// The streamed text is a prefix of the final transcript. `remainder` (possibly empty) is the
    /// only text the runtime inserts automatically; history keeps the whole transcript.
    Reconciled { remainder: String },
    /// Streamed text is, or may still be, in the field. The runtime delivers nothing, presses no
    /// Return and offers the full transcript for an explicit copy. `tag` and `cause` are stable,
    /// content-free labels for the log (for example `stopped_writes_remain` and a gate refusal
    /// code).
    Stopped {
        tag: &'static str,
        cause: &'static str,
    },
}

/// Timing of the sidecar restart watcher (`crate::sidecar`): how often it looks for the runtime to
/// go idle, and how long it waits before it gives up and clears the pending restart.
#[derive(Clone, Copy, Debug)]
pub struct SidecarRestartTiming {
    pub poll: std::time::Duration,
    pub give_up_after: std::time::Duration,
}

/// The values of the Wake Lab listener (`crate::wake_lab`): the wake model, the threshold default
/// and bounds, and the wording of the harness's own errors.
#[derive(Clone, Copy, Debug)]
pub struct WakeLabProfile {
    /// The wake model the listener loads.
    pub model: &'static str,
    /// The threshold used before the owner saves one.
    pub default_threshold: f32,
    /// Lowest threshold a saved value is clamped to.
    pub threshold_min: f32,
    /// Highest threshold a saved value is clamped to.
    pub threshold_max: f32,
    /// Error when the owner-access marker is absent.
    pub marker_absent: &'static str,
    /// Error prefix when the owner-access marker cannot be created (`"<prefix>: <cause>"`).
    pub enable_access_failed: &'static str,
    /// Error prefix when the settings file cannot be written (`"<prefix>: <cause>"`).
    pub config_write_failed: &'static str,
    /// Error prefix when the settings cannot be serialised (`"<prefix>: <cause>"`).
    pub config_serialize_failed: &'static str,
}

/// Application services the dictation runtime calls. Defaults are inert unless noted.
pub trait DictationHost: Send + Sync + 'static {
    /// The settings document as the engine should see it (a clone; nothing is persisted).
    fn settings_snapshot(&self) -> SettingsBlob;
    /// True once the user finished onboarding.
    fn onboarding_complete(&self) -> bool;
    /// Pro entitlement derived from the native licence owner.
    fn engine_is_pro(&self) -> bool {
        false
    }
    /// The ASR backend the engine may use for a stored choice and the entitlement.
    fn effective_asr_backend(&self, stored: &str, _is_pro: bool) -> String {
        stored.to_string()
    }
    /// How long history is kept.
    fn history_retention(&self) -> HistoryRetention {
        HistoryRetention::default()
    }
    /// Whether encrypted history is enabled; false selects the JSON file store.
    fn encrypted_history_enabled(&self) -> bool {
        true
    }
    /// Open the JSON file history store.
    fn open_json_history(&self) -> Result<Box<dyn HistoryStore>, String>;
    /// Open the encrypted history store.
    fn open_encrypted_history(&self) -> Result<Box<dyn HistoryStore>, String>;

    /// The custom vocabulary.
    fn vocabulary_list(&self) -> Vec<VocabularyEntry> {
        Vec::new()
    }
    /// Persist vocabulary suggestions learned from a history edit.
    fn suggest_vocabulary(&self, _candidates: Vec<Suggestion>) -> Result<(), String> {
        Ok(())
    }

    /// A backend that inserts text at the current destination.
    fn delivery_backend(&self) -> Box<dyn DeliveryBackend> {
        Box::new(DefaultDelivery)
    }
    /// Deliver an empty result (a command-only utterance).
    fn deliver_empty(&self) -> Result<DeliveryRecord, String> {
        self.delivery_backend()
            .deliver("")
            .map_err(|err| err.to_string())
    }
    /// Insert the final transcript, choosing between the start and stop destinations.
    ///
    /// Without an explicit host policy, preserve clipboard & transcript for recovery. Hosts with
    /// a `paste_delivery::DeliveryHost` override this with `final_delivery::deliver_final_for_host`.
    fn deliver_final(&self, request: FinalDelivery<'_>) -> Result<FinalOutcome, String> {
        Ok(FinalOutcome::Recovery(DeliveryRecord::new(
            rightkit_transcript_library::delivery::next_delivery_id(0),
            request.transcript,
            rightkit_transcript_library::delivery::DeliveryOutcome::CopiedFallback {
                reason:
                    rightkit_transcript_library::delivery::CopyFallbackReason::clipboard_preserved(),
            },
            request
                .stop_target
                .or(request.start_target)
                .cloned()
                .unwrap_or_else(TargetSnapshot::empty),
        )))
    }

    /// The start destination the runtime may freeze for a hotkey-time target. `None` leaves the
    /// start identity unresolved.
    fn resolve_start_target(&self, target: &TargetSnapshot) -> Option<TargetSnapshot> {
        Some(target.clone())
    }
    /// True when the host captures the start field context.
    fn supports_start_capture(&self) -> bool {
        false
    }
    /// Capture the start destination, focus pin, field context and content fingerprint.
    fn capture_start_destination(
        &self,
        initial: TargetSnapshot,
    ) -> (
        TargetSnapshot,
        Option<HostPin>,
        RecordingContext,
        Option<u64>,
    ) {
        (initial, None, unreadable_context(), None)
    }
    /// Like [`Self::capture_start_destination`] for a recording that has no hotkey-time target; the
    /// host samples the live foreground itself.
    fn capture_start_destination_late(
        &self,
        frozen: Option<TargetSnapshot>,
    ) -> (
        TargetSnapshot,
        Option<HostPin>,
        RecordingContext,
        Option<u64>,
    ) {
        self.capture_start_destination(frozen.unwrap_or_else(TargetSnapshot::empty))
    }
    /// Freeze the stop destination and its focus pin.
    fn capture_stop_destination(&self) -> (TargetSnapshot, Option<HostPin>) {
        (
            rightkit_dictation_command_host::delivery::snapshot_target(),
            None,
        )
    }
    /// Drop an accessibility observation retained for a focus pin.
    fn release_pending_accessibility_observation(&self) {}

    /// A history row changed locally; wake the sync scheduler.
    fn notify_local_change(&self) {}
    /// Publish tombstones for deleted record ids.
    fn enqueue_deleted_ids(&self, _ids: &[String]) -> Result<(), String> {
        Ok(())
    }

    /// Pause other media for a session.
    fn media_begin_for_session(&self, _session_id: &str) {}
    /// Resume media paused for a session.
    fn media_restore_for_session(&self, _session_id: &str, _reason: &str) {}
    /// Resume any media this runtime paused.
    fn media_restore_any(&self, _reason: &str) {}

    /// Streaming insertion: reconcile prior writes with final text; stopped outcomes forbid more writes.
    fn streaming_take_for_final(&self, _session_id: &str, _transcript: &str) -> StreamingFinal {
        StreamingFinal::NothingWritten
    }
    /// Streaming insertion: a partial transcript was committed. Host decides whether streaming
    /// insertion is enabled; default performs no insertion. Called with runtime's
    /// session mutex held: implementations must not reenter runtime or acquire its
    /// backend transition gate.
    fn streaming_on_committed(&self, _session_id: &str, _text: &str) {}
    /// Streaming insertion: press Enter in the destination.
    fn streaming_press_enter(&self) -> bool {
        false
    }
    /// Streaming insertion: erase what was typed live for a cancelled session.
    fn streaming_erase_on_cancel(&self, _session_id: &str) {}

    // ---- Shell policy and pill (process-wide values the runtime decides with)

    /// The pill and tray are allowed to show for this install (onboarding finished). Default:
    /// [`Self::onboarding_complete`].
    fn onboarding_surfaces_enabled(&self) -> bool {
        self.onboarding_complete()
    }
    /// True once the engine is warm and the pill is live. A dictation hotkey before this is
    /// discarded, not queued. Default: true.
    fn launch_finished(&self) -> bool {
        true
    }
    /// True while the onboarding dictation or shortcut test owns the hotkeys. Default: false.
    fn onboarding_dictation_test_active(&self) -> bool {
        false
    }
    /// Whether a dictation hotkey may start or stop a recording now. Default:
    /// [`Self::onboarding_complete`].
    fn onboarding_dictation_hotkeys_enabled(&self) -> bool {
        self.onboarding_complete()
    }
    /// Whether hotkeys currently light the onboarding shortcut test instead of dictating. Default:
    /// false.
    fn onboarding_shortcut_test_hotkeys_enabled(&self) -> bool {
        false
    }
    /// Whether push-to-talk is allowed now (normal use and the onboarding chat test). Default: a
    /// dictation hotkey is allowed or the onboarding test is active.
    fn ptt_dictation_allowed(&self) -> bool {
        self.onboarding_dictation_hotkeys_enabled() || self.onboarding_dictation_test_active()
    }
    /// Whether the onboarding Hub is the consumer of the live recording level. Default: onboarding
    /// is not complete.
    fn recording_level_meter_active(&self) -> bool {
        !self.onboarding_complete()
    }
    /// Show a look on the pill. The caller has already applied
    /// [`Self::onboarding_surfaces_enabled`] where the look is gated. Default: nothing.
    fn show_pill_cue(&self, _cue: PillCue) {}
    /// Show the pill look for a committed runtime state. Default: nothing.
    fn show_pill_for_state(&self, _state: &AppState) {}
    /// A delivery was recorded; `target` is the delivered destination when known. The shell may use
    /// it for a tip. Default: nothing.
    fn note_delivery_recorded(&self, _target: Option<&TargetSnapshot>) {}
    /// The runtime went idle; wake background work that waits for it. Default: nothing.
    fn notify_runtime_idle(&self) {}
    /// Run a job on the shell's blocking pool. Default: a new thread.
    fn spawn_blocking(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        std::thread::spawn(job);
    }
    /// Native overlay window handles delivery must exclude when it resolves the destination.
    /// Default: none.
    fn pill_overlay_handles(&self) -> Vec<isize> {
        Vec::new()
    }
    /// The destination observed at the start hotkey (evidence only, never a destination). Default:
    /// the foreground target.
    fn observe_start_target(&self) -> TargetSnapshot {
        rightkit_dictation_command_host::delivery::snapshot_target()
    }
    /// The pill clipboard popup shows this transcript instead of history; set after an edit of the
    /// "current" row. Default: nothing.
    fn set_pill_clipboard_override(&self, _text: Option<String>) -> Result<(), String> {
        Ok(())
    }
    /// Put text on the clipboard on the user's explicit request (an overwrite). Default: the
    /// command host's `copy_text`.
    fn copy_text_to_clipboard(&self, text: &str) -> Result<(), String> {
        rightkit_dictation_command_host::delivery::copy_text(text).map_err(|err| err.to_string())
    }
    /// The bundled realtime voice controls are present. Default: ok.
    fn require_voice_controls(&self) -> Result<(), String> {
        Ok(())
    }
    /// Activate the shared model before the first engine spawn of a process resumed mid-onboarding.
    /// Default: ok.
    fn ensure_shared_model_active(&self) -> Result<(), String> {
        Ok(())
    }
    /// Activate the shared model for a boot. Default: ok.
    fn prepare_shared_model(&self) -> Result<(), String> {
        Ok(())
    }
    /// True when the selected backend has no model that resolves. Default: false.
    fn parakeet_model_missing(&self) -> bool {
        false
    }
    /// The launch splash made progress. Default: nothing.
    fn splash_note_progress(&self) {}
    /// The launch splash should show its failure actions. Default: nothing.
    fn splash_note_failure(&self) {}

    // ---- Sidecar restarts and Wake Lab (required: each value is the application's)

    /// Poll interval and give-up deadline of the deferred sidecar restart watcher.
    fn sidecar_restart_timing(&self) -> SidecarRestartTiming;
    /// The Wake Lab values (see [`WakeLabProfile`]).
    fn wake_lab_profile(&self) -> WakeLabProfile;
    /// The Wake Lab owner-access marker file. Its presence is what allows the listener to start.
    fn wake_lab_marker_path(&self) -> std::path::PathBuf;
    /// The Wake Lab settings file (enabled flag and threshold).
    fn wake_lab_config_path(&self) -> std::path::PathBuf;
    /// Replace a whole small file so a reader sees the old file or the new one, creating parent
    /// directories when missing.
    fn replace_file_atomically(&self, path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()>;

    // ---- History commands

    /// The wording of history exports.
    fn history_export_labels(&self) -> ExportLabels;
    /// Persist the history retention choice. Default: nothing.
    fn set_history_retention(&self, _retention: HistoryRetention) -> Result<(), String> {
        Ok(())
    }
    /// Recovery diagnostics text (no key, no history content) for a user-chosen file. Default:
    /// empty.
    fn history_recovery_diagnostics(&self) -> String {
        String::new()
    }

    // ---- Platform hooks

    /// Windows: the recording state the low-level cancel hook should believe. Default: nothing.
    #[cfg(target_os = "windows")]
    fn set_recording_active_hook(&self, _active: bool) {}
    /// macOS: arm focus coverage for a start that is about to run. Default: nothing.
    #[cfg(target_os = "macos")]
    fn focus_coverage_start_begin(&self) {}
    /// macOS: the start is over; `outcome` is the state it produced. Default: nothing.
    #[cfg(target_os = "macos")]
    fn focus_coverage_start_finish(&self, _outcome: Option<&AppState>) {}
    /// macOS: whether Accessibility is granted (no prompt). Default: true.
    #[cfg(target_os = "macos")]
    fn accessibility_trusted(&self) -> bool {
        true
    }
    /// macOS: enable or disable the fn-chord tap. Default: nothing.
    #[cfg(target_os = "macos")]
    fn fn_hotkey_set_enabled(&self, _enabled: bool) {}
    /// macOS: register the fn-chord callbacks (toggle, stop and send, cancel, fn alone). Default:
    /// nothing.
    #[cfg(target_os = "macos")]
    fn fn_hotkey_set_chord_callbacks(
        &self,
        _toggle: HotkeyAction,
        _stop_send: HotkeyAction,
        _cancel: HotkeyAction,
        _fn_change: HotkeyFnAction,
    ) {
    }
    /// macOS: register the push-to-talk callbacks (start, stop, stop and send, cancel). Default:
    /// nothing.
    #[cfg(target_os = "macos")]
    fn fn_hotkey_set_ptt_callbacks(
        &self,
        _start: HotkeyAction,
        _stop: HotkeyAction,
        _stop_send: HotkeyAction,
        _cancel: HotkeyAction,
    ) {
    }
    /// macOS: apply the push-to-talk key and enabled setting to the tap. Default: nothing.
    #[cfg(target_os = "macos")]
    fn fn_hotkey_apply_ptt_settings(&self) {}
    /// macOS: spawn the fn-chord tap once both callback sets are registered. Default: nothing.
    #[cfg(target_os = "macos")]
    fn fn_hotkey_start(&self) {}
    /// macOS: whether push-to-talk is enabled in settings. Default: false.
    #[cfg(target_os = "macos")]
    fn push_to_talk_enabled(&self) -> bool {
        false
    }
    /// macOS: whether middle-mouse recording is enabled in settings. Default: false.
    #[cfg(target_os = "macos")]
    fn middle_mouse_recording_enabled(&self) -> bool {
        false
    }
    /// macOS: register the middle-mouse toggle callback. Default: nothing.
    #[cfg(target_os = "macos")]
    fn middle_mouse_set_toggle_callback(&self, _callback: HotkeyAction) {}
    /// macOS: apply the middle-mouse enabled setting and spawn the tap. Default: nothing.
    #[cfg(target_os = "macos")]
    fn middle_mouse_start(&self) {}

    /// Debug check that no UI dispatch happens while the runtime lock is held.
    fn assert_ui_dispatch_unlocked(&self) {}
}

fn unreadable_context() -> RecordingContext {
    RecordingContext {
        app_name: None,
        window_title: None,
        field_text: None,
        selected_text: None,
        state: RecordingContextState::Unreadable,
        request_attempted: false,
    }
}

struct DefaultDelivery;

impl DeliveryBackend for DefaultDelivery {
    fn deliver(&mut self, transcript: &str) -> Result<DeliveryRecord, DeliveryError> {
        rightkit_dictation_command_host::delivery::deliver_text(transcript)
    }
}

static HOST: OnceLock<Arc<dyn DictationHost>> = OnceLock::new();

/// Install the host once at startup. Returns false when one is already installed.
pub fn install_host(host: Arc<dyn DictationHost>) -> bool {
    HOST.set(host).is_ok()
}

static PRODUCT_NAME: OnceLock<&'static str> = OnceLock::new();
static ENGINE_NAME: OnceLock<&'static str> = OnceLock::new();
static MICROPHONE_OFF_LABEL: OnceLock<&'static str> = OnceLock::new();
static ENGINE_CRASH_MESSAGE: OnceLock<&'static str> = OnceLock::new();

/// Name the product in the pill label shown when Windows refuses microphone access ("Microphone
/// access is off for `<name>` in Windows Settings"). Default: "this app". Once per process;
/// returns false when a name was already set. Call before the first dictation.
pub fn set_product_name(name: &str) -> bool {
    if PRODUCT_NAME
        .set(Box::leak(name.to_owned().into_boxed_str()))
        .is_err()
    {
        return false;
    }
    let _ = MICROPHONE_OFF_LABEL.set(Box::leak(
        format!("Microphone access is off for {name} in Windows Settings").into_boxed_str(),
    ));
    true
}

/// The product name set by [`set_product_name`], if any.
pub fn product_name() -> Option<&'static str> {
    PRODUCT_NAME.get().copied()
}

pub(crate) fn microphone_off_label() -> Option<&'static str> {
    MICROPHONE_OFF_LABEL.get().copied()
}

/// Name the engine process in the message logged when it crashes ("`<name>` crashed"). Default:
/// "engine". Once per process; returns false when a name was already set.
pub fn set_engine_name(name: &str) -> bool {
    if ENGINE_NAME
        .set(Box::leak(name.to_owned().into_boxed_str()))
        .is_err()
    {
        return false;
    }
    let _ = ENGINE_CRASH_MESSAGE.set(Box::leak(format!("{name} crashed").into_boxed_str()));
    true
}

pub(crate) fn engine_crash_message() -> &'static str {
    ENGINE_CRASH_MESSAGE
        .get()
        .copied()
        .unwrap_or("engine crashed")
}

/// The installed host.
///
/// # Panics
///
/// Panics when [`install_host`] was not called first.
pub fn host() -> &'static dyn DictationHost {
    HOST.get()
        .expect("rightkit-dictation-tauri: install_host must be called before dictation starts")
        .as_ref()
}

/// A delivery backend that forwards to the host's.
pub struct HostDelivery(Box<dyn DeliveryBackend>);

impl HostDelivery {
    /// A backend from [`DictationHost::delivery_backend`].
    pub fn new() -> Self {
        Self(host().delivery_backend())
    }
}

impl Default for HostDelivery {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for HostDelivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostDelivery")
    }
}

impl DeliveryBackend for HostDelivery {
    fn deliver(&mut self, transcript: &str) -> Result<DeliveryRecord, DeliveryError> {
        self.0.deliver(transcript)
    }
}

/// A history store opened through the host.
pub struct HostHistory(Box<dyn HistoryStore>);

impl HostHistory {
    /// The JSON file store from [`DictationHost::open_json_history`].
    pub fn open_json() -> Result<Self, String> {
        host().open_json_history().map(Self)
    }
}

impl fmt::Debug for HostHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostHistory")
    }
}

impl HistoryStore for HostHistory {
    fn rotate_encryption_key(&mut self) -> Result<(), HistoryError> {
        self.0.rotate_encryption_key()
    }
    fn push(&mut self, record: DeliveryRecord) -> Result<(), HistoryError> {
        self.0.push(record)
    }
    fn push_batch(&mut self, records: Vec<DeliveryRecord>) -> Result<(), HistoryError> {
        self.0.push_batch(records)
    }
    fn finalize_session(
        &mut self,
        draft_delivery_id: &str,
        record: DeliveryRecord,
    ) -> Result<(), HistoryError> {
        self.0.finalize_session(draft_delivery_id, record)
    }
    fn recent(&self) -> &[DeliveryRecord] {
        self.0.recent()
    }
    fn find(&self, delivery_id: &str) -> Option<DeliveryRecord> {
        self.0.find(delivery_id)
    }
    fn update_transcript(
        &mut self,
        delivery_id: &str,
        transcript: String,
    ) -> Result<bool, HistoryError> {
        self.0.update_transcript(delivery_id, transcript)
    }
    fn update_transcript_scoped(
        &mut self,
        delivery_id: &str,
        transcript: String,
        allowed_origins: &[HistoryOrigin],
    ) -> Result<ScopedUpdateOutcome, HistoryError> {
        self.0
            .update_transcript_scoped(delivery_id, transcript, allowed_origins)
    }
    fn upsert_draft(&mut self, session_id: &str, text: String) -> Result<(), HistoryError> {
        self.0.upsert_draft(session_id, text)
    }
    fn delete(&mut self, delivery_id: &str) -> Result<bool, HistoryError> {
        self.0.delete(delivery_id)
    }
    fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        self.0.query(query)
    }
    fn delete_matching(&mut self, filter: &HistoryFilter) -> Result<usize, HistoryError> {
        self.0.delete_matching(filter)
    }
    fn prune_before(&mut self, cutoff_ms: u64) -> Result<usize, HistoryError> {
        self.0.prune_before(cutoff_ms)
    }
    fn clear(&mut self) -> Result<(), HistoryError> {
        self.0.clear()
    }
    fn delete_scoped(
        &mut self,
        delivery_id: &str,
        allowed_origins: &[HistoryOrigin],
    ) -> Result<ScopedDeleteOutcome, HistoryError> {
        self.0.delete_scoped(delivery_id, allowed_origins)
    }
    fn delete_matching_scoped(
        &mut self,
        filter: &HistoryFilter,
        allowed_origins: &[HistoryOrigin],
    ) -> Result<usize, HistoryError> {
        self.0.delete_matching_scoped(filter, allowed_origins)
    }
}
