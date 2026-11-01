//! Bounded, non-searchable session/task scratchpad. Never persisted or promoted.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
const MAX_BYTES: usize = 64 * 1024;
const MAX_ID: usize = 160;
const MAX_ENTRIES: usize = 256;
const TTL_MS: u64 = 86_400_000;
type PadMap = HashMap<(String, String), (Value, u64)>;

static PAD: OnceLock<Mutex<PadMap>> = OnceLock::new();
fn pad() -> &'static Mutex<PadMap> {
    PAD.get_or_init(|| Mutex::new(HashMap::new()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
fn purge(state: &mut HashMap<(String, String), (Value, u64)>, at: u64) {
    state.retain(|_, (_, expiry)| *expiry > at);
}
fn err(code: &str, message: &str) -> Value {
    json!({"schemaVersion":1,"operation":"membrane_scratchpad","errorVersion":1,"result":{"kind":"error","code":code,"message":message,"retryable":false}})
}
/// Execute save/load/clear with exact session+task binding.
pub fn handle(request: &Value) -> Value {
    let op = request
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !matches!(op, "save" | "load" | "clear") {
        return err(
            "scratchpad_operation_invalid",
            "operation must be one of save, load, clear",
        );
    }
    let session = request
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let task = request
        .get("taskId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if session.is_empty() || task.is_empty() || session.len() > MAX_ID || task.len() > MAX_ID {
        return err(
            "scratchpad_scope_required",
            "sessionId and taskId are required",
        );
    }
    let key = (session.to_owned(), task.to_owned());
    let mut state = match pad().lock() {
        Ok(state) => state,
        Err(_) => return err("scratchpad_unavailable", "scratchpad state unavailable"),
    };
    purge(&mut state, now());
    let data = match op {
        "save" => {
            let Some(value) = request.get("scratchpad").cloned() else {
                return err(
                    "scratchpad_envelope_invalid",
                    "scratchpad payload is required",
                );
            };
            if serde_json::to_vec(&value)
                .map(|v| v.len())
                .unwrap_or(usize::MAX)
                > MAX_BYTES
            {
                return err(
                    "scratchpad_payload_too_large",
                    "scratchpad exceeds bounded payload",
                );
            }
            if !state.contains_key(&key) && state.len() >= MAX_ENTRIES {
                if let Some(oldest) = state.keys().min().cloned() {
                    state.remove(&oldest);
                }
            }
            state.insert(key, (value.clone(), now().saturating_add(TTL_MS)));
            json!({"status":"saved_ephemeral","operation":"save","scratchpad":value})
        }
        "load" => {
            json!({"status":"loaded","operation":"load","scratchpad":state.get(&key).map(|(value, _)| value.clone())})
        }
        _ => {
            let cleared = state.remove(&key).is_some() as usize;
            json!({"status":"cleared","operation":"clear","cleared":cleared})
        }
    };
    json!({"schemaVersion":1,"operation":"membrane_scratchpad","errorVersion":1,"result":{"kind":"success","data":data}})
}
/// Drop all state for one authoritative session teardown.
pub fn clear_session(session: &str) -> Result<usize, String> {
    if session.trim().is_empty() || session.len() > MAX_ID {
        return Err("scratchpad_scope_required".into());
    }
    let mut state = pad()
        .lock()
        .map_err(|_| "scratchpad_unavailable".to_string())?;
    let before = state.len();
    state.retain(|(s, _), _| s != session);
    Ok(before - state.len())
}
pub fn hub_summary(session: &str) -> Result<Value, String> {
    if session.trim().is_empty() || session.len() > MAX_ID {
        return Err("scratchpad_scope_required".into());
    }
    let mut state = pad()
        .lock()
        .map_err(|_| "scratchpad_unavailable".to_string())?;
    purge(&mut state, now());
    Ok(json!({"sessionId":session,"entries":state.keys().filter(|(s, _)| s == session).count()}))
}
pub fn close_response(session: &str) -> (u16, Value) {
    match clear_session(session) {
        Ok(cleared) => (
            200,
            json!({"status":"cleared","sessionId":session,"cleared":cleared}),
        ),
        Err(code) => (400, err(&code, "invalid session scope")),
    }
}
