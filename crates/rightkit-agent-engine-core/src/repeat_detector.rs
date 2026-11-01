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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flags_identical_tool_and_args_repeated_within_one_turn() {
        let turn_key = 0xAAAA_usize;
        let sid = "session-repeat-same-turn";
        RepeatDetector::clear_session(sid);

        let first = RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "ls"}));
        assert!(!first.repeated, "first occurrence must not be flagged");
        assert_eq!(first.occurrence, 1);

        let second = RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "ls"}));
        assert!(second.repeated, "identical call repeated in the same turn");
        assert_eq!(second.occurrence, 2);

        let third = RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "ls"}));
        assert!(third.repeated);
        assert_eq!(third.occurrence, 3);
    }

    #[test]
    fn key_order_does_not_defeat_repeat_detection() {
        let turn_key = 0xBBBB_usize;
        let sid = "session-key-order";
        RepeatDetector::clear_session(sid);

        RepeatDetector::observe(
            turn_key,
            sid,
            "write_file",
            &json!({"path": "a.txt", "content": "hi"}),
        );
        let second = RepeatDetector::observe(
            turn_key,
            sid,
            "write_file",
            &json!({"content": "hi", "path": "a.txt"}),
        );
        assert!(
            second.repeated,
            "reordered object keys must normalize to the same signature"
        );
    }

    #[test]
    fn different_args_are_not_flagged_as_repeated() {
        let turn_key = 0xCCCC_usize;
        let sid = "session-different-args";
        RepeatDetector::clear_session(sid);

        RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "ls"}));
        let second = RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "pwd"}));
        assert!(!second.repeated, "different arguments are a different call");
        assert_eq!(second.occurrence, 1);
    }

    #[test]
    fn different_tool_name_same_args_are_not_flagged() {
        let turn_key = 0xDDDD_usize;
        let sid = "session-different-tool";
        RepeatDetector::clear_session(sid);

        RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "ls"}));
        let second =
            RepeatDetector::observe(turn_key, sid, "powershell", &json!({"command": "ls"}));
        assert!(!second.repeated);
    }

    #[test]
    fn repeat_across_two_different_turns_is_not_flagged() {
        let sid = "session-repeat-across-turns";
        RepeatDetector::clear_session(sid);

        let turn_one = 0x1111_usize;
        let first = RepeatDetector::observe(turn_one, sid, "shell", &json!({"command": "ls"}));
        assert!(!first.repeated);

        // A new turn begins (new turn_key). Even though the tool + args are
        // byte-for-byte identical, it must not be reported as repeated
        // because the earlier call was in a different turn.
        let turn_two = 0x2222_usize;
        let second = RepeatDetector::observe(turn_two, sid, "shell", &json!({"command": "ls"}));
        assert!(
            !second.repeated,
            "identical call in a NEW turn must not be flagged as repeated"
        );
        assert_eq!(
            second.occurrence, 1,
            "turn boundary resets the occurrence count"
        );
    }

    #[test]
    fn observer_never_returns_a_deny_or_block_signal() {
        // Contract check: `RepeatObservation` has no variant/field capable of
        // expressing denial — it is a plain struct of `repeated: bool` and
        // `occurrence: u32`. Exercise a pathological "same call spammed many
        // times" turn and confirm every call still returns normally (no
        // panic, no error type, nothing but the two report fields).
        let turn_key = 0xEEEE_usize;
        let sid = "session-observe-only-contract";
        RepeatDetector::clear_session(sid);

        for i in 1..=25u32 {
            let observation =
                RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "spam"}));
            assert_eq!(observation.occurrence, i);
            assert_eq!(observation.repeated, i > 1);
            // The type itself proves the non-blocking contract: there is no
            // way to construct a "deny" from this API surface.
        }
    }

    #[test]
    fn clear_session_drops_prior_state() {
        let turn_key = 0xFFFF_usize;
        let sid = "session-clear";
        RepeatDetector::clear_session(sid);
        RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "ls"}));
        RepeatDetector::clear_session(sid);
        let after_clear =
            RepeatDetector::observe(turn_key, sid, "shell", &json!({"command": "ls"}));
        assert!(
            !after_clear.repeated,
            "clearing a session's state must forget its prior calls"
        );
        assert_eq!(after_clear.occurrence, 1);
    }
}
