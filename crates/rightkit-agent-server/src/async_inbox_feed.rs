//! A097: the daemon is a producer for each session's async inbox.
//!
//! The engine already queues background-shell completions and deferred tool-call resolutions
//! for the session that launched them. Events the daemon itself observes on behalf of a
//! session, such as a cross-session `send_message` / broadcast delivery, are queued here so the
//! receiving agent can read them with the `async_inbox` tool outside the live turn.

use serde_json::json;

/// Queue a relayed session message into `target_session_id`'s inbox. Returns the inbox message
/// id, or `None` when that session is not live (a retired session never receives late events).
pub(crate) fn feed_session_message(
    target_session_id: &str,
    from_session_id: &str,
    channel: &str,
    message: &str,
) -> Option<u64> {
    rightkit_agent_tools::coding_tools::async_inbox::push(
        target_session_id,
        "session_message",
        format!("Message from {from_session_id} via {channel}."),
        Some(json!({
            "fromSessionId": from_session_id,
            "channel": channel,
            "message": message,
        })),
    )
}
