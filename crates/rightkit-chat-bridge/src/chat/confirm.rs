//! `<cli> chat confirm <message-id> [--summary "<what will be done>"]`: the
//! receiving agent asks the owner's iPhone before a reserved action (destruction, spend,
//! publication, production changes, the host app settings, device management) that a leased guest or the
//! server asked for through chat.
//!
//! The sender is the paired device the message arrived from over the device link, remembered with
//! the reply route (`ReplyRoute::mesh_device_id`, stamped by the receiving engine, never by the
//! sender). The engine reads that device's role from its registry (`device.chat.confirm`): a
//! computer or phone needs nothing (`not required`); a guest or server waits for a phone
//! confirmation, which this module polls every five seconds for up to ten minutes
//! (`device.chat.confirm.status`; the phone only answers when it next looks). The engine call is
//! passed in, so this module holds the decision logic and no connection.
//!
//! Exit codes: 0 approved or not required; 2 denied; 3 expired, unknown, or no way to ask (no
//! route, an engine that did not answer, no phone key enrolled). An unknown answer is never a yes.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::store::Store;

/// `ReplyRoute::mesh_device_id` of a message from a chat on this computer.
pub const LOCAL_SENDER: &str = "local";
/// How often the engine is asked whether the phone answered.
pub const POLL: Duration = Duration::from_secs(5);
/// The request lives ten minutes in the engine; the poll stops a little after it.
pub const WAIT: Duration = Duration::from_secs(10 * 60 + 30);
/// Consecutive failed polls after which the answer is unknown.
const POLL_FAILURES: u32 = 6;

/// A failed engine call: its stable code and the words.
#[derive(Debug, Clone)]
pub struct CallError {
    pub code: String,
    pub message: String,
}

/// How a confirmation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The sender is one of the owner's computers: nothing to confirm.
    NotRequired,
    /// The phone approved this request (once).
    Approved { id: String },
    /// The owner declined on the phone.
    Denied { id: String },
    /// Nobody answered within ten minutes.
    Expired { id: String },
    /// Nothing proves a yes: the reason in words, and the request id when one was made.
    Unknown { id: Option<String>, why: String },
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::NotRequired | Outcome::Approved { .. } => 0,
            Outcome::Denied { .. } => 2,
            Outcome::Expired { .. } | Outcome::Unknown { .. } => 3,
        }
    }

    fn id(&self) -> Option<&str> {
        match self {
            Outcome::Approved { id } | Outcome::Denied { id } | Outcome::Expired { id } => Some(id),
            Outcome::Unknown { id, .. } => id.as_deref(),
            Outcome::NotRequired => None,
        }
    }

    fn state(&self) -> &'static str {
        match self {
            Outcome::NotRequired => "not_required",
            Outcome::Approved { .. } => "approved",
            Outcome::Denied { .. } => "denied",
            Outcome::Expired { .. } => "expired",
            Outcome::Unknown { .. } => "unknown",
        }
    }

    /// The one line printed: the outcome and, when there is one, the challenge id.
    pub fn line(&self) -> String {
        let id = self
            .id()
            .map(|id| format!(" (challenge {id})"))
            .unwrap_or_default();
        match self {
            Outcome::NotRequired => "not required: sender is one of your computers".to_string(),
            Outcome::Approved { .. } => format!("approved on your iPhone{id}"),
            Outcome::Denied { .. } => format!("denied on your iPhone{id}: do not proceed"),
            Outcome::Expired { .. } => {
                format!("expired: nobody answered on your iPhone in 10 minutes{id}: do not proceed")
            }
            Outcome::Unknown { why, .. } => format!("unknown: {why}{id}: do not proceed"),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.exit_code() == 0,
            "state": self.state(),
            "id": self.id(),
            "message": self.line(),
        })
    }
}

fn unknown(id: Option<&str>, why: impl Into<String>) -> Outcome {
    Outcome::Unknown {
        id: id.map(str::to_string),
        why: why.into(),
    }
}

/// Ask for, then wait for, the phone's confirmation of the request in message `message_id`.
/// `call` is one engine method call (`device.chat.confirm`, `device.chat.confirm.status`).
/// `waiting` runs once, when a phone has been asked, so the CLI can say it is waiting.
pub fn confirm(
    store: &Store,
    message_id: &str,
    summary: Option<&str>,
    call: &dyn Fn(&str, Value) -> Result<Value, CallError>,
    waiting: &dyn Fn(&str),
) -> Outcome {
    let Some(route) = store.reply_route(message_id) else {
        return unknown(
            None,
            "no route is kept for that message id (it is not one delivered here in the last 30 days)",
        );
    };
    let sender = match route.mesh_device_id.as_deref() {
        // A chat on this computer is the owner's own.
        Some(LOCAL_SENDER) => return Outcome::NotRequired,
        Some(id) if !id.is_empty() => id.to_string(),
        // No registry device resolves (an ssh link with no device-link twin, or an older route):
        // the role cannot be read, so there is no yes from it.
        _ => {
            return unknown(
                None,
                "the sender cannot be tied to a paired device, so its role cannot be checked",
            );
        }
    };
    let Some(summary) = summary.map(str::trim).filter(|s| !s.is_empty()) else {
        return unknown(
            None,
            "--summary is required: describe exactly what you intend to do",
        );
    };
    let asked = match call(
        "device.chat.confirm",
        json!({
            "messageId": message_id,
            "senderDeviceId": sender,
            "chat": route.to_name,
            "summary": summary,
        }),
    ) {
        Ok(answer) => answer,
        Err(error) => return unknown(None, format!("{} ({})", error.message, error.code)),
    };
    match asked["state"].as_str() {
        Some("not_required") => return Outcome::NotRequired,
        Some("pending") => {}
        _ => return unknown(None, "the engine gave no usable answer"),
    }
    let Some(id) = asked["id"].as_str().map(str::to_string) else {
        return unknown(None, "the engine gave no challenge id");
    };
    waiting(&id);
    let started = Instant::now();
    let mut failures = 0u32;
    loop {
        std::thread::sleep(POLL);
        match call(
            "device.chat.confirm.status",
            json!({ "id": id, "senderDeviceId": sender }),
        ) {
            Ok(answer) => {
                failures = 0;
                match answer["state"].as_str() {
                    Some("approved") => return Outcome::Approved { id },
                    Some("denied") => return Outcome::Denied { id },
                    Some("expired") => return Outcome::Expired { id },
                    Some("pending") => {}
                    _ => return unknown(Some(&id), "the engine no longer holds this request"),
                }
            }
            Err(error) => {
                failures += 1;
                if failures >= POLL_FAILURES {
                    return unknown(
                        Some(&id),
                        format!("the engine stopped answering ({})", error.code),
                    );
                }
            }
        }
        if started.elapsed() >= WAIT {
            return Outcome::Expired { id };
        }
    }
}
