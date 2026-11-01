//! Remote tools, tier 1: read-only access to this computer for a caller that is not the user's
//! own terminal (Claude Code and Codex through the host app plugin today, a ChatGPT gateway later).
//! Every call acts on THIS machine only. Design: the workspace's
//! `docs/architecture/2026-10-15-pulse-central/remote-access.md`.
//!
//! * `settings`: the switches and caps, read from owner app's published `owner app-state.json`
//!   (`remoteTools`, `remoteRoots`, `remoteChatRead`, the five caps). Anything missing or
//!   unreadable means off.
//! * `manifest`: `core/remote-tools.json`, included at build time. A tool not in it, or listed as
//!   not built, is refused.
//! * `confine`: the approved-folder boundary. A path is canonicalized with every link resolved,
//!   must sit inside a canonicalized approved folder by path components, and the file that was
//!   actually opened is checked again.
//! * `files`: `files.info`, `files.read`, `files.find`, `files.image` (a PNG, JPEG or GIF as it is,
//!   base64, never decoded or shrunk).
//! * `docs`: `docs.read` (tier 2, needs `remoteDocs` too): the text of a PDF, DOCX or XLSX by
//!   page; the gateway offers it as `docs_read`, with the text redacted.
//! * `docs_write`: `docs.write` (tier 2, needs `remoteDocs` and `remoteFilesWrite`): create a DOCX
//!   or XLSX, or edit a DOCX, XLSX or PDF, by `rightkit-docs` typed operations, one approval per
//!   call; the gateway offers it as `docs_write`.
//! * `files_write`: `files.patch` (one file, or several under one approval) and `files.manage`
//!   (tier 2, `remoteFilesWrite`, one approval per
//!   act, verified writes, Trash or Recycle Bin for removal); `term`: `term.exec|write|read|stop` and `procs`:
//!   `procs.list|stop` (tier 2, `remoteTerminals`, a session grant or the standing policy, scrubbed
//!   environment, 30-minute idle expiry, PID plus start-time verified stops); `approvals`: the local
//!   grants they share.
//! * `term` also has `tty: true` sessions on a pseudo-terminal, `cmds` batches, `term.list`,
//!   `term.resize`, ready-for-input detection and the user's command policy (`remoteTermAllow`,
//!   `remoteTermDeny`); `chat_term` (`chat_term_read`, `chat_term_write`): a chat's live terminal,
//!   tmux panes only; `clipboard`: `clipboard.read`, one approval per read (`remoteClipboard`);
//!   `chat_start::stop`: `chat_stop`; `remoteReadOnly` refuses every gateway tool in
//!   `changes_things`. These are gateway-only except the `term` ones, which the engine does not list.
//! * `screen`: `screen.observe` (tier 2, `remoteScreenObserve`) and `screen.act` (tier 3,
//!   `remoteScreenAct`), through `rightkit-automation`: any app's windows except the blocked ones (`remoteBlockedApps`; the host app, System
//!   Settings and security prompts always), the disclosure grant (`remoteScreenDisclosure`), the OS
//!   permission checked and never asked for, and for `screen.act` a local approval only where the
//!   door asks: the gateway per action, the local agent and the owner for submit, buy, send, publish
//!   and delete controls (a password, card or ID is never typed).
//! * `apps`: `apps.launch` (tier 2, `remoteLaunch`): opens any app except the blocked ones
//!   (`remoteBlockedApps`); the local door asks nothing, the gateway one approval per app and then a
//!   standing grant. `media`: `media.control`
//!   (tier 2, `remoteMedia`): play, pause, next, previous and search-and-play in Spotify and Music
//!   (Apple Events on macOS with fixed scripts, SMTC on Windows); no per-call approval.
//! * `chat`: `chat_read` (transcript turns of a selected chat) and `chat_peers` (the selected
//!   chats on this computer), with `redact`.
//! * `gptchat`, `chat_start`, `files_fetch`: what is offered to ChatGPT through the gateway only
//!   (a call from any other door is refused): `chat_peers` (every chat that can be messaged),
//!   `chat_send`, `chat_reply` and `chat_inbox` (ChatGPT is one more chat, stamped as unverified),
//!   `chat_start` (a Codex or Claude chat started in an approved folder, one local approval per
//!   start) and `files.fetch` (save a file ChatGPT hands over, from pinned OpenAI hosts, into an
//!   approved folder, exclusive create, verified).
//! * `unlock`: the per-chat unlock (`chatUnlock`, `chatUnlockMinutes`): an external chat is unlocked
//!   once, with Touch ID (Mac) or the host app PIN (Windows Hello is not a proof until it signs),
//!   before `screen.act`, `apps.launch`, `media.control`, `files.patch|manage`, `term.*` and
//!   `procs.stop` run for it; an agent started inside the host app acts with its submitter's authority (the
//!   owner's owner app or hub: exempt; a chat: that chat's unlock) and the ChatGPT gateway keeps its
//!   per-action approvals. Session grants in `approvals` belong to one client.
//! * `agent_secrets`: `login.fill` and `payment.fill` (tier 3, `remoteFormFillLogins`,
//!   `remoteFormFillPayments`, local door only): the host app types one saved login or virtual-card field
//!   into the user's browser after checking the page's real address and asking the owner
//!   (Touch ID or the PIN); the agent never sees a value. Also the owner-only secure box
//!   (`remote.secure.put|list|remove`) and the monthly spend ledger.
//! * `audit`: one line per call in `remote-activity.jsonl`, which the hub's Activity page shows.
//!
//! `execute` is the one entry the CLI, the engine and the ChatGPT gateway
//! (`the gateway`) call, so they behave the same. A call from the gateway carries
//! `Origin::Gateway` and is refused unless `remoteGateway` is on as well as `remoteTools`.
//! Refusals are a `Refusal` with a stable `code`.

use serde_json::{Value, json};

#[cfg(all(feature = "test-overrides", not(debug_assertions)))]
compile_error!(
    "the `test-overrides` feature must never be enabled in a release build (it relaxes the protected-settings keychain check and trusts dev approver keys)"
);

pub mod agent_secrets;
pub mod approvals;
pub mod apps;
pub mod audit;
mod b64;
// The page address of the front browser on Windows (UI Automation), read by `agent_secrets`.
#[cfg(windows)]
mod browser_address;
pub mod chat;
pub mod chat_start;
pub mod chat_term;
pub mod clipboard;
pub mod config;
pub mod confine;
pub mod docs;
pub mod docs_write;
pub mod files;
pub mod files_fetch;
pub mod files_write;
pub mod gptchat;
pub mod manifest;
pub mod media;
pub mod owner_settings;
pub mod package;
pub mod procs;
pub mod protected_store;
pub mod screen;
#[cfg(any(target_os = "macos", windows))]
mod sensitive;
pub mod settings;
pub mod term;
pub mod unlock;

pub use unlock::Requester;

pub use rightkit_approvals::{Refusal, Result, approval_sig, protected_guard, redact};

/// What `rightkit-approvals` asks of this crate: the PIN, the unlock sessions and whether this
/// process is the engine.
struct ApprovalBridge;

impl rightkit_approvals::ApprovalHooks for ApprovalBridge {
    fn pin_is_set(&self) -> Result<bool> {
        unlock::pin_is_set()
    }
    fn confirm_pin(&self, pin: &str) -> Result<()> {
        unlock::confirm_pin(pin)
    }
    fn void_unlocks(&self) -> Result<()> {
        approvals::revoke_sessions_held(approvals::UNLOCK_FAMILY).map(|_| ())
    }
    fn is_engine_process(&self) -> bool {
        approvals::is_engine_process()
    }
}

/// Connects the gateway to `rightkit-approvals`. Call it once at start, before any approval is
/// asked for; `execute` also calls it, so a process that only executes calls needs nothing else.
/// Returns `false` when something else already installed approval hooks.
pub fn init() -> bool {
    rightkit_approvals::set_hooks(Box::new(ApprovalBridge))
}

/// The client name the ChatGPT gateway gives every call. The tunnel's key is never a requester
/// identity: the gateway is one local principal, and Activity shows it under this name. The
/// name is a label the caller supplies (the engine takes `client` from its own callers), so a
/// reader that must tell the gateway's calls apart goes by `origin`, never by `client`.
pub const GATEWAY_CLIENT: &str = "chatgpt-gateway";

/// Which door a call came in by. Set by the process that makes the call, never by the caller's
/// own request, so a request cannot claim to be local.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    /// The CLI or the engine, run by the user on this computer.
    #[default]
    Local,
    /// `the gateway`, serving ChatGPT through OpenAI's tunnel. It needs `remoteGateway`.
    Gateway,
}

impl Origin {
    /// The word Activity records.
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Local => "local",
            Origin::Gateway => "gateway",
        }
    }
}

/// One remote call, as the CLI, the engine or the gateway received it.
#[derive(Clone, Debug, Default)]
pub struct Call {
    /// Who asked: `cli`, `engine`, `chatgpt-gateway`, or a name the caller gave (letters,
    /// digits, `.`, `_`, `-`).
    pub client: String,
    /// The door the call came in by; `Local` unless the gateway made it.
    pub origin: Origin,
    /// The manifest tool name: `files.info`, `files.read`, `files.find`, `files.image`,
    /// `docs.read`, `chat_read`, `chat_peers`, and for structured arguments `files.patch`,
    /// `term.exec`, `chat_send`, `chat_start`, `files.fetch` and the rest of the manifest.
    pub tool: String,
    pub path: Option<String>,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
    pub query: Option<String>,
    pub root: Option<String>,
    pub ext: Vec<String>,
    pub chat: Option<String>,
    pub max_pixel: Option<u64>,
    /// `docs.read`: the pages wanted, `N` or `A-B`.
    pub pages: Option<String>,
    /// `docs.read`: the most characters of text to return.
    pub max_chars: Option<u64>,
    /// Who asks on the local door: an external chat by its session id, or the agent the engine
    /// started. Set by the process that makes the call (`unlock::Requester::from_params` for a
    /// request, `Agent` and `AgentFor` only inside the engine, from its sealed stamp of the task's
    /// submitter), never taken as a claim of being the agent. The
    /// tools that change this computer need an unlocked chat (`unlock`).
    pub requester: Requester,
    /// The process that made the call, as the engine's socket reported it; `None` when the door
    /// cannot tell. The unlock checks it descends from the chat it names (`unlock.rs`).
    pub caller_pid: Option<u32>,
}

/// Runs one call: manifest check, settings, the tool, then the Activity record. The record is
/// best-effort: a call whose record cannot be written still answers.
pub fn execute(call: &Call) -> Result<Value> {
    execute_args(call, &Value::Null)
}

/// `execute` for the tools that take structured arguments (`files.patch`, `files.manage`,
/// `term.*`, `procs.*`): `args` is the JSON object the caller sent, camelCase, read by the tool
/// itself (see `files_write`, `term`, `procs`). The older tools ignore it. The tools that change
/// this computer need their own switch and a local approval (`approvals`); a refusal that asks for
/// approval has the code `approval_required`.
pub fn execute_args(call: &Call, args: &Value) -> Result<Value> {
    init();
    let result = run(call, args);
    audit::record_args(call, args, &result);
    result
}

fn run(call: &Call, args: &Value) -> Result<Value> {
    manifest::require(&call.tool)?;
    let settings = settings::Settings::load();
    if call.origin == Origin::Gateway {
        settings.require_gateway()?;
        if settings.read_only && changes_things(&call.tool) {
            return Err(Refusal::new(
                "read_only_tool",
                "the ChatGPT gateway is read-only: the owner turned on remoteReadOnly in the app's settings",
            ));
        }
    }
    // Session grants belong to the client of this call, and an effectful tool on the local door
    // needs its chat unlocked first (`unlock`).
    let _client = approvals::set_client(&unlock::client_key(call));
    unlock::require(&settings, call)?;
    match call.tool.as_str() {
        "files.info" => files::info(
            &settings,
            need_path(call)?,
            args,
            call.origin == Origin::Gateway,
        ),
        "files.read"
            if !args["paths"].is_null()
                || !args["startLine"].is_null()
                || !args["endLine"].is_null() =>
        {
            files::read_more(
                &settings,
                call.path.as_deref(),
                args,
                call.offset,
                call.limit,
                call.origin == Origin::Gateway,
            )
        }
        "files.read" => files::read(
            &settings,
            need_path(call)?,
            call.offset,
            call.limit,
            call.origin == Origin::Gateway,
        ),
        "files.find" => files::find(
            &settings,
            call.query.as_deref().unwrap_or_default(),
            call.root.as_deref(),
            &call.ext,
            call.limit,
            args,
            call.origin == Origin::Gateway,
        ),
        "files.image" => files::image(
            &settings,
            need_path(call)?,
            call.max_pixel,
            call.origin == Origin::Gateway,
        ),
        "docs.read" => {
            // Tier 2: it needs `remoteDocs` as well. The gateway lists it only when the manifest
            // row names it (`docs_read`); what it returns passes the secret redactor like a file's
            // text, since it goes to OpenAI.
            let mut answer = docs::read(
                &settings,
                need_path(call)?,
                call.pages.as_deref(),
                call.max_chars,
            )?;
            // Redacted on every door, as files.read is (#578 D5), not only for the gateway.
            {
                let mut masked = 0usize;
                if let Some(pages) = answer["pages"].as_array_mut() {
                    for page in pages {
                        if let Some(text) = page["text"].as_str() {
                            let (clean, count) = redact::redact(text);
                            masked += count;
                            page["text"] = Value::String(clean);
                        }
                    }
                }
                answer["redactions"] = json!(masked);
            }
            Ok(answer)
        }
        "docs.write" => docs_write::write(&settings, call.origin, args),
        "files.patch" => files_write::patch(&settings, call.origin, args),
        "files.manage" => files_write::manage(&settings, call.origin, args),
        "term.exec" => term::exec(&settings, call.origin, args),
        "term.write" => term::write(&settings, call.origin, args),
        "term.read" => term::read(&settings, call.origin, args),
        "term.stop" => term::stop(&settings, call.origin, args),
        "term.list" => term::list(&settings, call.origin, args),
        "term.resize" => term::resize(&settings, call.origin, args),
        "chat_term_read" => chat_term::read(&settings, call.origin, args),
        "chat_term_write" => chat_term::write(&settings, call.origin, args),
        "clipboard.read" => clipboard::read(&settings, call.origin, args),
        "chat_stop" => chat_start::stop(&settings, call.origin, args),
        "screen.observe" => screen::observe(&settings, call.origin, args),
        "screen.act" => screen::act(&settings, call.origin, args),
        "apps.launch" => apps::launch(&settings, call.origin, args),
        "media.control" => media::control(&settings, call.origin, args),
        "login.fill" => agent_secrets::login_fill(&settings, call.origin, args),
        "payment.fill" => agent_secrets::payment_fill(&settings, call.origin, args),
        "procs.list" => procs::list(&settings, call.origin, args),
        "procs.stop" => procs::stop(&settings, call.origin, args),
        // The gateway lists every chat that can be messaged; the local door keeps the selected
        // chats only.
        "chat_peers" if call.origin == Origin::Gateway => gptchat::peers(&settings, call.origin),
        "chat_peers" => chat::peers(&settings),
        "chat_send" => gptchat::send(&settings, call.origin, args),
        "chat_reply" => gptchat::reply(&settings, call.origin, args),
        "chat_inbox" => gptchat::inbox(&settings, call.origin, args),
        "chat_start" => chat_start::start(&settings, call.origin, args),
        "files.fetch" => files_fetch::fetch(&settings, call.origin, args),
        "chat_read" => chat::read(
            &settings,
            call.chat.as_deref().unwrap_or_default(),
            call.limit,
        ),
        other => Err(Refusal::new(
            "not_built",
            format!("{other} is not built into this the app"),
        )),
    }
}

/// The tools that change this computer, run a command, send a message, start or stop a chat or act
/// on the screen: what the master read-only switch (`remoteReadOnly`) refuses through the gateway.
/// `term.stop` and `chat_stop` are not here: they only end work that is already running.
fn changes_things(tool: &str) -> bool {
    matches!(
        tool,
        "files.patch"
            | "files.manage"
            | "docs.write"
            | "files.fetch"
            | "term.exec"
            | "term.write"
            | "term.resize"
            | "procs.stop"
            | "screen.act"
            | "apps.launch"
            | "media.control"
            | "chat_send"
            | "chat_reply"
            | "chat_start"
            | "chat_term_write"
    )
}

fn need_path(call: &Call) -> Result<&str> {
    call.path
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Refusal::new("invalid_request", "a path is needed"))
}
