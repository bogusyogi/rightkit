//! The user's side of a send, a forward or a delete: list pending approvals,
//! show the real draft (or the thread), approve or deny. These functions are
//! reached only from the `approvals.*` engine methods (approvals.rs in the engine facade),
//! which the app's own webview calls through its Tauri commands (and, over the
//! core's UI pipe, only with the session secret); they are not in the
//! agent method table and no agent request, flag, setting or environment
//! variable reaches them.
//!
//! Approve is the only place the sender can be called, and it needs an
//! [`Approved`] value that only this file can build. Order of an approve:
//! locked check, then one short write closure that sweeps expired rows,
//! re-checks the agent's rights, claims the row (pending -> approved, one
//! guarded UPDATE, unexpired and from this run) and audits `approved`; then,
//! with NO database lock held, the draft is fetched again and its hash compared
//! with the hash the card showed; only then is the sender called, once. A
//! delete skips the draft and goes to `approval_trash::finish_delete`, which
//! recomputes the thread's hash (thread id and message ids), refuses on a
//! mismatch and alone can build the sibling `ApprovedTrash` token.

use super::access::{ConnAccess, LOCKED_MESSAGE};
use super::approval_attach;
use super::approval_kind as kinds;
use super::approval_store::{Approval, MailApproval, SqlStore, STUCK_MS};
use super::approval_trash::{self, ThreadSummary, ThreadTrasher};
use super::attach_path::{AttachFiles, AttachSummary};
use super::draft_view::{AttachmentInfo, FetchedDraft};
use super::gate::{record_decision, Decision};
use super::registry;
use rightkit_approvals::rules::{self, Decision as Ruling};
use rightkit_approvals::{Change, Grant, State, Store};
use serde::Serialize;
use std::sync::OnceLock;

/// Time and run identity, injected so tests control both.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
    /// Identifies this run of the app. A pending approval from another run can
    /// never be approved.
    fn boot_id(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        rightkit_search::values::now_ms()
    }
    fn boot_id(&self) -> String {
        static BOOT: OnceLock<String> = OnceLock::new();
        BOOT.get_or_init(|| {
            let mut b = [0u8; 12];
            let _ = getrandom::fill(&mut b);
            rightkit_fs::digest::hex(&b)
        })
        .clone()
    }
}

/// Proof that the user clicked Approve for this exact approval, with the draft
/// unchanged. The fields are private and [`approve`] is the only code that
/// builds one, so the sender cannot be called without going through it.
#[derive(Debug)]
pub struct Approved {
    account: String,
    draft_id: String,
}

impl Approved {
    fn mint(a: &Approval) -> Approved {
        Approved {
            account: a.account_id().to_string(),
            draft_id: a.draft_id().to_string(),
        }
    }
    pub fn account(&self) -> &str {
        &self.account
    }
    pub fn draft_id(&self) -> &str {
        &self.draft_id
    }
}

/// Why a fetch or a delivery did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailFailure {
    NoCredentials,
    Timeout,
    Gmail,
}

/// Gmail access for the approval flow, injected. Production: the app's Gmail
/// path. Tests: a recording fake. Both calls run with no database lock held and
/// must be bounded by their own deadline.
pub trait DraftMailer: Send + Sync {
    /// Read the draft as Gmail holds it now. Read only.
    fn fetch(&self, account: &str, draft_id: &str) -> Result<FetchedDraft, MailFailure>;
    /// Send the draft. Consumes the proof of approval. Returns the sent
    /// message id.
    fn deliver(&self, approved: Approved) -> Result<String, MailFailure>;
}

/// What the window learns about a decision. No content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionView {
    /// sent | trashed (a delete) | denied | failed | approved
    pub status: &'static str,
    /// user | timeout | draft_changed | unknown_outcome | no_credentials |
    /// fetch_failed | not_permitted | thread_missing
    pub reason: Option<String>,
}

fn view(a: &Approval) -> DecisionView {
    let status = match a.state {
        State::Done => kinds::done_word(a.kind()),
        State::Denied | State::Expired => "denied",
        State::Failed => "failed",
        _ => "approved",
    };
    DecisionView {
        status,
        reason: a.reason.clone(),
    }
}

/// A pending approval for the queue. No content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingView {
    pub id: String,
    /// send | forward | delete
    pub kind: String,
    pub agent_name: String,
    pub account: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// What the card shows, fetched from Gmail at display time (a delete: read
/// from the local database by id). For a delete only `thread` is filled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApprovalCard {
    pub id: String,
    /// send | forward | delete
    pub kind: String,
    /// The conversation a delete would move to Trash.
    pub thread: Option<ThreadSummary>,
    pub agent_name: String,
    pub account: String,
    pub expires_at: i64,
    pub reply_to: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub html_only: bool,
    /// True when the HTML part reads differently from the plain part; then
    /// `html_text` holds the HTML as text so nothing sent is hidden.
    pub html_differs: bool,
    pub html_text: String,
    pub attachments: Vec<AttachmentInfo>,
    /// An attach request: the exact path and the size in bytes (no draft).
    pub attach: Option<AttachSummary>,
    /// Passed back with Approve: the send is refused if the draft no longer
    /// hashes to this.
    pub content_hash: String,
    /// The draft no longer matches what the agent created.
    pub edited_since_created: bool,
}

fn agent_name(conn: &rightkit_db::rusqlite::Connection, id: &str) -> String {
    registry::get_agent(conn, id)
        .ok()
        .flatten()
        .map(|a| a.name)
        .unwrap_or_default()
}

fn locked() -> String {
    LOCKED_MESSAGE.to_string()
}

/// Pending approvals the window should show, oldest first.
pub fn pending<A: ConnAccess>(access: &A, clock: &dyn Clock) -> Result<Vec<PendingView>, String> {
    if access.is_locked() {
        return Err(locked());
    }
    let (now, boot) = (clock.now_ms(), clock.boot_id());
    access.write(|c| {
        let approvals = SqlStore::new(c, &boot);
        approvals.sweep(now)?;
        Ok(approvals
            .pending(at(now))
            .map_err(|e| e.message)?
            .into_iter()
            .map(|a| PendingView {
                agent_name: agent_name(c, a.agent_id()),
                kind: a.kind().to_string(),
                id: a.id().to_string(),
                account: a.account_id().to_string(),
                created_at: a.created_at(),
                expires_at: a.expires_at(),
            })
            .collect())
    })
}

/// The card for one pending approval, from a fresh read of the draft.
pub fn card<A: ConnAccess>(
    access: &A,
    mailer: &dyn DraftMailer,
    files: &dyn AttachFiles,
    clock: &dyn Clock,
    id: &str,
) -> Result<ApprovalCard, String> {
    if access.is_locked() {
        return Err(locked());
    }
    let (now, boot) = (clock.now_ms(), clock.boot_id());
    let (a, name, created_hash, thread) = access.write(|c| {
        let approvals = SqlStore::new(c, &boot);
        approvals.sweep(now)?;
        // `pending` holds only this run's unexpired requests.
        let a = approvals
            .pending(at(now))
            .map_err(|e| e.message)?
            .into_iter()
            .find(|a| a.id() == id && a.expires_at() > now)
            .ok_or("This request is no longer pending.")?;
        let name = agent_name(c, a.agent_id());
        if a.kind() == kinds::DELETE {
            // A delete has no draft: the card shows the thread, read locally by id.
            let t = approval_trash::summary(c, a.account_id(), a.draft_id())?
                .ok_or("This conversation is no longer available.")?;
            let (hash, _) = approval_trash::thread_binding(c, a.account_id(), a.draft_id())?;
            return Ok((a, name, None, Some((t, hash))));
        }
        let created =
            super::approval_store::any_draft_hash(c, a.agent_id(), a.account_id(), a.draft_id())?;
        Ok((a, name, created, None))
    })?;
    if let Some((t, hash)) = thread {
        return Ok(ApprovalCard {
            id: a.id().to_string(),
            kind: a.kind().to_string(),
            thread: Some(t),
            content_hash: hash,
            agent_name: name,
            account: a.account_id().to_string(),
            expires_at: a.expires_at(),
            ..ApprovalCard::default()
        });
    }
    if a.kind() == kinds::ATTACH {
        // An attach has no draft: the card shows the path and the size.
        return approval_attach::card(access, files, a, name);
    }
    let d = mailer
        .fetch(a.account_id(), a.draft_id())
        .map_err(|_| "The draft could not be read from Gmail.".to_string())?;
    let hash = d.hash();
    // A send compares the fields the agent's `draft` request set; a forward
    // was built by the app, so it compares the whole content hash taken when
    // the app re-read the new draft.
    let created = if a.kind() == kinds::FORWARD {
        hash.clone()
    } else {
        d.creation_hash()
    };
    let html_text = d.html_text_if_different();
    Ok(ApprovalCard {
        id: a.id().to_string(),
        kind: a.kind().to_string(),
        thread: None,
        agent_name: name,
        account: a.account_id().to_string(),
        expires_at: a.expires_at(),
        edited_since_created: created_hash.as_deref() != Some(created.as_str()),
        content_hash: hash,
        reply_to: d.reply_to,
        html_differs: html_text.is_some(),
        html_text: html_text.unwrap_or_default(),
        to: d.to,
        cc: d.cc,
        bcc: d.bcc,
        subject: d.subject,
        body: d.body,
        html_only: d.html_only,
        attachments: d.attachments,
        attach: None,
    })
}

fn at(ms: i64) -> u64 {
    ms.max(0) as u64
}

/// The claim: the core's decision rule moves the request from pending to approved (one guarded
/// UPDATE, unexpired and from this run). The decision is the call from the app's own window, so
/// there is no proof and no display digest here; the binding is checked after the claim, against
/// the hash the card showed.
fn claim_won(approvals: &SqlStore<'_>, id: &str, now: i64) -> Result<bool, String> {
    let ruling = Ruling {
        approve: true,
        shown: None,
        domain: "",
        proof: None,
        grant: Grant::Once {
            valid_ms: STUCK_MS as u64,
        },
        reason: None,
    };
    rules::decide(approvals, id, &ruling, at(now))
        .map(|won| won.is_some())
        .map_err(|e| e.refusal().message)
}

enum Claim {
    Won(Approval),
    Done(Approval),
}

/// Refuse a claim whose agent lost its rights while the card was open.
fn still_permitted(c: &rightkit_db::rusqlite::Connection, a: &Approval) -> Result<bool, String> {
    // An attach needs the draft profile; every mail kind needs send-with-approval.
    let least = if a.kind() == kinds::ATTACH {
        super::outcome::Profile::Draft
    } else {
        super::outcome::Profile::SendWithApproval
    };
    Ok(registry::get_agent(c, a.agent_id())?.is_some_and(|g| {
        g.revoked_at.is_none()
            && g.profile >= least
            && g.accounts.iter().any(|x| x == a.account_id())
    }))
}

pub fn failed<A: ConnAccess>(
    access: &A,
    clock: &dyn Clock,
    a: &Approval,
    reason: &str,
    counts: &str,
) -> Result<DecisionView, String> {
    let now = clock.now_ms();
    access.write(|c| {
        let change = Change::to(State::Failed, at(now)).reason(reason);
        if SqlStore::new(c, "").move_row(a.id(), State::Approved, &change)? {
            record_decision(
                c,
                a,
                Decision::SendFailed,
                &format!("reason={reason} {counts}"),
            )?;
        }
        Ok(())
    })?;
    Ok(DecisionView {
        status: "failed",
        reason: Some(reason.to_string()),
    })
}

/// A finished attach: the approval is closed as done and the stored name is
/// the result the agent reads from `approval`.
pub fn completed<A: ConnAccess>(
    access: &A,
    clock: &dyn Clock,
    a: &Approval,
    result_id: &str,
    counts: &str,
) -> Result<DecisionView, String> {
    let done = clock.now_ms();
    access.write(|c| {
        let change = Change::to(State::Done, at(done)).result(result_id);
        if SqlStore::new(c, "").move_row(a.id(), State::Approved, &change)? {
            record_decision(c, a, Decision::Sent, counts)?;
        }
        Ok(())
    })?;
    Ok(DecisionView {
        status: kinds::done_word(a.kind()),
        reason: None,
    })
}

/// The user clicked Approve on the card that showed `shown_hash` (a delete's
/// hash binds the thread id and its message ids; an attach's binds the path,
/// size and modified time).
pub fn approve<A: ConnAccess>(
    access: &A,
    mailer: &dyn DraftMailer,
    trasher: &dyn ThreadTrasher,
    files: &dyn AttachFiles,
    clock: &dyn Clock,
    id: &str,
    shown_hash: &str,
) -> Result<DecisionView, String> {
    if access.is_locked() {
        return Err(locked());
    }
    let (now, boot) = (clock.now_ms(), clock.boot_id());
    let claim = access.write(|c| {
        let approvals = SqlStore::new(c, &boot);
        approvals.sweep(now)?;
        let a = approvals
            .get(id)
            .map_err(|e| e.message)?
            .ok_or("No such approval.")?;
        if a.state != State::Pending {
            return Ok(Claim::Done(a));
        }
        let reread = |a: Approval| -> Result<Claim, String> {
            Ok(Claim::Done(
                approvals.get(id).map_err(|e| e.message)?.unwrap_or(a),
            ))
        };
        if !still_permitted(c, &a)? {
            // Revoked, lowered or narrowed since the request: nothing is sent.
            if claim_won(&approvals, id, now)? {
                let change = Change::to(State::Failed, at(now)).reason("not_permitted");
                approvals.move_row(id, State::Approved, &change)?;
                record_decision(c, &a, Decision::SendFailed, "reason=not_permitted")?;
            }
            return reread(a);
        }
        crate::host::host().guard(
            c,
            crate::host::MutationClass::ReversibleMutation,
            kinds::guard_action(a.kind()),
            Some(a.account_id()),
        )?;
        if !claim_won(&approvals, id, now)? {
            return reread(a);
        }
        record_decision(c, &a, Decision::Approved, "")?;
        Ok(Claim::Won(a))
    })?;
    let a = match claim {
        Claim::Done(a) => return Ok(view(&a)),
        Claim::Won(a) => a,
    };
    // No database lock is held from here on.
    if a.kind() == kinds::ATTACH {
        return approval_attach::finish(access, files, clock, &a, shown_hash);
    }
    if a.kind() == kinds::DELETE {
        return approval_trash::finish_delete(access, trasher, clock, &a, shown_hash);
    }
    let fetched = match mailer.fetch(a.account_id(), a.draft_id()) {
        Ok(d) => d,
        Err(_) => return failed(access, clock, &a, "fetch_failed", ""),
    };
    let counts = fetched.counts();
    if fetched.hash() != shown_hash {
        return failed(access, clock, &a, "draft_changed", &counts);
    }
    let token = Approved::mint(&a);
    match mailer.deliver(token) {
        Ok(message_id) => {
            let done = clock.now_ms();
            access.write(|c| {
                let change = Change::to(State::Done, at(done)).result(&message_id);
                if SqlStore::new(c, "").move_row(a.id(), State::Approved, &change)? {
                    record_decision(c, &a, Decision::Sent, &counts)?;
                }
                Ok(())
            })?;
            Ok(DecisionView {
                status: "sent",
                reason: None,
            })
        }
        Err(MailFailure::NoCredentials) => failed(access, clock, &a, "no_credentials", &counts),
        // A timeout or a Gmail error may have come after the mail left: never
        // retried, never reported as not sent.
        Err(_) => failed(access, clock, &a, "unknown_outcome", &counts),
    }
}

/// The user clicked Deny.
pub fn deny<A: ConnAccess>(
    access: &A,
    clock: &dyn Clock,
    id: &str,
) -> Result<DecisionView, String> {
    if access.is_locked() {
        return Err(locked());
    }
    let (now, boot) = (clock.now_ms(), clock.boot_id());
    access.write(|c| {
        let approvals = SqlStore::new(c, &boot);
        approvals.sweep(now)?;
        let a = approvals
            .get(id)
            .map_err(|e| e.message)?
            .ok_or("No such approval.")?;
        let ruling = Ruling {
            approve: false,
            shown: None,
            domain: "",
            proof: None,
            grant: Grant::Once { valid_ms: 0 },
            reason: Some("user"),
        };
        if a.state == State::Pending
            && rules::decide(&approvals, id, &ruling, at(now))
                .map_err(|e| e.refusal().message)?
                .is_some()
        {
            record_decision(c, &a, Decision::Denied, "reason=user")?;
        }
        Ok(view(
            &approvals.get(id).map_err(|e| e.message)?.unwrap_or(a),
        ))
    })
}
