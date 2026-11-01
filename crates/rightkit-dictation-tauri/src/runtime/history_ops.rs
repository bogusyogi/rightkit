// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

/// How long the shell waits for the engine to acknowledge a file-transcription cancel.
const FILE_CANCEL_TIMEOUT: Duration = Duration::from_secs(3);

/// The request that stops the engine's file transcription started by `target_request_id`.
fn file_transcription_cancel_frame(
    request_id: &str,
    target_request_id: &str,
) -> rightkit_dictation_core::engine::EngineFrame {
    engine_supervisor::build_request(
        request_id,
        EngineSchemaName::FileTranscriptionCancel,
        None,
        Some(EnginePayload::FileTranscriptionCancel {
            request_id: target_request_id.to_string(),
        }),
    )
}

impl EngineRuntime {
    /// Read complete persisted history in bounded pages. `recent()` is an
    /// intentionally small UI cache, so sync/deletion authority must query
    /// backing store instead of dropping older transcripts.
    pub fn all_history_records(&self) -> Result<Vec<DeliveryRecord>, String> {
        let mut records = self.history_records_matching(&HistoryFilter::default())?;
        let cancelled_filter = HistoryFilter {
            cancelled_only: true,
            ..Default::default()
        };
        records.extend(self.history_records_matching(&cancelled_filter)?);
        records.sort_by_key(|a| std::cmp::Reverse(a.delivered_at_ms));
        records.dedup_by(|a, b| a.delivery_id == b.delivery_id);
        Ok(records)
    }

    pub fn history_records_matching(
        &self,
        filter: &HistoryFilter,
    ) -> Result<Vec<DeliveryRecord>, String> {
        let history = self
            .history
            .as_ref()
            .ok_or_else(|| "history store unavailable".to_string())?;
        let mut records = Vec::new();
        let mut offset = 0u32;
        loop {
            let page = history
                .query(&HistoryQuery {
                    filter: filter.clone(),
                    offset,
                    limit: rightkit_transcript_library::history::MAX_HISTORY_PAGE_SIZE,
                })
                .map_err(|err| err.to_string())?;
            let count = page.items.len() as u32;
            records.extend(page.items);
            if !page.has_more || count == 0 {
                break;
            }
            offset = offset
                .checked_add(count)
                .ok_or_else(|| "history pagination overflow".to_string())?;
        }
        Ok(records)
    }

    /// The "repaste last" fallback after the current target is deleted. Only
    /// this Mac's own dictations qualify: file transcriptions, synced rows
    /// from other devices and crash-recovery drafts never become the target
    /// (the same rule `transcribe_file` follows by not setting it).
    fn newest_repaste_candidate(&self) -> Option<DeliveryRecord> {
        self.recent_cache
            .iter()
            .find(|record| {
                matches!(
                    record.source,
                    None | Some(rightkit_transcript_library::delivery::DeliverySource::Dictation)
                ) && !matches!(
                    record.outcome,
                    rightkit_transcript_library::delivery::DeliveryOutcome::Draft
                )
            })
            .cloned()
    }

    pub fn mirror_sidecar_manual_delivery(&mut self, record: DeliveryRecord) -> Result<(), String> {
        self.last_delivery = Some(record.clone());
        self.ensure_history_loaded()?;
        if let Some(history) = self.history.as_mut() {
            history.push(record).map_err(|err| err.to_string())?;
            self.recent_cache = history.recent().to_vec();
        }
        Ok(())
    }

    pub fn dev_seed_history_record(&mut self, record: DeliveryRecord) -> Result<(), String> {
        self.mirror_sidecar_manual_delivery(record)?;
        if let Err(sync_err) = self.sync_sidecar_recent_history() {
            crate::telemetry::log_message("dev_control_seed_history_sync", &sync_err);
        }
        Ok(())
    }

    pub fn is_no_last_delivery_error(message: &str) -> bool {
        message.contains("no last delivery")
    }

    pub fn engine_info(&mut self) -> Result<EngineInfoResult, String> {
        if crate::settings::onboarding_complete() {
            self.ensure_engine_ready()?;
        }
        let engine_version = self
            .sidecar
            .engine_version
            .lock()
            .ok()
            .and_then(|slot| slot.clone());
        Ok(EngineInfoResult {
            mode: "sidecar".to_string(),
            cold_load_s: None,
            engine_version,
        })
    }

    pub fn prewarm(&mut self) -> Result<(), String> {
        self.ensure_engine_ready()
    }

    pub fn transcribe_file(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<rightkit_dictation_core::engine::FileTranscript, String> {
        self.ensure_engine_ready()?;
        let timeout = sidecar_file_transcribe_timeout(&path);
        let sidecar = &self.sidecar;
        let request_id = format!("transcribe_file:{}", sidecar.supervisor.next_request_id());
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::FileTranscriptionRequest,
            None,
            Some(EnginePayload::FileTranscriptionRequest {
                path: path.to_string_lossy().to_string(),
            }),
        );
        let response = match self.sidecar_call_with_timeout(frame, timeout) {
            Ok(response) => response,
            Err(error) => {
                // The wait ended (timeout or failure): tell the engine to drop the job instead
                // of decoding for a caller that has gone.
                self.cancel_sidecar_file_transcription(&request_id);
                return Err(error);
            }
        };
        let out = Self::file_transcript_from_frame(&response)?;
        // Persist the result to history (source-tagged) so file transcriptions are
        // recoverable like dictation. A history write failure must never fail
        // the transcription the user is waiting on. Does NOT
        // set last_delivery — a file transcript should not become the pill's "repaste
        // last" target.
        if !out.text.trim().is_empty() {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "audio file".to_string());
            let target = rightkit_transcript_library::delivery::TargetSnapshot {
                process_id: None,
                process_name: None,
                window_title: None,
                window_handle: None,
                focused_control_handle: None,
                foreground_target: None,
                focused_text_input: None,
                is_elevated: None,
                stop_continuity: None,
                exe_path: None,
            };
            let id = rightkit_transcript_library::delivery::next_delivery_id(0);
            let record = rightkit_transcript_library::delivery::DeliveryRecord::new(
                id,
                out.text.clone(),
                rightkit_transcript_library::delivery::DeliveryOutcome::CopiedFallback {
                    reason: rightkit_transcript_library::delivery::CopyFallbackReason::NoTextField,
                },
                target,
            )
            .with_source(
                rightkit_transcript_library::delivery::DeliverySource::FileTranscription {
                    file_name,
                },
            );
            if let Err(err) = self.push_history_record(record) {
                crate::telemetry::log_message("file_transcribe_history", &err);
            }
        }
        Ok(out)
    }

    /// Ask the running engine to stop the file transcription started by `target_request_id`.
    /// Best effort: never spawns an engine, and a failed cancel only costs the engine's own
    /// stall timeout.
    fn cancel_sidecar_file_transcription(&self, target_request_id: &str) {
        let supervisor = &self.sidecar.supervisor;
        if !supervisor.is_running() {
            return;
        }
        let frame = file_transcription_cancel_frame(
            &format!("cancel_file:{}", supervisor.next_request_id()),
            target_request_id,
        );
        if let Err(error) = supervisor.call(&frame, FILE_CANCEL_TIMEOUT) {
            crate::telemetry::log_message("file_transcribe_cancel", &error);
        }
    }

    /// Append a record to history WITHOUT touching `last_delivery` (used for file
    /// transcriptions, which aren't a "last delivery" the pill can re-paste).
    pub fn push_history_record(
        &mut self,
        record: rightkit_transcript_library::delivery::DeliveryRecord,
    ) -> Result<(), String> {
        self.ensure_history_loaded()?;
        if let Some(history) = self.history.as_mut() {
            history.push(record).map_err(|err| err.to_string())?;
            self.recent_cache = history.recent().to_vec();
        }
        crate::cloud_sync::notify_local_change();
        self.resync_sidecar_history_or_log();
        Ok(())
    }

    /// Resync the sidecar's mirrored recent-history view and, on failure,
    /// log it under the existing `sidecar_history_sync` telemetry key
    /// instead of surfacing an error (a stale sidecar mirror is not fatal
    /// to the caller's own operation).
    fn resync_sidecar_history_or_log(&mut self) {
        if let Err(sync_err) = self.sync_sidecar_recent_history() {
            crate::telemetry::log_message("sidecar_history_sync", &sync_err);
        }
    }

    pub fn start_wake_listen(&mut self, params: WakeListenParams) -> Result<(), String> {
        self.ensure_engine_ready()?;
        let sidecar = &self.sidecar;
        let request_id = format!("wake_start:{}", sidecar.supervisor.next_request_id());
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::WakeListenStarted,
            None,
            Some(EnginePayload::WakeListenStarted {
                status: "start".into(),
                model: params.model,
                threshold: params.threshold,
                fake: None,
            }),
        );
        let response = self.sidecar_call(frame)?;
        if let Some(error) = response.error {
            return Err(error.message);
        }
        Ok(())
    }

    pub fn stop_wake_listen(&mut self) -> Result<(), String> {
        if !self.sidecar.supervisor.is_running() {
            return Ok(());
        }
        let sidecar = &self.sidecar;
        let request_id = format!("wake_stop:{}", sidecar.supervisor.next_request_id());
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::WakeListenStopped,
            None,
            Some(EnginePayload::WakeListenStopped {
                status: "stop".into(),
                total_fires: None,
            }),
        );
        let response = self.sidecar_call(frame)?;
        if let Some(error) = response.error {
            return Err(error.message);
        }
        Ok(())
    }

    pub fn shutdown_backend(&mut self) {
        crate::media_control::restore_any("backend_shutdown");
        // Sol audit finding 5(c): a shutdown here (deferred-restart give-up,
        // quit-while-recording, app exit mid-dictation) drops the worker
        // thread and the user's in-flight audio/transcript with no terminal
        // event — the renderer pill has no way to know the session died. This
        // does not invent a new renderer event (the event contract is
        // fail-closed and owned elsewhere); it only makes a killed session
        // diagnosable from the log via the existing telemetry pattern.
        if let Some(session_id) = self.active_session.as_deref() {
            crate::telemetry::log_message(
                "sidecar_shutdown_active_session_killed",
                &format!(
                    "session={session_id} state={}",
                    sidecar_shutdown_state_label(&self.state.get())
                ),
            );
        }
        self.active_session = None;
        self.pending_send_enter = false;
        self.stop_target = None;
        self.stop_focus_pin = None;
        crate::host::host().release_pending_accessibility_observation();
        self.pending_stop_context.clear();
        self.sync_sidecar_active_session();
        let sidecar = &self.sidecar;
        sidecar.supervisor.shutdown();
        // These mirrors belong to the process that just stopped. A replacement
        // sidecar starts from default config/vocabulary and must receive the
        // full snapshot even when the shell-side values are unchanged.
        self.last_sent_engine_config = None;
        self.last_sent_vocab = None;
        self.sidecar.info_ready.store(false, Ordering::Release);
        self.last_synced_sidecar_generation = 0;
        if let Ok(mut slot) = sidecar.engine_version.lock() {
            *slot = None;
        }
        if let Ok(mut active) = sidecar.active_session.lock() {
            *active = None;
        }
    }

    pub fn delete_history_item(&mut self, delivery_id: &str) -> Result<bool, String> {
        self.ensure_history_loaded()?;
        let record = self
            .history
            .as_ref()
            .and_then(|history| history.find(delivery_id));
        let Some(record) = record else {
            return Ok(false);
        };
        if !rightkit_transcript_library::history::MAC_DELETE_AUTHORITY.contains(
            &rightkit_transcript_library::history::record_origin(&record),
        ) {
            return Err("transcript belongs to another device and is read-only here".to_string());
        }
        crate::cloud_sync::enqueue_deleted_ids(&[delivery_id.to_string()])?;
        let changed = if let Some(history) = self.history.as_mut() {
            let changed = history
                .delete_scoped(
                    delivery_id,
                    rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
                )
                .map_err(|err| err.to_string())?
                == rightkit_transcript_library::history::ScopedDeleteOutcome::Deleted;
            self.recent_cache = history.recent().to_vec();
            changed
        } else {
            false
        };
        if changed {
            if self
                .last_delivery
                .as_ref()
                .map(|record| record.delivery_id.as_str())
                == Some(delivery_id)
            {
                self.last_delivery = self.newest_repaste_candidate();
            }
            self.resync_sidecar_history_or_log();
            crate::cloud_sync::notify_local_change();
        }
        Ok(changed)
    }

    pub fn query_history(&mut self, query: &HistoryQuery) -> Result<HistoryPage, String> {
        self.ensure_history_loaded()?;
        self.history
            .as_ref()
            .ok_or_else(|| "history store unavailable".to_string())?
            .query(query)
            .map_err(|err| err.to_string())
    }

    pub fn delete_history_matching(&mut self, filter: &HistoryFilter) -> Result<usize, String> {
        self.ensure_history_loaded()?;
        // HR-HISTORY-006: the Mac may publish tombstones ONLY for Mac-origin
        // records. This previously collected every matching id and enqueued
        // the lot, so a filtered clear on the Mac would have published
        // CloudKit tombstones for iPhone- and Watch-origin rows and deleted
        // them from every participating device. Scope the enqueue to this
        // device's own authority.
        let deleted_ids = self
            .history_records_matching(filter)?
            .iter()
            .filter(|record| crate::history::record_matches(record, filter))
            .filter(|record| {
                rightkit_transcript_library::history::MAC_DELETE_AUTHORITY
                    .contains(&rightkit_transcript_library::history::record_origin(record))
            })
            .map(|record| record.delivery_id.clone())
            .collect::<Vec<_>>();
        crate::cloud_sync::enqueue_deleted_ids(&deleted_ids)?;
        // Same authority applied to the local delete itself, so the store
        // rejects a foreign-origin row even if a caller bypasses the UI.
        let deleted = self
            .history
            .as_mut()
            .ok_or_else(|| "history store unavailable".to_string())?
            .delete_matching_scoped(
                filter,
                rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
            )
            .map_err(|err| err.to_string())?;
        if deleted > 0 {
            self.recent_cache = self
                .history
                .as_ref()
                .map(|history| history.recent().to_vec())
                .unwrap_or_default();
            self.last_delivery = self.newest_repaste_candidate();
            self.resync_sidecar_history_or_log();
            crate::cloud_sync::notify_local_change();
        }
        Ok(deleted)
    }

    /// Shared tail of `prune_history_before`/`clear_history`: cloud-enqueue
    /// tombstones for `deleted_ids` (Mac only), delete each scoped to this
    /// device's authority, then refresh `recent_cache`/`last_delivery` and
    /// resync the sidecar only if something actually changed. Returns the
    /// count actually deleted.
    fn delete_ids_and_resync(&mut self, deleted_ids: Vec<String>) -> Result<usize, String> {
        crate::cloud_sync::enqueue_deleted_ids(&deleted_ids)?;
        let history = self
            .history
            .as_mut()
            .ok_or_else(|| "history store unavailable".to_string())?;
        let mut deleted = 0;
        for id in deleted_ids {
            if history
                .delete_scoped(
                    &id,
                    rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
                )
                .map_err(|err| err.to_string())?
                == rightkit_transcript_library::history::ScopedDeleteOutcome::Deleted
            {
                deleted += 1;
            }
        }
        if deleted > 0 {
            self.recent_cache = self
                .history
                .as_ref()
                .map(|history| history.recent().to_vec())
                .unwrap_or_default();
            self.last_delivery = self.newest_repaste_candidate();
            self.resync_sidecar_history_or_log();
            crate::cloud_sync::notify_local_change();
        }
        Ok(deleted)
    }

    pub fn rotate_history_key(&mut self) -> Result<(), String> {
        self.ensure_history_loaded()?;
        self.history
            .as_mut()
            .ok_or_else(|| "history store unavailable".to_string())?
            .rotate_encryption_key()
            .map_err(|err| err.to_string())
    }

    pub fn prune_history_before(&mut self, cutoff_ms: u64) -> Result<usize, String> {
        self.ensure_history_loaded()?;
        let deleted_ids = self
            .all_history_records()?
            .iter()
            .filter(|record| record.delivered_at_ms < cutoff_ms)
            .filter(|record| {
                rightkit_transcript_library::history::MAC_DELETE_AUTHORITY
                    .contains(&rightkit_transcript_library::history::record_origin(record))
            })
            .map(|record| record.delivery_id.clone())
            .collect::<Vec<_>>();
        self.delete_ids_and_resync(deleted_ids)
    }

    pub fn update_history_item(
        &mut self,
        delivery_id: &str,
        transcript: String,
    ) -> Result<bool, String> {
        self.ensure_history_loaded()?;
        // WS4 hook: read the OLD transcript under the same
        // single history lock so the diff-then-write race is impossible.
        // `find` clones the record, so the borrow checker accepts the next
        // `update_transcript` call without dropping the read.
        let old_record = self
            .history
            .as_ref()
            .and_then(|history| history.find(delivery_id));
        if let Some(record) = old_record.as_ref() {
            if !rightkit_transcript_library::history::MAC_DELETE_AUTHORITY
                .contains(&rightkit_transcript_library::history::record_origin(record))
            {
                return Err(
                    "transcript belongs to another device and is read-only here".to_string()
                );
            }
        }
        let old_transcript = old_record.map(|record| record.transcript);
        let changed = if let Some(history) = self.history.as_mut() {
            let changed = history
                .update_transcript_scoped(
                    delivery_id,
                    transcript,
                    rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
                )
                .map_err(|err| err.to_string())?
                == rightkit_transcript_library::history::ScopedUpdateOutcome::Updated;
            self.recent_cache = history.recent().to_vec();
            changed
        } else {
            false
        };
        if changed {
            if self
                .last_delivery
                .as_ref()
                .map(|record| record.delivery_id.as_str())
                == Some(delivery_id)
            {
                self.last_delivery = self
                    .history
                    .as_ref()
                    .and_then(|history| history.find(delivery_id));
            }
            // WS4 hook: compare the freshly written transcript against the
            // pre-edit capture and feed the suggestion extractor.
            if let Some(old) = old_transcript {
                // Read the store, not `recent_cache`: rows older than the
                // capped cache are editable too.
                let new_transcript = self
                    .history
                    .as_ref()
                    .and_then(|history| history.find(delivery_id))
                    .map(|record| record.transcript);
                if let Some(new_text) = new_transcript {
                    let candidates =
                        rightkit_text_polish::vocab_learn::suggestion_candidates(&old, &new_text);
                    if !candidates.is_empty() {
                        if let Err(err) = crate::vocabulary::suggest_all(candidates) {
                            crate::telemetry::log_message("vocab_suggest_persist", &err);
                        }
                    }
                }
            }
            self.resync_sidecar_history_or_log();
            crate::cloud_sync::notify_local_change();
        }
        Ok(changed)
    }

    pub fn clear_history(&mut self) -> Result<(), String> {
        self.ensure_history_loaded()?;
        let deleted_ids = self
            .all_history_records()?
            .iter()
            .filter(|record| {
                rightkit_transcript_library::history::MAC_DELETE_AUTHORITY
                    .contains(&rightkit_transcript_library::history::record_origin(record))
            })
            .map(|record| record.delivery_id.clone())
            .collect::<Vec<_>>();
        crate::cloud_sync::enqueue_deleted_ids(&deleted_ids)?;
        let history = self
            .history
            .as_mut()
            .ok_or_else(|| "history store unavailable".to_string())?;
        let had_deleted = !deleted_ids.is_empty();
        for id in deleted_ids {
            history
                .delete_scoped(
                    &id,
                    rightkit_transcript_library::history::MAC_DELETE_AUTHORITY,
                )
                .map_err(|err| err.to_string())?;
        }
        self.recent_cache = history.recent().to_vec();
        self.last_delivery = self.newest_repaste_candidate();
        self.resync_sidecar_history_or_log();
        if had_deleted {
            crate::cloud_sync::notify_local_change();
        }
        Ok(())
    }
}

/// Log-safe variant label for `shutdown_backend`'s terminal-marker log —
/// deliberately NOT the `Debug` impl. `Pasting`, `Pasted`, and
/// `CopiedFallback` all embed the raw dictated `transcript` text, and local
/// telemetry logs are still a forensics/support-bundle target, so only the
/// variant name is logged here.
fn sidecar_shutdown_state_label(state: &AppState) -> &'static str {
    match state {
        AppState::Armed => "armed",
        AppState::Recording { .. } => "recording",
        AppState::Transcribing { .. } => "transcribing",
        AppState::Pasting { .. } => "pasting",
        AppState::Pasted { .. } => "pasted",
        AppState::PasteSentUnconfirmed { .. } => "paste_sent_unconfirmed",
        AppState::CopiedFallback { .. } => "copied_fallback",
        AppState::Cancelled { .. } => "cancelled",
        AppState::Error { .. } => "error",
    }
}
