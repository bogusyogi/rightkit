//! The Activity record of remote calls: one JSON line per call, written through
//! `rightkit-approvals`' audit file. A line holds the time, the client (a label the caller
//! gave), the origin (`local` or `gateway`, set by the process that made the call: go by this,
//! not the client, to tell the gateway's calls apart), the tool, the SHAPE of the arguments and
//! the outcome. Never a path, a file's content, a search term, a turn's text or anything else the
//! caller read.

use super::Call;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub use rightkit_approvals::audit::{
    FILE, SECURITY_FILE, append_activity, client, extension, record_secret, record_security,
    short_id,
};

/// Writes the record of one finished call. Best effort: failures are ignored.
pub fn record(call: &Call, result: &super::Result<Value>) {
    record_args(call, &Value::Null, result)
}

/// `record` for a call that carried structured arguments: the record adds only their SHAPE (the
/// op of `files.manage`, the length of a command, the number of edits), never a path, command
/// text, edit text, input or approval id.
pub fn record_args(call: &Call, args: &Value, result: &super::Result<Value>) {
    let mut record = line(call, result);
    shape(&mut record, args);
    append_activity(&record);
}

/// Adds the shape of structured arguments to a record.
fn shape(record: &mut Value, args: &Value) {
    let Some(fields) = record.as_object_mut() else {
        return;
    };
    if let Some(op) = args["op"].as_str()
        && matches!(op, "mkdir" | "move" | "copy" | "trash")
    {
        fields.insert("op".into(), json!(op));
    }
    if let Some(path) = args["path"].as_str()
        && !fields.contains_key("ext")
    {
        fields.insert("ext".into(), json!(extension(path)));
    }
    if let Some(command) = args["command"].as_str() {
        fields.insert("commandChars".into(), json!(command.chars().count()));
    }
    if let Some(edits) = args["edits"].as_array() {
        fields.insert("edits".into(), json!(edits.len()));
    }
    if let Some(ops) = args["ops"].as_array() {
        fields.insert("ops".into(), json!(ops.len()));
    }
    if let Some(files) = args["files"]
        .as_array()
        .or_else(|| args["paths"].as_array())
    {
        fields.insert("files".into(), json!(files.len()));
    }
    if let Some(create) = args["create"].as_str() {
        fields.insert("createChars".into(), json!(create.chars().count()));
    }
    if let Some(kind) = args["action"]["type"].as_str()
        && matches!(
            kind,
            "press"
                | "set_value"
                | "type_text"
                | "keys"
                | "click"
                | "scroll"
                | "focus"
                | "menu_press"
                | "wait"
                | "handover"
        )
    {
        fields.insert("screenAction".into(), json!(kind));
    }
    if let Some(tty) = args["tty"].as_bool() {
        fields.insert("tty".into(), json!(tty));
    }
    if let Some(cmds) = args["cmds"].as_array() {
        fields.insert("commands".into(), json!(cmds.len()));
    }
    // A chat terminal call: the kind and the first eight characters of the chat id, like chat_read.
    if let Some(chat) = args["chat"].as_str() {
        let (kind, id) = chat.split_once(':').unwrap_or(("", chat));
        let kind = if super::chat::CHAT_KINDS.contains(&kind) {
            kind
        } else {
            ""
        };
        let short: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(8)
            .collect();
        fields.insert("chatKind".into(), json!(kind));
        fields.insert("chatId".into(), json!(short));
    }
    if let Some(window) = args["window"].as_u64() {
        fields.insert("window".into(), json!(window));
    }
    if let Some(pid) = args["pid"].as_u64() {
        fields.insert("pid".into(), json!(pid));
    }
    if let Some(session) = args["session"].as_str() {
        let short: String = session
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect();
        fields.insert("session".into(), json!(short));
    }
    // Content search and tail/hash are shapes too, never the text searched for.
    if matches!(args["mode"].as_str(), Some("name" | "content")) {
        fields.insert("mode".into(), json!(args["mode"]));
    }
    if args["tail"].is_u64() {
        fields.insert("tail".into(), json!(args["tail"]));
    }
    if args["sha256"].is_boolean() {
        fields.insert("sha256".into(), json!(args["sha256"]));
    }
    if args.is_object() {
        fields.insert("approved".into(), json!(args["approval"].is_string()));
    }
}

/// The record's JSON.
fn line(call: &Call, result: &super::Result<Value>) -> Value {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut record = json!({
        "v": 1,
        "at": at,
        "client": client(&call.client),
        "origin": call.origin.as_str(),
        "tool": call.tool,
    });
    let fields = record.as_object_mut().expect("an object");
    match result {
        Ok(answer) => {
            fields.insert("outcome".into(), json!("ok"));
            if let Some(root) = answer["root"].as_u64() {
                fields.insert("root".into(), json!(root));
            }
            // A screen.act answer names the tier that ran and the crate's result word.
            if let Some(tier) = answer["tier"].as_str()
                && matches!(tier, "semantic" | "in_app" | "targeted" | "foreground")
            {
                fields.insert("tier".into(), json!(tier));
            }
            for key in ["bytes", "returned"] {
                if let Some(n) = answer[key].as_u64() {
                    fields.insert(key.into(), json!(n));
                }
            }
        }
        Err(refusal) => {
            fields.insert("outcome".into(), json!(refusal.code));
        }
    }
    if let Some(path) = &call.path {
        fields.insert("ext".into(), json!(extension(path)));
    }
    if let Some(query) = &call.query {
        fields.insert("queryChars".into(), json!(query.chars().count()));
    }
    if let Some(chat) = &call.chat {
        let (kind, id) = chat.split_once(':').unwrap_or(("", chat));
        let kind = if super::chat::CHAT_KINDS.contains(&kind) {
            kind
        } else {
            ""
        };
        let short: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(8)
            .collect();
        fields.insert("chatKind".into(), json!(kind));
        fields.insert("chatId".into(), json!(short));
    }
    record
}
