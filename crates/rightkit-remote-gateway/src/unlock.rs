//! Per-chat unlock (the owner, 2026-10-17): an external chat (Claude Code or Codex through the host app
//! plugin or CLI) must be unlocked once before the tools that change this computer run for it.
//!
//! > "When it asks for that permission at that particular time to run the browser, it the host app asks for
//! > a PIN. It can either be the system PIN or hello or whatever ... or it can be a specific PIN
//! > that is set in privately in the host app securely ... So it's on a chat by chat basis as opposed to a
//! > harness level or a model level ... if I start an agent inside the host app, then it has the
//! > permission already."
//!
//! * **Who is a chat.** The caller names itself with `fromChat`, its session id from Chat
//!   roster (`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`; `claude:<id>` and `codex:<id>` are
//!   accepted). The id must be a chat the roster lists, terminal chats included even when the
//!   roster hides them; an unknown id is `chat_unknown`, and a call with no id at all is
//!   `chat_unidentified`. Only a chat the roster calls `live` counts as alive (unknown liveness is
//!   not alive). For a Claude chat the id is verified: the engine stamps the caller's process from
//!   the socket and the call is refused `chat_mismatch` unless that process is the chat's own
//!   process (the pid in its session file) or a descendant of it. A Codex chat records no process,
//!   so for Codex the id stays a claim: a process of the same user that reads another Codex
//!   thread's id can claim it. The unlock keeps an honest chat from acting before the owner said
//!   so; it is not a boundary against code that already runs as the user.
//! * **What is gated.** `screen.act` (form fill included), `apps.launch`, `media.control`,
//!   `files.patch`, `files.manage`, `term.*` and `procs.stop`, on the local door only, when the
//!   switch of the tool itself is on (a tool that is off keeps its own refusal and asks nothing).
//!   Reading tools are not gated. Sensitive values are still never typed and a submit still asks
//!   every time; the unlock replaces nothing in `screen.rs`.
//! * **The agent.** An agent started inside the host app (`agent.act`) acts with the authority of whoever
//!   submitted its task, as the engine's socket saw them (`submitter`, stamped by `resident.rs` and
//!   sealed by this engine process; a JSON field of the task never promotes anyone). Submitted by the
//!   owner's own owner app or hub (executable verified), it is `Requester::Agent` and already unlocked.
//!   Submitted from a chat (a Claude chat found by process ancestry, or a chat named by `fromChat`,
//!   verified like a tool call), it is `Requester::AgentFor(<chat>)`: the chat must be unlocked to
//!   submit, and every effectful call of the run needs that chat's unlock again. Submitted by
//!   anything else it is unidentified and, with `chatUnlock` on, refused. ChatGPT through the gateway
//!   keeps its per-action approvals and is not asked to unlock. `chatUnlock` off turns the whole
//!   gate off.
//! * **How it is unlocked.** A locked call files one pending request (kind `session`, family
//!   `unlock`, client `chat:<id>`) in the engine's approval record and answers `chat_locked`. The
//!   owner answers on this computer, in owner app: Touch ID on a Mac (a Secure Enclave key whose
//!   use needs the fingerprint; owner app signs the request and sends `remote.unlock.decide` with
//!   `method: "sig"`, which `approval_sig.rs` verifies against an enrolled key, the engine's own
//!   request id, digest and client, a short expiry and a single-use nonce) or the host app PIN
//!   (`method: "pin"`, verified only here). `method: "os"` (owner app's word that the system accepted
//!   the owner) is refused on every platform: a caller's word is not an approval. On Windows that
//!   leaves the PIN until Windows Hello signs approvals with an enrolled key. The generic
//!   `remote.approvals.decide` refuses to approve an unlock.
//! * **The PIN** is set from the hub (`remote.unlock.pin.set`) and stored as a salted PBKDF2-HMAC-
//!   SHA-256 hash in the OS credential store (`rightkit-credentials`), never in a file or a log.
//!   Three wrong PINs are free; each one after that locks entry for 30 seconds, doubling every time
//!   up to 24 hours. The count and the lock are kept in the credential store, so a restart does not
//!   clear them, and every check runs under one lock; a guess is counted before it is checked, and
//!   a store that cannot record the count or cannot be read refuses the check. Changing a PIN needs
//!   the current one. The engine's own store records that a PIN was set
//!   (`protected_store::mark_pin_set`), so deleting only the PIN's credential item makes the PIN lost
//!   (`pin_lost`): it cannot be reset without the old one, except through an authenticated recovery
//!   (`remote.security.recover`, `authorise_recovery`): the current PIN, or a signature of an
//!   enrolled, proven approver key over an engine-issued, single-use challenge, the exact operation
//!   (repair, and the new PIN's digest) and this engine process. The owner app's word that Touch ID or
//!   Windows Hello accepted the owner is not a proof. The approver keys are proven only after the PIN
//!   has been entered since the engine started, so a lost PIN after a restart, or a lost or replaced
//!   key, cannot be recovered here: recovery is refused (`recovery_unavailable`) and nothing changes.
//!   If the engine's item, its marker file and the PIN item are all
//!   deleted, that is a first run and the PIN reads as unset (every gate is then off).
//!   The PIN also confirms a change that makes a security setting less safe (`owner_settings.rs`).
//! * **How long.** Until the chat ends (the roster no longer lists it live, checked on use) when
//!   `chatUnlockMinutes` is 0 and the roster calls the chat live, else that many minutes. A chat
//!   the roster does not call live (registered, unknown) cannot be seen ending, so with 0 it gets
//!   15 minutes (`MIN_UNLOCK_SECONDS`) instead of a prompt on every call. A grant never idles out.
//!   The record lives in the engine's memory. After an owner-check unlock (Touch ID or password on
//!   the Mac, Windows Hello on Windows: `method: "sig"` with an enrolled owner-check key, no PIN) the
//!   engine also keeps one token for the chat in the OS credential store (`store_token`: sealed by the
//!   owner-check secret, with the chat, an expiry of at most 24 hours and a generation), and a restart
//!   restores the unlock from a valid one (`restore_token`) instead of locking every chat again.
//!   Revoking all unlocks bumps the generation. A token for an "until the chat ends" unlock also needs
//!   the chat still live.
//! * **Keyed by client.** Session grants everywhere (`approvals`) belong to one client, so a grant
//!   given to one chat never covers another chat, the agent or the gateway.

use super::settings::Settings;
use super::{Call, Origin, Refusal, Result, approvals};
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Who is asking for a tool on the local door.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Requester {
    /// The caller did not say which chat it is.
    #[default]
    Unidentified,
    /// An external chat, by the session id it gave.
    Chat(String),
    /// The agent the engine started itself (`agent.act`) for a task the owner's owner app or hub
    /// submitted. Set only inside the engine, from the engine's own seal (`submitter`).
    Agent,
    /// The agent the engine started for a task a chat submitted: it acts under that chat's unlock.
    /// Set only inside the engine, from the engine's own seal (`submitter`).
    AgentFor(String),
}

impl Requester {
    /// The requester a request names: `fromChat` of its params. Never `Agent`.
    pub fn from_params(params: &Value) -> Requester {
        let id = params["fromChat"].as_str().map(str::trim).unwrap_or("");
        let id = match id.split_once(':') {
            Some((kind, rest)) if super::chat::CHAT_KINDS.contains(&kind) => rest,
            _ => id,
        };
        if id.is_empty() {
            Requester::Unidentified
        } else {
            Requester::Chat(id.to_string())
        }
    }
}

/// The client key session grants and requests use for a call (`approvals::set_client`).
pub fn client_key(call: &Call) -> String {
    match (call.origin, &call.requester) {
        (Origin::Gateway, _) => format!("gateway:{}", call.client),
        (Origin::Local, Requester::Agent) => "agent".to_string(),
        // Its own grants, so the chat's per-action grants never cover its agent (and `agent.stop`
        // finds its requests); the unlock it needs is the chat's (`require`).
        (Origin::Local, Requester::AgentFor(id)) => format!("agent:chat:{id}"),
        (Origin::Local, Requester::Chat(id)) => format!("chat:{id}"),
        (Origin::Local, Requester::Unidentified) => format!("local:{}", call.client),
    }
}

/// The tools that change this computer and so need an unlocked chat.
pub fn effectful(tool: &str) -> bool {
    matches!(
        tool,
        "screen.act"
            | "apps.launch"
            | "media.control"
            | "login.fill"
            | "payment.fill"
            | "files.patch"
            | "files.manage"
            | "docs.write"
            | "term.exec"
            | "term.write"
            | "term.read"
            | "term.stop"
            | "term.list"
            | "term.resize"
            | "procs.stop"
    )
}

/// The refusal of a tool whose own switch is off, so a tool that cannot run anyway asks nothing.
fn tool_switch(settings: &Settings, tool: &str) -> Result<()> {
    match tool {
        "screen.act" => settings.require_screen_act(),
        "apps.launch" => settings.require_launch(),
        "media.control" => settings.require_media(),
        "login.fill" => settings.require_logins(),
        "payment.fill" => settings.require_payments(),
        "files.patch" | "files.manage" => settings.require_files_write(),
        "docs.write" => settings
            .require_docs()
            .and_then(|()| settings.require_files_write()),
        _ => settings.require_terminals(),
    }
}

/// A session id as a chat has one: letters, digits, `.`, `_`, `-`, at most 128.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Lets the call through, or refuses it `chat_locked` (and asks the owner), `chat_unknown` or
/// `chat_unidentified`. Only an effectful call on the local door is checked.
pub fn require(settings: &Settings, call: &Call) -> Result<()> {
    if call.origin != Origin::Local || !effectful(&call.tool) {
        return Ok(());
    }
    tool_switch(settings, &call.tool)?;
    if !settings.chat_unlock {
        return Ok(());
    }
    match &call.requester {
        Requester::Agent => Ok(()),
        Requester::Unidentified => Err(unidentified()),
        Requester::Chat(id) => {
            let chat = verify_chat(id, Caller::Process(call.caller_pid))?;
            unlocked_or_ask(settings, id, &chat, what_for(&call.tool))
        }
        // The engine's agent for a chat: the chat was verified when its task was submitted
        // (`submitter`), and the run's calls come from the engine's own broker, so only that the
        // chat is still listed and still unlocked is checked, under the chat's own client key.
        Requester::AgentFor(id) => {
            let chat = verify_chat(id, Caller::Verified)?;
            let _client = approvals::set_client(&format!("chat:{id}"));
            unlocked_or_ask(settings, id, &chat, what_for(&call.tool))
        }
    }
}

fn unidentified() -> Refusal {
    Refusal::new(
        "chat_unidentified",
        "The app cannot tell which chat is asking, so it cannot ask the owner to unlock it. Pass this chat's session id as fromChat (CLAUDE_CODE_SESSION_ID for Claude Code, CODEX_THREAD_ID for Codex), or run the tool through `pulse act`",
    )
}

/// How a chat's id is checked against the process that made the call.
#[derive(Clone, Copy)]
enum Caller {
    /// The process the engine's socket reported, `None` when the door could not tell.
    Process(Option<u32>),
    /// Checked already, when the agent's task was submitted (`submitter`).
    Verified,
}

/// The roster's chat for `id`, or `invalid_request`, `chat_unknown` or `chat_mismatch`.
fn verify_chat(id: &str, caller: Caller) -> Result<Chat> {
    if !valid_id(id) {
        return Err(Refusal::new(
            "invalid_request",
            "fromChat is a chat's session id",
        ));
    }
    let Some(chat) = chat_info(id) else {
        return Err(Refusal::new(
            "chat_unknown",
            "The app does not list a live chat with that session id on this computer, so it cannot ask the owner to unlock it",
        ));
    };
    // The id is a claim. Where the chat's own process is known (Claude), the caller must be that
    // process or one of its descendants, so another process cannot borrow an unlocked chat's id.
    // A caller the door could not identify is refused too. Codex records no process: its id stays
    // a claim (the same-user limit above).
    if let (Some(chat_pid), Caller::Process(caller_pid)) = (chat.pid, caller) {
        let verified = caller_pid.is_some_and(|caller| descends_from(caller, chat_pid));
        if !verified {
            return Err(Refusal::new(
                "chat_mismatch",
                "this call does not come from the chat it names (fromChat); run it from that chat's own session",
            ));
        }
    }
    Ok(chat)
}

/// Passes when the current client (`approvals::set_client`, `chat:<id>`) holds an unlock; else
/// files one unlock request for the owner and refuses `chat_locked`.
fn unlocked_or_ask(settings: &Settings, id: &str, chat: &Chat, what: &str) -> Result<()> {
    if approvals::session_active_held(Origin::Local, approvals::UNLOCK_FAMILY)? {
        return Ok(());
    }
    let how_long = match settings.chat_unlock_minutes {
        0 => "until the chat ends".to_string(),
        n => format!("for {n} minutes"),
    };
    let summary = format!(
        "{} chat \"{}\" ({}) wants to act on this computer, first {}. Unlocking lets it open apps, control windows, change files in approved folders and run terminal commands {how_long}; it still never types passwords or card numbers, and sending or buying still asks every time",
        chat.kind_word(),
        chat.name,
        short_id(id),
        what,
    );
    Err(approvals::request_unlock_held(Origin::Local, &summary))
}

/// Who submitted an `agent.act` task, from what the engine's socket saw (`resident.rs`), never
/// from the task's JSON: `owner_ui` (the installed owner app or hub, executable verified) is the owner
/// (`Agent`); `params.fromChat` names a chat, verified like a tool call's (`chat_unknown`,
/// `chat_mismatch`); with no name, a Claude chat the caller descends from; anything else is
/// `Unidentified`. Engine only.
pub fn submitter(params: &Value, caller_pid: Option<u32>, owner_ui: bool) -> Result<Requester> {
    if owner_ui {
        return Ok(Requester::Agent);
    }
    if let Requester::Chat(id) = Requester::from_params(params) {
        verify_chat(&id, Caller::Process(caller_pid))?;
        return Ok(Requester::Chat(id));
    }
    Ok(caller_pid
        .and_then(chat_of_process)
        .map_or(Requester::Unidentified, Requester::Chat))
}

/// Whether `requester` (the engine's stamp of a task's submitter, `submitter`) may start an
/// acting agent now: the owner always; a chat only while it is unlocked (else the owner is asked
/// and this refuses `chat_locked`); an unidentified submitter is refused `chat_unidentified`.
/// `chatUnlock` off lets every submitter through. Engine only.
pub fn require_submitter(settings: &Settings, requester: &Requester) -> Result<()> {
    if !settings.chat_unlock {
        return Ok(());
    }
    match requester {
        Requester::Agent => Ok(()),
        Requester::Unidentified => Err(unidentified()),
        Requester::Chat(id) | Requester::AgentFor(id) => {
            let chat = verify_chat(id, Caller::Verified)?;
            let _client = approvals::set_client(&format!("chat:{id}"));
            unlocked_or_ask(settings, id, &chat, "through the app's own acting agent")
        }
    }
}

fn what_for(tool: &str) -> &'static str {
    match tool {
        "screen.act" => "to click, type or press keys in an app",
        "apps.launch" => "to open an app",
        "media.control" => "to control music playback",
        "login.fill" => "to type a saved login into your browser",
        "payment.fill" => "to type a saved virtual card into a checkout",
        "files.patch" | "files.manage" | "docs.write" => "to change a file",
        _ => "to use the terminal",
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// What the roster knows of a chat.
#[derive(Clone)]
struct Chat {
    name: String,
    kind: String,
    /// Only a chat the roster calls `live`. "unknown", "stale" and "unreachable" are not alive.
    alive: bool,
    /// The chat's own process, when its session file records one (Claude). Codex records none, so
    /// a Codex id stays a claim.
    pid: Option<u32>,
}

impl Chat {
    fn kind_word(&self) -> &'static str {
        match self.kind.as_str() {
            "codex" => "Codex",
            "muse" => "Muse",
            _ => "Claude",
        }
    }
}

/// A chat of the Chat roster on this computer, by session id. The roster is read with terminal
/// chats included, so a CLI chat hidden from the roster list can still ask for an unlock.
fn lookup(id: &str) -> Option<Chat> {
    let store = rightkit_chat_bridge::chat::Store::open_default().ok()?;
    rightkit_chat_bridge::chat::roster::local_sessions_with_terminal(&store)
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| Chat {
            name: s
                .name
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(60)
                .collect(),
            kind: s.kind,
            alive: s.liveness == "live",
            pid: s.pid,
        })
}

/// The chat of the roster whose own process (Claude: the pid in its session file) is `pid` or its
/// nearest ancestor. A Codex chat records no process and is never found this way.
fn chat_of_process(pid: u32) -> Option<String> {
    let line = ancestors(pid);
    let store = rightkit_chat_bridge::chat::Store::open_default().ok()?;
    let sessions = rightkit_chat_bridge::chat::roster::local_sessions_with_terminal(&store);
    line.iter().find_map(|at| {
        sessions
            .iter()
            .find(|s| s.pid == Some(*at) && valid_id(&s.id))
            .map(|s| s.id.clone())
    })
}

/// How long a roster answer for a chat is reused. The roster reads session files and the Codex
/// index, which one call per tool use should not repeat.
const ALIVE_CACHE: Duration = Duration::from_secs(20);

type ChatCache = Vec<(String, Instant, Option<Chat>)>;
static CHATS: Mutex<ChatCache> = Mutex::new(Vec::new());

/// `lookup`, remembered for `ALIVE_CACHE`.
fn chat_info(id: &str) -> Option<Chat> {
    let mut cache = CHATS.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(_, at, _)| at.elapsed() < ALIVE_CACHE);
    if let Some((_, _, chat)) = cache.iter().find(|(key, _, _)| key == id) {
        return chat.clone();
    }
    drop(cache);
    let chat = lookup(id);
    let mut cache = CHATS.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 64 {
        cache.remove(0);
    }
    cache.push((id.to_string(), Instant::now(), chat.clone()));
    chat
}

/// Whether the chat behind a client key (`chat:<id>`) is still a live chat. Fails closed: a chat
/// the roster no longer lists, lists as anything but `live` (unknown liveness included), or a
/// build with no roster, has ended.
pub fn chat_alive(client: &str) -> bool {
    let Some(id) = client.strip_prefix("chat:") else {
        return false;
    };
    chat_info(id).is_some_and(|c| c.alive)
}

/// Whether process `pid` is `ancestor` or one of its descendants (the parent chain, at most 64
/// steps). Unreadable process information is "no".
fn descends_from(pid: u32, ancestor: u32) -> bool {
    ancestors(pid).contains(&ancestor)
}

/// `pid` and its parent chain, nearest first, at most 64 entries; the chain ends where process
/// information cannot be read.
fn ancestors(pid: u32) -> Vec<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut line = Vec::new();
    let Ok(mut system) = System::new() else {
        // The chain ends where process information cannot be read: here, at the first entry.
        line.push(pid);
        return line;
    };
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let mut at = Pid::from_u32(pid);
    for _ in 0..64 {
        line.push(at.as_u32());
        match system.process(at).and_then(|p| p.parent()) {
            Some(parent) if parent != at => at = parent,
            _ => break,
        }
    }
    line
}

// ---- the engine's seals ----------------------------------------------------------------------

/// A key this engine process makes once and never stores: a seal (`seal`) proves that this process
/// wrote a stamp (an agent task's submitter, `resident.rs`), so a stamp copied from a request, from
/// another process or from before a restart proves nothing.
static SEAL_KEY: std::sync::OnceLock<Option<ring::hmac::Key>> = std::sync::OnceLock::new();

fn seal_key() -> Option<&'static ring::hmac::Key> {
    SEAL_KEY
        .get_or_init(|| {
            ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &ring::rand::SystemRandom::new())
                .ok()
        })
        .as_ref()
}

/// The hex HMAC of `text` under this process's seal key. Engine only.
pub fn seal(text: &str) -> Result<String> {
    let unavailable = || {
        Refusal::new(
            "seal_unavailable",
            "this engine cannot seal who asked, so nothing was accepted",
        )
    };
    if !approvals::is_engine_process() {
        return Err(unavailable());
    }
    let key = seal_key().ok_or_else(unavailable)?;
    Ok(rightkit_fs::digest::hex(
        ring::hmac::sign(key, text.as_bytes()).as_ref(),
    ))
}

/// Whether `mac` is this process's seal of `text` (constant time). False outside the engine, for a
/// malformed seal and for a seal made by any other process or before a restart.
pub fn sealed(text: &str, mac: &str) -> bool {
    if !approvals::is_engine_process() || mac.len() != 64 {
        return false;
    }
    let (Some(key), Some(tag)) = (seal_key(), rightkit_fs::digest::unhex(mac)) else {
        return false;
    };
    ring::hmac::verify(key, text.as_bytes(), &tag).is_ok()
}

// ---- the host app PIN ---------------------------------------------------------------------------

const PIN_SERVICE: &str = "pulse.chat-unlock";
const PIN_ACCOUNT: &str = "pin-v1";
const PIN_MIN: usize = 6;
const PIN_MAX: usize = 64;
const PBKDF2_ITERATIONS: u32 = 600_000;
const SALT_BYTES: usize = 16;
const HASH_BYTES: usize = 32;
/// The shortest an unlock lasts for a chat the roster does not call live (registered, unknown,
/// stale) when `chatUnlockMinutes` is 0: such a chat's end cannot be seen, so "until it ends" would
/// ask again on every call.
const MIN_UNLOCK_SECONDS: u64 = 15 * 60;
/// Wrong PINs in a row that cost nothing; the next one locks entry for `LOCK_BASE_SECONDS`, and each
/// wrong PIN after that doubles the lock, up to `LOCK_MAX_SECONDS`.
const FREE_FAILURES: u32 = 3;
const LOCK_BASE_SECONDS: u64 = 30;
const LOCK_MAX_SECONDS: u64 = 24 * 60 * 60;
const ATTEMPTS_ACCOUNT: &str = "attempts-v1";

/// Serialises every PIN check from the lock test to the recorded outcome, so parallel guesses
/// cannot all pass the lock test before the first failure is counted.
static PIN_GATE: Mutex<()> = Mutex::new(());

fn pin_store() -> Result<rightkit_credentials::Store> {
    let unavailable = |why: String| {
        Refusal::new(
            "pin_unavailable",
            format!("the OS credential store cannot hold the PIN: {why}"),
        )
    };
    let namespace = rightkit_credentials::Namespace::new(PIN_SERVICE.to_string())
        .map_err(|e| unavailable(e.to_string()))?;
    let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
    if !store.is_durable() {
        return Err(unavailable("the store is guarded or unavailable".into()));
    }
    Ok(store)
}

/// `v1$<iterations>$<salt hex>$<hash hex>` of `pin` under a fresh salt.
fn hash_pin(pin: &str) -> Result<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut salt = [0u8; SALT_BYTES];
    SystemRandom::new().fill(&mut salt).map_err(|_| {
        Refusal::new(
            "pin_unavailable",
            "the system could not make a random salt for the PIN",
        )
    })?;
    let mut hash = [0u8; HASH_BYTES];
    let iterations = std::num::NonZeroU32::new(PBKDF2_ITERATIONS).expect("a non-zero count");
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        &salt,
        pin.as_bytes(),
        &mut hash,
    );
    Ok(format!(
        "v1${PBKDF2_ITERATIONS}${}${}",
        rightkit_fs::digest::hex(&salt),
        rightkit_fs::digest::hex(&hash)
    ))
}

/// Whether `pin` is the one `stored` holds (`ring` compares in constant time).
fn pin_matches(stored: &str, pin: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("v1"), Some(iterations), Some(salt), Some(hash), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    let Some(iterations) = iterations
        .parse::<u32>()
        .ok()
        .filter(|n| (1..=10_000_000).contains(n))
        .and_then(std::num::NonZeroU32::new)
    else {
        return false;
    };
    let (Some(salt), Some(hash)) = (
        rightkit_fs::digest::unhex(salt),
        rightkit_fs::digest::unhex(hash),
    ) else {
        return false;
    };
    ring::pbkdf2::verify(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        &salt,
        pin.as_bytes(),
        &hash,
    )
    .is_ok()
}

/// A PIN being checked: any length up to `PIN_MAX` (PINs set before the minimum rose to 6 still
/// verify), no control characters.
fn checkable_pin(pin: &str) -> Result<()> {
    if pin.is_empty() || pin.chars().count() > PIN_MAX || pin.chars().any(char::is_control) {
        return Err(Refusal::new("pin_invalid", "that is not a valid PIN"));
    }
    Ok(())
}

/// A new PIN: `PIN_MIN` to `PIN_MAX` characters.
fn valid_pin(pin: &str) -> Result<()> {
    let n = pin.chars().count();
    if !(PIN_MIN..=PIN_MAX).contains(&n) || pin.chars().any(char::is_control) {
        return Err(Refusal::new(
            "pin_invalid",
            format!("a PIN is {PIN_MIN} to {PIN_MAX} characters with no control characters"),
        ));
    }
    Ok(())
}

/// Whether the PIN's own credential item exists. An unreadable store is "no". Engine only.
pub(super) fn pin_item_present() -> bool {
    pin_store()
        .and_then(|store| {
            store
                .get(PIN_ACCOUNT)
                .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))
        })
        .is_ok_and(|secret| secret.is_some())
}

/// Whether the host app PIN is set. Engine only (the hub and owner app ask through the engine). A PIN
/// the engine recorded as set (`protected_store::pin_was_set`) stays set when its credential item
/// has been deleted: the PIN is then lost, not unset, and cannot be reset without the old one.
pub fn pin_is_set() -> Result<bool> {
    let item = pin_store()?
        .get(PIN_ACCOUNT)
        .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))?
        .is_some();
    Ok(item || super::protected_store::pin_was_set()?)
}

/// `(failures in a row, lock end in Unix seconds)` as the credential store keeps them, so a
/// restart does not clear a lockout. An unreadable or damaged record counts as locked out for the
/// base time (fail closed); only a record that is not there is a clean slate.
fn read_attempts(store: &rightkit_credentials::Store) -> (u32, u64) {
    let locked = || (FREE_FAILURES + 1, now_secs() + LOCK_BASE_SECONDS);
    let secret = match store.get(ATTEMPTS_ACCOUNT) {
        Ok(Some(secret)) => secret,
        Ok(None) => return (0, 0),
        Err(_) => return locked(),
    };
    let Ok(text) = secret.expose_str() else {
        return locked();
    };
    let mut parts = text.split('$');
    match (
        parts.next().and_then(|f| f.parse().ok()),
        parts.next().and_then(|u| u.parse().ok()),
    ) {
        (Some(f), Some(u)) => (f, u),
        _ => locked(),
    }
}

/// Records the count and the lock. A store that cannot record it is an error, and the caller
/// refuses without looking at the PIN (fail closed).
fn write_attempts(store: &rightkit_credentials::Store, failures: u32, until: u64) -> Result<()> {
    store
        .set_str(ATTEMPTS_ACCOUNT, &format!("{failures}${until}"))
        .map_err(|e| {
            Refusal::new(
                "pin_unavailable",
                format!("the PIN attempt count cannot be recorded, so no PIN is checked: {e}"),
            )
        })
}

fn lock_seconds(failures: u32) -> u64 {
    if failures <= FREE_FAILURES {
        return 0;
    }
    let doublings = (failures - FREE_FAILURES - 1).min(20);
    LOCK_BASE_SECONDS
        .saturating_mul(1u64 << doublings)
        .min(LOCK_MAX_SECONDS)
}

/// Checks `pin` against the stored hash: `pin_not_set`, `pin_wrong`, `pin_locked`, `pin_invalid`.
/// Serialised, with an exponential lockout that survives a restart.
fn verify_pin(pin: &str) -> Result<()> {
    let _gate = PIN_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let store = pin_store()?;
    let (failures, until) = read_attempts(&store);
    let now = now_secs();
    if until > now {
        super::audit::record_security(
            "pin.refused_locked",
            serde_json::json!({ "lockSeconds": until - now }),
        );
        return Err(Refusal::new(
            "pin_locked",
            format!(
                "too many wrong PINs; PIN entry is locked for {} more seconds",
                until - now
            ),
        ));
    }
    checkable_pin(pin)?;
    let stored = match store
        .get(PIN_ACCOUNT)
        .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))?
    {
        Some(stored) => stored,
        None if super::protected_store::pin_was_set()? => {
            return Err(Refusal::new(
                "pin_lost",
                "the PIN was set but its stored hash is gone (deleted or reset from outside the app), so no PIN can be checked; on a Mac, reset it from the notch (Lost PIN, signed with Touch ID by the notch's enrolled key); Windows Hello cannot prove a reset yet",
            ));
        }
        None => {
            return Err(Refusal::new(
                "pin_not_set",
                "no PIN is set; set one in the app settings, or on a Mac unlock with Touch ID in the notch",
            ));
        }
    };
    let text = stored
        .expose_str()
        .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))?;
    // This guess is counted before it is looked at, so a store that cannot record it refuses the
    // check instead of allowing uncounted guesses.
    let failures = failures.saturating_add(1);
    let wait = lock_seconds(failures);
    write_attempts(&store, failures, if wait == 0 { 0 } else { now + wait })?;
    if pin_matches(text, pin) {
        // A reset that fails leaves the count where it is: safe, and the next right PIN clears it.
        let _ = write_attempts(&store, 0, 0);
        // The right PIN also opens the seal on the protected settings (macOS, Windows). Touch ID alone does not.
        super::protected_store::pin_verified(pin);
        if failures > 1 {
            super::audit::record_security(
                "pin.ok_after_failures",
                serde_json::json!({ "failures": failures - 1 }),
            );
        }
        return Ok(());
    }
    super::audit::record_security(
        if wait == 0 {
            "pin.wrong"
        } else {
            "pin.locked_out"
        },
        serde_json::json!({ "failures": failures, "lockSeconds": wait }),
    );
    if wait == 0 {
        Err(Refusal::new(
            "pin_wrong",
            format!(
                "wrong PIN; {} attempts left before PIN entry locks",
                FREE_FAILURES + 1 - failures
            ),
        ))
    } else {
        Err(Refusal::new(
            "pin_locked",
            format!("wrong PIN; PIN entry is locked for {wait} seconds"),
        ))
    }
}

/// Checks the owner's PIN for a change that makes a gate less safe (`owner_settings.rs`).
pub fn confirm_pin(pin: &str) -> Result<()> {
    verify_pin(pin)
}

/// Sets or changes the PIN. A PIN that exists needs `current` (counted like any attempt).
pub fn pin_set(pin: &str, current: Option<&str>) -> Result<()> {
    valid_pin(pin)?;
    if pin_is_set()? {
        let current = current
            .filter(|c| !c.is_empty())
            .ok_or_else(|| Refusal::new("pin_wrong", "the current PIN is needed to change it"))?;
        verify_pin(current)?;
    }
    let hashed = hash_pin(pin)?;
    pin_store()?
        .set_str(PIN_ACCOUNT, &hashed)
        .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))?;
    // Recorded in the engine's own store, so deleting the PIN item later cannot reopen a reset.
    super::protected_store::mark_pin_set(pin)
}

// ---- recovery --------------------------------------------------------------------------------

/// How long a recovery challenge may be answered, in seconds (an approval signature's own limit).
const RECOVERY_VALID: u64 = 120;
/// The most recovery challenges waiting at once; the oldest goes first.
const RECOVERY_OPEN_MAX: usize = 8;

/// The recovery challenges this engine process issued and nobody answered yet: (challenge, expiry).
static RECOVERY_OPEN: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// This engine process, as a recovery signature names it (`client:engine:<id>`): a random id made
/// once per process, so a signature made for one engine run answers no other.
static ENGINE_INSTANCE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

fn random_hex(bytes: usize) -> Option<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut buffer = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buffer).ok()?;
    Some(rightkit_fs::digest::hex(&buffer))
}

fn recovery_client() -> Result<String> {
    ENGINE_INSTANCE
        .get_or_init(|| random_hex(16))
        .as_ref()
        .map(|id| format!("engine:{id}"))
        .ok_or_else(|| {
            Refusal::new(
                "recovery_unavailable",
                "this engine cannot make a recovery challenge",
            )
        })
}

/// The `action` line a recovery signature commits to: SHA-256 hex of
/// `pulse.recover.v1\nrepair:<true|false>\npin:<SHA-256 hex of the new PIN, or empty>`, so a
/// signature for a repair cannot set a PIN and a signature for one new PIN cannot set another.
pub fn recovery_action(repair: bool, new_pin: Option<&str>) -> String {
    let pin = new_pin
        .filter(|p| !p.is_empty())
        .map(|p| rightkit_fs::digest::hex(&rightkit_crypto::sha256(p.as_bytes())))
        .unwrap_or_default();
    rightkit_fs::digest::hex(&rightkit_crypto::sha256(
        format!("pulse.recover.v1\nrepair:{repair}\npin:{pin}").as_bytes(),
    ))
}

/// The engine's `remote.security.recover.challenge`: a fresh single-use challenge, valid
/// `RECOVERY_VALID` seconds, for owner app to sign (`approval_sig` v1, kind `recover`, request the
/// challenge, action `recovery_action`, client the `client` returned here). Engine only.
pub fn recovery_challenge() -> Result<Value> {
    if !approvals::is_engine_process() {
        return Err(Refusal::new(
            "approval_unavailable",
            "recovery is answered by the engine, and this process is not the engine",
        ));
    }
    let challenge = random_hex(32).ok_or_else(|| {
        Refusal::new(
            "recovery_unavailable",
            "the system could not make a random recovery challenge",
        )
    })?;
    let client = recovery_client()?;
    let now = now_secs();
    let expires = now + RECOVERY_VALID;
    let mut open = RECOVERY_OPEN.lock().unwrap_or_else(|e| e.into_inner());
    open.retain(|(_, until)| *until > now);
    if open.len() >= RECOVERY_OPEN_MAX {
        open.remove(0);
    }
    open.push((challenge.clone(), expires));
    Ok(serde_json::json!({
        "kind": "recover",
        "challenge": challenge,
        "client": client,
        "expires": expires,
    }))
}

/// Removes `challenge` from the open ones; an unknown or expired one is a refusal. A challenge is
/// used up by its first answer, right or wrong, so a signature cannot be guessed at.
fn take_challenge(challenge: &str) -> Result<()> {
    let now = now_secs();
    let mut open = RECOVERY_OPEN.lock().unwrap_or_else(|e| e.into_inner());
    open.retain(|(_, until)| *until > now);
    match open.iter().position(|(c, _)| c == challenge) {
        Some(at) => {
            open.remove(at);
            Ok(())
        }
        None => Err(Refusal::new(
            "recovery_challenge_unknown",
            "this recovery challenge is unknown, expired or already used; start the recovery again",
        )),
    }
}

/// Proof that the owner authorised one recovery (`authorise_recovery`). Only this module makes one,
/// so nothing resets the PIN without it. A signed one names the operation it was signed for.
pub struct Recovery {
    /// `(repair, recovery_action)` the signature covered; `None` for the current PIN.
    signed_for: Option<(bool, String)>,
}

/// Authenticates one `remote.security.recover` on its own, before anything changes. `from_notch`
/// (the engine verified the caller's executable) is required but proves nothing by itself. The
/// proof is either the current PIN (`current_pin`, counted like every attempt), or `sig`: a
/// signature of an enrolled, proven approver key (`approval_sig::verify`, kind `recover`) over
/// `challenge` (from `recovery_challenge`, single use), `recovery_action(repair, new_pin)` and this
/// engine process. The owner app's word that the system accepted the owner is no proof, and neither is
/// Windows Hello until it signs with an enrolled key. With neither, or when no key this engine can
/// prove made the signature (a lost PIN after a restart, a lost or replaced key), the recovery is
/// refused and nothing changes. Engine only.
pub fn authorise_recovery(
    repair: bool,
    new_pin: Option<&str>,
    current_pin: Option<&str>,
    challenge: Option<&str>,
    sig: Option<&Value>,
    from_notch: bool,
) -> Result<Recovery> {
    if !from_notch {
        return Err(Refusal::new(
            "unlock_needs_auth",
            "recovery runs in the app notch on this computer, never from another program",
        ));
    }
    if let Some(current) = current_pin.filter(|p| !p.is_empty()) {
        verify_pin(current)?;
        return Ok(Recovery { signed_for: None });
    }
    let challenge = challenge.filter(|c| !c.is_empty());
    let sig = sig.filter(|s| !s.is_null());
    let (Some(challenge), Some(sig)) = (challenge, sig) else {
        return Err(Refusal::new(
            "recovery_needs_proof",
            "recovering the app's security needs the owner's proof: the current PIN, or on a Mac the notch's Touch ID signature of this recovery; the notch's word that the system accepted you is not enough, and Windows Hello cannot sign it yet",
        ));
    };
    take_challenge(challenge)?;
    let signed = super::approval_sig::Signed::from_value(sig)?;
    let client = recovery_client()?;
    let action = recovery_action(repair, new_pin);
    super::approval_sig::verify("recover", challenge, &action, &client, &signed)
    .map_err(|refusal| match refusal.code {
        "approver_registry_unproven" | "approver_unknown" | "approver_registry_unavailable" => {
            Refusal::new(
                "recovery_unavailable",
                "no owner key this engine can trust signed this recovery (the keys count only after the PIN was entered since the app started, and a lost or replaced key proves nothing), so a lost PIN cannot be recovered here; nothing was changed",
            )
        }
        _ => refusal,
    })?;
    Ok(Recovery {
        signed_for: Some((repair, action)),
    })
}

impl Recovery {
    /// Whether this proof covers the repair of the protected settings: the current PIN does; a
    /// signature only when it was made for a repair.
    pub fn covers_repair(&self) -> bool {
        self.signed_for.as_ref().is_none_or(|(repair, _)| *repair)
    }
}

/// Sets a new PIN without the old one, for an owner who lost it, after `authorise_recovery`
/// authenticated this exact reset. The PIN's attempt count and lock are cleared. On macOS and
/// Windows the seal key was wrapped with the old PIN, so the loosened gates it sealed read as
/// closed defaults afterwards and are set again through the owner path. Engine only.
pub fn pin_reset_by_owner(pin: &str, proof: &Recovery) -> Result<()> {
    if let Some((repair, action)) = &proof.signed_for
        && *action != recovery_action(*repair, Some(pin))
    {
        return Err(Refusal::new(
            "approval_signature_invalid",
            "the owner's signature was made for another recovery",
        ));
    }
    valid_pin(pin)?;
    let hashed = hash_pin(pin)?;
    let _gate = PIN_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let store = pin_store()?;
    store
        .set_str(PIN_ACCOUNT, &hashed)
        .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))?;
    let _ = write_attempts(&store, 0, 0);
    super::protected_store::mark_pin_set(pin)
}

// ---- device-link owner check -------------------------------------------------------------------

/// How long a device-operation challenge may be answered, in seconds.
const DEVICE_VALID: u64 = 120;
/// The most device challenges waiting at once; the oldest goes first.
const DEVICE_OPEN_MAX: usize = 8;
/// The device challenges this engine process issued and nobody answered yet: (challenge, op, expiry).
static DEVICE_OPEN: Mutex<Vec<(String, String, u64)>> = Mutex::new(Vec::new());
/// The admissions the open `approve` challenges are bound to: (challenge, admission). Same size
/// limit and lifetime as `DEVICE_OPEN`; a challenge leaves both lists together.
static ADMIT_OPEN: Mutex<Vec<(String, Admission)>> = Mutex::new(Vec::new());

/// What an `approve` owner check admits (the circle, `devicelink/circle.rs`): the endorsement
/// bodies one owner-check signature covers (the pairing's new device, plus every member this
/// computer has not endorsed yet), through `approval_sig::endorse_action`. The signature becomes
/// the batch every other member verifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub bodies: Vec<rightkit_device_link_types::EndorsementBody>,
    /// The join token's grant the SAME signature covers (`join`: making a token or QR): at most
    /// one, in the same batch hash as `bodies`, which may then be empty. Empty for an `approve`.
    pub grants: Vec<rightkit_device_link_types::TokenGrant>,
    /// The pairing's new device: the one body the joiner itself must find in the batch. Empty for
    /// a `join` (no joiner exists yet).
    pub new_device_id: String,
    /// The pairing's confirmed transcript hash (64 hex). It becomes the challenge itself, so the
    /// owner-check signature also commits to this one pairing (the joiner checks it). Empty for a
    /// `join`, whose challenge is random.
    pub transcript_hash: String,
}

impl Admission {
    /// The `action` the owner-check signature commits to.
    pub fn action(&self) -> String {
        {
            let mut lines: Vec<String> = self
                .bodies
                .iter()
                .map(rightkit_device_link_types::EndorsementBody::line)
                .collect();
            lines.extend(
                self.grants
                    .iter()
                    .map(rightkit_device_link_types::TokenGrant::line),
            );
            rightkit_owner_approvals::approval_sig::endorse_action(&lines)
        }
    }

    /// The body for `device_id`, if the batch carries one.
    pub fn body(&self, device_id: &str) -> Option<&rightkit_device_link_types::EndorsementBody> {
        self.bodies.iter().find(|b| b.device_id == device_id)
    }
}

/// The device operations the owner check covers: `join` (making a join token, `device.join.create`)
/// and `approve:<pairing id, 32 hex>` (approving one pairing, `device.pair.approve`).
fn device_op_known(op: &str) -> bool {
    op == "join"
        || op
            .strip_prefix("approve:")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The `action` line a device signature commits to: SHA-256 hex of `pulse.device.v1\nop:<op>`, so a
/// signature made to make a token cannot approve a pairing and one pairing's cannot approve another.
pub fn device_action(op: &str) -> String {
    rightkit_fs::digest::hex(&rightkit_crypto::sha256(
        format!("pulse.device.v1\nop:{op}").as_bytes(),
    ))
}

/// `device.owner.challenge`: a fresh single-use challenge, valid `DEVICE_VALID` seconds, for the
/// owner app to sign (`approval_sig` v1, kind `device`, request the challenge, action `device_action`,
/// the `client` returned here) before `device.join.create` or `device.pair.approve`. Engine only;
/// it grants nothing by itself.
pub fn device_challenge(op: &str) -> Result<Value> {
    device_challenge_admit(op, None)
}

/// [`device_challenge`] for an `approve:<id>` operation that admits `admission`, or a `join` that
/// signs a token grant: the challenge's `action` is `approval_sig::endorse_action` over its bodies
/// and grant (and the answer carries `admit` with `endorse` and `grants`, their fields, for the
/// owner app to rebuild the action itself), so the owner-check signature is also the endorsement
/// batch. An `approve` admission has no grant; a `join` admission has exactly one and no joiner.
/// Any other `op` ignores `admission`.
pub fn device_challenge_admit(op: &str, admission: Option<Admission>) -> Result<Value> {
    let admission = admission.filter(|a| {
        if op.starts_with("approve:") {
            a.grants.is_empty()
        } else {
            op == "join"
                && a.grants.len() == 1
                && a.new_device_id.is_empty()
                && a.transcript_hash.is_empty()
        }
    });
    if !approvals::is_engine_process() {
        return Err(Refusal::new(
            "approval_unavailable",
            "the owner check is answered by the engine, and this process is not the engine",
        ));
    }
    if !device_op_known(op) {
        return Err(Refusal::new("invalid_params", "unknown device operation"));
    }
    // An admission's challenge IS the pairing's transcript hash: unique per session (it holds both
    // fresh nonces), and the signature's request field then binds the pairing for the joiner.
    let challenge = match &admission {
        Some(_) if op == "join" => random_hex(32).ok_or_else(|| {
            Refusal::new(
                "approval_unavailable",
                "the system could not make a random challenge",
            )
        })?,
        Some(a)
            if a.transcript_hash.len() == 64
                && a.transcript_hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            a.transcript_hash.to_ascii_lowercase()
        }
        Some(_) => return Err(Refusal::new("invalid_params", "bad pairing transcript")),
        None => random_hex(32).ok_or_else(|| {
            Refusal::new(
                "approval_unavailable",
                "the system could not make a random challenge",
            )
        })?,
    };
    let client = recovery_client()?;
    let expires = now_secs() + DEVICE_VALID;
    let now = now_secs();
    let mut open = DEVICE_OPEN.lock().unwrap_or_else(|e| e.into_inner());
    open.retain(|(c, _, until)| *until > now && *c != challenge);
    if open.len() >= DEVICE_OPEN_MAX {
        open.remove(0);
    }
    open.push((challenge.clone(), op.to_string(), expires));
    let live: Vec<String> = open.iter().map(|(c, _, _)| c.clone()).collect();
    drop(open);
    let mut admits = ADMIT_OPEN.lock().unwrap_or_else(|e| e.into_inner());
    admits.retain(|(c, _)| live.contains(c) && *c != challenge);
    let mut answer = serde_json::json!({
        "kind": "device",
        "op": op,
        "challenge": challenge,
        "action": device_action(op),
        "client": client,
        "expires": expires,
    });
    if let Some(admission) = admission {
        answer["action"] = serde_json::json!(admission.action());
        answer["admit"] = serde_json::json!({
            "endorse": admission
                .bodies
                .iter()
                .map(|b| serde_json::json!({
                    "deviceId": b.device_id,
                    "publicKey": b.public_key,
                    "checkDevice": b.member_check.as_ref().map(|c| c.device.clone()),
                    "checkKey": b.member_check.as_ref().map(|c| c.public_key.clone()),
                    "canAdmit": b.can_admit,
                    "at": b.issued_at_ms,
                }))
                .collect::<Vec<_>>(),
            "grants": admission
                .grants
                .iter()
                .map(|g| serde_json::json!({
                    "admitterDeviceId": g.admitter_device_id,
                    "pairingId": g.pairing_id,
                    "secretHash": g.secret_hash,
                    "canAdmit": g.can_admit,
                    "issuedAt": g.issued_at_ms,
                    "expiresAt": g.expires_at_ms,
                    "memberClass": g.member_class,
                    "leaseExpiresAt": g.lease_expires_at_ms,
                }))
                .collect::<Vec<_>>(),
        });
        admits.push((challenge, admission));
    }
    Ok(answer)
}

/// The owner check for one device operation, made INSIDE the engine before anything is created or
/// approved. The proof is the host app PIN (`pin`, counted like every attempt), or `sig`: a signature
/// of an enrolled, proven approver key (a Mac's Touch ID key) over `challenge` (from
/// `device_challenge`, single use, for exactly this `op`), kind `device`. A caller's word that the
/// system accepted the owner is no proof; a Windows Hello signature counts as an enrolled RSA key's. With no PIN
/// set and no proven key the check refuses (`pin_not_set`): a device is never added unprotected.
/// Engine only.
pub fn authorise_device_op(
    op: &str,
    pin: Option<&str>,
    challenge: Option<&str>,
    sig: Option<&Value>,
) -> Result<()> {
    authorise_device_op_proof(op, pin, challenge, sig).map(|_| ())
}

/// [`authorise_device_op`] that also returns the admission proof: with a signature over a challenge
/// issued by [`device_challenge_admit`], the owner-check proof and the admission it commits to;
/// `None` for a PIN or a challenge that admits nothing. Engine only.
pub fn authorise_device_op_proof(
    op: &str,
    pin: Option<&str>,
    challenge: Option<&str>,
    sig: Option<&Value>,
) -> Result<Option<(super::approval_sig::OwnerCheckProof, Admission)>> {
    if !approvals::is_engine_process() {
        return Err(Refusal::new(
            "approval_unavailable",
            "the owner check is made by the engine, and this process is not the engine",
        ));
    }
    if !device_op_known(op) {
        return Err(Refusal::new("invalid_params", "unknown device operation"));
    }
    if let Some(pin) = pin.filter(|p| !p.is_empty()) {
        return verify_pin(pin).map(|_| None);
    }
    let challenge = challenge.filter(|c| !c.is_empty());
    let sig = sig.filter(|s| !s.is_null());
    let (Some(challenge), Some(sig)) = (challenge, sig) else {
        return Err(Refusal::new(
            "unlock_needs_auth",
            "adding or approving a device needs you: enter your PIN, or confirm with Touch ID (Mac) or Windows Hello",
        ));
    };
    // A challenge is used up by its first answer, right or wrong, and only answers its own op.
    {
        let now = now_secs();
        let mut open = DEVICE_OPEN.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|(_, _, until)| *until > now);
        let Some(at) = open.iter().position(|(c, o, _)| c == challenge && o == op) else {
            return Err(Refusal::new(
                "device_challenge_unknown",
                "this owner check is unknown, expired, already used or for another operation; try again",
            ));
        };
        open.remove(at);
    }
    let admission = {
        let mut admits = ADMIT_OPEN.lock().unwrap_or_else(|e| e.into_inner());
        admits
            .iter()
            .position(|(c, _)| c == challenge)
            .map(|at| admits.remove(at).1)
    };
    let action = match &admission {
        Some(a) => a.action(),
        None => device_action(op),
    };
    let signed = super::approval_sig::Signed::from_value(sig)?;
    let client = recovery_client()?;
    // The owner-check keys live in their own sealed file, not the PIN-sealed registry, so this
    // needs no PIN after a restart.
    super::approval_sig::verify_owner_check_proof(challenge, &action, &client, &signed)
        .map(|proof| admission.map(|a| (proof, a)))
        .map_err(|refusal| match refusal.code {
            "approver_unknown" => Refusal::new(
                "unlock_needs_auth",
                "no enrolled owner-check key made this signature; confirm with Touch ID again or enter your PIN",
            ),
            _ => refusal,
        })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The engine's `remote.unlock.decide`, without a signature: the pre-signature entry point, kept so
/// the engine contract keeps compiling. It is `decide_signed` with `sig: None`, so only the PIN
/// approves; the engine contract passes `p.get("sig")` to `decide_signed`.
pub fn decide(
    id: &str,
    approve: bool,
    digest: &str,
    method: Option<&str>,
    pin: Option<&str>,
) -> Result<bool> {
    decide_signed(id, approve, digest, method, pin, None)
}

/// The engine's `remote.unlock.decide`: the owner's answer to a waiting unlock request. Denying
/// needs no proof. Approving needs `method`: `"sig"` (the owner's enrolled, biometric-gated key
/// signed this request: `approval_sig`, verified here against the engine's own record of the
/// request) or `"pin"` with the PIN, which is verified here. `"os"` (owner app's word that Touch
/// ID or Windows Hello accepted the owner) is refused on every platform: a caller's word is not an
/// approval, so Windows uses the PIN until Hello signs with an enrolled key. `Ok(false)`: no such
/// waiting request. Engine only.
pub fn decide_signed(
    id: &str,
    approve: bool,
    digest: &str,
    method: Option<&str>,
    pin: Option<&str>,
    sig: Option<&Value>,
) -> Result<bool> {
    if !approve {
        return approvals::decide_unlock(id, false, digest, 0);
    }
    if !approvals::is_unlock_request(id) {
        return Ok(false);
    }
    let mut signed_unlock = false;
    match method {
        Some("sig") => {
            let signed = super::approval_sig::Signed::from_value(sig.unwrap_or(&Value::Null))?;
            let client = approvals::unlock_request_client(id).unwrap_or_default();
            // The owner check: the enrolled owner-check key (Touch ID or password on the Mac,
            // Windows Hello on Windows) signs, no PIN. Any read or parse error of the owner-check
            // keys refuses. The PIN-sealed registry is used only when no owner-check key is
            // enrolled at all, and no token is stored on that path.
            match super::approval_sig::verify_owner_check_kind(
                "unlock", id, digest, &client, &signed,
            ) {
                Ok(()) => signed_unlock = true,
                Err(r) if r.code == "approver_unknown" => {
                    if super::approval_sig::owner_check_enrolled()? {
                        return Err(r);
                    }
                    super::approval_sig::verify("unlock", id, digest, &client, &signed)?;
                }
                Err(r) => return Err(r),
            }
        }
        Some("os") => {
            return Err(Refusal::new(
                "unlock_needs_auth",
                "an unlock needs the owner's signed approval (Touch ID in the notch on a Mac) or the PIN; the notch's word that the system accepted the owner is not enough, and on Windows Hello cannot sign yet, so use the PIN",
            ));
        }
        Some("pin") => verify_pin(pin.unwrap_or(""))?,
        _ => {
            return Err(Refusal::new(
                "unlock_needs_auth",
                "an unlock needs method sig (from the notch) or pin",
            ));
        }
    }
    let minutes = Settings::load().chat_unlock_minutes;
    let expires = if minutes != 0 {
        now_secs() + minutes * 60
    } else if approvals::unlock_request_client(id)
        .and_then(|client| client.strip_prefix("chat:").map(str::to_string))
        .and_then(|chat| chat_info(&chat))
        .is_some_and(|chat| chat.alive)
    {
        // A chat the roster calls live: until it ends, checked on use.
        0
    } else {
        // A registered chat or one of unknown liveness cannot be seen ending, so "until the chat
        // ends" would ask again on every call. It gets a fixed minimum instead.
        now_secs() + MIN_UNLOCK_SECONDS
    };
    let client = approvals::unlock_request_client(id).unwrap_or_default();
    let decided = approvals::decide_unlock(id, true, digest, expires)?;
    if decided && signed_unlock {
        // The owner check passed: keep one token for this chat in the OS credential store, with an
        // expiry, so the unlock survives an engine restart. The in-memory grant stands without it.
        let cap = now_secs() + TOKEN_MAX_SECONDS;
        let (token_expires, until_end) = if expires != 0 {
            (expires.min(cap), false)
        } else {
            (cap, true)
        };
        let _ = store_token(&client, token_expires, until_end);
    }
    Ok(decided)
}

// ---- the stored unlock token ----------------------------------------------------------------

/// The longest a stored token lives, also the bound of an "until the chat ends" unlock across an
/// engine restart.
const TOKEN_MAX_SECONDS: u64 = 24 * 60 * 60;
const TOKEN_GEN_ACCOUNT: &str = "token-gen-v1";

fn token_account(client: &str) -> String {
    let digest = approvals::bind_of(&[client.as_bytes()]);
    format!("token-{}", &digest[..32])
}

/// The stored generation; `None` when the item is missing (unknown: every token is refused).
fn token_generation(store: &rightkit_credentials::Store) -> Result<Option<u64>> {
    match store.get(TOKEN_GEN_ACCOUNT) {
        Ok(Some(secret)) => secret
            .expose_str()
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .map(Some)
            .ok_or_else(|| {
                Refusal::new("pin_unavailable", "the unlock token generation is damaged")
            }),
        Ok(None) => Ok(None),
        Err(e) => Err(Refusal::new("pin_unavailable", e.to_string())),
    }
}

const TOKEN_INDEX_ACCOUNT: &str = "token-index-v1";

fn token_index(store: &rightkit_credentials::Store) -> Result<Vec<String>> {
    match store.get(TOKEN_INDEX_ACCOUNT) {
        Ok(Some(secret)) => Ok(secret
            .expose_str()
            .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))?
            .lines()
            .filter(|l| l.starts_with("chat:"))
            .map(str::to_string)
            .collect()),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(Refusal::new("pin_unavailable", e.to_string())),
    }
}

fn write_token_index(store: &rightkit_credentials::Store, clients: &[String]) -> Result<()> {
    store
        .set_str(TOKEN_INDEX_ACCOUNT, &clients.join("\n"))
        .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))
}

fn token_text(client: &str, until_end: bool, expires: u64, generation: u64, nonce: &str) -> String {
    format!(
        "pulse.unlock.token.v1|{client}|{}|{expires}|{generation}|{nonce}",
        u8::from(until_end)
    )
}

/// Stores the unlock token of `client` (`chat:<id>`): a random nonce sealed by the owner-check
/// secret together with the chat, the expiry and the generation. Engine only; the unlock path calls
/// it after a verified owner check.
fn store_token(client: &str, expires: u64, until_end: bool) -> Result<()> {
    use ring::rand::{SecureRandom, SystemRandom};
    if !approvals::is_engine_process() || !client.starts_with("chat:") {
        return Err(Refusal::new("unlock_needs_auth", "not stored"));
    }
    let store = pin_store()?;
    let generation = match token_generation(&store)? {
        Some(g) => g,
        None => {
            store
                .set_str(TOKEN_GEN_ACCOUNT, &now_ms().to_string())
                .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))?;
            now_ms()
        }
    };
    let mut index = token_index(&store)?;
    if !index.iter().any(|c| c == client) {
        index.push(client.to_string());
        write_token_index(&store, &index)?;
    }
    let mut raw = [0u8; 32];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| Refusal::new("pin_unavailable", "no random nonce for the unlock token"))?;
    let nonce = rightkit_fs::digest::hex(&raw);
    let text = token_text(client, until_end, expires, generation, &nonce);
    let mac = super::approval_sig::owner_check_seal(&text)?;
    store
        .set_str(
            &token_account(client),
            &format!(
                "v1${}${}${expires}${generation}${nonce}${mac}",
                client,
                u8::from(until_end)
            ),
        )
        .map_err(|e| Refusal::new("pin_unavailable", e.to_string()))
}

/// The expiry (Unix seconds) and whether it is an "until the chat ends" token, of a valid stored token for `client`, or `None`. Valid: the engine
/// reads it, the seal verifies under the owner-check secret, the chat and generation match, it has
/// not expired, and an "until the chat ends" token needs the chat still live. An invalid or expired
/// item is deleted. Engine only.
pub(super) fn restore_token(client: &str) -> Option<(u64, bool)> {
    if !approvals::is_engine_process() || !client.starts_with("chat:") {
        return None;
    }
    let store = pin_store().ok()?;
    let account = token_account(client);
    let secret = store.get(&account).ok()??;
    let text = secret.expose_str().ok()?.to_owned();
    let parts: Vec<&str> = text.split('$').collect();
    let valid = (|| {
        let [version, chat, until_end, expires, generation, nonce, mac] = parts[..] else {
            return None;
        };
        let (expires, generation) = (
            expires.parse::<u64>().ok()?,
            generation.parse::<u64>().ok()?,
        );
        let until_end = match until_end {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        let sealed = token_text(chat, until_end, expires, generation, nonce);
        (version == "v1"
            && chat == client
            && expires > now_secs()
            && expires <= now_secs() + TOKEN_MAX_SECONDS
            && token_generation(&store).ok()?? == generation
            && super::approval_sig::owner_check_seal_ok(&sealed, mac)
            && (!until_end || chat_alive(client)))
        .then_some((expires, until_end))
    })();
    if valid.is_none() {
        let _ = store.delete(&account);
    }
    valid
}

/// Deletes the stored token of `client`. Engine only.
pub(super) fn forget_token(client: &str) {
    if approvals::is_engine_process()
        && let Ok(store) = pin_store()
    {
        let _ = store.delete(&token_account(client));
    }
}

/// Voids every stored token: the generation moves on first (so no sealed token matches again, even
/// if a delete fails), then every indexed token is deleted. Any failure is reported. Engine only.
pub(super) fn forget_all_tokens() -> Result<()> {
    if !approvals::is_engine_process() {
        return Err(Refusal::new("approval_unavailable", "not the engine"));
    }
    let store = pin_store()?;
    let failed = |e: String| {
        Refusal::new(
            "pin_unavailable",
            format!("unlock tokens not all removed: {e}"),
        )
    };
    let next = token_generation(&store)?.map_or(now_ms(), |g| (g + 1).max(now_ms()));
    store
        .set_str(TOKEN_GEN_ACCOUNT, &next.to_string())
        .map_err(|e| failed(e.to_string()))?;
    let mut left = Vec::new();
    let mut first_error = None;
    for client in token_index(&store)? {
        if let Err(e) = store.delete(&token_account(&client)) {
            first_error.get_or_insert(e.to_string());
            left.push(client);
        }
    }
    write_token_index(&store, &left)?;
    match first_error {
        Some(e) => Err(failed(e)),
        None => Ok(()),
    }
}
