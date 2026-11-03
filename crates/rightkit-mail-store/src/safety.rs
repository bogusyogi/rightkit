// Mutation guard — the audit layer every remote write passes through before it
// changes Google-side state. Each action is classified by blast radius and
// every guarded call writes an audit_log row.
//
// Classes, rising danger:
//   SafeMutation        — trivially reversible (mark read/unread, star, label)
//   ReversibleMutation  — reversible within a window (archive, trash, send+undo)
//
// A class with no caller (read-only, irreversible) is not kept here: the app
// offers no permanent delete, so nothing could construct it.

use rightkit_db::rusqlite::Connection;

/// Developer flag (settings key) the UI reads to show destructive actions.
pub const DESTRUCTIVE_FLAG: &str = "destructive_remote_actions_enabled";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationClass {
    SafeMutation,
    ReversibleMutation,
}

impl MutationClass {
    fn label(self) -> &'static str {
        match self {
            MutationClass::SafeMutation => "safe",
            MutationClass::ReversibleMutation => "reversible",
        }
    }
}

/// Permit `action` of the given class. Records an `audit_log` row (`mutation`).
pub fn guard(
    conn: &Connection,
    class: MutationClass,
    action: &str,
    account_id: Option<&str>,
) -> Result<(), String> {
    crate::db::audit(
        conn,
        "mutation",
        account_id,
        &format!("{} action: {action}", class.label()),
    );
    Ok(())
}
