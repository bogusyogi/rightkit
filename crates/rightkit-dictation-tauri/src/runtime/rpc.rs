// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

impl EngineRuntime {
    /// The engine config as the engine should hold it right now, derived fresh in native code.
    fn current_engine_config_value(&self) -> Result<serde_json::Value, String> {
        // Derive Pro from the native licence owner's verified signed lifecycle
        // (NativeEngineEntitlements, forgery-resistant; see license::has_pro).
        // The pushed flag below is derived here, in native code, on every sync: no renderer value,
        // settings file or environment variable can authorise a paid action. There is no
        // force-Pro switch in any build: a journey that needs Pro seeds a signed licence instead.
        let is_pro = crate::license::engine_is_pro();
        let mut cfg = crate::settings::snapshot();
        cfg.is_pro = Some(is_pro);
        // Whisper is Pro: the engine is never pushed a Whisper backend without it. The stored
        // setting stays, so Whisper returns with Pro.
        if let Some(raw) = cfg.asr_backend.as_deref() {
            let effective = crate::settings::effective_asr_backend(raw, is_pro);
            if effective != raw {
                cfg.asr_backend = Some(effective);
            }
        }
        // The polish policy rides the pushed config instead of the spawn environment (release
        // switch rule). Inserted unconditionally so a stale or hand-edited `l3_policy` in
        // settings.json can never ride along; `snapshot()` is a clone, nothing is persisted.
        cfg.extras.insert(
            "l3_policy".to_string(),
            rightkit_dictation_app_runtime::engine_supervisor::l3_policy_value(),
        );
        // Locking to a specific language (→ Whisper) is Pro. Free users
        // always get Parakeet auto-detect, regardless of a patched value.
        if !is_pro {
            cfg.dictation_language = Some("auto".to_string());
        }
        if rightkit_dictation_models::shared::is_load_check() {
            cfg.dictation_language = Some("auto".to_string());
            cfg.asr_backend = Some("parakeet-tdt".to_string());
        }
        serde_json::to_value(cfg).map_err(|err| err.to_string())
    }

    fn engine_config_frame(&self, config: serde_json::Value) -> EngineFrame {
        let request_id = format!(
            "replace_engine_config:{}",
            self.sidecar.supervisor.next_request_id()
        );
        engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::ReplaceEngineConfig,
            None,
            Some(EnginePayload::ReplaceEngineConfig { config }),
        )
    }

    pub fn sync_sidecar_engine_config(&mut self) -> Result<(), String> {
        let config_value = self.current_engine_config_value()?;
        // H10 fix (see docs/PERF_AUDIT_2026-07-15.md): without this check, every
        // `start_dictation` would re-push this RPC under `runtime_lock` even when
        // nothing changed since the last push. `config_value` is recomputed fresh above, so
        // any real settings/license change still produces a different value
        // and still pushes — this only skips the byte-identical resend.
        if self.last_sent_engine_config.as_ref() == Some(&config_value) {
            return Ok(());
        }
        let frame = self.engine_config_frame(config_value.clone());
        let response = self.sidecar_call(frame)?;
        Self::ack_from_frame(&response)?;
        self.last_sent_engine_config = Some(config_value);
        Ok(())
    }

    /// The vocabulary as the engine should hold it right now (terms, and sound-alike details when
    /// any term has them).
    fn current_vocabulary() -> (
        Vec<String>,
        Option<Vec<rightkit_dictation_core::engine::VocabTermWire>>,
    ) {
        let entries = crate::vocabulary::list();
        let terms = entries
            .iter()
            .map(|entry| entry.term.clone())
            .collect::<Vec<_>>();
        // Carry sound-alike aliases on the parallel `term_details` wire so
        // the engine mirror can render the context-gated correction block.
        // An old engine that ignores the unknown field still works from
        // `terms`; a new engine that receives no `term_details` falls back
        // to the bare list.
        let term_details = if entries.iter().any(|entry| !entry.sounds_like.is_empty()) {
            Some(
                entries
                    .into_iter()
                    .map(|entry| rightkit_dictation_core::engine::VocabTermWire {
                        term: entry.term,
                        sounds_like: entry.sounds_like,
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        (terms, term_details)
    }

    fn vocabulary_frame(
        &self,
        terms: Vec<String>,
        term_details: Option<Vec<rightkit_dictation_core::engine::VocabTermWire>>,
    ) -> EngineFrame {
        let request_id = format!(
            "replace_vocabulary:{}",
            self.sidecar.supervisor.next_request_id()
        );
        engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::ReplaceVocabulary,
            None,
            Some(EnginePayload::ReplaceVocabulary {
                terms,
                term_details,
            }),
        )
    }

    /// The config and vocabulary pushes a freshly spawned engine needs before it records: it
    /// holds nothing until they arrive, so it would run as Free with no vocabulary and AI polish
    /// off. Built from the current values, in the order they must be sent.
    pub fn sidecar_snapshot_frames(&self) -> Result<Vec<EngineFrame>, String> {
        let config = self.current_engine_config_value()?;
        let (terms, term_details) = Self::current_vocabulary();
        Ok(vec![
            self.engine_config_frame(config),
            self.vocabulary_frame(terms, term_details),
        ])
    }

    pub fn sync_sidecar_vocabulary(&mut self) -> Result<(), String> {
        let (terms, term_details) = Self::current_vocabulary();
        // H10 fix: same skip-if-unchanged shape as the engine-config push
        // above. Vocab edits (add/remove/import) still push immediately —
        // they change `terms`/`term_details`, so the cache always misses.
        if self.last_sent_vocab.as_ref() == Some(&(terms.clone(), term_details.clone())) {
            return Ok(());
        }
        let frame = self.vocabulary_frame(terms.clone(), term_details.clone());
        let response = self.sidecar_call(frame)?;
        Self::ack_from_frame(&response)?;
        self.last_sent_vocab = Some((terms, term_details));
        Ok(())
    }

    pub fn sync_sidecar_recent_history(&mut self) -> Result<(), String> {
        self.ensure_history_loaded()?;
        let records = self.recent_cache.clone();
        let sidecar = &self.sidecar;
        let request_id = format!(
            "replace_recent_history:{}",
            sidecar.supervisor.next_request_id()
        );
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::ReplaceRecentHistory,
            None,
            Some(EnginePayload::ReplaceRecentHistory {
                records: serde_json::to_value(records).map_err(|err| err.to_string())?,
            }),
        );
        let response = self.sidecar_call(frame)?;
        if let Some(error) = &response.error {
            return Err(error.message.clone());
        }
        Ok(())
    }

    pub fn sidecar_info_request(&self) -> Result<EngineInfoResult, String> {
        // Frame/RPC lives on `SidecarHandle` so the lock-free prewarm
        // handshake (`warm_handshake`) can run the same request without the
        // global runtime lock.
        self.sidecar.request_engine_info()
    }

    pub fn sidecar_repaste_last(&self) -> Result<DeliveryRecord, String> {
        let sidecar = &self.sidecar;
        let request_id = format!("repaste_last:{}", sidecar.supervisor.next_request_id());
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::RepasteLastRequest,
            None,
            Some(EnginePayload::RepasteLastRequest),
        );
        let response = self.sidecar_call(frame)?;
        Self::delivery_record_from_frame(&response)
    }

    pub fn sidecar_copy_last(&self) -> Result<String, String> {
        let sidecar = &self.sidecar;
        let request_id = format!("copy_last:{}", sidecar.supervisor.next_request_id());
        let frame = engine_supervisor::build_request(
            &request_id,
            EngineSchemaName::CopyLastRequest,
            None,
            Some(EnginePayload::CopyLastRequest),
        );
        let response = self.sidecar_call(frame)?;
        Self::transcript_text_from_frame(&response)
    }

    pub fn sidecar_is_transcribing_session(&self, session_id: &str) -> Result<bool, String> {
        // Same RPC as `SidecarHandle::is_transcribing_session` (the watchdog's
        // lock-free probe); kept as a delegate so the locked path and the
        // lock-free path can never drift.
        self.sidecar.is_transcribing_session(session_id)
    }

    pub fn sidecar_call(
        &self,
        frame: rightkit_dictation_core::engine::EngineFrame,
    ) -> Result<rightkit_dictation_core::engine::EngineFrame, String> {
        self.sidecar_call_with_timeout(frame, SIDECAR_RPC_TIMEOUT)
    }

    pub fn sidecar_call_with_timeout(
        &self,
        frame: rightkit_dictation_core::engine::EngineFrame,
        timeout: Duration,
    ) -> Result<rightkit_dictation_core::engine::EngineFrame, String> {
        let sidecar = &self.sidecar;
        sidecar.supervisor.call(&frame, timeout)
    }

    pub fn delivery_record_from_frame(
        frame: &rightkit_dictation_core::engine::EngineFrame,
    ) -> Result<DeliveryRecord, String> {
        if let Some(error) = &frame.error {
            return Err(error.message.clone());
        }
        match frame.payload.as_ref() {
            Some(EnginePayload::ManualDeliveryResult { record }) => {
                serde_json::from_value(record.clone()).map_err(|err| err.to_string())
            }
            _ => Err("unexpected sidecar response payload".to_string()),
        }
    }

    pub fn transcript_text_from_frame(
        frame: &rightkit_dictation_core::engine::EngineFrame,
    ) -> Result<String, String> {
        if let Some(error) = &frame.error {
            return Err(error.message.clone());
        }
        match frame.payload.as_ref() {
            Some(EnginePayload::CopyLastResult { text }) => Ok(text.clone()),
            _ => Err("unexpected sidecar response payload".to_string()),
        }
    }

    pub fn ack_from_frame(
        frame: &rightkit_dictation_core::engine::EngineFrame,
    ) -> Result<(), String> {
        if let Some(error) = &frame.error {
            return Err(error.message.clone());
        }
        match frame.payload.as_ref() {
            Some(EnginePayload::EngineAck { status, .. }) if status == "ok" => Ok(()),
            _ => Err("unexpected sidecar ack payload".to_string()),
        }
    }

    pub fn file_transcript_from_frame(
        frame: &rightkit_dictation_core::engine::EngineFrame,
    ) -> Result<rightkit_dictation_core::engine::FileTranscript, String> {
        if let Some(error) = &frame.error {
            return Err(error.message.clone());
        }
        match frame.payload.as_ref() {
            Some(EnginePayload::FileTranscriptionResult {
                text,
                srt,
                vtt,
                words,
            }) => Ok(rightkit_dictation_core::engine::FileTranscript {
                text: text.clone(),
                srt: srt.clone(),
                vtt: vtt.clone(),
                words: words.clone(),
            }),
            _ => Err("unexpected sidecar file transcript payload".to_string()),
        }
    }
}
