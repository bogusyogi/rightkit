//! One JSON command surface over a [`Service`], for a host's webview (rightkit#407).
//!
//! A Tauri host registers a single command and forwards to [`invoke`]:
//!
//! ```text
//! #[tauri::command]
//! fn send_invoke(state: State<SendState>, command: String, args: serde_json::Value)
//!     -> Result<serde_json::Value, rightkit_send::Error> {
//!     rightkit_send::commands::invoke(&state.service, &command, &args)
//! }
//! ```
//!
//! `@rightkit/send-ui` calls exactly that command (`createSendClient`). The adapter holds no
//! state and depends on no webview crate, so it serves any host that can pass JSON: Tauri, a CLI,
//! a test harness. Arguments are camelCase JSON objects; results are the crate's serializable
//! types (`Snapshot`, `TrustEntry`, `TrustState`). Failures are the typed [`Error`], which
//! serializes as `{ "code": ... }` for the UI to word.

use super::error::Error;
use super::trust::TrustState;
use super::{SendItem, Service};
use serde_json::Value;
use std::path::PathBuf;

/// Every command [`invoke`] answers.
pub const COMMANDS: &[&str] = &[
    "snapshot",
    "refresh",
    "send",
    "respond",
    "cancel",
    "dismiss",
    "setSaveDir",
    "trustList",
    "trustDefault",
    "trustSet",
    "trustLabel",
    "trustForget",
    "trustSetDefault",
];

fn bad(command: &str, detail: &str) -> Error {
    Error::BadArguments {
        command: command.to_string(),
        detail: detail.to_string(),
    }
}

fn text(args: &Value, command: &str, key: &str) -> Result<String, Error> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| bad(command, &format!("{key} must be a string")))
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn strings(args: &Value, command: &str, key: &str) -> Result<Vec<String>, Error> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| bad(command, &format!("{key} must hold strings")))
            })
            .collect(),
        Some(_) => Err(bad(command, &format!("{key} must be a list"))),
    }
}

fn state(args: &Value, command: &str) -> Result<TrustState, Error> {
    TrustState::parse(&text(args, command, "state")?)
        .ok_or_else(|| bad(command, "state must be allow, ask or deny"))
}

fn json<T: serde::Serialize>(value: &T) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|e| Error::Internal {
        detail: e.to_string(),
    })
}

/// Run `command` with `args` (a JSON object, or null for none) on `service`.
pub fn invoke(service: &Service, command: &str, args: &Value) -> Result<Value, Error> {
    match command {
        "snapshot" => json(&service.snapshot()),
        "refresh" => {
            service.refresh();
            Ok(Value::Null)
        }
        "send" => {
            let target = text(args, command, "target")?;
            let mut items: Vec<SendItem> = strings(args, command, "paths")?
                .into_iter()
                .map(|p| SendItem::Path(PathBuf::from(p)))
                .collect();
            items.extend(
                strings(args, command, "texts")?
                    .into_iter()
                    .map(SendItem::Text),
            );
            let id = service.send(&target, items, flag(args, "clipboard"))?;
            Ok(Value::String(id))
        }
        "respond" => {
            let id = text(args, command, "id")?;
            Ok(Value::Bool(service.respond(&id, flag(args, "accept"))))
        }
        "cancel" => Ok(Value::Bool(service.cancel(&text(args, command, "id")?))),
        "dismiss" => {
            service.dismiss(&text(args, command, "id")?);
            Ok(Value::Null)
        }
        "setSaveDir" => {
            service.set_save_dir(PathBuf::from(text(args, command, "path")?));
            Ok(Value::Null)
        }
        "trustList" => json(&service.devices_trust()),
        "trustDefault" => json(&service.default_state()),
        "trustSet" => {
            let fingerprint = text(args, command, "fingerprint")?;
            service.set_trust(&fingerprint, state(args, command)?)?;
            Ok(Value::Null)
        }
        "trustLabel" => {
            let fingerprint = text(args, command, "fingerprint")?;
            let label = text(args, command, "label")?;
            service.set_label(&fingerprint, &label)?;
            Ok(Value::Null)
        }
        "trustForget" => {
            service.forget(&text(args, command, "fingerprint")?)?;
            Ok(Value::Null)
        }
        "trustSetDefault" => {
            service.set_default_state(state(args, command)?)?;
            Ok(Value::Null)
        }
        other => Err(Error::UnknownCommand {
            command: other.to_string(),
        }),
    }
}

/// The JSON command files waiting in `dir`, oldest name first, as the host's other process dropped
/// them (a webview or notch writes one file per command and wakes the host). Each file is removed
/// as it is read, so a command runs once; a file that is not valid JSON is removed and skipped. A
/// missing folder has none.
pub fn drain_json_dir(dir: &std::path::Path) -> Vec<Value> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let mut commands = Vec::new();
    for file in files {
        let command: Option<Value> = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let _ = std::fs::remove_file(&file);
        commands.extend(command);
    }
    commands
}

/// The strings of a JSON array (`value`), skipping anything else; none when it is not an array.
pub fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The items of one send from a command's `paths` and optional `text`: each path as a file or
/// folder, then the text when it is not empty. `None` when there is nothing to send.
pub fn send_items(paths: Vec<String>, text: Option<String>) -> Option<Vec<SendItem>> {
    let mut items: Vec<SendItem> = paths
        .into_iter()
        .map(|p| SendItem::Path(PathBuf::from(p)))
        .collect();
    if let Some(text) = text.filter(|t| !t.is_empty()) {
        items.push(SendItem::Text(text));
    }
    if items.is_empty() { None } else { Some(items) }
}
