//! ENG-92 — Tool-call loop/repeat detection.
//!
//! Detects when the *same* tool call (name + normalized arguments) is issued
//! again within the same turn, and surfaces that fact for the caller to act
//! on. This detector is strictly **observe-only**: it never blocks, denies,
//! or mutates a call — `observe()` always returns, and its result is a
//! report, not a verdict. Wiring it into an enforcement decision is
//! explicitly out of scope for ENG-92 and must not be added here.
//!
//! # Turn scoping without a new engine field
//!
//! The engine allocates a fresh `turn_results: HashMap<String, ToolResult>`
//! once per model turn (see `engine_loop.rs` / `engine_loop_dispatch.rs`) and
//! threads it by `&mut` reference through every tool-call dispatch for that
//! turn. This detector uses the *address* of that map — obtained by the
//! caller via `turn_results as *const _ as usize` — as an opaque, per-turn
//! key. That lets the detector scope itself to "this turn" without adding a
//! new field to `SessionEngine`, at the cost of relying on the map staying
//! at a stable address for the duration of one turn, which the existing
//! `&mut` threading already guarantees (it is never reallocated or moved
//! mid-turn in the current dispatch code paths).
//!
//! State is additionally keyed by session id so that two sessions racing on
//! the (statistically rare, but not impossible) same map address never cross
//! signals. Only the most recent turn's signatures are retained per session;
//! recognizing a new turn key drops the previous turn's counts, so memory
//! stays bounded to one turn per active session rather than growing forever.

use std::collections::HashMap;
use std::sync::Mutex;

/// Result of observing one tool call against the current turn's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatObservation {
    /// `true` when this exact tool name + normalized arguments was already
    /// observed earlier in the same turn. Observe-only: never a deny signal.
    pub repeated: bool,
    /// 1 for the first occurrence in this turn, 2 for the second, etc.
    pub occurrence: u32,
}

struct SessionTurnState {
    turn_key: usize,
    counts: HashMap<String, u32>,
}

static STATE: Mutex<Option<HashMap<String, SessionTurnState>>> = Mutex::new(None);

/// Observe-only, non-blocking tool-call repeat detector.
pub struct RepeatDetector;

impl RepeatDetector {
    /// Record one tool call and report whether the same tool name +
    /// normalized arguments has already been seen earlier in the same turn.
    ///
    /// `turn_key` identifies "this turn" (see module docs); `session_id`
    /// disambiguates concurrent sessions. This function never returns an
    /// error and never signals denial — it is purely observational.
    pub fn observe(
        turn_key: usize,
        session_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> RepeatObservation {
        let signature = call_signature(tool_name, args);
        let mut guard = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sessions = guard.get_or_insert_with(HashMap::new);
        let entry = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionTurnState {
                turn_key,
                counts: HashMap::new(),
            });
        if entry.turn_key != turn_key {
            // A new turn started for this session; last turn's signatures no
            // longer apply. Drop them so memory does not grow across turns.
            entry.turn_key = turn_key;
            entry.counts.clear();
        }
        let count = entry.counts.entry(signature).or_insert(0);
        *count += 1;
        RepeatObservation {
            repeated: *count > 1,
            occurrence: *count,
        }
    }

    /// Drop all tracked state for a session. Not required for correctness
    /// (state is bounded to one turn per session already) but available for
    /// callers that want to release memory promptly on session teardown.
    #[cfg(test)]
    pub fn clear_session(session_id: &str) {
        if let Ok(mut guard) = STATE.lock() {
            if let Some(sessions) = guard.as_mut() {
                sessions.remove(session_id);
            }
        }
    }
}

/// Canonicalize `tool_name` + `args` into a stable string signature so that
/// key order in the JSON object (e.g. `{"a":1,"b":2}` vs `{"b":2,"a":1}`)
/// never produces a false "not repeated" verdict.
fn call_signature(tool_name: &str, args: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(tool_name);
    out.push('\u{0}');
    canonicalize_json(args, &mut out);
    out
}

fn canonicalize_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                canonicalize_json(&map[*key], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonicalize_json(item, out);
            }
            out.push(']');
        }
        other => {
            out.push_str(&other.to_string());
        }
    }
}
