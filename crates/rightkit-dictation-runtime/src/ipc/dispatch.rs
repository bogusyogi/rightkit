#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{events::*, server::*};

pub(crate) fn handle(
    runtime: &Arc<Mutex<EngineRuntime>>,
    writer: &Arc<Mutex<io::Stdout>>,
    worker_pump_started: &Arc<AtomicBool>,
    request_id: &str,
    req: Request,
) -> EngineFrame {
    let trace_id = next_id();
    match req {
        Request::Health => {
            let l3 = crate::l3_cleanup::health();
            let status = if l3.circuit_open || l3.failures > 0 || l3.local_fallbacks > 0 {
                rightkit_dictation_core::engine::EngineHealthStatus::Degraded
            } else {
                rightkit_dictation_core::engine::EngineHealthStatus::Ok
            };
            EngineFrame::base(
                EngineSchemaName::EngineHealth,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::Health {
                    status,
                    diagnostics: Some(json!({
                        "l3_cleanup": {
                            "circuit_open": l3.circuit_open,
                            "groq_circuit_open": l3.groq_circuit_open,
                            "nvidia_circuit_open": l3.nvidia_circuit_open,
                            "openrouter_circuit_open": l3.openrouter_circuit_open,
                            "consecutive_failures": l3.consecutive_failures,
                            "attempts": l3.attempts,
                            "successes": l3.successes,
                            "failures": l3.failures,
                            "skips": l3.skips,
                            "local_fallbacks": l3.local_fallbacks,
                            "circuit_opens": l3.circuit_opens
                        }
                    })),
                }),
                None,
            )
        }
        Request::Capabilities => EngineFrame::base(
            EngineSchemaName::EngineCapabilities,
            request_id,
            None,
            &trace_id,
            Some(EnginePayload::Capabilities {
                capabilities: {
                    let mut capabilities: Vec<String> = vec![
                        "dictation".into(),
                        "final_only".into(),
                        "focus_tracking".into(),
                        "delivery_precedence".into(),
                    ];
                    // ENG-07: advertised only under `RIGHTKIT_L3_LIVE_RECONFIGURE=1` (test-mode).
                    if crate::perf_levers::l3_live_reconfigure() {
                        capabilities.push(
                            rightkit_dictation_core::engine::CAPABILITY_LIVE_L3_RECONFIGURE.into(),
                        );
                    }
                    capabilities
                },
            }),
            None,
        ),
        Request::Info => {
            let mut runtime_guard = runtime.lock();
            if let Err(message) = runtime_guard.ensure_worker() {
                return error_frame(request_id, &trace_id, None, &message);
            }
            drop(runtime_guard);
            if let Err(message) = ensure_worker_event_pump(runtime, writer, worker_pump_started) {
                return error_frame(request_id, &trace_id, None, &message);
            }
            let info = runtime.lock().engine_info();
            EngineFrame::base(
                EngineSchemaName::EngineInfo,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::EngineInfo {
                    mode: info.mode,
                    cold_load_s: info.cold_load_s,
                    engine_version: info.engine_version,
                }),
                None,
            )
        }
        Request::ReplaceEngineConfig { config } => {
            crate::settings::replace_runtime_config(super::l3_live::apply_pushed_config(config));
            crate::perf_levers::prewarm_app_index_if_pro();
            // Accept configuration and queue model preparation when idle.
            // The shell holds its runtime mutex across this ACK: waiting for
            // model preparation here would still block its next StartDictation
            // even though the capture worker reloads asynchronously. Recording
            // owns a separate reload barrier before inference/final decode.
            let warm: Option<crate::worker::WorkerHandle> = {
                let mut r = runtime.lock();
                if r.is_idle() {
                    r.worker_handle().ok()
                } else {
                    None
                }
            };
            if let Some(handle) = warm {
                if let Err(message) =
                    handle.send(crate::worker::WorkerCmd::ReloadModel { reply: None })
                {
                    return error_frame(request_id, &trace_id, None, &message);
                }
            }
            EngineFrame::base(
                EngineSchemaName::EngineAck,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::EngineAck {
                    status: "ok".into(),
                    detail: Some("engine_config_replaced".into()),
                }),
                None,
            )
        }
        Request::SetProviderKeys { frame } => {
            // The owner's hand-off snapshot: a full replacement, previous secrets wiped on
            // drop. Nothing about the values is logged; a frame that does not decode leaves
            // the store as it was and is rejected without echoing it.
            match rightkit_dictation_text_pipeline::provider_keys::apply_frame(&frame) {
                Ok((ack, failed)) => {
                    if !failed.is_empty() {
                        let names: Vec<&str> =
                            failed.iter().map(|(name, _)| name.as_str()).collect();
                        tracing::warn!(?names, "owner could not read provider keys; wiped");
                    }
                    EngineFrame::base(
                        EngineSchemaName::EngineAck,
                        request_id,
                        None,
                        &trace_id,
                        Some(EnginePayload::EngineAck {
                            status: "ok".into(),
                            // The owner's Ack frame (hex), checked by the spawner.
                            detail: Some(ack),
                        }),
                        None,
                    )
                }
                Err(_) => error_frame(
                    request_id,
                    &trace_id,
                    None,
                    "provider key hand-off rejected",
                ),
            }
        }
        Request::ReplaceVocabulary {
            terms,
            term_details,
        } => {
            // Prefer `term_details` (carries sounds_like aliases) when the
            // shell sends it; fall back to bare `terms` so an old shell still
            // works. The mirror keeps the string-only `terms()` accessor for
            // ASR bias / casing restore while exposing sound-alike pairs for
            // the LLM prompt block.
            let count = if let Some(details) = term_details.as_ref() {
                if !details.is_empty() {
                    let pairs: Vec<(String, Vec<String>)> = details
                        .iter()
                        .map(|detail| (detail.term.clone(), detail.sounds_like.clone()))
                        .collect();
                    let bare: Vec<String> = pairs.iter().map(|(term, _)| term.clone()).collect();
                    crate::vocabulary::replace_terms_with_aliases(bare, pairs);
                    details.len()
                } else {
                    crate::vocabulary::replace_terms(terms.clone());
                    terms.len()
                }
            } else {
                crate::vocabulary::replace_terms(terms.clone());
                terms.len()
            };
            EngineFrame::base(
                EngineSchemaName::EngineAck,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::EngineAck {
                    status: "ok".into(),
                    detail: Some(format!("vocabulary_terms={count}")),
                }),
                None,
            )
        }
        Request::StartDictation {
            session_id,
            recording_target,
            recording_context,
            last_pasted,
        } => {
            // Get the already-warm worker handle under a brief lock. Model
            // changes are proactively warmed by ReplaceEngineConfig, and the
            // worker keeps the authoritative reload-if-changed fallback. Do not
            // add a second synchronous reload round trip to this hotkey path.
            let handle = match runtime.lock().worker_handle() {
                Ok(h) => h,
                Err(message) => {
                    return error_frame(request_id, &trace_id, Some(&session_id), &message)
                }
            };
            // Flip state under a BRIEF lock. `begin_recording` rejects instead
            // of overwriting when a previous session is still recording or
            // finalizing. Queue Start immediately after that transition. The
            // worker resumes capture first, then overlaps its AX/UIA prefetch
            // with seed capture; doing a synchronous snapshot here delayed and
            // duplicated that exact worker path.
            // `Ok(None)` is the idempotent same-session re-entry — nothing
            // further to do (the dispatch already ran the first time).
            //
            //
            // The shell's last paste is recorded under the same brief lock once the session is
            // accepted, so "delete that" and the casing commands check the text it delivered. A
            // rejected start leaves it alone. It is not pushed to `recent`: the shell owns history.
            let begun = {
                let mut r = runtime.lock();
                let begun = r.begin_recording(&session_id, recording_target, recording_context);
                if begun.is_ok() {
                    if let Some(record) = last_pasted {
                        r.set_last_delivery(record);
                    }
                }
                begun
            };
            match begun {
                Ok(Some(_focus)) => {
                    let _ = handle.send(crate::worker::WorkerCmd::StartRecording {
                        session_id: session_id.clone(),
                    });
                }
                Ok(None) => {}
                Err(message) => {
                    return error_frame(request_id, &trace_id, Some(&session_id), &message)
                }
            }
            EngineFrame::base(
                EngineSchemaName::RecordingStarted,
                request_id,
                Some(session_id.clone()),
                &trace_id,
                Some(EnginePayload::RecordingStarted {
                    session_id,
                    last_pasted: None,
                }),
                None,
            )
        }
        Request::UpdateRecordingContext {
            session_id,
            context,
            recording_target,
        } => {
            let accepted =
                runtime
                    .lock()
                    .set_recording_context(&session_id, recording_target, context);
            EngineFrame::base(
                EngineSchemaName::EngineAck,
                request_id,
                Some(session_id),
                &trace_id,
                Some(EnginePayload::EngineAck {
                    status: if accepted { "ok" } else { "stale" }.into(),
                    detail: None,
                }),
                None,
            )
        }
        Request::TranscribeFile { path } => {
            // Clone the worker handle under a brief lock, then run the
            // (multi-second) transcription OUTSIDE the runtime lock so it doesn't
            // block prewarm / ptt / settings calls on the mutex.
            let handle = match runtime.lock().worker_handle() {
                Ok(h) => h,
                Err(message) => return error_frame(request_id, &trace_id, None, &message),
            };
            // The shell cancels by request id when its wait ends. The job is the app's own, so
            // it goes ahead of queued peer jobs.
            let pending = PendingFileTranscription::register(request_id);
            match handle.transcribe_file_own(path, pending.cancel_flag()) {
                Ok(transcript) => EngineFrame::base(
                    EngineSchemaName::FileTranscriptionResult,
                    request_id,
                    None,
                    &trace_id,
                    Some(EnginePayload::FileTranscriptionResult {
                        text: transcript.text,
                        srt: transcript.srt,
                        vtt: transcript.vtt,
                        words: transcript.words,
                    }),
                    None,
                ),
                Err(message) => error_frame(request_id, &trace_id, None, &message),
            }
        }
        Request::CancelFileTranscription { request_id: target } => {
            let found = cancel_file_transcription(&target);
            EngineFrame::base(
                EngineSchemaName::EngineAck,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::EngineAck {
                    status: "ok".into(),
                    detail: Some(if found { "cancelled" } else { "not_running" }.into()),
                }),
                None,
            )
        }
        Request::StopDictation {
            session_id,
            send_enter,
            local_only,
        } => {
            let mut r = runtime.lock();
            match r.begin_stop(&session_id, send_enter, local_only) {
                Ok(()) => EngineFrame::base(
                    EngineSchemaName::TranscribingStarted,
                    request_id,
                    Some(session_id),
                    &trace_id,
                    Some(EnginePayload::TranscribingStarted {
                        stop_kind: Some(if local_only {
                            rightkit_dictation_core::engine::StopKind::CancelToHistory
                        } else if send_enter {
                            rightkit_dictation_core::engine::StopKind::SendEnter
                        } else {
                            rightkit_dictation_core::engine::StopKind::Stop
                        }),
                        stop_target: None,
                    }),
                    None,
                ),
                Err(message) => error_frame(request_id, &trace_id, Some(&session_id), &message),
            }
        }
        Request::CancelDictation { session_id } => {
            let source = cancel_source_from_request_id(request_id);
            tracing::info!(
                target: "cancel_lifecycle",
                phase = "engine_received",
                source,
                session_id,
                request_id,
                "external cancel received"
            );
            runtime
                .lock()
                .cancel_traced(&session_id, request_id, source);
            EngineFrame::base(
                EngineSchemaName::TranscribingStarted,
                request_id,
                Some(session_id),
                &trace_id,
                Some(EnginePayload::TranscribingStarted {
                    stop_kind: Some(rightkit_dictation_core::engine::StopKind::Cancel),
                    stop_target: None,
                }),
                None,
            )
        }
        Request::RepasteLast => {
            let mut r = runtime.lock();
            match r.repaste_last() {
                Ok(record) => EngineFrame::base(
                    EngineSchemaName::ManualDeliveryResult,
                    request_id,
                    None,
                    &trace_id,
                    Some(EnginePayload::ManualDeliveryResult {
                        record: serde_json::to_value(record).unwrap_or(Value::Null),
                    }),
                    None,
                ),
                Err(message) => error_frame(request_id, &trace_id, None, &message),
            }
        }
        Request::CopyLast => {
            let mut r = runtime.lock();
            match r.copy_last() {
                Ok(transcript) => EngineFrame::base(
                    EngineSchemaName::CopyLastResult,
                    request_id,
                    None,
                    &trace_id,
                    Some(EnginePayload::CopyLastResult { text: transcript }),
                    None,
                ),
                Err(message) => error_frame(request_id, &trace_id, None, &message),
            }
        }
        Request::GetState => {
            let state = runtime.lock().state().clone();
            let payload = EnginePayload::EngineStateSnapshot {
                state: format!("{:?}", state),
            };
            EngineFrame::base(
                EngineSchemaName::EngineStateSnapshot,
                request_id,
                None,
                &trace_id,
                Some(payload),
                None,
            )
        }
        Request::GetRecentHistory { limit } => {
            let records = runtime.lock().recent_history(limit);
            let payload = EnginePayload::RecentHistoryResult {
                records: serde_json::to_value(records).unwrap_or(Value::Null),
            };
            EngineFrame::base(
                EngineSchemaName::RecentHistoryResult,
                request_id,
                None,
                &trace_id,
                Some(payload),
                None,
            )
        }
        Request::ReplaceRecentHistory { records } => {
            let count = records.len();
            runtime.lock().replace_recent_history(records);
            EngineFrame::base(
                EngineSchemaName::EngineAck,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::EngineAck {
                    status: "ok".into(),
                    detail: Some(format!("records={count}")),
                }),
                None,
            )
        }
        Request::StartWakeListen { model, threshold } => {
            if !crate::settings::wake_lab_marker_present() {
                return error_frame(request_id, &trace_id, None, "Wake Lab marker is absent");
            }
            let threshold = threshold.unwrap_or(0.25).clamp(0.01, 0.99);
            let handle = match runtime.lock().worker_handle() {
                Ok(handle) => handle,
                Err(message) => return error_frame(request_id, &trace_id, None, &message),
            };
            if let Err(message) = handle.start_wake_listen(threshold) {
                return error_frame(request_id, &trace_id, None, &message);
            }
            EngineFrame::base(
                EngineSchemaName::WakeListenStarted,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::WakeListenStarted {
                    status: "started".into(),
                    model: model.or_else(|| Some("kws-hey-zephyr".into())),
                    threshold: Some(threshold),
                    fake: None,
                }),
                None,
            )
        }
        Request::StopWakeListen => {
            let total_fires = match runtime
                .lock()
                .worker_handle()
                .and_then(|handle| handle.stop_wake_listen())
            {
                Ok(total_fires) => total_fires,
                Err(message) => return error_frame(request_id, &trace_id, None, &message),
            };
            EngineFrame::base(
                EngineSchemaName::WakeListenStopped,
                request_id,
                None,
                &trace_id,
                Some(EnginePayload::WakeListenStopped {
                    status: "stopped".into(),
                    total_fires: Some(total_fires),
                }),
                None,
            )
        }
        Request::ReconfigureL3 {
            ai_polish_enabled,
            provider_state,
            provider_ready,
        } => {
            match super::l3_live::reconfigure(ai_polish_enabled, &provider_state, provider_ready) {
                Ok(()) => EngineFrame::base(
                    EngineSchemaName::EngineAck,
                    request_id,
                    None,
                    &trace_id,
                    Some(EnginePayload::EngineAck {
                        status: "ok".into(),
                        detail: Some("l3_reconfigured".into()),
                    }),
                    None,
                ),
                Err(message) => error_frame(request_id, &trace_id, None, &message),
            }
        }
        Request::Shutdown => EngineFrame::health(request_id, &trace_id),
    }
}

pub(crate) fn ensure_worker_event_pump(
    runtime: &Arc<Mutex<EngineRuntime>>,
    writer: &Arc<Mutex<io::Stdout>>,
    started: &Arc<AtomicBool>,
) -> Result<(), String> {
    if started
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }
    if let Err(error) = spawn_worker_event_pump(runtime.clone(), writer.clone()) {
        started.store(false, Ordering::SeqCst);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn error_frame(
    request_id: &str,
    trace_id: &str,
    session_id: Option<&str>,
    message: &str,
) -> EngineFrame {
    EngineFrame::base(
        EngineSchemaName::EngineError,
        request_id,
        session_id.map(str::to_string),
        trace_id,
        None,
        Some(EngineErrorPayload {
            code: "E_ENGINE".into(),
            message: message.to_string(),
            retryable: false,
            diagnostics: None,
        }),
    )
}

/// Wire name of a schema: the `method` of its request envelope.
pub(crate) fn schema_method(schema: &EngineSchemaName) -> Option<String> {
    serde_json::to_value(schema)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// Identity of this engine process, sent in `hello` and on every event envelope (`generation`).
/// The shell fences on it: an event whose instance is not the current process's is stale.
pub(crate) fn engine_instance() -> &'static str {
    static INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANCE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("{:x}-{:x}", std::process::id(), nanos)
    })
}

/// Write one frame: encode, write and flush all happen while the writer's lock is held, so
/// frames from the request loop, handler threads and the worker event pump never interleave.
pub(crate) fn write_locked<W: Write, T: serde::Serialize>(
    writer: &Mutex<W>,
    frame: &T,
) -> Result<(), FrameError> {
    let mut guard = writer.lock();
    write_json_line_flush(&mut *guard, frame, UNCAPPED)
}

/// One envelope line on stdout (see [`write_locked`]).
pub(crate) fn write_wire_frame<T: serde::Serialize>(
    writer: &Arc<Mutex<io::Stdout>>,
    frame: &T,
) -> Result<(), FrameError> {
    write_locked(writer, frame)
}

/// An output failure on a handler or event thread cannot be reported to anyone, and a sidecar
/// that cannot report terminal state must not retain delivery authority: exit so the supervisor
/// revokes and restarts this process.
fn exit_on_write_failure(result: Result<(), String>) {
    if let Err(error) = result {
        eprintln!("rightkit-engine fatal IPC output failure: {error}");
        std::process::exit(1);
    }
}

/// Write `frame` as an id-less event; exit the process if the write fails.
pub(crate) fn emit_frame(writer: &Arc<Mutex<io::Stdout>>, frame: &EngineFrame) {
    exit_on_write_failure(write_event_frame(writer, frame));
}

/// Write the response for `id`; exit the process if the write fails.
pub(crate) fn respond_or_exit(writer: &Arc<Mutex<io::Stdout>>, id: u64, frame: &EngineFrame) {
    exit_on_write_failure(
        write_wire_frame(writer, &response_to_wire(id, frame))
            .map_err(|error| format!("write IPC response: {error}")),
    );
}

/// Requests answered at once with `accepted`; their result is reported as an id-less event.
type PendingFileMap = std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>;

/// Cancel flags of the file transcriptions this engine is running for the shell, by the id of the
/// request that started each. A request that finished has no entry.
fn pending_file_transcriptions() -> &'static PendingFileMap {
    static PENDING: std::sync::OnceLock<PendingFileMap> = std::sync::OnceLock::new();
    PENDING.get_or_init(Default::default)
}

/// One running file transcription's entry in the cancel map; dropping it removes the entry.
pub(crate) struct PendingFileTranscription {
    request_id: String,
    cancel: Arc<AtomicBool>,
}

impl PendingFileTranscription {
    pub(crate) fn register(request_id: &str) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        pending_file_transcriptions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(request_id.to_string(), Arc::clone(&cancel));
        Self {
            request_id: request_id.to_string(),
            cancel,
        }
    }

    pub(crate) fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
}

impl Drop for PendingFileTranscription {
    fn drop(&mut self) {
        let mut pending = pending_file_transcriptions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A reused id may belong to a newer job; remove only this job's own entry.
        if pending
            .get(&self.request_id)
            .is_some_and(|flag| Arc::ptr_eq(flag, &self.cancel))
        {
            pending.remove(&self.request_id);
        }
    }
}

/// Set the cancel flag of the running file transcription started by `request_id`. False when no
/// such job is running (it finished, or never reached this engine).
pub(crate) fn cancel_file_transcription(request_id: &str) -> bool {
    let flag = pending_file_transcriptions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(request_id)
        .cloned();
    match flag {
        Some(flag) => {
            flag.store(true, Ordering::Release);
            true
        }
        None => false,
    }
}

pub(crate) fn accepts_immediately(request: &Request) -> bool {
    matches!(
        request,
        Request::StopDictation { .. }
            | Request::CancelDictation { .. }
            | Request::UpdateRecordingContext { .. }
    )
}

/// Session an accepted-style request belongs to (stamped on its completion event).
pub(crate) fn accepted_session_id(request: &Request) -> Option<String> {
    match request {
        Request::StopDictation { session_id, .. }
        | Request::CancelDictation { session_id }
        | Request::UpdateRecordingContext { session_id, .. } => Some(session_id.clone()),
        _ => None,
    }
}

/// The immediate `response` to an accepted-style request.
pub(crate) fn accepted_response(id: u64) -> WireFrame {
    WireFrame::ok(id, json!({ "accepted": true }))
}

/// The `hello` envelope (adds `instance` to the standard fields).
pub(crate) fn hello_envelope() -> Value {
    json!({
        "type": "hello",
        "engine": "rightkit-engine",
        "version": ENGINE_VERSION,
        "contract_version": CONTRACT_VERSION,
        "instance": engine_instance(),
    })
}

/// The unsolicited `event` envelope for `frame`: no request id, `session_id` set, and the
/// engine `generation` in the payload next to the frame.
pub(crate) fn event_envelope(frame: &EngineFrame) -> Result<WireFrame, String> {
    let body =
        serde_json::to_value(frame).map_err(|error| format!("serialize IPC event: {error}"))?;
    Ok(WireFrame::Event {
        id: None,
        session_id: frame.session_id.clone(),
        event: json!({ "generation": engine_instance(), "frame": body }),
    })
}

/// The `response` envelope for a handled request. A reply frame that carries `error` is an
/// `ok:false` response with the whole frame under `error.details.frame`, so the shell can hand
/// the engine's own error frame back to its caller unchanged.
pub(crate) fn response_to_wire(id: u64, frame: &EngineFrame) -> WireFrame {
    match &frame.error {
        None => WireFrame::ok(id, serde_json::to_value(frame).unwrap_or(Value::Null)),
        Some(error) => {
            let wire = WireError {
                code: error.code.clone(),
                message: error.message.clone(),
                retryable: error.retryable,
                details: serde_json::to_value(frame)
                    .ok()
                    .map(|f| json!({ "frame": f })),
            };
            WireFrame::err(id, wire)
        }
    }
}

/// Write `frame` as an id-less event envelope carrying the engine generation.
pub(crate) fn write_event_frame(
    writer: &Arc<Mutex<io::Stdout>>,
    frame: &EngineFrame,
) -> Result<(), String> {
    let envelope = event_envelope(frame)?;
    write_wire_frame(writer, &envelope).map_err(|error| format!("write IPC event: {error}"))
}

pub(crate) fn emit_event(
    writer: &Arc<Mutex<io::Stdout>>,
    schema_name: EngineSchemaName,
    session_id: Option<String>,
    payload: Option<EnginePayload>,
) {
    let frame = EngineFrame::base(
        schema_name,
        &next_id(),
        session_id,
        &next_id(),
        payload,
        None,
    );
    emit_frame(writer, &frame);
}

#[cfg(test)]
mod accepted_tests {
    use super::*;

    #[test]
    fn stop_cancel_and_context_are_accepted_at_once_and_nothing_else_is() {
        assert!(accepts_immediately(&Request::StopDictation {
            session_id: "s".into(),
            send_enter: false,
            local_only: false
        }));
        assert!(accepts_immediately(&Request::CancelDictation {
            session_id: "s".into()
        }));
        assert!(!accepts_immediately(&Request::Health));
        assert!(!accepts_immediately(&Request::StartDictation {
            session_id: "s".into(),
            recording_target: None,
            recording_context: None,
            last_pasted: None
        }));
    }

    #[test]
    fn accepted_requests_name_their_session_for_the_completion_event() {
        let stop = Request::StopDictation {
            session_id: "s9".into(),
            send_enter: false,
            local_only: false,
        };
        assert_eq!(accepted_session_id(&stop).as_deref(), Some("s9"));
        assert_eq!(
            accepted_session_id(&Request::CancelDictation {
                session_id: "c1".into()
            })
            .as_deref(),
            Some("c1")
        );
        assert_eq!(accepted_session_id(&Request::Health), None);
        // The envelope then carries both session and generation.
        let mut frame = error_frame("r", "t", None, "x");
        frame.session_id = accepted_session_id(&stop);
        let envelope = serde_json::to_value(event_envelope(&frame).unwrap()).unwrap();
        assert_eq!(envelope["session_id"], "s9");
        assert_eq!(envelope["event"]["generation"], engine_instance());
    }

    #[test]
    fn accepted_response_is_ok_true() {
        let value = serde_json::to_value(accepted_response(7)).unwrap();
        assert_eq!(value["type"], "response");
        assert_eq!(value["id"], 7);
        assert_eq!(value["ok"], true);
        assert_eq!(value["result"]["accepted"], true);
    }

    #[test]
    fn events_are_idless_and_carry_the_generation_and_session() {
        let frame = error_frame("r", "t", Some("sess"), "boom");
        let envelope = serde_json::to_value(event_envelope(&frame).unwrap()).unwrap();
        assert_eq!(envelope["type"], "event");
        assert!(envelope.get("id").is_none());
        assert_eq!(envelope["session_id"], "sess");
        assert_eq!(envelope["event"]["generation"], engine_instance());
        assert_eq!(envelope["event"]["frame"]["request_id"], "r");
    }

    #[test]
    fn hello_names_the_instance_and_contract() {
        let hello = hello_envelope();
        assert_eq!(hello["contract_version"], CONTRACT_VERSION);
        assert_eq!(hello["instance"], engine_instance());
    }
}

#[cfg(test)]
mod file_cancel_and_last_paste_tests {
    use super::*;
    use rightkit_transcript_library::delivery::{DeliveryOutcome, DeliveryRecord, TargetSnapshot};

    fn blank_target() -> TargetSnapshot {
        TargetSnapshot {
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
        }
    }

    fn pasted(text: &str) -> DeliveryRecord {
        DeliveryRecord::new("d1", text, DeliveryOutcome::Pasted, blank_target())
    }

    #[test]
    fn cancel_flips_the_flag_of_a_live_request_only() {
        let live = PendingFileTranscription::register("transcribe_file:test-live");
        let flag = live.cancel_flag();
        assert!(!flag.load(Ordering::Acquire));
        assert!(!cancel_file_transcription("transcribe_file:test-other"));
        assert!(!flag.load(Ordering::Acquire));
        assert!(cancel_file_transcription("transcribe_file:test-live"));
        assert!(flag.load(Ordering::Acquire));
        drop(live);
        // Finished: no entry is left, so a late cancel finds nothing.
        assert!(!cancel_file_transcription("transcribe_file:test-live"));
    }

    #[test]
    fn a_finished_job_does_not_remove_the_entry_of_a_reused_id() {
        let first = PendingFileTranscription::register("transcribe_file:test-reuse");
        let second = PendingFileTranscription::register("transcribe_file:test-reuse");
        drop(first);
        assert!(cancel_file_transcription("transcribe_file:test-reuse"));
        assert!(second.cancel_flag().load(Ordering::Acquire));
    }

    fn start(runtime: &Arc<Mutex<EngineRuntime>>, last_pasted: Option<DeliveryRecord>) {
        let writer = Arc::new(Mutex::new(io::stdout()));
        let pump = Arc::new(AtomicBool::new(false));
        let frame = handle(
            runtime,
            &writer,
            &pump,
            "start:1",
            Request::StartDictation {
                session_id: "s1".into(),
                recording_target: None,
                recording_context: None,
                last_pasted,
            },
        );
        assert!(frame.error.is_none(), "start failed: {:?}", frame.error);
    }

    #[test]
    fn start_with_the_shells_last_paste_records_it_as_the_last_delivery() {
        let runtime = Arc::new(Mutex::new(EngineRuntime::new(std::path::PathBuf::from(
            ".",
        ))));
        start(&runtime, Some(pasted("hello world")));
        let r = runtime.lock();
        assert_eq!(r.last_delivery().unwrap().transcript, "hello world");
        // The shell owns history: the record is not added to the engine's recent list.
        assert!(r.recent_history(10).is_empty());
    }

    #[test]
    fn start_without_a_last_paste_leaves_no_last_delivery() {
        let runtime = Arc::new(Mutex::new(EngineRuntime::new(std::path::PathBuf::from(
            ".",
        ))));
        start(&runtime, None);
        assert!(runtime.lock().last_delivery().is_none());
    }
}
