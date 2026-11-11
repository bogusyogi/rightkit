//! The three kinds of request that wait for the user's click in the app window.
//! All three share one approval table, state machine, expiry, pending cap and
//! exactly-once claim; the kind only picks the wording, the limits that count
//! it and which effect the Approve click may run:
//!
//! * `send`: send a draft the agent made;
//! * `forward`: send a forward draft the app built from an existing message;
//! * `delete`: move one thread to Trash (recoverable; never a permanent delete);
//! * `attach`: copy one file the user approved, by its exact absolute path and
//!   size, into the agent's export folder (see `attach_path`).

pub const SEND: &str = "send";
pub const FORWARD: &str = "forward";
pub const DELETE: &str = "delete";
pub const ATTACH: &str = "attach";

/// Audit method and gate method name for a kind.
pub fn method(kind: &str) -> &'static str {
    match kind {
        FORWARD => FORWARD,
        DELETE => DELETE,
        ATTACH => ATTACH,
        _ => SEND,
    }
}

/// The app's mutation-guard action recorded when the user approves.
pub fn guard_action(kind: &str) -> &'static str {
    match kind {
        FORWARD => "agent_forward",
        DELETE => "agent_delete",
        ATTACH => "agent_attach",
        _ => "agent_send",
    }
}

/// Status word the agent and the window see for a finished request.
pub fn done_word(kind: &str) -> &'static str {
    if kind == DELETE {
        "trashed"
    } else if kind == ATTACH {
        "attached"
    } else {
        "sent"
    }
}

/// Fixed sentence in the reply to the agent while the request waits.
pub fn pending_note(kind: &str) -> String {
    let what = match kind {
        FORWARD => "Nothing is forwarded until they do.",
        DELETE => "Nothing is moved to Trash until they do.",
        ATTACH => "The file is not read until they do.",
        _ => "Nothing is sent until they do.",
    };
    format!(
        "The user must approve this in the {} window. {what} Poll `approval` for the result.",
        crate::host::product()
    )
}
