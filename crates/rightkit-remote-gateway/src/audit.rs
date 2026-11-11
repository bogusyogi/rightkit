//! The Activity record of remote calls: one JSON line per call, written through
//! `rightkit-owner-approvals`' audit file. A line holds the time, the client (a label the caller
//! gave), the origin (`local` or `gateway`, set by the process that made the call: go by this,
//! not the client, to tell the gateway's calls apart), the tool, the SHAPE of the arguments and
//! the outcome. Never a path, a file's content, a search term, a turn's text or anything else the
//! caller read.

use super::Call;
use rightkit_agent_gateway::audit::{AuditSink, Entry};
use serde_json::{Map, Value, json};

pub use rightkit_owner_approvals::audit::{
    FILE, SECURITY_FILE, append_activity, client, extension, record_secret, record_security,
    short_id,
};

/// The Activity file as the core gate's audit sink: one JSON line per row, `at` in seconds.
/// Best effort: a line that cannot be written is dropped, and the call still answers.
pub struct ActivityFile;

impl AuditSink for ActivityFile {
    fn record(&self, entry: &Entry<'_>) -> std::result::Result<(), String> {
        let mut line = json!({
            "v": 1,
            "at": entry.at_ms().max(0) / 1000,
            "client": entry.agent_name.unwrap_or_default(),
            "origin": entry.origin.unwrap_or_default().as_str(),
            "tool": entry.method,
            "outcome": entry.outcome,
        });
        if let (Some(row), Some(facts)) = (line.as_object_mut(), entry.fields) {
            for (key, value) in facts {
                row.insert(key.clone(), value.clone());
            }
        }
        append_activity(&line);
        Ok(())
    }
}

/// Writes the record of one finished call. Best effort: failures are ignored.
pub fn record(call: &Call, result: &super::Result<Value>) -> std::result::Result<(), String> {
    record_args(call, &Value::Null, result)
}

/// `record` for a call that carried structured arguments: the record adds only their SHAPE (the
/// op of `files.manage`, the length of a command, the number of edits), never a path, command
/// text, edit text, input or approval id.
pub fn record_args(
    call: &Call,
    args: &Value,
    result: &super::Result<Value>,
) -> std::result::Result<(), String> {
    let mut fields = facts(call, result);
    shape(&mut fields, args);
    let client = client(&call.client);
    let outcome = match result {
        Ok(_) => "ok",
        Err(refusal) => refusal.code,
    };
    ActivityFile.record(&Entry {
        agent_name: Some(&client),
        origin: Some(call.origin),
        method: &call.tool,
        outcome,
        fields: Some(&fields),
        ..Entry::default()
    })
}

/// Adds the shape of structured arguments to a record.
fn shape(fields: &mut Map<String, Value>, args: &Value) {
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

/// The record's facts beyond who, which door, which tool and the outcome.
fn facts(call: &Call, result: &super::Result<Value>) -> Map<String, Value> {
    let mut record = Map::new();
    let fields = &mut record;
    if let Ok(answer) = result {
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
