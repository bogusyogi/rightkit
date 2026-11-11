//! The one entry point for agent requests, on the core gate
//! (`rightkit_agent_gateway::gate`): its checks in its order, never reordered and never
//! bypassable (no parameter, environment variable or cfg skips a step):
//!
//! 1. pairing key known and not revoked        -> `unauthorised`
//! 2. app unlocked                             -> `app_locked`
//! 3. method known (static table)              -> `bad_request`
//! 4. agent profile >= method minimum          -> `forbidden_profile`
//! 5. account argument in the agent's allow list -> `account_not_allowed`
//! 6. handler runs, errors propagate
//!
//! The gate, not the handler, writes exactly one audit row per request for
//! every outcome ([`Logging::Required`]: a result is never returned if its audit row failed). The
//! user's decisions on a send, forward or delete approval (approved, denied,
//! sent or trashed, failed) are audited through [`record_decision`], which is
//! the only other writer.
//!
//! A method with an effect outside the database (`draft` and `forward`;
//! `label`, `archive` and `move`; `attachment`) does not run it inside a database closure. Step 7
//! validates and authorises it under short closures and returns a typed
//! [`Step`]; [`handle`] then performs it through the injected [`DraftCreator`]
//! with no database lock held (`gate_effects` holds the label and attachment
//! halves), and audits the final outcome once.

use super::access::ConnAccess;
use super::approval::Clock;
use super::approval::SystemClock;
use super::approval_kind;
use super::approval_store::{self, Approval, MailApproval};
use super::draft::{self, DraftCreator, DraftRequest};
use super::draft_files;
use super::draft_view;
use super::forward::ForwardRequest;
use super::gate_effects;
use super::gate_forward;
use super::mutate_local::Plan;
use super::outcome::to_json;
use super::outcome::{from_db_error, AgentError, Profile, Request};
use super::read_handlers;
use super::registry::{self, Agent, Verified};
use super::send_request;
use rightkit_agent_gateway::audit::{self, Entry};
use rightkit_agent_gateway::gate::{self as core, Logging, Method};
use rightkit_db::rusqlite::Connection;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Read handler implemented in `read_handlers`; takes an `account` argument.
    ReadAccount,
    /// Read handler implemented; no account argument.
    ReadNoAccount,
    /// `draft`: takes an `account`; its effect runs outside the gate's closures.
    Draft,
    /// `send`: takes an `account`; only ever creates a pending approval.
    Send,
    /// `forward`: takes an `account`; builds a forward draft, then only ever
    /// creates a pending approval.
    Forward,
    /// `delete`: takes an `account`; only ever creates a pending approval to
    /// move one thread to Trash.
    Delete,
    /// `attach_file`: takes an `account`; only ever creates a pending approval
    /// to copy one file, named by its absolute path, into the export folder.
    AttachFile,
    /// `approval`: status of an approval this agent created.
    Approval,
    /// `label`, `archive`, `move`: takes an `account`; its Gmail effect runs
    /// outside the gate's closures.
    Mutate,
    /// `attachment`: takes an `account`; fetches one file into the export dir.
    Attachment,
}

/// What a request authorised: a finished result, or an effect to perform.
pub enum Step {
    Done(Value),
    Draft {
        req: DraftRequest,
        label: bool,
    },
    Forward {
        req: ForwardRequest,
        agent: Agent,
    },
    Mutate(Plan),
    Attachment {
        req: super::attachment::AttachmentRequest,
        agent_id: String,
    },
}

/// One row of the method table: every method but `accounts` and `approval` takes an `account`
/// the agent must be allowed.
const fn m(name: &'static str, min: Profile, kind: Kind) -> Method<Profile, Kind> {
    Method {
        name,
        min,
        scoped: !matches!(kind, Kind::ReadNoAccount | Kind::Approval),
        changes: !matches!(
            kind,
            Kind::ReadAccount | Kind::ReadNoAccount | Kind::Approval | Kind::Attachment
        ),
        kind,
    }
}

/// Static method table: name, minimum profile, kind. The only source of
/// policy for methods; nothing in a request can change it.
const METHODS: &[Method<Profile, Kind>] = &[
    m("accounts", Profile::ReadOnly, Kind::ReadNoAccount),
    m("search", Profile::ReadOnly, Kind::ReadAccount),
    m("thread", Profile::ReadOnly, Kind::ReadAccount),
    m("labels", Profile::ReadOnly, Kind::ReadAccount),
    m("draft", Profile::Draft, Kind::Draft),
    m("send", Profile::SendWithApproval, Kind::Send),
    m("approval", Profile::Draft, Kind::Approval),
    m("label", Profile::Draft, Kind::Mutate),
    m("archive", Profile::Draft, Kind::Mutate),
    m("move", Profile::Draft, Kind::Mutate),
    m("attachment", Profile::ReadOnly, Kind::Attachment),
    m("delete", Profile::SendWithApproval, Kind::Delete),
    m("forward", Profile::SendWithApproval, Kind::Forward),
    m("attach_file", Profile::Draft, Kind::AttachFile),
];

/// What the audit row records about a request.
#[derive(Default)]
pub struct Ctx {
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub account: Option<String>,
    pub method: &'static str,
    pub detail: Option<String>,
    /// Set when a `send`, `forward` or `delete` created a new pending approval.
    pub new_approval: Option<String>,
    /// Held by a `label`, `archive` or `move` from authorisation until the
    /// audit row is written (see `gate_effects`).
    pub held: Option<std::sync::MutexGuard<'static, ()>>,
}

/// A handled request: the reply, and the approval it created (if any) so the
/// transport can tell the user's window.
pub struct Handled {
    pub result: Result<Value, AgentError>,
    pub new_approval: Option<String>,
}

/// Handle one request. Returns the bare result object or the refusal.
pub fn handle<A: ConnAccess>(
    access: &A,
    creator: &dyn DraftCreator,
    req: &Request,
) -> Result<Value, AgentError> {
    handle_full(access, creator, &SystemClock, req).result
}

/// [`handle`] with an injected clock, also reporting a new approval.
pub fn handle_full<A: ConnAccess>(
    access: &A,
    creator: &dyn DraftCreator,
    clock: &dyn Clock,
    req: &Request,
) -> Handled {
    let mut ctx = Ctx {
        method: "unknown",
        ..Ctx::default()
    };
    let result = match evaluate(access, clock, req, &mut ctx) {
        Ok(Step::Done(v)) => Ok(v),
        Ok(Step::Draft { mut req, label }) => {
            perform_draft(access, creator, clock, &mut req, label, &mut ctx)
        }
        Ok(Step::Forward { req, agent }) => {
            gate_forward::perform_forward(access, creator, clock, &agent, &req, &mut ctx)
        }
        Ok(Step::Mutate(plan)) => gate_effects::perform_mutate(access, creator, &plan, &mut ctx),
        Ok(Step::Attachment { req, agent_id }) => {
            gate_effects::perform_attachment(creator, &req, &agent_id, &mut ctx)
        }
        Err(e) => Err(e),
    };
    let outcome = match &result {
        Ok(_) => "ok",
        Err(e) => e.code(),
    };
    let entry = Entry {
        agent_id: ctx.agent_id.as_deref(),
        agent_name: ctx.agent_name.as_deref(),
        scope: ctx.account.as_deref(),
        method: ctx.method,
        outcome,
        detail: ctx.detail.as_deref(),
        at: Some(clock.now_ms()),
        ..Entry::default()
    };
    let written = access.write(|c| audit::record(c, &entry));
    let new_approval = ctx.new_approval.take();
    // No audit row, no result; a refusal stands even if it could not be logged (e.g. locked).
    let result = core::conclude(result, written, Logging::Required, |e| from_db_error(&e));
    Handled {
        new_approval: result.as_ref().ok().and(new_approval),
        result,
    }
}

/// What the user decided about an approval, as audited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Approved,
    Denied,
    Sent,
    SendFailed,
}

impl Decision {
    /// The audit outcome. A delete says `trashed` and `failed`; a send or a
    /// forward says `sent` and `send_failed`.
    fn as_str(self, kind: &str) -> &'static str {
        let delete = kind == approval_kind::DELETE;
        let attach = kind == approval_kind::ATTACH;
        match self {
            Decision::Approved => "approved",
            Decision::Denied => "denied",
            Decision::Sent if delete => "trashed",
            Decision::Sent if attach => "attached",
            Decision::Sent => "sent",
            Decision::SendFailed if delete || attach => "failed",
            Decision::SendFailed => "send_failed",
        }
    }
}

/// Audit one decision on an approval. The detail holds the approval id and
/// counts or reason codes only: any other character class (an address, a word
/// of content) replaces the whole detail, so no caller can log content here.
pub fn record_decision(
    conn: &Connection,
    a: &Approval,
    d: Decision,
    detail: &str,
) -> Result<(), String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_=. -".contains(c);
    let text = if detail.chars().all(plain) {
        format!("approval={} {}", a.id(), detail.trim())
    } else {
        format!("approval={} detail=withheld", a.id())
    };
    let name = registry::get_agent(conn, a.agent_id())
        .ok()
        .flatten()
        .map(|g| g.name);
    audit::record(
        conn,
        &Entry {
            agent_id: Some(a.agent_id()),
            agent_name: name.as_deref(),
            scope: Some(a.account_id()),
            method: approval_kind::method(a.kind()),
            outcome: d.as_str(a.kind()),
            detail: Some(text.trim()),
            ..Entry::default()
        },
    )
}

/// [`handle`] serialised for the transport and CLI.
pub fn handle_json<A: ConnAccess>(access: &A, creator: &dyn DraftCreator, req: &Request) -> Value {
    to_json(&handle(access, creator, req))
}

/// Run the authorised draft effect. No database closure is open here.
fn perform_draft<A: ConnAccess>(
    access: &A,
    creator: &dyn DraftCreator,
    clock: &dyn Clock,
    req: &mut DraftRequest,
    label: bool,
    ctx: &mut Ctx,
) -> Result<Value, AgentError> {
    // Read the named export files now, with no database lock held.
    let agent = ctx.agent_id.clone().unwrap_or_default();
    if let Err((e, reason)) = draft_files::fill(creator, &agent, req) {
        ctx.detail = Some(format!("{} reason={reason}", draft::counts(req)));
        return Err(e);
    }
    let req: &DraftRequest = req;
    let counts = draft::counts(req);
    match creator.create(req, label) {
        Ok(done) => {
            ctx.detail = Some(format!("{counts} labelled={}", done.labelled));
            // Remember the draft: only drafts made here can later be sent.
            let (hash, now) = (draft_view::request_hash(req), clock.now_ms());
            let digest = draft_files::digest(&req.attachments);
            let saved = access.write(|c| {
                if req.attachments.is_empty() {
                    approval_store::record_draft(
                        c,
                        &agent,
                        &req.account,
                        &done.draft_id,
                        &hash,
                        now,
                    )
                } else {
                    approval_store::record_draft_with_digest(
                        c,
                        &agent,
                        &req.account,
                        &done.draft_id,
                        &hash,
                        &digest,
                        now,
                    )
                }
            });
            if saved.is_err() {
                ctx.detail = Some(format!("{counts} reason=record_failed"));
                return Err(AgentError::DraftFailed);
            }
            Ok(json!({
                "draft_id": done.draft_id,
                "labelled": done.labelled,
                "note": "Saved as a draft in Gmail. It has not been sent; the user must review and send it.",
            }))
        }
        Err(why) => {
            ctx.detail = Some(format!("{counts} reason={}", why.as_str()));
            Err(AgentError::DraftFailed)
        }
    }
}

fn evaluate<A: ConnAccess>(
    access: &A,
    clock: &dyn Clock,
    req: &Request,
    ctx: &mut Ctx,
) -> Result<Step, AgentError> {
    // 1. Key.
    let agent: Agent = match access
        .read(|c| registry::verify_key(c, &req.key))
        .map_err(|e| from_db_error(&e))?
    {
        Verified::Agent(a) => a,
        Verified::Revoked { id, name } => {
            ctx.agent_id = Some(id);
            ctx.agent_name = Some(name);
            return Err(AgentError::Unauthorised);
        }
        Verified::Unknown => return Err(AgentError::Unauthorised),
    };
    ctx.agent_id = Some(agent.id.clone());
    ctx.agent_name = Some(agent.name.clone());

    // 2. Lock.
    if access.is_locked() {
        return Err(AgentError::AppLocked);
    }

    // 3. Method.
    let method = core::lookup(METHODS, &req.method)?;
    let (name, kind) = (method.name, method.kind);
    ctx.method = name;

    // 4. Profile, from the registry row only.
    core::allow_level(agent.profile, method)?;

    // 5. Account allow list.
    if method.scoped {
        let account = read_handlers::str_param(&req.params, "account", 200)?;
        core::allow_scope(&agent.scopes, account)?;
        ctx.account = Some(account.to_string());
    }

    // 6. Handler, or the authorised effect for the caller to perform.
    if kind == Kind::Draft {
        return authorise_draft(access, req, ctx);
    }
    if kind == Kind::Forward {
        return gate_forward::authorise_forward(access, clock, &agent, req, ctx);
    }
    if kind == Kind::Mutate {
        return gate_effects::authorise_mutate(access, clock, &agent, name, req, ctx);
    }
    if kind == Kind::Attachment {
        return gate_effects::authorise_attachment(access, &agent, req, ctx);
    }
    if matches!(
        kind,
        Kind::Send | Kind::Delete | Kind::AttachFile | Kind::Approval
    ) {
        return approval_step(access, clock, &agent, kind, req, ctx);
    }
    let out = access
        .read(|c| Ok(read_handlers::dispatch(c, &agent, name, &req.params)))
        .map_err(|e| from_db_error(&e))??;
    ctx.detail = Some(out.detail);
    Ok(Step::Done(out.value))
}

/// `send`, `delete` and `attach_file` create a pending approval; `approval`
/// reads one. All are local database work: none can reach Gmail or a file.
fn approval_step<A: ConnAccess>(
    access: &A,
    clock: &dyn Clock,
    agent: &Agent,
    kind: Kind,
    req: &Request,
    ctx: &mut Ctx,
) -> Result<Step, AgentError> {
    let account = ctx.account.clone().unwrap_or_default();
    let (value, created) = access
        .write(|c| {
            Ok(if kind == Kind::Send {
                send_request::create(c, clock, agent, &account, &req.params)
            } else if kind == Kind::AttachFile {
                send_request::create_attach(c, clock, agent, &account, &req.params)
            } else if kind == Kind::Delete {
                send_request::create_delete(c, clock, agent, &account, &req.params)
            } else {
                send_request::status(c, clock, agent, &req.params).map(|v| (v, None))
            })
        })
        .map_err(|e| from_db_error(&e))??;
    if let Some(id) = value.get("approval_id").and_then(Value::as_str) {
        ctx.detail = Some(format!("approval={id}"));
    }
    ctx.new_approval = created;
    Ok(Step::Done(value))
}

/// Validate a draft request and resolve what the effect needs from the
/// database, in short closures. Nothing is created here.
fn authorise_draft<A: ConnAccess>(
    access: &A,
    req: &Request,
    ctx: &mut Ctx,
) -> Result<Step, AgentError> {
    let account = ctx.account.clone().unwrap_or_default();
    let mut dr = draft::validate(&account, &req.params)?;
    let (label, reply) = access
        .read(|c| {
            let label = draft::label_option(c)?;
            let reply = match &dr.reply_thread {
                Some(t) => Some(draft::reply_target(c, &account, t)?),
                None => None,
            };
            Ok((label, reply))
        })
        .map_err(|e| from_db_error(&e))?;
    match reply {
        Some(None) => {
            return Err(AgentError::BadRequest(
                "`reply_to_thread` was not found".into(),
            ))
        }
        Some(Some(id)) => dr.in_reply_to = id,
        None => {}
    }
    access
        .write(|c| {
            crate::host::host().guard(
                c,
                crate::host::MutationClass::SafeMutation,
                "agent_draft",
                Some(&account),
            )
        })
        .map_err(|e| from_db_error(&e))?;
    Ok(Step::Draft { req: dr, label })
}

/// Every method name the gate knows, for the transport allowlist (so the
/// allowlist cannot drift from the method table).
pub fn method_names() -> impl Iterator<Item = &'static str> {
    METHODS.iter().map(|m| m.name)
}
