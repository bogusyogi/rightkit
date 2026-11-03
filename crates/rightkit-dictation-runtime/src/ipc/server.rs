#![allow(unused_imports)]
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{dispatch::*, events::*};

// RightKit stdio contract over stdin/stdout for the `rightkit-dictation-engine` sidecar.
//
// One envelope per line (`rightkit_sidecar::wire::Frame`): the engine writes `hello` once the
// resident worker is warm, then answers each `request` with a `response` and reports everything
// else as an unsolicited `event`. stderr is reserved for tracing; the shell drains and redacts it.
// The transport is line-delimited so a single corrupted frame never desyncs the stream and the
// channel is trivially recoverable (drop the line, log, continue).
//
// The payload reuses `rightkit_dictation_core::engine::EngineFrame` so the schema and validation live in
// one place: request `params` = the frame, `method` = its `schema_name`, response `result` = the
// reply frame (an error reply is `ok:false` with the frame under `error.details.frame`).
// The supervision (spawn, hello, restart, liveness, bounded shutdown) is RightKit's
// `rightkit_job_queue::EngineSupervisor` in the shell.

pub(crate) use std::io::{self, BufRead, Write};
pub(crate) use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) use std::sync::Arc;

pub(crate) use parking_lot::Mutex;
pub(crate) use rightkit_dictation_core::engine::{
    DiagnosticsPayload, EngineErrorPayload, EngineFrame, EnginePayload, EngineSchemaName, StopKind,
    CURRENT_SCHEMA_VERSION, PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
pub(crate) use rightkit_dictation_core::settings::SettingsBlob;
pub(crate) use rightkit_sidecar::{
    read_json_line_or_oversized_opts, write_json_line_flush, FrameError, Incoming, ReadOptions,
    WhitespaceSet, UNCAPPED,
};
pub(crate) use serde_json::{json, Value};
// The engine speaks the RightKit stdio contract (M4): `hello`, then `request` / `response` /
// `event` envelopes. `EngineFrame` is the typed payload: the request `params`, the response
// `result` and the event body. `method` is the frame's `schema_name`.
pub(crate) use rightkit_job_queue::{CONTRACT_VERSION, DEFAULT_FRAME_LIMIT};
pub(crate) use rightkit_sidecar::wire::{Frame as WireFrame, WireError};

pub(crate) use crate::delivery::DeliveryRecord;
pub(crate) use crate::runtime::{EngineRuntime, FinalizeOutcome};

/// Build identity, independent of whichever ASR model the worker loads.
pub(crate) const ENGINE_VERSION: &str = concat!("rightkit-engine/", env!("CARGO_PKG_VERSION"));

/// Run the sidecar's read loop. Blocks until EOF on stdin (shell quit) or
/// a `Request::Shutdown`.
pub fn run(runtime: Arc<Mutex<EngineRuntime>>) -> anyhow::Result<()> {
    let writer = Arc::new(Mutex::new(io::stdout()));
    let worker_pump_started = Arc::new(AtomicBool::new(false));
    // Do not announce readiness until the resident worker has loaded the ASR
    // model. The shell's prewarm/onboarding path waits for this first frame, so
    // emitting it before `ensure_worker()` would make the UI claim the engine is
    // warm while the first real recording still pays model-load cost.
    {
        let mut guard = runtime.lock();
        guard
            .ensure_worker()
            .map_err(|err| anyhow::anyhow!("worker warmup failed before ready: {err}"))?;
    }
    ensure_worker_event_pump(&runtime, &writer, &worker_pump_started)
        .map_err(|err| anyhow::anyhow!("worker event pump failed before ready: {err}"))?;
    // Machine-wide speech service endpoint (model is warm). Failure must not
    // take down dictation; it is logged and the engine continues stdio-only.
    let _speech_service = match crate::speech_service::start(
        runtime.clone(),
        crate::speech_service::role_for(false),
    ) {
        Ok(service) => Some(service),
        Err(error) => {
            tracing::error!(event = "service_start_failed", error = %error, "speech service unavailable");
            None
        }
    };
    // Emit the hello envelope so the shell can synchronize on the sidecar being alive,
    // contract-compatible, and model-hot before it sends work.
    write_wire_frame(&writer, &hello_envelope())
        .map_err(|err| anyhow::anyhow!("hello frame write failed: {err}"))?;
    tracing::info!(
        "rightkit-engine ready: protocol {}.{}, schema {}, contract {}",
        PROTOCOL_MAJOR,
        PROTOCOL_MINOR,
        CURRENT_SCHEMA_VERSION,
        CONTRACT_VERSION
    );

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut buffer = Vec::new();

    loop {
        // EOF is a clean end; a read failure or a failed reply write is surfaced as the error.
        let NextRequest::Request {
            id: wire_id,
            method,
            mut params,
        } = read_next_request(&mut reader, &mut buffer, &*writer, DEFAULT_FRAME_LIMIT)?
        else {
            tracing::info!("stdin closed; exiting");
            return Ok(());
        };
        if method == "shutdown" {
            tracing::info!("shutdown request received");
            write_wire_frame(&writer, &WireFrame::ok(wire_id, json!({})))?;
            let _ = write_wire_frame(
                &writer,
                &WireFrame::Bye {
                    reason: "shutdown".to_string(),
                },
            );
            return Ok(());
        }
        let secret_frame = is_provider_keys_frame(&params);
        let frame = match rightkit_dictation_core::engine::validate_engine_frame(&params) {
            Ok(f) => f,
            Err(err) => {
                if secret_frame {
                    tracing::warn!("dropping invalid frame (schema=set_provider_keys)");
                    rightkit_dictation_core::engine::wipe_json_strings(&mut params);
                } else {
                    tracing::warn!(error = %err, "dropping invalid frame");
                }
                write_wire_frame(
                    &writer,
                    &WireFrame::err(
                        wire_id,
                        WireError::new("invalid_frame", "frame failed validation"),
                    ),
                )?;
                continue;
            }
        };
        if secret_frame {
            // The typed frame owns the keys now (SecretString, wiped on
            // drop); scrub the raw copies this loop still holds.
            rightkit_dictation_core::engine::wipe_json_strings(&mut params);
        }
        if schema_method(&frame.schema_name).as_deref() != Some(method.as_str()) {
            tracing::warn!("dropping request whose method does not match its schema");
            write_wire_frame(
                &writer,
                &WireFrame::err(
                    wire_id,
                    WireError::new("method_mismatch", "method does not match the frame schema"),
                ),
            )?;
            continue;
        }
        let request_id = frame.request_id.clone();
        let Some(req) = decode_request(frame) else {
            write_wire_frame(
                &writer,
                &WireFrame::err(
                    wire_id,
                    WireError::new(
                        "unsupported_request",
                        "the engine does not serve this request",
                    ),
                ),
            )?;
            continue;
        };
        if matches!(req, Request::Shutdown) {
            tracing::info!("shutdown request received");
            write_wire_frame(&writer, &WireFrame::ok(wire_id, json!({})))?;
            return Ok(());
        }
        // Stop / cancel / context requests are acknowledged at once and report their result as an
        // id-less event carrying this engine's generation (failures included, as an error frame):
        // the reply never waits on the work, and the shell fences on the generation.
        if accepts_immediately(&req) {
            // Flushed (under the stdout lock) before the work that can emit the completion exists.
            write_wire_frame(&writer, &accepted_response(wire_id))?;
            let req_runtime = runtime.clone();
            let req_writer = writer.clone();
            let req_pump = worker_pump_started.clone();
            let session = accepted_session_id(&req);
            std::thread::spawn(move || {
                let mut result = handle(&req_runtime, &req_writer, &req_pump, &request_id, req);
                // The completion always names its session (and write_event_frame the generation).
                if result.session_id.is_none() {
                    result.session_id = session;
                }
                emit_frame(&req_writer, &result);
            });
            continue;
        }
        // Handle each request on its own thread so a slow op never blocks the
        // stdin read loop. Combined with the lock-narrowed handlers (which release
        // the runtime mutex across worker round-trips), concurrent requests no
        // longer serialize. The shell matches responses by envelope id, so
        // out-of-order completion is fine. Every frame is written under the one stdout lock,
        // held across encode, write and flush (shared with the worker event pump), so frames
        // never interleave.
        let req_runtime = runtime.clone();
        let req_writer = writer.clone();
        let req_pump = worker_pump_started.clone();
        std::thread::spawn(move || {
            let response = handle(&req_runtime, &req_writer, &req_pump, &request_id, req);
            respond_or_exit(&req_writer, wire_id, &response);
        });
    }
}

/// The next thing the request loop must act on.
pub(crate) enum NextRequest {
    /// Clean end of input.
    Eof,
    Request {
        id: u64,
        method: String,
        params: Value,
    },
}

/// Read until the next well-formed request. An oversized request is answered with
/// `frame_too_large` (its id from the frame head) and a malformed line is dropped and scrubbed;
/// either way the reader is back on a line boundary and the loop carries on. EOF is `Eof`. A read
/// failure, or a failure to write that reply, is returned, never swallowed.
pub(crate) fn read_next_request<R: BufRead, W: Write>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    writer: &Mutex<W>,
    limit: usize,
) -> Result<NextRequest, FrameError> {
    let options = ReadOptions {
        skip_whitespace_only_lines: true,
        whitespace: WhitespaceSet::Unicode,
        ..ReadOptions::default()
    };
    loop {
        match read_json_line_or_oversized_opts::<_, WireFrame>(reader, buffer, limit, options) {
            Ok(None) => return Ok(NextRequest::Eof),
            Ok(Some(Incoming::Frame(WireFrame::Request {
                id, method, params, ..
            }))) => {
                return Ok(NextRequest::Request { id, method, params });
            }
            Ok(Some(Incoming::Frame(_))) => tracing::warn!("dropping non-request envelope"),
            Ok(Some(Incoming::Oversized(over))) => {
                // Answer the request it belongs to; the engine stays up and in sync.
                tracing::warn!(
                    bytes = over.bytes,
                    limit = over.limit,
                    "rejecting oversized request"
                );
                if let Some(id) = over.id() {
                    write_locked(
                        writer,
                        &WireFrame::err(
                            id,
                            WireError::new(
                                "frame_too_large",
                                "request exceeded the engine frame limit",
                            ),
                        ),
                    )?;
                }
            }
            Err(err @ FrameError::Io(_)) => return Err(err),
            Err(err) => {
                // A parse error carries only a category and position, never content; still,
                // never echo a provider-keys frame, and scrub the raw line left in `buffer`.
                if String::from_utf8_lossy(buffer).contains("set_provider_keys") {
                    tracing::warn!("dropping malformed frame (schema=set_provider_keys)");
                } else {
                    tracing::warn!(error = %err, "dropping malformed frame");
                }
                buffer.iter_mut().for_each(|byte| *byte = 0);
            }
        }
    }
}

#[derive(Debug)]
pub enum Request {
    Health,
    Capabilities,
    Info,
    ReplaceEngineConfig {
        config: SettingsBlob,
    },
    /// BYOK provider keys as the owner's hand-off frame (hex). `SecretString` Debug is redacted
    /// and wipes on drop.
    SetProviderKeys {
        frame: rightkit_dictation_core::engine::SecretString,
    },
    /// Live L3 policy change; this engine answers it with an error and restarts to reconfigure.
    ReconfigureL3 {
        ai_polish_enabled: bool,
        provider_state: String,
        provider_ready: bool,
    },
    ReplaceVocabulary {
        terms: Vec<String>,
        /// Parallel optional list carrying sound-alike aliases per term. Old
        /// shells that send no `term_details` fall back to the bare `terms`
        /// list (no aliases).
        term_details: Option<Vec<rightkit_dictation_core::engine::VocabTermWire>>,
    },
    StartDictation {
        session_id: String,
        recording_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
        recording_context: Option<rightkit_dictation_core::engine::RecordingContext>,
        /// The shell's last real paste, so voice undo and casing commands see the text it
        /// delivered (the engine never pastes in shell-delivery mode).
        last_pasted: Option<DeliveryRecord>,
    },
    UpdateRecordingContext {
        session_id: String,
        context: rightkit_dictation_core::engine::RecordingContext,
        recording_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
    },
    TranscribeFile {
        path: std::path::PathBuf,
    },
    /// Stop the file transcription started by the request with this id.
    CancelFileTranscription {
        request_id: String,
    },
    StopDictation {
        session_id: String,
        send_enter: bool,
        /// `true` only for a `StopKind::CancelToHistory` finalize (the
        /// shell's cancel-to-history flow): the sidecar still runs ASR to
        /// completion and returns the transcript, but skips the cloud
        /// L1/L2/L3 AI polish lanes entirely — see
        /// `runtime/finalize_transcript.rs`. `false` for every
        /// other `StopDictation` request (Stop/SendEnter and the legacy
        /// request_id-suffix fallback), which must behave exactly as
        /// before this field was added.
        local_only: bool,
    },
    CancelDictation {
        session_id: String,
    },
    RepasteLast,
    CopyLast,
    GetState,
    GetRecentHistory {
        limit: usize,
    },
    ReplaceRecentHistory {
        records: Vec<DeliveryRecord>,
    },
    StartWakeListen {
        model: Option<String>,
        threshold: Option<f32>,
    },
    StopWakeListen,
    Shutdown,
}

/// True when the raw frame is a `set_provider_keys` request. Used only to pick
/// a schema-name-only log line; never reads or logs the body.
pub(crate) fn is_provider_keys_frame(value: &Value) -> bool {
    value.get("schema_name").and_then(Value::as_str) == Some("set_provider_keys")
}

pub(crate) fn decode_request(frame: EngineFrame) -> Option<Request> {
    use EngineSchemaName::*;
    let payload = frame.payload?;
    let session_id = frame.session_id.clone();
    let request_id = frame.request_id.clone();
    let recording_target = frame.recording_target.clone();
    let recording_context = frame.recording_context.clone();
    match (frame.schema_name, payload) {
        (EngineHealth, EnginePayload::Health { .. }) => Some(Request::Health),
        (EngineCapabilities, EnginePayload::Capabilities { .. }) => Some(Request::Capabilities),
        (EngineInfo, EnginePayload::EngineInfo { .. }) => Some(Request::Info),
        (ReplaceEngineConfig, EnginePayload::ReplaceEngineConfig { config }) => {
            serde_json::from_value::<SettingsBlob>(config)
                .ok()
                .map(|config| Request::ReplaceEngineConfig { config })
        }
        (SetProviderKeys, EnginePayload::SetProviderKeys { frame }) => {
            Some(Request::SetProviderKeys { frame })
        }
        (
            ReconfigureL3,
            EnginePayload::ReconfigureL3 {
                ai_polish_enabled,
                provider_state,
                provider_ready,
            },
        ) => Some(Request::ReconfigureL3 {
            ai_polish_enabled,
            provider_state,
            provider_ready,
        }),
        (
            ReplaceVocabulary,
            EnginePayload::ReplaceVocabulary {
                terms,
                term_details,
            },
        ) => Some(Request::ReplaceVocabulary {
            terms,
            term_details,
        }),
        (EngineStateRequest, EnginePayload::EngineStateRequest) => Some(Request::GetState),
        (RepasteLastRequest, EnginePayload::RepasteLastRequest) => Some(Request::RepasteLast),
        (CopyLastRequest, EnginePayload::CopyLastRequest) => Some(Request::CopyLast),
        (RecentHistoryRequest, EnginePayload::RecentHistoryRequest { limit }) => {
            Some(Request::GetRecentHistory { limit })
        }
        (ReplaceRecentHistory, EnginePayload::ReplaceRecentHistory { records }) => {
            let records =
                serde_json::from_value::<Vec<DeliveryRecord>>(records).unwrap_or_default();
            Some(Request::ReplaceRecentHistory { records })
        }
        (
            WakeListenStarted,
            EnginePayload::WakeListenStarted {
                model, threshold, ..
            },
        ) => Some(Request::StartWakeListen { model, threshold }),
        (WakeListenStopped, EnginePayload::WakeListenStopped { .. }) => {
            Some(Request::StopWakeListen)
        }
        (
            RecordingStarted,
            EnginePayload::RecordingStarted {
                session_id,
                last_pasted,
            },
        ) => Some(Request::StartDictation {
            session_id,
            recording_target,
            recording_context,
            last_pasted,
        }),
        (
            RecordingContextCaptured,
            EnginePayload::RecordingContextCaptured {
                session_id,
                context,
                recording_target,
            },
        ) => Some(Request::UpdateRecordingContext {
            session_id,
            context,
            recording_target,
        }),
        (FileTranscriptionRequest, EnginePayload::FileTranscriptionRequest { path }) => {
            Some(Request::TranscribeFile {
                path: std::path::PathBuf::from(path),
            })
        }
        (FileTranscriptionCancel, EnginePayload::FileTranscriptionCancel { request_id }) => {
            Some(Request::CancelFileTranscription { request_id })
        }
        (RecordingLevel, EnginePayload::RecordingLevel { .. }) => None,
        (TranscribingStarted, EnginePayload::TranscribingStarted { stop_kind, .. }) => {
            let id = session_id?;
            // Prefer the typed `stop_kind` (protocol v2) when the sender set
            // it; fall back to the legacy request_id suffix convention for
            // frames from an older shell build that doesn't send it yet.
            // Both paths are kept live during the transition — see the
            // module doc comment and dispatch #9/#10.
            match stop_kind {
                Some(StopKind::Cancel) => {
                    return Some(Request::CancelDictation { session_id: id });
                }
                Some(StopKind::SendEnter) => {
                    return Some(Request::StopDictation {
                        session_id: id,
                        send_enter: true,
                        local_only: false,
                    });
                }
                Some(StopKind::Stop) => {
                    return Some(Request::StopDictation {
                        session_id: id,
                        send_enter: false,
                        local_only: false,
                    });
                }
                Some(StopKind::CancelToHistory) => {
                    return Some(Request::StopDictation {
                        session_id: id,
                        send_enter: false,
                        local_only: true,
                    });
                }
                None => {}
            }
            if request_id.contains(":cancel") {
                return Some(Request::CancelDictation { session_id: id });
            }
            let send_enter = request_id.contains(":send_enter");
            Some(Request::StopDictation {
                session_id: id,
                send_enter,
                local_only: false,
            })
        }
        (TranscriptFinal, EnginePayload::TranscriptFinal { .. }) => None,
        (EngineError, _) => None,
        _ => None,
    }
}

pub(crate) fn cancel_source_from_request_id(request_id: &str) -> &'static str {
    match request_id.rsplit(':').next() {
        Some("webview_command") => "webview_command",
        Some("fn_escape") => "fn_escape",
        Some("pill_cancel_button") => "pill_cancel_button",
        Some("ptt_chord_abort") => "ptt_chord_abort",
        Some("keyboard_cancel_hook") => "keyboard_cancel_hook",
        _ => "unknown",
    }
}
