// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

pub struct EngineRuntime {
    /// Shell state machine. `SharedState` (`Arc<RwLock<AppState>>`): the SAME
    /// Arc is cloned into `sidecar.state`, which is published via
    /// `shared::ENGINE_SIDECAR`, so `app_state_lock_free()` can serve
    /// `get_app_state`/watchdog reads while this runtime is behind a held
    /// `Mutex<RuntimeApp>` (e.g. during prewarm's config/vocab sync).
    /// Single-writer: every mutation still happens under the global lock.
    pub(crate) state: SharedState,
    pub(crate) sidecar: SidecarHandle,
    // Pass 3 — boxed trait object so we can swap between SQLCipher (default)
    // and the JSON history (env-var fallback) without changing call sites.
    pub(crate) history: Option<Box<dyn HistoryStore>>,
    /// Last background CloudKit history generation reflected by `history`.
    pub(crate) history_generation_seen: u64,
    pub(crate) recent_cache: Vec<DeliveryRecord>,
    pub(crate) last_delivery: Option<DeliveryRecord>,
    pub(crate) sequence: u64,
    pub(crate) active_session: Option<String>,
    /// Destination frozen at first recording-stop boundary; never restored.
    pub(crate) stop_target: Option<crate::delivery::TargetSnapshot>,
    /// Exact AX element/window retained only for this recording session.
    pub(crate) stop_focus_pin: Option<crate::host::HostPin>,
    /// Detached start capture; result is consumed at first stop boundary.
    pub(crate) start_capture_rx: Option<Receiver<StartCapture>>,
    pub(crate) start_dispatch_ready: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) start_target: Option<crate::delivery::TargetSnapshot>,
    pub(crate) start_focus_pin: Option<crate::host::HostPin>,
    pub(crate) start_context: Option<rightkit_dictation_core::engine::RecordingContext>,
    pub(crate) start_content_fingerprint: Option<u64>,
    /// First accepted stop action, bound to its recording session.
    pub(crate) pending_stop_context: PendingStopContextLatch,
    /// Freeze fix: the pill `enter` action is stop+paste+Enter. Because stop is
    /// now async (the Enter can't be sent inline by the command), this is set in
    /// `prepare_stop` and consumed in `finalize` once the paste has landed.
    pub(crate) pending_send_enter: bool,
    /// Held until lib.rs setup() takes it once to spawn the consumer thread.
    /// The matching sender lives inside the sidecar bridge.
    pub(crate) event_rx: Option<Receiver<EngineEvent>>,
    /// H10 fix (see docs/PERF_AUDIT_2026-07-15.md): last engine-config JSON
    /// successfully pushed to the sidecar. `prepare_start` re-syncs a running engine on every
    /// `start_dictation` (a respawned one gets the same pushes from `PreparedStart::send`);
    /// without this it would re-send this RPC under `runtime_lock` even when nothing changed. `None` until the first successful push, so
    /// the first start (and any change — the value is recomputed fresh each
    /// call, so a settings/license change naturally produces a different
    /// value) always pushes.
    pub(crate) last_sent_engine_config: Option<serde_json::Value>,
    /// H10 fix: last vocabulary (terms + sound-alike details) successfully
    /// pushed to the sidecar. Same skip-if-unchanged shape as
    /// `last_sent_engine_config`.
    pub(crate) last_sent_vocab: Option<(
        Vec<String>,
        Option<Vec<rightkit_dictation_core::engine::VocabTermWire>>,
    )>,
    /// NOTE: the `EngineInfo` cold-readiness handshake flag moved onto
    /// `SidecarHandle::info_ready` (shared `AtomicBool`) so the lock-free
    /// prewarm handshake can set it without this lock — see
    /// `SidecarHandle::warm_handshake` and runtime.rs's locking doc.
    /// Last sidecar generation whose process-local mirrors are known current.
    /// A cloned supervisor handle can restart the process without going through
    /// `shutdown_backend`, so generation changes must invalidate config/vocab caches.
    pub(crate) last_synced_sidecar_generation: u64,
    /// Session id of a recording whose finalize must go to encrypted history
    /// as `Cancelled` (raw/L0 transcript, no clipboard/paste/Enter/delivery)
    /// instead of a normal OS delivery. Session-keyed so a stale value can
    /// never affect a different session. Set by `prepare_cancel_to_history`
    /// and by the spoken "zephyr cancel" tail (`SidecarControlTail::Reset`)
    /// inside `accept_sidecar_transcript`.
    pub(crate) cancel_to_history_session: Option<String>,
    /// Draft-row throttle state for `TranscriptPartial` (crash-recovery draft
    /// history rows): `(session_id, last_written_revision, last_written_at)`.
    /// A new session always writes on its first partial (session_id
    /// mismatch); within a session, writes only when `revision` increased
    /// AND at least `DRAFT_UPSERT_MIN_INTERVAL` has elapsed since the last
    /// write, so a live dictation doesn't hammer the encrypted DB on every
    /// ASR partial tick.
    pub(crate) draft_throttle: Option<(String, u64, Instant)>,
}

impl EngineRuntime {
    pub fn clear_start_capture(&mut self) {
        {
            self.start_capture_rx = None;
            if let Some(ready) = self.start_dispatch_ready.take() {
                ready.store(false, std::sync::atomic::Ordering::Release);
            }
            self.start_target = None;
            self.start_focus_pin = None;
            self.start_content_fingerprint = None;
        }
        self.start_context = None;
    }
}

pub type StartCapture = (
    crate::delivery::TargetSnapshot,
    Option<crate::host::HostPin>,
    rightkit_dictation_core::engine::RecordingContext,
    Option<u64>,
);

/// Minimum spacing between draft-row writes for the same session (see
/// `draft_throttle`). ~2s per the crash-recovery design: the only
/// unrecoverable loss on a crash is the uncommitted tail since the last
/// write, so this bounds worst-case loss without writing on every partial.
pub const DRAFT_UPSERT_MIN_INTERVAL: Duration = Duration::from_secs(2);

/// Internally-synchronized handle to the supervised sidecar plus the
/// lock-free-readable mirrors of `EngineRuntime` state. `Clone` — the same
/// instance is published in `shared::ENGINE_SIDECAR` so status queries
/// (`get_engine_info`, `get_app_state`, watchdog probes) and the long prewarm
/// waits never queue behind the global `Mutex<RuntimeApp>`.
#[derive(Clone)]
pub struct SidecarHandle {
    pub(crate) supervisor: Arc<EngineSupervisor>,
    pub(crate) active_session: Arc<StdMutex<Option<String>>>,
    pub(crate) engine_version: Arc<StdMutex<Option<String>>>,
    /// Lock-free mirror of `EngineRuntime.state` — the same `SharedState` Arc.
    pub(crate) state: SharedState,
    /// `EngineInfo` cold-readiness handshake completed for the current sidecar
    /// generation. Shared so `warm_handshake()` can set it lock-free.
    pub(crate) info_ready: Arc<AtomicBool>,
}

impl SidecarHandle {
    /// Start the sidecar and wait out its ready frame WITHOUT the global
    /// runtime lock (`EngineSupervisor` is internally synchronized and
    /// `start()` is idempotent — returns "already-running" when warm).
    /// Returns true when this call actually spawned a fresh process.
    pub fn ensure_started(&self) -> Result<bool, String> {
        let version = self.supervisor.start()?;
        if let Ok(mut slot) = self.engine_version.lock() {
            if slot.is_none() || version != "already-running" {
                *slot = Some(version.clone());
            }
        }
        Ok(version != "already-running")
    }

    /// Lock-free half of `EngineRuntime::ensure_engine_ready`: process start +
    /// ready-wait + the up-to-`SIDECAR_ENGINE_INFO_TIMEOUT` EngineInfo
    /// handshake. Prewarm runs this on its own thread so the multi-second
    /// model load never holds `runtime_lock`; the short config/vocab snapshot
    /// push still runs under the lock afterwards
    /// (`sync_sidecar_runtime_snapshot_if_running`).
    pub fn warm_handshake(&self) -> Result<(), String> {
        if self.ensure_started()? {
            // Fresh process: any previously-observed readiness belongs to the
            // dead generation.
            self.info_ready.store(false, Ordering::Release);
        }
        if !self.info_ready.load(Ordering::Acquire) {
            self.request_engine_info()?;
            self.info_ready.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// The `EngineInfo` readiness handshake. Only needs the supervisor's
    /// internally-synchronized `call`, so it is safe off the runtime lock.
    pub fn request_engine_info(&self) -> Result<EngineInfoResult, String> {
        let request_id = format!("engine_info:{}", self.supervisor.next_request_id());
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::EngineInfo,
            None,
            Some(EnginePayload::EngineInfo {
                mode: "get_info".into(),
                cold_load_s: None,
                engine_version: None,
            }),
        );
        let response = self.supervisor.call(&frame, SIDECAR_ENGINE_INFO_TIMEOUT)?;
        if let Some(error) = response.error {
            return Err(error.message);
        }
        match response.payload {
            Some(EnginePayload::EngineInfo {
                mode,
                cold_load_s,
                engine_version,
            }) => Ok(EngineInfoResult {
                mode,
                cold_load_s,
                engine_version,
            }),
            _ => Err("unexpected sidecar engine info response".to_string()),
        }
    }

    /// Lock-free watchdog revocation (#149): build the cancel for `session_id`
    /// and wait (bounded) for the sidecar ack, restarting it on failure.
    pub fn revoke_session_acked(&self, session_id: &str) -> engine_supervisor::RevocationOutcome {
        let source = CancelSource::Watchdog;
        let request_id = format!(
            "cancel:{}:cancel:{}",
            self.supervisor.next_request_id(),
            source.as_str()
        );
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::TranscribingStarted,
            Some(session_id.to_string()),
            Some(EnginePayload::TranscribingStarted {
                stop_kind: Some(StopKind::Cancel),
                stop_target: None,
            }),
        );
        PreparedEngineCancel {
            session_id: session_id.to_string(),
            source,
            request_id,
            supervisor: Arc::clone(&self.supervisor),
            active_session: Arc::clone(&self.active_session),
            frame,
        }
        .revoke_acked(WATCHDOG_REVOCATION_ACK_TIMEOUT)
    }

    /// Lock-free sibling of `EngineRuntime::sidecar_is_transcribing_session` —
    /// the watchdog probe must not queue behind a held runtime lock.
    pub fn is_transcribing_session(&self, session_id: &str) -> Result<bool, String> {
        if !self.supervisor.is_running() {
            return Ok(false);
        }
        let request_id = format!("engine_state:{}", self.supervisor.next_request_id());
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::EngineStateRequest,
            None,
            Some(EnginePayload::EngineStateRequest),
        );
        let response = self.supervisor.call(&frame, SIDECAR_STATE_PROBE_TIMEOUT)?;
        if let Some(error) = response.error {
            return Err(error.message);
        }
        match response.payload {
            Some(EnginePayload::EngineStateSnapshot { state }) => {
                let matches = engine_state_snapshot_is_transcribing_session(&state, session_id);
                if !matches {
                    crate::telemetry::log_message(
                        "sidecar_state_probe_mismatch",
                        &format!("session={session_id}; sidecar_state={state}"),
                    );
                }
                Ok(matches)
            }
            _ => Err("unexpected sidecar state response".to_string()),
        }
    }
}

pub fn build_history_store() -> Result<Box<dyn HistoryStore>, String> {
    // The RIGHTKIT_HISTORY=json escape hatch is debug-only: a release build must
    // never let an env var downgrade encrypted history to plaintext JSON. In release,
    // json_only is driven solely by whether Pass 3 (encrypted history) is enabled.
    #[cfg(feature = "test-mode")]
    let env_json_only = std::env::var("RIGHTKIT_HISTORY")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    #[cfg(not(feature = "test-mode"))]
    let env_json_only = false;
    let json_only = env_json_only || !crate::host::host().encrypted_history_enabled();
    let mut store: Box<dyn HistoryStore> = if json_only {
        crate::host::host().open_json_history()?
    } else {
        match crate::host::host().open_encrypted_history() {
            Ok(store) => store,
            Err(err) => {
                crate::telemetry::log_message(
                    "history_sqlcipher_init_error",
                    &format!("{err}; refusing plaintext JSON fallback"),
                );
                return Err(format!(
                    "encrypted history init failed: {err}. \
                     Set RIGHTKIT_HISTORY=json only for explicit dev/debug fallback."
                ));
            }
        }
    };
    if let Some(cutoff_ms) =
        crate::settings::history_retention().cutoff_ms(crate::delivery::now_ms())
    {
        let mut filter = HistoryFilter {
            until_ms: Some(cutoff_ms),
            ..HistoryFilter::default()
        };
        store
            .delete_matching_scoped(
                &filter,
                rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
            )
            .map_err(|err| format!("startup dictation history prune: {err}"))?;
        filter.cancelled_only = true;
        store
            .delete_matching_scoped(
                &filter,
                rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
            )
            .map_err(|err| format!("startup cancelled history prune: {err}"))?;
    }
    Ok(store)
}

static HISTORY_STORAGE_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Open history independently from the global dictation runtime. Cloud sync
/// must never acquire the mutex used by recording, stop, cancel, or pill work.
pub fn open_history_store_for_background() -> Result<Box<dyn HistoryStore>, String> {
    build_history_store()
}

pub fn mark_background_history_changed() {
    HISTORY_STORAGE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
}

pub fn history_storage_generation() -> u64 {
    HISTORY_STORAGE_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// Pure sweep step: given an already-open store, find every `Draft` row and
/// flip it to `Cancelled`. A live recording never survives a process
/// restart, so any `Draft` present when this runs means the app died
/// mid-recording — this is the crash-recovery half of draft history rows.
/// Reuses the store's existing `delete` + `push` (no new trait method
/// needed) so this is independently testable against any `HistoryStore`
/// (Memory/File/SQLCipher) without a live engine. Returns the number of
/// drafts recovered.
pub fn recover_orphaned_drafts_in_store(store: &mut dyn HistoryStore) -> Result<usize, String> {
    let orphaned: Vec<DeliveryRecord> = store
        .recent()
        .iter()
        .filter(|record| matches!(record.outcome, crate::delivery::DeliveryOutcome::Draft))
        .filter(|record| {
            rightkit_transcript_library::history::MAC_DELETE_AUTHORITY
                .contains(&rightkit_transcript_library::history::record_origin(record))
        })
        .cloned()
        .collect();
    let mut recovered = 0usize;
    for draft in orphaned {
        let recovered_record = DeliveryRecord::new(
            rightkit_transcript_library::delivery::next_delivery_id(0),
            draft.transcript.clone(),
            rightkit_transcript_library::delivery::DeliveryOutcome::Cancelled,
            draft.target.clone(),
        )
        .with_raw(draft.raw_transcript.clone(), draft.recording_ms);
        let deleted = store
            .delete_scoped(
                &draft.delivery_id,
                rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
            )
            .map_err(|err| err.to_string())?;
        if deleted != rightkit_transcript_library::history::ScopedDeleteOutcome::Deleted {
            continue;
        }
        store
            .push(recovered_record)
            .map_err(|err| err.to_string())?;
        recovered += 1;
    }
    Ok(recovered)
}

/// Startup sweep — call once from `run()`'s `setup()`. Opens the production
/// history store directly (independent of the engine runtime's own lazily
/// loaded connection, which does not exist yet this early in boot) so
/// crash-orphaned drafts are cleaned up before any UI can observe them.
pub fn recover_orphaned_drafts() -> Result<usize, String> {
    let mut store = build_history_store()?;
    let recovered = recover_orphaned_drafts_in_store(store.as_mut())?;
    if recovered > 0 {
        crate::telemetry::log_message(
            "startup_orphaned_drafts_recovered",
            &format!("count={recovered}"),
        );
    }
    Ok(recovered)
}

pub const SIDECAR_RPC_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound on the sidecar's cancel ack before the watchdog restarts it (#149).
pub const WATCHDOG_REVOCATION_ACK_TIMEOUT: Duration = Duration::from_secs(3);
pub const SIDECAR_STATE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
pub const SIDECAR_ENGINE_INFO_TIMEOUT: Duration = Duration::from_secs(120);
pub const SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MIN_SECS: u64 = 30;
pub const SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MAX_SECS: u64 = 900;
