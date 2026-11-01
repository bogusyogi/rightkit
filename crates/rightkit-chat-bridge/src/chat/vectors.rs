//! A stable, narrow surface for RightKit's characterization harness
//! (`docs/chat-vectors`). Each function calls the real code and adds no behaviour of
//! its own. Nothing in the host app should call this module.

use super::deliver_claude;
use super::envelope::{Envelope, Sender, Target};

/// The sender label a Claude chat is shown (`deliver_claude::sender_label`).
pub fn sender_label(name: &str) -> String {
    deliver_claude::sender_label(name)
}

/// `from.name` after `Envelope::new` has cut it to 256 bytes at a character boundary.
pub fn envelope_sender_name(name: &str) -> String {
    let from = Sender {
        harness: String::new(),
        device_id: String::new(),
        device: String::new(),
        session: String::new(),
        name: name.to_string(),
    };
    Envelope::new(from, Target::default(), "")
        .map(|envelope| envelope.from.name)
        .unwrap_or_default()
}

/// The `<cross-session-message>` wrapper (`deliver_claude::wrap_content`). An oversized text,
/// which the real code refuses before wrapping, gives an empty string here.
pub fn wrap_content(from: &str, session: &str, name: &str, text: &str) -> String {
    deliver_claude::wrap_content(from, session, name, text).unwrap_or_default()
}

/// The text a Claude chat reads for an envelope: header line, then the body.
pub fn agent_text(env: &Envelope) -> String {
    deliver_claude::agent_text(env)
}

/// The line `deliver_via` writes to a Claude peer's socket for a user message: compact
/// JSON and a newline. On Windows the real delivery writes an auth frame before it.
pub fn claude_frame(env: &Envelope, peer_key: &str, reply_from: Option<&str>) -> String {
    deliver_claude::user_frame(env, peer_key, reply_from)
        .and_then(|frame| deliver_claude::frame_line(&frame))
        .unwrap_or_default()
}
