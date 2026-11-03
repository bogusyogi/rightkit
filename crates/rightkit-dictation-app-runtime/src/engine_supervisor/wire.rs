// RightKit stdio-contract mapping for the the app engine (M4).
//
// The engine speaks `rightkit_job_queue`'s envelope: `hello`, then `request` / `response` /
// `event` lines. `EngineFrame` stays the typed payload so schema and validation live in
// `rightkit_dictation_core::engine`:
//
// - request: `method` = the frame's `schema_name`, `params` = the frame;
// - response: `result` = the reply frame; an error reply is `ok:false` with the whole frame under
//   `error.details.frame` (see `rightkit-dictation-runtime/src/ipc/dispatch.rs::response_to_wire`);
// - event: an unsolicited envelope whose `event` is a frame.

use rightkit_dictation_core::engine::{validate_engine_frame, EngineFrame};
use rightkit_job_queue::JobError;
use serde_json::Value;

/// Methods the shell may send to the engine: the explicit allowlist (the app policy). The
/// engine accepts a request only when its `method` equals the frame's `schema_name` and the
/// schema is a request it decodes, so this is the shell-side half of the same list.
pub(crate) const ENGINE_METHOD_ALLOWLIST: &[&str] = &[
    "engine_health",
    "engine_capabilities",
    "engine_info",
    "replace_engine_config",
    "set_provider_keys",
    "reconfigure_l3",
    "replace_vocabulary",
    "engine_state_request",
    "recent_history_request",
    "replace_recent_history",
    "repaste_last_request",
    "copy_last_request",
    "file_transcription_request",
    "file_transcription_cancel",
    "recording_started",
    "recording_context_captured",
    "transcribing_started",
    "wake_listen_started",
    "wake_listen_stopped",
];

/// The request `method` for `frame`, refused unless it is on the allowlist.
pub(crate) fn method_for(frame: &EngineFrame) -> Result<String, String> {
    let method = serde_json::to_value(&frame.schema_name)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| "engine frame schema has no method name".to_string())?;
    if ENGINE_METHOD_ALLOWLIST.contains(&method.as_str()) {
        Ok(method)
    } else {
        Err(format!("engine method {method:?} is not allowed"))
    }
}

/// A response `result` (or event body) back into a validated frame.
pub(crate) fn frame_from_value(value: &Value) -> Result<EngineFrame, String> {
    validate_engine_frame(value).map_err(|error| format!("invalid engine frame: {error}"))
}

/// The engine's own error frame carried by an `ok:false` response, when the error has one.
pub(crate) fn error_frame_of(error: &JobError) -> Option<EngineFrame> {
    let frame = error.details()?.get("frame")?;
    frame_from_value(frame).ok()
}

/// Engine version from the hello envelope (`version`).
pub(crate) fn hello_version(hello: Option<&Value>) -> String {
    hello
        .and_then(|hello| hello.get("version"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string())
}

/// Methods the engine acknowledges at once (`{"accepted":true}`) and completes as an id-less,
/// generation-carrying event (stop / cancel via `transcribing_started`, and the context update).
pub(crate) const ACCEPTED_METHODS: &[&str] =
    &["transcribing_started", "recording_context_captured"];

/// Engine process identity (`instance`) from the hello envelope; the event fence.
pub(crate) fn hello_instance(hello: Option<&Value>) -> Option<String> {
    hello
        .and_then(|hello| hello.get("instance"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The engine's immediate answer to an accepted-style request. Anything but `accepted: true`, or
/// any error, is a failed send: the caller sees it at once instead of a silent late reply.
pub(crate) fn parse_accepted(reply: Result<Value, JobError>) -> Result<(), String> {
    match reply {
        Ok(result) if result.get("accepted").and_then(Value::as_bool) == Some(true) => Ok(()),
        Ok(_) => Err("sidecar did not accept the request".to_string()),
        Err(error) => Err(format!("sidecar call timeout/request failure: {error}")),
    }
}

/// Keep the real spawn error text; add the stale-sidecar hint when it looks like Access Denied
/// (Windows os error 5 / PermissionDenied: consistent with a prior sidecar that did not reap and
/// still holds the binary). Traceability only; the genuine error is never replaced.
pub(crate) fn with_spawn_hint(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("permission denied")
        || lower.contains("os error 5")
        || lower.contains("access is denied")
    {
        format!(
            "{message} (PermissionDenied/os error 5: consistent with a stale sidecar from a prior shutdown that did not reap still holding the binary)"
        )
    } else {
        message.to_string()
    }
}
