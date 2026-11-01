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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_side_message_appears_in_the_sessions_inbox() {
        let session = "a097-daemon-feed-live";
        let _lease = rightkit_agent_tools::coding_tools::async_inbox::activate_session(session);
        let id = feed_session_message(session, "s-sender", "team", "build is green").unwrap();

        let (messages, dropped) = rightkit_agent_tools::coding_tools::async_inbox::read(session, 10);
        assert_eq!(dropped, 0);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, id);
        assert_eq!(messages[0].channel, "session_message");
        assert_eq!(
            messages[0].data.as_ref().unwrap()["message"],
            "build is green"
        );
        assert_eq!(
            messages[0].data.as_ref().unwrap()["fromSessionId"],
            "s-sender"
        );
        rightkit_agent_tools::coding_tools::async_inbox::retire_session(session);
    }

    #[test]
    fn retired_or_unknown_session_gets_nothing() {
        let session = "a097-daemon-feed-retired";
        let _lease = rightkit_agent_tools::coding_tools::async_inbox::activate_session(session);
        rightkit_agent_tools::coding_tools::async_inbox::retire_session(session);
        assert_eq!(feed_session_message(session, "s", "c", "late"), None);
        assert_eq!(rightkit_agent_tools::coding_tools::async_inbox::pending_count(session), 0);
    }
}
