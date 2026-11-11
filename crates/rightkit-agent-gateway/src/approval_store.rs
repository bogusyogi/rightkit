//! Database side of send approvals: the agent-created draft record, and the
//! approval rows as a `rightkit-approvals` [`Store`] ([`SqlStore`]). The record
//! and the rules (one decision, one claim, the pending cap) are the core's; this
//! file keeps the table, the mail limits (send and delete windows, decline
//! cooldown) and the run binding. A move is one UPDATE guarded by the status it
//! expects, so two deciders can never both win. Rows hold ids, times and reason
//! codes only. The only audit rows written here come from `sweep`, through the
//! gate module's `record_decision`; every other move is audited by its caller.

use super::gate::{record_decision, Decision};
use rightkit_approvals::{Binding, Change, Origin, Record, Refusal, Request, State, Store};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};

/// An approval waits this long, then it is denied with reason `timeout`.
pub const TIMEOUT_MS: i64 = 15 * 60 * 1000;
/// A send that was claimed but never finished is failed after this long.
pub const STUCK_MS: i64 = 2 * 60 * 1000;
/// Most pending approvals one agent may have.
pub const PENDING_CAP: i64 = 5;
/// Send limits, per agent and per account. Constants: no setting raises them.
pub const HOURLY_LIMIT: i64 = 10;
pub const DAILY_LIMIT: i64 = 50;
pub const HOUR_MS: i64 = 60 * 60 * 1000;
pub const DAY_MS: i64 = 24 * HOUR_MS;
/// Delete limits (threads moved to Trash), per agent and per account.
pub const DELETE_HOURLY_LIMIT: i64 = 20;
pub const DELETE_DAILY_LIMIT: i64 = 100;
/// After the user denies a request, the same agent may not ask for the same
/// target again for this long. A timeout does not count.
pub const DECLINE_COOLDOWN_MS: i64 = 10 * 60 * 1000;

/// An approval is the core's record (`rightkit_approvals::Record`): `kind` is send, forward,
/// delete or attach (see `approval_kind`), `scope` the account, `origin.client` the agent,
/// `binding.destination` the draft (for a delete the thread; for an attach the target key), and
/// `detail.target_key` the key of the decline cooldown. [`MailApproval`] reads it in this
/// crate's words.
pub type Approval = Record;

/// The mail words for the core record's fields.
pub trait MailApproval {
    fn id(&self) -> &str;
    fn agent_id(&self) -> &str;
    fn account_id(&self) -> &str;
    /// The draft id; for a delete the thread id.
    fn draft_id(&self) -> &str;
    fn kind(&self) -> &str;
    fn created_at(&self) -> i64;
    fn expires_at(&self) -> i64;
    /// pending | approved | sent | denied | failed, as the table and the agent see it.
    fn status(&self) -> &'static str;
}

impl MailApproval for Record {
    fn id(&self) -> &str {
        &self.request.id
    }
    fn agent_id(&self) -> &str {
        &self.request.origin.client
    }
    fn account_id(&self) -> &str {
        &self.request.scope
    }
    fn draft_id(&self) -> &str {
        &self.request.binding.destination
    }
    fn kind(&self) -> &str {
        &self.request.kind
    }
    fn created_at(&self) -> i64 {
        ms_i64(self.request.created_ms)
    }
    fn expires_at(&self) -> i64 {
        ms_i64(self.request.expires_ms)
    }
    fn status(&self) -> &'static str {
        status_word(self.state)
    }
}

fn ms_u64(v: i64) -> u64 {
    v.max(0) as u64
}

fn ms_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The table's status word for a core state. A timeout is stored as `denied` with reason
/// `timeout`, as before. The table has no claimed status (its `approved` row is the send in
/// flight, settled by `sweep`), so a move to or from `Claimed` is refused before it gets here.
fn status_word(state: State) -> &'static str {
    match state {
        State::Pending => "pending",
        // Never reached for `Claimed` (`move_row` refuses it).
        State::Approved | State::Claimed => "approved",
        State::Done => "sent",
        State::Denied | State::Expired => "denied",
        State::Failed => "failed",
    }
}

fn state_of(status: &str, reason: Option<&str>) -> State {
    match status {
        "pending" => State::Pending,
        "approved" => State::Approved,
        "sent" => State::Done,
        "denied" if reason == Some("timeout") => State::Expired,
        "denied" => State::Denied,
        _ => State::Failed,
    }
}

const COLS: &str = "id, agent_id, account_id, draft_id, created_at, expires_at, boot_id, status, reason, decided_at, result_id, kind, target_key";

fn row(r: &rightkit_db::rusqlite::Row<'_>) -> rightkit_db::rusqlite::Result<Approval> {
    let status: String = r.get(7)?;
    let reason: Option<String> = r.get(8)?;
    let target_key: Option<String> = r.get(12)?;
    Ok(Record {
        request: Request {
            id: r.get(0)?,
            kind: r.get(11)?,
            scope: r.get(2)?,
            origin: Origin {
                door: String::new(),
                filer: String::new(),
                client: r.get(1)?,
            },
            binding: Binding::new("", r.get::<_, String>(3)?),
            summary: String::new(),
            detail: match target_key {
                Some(key) => serde_json::json!({ "target_key": key }),
                None => serde_json::Value::Null,
            },
            created_ms: ms_u64(r.get(4)?),
            expires_ms: ms_u64(r.get(5)?),
        },
        state: state_of(&status, reason.as_deref()),
        decided_ms: ms_u64(r.get::<_, Option<i64>>(9)?.unwrap_or(0)),
        last_used_ms: 0,
        grant: None,
        reason,
        result: r.get(10)?,
    })
}

fn db(what: &str, e: rightkit_db::rusqlite::Error) -> String {
    format!("approval {what}: {e}")
}

fn refusal(e: String) -> Refusal {
    Refusal::new("db", e)
}

/// The `agent_approvals` table as a `rightkit-approvals` store, for one run of the app
/// (`boot`): a pending row of another run is never live, and a claim (pending to approved) also
/// requires the row to be unexpired and from this run, in the same guarded UPDATE.
pub struct SqlStore<'c> {
    conn: &'c Connection,
    boot: String,
}

impl<'c> SqlStore<'c> {
    pub fn new(conn: &'c Connection, boot: &str) -> SqlStore<'c> {
        SqlStore {
            conn,
            boot: boot.to_string(),
        }
    }

    fn list(
        &self,
        sql: &str,
        p: &[&dyn rightkit_db::rusqlite::ToSql],
    ) -> Result<Vec<Approval>, String> {
        let mut st = self.conn.prepare(sql).map_err(|e| db("list", e))?;
        let rows = st.query_map(p, row).map_err(|e| db("list", e))?;
        rows.map(|r| r.map_err(|e| db("list", e))).collect()
    }

    /// Close what can no longer be decided: pending rows past their expiry or left by an earlier
    /// run become denied/timeout; in-flight sends that never finished become
    /// failed/unknown_outcome. Each move is audited once by the gate.
    pub fn sweep(&self, now: i64) -> Result<(), String> {
        let pending = self.list(
            &format!(
                "SELECT {COLS} FROM agent_approvals
                 WHERE status = 'pending' AND (expires_at <= ?1 OR boot_id <> ?2)"
            ),
            &[&now, &self.boot],
        )?;
        for a in pending {
            let change = Change::to(State::Expired, ms_u64(now)).reason("timeout");
            if self.move_row(a.id(), State::Pending, &change)? {
                record_decision(self.conn, &a, Decision::Denied, "reason=timeout")?;
            }
        }
        let stuck = self.list(
            &format!(
                "SELECT {COLS} FROM agent_approvals
                 WHERE status = 'approved' AND (decided_at <= ?1 OR boot_id <> ?2)"
            ),
            &[&(now - STUCK_MS), &self.boot],
        )?;
        for a in stuck {
            let change = Change::to(State::Failed, ms_u64(now)).reason("unknown_outcome");
            if self.move_row(a.id(), State::Approved, &change)? {
                record_decision(
                    self.conn,
                    &a,
                    Decision::SendFailed,
                    "reason=unknown_outcome",
                )?;
            }
        }
        Ok(())
    }

    /// One guarded UPDATE: `id` moves only while it is in `from` (a claim also only while
    /// unexpired and from this run). True only for the caller that made the move.
    /// A move to or from `Claimed` is refused: the table's in-flight send is its `approved` row.
    pub fn move_row(&self, id: &str, from: State, change: &Change) -> Result<bool, String> {
        if from == State::Claimed || change.to == State::Claimed {
            return Err("approval: this store keeps no claimed approvals".to_string());
        }
        let at = ms_i64(change.at_ms);
        let n = if from == State::Pending && change.to == State::Approved {
            self.conn
                .execute(
                    "UPDATE agent_approvals SET status = 'approved', decided_at = ?2
                     WHERE id = ?1 AND status = 'pending' AND expires_at > ?2 AND boot_id = ?3",
                    params![id, at, self.boot],
                )
                .map_err(|e| db("claim", e))?
        } else {
            let reason = match (change.to, change.reason.as_deref()) {
                (State::Expired, None) => Some("timeout"),
                (_, reason) => reason,
            };
            self.conn
                .execute(
                    "UPDATE agent_approvals SET status = ?3, reason = ?4, decided_at = ?5, result_id = ?6
                     WHERE id = ?1 AND status = ?2",
                    params![
                        id,
                        status_word(from),
                        status_word(change.to),
                        reason,
                        at,
                        change.result
                    ],
                )
                .map_err(|e| db("update", e))?
        };
        Ok(n == 1)
    }
}

impl Store for SqlStore<'_> {
    fn insert(&self, a: &Record) -> rightkit_approvals::Result<()> {
        let target_key = a.request.detail["target_key"]
            .as_str()
            .unwrap_or(a.draft_id())
            .to_string();
        self.conn
            .execute(
                "INSERT INTO agent_approvals(id, agent_id, account_id, draft_id, created_at, expires_at, boot_id, status, kind, target_key)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9)",
                params![
                    a.id(),
                    a.agent_id(),
                    a.account_id(),
                    a.draft_id(),
                    a.created_at(),
                    a.expires_at(),
                    self.boot,
                    a.kind(),
                    target_key
                ],
            )
            .map(|_| ())
            .map_err(|e| refusal(db("insert", e)))
    }

    fn get(&self, id: &str) -> rightkit_approvals::Result<Option<Record>> {
        self.conn
            .query_row(
                &format!("SELECT {COLS} FROM agent_approvals WHERE id = ?1"),
                [id],
                row,
            )
            .optional()
            .map_err(|e| refusal(db("read", e)))
    }

    /// Pending approvals of this run that have not expired, oldest first.
    fn pending(&self, now_ms: u64) -> rightkit_approvals::Result<Vec<Record>> {
        self.list(
            &format!(
                "SELECT {COLS} FROM agent_approvals
                 WHERE status = 'pending' AND boot_id = ?1 AND expires_at > ?2
                 ORDER BY created_at, rowid"
            ),
            &[&self.boot, &ms_i64(now_ms)],
        )
        .map_err(refusal)
    }

    fn approved(&self, now_ms: u64) -> rightkit_approvals::Result<Vec<Record>> {
        self.list(
            &format!(
                "SELECT {COLS} FROM agent_approvals
                 WHERE status = 'approved' AND boot_id = ?1 AND decided_at > ?2
                 ORDER BY created_at, rowid"
            ),
            &[&self.boot, &(ms_i64(now_ms) - STUCK_MS)],
        )
        .map_err(refusal)
    }

    /// None: the table has no claimed status (see `transition`).
    fn claimed(&self) -> rightkit_approvals::Result<Vec<Record>> {
        Ok(Vec::new())
    }

    /// A move to or from `Claimed` is refused (`move_row`).
    fn transition(
        &self,
        id: &str,
        from: State,
        change: &Change,
    ) -> rightkit_approvals::Result<bool> {
        self.move_row(id, from, change).map_err(refusal)
    }

    /// All pending rows of an agent (or all), whatever their run: the cap counts what the sweep
    /// left.
    fn count_pending(
        &self,
        client: Option<&str>,
        _now_ms: u64,
    ) -> rightkit_approvals::Result<usize> {
        let n: i64 = match client {
            Some(agent) => self.conn.query_row(
                "SELECT COUNT(*) FROM agent_approvals WHERE agent_id = ?1 AND status = 'pending'",
                [agent],
                |r| r.get(0),
            ),
            None => self.conn.query_row(
                "SELECT COUNT(*) FROM agent_approvals WHERE status = 'pending'",
                [],
                |r| r.get(0),
            ),
        }
        .map_err(|e| refusal(db("count", e)))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

/// Remember a draft the agent created, with the hash of what was validated.
pub fn record_draft(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    hash: &str,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_drafts(agent_id, account_id, draft_id, created_at, content_hash)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![agent_id, account, draft_id, now, hash],
    )
    .map(|_| ())
    .map_err(|e| db("record draft", e))
}

/// [`record_draft`] for a draft that carries attachments: also stores the
/// digest over their names, types, sizes and bytes. The digest is stored only;
/// approve does not compare it.
pub fn record_draft_with_digest(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    hash: &str,
    attachment_digest: &str,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_drafts(agent_id, account_id, draft_id, created_at, content_hash, attachment_digest)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![agent_id, account, draft_id, now, hash, attachment_digest],
    )
    .map(|_| ())
    .map_err(|e| db("record draft", e))
}

/// Remember a forward draft the app built for this agent. Only a record made
/// here has kind `forward`; `send` accepts `draft` records alone, so a forward
/// draft can never be approved under the plain Send heading.
pub fn record_forward_draft(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    hash: &str,
    attachment_digest: &str,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_drafts(agent_id, account_id, draft_id, created_at, content_hash, kind, attachment_digest)
         VALUES(?1, ?2, ?3, ?4, ?5, 'forward', ?6)",
        params![agent_id, account, draft_id, now, hash, attachment_digest],
    )
    .map(|_| ())
    .map_err(|e| db("record forward draft", e))
}

/// The recorded hash when this agent created this draft with `draft` (not a
/// forward draft) in this account.
pub fn draft_hash(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT content_hash FROM agent_drafts
         WHERE agent_id = ?1 AND account_id = ?2 AND draft_id = ?3 AND kind = 'draft'",
        params![agent_id, account, draft_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db("draft lookup", e))
}

/// The recorded hash for a draft of either kind (the approval card compares it).
pub fn any_draft_hash(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT content_hash FROM agent_drafts WHERE agent_id = ?1 AND account_id = ?2 AND draft_id = ?3",
        params![agent_id, account, draft_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db("draft lookup", e))
}

/// True when the user denied (reason `user`, not a timeout) a request of this
/// kind by this agent for this target after `since` (exclusive).
pub fn declined_since(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    kind: &str,
    target_key: &str,
    since: i64,
) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM agent_approvals
         WHERE agent_id = ?1 AND account_id = ?2 AND kind = ?3 AND target_key = ?4
           AND status = 'denied' AND reason = 'user' AND decided_at > ?5
         LIMIT 1",
        params![agent_id, account, kind, target_key, since],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .map(|v| v.is_some())
    .map_err(|e| db("decline lookup", e))
}

/// The newest approval of `kind` for this draft (a delete: this thread) that is
/// pending or in flight or, for the mail kinds, already sent. A thread that was
/// trashed (and perhaps restored since) can be asked about again.
pub fn live_for_draft(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    kind: &str,
) -> Result<Option<Approval>, String> {
    let states = if kind == super::approval_kind::DELETE || kind == super::approval_kind::ATTACH {
        "('pending','approved')"
    } else {
        "('pending','approved','sent')"
    };
    conn.query_row(
        &format!(
            "SELECT {COLS} FROM agent_approvals
             WHERE agent_id = ?1 AND account_id = ?2 AND draft_id = ?3 AND kind = ?4
               AND status IN {states}
             ORDER BY created_at DESC LIMIT 1"
        ),
        params![agent_id, account, draft_id, kind],
        row,
    )
    .optional()
    .map_err(|e| db("lookup", e))
}

/// Remember the exact path of a new `attach` approval.
pub fn record_attach_path(conn: &Connection, approval_id: &str, path: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_attach_requests(approval_id, path) VALUES(?1, ?2)",
        params![approval_id, path],
    )
    .map(|_| ())
    .map_err(|e| db("record attach path", e))
}

/// The path an `attach` approval asks about.
pub fn attach_path(conn: &Connection, approval_id: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT path FROM agent_attach_requests WHERE approval_id = ?1",
        [approval_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db("attach path", e))
}

/// Requests created since `since` (exclusive) by one agent (`by_agent`) or
/// into one account, whatever became of them: each one interrupted the user.
/// `deletes` counts the delete kind; otherwise sends and forwards together.
pub fn count_sends(
    conn: &Connection,
    by_agent: bool,
    key: &str,
    since: i64,
    deletes: bool,
) -> Result<i64, String> {
    let col = if by_agent { "agent_id" } else { "account_id" };
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM agent_approvals
             WHERE {col} = ?1 AND created_at > ?2 AND (kind = 'delete') = ?3"
        ),
        params![key, since, deletes],
        |r| r.get(0),
    )
    .map_err(|e| db("count", e))
}
