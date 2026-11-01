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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn safe_mutation_always_allowed() {
        let conn = db::test_conn();
        assert!(guard(
            &conn,
            MutationClass::SafeMutation,
            "mark_read",
            Some("a@b.com")
        )
        .is_ok());
    }

    fn audit_log_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap()
    }

    /// L2 regression lock: `guard`'s only write is the `audit_log` INSERT
    /// (`crate::db::audit`, whose `execute` result is discarded via `let _ =`).
    /// Run on a normal writable connection — the shape every guard call site
    /// must use for the guard step after the L2 fixup (`guard_on_writer` in
    /// `commands/mod.rs`) — the row must actually land. This is the exact
    /// write that silently no-op'd when L2 routed guard through the reader's
    /// `PRAGMA query_only = ON` connection (see the `query_only_drops_audit_write`
    /// test below for the failure mode itself).
    #[test]
    fn guard_on_writable_conn_writes_audit_row() {
        let conn = db::test_conn();
        assert_eq!(audit_log_count(&conn), 0);

        guard(
            &conn,
            MutationClass::SafeMutation,
            "mark_read",
            Some("a@b.com"),
        )
        .unwrap();

        assert_eq!(
            audit_log_count(&conn),
            1,
            "guard on a writable connection must persist an audit_log row"
        );
    }

    /// Documents the regression itself: a `query_only` connection (what the
    /// reader pool uses) silently swallows guard's audit INSERT instead of
    /// erroring, because `db::audit` discards the write's `Result`. This is
    /// exactly why guard's audit write must run on the writer
    /// (`guard_on_writer`), never on `with_reader`.
    #[test]
    fn query_only_drops_audit_write() {
        let conn = db::test_conn();
        conn.execute_batch("PRAGMA query_only = ON;").unwrap();

        // guard() itself still reports Ok — the dropped write is silent.
        let r = guard(
            &conn,
            MutationClass::SafeMutation,
            "mark_read",
            Some("a@b.com"),
        );
        assert!(r.is_ok());
        assert_eq!(
            audit_log_count(&conn),
            0,
            "query_only must silently drop the audit row (the L2 regression)"
        );
    }
}
