//! The `chat.send` line of the hub log: one line for every message this computer sent to a chat
//! on another computer, and how it went (#500: a send that failed left no trace on the sending
//! computer, so "it never arrived" could not be told from "it was never sent").
//!
//! The line goes to `<state>/logs/hub.jsonl`, the file the engine's `hublog` writes, in the same
//! format (`ts_ms`, `level`, `event`, fields) and by the same store, so the CLI (which has no
//! engine) and the engine (the hub's chat commands) log alike. It carries the target computer, the
//! transport tried, whether ssh took over from the device link, the receipt state, the device
//! link's failure code, and the time it took in milliseconds. Never message text, a chat title, a
//! session id or an address. "Keep a hub log" (`hubLog`, default on) switches it off.

use rightkit_logs::{EventRecord, JsonlStore, Level};
use serde_json::{Value, json};

/// A log file starts a new one at this size (the engine's `hublog` uses the same).
const FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Older files kept beside the current one (the engine's `hublog` keeps the same two).
const OLDER_FILES: u32 = 2;

/// What one send to another computer did.
#[derive(Debug, Clone, Default)]
pub struct SendRecord {
    /// The computer the chat is on (its link's name).
    pub device: String,
    /// "device link" or "ssh": the transport that produced the answer, or the last one tried.
    pub transport: String,
    /// The device link failed with nothing delivered and ssh took over.
    pub fallback: bool,
    /// The receipt state (`delivered`, `unknown`, ...), or `error` when no receipt came back.
    pub state: String,
    /// The device link's failure code, or empty.
    pub error_code: String,
    pub duration_ms: u64,
}

fn enabled() -> bool {
    rightkit_app_host::setting_bool("hubLog", true)
}

/// Append the `chat.send` line. Best effort: a failure to write is never an error of the send.
pub fn record(send: &SendRecord) {
    let Some(dir) = rightkit_app_host::state_root() else {
        return;
    };
    if !enabled() {
        return;
    }
    let fields = json!({
        "device": send.device,
        "transport": send.transport,
        "fallback": send.fallback,
        "state": send.state,
        "errorCode": send.error_code,
        // A device-link refusal that ssh did not take over from (R05): the code is terminal, as
        // distinct from an unreachable link that fell back (`fallback` true).
        "terminalRefusal": send.transport == "device link"
            && !send.fallback
            && !send.error_code.is_empty()
            && send.state == "error",
        "durationMs": send.duration_ms,
        "pid": std::process::id(),
    });
    let ts_ms = super::envelope::now_ms() as i64;
    let level = if matches!(send.state.as_str(), "unknown" | "error") {
        Level::Warn
    } else {
        Level::Info
    };
    let mut line = EventRecord::new(ts_ms, level, "chat.send");
    if let Value::Object(map) = fields {
        for (key, value) in map {
            line = line.with_field(key, value);
        }
    }
    let store = JsonlStore::new(dir.join("logs").join("hub.jsonl"), FILE_BYTES)
        .with_generations(OLDER_FILES)
        .with_cross_process_lock();
    store.append_best_effort(&line);
}

/// Append one structured line to the hub log from anywhere in chat (hook runs, receives, roster
/// sweeps). `fields` carries fixed codes, counts and durations only: never message text, titles,
/// session ids, file names or anything a peer supplied. Best effort and silenced by `hubLog`.
pub fn event(level: Level, event: &str, fields: Value) {
    let Some(dir) = rightkit_app_host::state_root() else {
        return;
    };
    if !enabled() {
        return;
    }
    let mut line = EventRecord::new(super::envelope::now_ms() as i64, level, event);
    if let Value::Object(map) = fields {
        for (key, value) in map {
            line = line.with_field(key, value);
        }
    }
    let store = JsonlStore::new(dir.join("logs").join("hub.jsonl"), FILE_BYTES)
        .with_generations(OLDER_FILES)
        .with_cross_process_lock();
    store.append_best_effort(&line);
}
