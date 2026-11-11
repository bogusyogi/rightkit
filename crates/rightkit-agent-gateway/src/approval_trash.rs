//! The delete kind of approval: move ONE thread to Gmail Trash (recoverable
//! for 30 days) when the user clicks Approve. Permanent deletion is not offered
//! to agents in any form: nothing here purges a message, deletes a thread or
//! empties Trash, and no parameter, flag or method reaches such a call.
//!
//! The effect needs an [`ApprovedTrash`] value that only [`finish_delete`] can
//! build (private fields, one constructor, one call site), and `finish_delete`
//! is reached only from `approval::approve`, after the claim (pending ->
//! approved, one guarded UPDATE) was won. The trash call runs with no database
//! lock held, bounded by its own deadline, and an unknown outcome is never
//! retried. The local database is then updated the way the app's own trash
//! action does (`apply_local_action` in the bulk-action engine group, `bulk.rs`).

use super::access::ConnAccess;
use super::approval::{failed, Clock, DecisionView, MailFailure};
use super::approval_store::{Approval, MailApproval, SqlStore};
use super::gate::{record_decision, Decision};
use rightkit_approvals::{Change, State};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_fs::digest::Sha256Hasher;
use serde::Serialize;

/// Proof that the user clicked Approve for this exact delete approval. The
/// fields are private and [`finish_delete`] is the only code that builds one,
/// so the trash call cannot be made without going through it.
#[derive(Debug)]
pub struct ApprovedTrash {
    account: String,
    // Read only by the delete tests' fake trasher; the approval path stays free of test gates.
    #[allow(dead_code)]
    thread_id: String,
    message_ids: Vec<String>,
}

impl ApprovedTrash {
    fn mint(a: &Approval, message_ids: Vec<String>) -> ApprovedTrash {
        ApprovedTrash {
            account: a.account_id().to_string(),
            thread_id: a.draft_id().to_string(),
            message_ids,
        }
    }
    pub fn account(&self) -> &str {
        &self.account
    }
    #[allow(dead_code)]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub fn message_ids(&self) -> &[String] {
        &self.message_ids
    }
}

/// Gmail access for the delete kind, injected. Production: `trash_gmail`.
/// Tests: a recording fake. Called with no database lock held; must be
/// bounded by its own deadline.
pub trait ThreadTrasher: Send + Sync {
    /// Move the thread's messages to Trash. Consumes the proof of approval.
    /// `NoCredentials` means nothing was changed; any other failure may have
    /// come after Gmail acted and is never retried.
    fn trash(&self, approved: ApprovedTrash) -> Result<(), MailFailure>;
}

/// What the gate needs to know about a thread before it asks the user.
pub struct ThreadState {
    pub in_trash: bool,
}

/// `None` when the account has no such thread.
pub fn thread_state(
    conn: &Connection,
    account: &str,
    thread_id: &str,
) -> Result<Option<ThreadState>, String> {
    let known: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM threads WHERE account_id = ?1 AND id = ?2",
            params![account, thread_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("thread lookup: {e}"))?;
    if known.is_none() {
        return Ok(None);
    }
    let in_trash: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM thread_labels WHERE account_id = ?1 AND thread_id = ?2 AND label = 'TRASH'",
            params![account, thread_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("thread lookup: {e}"))?;
    Ok(Some(ThreadState {
        in_trash: in_trash.is_some(),
    }))
}

/// What the delete card shows, read from the local database by id at card
/// time. Plain text from third parties; the window renders it as text only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadSummary {
    pub subject: String,
    pub sender: String,
    /// Epoch milliseconds of the newest message.
    pub date: i64,
    pub message_count: i64,
}

pub fn summary(
    conn: &Connection,
    account: &str,
    thread_id: &str,
) -> Result<Option<ThreadSummary>, String> {
    let row: Option<(Option<String>, i64, i64)> = conn
        .query_row(
            "SELECT subject, last_message_at, message_count FROM threads
             WHERE account_id = ?1 AND id = ?2",
            params![account, thread_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("thread summary: {e}"))?;
    let Some((subject, date, count)) = row else {
        return Ok(None);
    };
    let from: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT from_name, from_addr FROM messages
             WHERE account_id = ?1 AND thread_id = ?2
             ORDER BY internal_date DESC LIMIT 1",
            params![account, thread_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("thread summary: {e}"))?;
    let (name, addr) = from.unwrap_or_default();
    let (name, addr) = (name.unwrap_or_default(), addr.unwrap_or_default());
    let sender = match (name.trim().is_empty(), addr.trim().is_empty()) {
        (false, false) => format!("{} <{}>", name.trim(), addr.trim()),
        (false, true) => name.trim().to_string(),
        _ => addr.trim().to_string(),
    };
    Ok(Some(ThreadSummary {
        subject: subject.unwrap_or_default(),
        sender,
        date,
        message_count: count,
    }))
}

/// What a delete card binds to: a SHA-256 over the thread id and the sorted
/// ids of the thread's messages in the local database, and those ids. The card
/// shows the hash; the approve recomputes it and trashes exactly these ids, or
/// nothing when a message arrived (or left) in between.
pub fn thread_binding(
    conn: &Connection,
    account: &str,
    thread_id: &str,
) -> Result<(String, Vec<String>), String> {
    let mut ids = crate::host::host().thread_message_ids(conn, account, thread_id)?;
    ids.sort();
    ids.dedup();
    let mut h = Sha256Hasher::new();
    let mut feed = |s: &str| {
        h.update(&(s.len() as u64).to_be_bytes());
        h.update(s.as_bytes());
    };
    feed("thread");
    feed(thread_id);
    for id in &ids {
        feed(id);
    }
    Ok((h.finalize(), ids))
}

/// Mirror a trash locally the way the app's own action does: the thread leaves
/// the inbox (and Drafts) and gains Trash, so the lists show it before the next
/// sync.
fn apply_local_trash(conn: &Connection, account: &str, thread: &str) -> Result<(), String> {
    let host = crate::host::host();
    host.local_drop_label(conn, account, thread, "INBOX")?;
    host.local_drop_label(conn, account, thread, "DRAFT")?;
    host.local_add_label(conn, account, thread, "TRASH")
}

/// Run the trash for a claimed delete approval. Called only by
/// `approval::approve`, after the claim was won and the agent's rights were
/// re-checked. No database lock is held while Gmail is called. `shown_hash` is
/// the hash the card showed: if the thread's messages are no longer the ones it
/// hashed, the approval fails with `draft_changed` and nothing is trashed.
pub fn finish_delete<A: ConnAccess>(
    access: &A,
    trasher: &dyn ThreadTrasher,
    clock: &dyn Clock,
    a: &Approval,
    shown_hash: &str,
) -> Result<DecisionView, String> {
    let (hash, ids) = access.read(|c| thread_binding(c, a.account_id(), a.draft_id()))?;
    if ids.is_empty() {
        return failed(access, clock, a, "thread_missing", "messages=0");
    }
    let counts = format!("messages={}", ids.len());
    if hash != shown_hash {
        return failed(access, clock, a, "draft_changed", &counts);
    }
    let token = ApprovedTrash::mint(a, ids);
    match trasher.trash(token) {
        Ok(()) => {
            let done = clock.now_ms();
            access.write(|c| {
                let change = Change::to(State::Done, done.max(0) as u64);
                if SqlStore::new(c, "").move_row(a.id(), State::Approved, &change)? {
                    let local = apply_local_trash(c, a.account_id(), a.draft_id());
                    let note = if local.is_ok() {
                        counts.clone()
                    } else {
                        format!("{counts} local=pending_sync")
                    };
                    record_decision(c, a, Decision::Sent, &note)?;
                }
                Ok(())
            })?;
            Ok(DecisionView {
                status: "trashed",
                reason: None,
            })
        }
        Err(MailFailure::NoCredentials) => failed(access, clock, a, "no_credentials", &counts),
        // A timeout or a Gmail error may have come after Gmail acted: never
        // retried, never reported as not trashed.
        Err(_) => failed(access, clock, a, "unknown_outcome", &counts),
    }
}
