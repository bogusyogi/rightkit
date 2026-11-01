//! Local approvals for the remote tools that change this computer.
//!
//! A tool that writes, removes, starts a program or stops one never acts on a switch alone. It
//! needs a grant that the user gave on this computer. The grant and the request that asked for it
//! live in the resident engine's memory and nowhere else (see "the engine's request and decision
//! record" below): no file holds a request or an answer, so no hand-edited file can approve
//! anything. Two kinds exist:
//!
//! * **act**: one approval for one action. The tool works out a `bind` (a SHA-256 over the tool,
//!   the door the call came in by, the resolved target and what is about to happen to it) and asks
//!   for approval. The caller repeats the call with `approval = <id>` once the user has approved
//!   it. The approval holds only for that exact bind: a file that changed, a different target or
//!   a different edit is `approval_invalid`. It is used once and expires `ACT_VALID` after it was
//!   given.
//! * **session**: one approval for a family (`terminal`) and a door (`local` or `gateway`). It
//!   holds until it has been idle for `SESSION_IDLE` and ends when the family's switch goes off.
//!   A standing policy (a setting the user turned on) replaces the prompt but not the idle expiry
//!   of the sessions themselves.
//!
//! **Who writes the grant.** The tools in this crate only ever create `pending` requests. Turning
//! one into `approved` (`decide_held`) belongs to the owner's owner app or hub through the engine's
//! owner-only `remote.approvals.decide`; no remote tool, no gateway call and no other engine call
//! reaches it. Without the engine running a call that needs approval is `approval_unavailable`.
//!
//! A request shows the user a `summary` that names the tool, the door and the target (a file
//! name, never file content or a command's secrets). Nothing in a summary or an id is ever
//! returned to a different door's caller as a way to approve.

use super::{Origin, Refusal, Result};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long an act approval can be used after the user gave it.
pub const ACT_VALID: Duration = Duration::from_secs(10 * 60);
/// How long a request waits for an answer.
pub const PENDING_VALID: Duration = Duration::from_secs(10 * 60);
/// A session grant ends after this long without a call.
pub const SESSION_IDLE: Duration = Duration::from_secs(30 * 60);
/// A standing grant (a family starting `launch:` or `fill:`, one app or site) ends after this
/// long without use. It lives in the engine's memory, so an engine restart ends it too.
pub const STANDING_IDLE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The family prefixes whose session grants are standing.
const STANDING_PREFIXES: [&str; 2] = ["launch:", "fill:"];
/// The session family of a per-chat unlock (`unlock.rs`). Its grants are keyed by client
/// (`chat:<id>`), end at a time or with the chat, and are given only by `decide_unlock`.
pub const UNLOCK_FAMILY: &str = "unlock";
/// Most requests and grants kept; the oldest finished ones go first.
const MAX_ITEMS: usize = 64;
/// Most requests waiting at once: a caller cannot fill the user's prompt list.
const MAX_PENDING: usize = 16;
/// Longest summary kept. Long enough for `chat_start` to show a whole prompt (see its `PROMPT_SHOWN`).
pub const SUMMARY_MAX: usize = 1600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    /// `act` or `session`.
    pub kind: String,
    /// The tool for an act, the family (`terminal`) for a session.
    pub scope: String,
    pub origin: String,
    /// Who asked, as the engine's record keys a session grant: `chat:<id>` for an external chat,
    /// `agent` for an agent started inside the host app, `gateway:<name>` for the gateway, `local:<name>`
    /// otherwise. Empty for a request a tool in the engine filed for itself (the budget).
    pub client: String,
    /// Hex SHA-256 for an act; empty for a session.
    pub bind: String,
    pub summary: String,
    /// `pending`, `approved` or `denied`.
    pub state: String,
    pub created: u64,
    pub decided: u64,
    pub last_used: u64,
    /// Display-only structured facts the engine put beside the summary (a payment's merchant,
    /// amount, card label and last four, page host, month room and caps; never a secret). Null
    /// when the request has none. The summary stays what the digest binds; a surface that cannot
    /// read `detail` shows the summary.
    pub detail: Value,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 8];
    // An id names a request; it is not a secret (only the user's local answer approves). A failed
    // system RNG leaves zeros, which a second live request would collide with, so the pair is
    // also mixed with the clock.
    if SystemRandom::new().fill(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        bytes = nanos.to_be_bytes();
    }
    rightkit_fs::digest::hex(&bytes)
}

fn clean_summary(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(SUMMARY_MAX)
        .collect()
}

fn origin_name(origin: Origin) -> &'static str {
    origin.as_str()
}

fn waiting(id: &str, what: &str) -> Refusal {
    Refusal::new(
        "approval_required",
        format!(
            "{what} needs your approval on this computer (request {id}). Approve it in the app, then repeat the call{}",
            if what.ends_with(" session") {
                "."
            } else {
                " with approval set to that id."
            }
        ),
    )
}

/// Hex SHA-256 of `parts`, each length-prefixed so two different splits never hash alike.
pub fn bind_of(parts: &[&[u8]]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize()
}

// ---- the engine's request and decision record ------------------------------------------------
//
// A file a program running as the user can edit would prove nothing, so neither a request nor an
// `approved` is ever stored in one (the old `remote-approvals.json` API is gone). The resident
// engine (`mark_engine_process`) keeps the record in its own memory and nowhere else:
//
// * **Requests.** A tool in the engine files its request in memory (`request_act_held`,
//   `request_session_held`); a tool in another process (the gateway) files it through the engine
//   (`remote.approvals.request`), and the engine remembers which hello label filed it. Nothing is
//   written to a file and nothing is read back from one.
// * **The owner's click.** `decide_held` (the engine's owner-only `remote.approvals.decide`)
//   answers the request in memory, and only when the digest of what the hub displayed (kind, tool
//   or family, door, bind, summary: `request_digest`) equals the digest of the engine's own request.
// * **Using an answer.** `take_act_held`, `session_active_held` and `outcome` run in place inside
//   the engine and ask the resident engine over its socket from any other process. The engine
//   derives the door from the caller's hello label, never from a parameter; only the label that
//   filed an act may use it, and a gateway-door session is used and refreshed only by the gateway
//   label.
//
// Requests, answers and the used ids live only in engine memory, so a restart forgets them and the
// call asks again. Nothing is persisted and no key is involved: no file can be copied back to
// replay an approval and no credential-store secret can forge one.
//
// The one exception is a chat's unlock after an owner check: `unlock.rs` keeps a sealed token with an
// expiry in the OS credential store and `serve_session` restores the grant from it (`restore_token`).
//
// Not closed: the engine cannot tell a program that claims a hello label from the program that
// owns it (the label is a claim, the socket is owner-only), so a same-user process can file a
// request under a label with a misleading summary that the owner may approve. Budget requests are
// filed by the engine itself and committed only against the engine's own text (`take_act_exact`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

/// A refusal stays visible to `outcome` this long, so the caller that asked can learn it.
const DENIED_VISIBLE: u64 = 5 * 60;
/// Most consumed act ids remembered, so a used approval cannot be used again.
const USED_KEPT: usize = 256;
/// Longest tool, family or bind a request may carry.
const FIELD_MAX: usize = 160;

static ENGINE: AtomicBool = AtomicBool::new(false);
static LABEL: Mutex<Option<&'static str>> = Mutex::new(None);
static HELD: Mutex<Held> = Mutex::new(Held {
    requests: Vec::new(),
    decisions: Vec::new(),
    used: Vec::new(),
});

thread_local! {
    /// Who the call running on this thread is for (`set_client`). Session grants are keyed by it,
    /// so a grant given to one chat or door client never covers another.
    static CLIENT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Puts the previous client back when the call ends.
pub struct ClientGuard(String);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.0);
        CLIENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// Names the client of the call about to run on this thread: `chat:<id>`, `agent`,
/// `gateway:<name>` or `local:<name>` (see `remote::run`). Held until the guard drops.
pub fn set_client(key: &str) -> ClientGuard {
    let cleaned = clean_client(key);
    ClientGuard(CLIENT.with(|c| c.replace(cleaned)))
}

/// The client of the call running on this thread (`set_client`); empty outside a call.
pub fn current_client() -> String {
    CLIENT.with(|c| c.borrow().clone())
}

/// A client key is text a caller supplied: bound it and keep it on one line.
fn clean_client(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(FIELD_MAX)
        .collect()
}

struct Held {
    requests: Vec<Request>,
    decisions: Vec<Decision>,
    used: Vec<String>,
}

/// A request waiting for the owner, as the engine recorded it.
#[derive(Clone)]
struct Request {
    id: String,
    kind: String,
    scope: String,
    origin: String,
    /// The hello label that filed it through the engine; empty for a tool running in the engine.
    filer: String,
    /// The requesting client (see `Item::client`).
    client: String,
    bind: String,
    summary: String,
    created: u64,
    /// See `Item::detail`.
    detail: Value,
}

/// One answer the owner gave through the engine.
#[derive(Clone)]
struct Decision {
    id: String,
    kind: String,
    scope: String,
    origin: String,
    filer: String,
    client: String,
    bind: String,
    summary: String,
    approved: bool,
    decided: u64,
    /// A session's last use.
    last_used: u64,
    /// An unlock's end in Unix seconds; 0 is "until the chat ends". Unused for other kinds.
    expires: u64,
}

/// Where a request stands, as the engine knows it. Carries nothing that could approve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub id: String,
    /// `pending`, `approved`, `denied`, `used` or `unknown`.
    pub state: String,
    pub kind: String,
    pub scope: String,
    pub origin: String,
    pub decided: u64,
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "state": self.state, "kind": self.kind, "scope": self.scope,
            "origin": self.origin, "decided": self.decided,
        })
    }

    pub fn from_json(value: &Value) -> Option<Outcome> {
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_string();
        Some(Outcome {
            id: value["id"].as_str()?.to_string(),
            state: value["state"].as_str()?.to_string(),
            kind: text("kind"),
            scope: text("scope"),
            origin: text("origin"),
            decided: value["decided"].as_u64().unwrap_or(0),
        })
    }
}

/// The resident engine calls this once, before it serves: from then on this process keeps the
/// request and decision record and answers `remote.approvals.*` for every other process.
pub fn mark_engine_process() {
    ENGINE.store(true, Ordering::SeqCst);
}

/// The app label a process that is not the engine says hello as when it asks the engine
/// (`the gateway` for the gateway; the default is `pulse-cli`).
pub fn set_engine_label(label: &'static str) {
    *LABEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(label);
}

fn is_engine() -> bool {
    ENGINE.load(Ordering::SeqCst)
}

/// Whether this process is the resident engine (`mark_engine_process`).
pub(super) fn is_engine_process() -> bool {
    is_engine()
}

/// Asks the resident engine one question from a process that is not the engine
/// (`protected_store.rs` uses it for the owner-only settings), after the OS has said that the
/// process on the other end of the socket runs the engine's own executable: `service.json` and the
/// socket path are files and names any process of the user can create, so a fake "engine" could
/// otherwise answer with gates switched on. The executable must be `pulse-engine` in the directory
/// of this process's executable (`Helpers/` in an installed the host app, for the CLI and the gateway);
/// any other layout, an unreadable peer or a platform that cannot tell is refused (fail closed).
/// Nothing is sent to a peer that fails the check.
pub(super) fn ask_engine_verified(method: &str, params: Value) -> Result<Value> {
    let client = verified_engine_client().map_err(|()| engine_unreachable())?;
    client
        .call(method, params)
        .map_err(|error| Refusal::new("approval_unavailable", error.message))
}

/// A connection to the installed resident engine: the OS names the program behind the endpoint
/// before a byte is written and it must be the `pulse-engine` beside this executable.
fn verified_engine_client() -> std::result::Result<rightkit_daemon::Client, ()> {
    let config = super::config::config();
    let name = if cfg!(windows) {
        format!("{}.exe", config.engine_executable)
    } else {
        config.engine_executable.clone()
    };
    let expect = rightkit_daemon::HostExpectation::sibling_executable(&name).map_err(|_| ())?;
    let suite = rightkit_daemon::Suite::per_user_default().map_err(|_| ())?;
    let label =
        (*LABEL.lock().unwrap_or_else(|e| e.into_inner())).unwrap_or(config.default_label.as_str());
    let config =
        rightkit_daemon::ClientConfig::new(config.engine_service.as_str(), label).suite(suite);
    rightkit_daemon::Client::connect_expecting(&config, &expect).map_err(|_| ())
}

/// The client key (`chat:<id>`) of the waiting unlock request `id`. Engine only.
pub(super) fn unlock_request_client(id: &str) -> Option<String> {
    if !is_engine() {
        return None;
    }
    let mut h = held();
    prune_held(&mut h);
    h.requests
        .iter()
        .find(|r| r.id == id && r.kind == "session" && r.scope == UNLOCK_FAMILY)
        .map(|r| r.client.clone())
}

fn held() -> MutexGuard<'static, Held> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

fn not_engine() -> Refusal {
    Refusal::new(
        "approval_unavailable",
        "the app's approval record is held by the engine, and this process is not the engine",
    )
}

fn engine_unreachable() -> Refusal {
    Refusal::new(
        "approval_unavailable",
        "the app's engine is not running, so this approval cannot be asked for or checked and nothing was done; start the app, then repeat the call",
    )
}

fn invalid_field() -> Refusal {
    Refusal::new(
        "approval_invalid",
        "the approval request is malformed (kind, tool or bind)",
    )
}

/// The digest the owner's click is bound to: a SHA-256 over the request's kind, tool or family,
/// door, bind and summary, exactly as the list shows them. The hub computes it from the item it
/// displayed; the engine refuses the click when it differs from its own request's.
pub fn request_digest(kind: &str, scope: &str, origin: &str, bind: &str, summary: &str) -> String {
    bind_of(&[
        b"pulse.approval.request.v1",
        kind.as_bytes(),
        scope.as_bytes(),
        origin.as_bytes(),
        bind.as_bytes(),
        summary.as_bytes(),
    ])
}

fn prune_held(h: &mut Held) {
    let at = now();
    h.requests
        .retain(|r| at.saturating_sub(r.created) <= PENDING_VALID.as_secs());
    h.decisions.retain(|d| match (d.kind.as_str(), d.approved) {
        ("act", true) => at.saturating_sub(d.decided) <= ACT_VALID.as_secs(),
        // An unlock ends at its own time (the setting) or when its chat ends (checked on use,
        // `serve_session`); it never idles out.
        ("session", true) if d.scope == UNLOCK_FAMILY => d.expires == 0 || at < d.expires,
        ("session", true) => {
            let idle = if STANDING_PREFIXES.iter().any(|p| d.scope.starts_with(p)) {
                STANDING_IDLE
            } else {
                SESSION_IDLE
            };
            at.saturating_sub(d.last_used.max(d.decided)) <= idle.as_secs()
        }
        _ => at.saturating_sub(d.decided) <= DENIED_VISIBLE,
    });
    while h.requests.len() > MAX_ITEMS {
        h.requests.remove(0);
    }
    while h.decisions.len() > MAX_ITEMS {
        h.decisions.remove(0);
    }
    while h.used.len() > USED_KEPT {
        h.used.remove(0);
    }
}

/// Records a request in engine memory and returns its id; a repeat of the same pending request
/// (same kind, tool, door, filer and bind) answers with the same id. Engine only.
fn file_request(
    filer: &str,
    client: &str,
    kind: &str,
    scope: &str,
    origin: &str,
    bind: &str,
    summary: &str,
) -> Result<String> {
    file_request_detail(
        filer,
        client,
        kind,
        scope,
        origin,
        bind,
        summary,
        Value::Null,
    )
}

/// `file_request` with the request's display-only `detail` (see `Item::detail`).
#[allow(clippy::too_many_arguments)]
fn file_request_detail(
    filer: &str,
    client: &str,
    kind: &str,
    scope: &str,
    origin: &str,
    bind: &str,
    summary: &str,
    detail: Value,
) -> Result<String> {
    if !is_engine() {
        return Err(not_engine());
    }
    if !matches!(kind, "act" | "session")
        || scope.is_empty()
        || scope.len() > FIELD_MAX
        || bind.len() > FIELD_MAX
        || client.len() > FIELD_MAX
    {
        return Err(invalid_field());
    }
    let client = clean_client(client);
    let mut h = held();
    prune_held(&mut h);
    if let Some(existing) = h.requests.iter().find(|r| {
        r.kind == kind
            && r.scope == scope
            && r.origin == origin
            && r.filer == filer
            && r.client == client
            && r.bind == bind
    }) {
        return Ok(existing.id.clone());
    }
    if h.requests.len() >= MAX_PENDING {
        return Err(Refusal::new(
            "approval_busy",
            "too many approval requests are waiting; answer or let them expire first",
        ));
    }
    let id = new_id();
    h.requests.push(Request {
        id: id.clone(),
        kind: kind.to_string(),
        scope: scope.to_string(),
        origin: origin.to_string(),
        filer: filer.to_string(),
        client,
        bind: bind.to_string(),
        summary: clean_summary(summary),
        created: now(),
        detail,
    });
    Ok(id)
}

/// Asks the resident engine one `remote.approvals.*` question from a process that is not the
/// engine. The engine's refusals keep their codes; a missing engine is `approval_unavailable`.
fn ask_engine(method: &str, params: Value) -> Result<Value> {
    let client = verified_engine_client().map_err(|()| engine_unreachable())?;
    client.call(method, params).map_err(|error| {
        let code = match error.code.as_str() {
            "approval_pending" => "approval_pending",
            "approval_invalid" => "approval_invalid",
            "approval_busy" => "approval_busy",
            _ => "approval_unavailable",
        };
        Refusal::new(code, error.message)
    })
}

/// Calls one remote tool method on the resident engine as this process's label (`pulse-cli`), for the
/// effectful `pulse act` command. The engine stamps the caller's process from the socket and decides
/// everything (switches, unlock, approvals); a refusal keeps its own code. `Err((message, code))`.
pub fn call_engine_tool(
    method: &str,
    params: Value,
) -> std::result::Result<Value, (String, String)> {
    let unreachable = || {
        (
            "the app's engine is not running, so this tool cannot run; open the app and try again"
                .to_string(),
            "engine_unavailable".to_string(),
        )
    };
    let client = verified_engine_client().map_err(|()| unreachable())?;
    client
        .call(method, params)
        .map_err(|error| (error.message, error.code))
}

/// Asks for approval of one action, on the engine's record: filed in memory when this process is
/// the engine, through the engine otherwise. Returns the refusal to give the caller
/// (`approval_required` with the request id, `approval_busy` or `approval_unavailable`).
pub fn request_act_held(origin: Origin, tool: &str, bind: &str, summary: &str) -> Refusal {
    let client = current_client();
    let filed = if is_engine() {
        file_request("", &client, "act", tool, origin_name(origin), bind, summary)
    } else {
        ask_engine(
            "remote.approvals.request",
            json!({"kind": "act", "scope": tool, "bind": bind, "summary": summary, "client": client}),
        )
        .and_then(|answer| {
            answer["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(engine_unreachable)
        })
    };
    match filed {
        Ok(id) => waiting(&id, &format!("{tool} on this target")),
        Err(refusal) => refusal,
    }
}

/// `request_act_held` with display-only structured facts for the owner's card (`Item::detail`).
/// Filed with the facts only from inside the engine (payments run there); another process files
/// the summary alone, which every surface can show.
pub fn request_act_held_detail(
    origin: Origin,
    tool: &str,
    bind: &str,
    summary: &str,
    detail: Value,
) -> Refusal {
    if !is_engine() {
        return request_act_held(origin, tool, bind, summary);
    }
    let client = current_client();
    match file_request_detail(
        "",
        &client,
        "act",
        tool,
        origin_name(origin),
        bind,
        summary,
        detail,
    ) {
        Ok(id) => waiting(&id, &format!("{tool} on this target")),
        Err(refusal) => refusal,
    }
}

/// Asks for a session grant for `family` on this door, on the engine's record (see
/// `request_act_held`).
pub fn request_session_held(origin: Origin, family: &str, summary: &str) -> Refusal {
    match file_session(origin, family, summary) {
        Ok(id) => waiting(&id, &format!("a {family} session")),
        Err(refusal) => refusal,
    }
}

/// `request_session_held` for a per-chat unlock: the refusal has the code `chat_locked` and says
/// what the owner does on this computer. Any failure to file keeps its own code.
pub fn request_unlock_held(origin: Origin, summary: &str) -> Refusal {
    match file_session(origin, UNLOCK_FAMILY, summary) {
        Ok(id) => Refusal::new(
            "chat_locked",
            format!(
                "this chat is locked. The app asked the owner on this computer to unlock it (request {id}): Touch ID on a Mac, or the PIN. Repeat the call after they did; do not look for another way"
            ),
        ),
        Err(refusal) => refusal,
    }
}

/// Files a session request for the current client, in memory when this process is the engine and
/// through the engine otherwise; returns its id.
fn file_session(origin: Origin, family: &str, summary: &str) -> Result<String> {
    let client = current_client();
    if is_engine() {
        file_request(
            "",
            &client,
            "session",
            family,
            origin_name(origin),
            "",
            summary,
        )
    } else {
        ask_engine(
            "remote.approvals.request",
            json!({"kind": "session", "scope": family, "bind": "", "summary": summary, "client": client}),
        )
        .and_then(|answer| {
            answer["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(engine_unreachable)
        })
    }
}

/// Files an act request from inside the engine and returns its id (the budget approval, whose
/// summary the engine writes itself). Any other process gets `approval_unavailable`.
pub fn request_act_id(origin: Origin, tool: &str, bind: &str, summary: &str) -> Result<String> {
    file_request("", "", "act", tool, origin_name(origin), bind, summary)
}

/// The engine's answer to `remote.approvals.request`: the request is recorded under the caller's
/// hello label and the door that label implies (`origin` is derived from it by the engine).
pub fn serve_request(
    filer: &str,
    client: &str,
    origin: Origin,
    kind: &str,
    scope: &str,
    bind: &str,
    summary: &str,
) -> Result<Value> {
    // An unlock request carries the engine's own words about a chat it identified; a process that
    // asks through the socket cannot file one, or it could put any text in front of the owner.
    if scope == UNLOCK_FAMILY {
        return Err(invalid_field());
    }
    file_request(
        filer,
        client,
        kind,
        scope,
        origin_name(origin),
        bind,
        summary,
    )
    .map(|id| json!({ "id": id }))
}

/// The requests waiting for the owner, from engine memory, for the owner's interface. Engine only.
pub fn pending_held() -> Result<Vec<Item>> {
    if !is_engine() {
        return Err(not_engine());
    }
    let mut h = held();
    prune_held(&mut h);
    Ok(h.requests
        .iter()
        .map(|r| Item {
            id: r.id.clone(),
            kind: r.kind.clone(),
            scope: r.scope.clone(),
            origin: r.origin.clone(),
            client: r.client.clone(),
            bind: r.bind.clone(),
            summary: r.summary.clone(),
            state: "pending".into(),
            created: r.created,
            decided: 0,
            last_used: 0,
            detail: r.detail.clone(),
        })
        .collect())
}

/// The engine's `remote.approvals.decide`: records the owner's answer to the pending request `id`
/// in engine memory. `digest` is `request_digest` of the item the owner was shown; a request whose
/// own digest differs is refused `approval_changed` and stays pending. **Only the engine calls
/// this, for the owner's owner app or hub.** `Ok(false)`: no such pending request, or it waited past
/// `PENDING_VALID`.
pub fn decide_held(id: &str, approve: bool, digest: &str) -> Result<bool> {
    decide_signed(id, approve, digest, None, None, None)
}

/// `decide_held` with the owner's proof, for the engine's `remote.approvals.decide` (acts, logins,
/// payments, budgets: every request that is not an unlock). Denying needs no proof. Approving needs
/// `method`: `"sig"` (an enrolled approver key signed this request, kind `approve`, bound to the
/// engine's own request id, digest and client: `approval_sig`) or `"pin"` (the host app PIN, verified
/// inside the engine). `"os"` (owner app's word that Touch ID or Windows Hello accepted the owner)
/// is refused on every platform, Windows included until Hello signs with an enrolled key. Without a
/// proof an approval is refused `unlock_needs_auth`, so a caller's word, the hub's click included,
/// never approves. An unlock request is refused here whatever the proof (`unlock::decide_signed`
/// answers those).
pub fn decide_signed(
    id: &str,
    approve: bool,
    digest: &str,
    method: Option<&str>,
    pin: Option<&str>,
    sig: Option<&Value>,
) -> Result<bool> {
    if !approve {
        return decide_inner(id, false, digest, false);
    }
    if !is_engine() {
        return Err(not_engine());
    }
    let (client, is_unlock) = {
        let mut h = held();
        prune_held(&mut h);
        match h.requests.iter().find(|r| r.id == id) {
            Some(r) => (
                r.client.clone(),
                r.kind == "session" && r.scope == UNLOCK_FAMILY,
            ),
            None => return Ok(false),
        }
    };
    if !is_unlock {
        match method {
            Some("sig") => {
                let signed = super::approval_sig::Signed::from_value(sig.unwrap_or(&Value::Null))?;
                super::approval_sig::verify("approve", id, digest, &client, &signed)?;
            }
            Some("os") => {
                return Err(Refusal::new(
                    "unlock_needs_auth",
                    "an approval needs the owner's signed approval (Touch ID on a Mac) or the PIN; a caller's word that the system accepted the owner is not enough, and on Windows Hello cannot sign yet, so use the PIN",
                ));
            }
            Some("pin") => super::unlock::confirm_pin(pin.unwrap_or(""))?,
            _ => {
                return Err(Refusal::new(
                    "unlock_needs_auth",
                    "an approval needs method sig (signed by the owner's key) or pin",
                ));
            }
        }
    }
    decide_inner(id, true, digest, false)
}

/// The engine's `remote.unlock.decide`, after the caller proved who the owner is: `unlock.rs`
/// verifies the owner's signature or the PIN before it calls this. Only a pending
/// request of the unlock family can be answered here, and the unlock ends at `expires` (Unix
/// seconds) or, when that is 0, with the chat. Denying needs no proof.
pub fn decide_unlock(id: &str, approve: bool, digest: &str, expires: u64) -> Result<bool> {
    let decided = decide_inner(id, approve, digest, true)?;
    if decided && approve && expires > 0 {
        let mut h = held();
        if let Some(d) = h
            .decisions
            .iter_mut()
            .find(|d| d.id == id && d.kind == "session" && d.scope == UNLOCK_FAMILY)
        {
            d.expires = expires;
        }
    }
    Ok(decided)
}

/// Whether request `id` is a waiting unlock request. Engine only; anything else is false.
pub fn is_unlock_request(id: &str) -> bool {
    if !is_engine() {
        return false;
    }
    let mut h = held();
    prune_held(&mut h);
    h.requests
        .iter()
        .any(|r| r.id == id && r.kind == "session" && r.scope == UNLOCK_FAMILY)
}

fn decide_inner(id: &str, approve: bool, digest: &str, unlock: bool) -> Result<bool> {
    if !is_engine() {
        return Err(not_engine());
    }
    let mut h = held();
    prune_held(&mut h);
    let Some(position) = h.requests.iter().position(|r| r.id == id) else {
        return Ok(false);
    };
    let r = &h.requests[position];
    let is_unlock = r.kind == "session" && r.scope == UNLOCK_FAMILY;
    // The unlock answer never approves anything else, and a plain approve never opens an unlock.
    // A plain deny of an unlock is allowed.
    if unlock && !is_unlock {
        return Err(Refusal::new(
            "approval_invalid",
            "that request is not an unlock request",
        ));
    }
    if is_unlock && !unlock && approve {
        return Err(Refusal::new(
            "unlock_needs_auth",
            "an unlock is approved only with Touch ID (Mac) or the PIN, from the notch or the hub's PIN field",
        ));
    }
    if request_digest(&r.kind, &r.scope, &r.origin, &r.bind, &r.summary) != digest {
        return Err(Refusal::new(
            "approval_changed",
            "what you answered is not what is waiting; refresh the list and look again",
        ));
    }
    let r = h.requests.remove(position);
    let at = now();
    // The decision is kept only in memory; this line is its durable trace. Codes only.
    super::audit::record_security(
        "approval.decided",
        serde_json::json!({
            "kind": r.kind,
            "door": r.origin,
            "approved": approve,
            "unlock": is_unlock,
        }),
    );
    h.decisions.push(Decision {
        id: r.id,
        kind: r.kind,
        scope: r.scope,
        origin: r.origin,
        filer: r.filer,
        client: r.client,
        bind: r.bind,
        summary: r.summary,
        approved: approve,
        decided: at,
        last_used: at,
        expires: 0,
    });
    prune_held(&mut h);
    Ok(true)
}

fn refused(id: &str, pending: bool, why: &str) -> Refusal {
    let code = if pending {
        "approval_pending"
    } else {
        "approval_invalid"
    };
    Refusal::new(code, format!("approval {id}: {why}"))
}

/// Uses the approval `id` from the engine's memory. Engine only. `filer` is the hello label that
/// filed the request (empty for a tool in the engine) and must be the caller's; `summary` of
/// `Some` must also equal what the owner was shown.
fn take_local(
    filer: &str,
    origin: &str,
    tool: &str,
    bind: &str,
    id: &str,
    summary: Option<&str>,
) -> Result<()> {
    if !is_engine() {
        return Err(not_engine());
    }
    let mut h = held();
    prune_held(&mut h);
    if h.used.iter().any(|u| u == id) {
        return Err(refused(id, false, "unknown, expired or already used"));
    }
    let different = "it was given for a different action or target; ask again";
    if let Some(position) = h
        .decisions
        .iter()
        .position(|d| d.kind == "act" && d.id == id)
    {
        let d = &h.decisions[position];
        if d.filer != filer || d.scope != tool || d.origin != origin || d.bind != bind {
            return Err(refused(id, false, different));
        }
        if summary.is_some_and(|shown| shown != d.summary) {
            return Err(refused(
                id,
                false,
                "what the owner was shown is not what this call asks; ask again",
            ));
        }
        if !d.approved {
            return Err(refused(id, false, "it was refused"));
        }
        h.decisions.remove(position);
        h.used.push(id.to_string());
        return Ok(());
    }
    if let Some(r) = h.requests.iter().find(|r| r.kind == "act" && r.id == id) {
        if r.filer != filer || r.scope != tool || r.origin != origin || r.bind != bind {
            return Err(refused(id, false, different));
        }
        return Err(refused(id, true, "still waiting for your answer"));
    }
    Err(refused(id, false, "unknown, expired or already used"))
}

/// Uses the approval `id` for the action `bind`, on the engine's record of the owner's answer:
/// in memory when this process is the engine, otherwise through the resident engine, which takes
/// the door from this process's hello label and requires that same label to have filed the
/// request. Same rules for every approval (approved, unexpired, unused, same tool, door and bind;
/// used up by passing); a request that was not answered through the engine's owner-only decide
/// never passes.
pub fn take_act_held(origin: Origin, tool: &str, bind: &str, id: &str) -> Result<()> {
    if is_engine() {
        return take_local("", origin_name(origin), tool, bind, id, None);
    }
    ask_engine(
        "remote.approvals.take",
        json!({"tool": tool, "bind": bind, "id": id}),
    )
    .map(|_| ())
}

/// `take_act_held` that also requires the summary the owner was shown to be exactly `summary`.
/// **Engine only**, and only for a request a tool in the engine filed (the budget approval, whose
/// summary the engine writes itself): any other process gets `approval_unavailable`.
pub fn take_act_exact(
    origin: Origin,
    tool: &str,
    bind: &str,
    id: &str,
    summary: &str,
) -> Result<()> {
    take_local(
        "",
        origin_name(origin),
        tool,
        bind,
        id,
        Some(&clean_summary(summary)),
    )
}

/// The engine's answer to `remote.approvals.take`: `filer` is the caller's hello label and
/// `origin` the door that label implies.
pub fn serve_take(filer: &str, origin: Origin, tool: &str, bind: &str, id: &str) -> Result<Value> {
    take_local(filer, origin_name(origin), tool, bind, id, None).map(|_| json!({"ok": true}))
}

/// Where request `id` stands. Read-only and open to any caller: it says `approved` only for an
/// answer the owner gave the engine, never for a file entry.
pub fn outcome(id: &str) -> Result<Outcome> {
    if !is_engine() {
        let answer = ask_engine("remote.approvals.outcome", json!({"id": id}))?;
        return Outcome::from_json(&answer).ok_or_else(engine_unreachable);
    }
    serve_outcome(id)
}

/// The engine's answer to `remote.approvals.outcome`.
pub fn serve_outcome(id: &str) -> Result<Outcome> {
    if !is_engine() {
        return Err(not_engine());
    }
    let mut h = held();
    prune_held(&mut h);
    let known = |state: &str, kind: &str, scope: &str, origin: &str, decided: u64| Outcome {
        id: id.to_string(),
        state: state.to_string(),
        kind: kind.to_string(),
        scope: scope.to_string(),
        origin: origin.to_string(),
        decided,
    };
    if let Some(d) = h.decisions.iter().find(|d| d.id == id) {
        let state = if d.approved { "approved" } else { "denied" };
        return Ok(known(state, &d.kind, &d.scope, &d.origin, d.decided));
    }
    if h.used.iter().any(|u| u == id) {
        return Ok(known("used", "act", "", "", 0));
    }
    if let Some(r) = h.requests.iter().find(|r| r.id == id) {
        return Ok(known("pending", &r.kind, &r.scope, &r.origin, 0));
    }
    Ok(known("unknown", "", "", "", 0))
}

/// Whether a session grant the owner gave through the engine is active for `family` on this door;
/// an active grant counts as use. `Ok(false)` means none: ask with `request_session_held`.
pub fn session_active_held(origin: Origin, family: &str) -> Result<bool> {
    let client = current_client();
    if !is_engine() {
        let answer = ask_engine(
            "remote.approvals.session",
            json!({"family": family, "client": client}),
        )?;
        return Ok(answer["active"].as_bool().unwrap_or(false));
    }
    serve_session(origin, &client, family)
}

/// The engine's answer to `remote.approvals.session`: `origin` is the door the caller's hello
/// label implies, so only the gateway label uses or refreshes a gateway-door grant. A grant also
/// belongs to one `client` (a chat, the agent, the gateway), never to every caller of the door.
pub fn serve_session(origin: Origin, client: &str, family: &str) -> Result<bool> {
    if !is_engine() {
        return Err(not_engine());
    }
    let origin = origin_name(origin);
    let client = clean_client(client);
    if family == UNLOCK_FAMILY && origin == origin_name(Origin::Local) {
        // After an engine restart the memory holds no grant: a valid stored token (OS credential
        // store, sealed, with an expiry; `unlock::restore_token`) brings the chat's unlock back.
        let known = held().decisions.iter().any(|d| {
            d.kind == "session"
                && d.approved
                && d.scope == family
                && d.origin == origin
                && d.client == client
        });
        if !known && let Some((token_expires, until_end)) = super::unlock::restore_token(&client) {
            // An "until the chat ends" grant keeps expires 0, so it stays bound to the chat's liveness.
            let expires = if until_end { 0 } else { token_expires };
            let mut h = held();
            let at = now();
            h.decisions.push(Decision {
                id: new_id(),
                kind: "session".into(),
                scope: family.into(),
                origin: origin.into(),
                filer: String::new(),
                client: client.clone(),
                bind: String::new(),
                summary: "restored from the stored unlock token".into(),
                approved: true,
                decided: at,
                last_used: at,
                expires,
            });
        }
    }
    if family == UNLOCK_FAMILY {
        // An unlock that lasts until its chat ends is checked against the roster on every use,
        // outside the record's lock (it reads the chat files).
        let until_chat_ends = {
            let mut h = held();
            prune_held(&mut h);
            h.decisions.iter().any(|d| {
                d.kind == "session"
                    && d.approved
                    && d.scope == family
                    && d.origin == origin
                    && d.client == client
                    && d.expires == 0
            })
        };
        if until_chat_ends && !super::unlock::chat_alive(&client) {
            super::unlock::forget_token(&client);
            let mut h = held();
            h.decisions.retain(|d| {
                !(d.kind == "session"
                    && d.scope == family
                    && d.origin == origin
                    && d.client == client)
            });
            return Ok(false);
        }
    }
    let mut h = held();
    prune_held(&mut h);
    let at = now();
    Ok(
        match h.decisions.iter_mut().find(|d| {
            d.kind == "session"
                && d.approved
                && d.scope == family
                && d.origin == origin
                && d.client == client
        }) {
            Some(d) => {
                d.last_used = at;
                true
            }
            None => false,
        },
    )
}

/// Ends every session grant, and drops every waiting session request, for `family`. It only ever
/// reduces trust, so any caller may ask.
pub fn revoke_sessions_held(family: &str) -> Result<usize> {
    if !is_engine() {
        let answer = ask_engine("remote.approvals.revoke", json!({"family": family}))?;
        return Ok(answer["removed"].as_u64().unwrap_or(0) as usize);
    }
    serve_revoke(family)
}

/// The engine's answer to `remote.approvals.revoke`.
pub fn serve_revoke(family: &str) -> Result<usize> {
    if !is_engine() {
        return Err(not_engine());
    }
    let mut h = held();
    let before = h.decisions.len() + h.requests.len();
    h.decisions
        .retain(|d| !(d.kind == "session" && d.scope == family));
    h.requests
        .retain(|r| !(r.kind == "session" && r.scope == family));
    let removed = before - h.decisions.len() - h.requests.len();
    drop(h);
    // Memory is cleared first; a token that cannot be removed is reported.
    if family == UNLOCK_FAMILY {
        super::unlock::forget_all_tokens()?;
    }
    Ok(removed)
}
