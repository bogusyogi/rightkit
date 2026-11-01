//! Which chats can be messaged: live Claude chats read from
//! `~/.claude/sessions/*.json`, registered chats (and Claude without a session
//! file), the most recent Codex threads from
//! the Codex state database (`deliver_codex`), the recent Muse sessions from Muse's own
//! index (`muse`, while the setting `muse` is on), and the chats linked computers list over
//! ssh. A peer is shown as `"<chat title> on <device>"`.
//!
//! App chats (Claude Desktop, the IDE extension, Codex Desktop) are listed while they
//! live. Terminal chats (an interactive `claude` CLI session, an interactive Codex CLI
//! thread) are listed too, but only while active: activity within `TERMINAL_CHAT_WINDOW`
//! (6 hours), and only while the setting `terminalChats` (default on) is on. The terminal
//! leaves session files behind for days; an older one is not a chat anyone is in.

use super::ChatError;
use super::deliver_claude::{start_identity, text_of};
use super::deliver_codex::{
    ChatState, CodexThread, DiscoveryError, chat_state_of, find_thread_by_id, list_threads,
    prime_rollout_paths, terminal_session_threads,
};
use super::deliver_hook::{self, HookState};
use super::harness;
use super::links::RemoteChats;
use super::muse::{self, MuseSession};
use super::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One chat as a linked computer lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterEntry {
    pub session: String,
    pub name: String,
    /// "claude", "codex" or "muse".
    pub kind: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub status: String,
    /// Milliseconds since the epoch of the chat's last activity, when known.
    #[serde(default)]
    pub updated_ms: Option<u64>,
    /// "live" (process identity verified), "stale" or "unknown" (no way to tell).
    #[serde(default = "unknown_liveness")]
    pub liveness: String,
    /// What a message does to this chat while it is idle (`wake_word`): "wake", "queue" or "".
    #[serde(default)]
    pub wake: String,
    /// The Claude account the chat belongs to, by the host app's name for it; "" when this
    /// computer has chats of one account only. Shown beside the name, never part of it.
    #[serde(default)]
    pub account: String,
}

/// The liveness of a chat on a linked computer that could not be asked now: its row is
/// the last listing that computer answered with (see `RemoteChats::cached`).
pub const LIVENESS_UNREACHABLE: &str = "unreachable";

fn unknown_liveness() -> String {
    "unknown".to_string()
}

/// A chat on this computer, with what a delivery needs to reach it.
#[derive(Debug, Clone, Serialize)]
pub struct LocalSession {
    pub id: String,
    /// "claude", "codex" or "muse".
    pub kind: String,
    pub name: String,
    pub cwd: String,
    pub status: String,
    pub updated_ms: Option<u64>,
    /// "live" (process and its start time verified), "stale" or "unknown".
    pub liveness: String,
    pub pid: Option<u32>,
    /// Claude's per-chat inbox socket (named pipe on Windows), when it has one.
    pub messaging_socket: Option<String>,
    pub peer_protocol: Option<u64>,
    pub entrypoint: Option<String>,
    /// The whole session file (or registration) as read.
    pub raw: Value,
}

/// The three idle-wake switches (`Store::codex_wake_idle`, `claude_wake_idle`, `muse_wake_idle`),
/// read once for a whole listing. A store that cannot be opened reads as every switch off.
#[derive(Debug, Clone, Copy, Default)]
pub struct WakeSwitches {
    pub codex: bool,
    pub claude: bool,
    pub muse: bool,
}

impl WakeSwitches {
    pub fn current() -> WakeSwitches {
        Store::open_default()
            .map(|store| WakeSwitches {
                codex: store.codex_wake_idle(),
                claude: store.claude_wake_idle(),
                muse: store.muse_wake_idle(),
            })
            .unwrap_or_default()
    }
}

/// What a message does to `session` while the chat is idle, per harness: "wake" when the host app starts
/// a turn in it (its switch is on and this the host app has a way to), "queue" when the message waits
/// for the chat's next prompt or tool call (the switch is off, the chat is in a Claude profile
/// The host app cannot open, or the harness has no supported wake entry: Muse, Claude B), "" for a chat
/// that is not idle. It is the roster's promise, not a receipt: delivery still looks again, and
/// never wakes a chat its owner stopped or an app may hold.
pub fn wake_word(session: &LocalSession, switches: &WakeSwitches) -> &'static str {
    let wakes = |on: bool| if on { "wake" } else { "queue" };
    match session.kind.as_str() {
        "claude" if session.raw["desktopIdle"].as_bool() == Some(true) => {
            wakes(switches.claude && session.raw["desktopWakeable"].as_bool() == Some(true))
        }
        "codex" if session.status == "idle" && session.liveness == "stale" => wakes(switches.codex),
        muse::KIND if session.status == "idle" && session.liveness == "stale" => {
            wakes(switches.muse && muse::muse_wake_command().is_some())
        }
        _ => "",
    }
}

impl LocalSession {
    pub fn entry(&self) -> RosterEntry {
        self.entry_with(&WakeSwitches::current())
    }

    /// `entry` with the wake switches already read.
    pub fn entry_with(&self, switches: &WakeSwitches) -> RosterEntry {
        RosterEntry {
            session: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            cwd: self.cwd.clone(),
            status: self.status.clone(),
            updated_ms: self.updated_ms,
            liveness: self.liveness.clone(),
            wake: wake_word(self, switches).to_string(),
            account: self.account(),
        }
    }

    /// The account label `claude_sessions_all` and `add_idle_desktop` recorded in `raw`.
    pub fn account(&self) -> String {
        self.raw["pulseAccount"].as_str().unwrap_or("").to_string()
    }
}

/// A chat that can be messaged, here or on a linked computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Peer {
    /// What `<cli> chat send` accepts: the session id here, `<device>:<session>` elsewhere.
    pub id: String,
    pub session: String,
    pub name: String,
    /// `"<chat title> on <device>"`.
    pub display: String,
    /// The computer the chat runs on: this one's alias, or a link's device name.
    pub device: String,
    pub device_alias: String,
    pub local: bool,
    pub kind: String,
    pub cwd: String,
    pub status: String,
    pub updated_ms: Option<u64>,
    /// "live", "stale", "unknown" or "unreachable" (a linked computer that could not be
    /// asked now; the row is its last listing); for a Codex chat see `codex_session`.
    pub liveness: String,
    /// When the row was last seen, for an "unreachable" row (ms since the epoch).
    pub last_seen_ms: Option<u64>,
    /// What a message does to this chat while it is idle: "wake", "queue" or "" (`wake_word`).
    pub wake: String,
    /// The Claude account label (see `RosterEntry::account`).
    pub account: String,
}

/// How recently a terminal chat must have been active to be listed: 6 hours. A documented
/// constant, not a setting (docs/settings.md, "List terminal chats active in the last 6
/// hours" is the switch).
pub const TERMINAL_CHAT_WINDOW: Duration = Duration::from_secs(6 * 3600);

/// Whether `updated_ms` (milliseconds since the epoch of a chat's last activity) is within
/// `TERMINAL_CHAT_WINDOW` of `now_ms`. A stamp more than a minute ahead of the clock is not
/// believable and is not recent.
pub(super) fn within_terminal_window(updated_ms: u64, now_ms: u64) -> bool {
    let window = u64::try_from(TERMINAL_CHAT_WINDOW.as_millis()).unwrap_or(u64::MAX);
    updated_ms <= now_ms.saturating_add(60_000) && now_ms.saturating_sub(updated_ms) <= window
}

/// `~/.claude/sessions` (or `$CLAUDE_CONFIG_DIR/sessions`).
pub fn claude_sessions_dir() -> Option<PathBuf> {
    Some(super::deliver_claude::sessions_dir())
}

/// A check for whether a process id is running. Each id the list asks about is checked on its
/// own and remembered for this list: enumerating every process on the machine (sysinfo) was a
/// third of the Dell engine's idle CPU, run every few seconds for a few dozen ids.
pub fn live_pids() -> impl Fn(u32) -> bool {
    let seen = std::cell::RefCell::new(std::collections::HashMap::<u32, bool>::new());
    move |pid| {
        *seen
            .borrow_mut()
            .entry(pid)
            .or_insert_with(|| pid_running(pid))
    }
}

#[cfg(unix)]
fn pid_running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn pid_running(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: the handle is checked, used for one query and closed.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let running =
            GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(handle);
        running
    }
}

fn title_for(name: &str, cwd: &str, fallback: &str) -> String {
    let name = name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// `start_identity`, remembered for a minute for the same pid, recorded start and domain.
///
/// On a Mac the answer comes from running `ps`, and the hub lists the chats every few seconds:
/// one process start per Claude chat per listing. A pid with the same recorded start cannot
/// change its answer while that process lives; the minute bounds the case where it died and
/// the pid went to another process. Delivery asks afresh and never comes through here.
fn listed_identity(pid: u32, recorded: Option<&str>, domain: Option<&str>) -> Option<bool> {
    struct Known {
        pid: u32,
        recorded: Option<String>,
        domain: Option<String>,
        at: Instant,
        answer: Option<bool>,
    }
    static KNOWN: Mutex<Vec<Known>> = Mutex::new(Vec::new());
    let same = |k: &Known| {
        k.pid == pid && k.recorded.as_deref() == recorded && k.domain.as_deref() == domain
    };
    let fresh = Duration::from_secs(60);
    {
        let known = KNOWN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(k) = known.iter().find(|k| same(k) && k.at.elapsed() < fresh) {
            return k.answer;
        }
    }
    let answer = start_identity(pid, recorded, domain);
    let mut known = KNOWN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    known.retain(|k| !same(k) && k.at.elapsed() < fresh);
    known.push(Known {
        pid,
        recorded: recorded.map(str::to_string),
        domain: domain.map(str::to_string),
        at: Instant::now(),
        answer,
    });
    answer
}

/// A Claude session file's entrypoint decides whether it is a chat to list:
/// * Claude Desktop and the IDE extension (`claude-vscode`) are app chats: always listed
///   (a file without the key is an older Claude's and is kept).
/// * The terminal (`cli`) is listed only when `terminal` is on, the session is `interactive`
///   and its newest `statusUpdatedAt` / `updatedAt` is within `TERMINAL_CHAT_WINDOW`.
/// * Anything else is not a chat: SDK and non-interactive sessions, the host app hub's own
///   registration (`pulse-hub`, which Claude needs to accept its posts) and `cc-post`.
fn listed_entrypoint(value: &Value, terminal: bool, now_ms: u64) -> bool {
    match value.get("entrypoint").and_then(Value::as_str) {
        None | Some("claude-desktop" | "claude-vscode") => true,
        Some("cli") => {
            terminal
                && value.get("kind").and_then(Value::as_str) == Some("interactive")
                && ["statusUpdatedAt", "updatedAt"]
                    .iter()
                    .filter_map(|k| value.get(*k).and_then(Value::as_u64))
                    .max()
                    .is_some_and(|ms| within_terminal_window(ms, now_ms))
        }
        Some(_) => false,
    }
}

fn parse_claude(
    value: Value,
    alive: &dyn Fn(u32) -> bool,
    terminal: bool,
    now_ms: u64,
) -> Option<LocalSession> {
    let pid = u32::try_from(value.get("pid")?.as_u64()?).ok()?;
    // Checked before the process: a stale terminal file costs no process lookup.
    if !listed_entrypoint(&value, terminal, now_ms) {
        return None;
    }
    if !alive(pid) {
        return None;
    }
    // A reused pid is not this chat: the process must have started when the file says.
    let recorded_start = text_of(&value["procStart"]);
    let domain = text_of(&value["pidDomain"]);
    let liveness = match listed_identity(pid, recorded_start.as_deref(), domain.as_deref()) {
        Some(true) => "live",
        Some(false) => return None,
        None => "unknown",
    };
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let id = text("sessionId").filter(|s| !s.is_empty())?;
    let cwd = text("cwd").unwrap_or_default();
    let name = title_for(&text("name").unwrap_or_default(), &cwd, "Claude chat");
    Some(LocalSession {
        id,
        kind: "claude".to_string(),
        name,
        cwd,
        status: text("status").unwrap_or_else(|| "unknown".to_string()),
        updated_ms: ["statusUpdatedAt", "updatedAt"]
            .iter()
            .find_map(|k| value.get(*k).and_then(Value::as_u64)),
        liveness: liveness.to_string(),
        pid: Some(pid),
        messaging_socket: text("messagingSocketPath").filter(|s| !s.is_empty()),
        peer_protocol: value.get("peerProtocol").and_then(Value::as_u64),
        entrypoint: text("entrypoint"),
        raw: value,
    })
}

type RegistryCache = HashMap<std::path::PathBuf, (std::time::SystemTime, u64, Value)>;

fn claude_registry_cache() -> std::sync::MutexGuard<'static, RegistryCache> {
    static CACHE: std::sync::OnceLock<Mutex<RegistryCache>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// One Claude registry file's JSON, parsed again only when its (mtime, length) changed.
/// Liveness and recency are still judged on every listing from the cached value.
fn claude_registry_value(entry: &std::fs::DirEntry) -> Option<Value> {
    let path = entry.path();
    let stamp = entry
        .metadata()
        .ok()
        .and_then(|m| Some((m.modified().ok()?, m.len())));
    if let Some((m, l)) = stamp
        && let Some((cm, cl, value)) = claude_registry_cache().get(&path)
        && (*cm, *cl) == (m, l)
    {
        return Some(value.clone());
    }
    let value: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    if let Some((m, l)) = stamp {
        claude_registry_cache().insert(path, (m, l, value.clone()));
    }
    Some(value)
}

/// Live Claude chats described by the `*.json` files in `dir`. `terminal` adds the
/// interactive terminal chats active within `TERMINAL_CHAT_WINDOW` (see `listed_entrypoint`).
pub fn claude_sessions_in(
    dir: &Path,
    alive: &dyn Fn(u32) -> bool,
    terminal: bool,
) -> Vec<LocalSession> {
    let now_ms = super::envelope::now_ms();
    // The files this listing saw: a cached file not among them is gone, so the cache cannot
    // grow (no second stat per cached file on every listing).
    let mut seen: std::collections::HashSet<PathBuf> = Default::default();
    let mut sessions: Vec<LocalSession> = std::fs::read_dir(dir)
        .map(|listing| {
            listing
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| {
                    seen.insert(e.path());
                    claude_registry_value(&e)
                })
                .filter_map(|value| parse_claude(value, alive, terminal, now_ms))
                .collect()
        })
        .unwrap_or_default();
    claude_registry_cache().retain(|p, _| p.parent() != Some(dir) || seen.contains(p));
    sessions.sort_by(|a, b| a.id.cmp(&b.id));
    // One session id can have two files (a resumed chat's old file, still alive): one row.
    keep_newest(sessions, |s| s.id.clone(), |s| s.updated_ms)
}

/// One row per `key`, the one with the newest `updated` (the first one on a tie). Rows keep
/// the order of their first occurrence.
fn keep_newest<T, K: Eq + Hash>(
    rows: Vec<T>,
    key: impl Fn(&T) -> K,
    updated: impl Fn(&T) -> Option<u64>,
) -> Vec<T> {
    let mut at: HashMap<K, usize> = HashMap::new();
    let mut out: Vec<T> = Vec::with_capacity(rows.len());
    for row in rows {
        let k = key(&row);
        if let Some(&i) = at.get(&k) {
            if updated(&row).unwrap_or(0) > updated(&out[i]).unwrap_or(0) {
                out[i] = row;
            }
            continue;
        }
        at.insert(k, out.len());
        out.push(row);
    }
    out
}

/// Every chat on this computer: live Claude chats from disk, plus live chats
/// registered that disk discovery did not already list.
pub fn local_sessions_in(
    store: &Store,
    claude_dir: Option<&Path>,
    alive: &dyn Fn(u32) -> bool,
) -> Vec<LocalSession> {
    local_sessions_in_with(store, claude_dir, alive, store.terminal_chats())
}

/// `local_sessions_in` with the terminal chats forced on or off, whatever the roster setting says.
pub fn local_sessions_in_with(
    store: &Store,
    claude_dir: Option<&Path>,
    alive: &dyn Fn(u32) -> bool,
    terminal: bool,
) -> Vec<LocalSession> {
    let sessions = claude_dir
        .map(|dir| claude_sessions_in(dir, alive, terminal))
        .unwrap_or_default();
    with_registered(store, sessions, alive)
}

/// Live Claude chats from every Claude profile folder on this computer
/// (`deliver_claude::sessions_dirs`, #350). A chat in another profile carries the profile in its
/// name ("the host app VM (Claude B)"); a chat found in two folders is listed once.
pub fn claude_sessions_all(alive: &dyn Fn(u32) -> bool, terminal: bool) -> Vec<LocalSession> {
    let mut sessions: Vec<(LocalSession, Option<String>)> = Vec::new();
    for (dir, label) in super::deliver_claude::sessions_dirs() {
        // The account this folder is signed in to, so a running or terminal chat names it.
        let account = dir
            .parent()
            .and_then(rightkit_ai_accounts::names::account_of_config);
        for mut session in claude_sessions_in(&dir, alive, terminal) {
            if sessions.iter().any(|(s, _)| s.id == session.id) {
                continue;
            }
            if let Some(label) = &label {
                session.name = format!("{} ({label})", session.name);
            }
            sessions.push((session, account.clone()));
        }
    }
    // With more than one account's chats on this computer, each chat names its account.
    let several = sessions
        .iter()
        .filter_map(|(_, account)| account.as_deref())
        .collect::<std::collections::HashSet<_>>()
        .len()
        > 1;
    sessions
        .into_iter()
        .map(|(mut session, account)| {
            if several && let Some(account) = account {
                set_account(
                    &mut session,
                    &rightkit_ai_accounts::desktop::account_label(&account),
                );
            }
            session
        })
        .collect()
}

/// Records the account label on a chat (in `raw`, so no constructor changes): listings show it
/// beside the name, and it is never part of the name a send matches.
fn set_account(session: &mut LocalSession, label: &str) {
    if !session.raw.is_object() {
        session.raw = serde_json::json!({});
    }
    session.raw["pulseAccount"] = Value::String(label.to_string());
}

/// `sessions` plus the live chats registered with the host app that disk discovery did not list.
fn with_registered(
    store: &Store,
    mut sessions: Vec<LocalSession>,
    alive: &dyn Fn(u32) -> bool,
) -> Vec<LocalSession> {
    for registered in store.registered_sessions() {
        if !alive(registered.pid) || sessions.iter().any(|s| s.id == registered.id) {
            continue;
        }
        sessions.push(LocalSession {
            name: title_for(&registered.name, &registered.cwd, "Chat"),
            id: registered.id.clone(),
            kind: registered.kind.clone(),
            cwd: registered.cwd.clone(),
            status: "unknown".to_string(),
            updated_ms: None,
            liveness: "unknown".to_string(),
            pid: Some(registered.pid),
            messaging_socket: None,
            peer_protocol: None,
            entrypoint: None,
            raw: serde_json::to_value(&registered).unwrap_or(Value::Null),
        });
    }
    sessions
}

/// How many idle Claude Desktop chats (newest first) are listed.
const IDLE_DESKTOP_LIMIT: usize = 100;

/// What the idle-chat listing keeps of one Desktop chat record.
#[derive(Clone)]
struct DesktopRecord {
    host: String,
    cli: String,
    cwd: String,
    title: String,
    last_activity: Option<u64>,
    /// The account folder the record sits under (its id).
    account: String,
}

/// (mtime, length, parsed record, when it was last fully parsed).
type DesktopCache = std::collections::HashMap<
    std::path::PathBuf,
    (
        std::time::SystemTime,
        u64,
        Option<DesktopRecord>,
        std::time::Instant,
    ),
>;

/// A live Desktop session rewrites its record every few seconds, and a full parse skips the whole
/// message cache (up to ~300 KB). A record that only grew or was touched is not parsed again for
/// this long; the fields the roster uses (ids, cwd, title, archived/scheduled flags) change
/// rarely, and `last_activity` is taken from the new mtime meanwhile. A deferred change is never
/// lost: the cache keeps the last parsed stamp and re-arms the sweep until the full parse runs,
/// so a rename, archive or schedule change shows at most this long (plus one debounce) late.
const DESKTOP_REPARSE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

fn desktop_cache() -> std::sync::MutexGuard<'static, DesktopCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<DesktopCache>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// One Desktop chat record, parsed only when its (mtime, length) changed since the last refresh
///. `None` when unreadable, archived, scheduled or missing its ids.
fn desktop_record(path: &std::path::Path, entry: &std::fs::DirEntry) -> Option<DesktopRecord> {
    let meta = entry.metadata().ok()?;
    let stamp = (meta.modified().ok()?, meta.len());
    if let Some((m, l, rec, parsed)) = desktop_cache().get_mut(path) {
        if (*m, *l) == stamp {
            return rec.clone();
        }
        // Changed, but only forward and only recently parsed: reuse the fields, advance the
        // activity time to the new mtime. A shrunk or replaced file (older mtime, shorter) and a
        // record that was not a chat (None: it may have been unarchived) are parsed in full.
        if rec.is_some()
            && stamp.0 >= *m
            && stamp.1 >= *l
            && parsed.elapsed() < DESKTOP_REPARSE_AFTER
        {
            if let Some(r) = rec.as_mut()
                && r.last_activity.is_some()
                && let Ok(ms) = stamp.0.duration_since(std::time::UNIX_EPOCH)
            {
                r.last_activity = r.last_activity.max(Some(ms.as_millis() as u64));
            }
            // The cached stamp stays the last fully parsed one, so every later sweep still sees
            // the change and parses in full once DESKTOP_REPARSE_AFTER has passed; re-arming the
            // sweep makes that happen even if the file is not touched again (no watch event).
            DESKTOP_DIRTY.store(true, std::sync::atomic::Ordering::SeqCst);
            return rec.clone();
        }
    }
    // A record untouched for 30 days is not a chat anyone is coming back to: not read at all.
    // The Dell has 6,000+ records (~950 MB, some 300 KB each), so this and the typed read below
    // keep the first sweep after a start from parsing every one in full.
    if meta
        .modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age > std::time::Duration::from_secs(30 * 86_400))
    {
        desktop_cache().insert(
            path.to_path_buf(),
            (stamp.0, stamp.1, None, std::time::Instant::now()),
        );
        return None;
    }
    /// Only the fields the roster uses; the rest of the record (its message cache) is skipped
    /// by the parser without being built.
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RecordFields {
        session_id: Option<String>,
        cli_session_id: Option<String>,
        cwd: Option<String>,
        title: Option<String>,
        last_activity_at: Option<u64>,
        is_archived: Option<bool>,
        scheduled_task_id: Option<serde::de::IgnoredAny>,
    }
    let nonempty = |v: Option<String>| v.filter(|s| !s.is_empty());
    let rec = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RecordFields>(&bytes).ok())
        .and_then(|value| {
            if value.is_archived == Some(true) || value.scheduled_task_id.is_some() {
                return None;
            }
            Some(DesktopRecord {
                host: nonempty(value.session_id)?,
                cli: nonempty(value.cli_session_id)?,
                cwd: value.cwd.unwrap_or_default(),
                title: value.title.unwrap_or_default(),
                last_activity: value.last_activity_at,
                account: path
                    .parent()
                    .and_then(std::path::Path::parent)
                    .and_then(|dir| dir.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        });
    desktop_cache().insert(
        path.to_path_buf(),
        (stamp.0, stamp.1, rec.clone(), std::time::Instant::now()),
    );
    rec
}

/// The idle Claude Desktop Code chats: Desktop keeps one record per chat,
/// `<data folder>/claude-code-sessions/<account>/<org>/local_<uuid>.json`, whether or not the chat's
/// CLI is running. A chat with no running process has no registry file and no pipe, so the
/// process-based listing never saw it. Each one the signed-in account has, not archived or
/// scheduled and not already listed as running, is added as a `stale` chat with status `idle`. Its
/// id is the CLI session id; `raw` carries Desktop's record id (`hostSessionId`) so a send can
/// wake it (`deliver_claude::wake_idle_desktop`). `others` adds the data folders of the other
/// Claude profiles (Claude B); only the default folder's Desktop takes the `claude://` link a
/// wake uses, so a chat elsewhere is listed but marked not wakeable.
pub fn add_idle_desktop(sessions: &mut Vec<LocalSession>, others: bool) {
    let mut idle: Vec<LocalSession> = Vec::new();
    let scan = desktop_scan(others);
    // With more than one account's chats on this computer, each chat names its account.
    let several_accounts = scan
        .iter()
        .map(|(_, _, _, rec)| rec.account.as_str())
        .filter(|a| !a.is_empty())
        .collect::<std::collections::HashSet<_>>()
        .len()
        > 1;
    // The scan is newest first, so the listing stops at the limit instead of building a row for
    // every record (the Dell has thousands) only to cut the list after sorting; a record skipped
    // as already listed depends only on earlier rows, so the cut list is the same.
    for (root, label, main, rec) in scan.iter() {
        if idle.len() >= IDLE_DESKTOP_LIMIT {
            break;
        }
        let (host, cli) = (rec.host.as_str(), rec.cli.as_str());
        if sessions
            .iter()
            .chain(idle.iter())
            .any(|s| s.id == cli || s.raw["hostSessionId"].as_str() == Some(host))
        {
            continue;
        }
        let cwd = rec.cwd.as_str();
        let mut name = title_for(&rec.title, cwd, "Chat");
        if let Some(label) = label {
            name = format!("{name} ({label})");
        }
        let account = if several_accounts && !rec.account.is_empty() {
            rightkit_ai_accounts::desktop::account_label(&rec.account)
        } else {
            String::new()
        };
        idle.push(LocalSession {
            id: cli.to_string(),
            kind: "claude".to_string(),
            name,
            cwd: cwd.to_string(),
            status: "idle".to_string(),
            updated_ms: rec.last_activity,
            liveness: "stale".to_string(),
            pid: None,
            messaging_socket: None,
            peer_protocol: None,
            entrypoint: Some("claude-desktop".to_string()),
            raw: serde_json::json!({
                "hostSessionId": host,
                "desktopIdle": true,
                "desktopWakeable": *main,
                "desktopRoot": root.to_string_lossy(),
                "pulseAccount": account,
            }),
        });
    }
    idle.sort_by_key(|a| std::cmp::Reverse(a.updated_ms));
    idle.truncate(IDLE_DESKTOP_LIMIT);
    sessions.extend(idle);
}

/// Set by the folder watch (engine) when anything under a `claude-code-sessions` folder changed.
static DESKTOP_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// A folder watch is running, so an unchanged folder needs no directory sweep.
static DESKTOP_WATCHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Slow backstop: with a watch running, the folders are still swept this often.
const DESKTOP_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(120);
/// A watch-flagged change sweeps at most this often (active Desktop sessions flag one every few
/// seconds, so while Desktop is busy the sweep runs at this rate: over the Dell's 6,000+ records
/// that was a walk every 10 s, #449). The idle list has no liveness of its own (running chats come
/// from the registry files), so it only lags new, renamed or reordered idle chats by this much.
const DESKTOP_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(30);

type DesktopRows = Vec<(PathBuf, Option<String>, bool, DesktopRecord)>;
/// Shared, so a listing that finds the folders unchanged takes a pointer, not a copy of every record.
type DesktopScan = std::sync::Arc<DesktopRows>;
type DesktopMemo = (bool, std::time::Instant, DesktopScan);

/// The folder watch reports a change under a Desktop chat folder: the next listing sweeps again.
pub fn mark_desktop_dirty() {
    DESKTOP_DIRTY.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Whether a folder watch is running (`false` when it ended or could not start).
pub fn set_desktop_watched(watched: bool) {
    DESKTOP_WATCHED.store(watched, std::sync::atomic::Ordering::SeqCst);
    if !watched {
        mark_desktop_dirty();
    }
}

/// The Desktop chat records, swept from disk only when the watch flagged a change, the last sweep
/// is older than `DESKTOP_BACKSTOP`, `others` changed, or no watch runs (then every call sweeps,
/// as before). Within a sweep an unchanged record (path, length, mtime) is not parsed again.
fn desktop_scan(others: bool) -> DesktopScan {
    static MEMO: std::sync::Mutex<Option<DesktopMemo>> = std::sync::Mutex::new(None);
    let mut memo = MEMO.lock().unwrap_or_else(|p| p.into_inner());
    let dirty = DESKTOP_DIRTY.swap(false, std::sync::atomic::Ordering::SeqCst);
    if DESKTOP_WATCHED.load(std::sync::atomic::Ordering::SeqCst)
        && let Some((was_others, at, scan)) = memo.as_ref()
        && *was_others == others
    {
        let age = at.elapsed();
        if !dirty && age < DESKTOP_BACKSTOP {
            return std::sync::Arc::clone(scan);
        }
        if dirty && age < DESKTOP_DEBOUNCE {
            // Too soon: keep the flag so the change is swept once the debounce passes.
            DESKTOP_DIRTY.store(true, std::sync::atomic::Ordering::SeqCst);
            return std::sync::Arc::clone(scan);
        }
    }
    let scan = std::sync::Arc::new(desktop_sweep(others));
    *memo = Some((
        others,
        std::time::Instant::now(),
        std::sync::Arc::clone(&scan),
    ));
    scan
}

fn desktop_sweep(others: bool) -> DesktopRows {
    let mut found: DesktopRows = Vec::new();
    for (root, label, main) in super::deliver_claude::desktop_chat_roots() {
        if !main && !others {
            continue;
        }
        // Every account folder (any signed-in account), not only `lastKnownAccountUuid`:
        // a slot's chats may sit under an account the config no longer names.
        let Ok(accounts) = std::fs::read_dir(root.join("claude-code-sessions")) else {
            continue;
        };
        let orgs: Vec<std::fs::DirEntry> = accounts
            .flatten()
            .filter_map(|a| std::fs::read_dir(a.path()).ok())
            .flat_map(|d| d.flatten())
            .collect();
        for org in orgs {
            let Ok(records) = std::fs::read_dir(org.path()) else {
                continue;
            };
            let org_path = org.path();
            let mut seen: std::collections::HashSet<std::path::PathBuf> = Default::default();
            for record in records.flatten() {
                let file = record.file_name().to_string_lossy().into_owned();
                if !file.starts_with("local_") || !file.ends_with(".json") {
                    continue;
                }
                let path = record.path();
                // Seen before it is judged: a record that is not a chat (archived, scheduled, older
                // than 30 days) is cached as such, and must stay cached. Marking only the chats
                // let the `retain` below drop those entries every sweep, so each sweep read and
                // parsed every archived record in full again (#444: the Dell's ~290 MB per sweep).
                seen.insert(path.clone());
                let Some(rec) = desktop_record(&path, &record) else {
                    continue;
                };
                found.push((root.clone(), label.clone(), main, rec));
            }
            desktop_cache()
                .retain(|path, _| path.parent() != Some(org_path.as_path()) || seen.contains(path));
        }
    }
    // The same chat is recorded under several account/org folders: list it once, newest kept.
    found.sort_by_key(|(_, _, _, rec)| std::cmp::Reverse(rec.last_activity));
    let mut cli_seen: std::collections::HashSet<String> = Default::default();
    found.retain(|(_, _, _, rec)| cli_seen.insert(rec.cli.clone()));
    found
}

/// How many Codex threads (newest first, not archived) are listed.
pub const CODEX_THREAD_LIMIT: usize = 100;

/// How many of the newest Codex threads have their rollout file read for their liveness
/// (the rest are judged by their hook heartbeat alone; a rollout tail is up to 512 KiB).
const CODEX_ROLLOUT_LIMIT: usize = 20;

/// A Codex thread as a chat here. Codex keeps no registry of open chats, so liveness comes
/// from the same evidence delivery uses (`deliver_codex::chat_state_of`): the Codex hook's
/// last run for the thread and, when `inspect`, the last lifecycle event in its rollout.
/// A running turn is "live" with status "busy"; a chat between turns (a run ended, the last
/// turn finished, stopped by its owner, or no tool run for ten minutes) is "stale", the
/// roster's closest word for "not running anything" (it has no "idle" liveness), with
/// status "idle"; "unknown" only when there is neither a hook run nor a readable rollout,
/// and then the status only says how recently the thread was written.
fn codex_session(thread: CodexThread, inspect: bool) -> LocalSession {
    let (liveness, observed) = match chat_state_of(&thread.id, inspect) {
        ChatState::Busy => ("live", Some("busy")),
        ChatState::Stopped | ChatState::Idle | ChatState::Quiet => ("stale", Some("idle")),
        ChatState::Unknown => ("unknown", None),
    };
    let status = if thread.archived {
        "archived".to_string()
    } else {
        observed.map_or_else(|| codex_status(thread.updated_ms), str::to_string)
    };
    LocalSession {
        name: title_for(&thread.name, "", "Codex chat"),
        raw: serde_json::json!({
            "id": thread.id,
            "thread_name": thread.name,
            "updated_at": thread.updated_at,
            "updated_at_ms": thread.updated_ms,
            "archived": thread.archived,
        }),
        id: thread.id,
        kind: "codex".to_string(),
        cwd: thread.cwd,
        status,
        updated_ms: Some(thread.updated_ms).filter(|m| *m > 0 && !in_future(*m)),
        liveness: liveness.to_string(),
        pid: None,
        messaging_socket: None,
        peer_protocol: None,
        entrypoint: None,
    }
}

/// Add the Codex threads in `threads` that are not already listed. App threads always; an
/// interactive terminal thread (`CodexThread::terminal`) only when `terminal` is on and the
/// thread was written within `TERMINAL_CHAT_WINDOW` (the rollout's write time is the one
/// recency Codex records; the hook heartbeat only follows a turn that wrote it).
pub fn add_codex_threads(
    sessions: &mut Vec<LocalSession>,
    threads: Vec<CodexThread>,
    terminal: bool,
) {
    let now_ms = super::envelope::now_ms();
    // One pass: the threads that will be listed and have their rollout read, decided first, so
    // their rollout paths come from one database query instead of one open per chat.
    let listed = |thread: &CodexThread| {
        !sessions.iter().any(|s| s.id == thread.id)
            && !(thread.terminal
                && !(terminal && within_terminal_window(thread.updated_ms, now_ms)))
    };
    let inspected: Vec<&str> = threads
        .iter()
        .enumerate()
        .filter(|(rank, thread)| *rank < CODEX_ROLLOUT_LIMIT && listed(thread))
        .map(|(_, thread)| thread.id.as_str())
        .collect();
    prime_rollout_paths(&inspected);
    for (rank, thread) in threads.into_iter().enumerate() {
        if sessions.iter().any(|s| s.id == thread.id) {
            continue;
        }
        if thread.terminal && !(terminal && within_terminal_window(thread.updated_ms, now_ms)) {
            continue;
        }
        sessions.push(codex_session(thread, rank < CODEX_ROLLOUT_LIMIT));
    }
    // A thread a running `codex resume <id>` terminal session has open is a terminal chat
    // whatever created it: a thread begun by `codex exec` and resumed in the TUI has source
    // `exec`, which the list above never selects. The terminal setting applies;
    // the 6-hour window does not, since a running session is live whenever it last wrote.
    if terminal {
        for id in terminal_session_threads(false) {
            if sessions.iter().any(|s| s.id == id) {
                continue;
            }
            if let Ok(Some(thread)) = find_thread_by_id(&id)
                && !thread.archived
            {
                sessions.push(codex_session(thread, true));
            }
        }
    }
}

/// The Codex thread with exactly this id (archived ones too), whether or not it is
/// among the recent threads the roster lists. `Ok(None)` when it does not exist.
pub fn resolve_exact_codex(id: &str) -> Result<Option<LocalSession>, DiscoveryError> {
    Ok(find_thread_by_id(id)?.map(|thread| codex_session(thread, true)))
}

/// A hook harness's session (Muse's or a registered one's) as a chat here: the session's name and
/// folder from the harness's own record, the process from its registration (`pid`) when the hook
/// has registered it. Its liveness comes from the hook's last run (`apply_hook_state`).
fn hook_session(kind: &str, found: MuseSession, pid: Option<u32>) -> LocalSession {
    let mut label = kind.to_string();
    if let Some(first) = label.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    LocalSession {
        name: title_for(&found.title, &found.cwd, &format!("{label} chat")),
        raw: serde_json::json!({
            "id": found.id,
            "title": found.title,
            "updated_at_ms": found.updated_ms,
        }),
        id: found.id,
        kind: kind.to_string(),
        cwd: found.cwd,
        status: "unknown".to_string(),
        updated_ms: Some(found.updated_ms).filter(|m| *m > 0 && !in_future(*m)),
        liveness: "unknown".to_string(),
        pid,
        messaging_socket: None,
        peer_protocol: None,
        entrypoint: None,
    }
}

/// What the hook's last run says about a hook harness's chat (Muse or another): a run going is "live" and "busy"; between
/// turns or quiet is "stale" and "idle" (the roster's word for not running anything, as for Codex);
/// "unknown" with only how recently Muse wrote the session when the hook never ran for it.
fn apply_hook_state(session: &mut LocalSession) {
    let (liveness, status) = match deliver_hook::state_of(&session.kind, &session.id) {
        HookState::Busy => ("live", "busy".to_string()),
        HookState::Idle | HookState::Quiet => ("stale", "idle".to_string()),
        HookState::Unknown => (
            "unknown",
            codex_status(session.updated_ms.unwrap_or_default()),
        ),
    };
    session.liveness = liveness.to_string();
    session.status = status;
}

/// Hook-harness chats: Muse and every registered harness (`harness::kinds`, #434). With a
/// harness's switch off none of its chats is listed (registered ones included). With it on, the
/// chats its hook registered (already in `sessions` while their process lives) are kept and joined
/// by the harness's recent sessions, so a chat that has not run the hook yet still shows. A
/// session found only on disk is listed like a Codex terminal thread: only with `terminal` on and
/// active within `TERMINAL_CHAT_WINDOW` (the hook's registration is the evidence of a chat that is
/// open). Liveness comes from the hook's heartbeat for every such chat.
pub fn add_hook_harness_sessions(sessions: &mut Vec<LocalSession>, store: &Store, terminal: bool) {
    let mut kinds = harness::kinds(store);
    for s in sessions.iter() {
        if super::codex_hook::valid_hook_harness(&s.kind) && !kinds.contains(&s.kind) {
            kinds.push(s.kind.clone());
        }
    }
    let now_ms = super::envelope::now_ms();
    for kind in &kinds {
        let enabled = store.harness_chats(kind);
        sessions.retain(|s| enabled || s.kind != *kind);
        let found_sessions = if enabled {
            harness::list(store, kind, muse::SESSION_LIMIT)
        } else {
            Vec::new()
        };
        for mut found in found_sessions {
            let registered = sessions
                .iter()
                .find(|s| s.id == found.id && s.kind == *kind)
                .map(|s| s.pid);
            if registered.is_none() {
                let recent =
                    found.updated_ms > 0 && within_terminal_window(found.updated_ms, now_ms);
                if !(terminal && recent) || sessions.iter().any(|s| s.id == found.id) {
                    continue;
                }
            }
            // The index or folder walk may not know a live chat's name or folder (Muse's index
            // leaves them empty for some sessions): the hook's registration record supplies them,
            // so the chat is listed by name rather than as a generic "Muse chat".
            if found.title.trim().is_empty() || found.cwd.is_empty() {
                let record = store
                    .registered_session(&found.id)
                    .filter(|r| r.kind == *kind);
                if let Some(record) = record {
                    if found.title.trim().is_empty() {
                        found.title = record.name;
                    }
                    if found.cwd.is_empty() {
                        found.cwd = record.cwd;
                    }
                }
            }
            sessions.retain(|s| s.id != found.id);
            sessions.push(hook_session(kind, found, registered.flatten()));
        }
    }
    for session in sessions
        .iter_mut()
        .filter(|s| super::codex_hook::valid_hook_harness(&s.kind))
    {
        apply_hook_state(session);
    }
}

/// The hook-harness chat with exactly this id (Muse first), whether or not it is among the recent
/// ones; harnesses whose switch is off are not searched.
pub fn resolve_exact_hook(store: &Store, id: &str) -> Option<LocalSession> {
    harness::kinds(store)
        .into_iter()
        .filter(|kind| store.harness_chats(kind))
        .find_map(|kind| {
            let mut session = hook_session(&kind, harness::find(store, &kind, id)?, None);
            apply_hook_state(&mut session);
            Some(session)
        })
}

/// A stamp more than a minute ahead of this computer's clock is not believable.
fn in_future(updated_ms: u64) -> bool {
    updated_ms > super::envelope::now_ms() + 60_000
}

/// Codex keeps no registry of open chats, so this only says how recently a thread was
/// written: "recent" within the last ten minutes, otherwise "idle". A future timestamp
/// is never "recent". Neither word says the chat is open or waiting (see `liveness`).
pub fn codex_status(updated_ms: u64) -> String {
    let now = super::envelope::now_ms();
    let recent = updated_ms > 0 && updated_ms <= now + 60_000 && now <= updated_ms + 10 * 60 * 1000;
    if recent {
        "recent".to_string()
    } else {
        "idle".to_string()
    }
}

/// Seconds since the epoch of an ISO-8601 `YYYY-MM-DDTHH:MM:SS…` UTC stamp.
pub(super) fn iso_epoch(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| text.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + s).ok()
}

/// `local_sessions_in` for this computer's real folders, plus the most recent
/// Codex threads from `~/.codex/session_index.jsonl`.
pub fn local_sessions(store: &Store) -> Vec<LocalSession> {
    let alive = live_pids();
    let claude = claude_sessions_all(&alive, store.terminal_chats());
    let mut sessions = with_registered(store, claude, &alive);
    add_idle_desktop(&mut sessions, store.claude_profiles());
    add_codex_threads(
        &mut sessions,
        list_threads(CODEX_THREAD_LIMIT),
        store.terminal_chats(),
    );
    add_hook_harness_sessions(&mut sessions, store, store.terminal_chats());
    sessions
}

/// Every chat on this computer including the terminal chats, whether or not "List terminal chats"
/// is on. For identity checks (the per-chat unlock), which must find a CLI chat the roster hides.
pub fn local_sessions_with_terminal(store: &Store) -> Vec<LocalSession> {
    let alive = live_pids();
    let claude = claude_sessions_all(&alive, true);
    let mut sessions = with_registered(store, claude, &alive);
    add_codex_threads(&mut sessions, list_threads(CODEX_THREAD_LIMIT), true);
    add_hook_harness_sessions(&mut sessions, store, true);
    sessions
}

pub fn local_entries(sessions: &[LocalSession]) -> Vec<RosterEntry> {
    let switches = WakeSwitches::current();
    sessions.iter().map(|s| s.entry_with(&switches)).collect()
}

fn display(name: &str, alias: &str) -> String {
    format!("{name} on {alias}")
}

/// Local chats plus what each linked computer listed, each labelled with its
/// computer. A link that cannot be asked now contributes its last good listing (`cached`),
/// marked "unreachable"; a link with no listing at all contributes nothing. One chat (a
/// device and a session id) is one peer: when it is listed more than once, the newest
/// `updated_ms` gives the row.
pub fn merge(local: &[LocalSession], local_alias: &str, remotes: &[RemoteChats]) -> Vec<Peer> {
    merge_with(local, local_alias, remotes, &Default::default())
}

/// `merge`, with `listed_live_elsewhere`: Claude sessions a linked computer last listed live
/// (`links::live_sessions_elsewhere`, the kept listings), so a stale local row is hidden also
/// when that computer did not answer in this command or is not among `remotes`.
pub fn merge_with(
    local: &[LocalSession],
    local_alias: &str,
    remotes: &[RemoteChats],
    listed_live_elsewhere: &std::collections::BTreeSet<String>,
) -> Vec<Peer> {
    let switches = WakeSwitches::current();
    // A Claude Desktop chat whose environment is another computer (Desktop's SSH environments) is
    // listed here from Desktop's record and there from its running process, under the same session
    // id. Only the process there can take a message (its hook runs there, #483), so a local row that
    // is not live gives way to a linked computer that answered now and lists that session.
    let elsewhere: std::collections::BTreeSet<&str> = remotes
        .iter()
        .filter_map(|r| r.listing.as_ref().ok())
        .flat_map(|listing| listing.chats.iter())
        .filter(|entry| entry.kind == "claude")
        .map(|entry| entry.session.as_str())
        .chain(listed_live_elsewhere.iter().map(String::as_str))
        .collect();
    let mut peers: Vec<Peer> = local
        .iter()
        .filter(|s| {
            !(s.kind == "claude" && s.liveness != "live" && elsewhere.contains(s.id.as_str()))
        })
        .map(|s| Peer {
            wake: wake_word(s, &switches).to_string(),
            id: s.id.clone(),
            session: s.id.clone(),
            name: s.name.clone(),
            display: display(&s.name, local_alias),
            device: local_alias.to_string(),
            device_alias: local_alias.to_string(),
            local: true,
            kind: s.kind.clone(),
            cwd: s.cwd.clone(),
            status: s.status.clone(),
            updated_ms: s.updated_ms,
            liveness: s.liveness.clone(),
            last_seen_ms: None,
            account: s.account(),
        })
        .collect();
    for remote in remotes {
        let (chats, last_seen_ms) = match (&remote.listing, &remote.cached) {
            (Ok(listing), _) => (listing.chats.as_slice(), None),
            (Err(_), Some(cached)) => (cached.listing.chats.as_slice(), Some(cached.last_seen_ms)),
            (Err(_), None) => continue,
        };
        for entry in chats {
            peers.push(Peer {
                id: format!("{}:{}", remote.link.device, entry.session),
                session: entry.session.clone(),
                name: entry.name.clone(),
                display: display(&entry.name, &remote.link.device),
                device: remote.link.device.clone(),
                device_alias: remote.link.device.clone(),
                local: false,
                kind: entry.kind.clone(),
                cwd: entry.cwd.clone(),
                status: entry.status.clone(),
                updated_ms: entry.updated_ms,
                liveness: if last_seen_ms.is_some() {
                    LIVENESS_UNREACHABLE.to_string()
                } else {
                    entry.liveness.clone()
                },
                last_seen_ms,
                wake: entry.wake.clone(),
                account: entry.account.clone(),
            });
        }
    }
    keep_newest(
        peers,
        |p| (p.device.to_lowercase(), p.session.clone()),
        |p| p.updated_ms,
    )
}

fn matches_tier(tier: usize, p: &Peer, wanted: &str) -> bool {
    match tier {
        0 => p.id.to_lowercase() == wanted,
        1 => p.session.to_lowercase() == wanted,
        2 => p.display.to_lowercase() == wanted || p.name.to_lowercase() == wanted,
        // A part of a display name is for a fragment the user typed ("swift"). A full address
        // ("<title> on <device>") that matched nothing is never read as a fragment of another
        // chat's name: it is refused.
        _ => !wanted.contains(" on ") && p.display.to_lowercase().contains(wanted),
    }
}

/// The peer a user or chat meant: exact id, then session id, then title or
/// "title on device", then a part of the display name (a fragment, never a full
/// "title on device" address). More than one match is an error that lists them (never picked between, even when
/// one is live or newer). A name that matches nothing is
/// refused, never read as some other chat.
pub fn resolve(peers: &[Peer], query: &str) -> Result<Peer, ChatError> {
    match resolve_exact(peers, query) {
        // A name copied from a listing carries its ` [Claude A]` label: match without it.
        Err(first @ ChatError::NotFound(_)) => {
            let stripped = strip_account_label(query);
            if stripped == query {
                return Err(first);
            }
            resolve_exact(peers, &stripped).map_err(|_| first)
        }
        other => other,
    }
}

/// `query` without ` [..]` account labels (`the host app VM [Claude A] on mac` is `the host app VM on mac`).
fn strip_account_label(query: &str) -> String {
    let mut out = String::new();
    let mut rest = query;
    while let Some(open) = rest.find(" [") {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        out.push_str(&rest[..open]);
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

fn resolve_exact(peers: &[Peer], query: &str) -> Result<Peer, ChatError> {
    let wanted = query.trim().to_lowercase();
    if wanted.is_empty() {
        return Err(ChatError::Invalid("who should it go to?".into()));
    }
    for tier in 0..4 {
        let hits: Vec<&Peer> = peers
            .iter()
            .filter(|p| matches_tier(tier, p, &wanted))
            .collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(hits[0].clone()),
            _ => {
                // Never a guess between chats that answer to one name (#500: "message pulse"
                // went to the newest of several): the candidates are listed, the sender picks by
                // id or "<title> on <device>" with an id.
                let names: Vec<String> = hits
                    .iter()
                    .map(|p| format!("{} ({})", p.display, p.id))
                    .collect();
                return Err(ChatError::Invalid(format!(
                    "\"{query}\" matches more than one chat: {}",
                    names.join(", ")
                )));
            }
        }
    }
    // Nothing matched: refuse and list the chats on the named device (or all, capped) so the
    // sender picks one rather than guessing.
    let device = wanted.rsplit_once(" on ").map(|(_, d)| d.trim());
    let mut near: Vec<String> = peers
        .iter()
        .filter(|p| device.is_none_or(|d| p.device.eq_ignore_ascii_case(d)))
        .take(8)
        .map(|p| format!("{} ({})", p.display, p.id))
        .collect();
    if near.is_empty() {
        return Err(ChatError::NotFound(query.to_string()));
    }
    near.sort();
    let scope = match device {
        Some(d) => format!("known chats on {d}"),
        None => "known chats".to_string(),
    };
    Err(ChatError::NotFound(format!(
        "\"{query}\"; {scope}: {}",
        near.join(", ")
    )))
}
