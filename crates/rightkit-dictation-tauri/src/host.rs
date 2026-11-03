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
use rightkit_text_polish::vocab_learn::Suggestion;
use rightkit_text_polish::vocabulary::VocabularyEntry;
use rightkit_transcript_library::delivery::{
    DeliveryBackend, DeliveryError, DeliveryRecord, TargetSnapshot,
};
use rightkit_transcript_library::history::{
    HistoryError, HistoryFilter, HistoryOrigin, HistoryPage, HistoryQuery, HistoryRetention,
    HistoryStore, ScopedDeleteOutcome, ScopedUpdateOutcome,
};

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
    /// The default pastes at the current foreground target.
    fn deliver_final(&self, request: FinalDelivery<'_>) -> Result<FinalOutcome, String> {
        let record = rightkit_dictation_command_host::delivery::deliver_text_with_enter(
            request.transcript,
            request.want_enter,
        )
        .map_err(|err| err.to_string())?;
        (request.on_inserted)();
        Ok(FinalOutcome::Delivered(record))
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

    /// Streaming insertion: the text still missing after what was typed live, taken for the final.
    fn streaming_take_for_final(&self, _session_id: &str, _transcript: &str) -> Option<String> {
        None
    }
    /// Streaming insertion: press Enter in the destination.
    fn streaming_press_enter(&self) -> bool {
        false
    }
    /// Streaming insertion: erase what was typed live for a cancelled session.
    fn streaming_erase_on_cancel(&self, _session_id: &str) {}

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
