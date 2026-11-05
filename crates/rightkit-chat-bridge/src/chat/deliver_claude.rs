//! Push a Chat message into a running Claude chat on this computer, and
//! listen for the chat's replies.
//!
//! Every Claude Code / Claude Desktop (Code tab) chat process publishes
//! `~/.claude/sessions/<pid>.json` (pid, sessionId, name, peerProtocol,
//! messagingSocketPath, procStart, pidDomain) and a 0600 key file
//! `~/.claude/sessions/<pid>.<hash>.key` holding `{peerToken, procStart,
//! pidDomain}`. The chat listens on a Unix stream socket (a named pipe on
//! Windows) and speaks newline-delimited JSON:
//!
//! Wire shape (captured from a real Claude Code 2.1.293 session on macOS). One
//! newline-terminated JSON line; the sender's identity lives inside the content
//! as a wrapper tag, not in top-level fields:
//!
//! `{"msgV":1,"msg_id":..,"type":"user","message":{"role":"user","content":
//! "<cross-session-message from=\"uds:<reply>\" from-session=\"..\"
//! from-name=\"<peer> via the host app\" from-mode=\"bypass\">\n<text>\n</cross-session-message>"},
//! "priority":"next","from":"uds:<reply>"}`
//!
//! On macOS/Linux no auth line is sent. On native Windows the auth frame
//! `{"type":"auth","token":<peerToken>}` goes first, using the key file token.
//! The whole payload is prepared before connecting (the chat wants it within
//! 30 s). Outcomes: an explicit chat verdict is Delivered, Queued (accepted or
//! held natively, consumption unknown) or Refused; a real client is normally
//! silent, so no control frame within 3 s, or a close without one, is only Sent
//! (transport ok, no verdict). The chat may answer with `{"type":"control","action":
//! "peer_message_status"|"peer_message_hold"|..}`. The key is read but never
//! logged or put in an error.
//!
//! Replies: the chat answers to `from`, a socket this process listens on
//! (`ReplyHub`). One socket per remote peer, so a reply knows who it is for.
//! Empty connections (a liveness probe) are tolerated. Both ends use the published
//! `rightkit-daemon` transport (`Listener`, `open_stream`): on Windows the reply
//! listener is one named pipe, `\\.\pipe\LOCAL\cc-msg-<fnv>`, whose DACL grants the
//! current user only, and every accept checks the peer is the current user. The
//! stream has no timeouts of its own, so a watcher thread per stage (connect 3 s by
//! the transport, write 3 s, ACK read 10 s, reply idle 5 s and 30 s in all) aborts
//! the connection when a bound passes; a write that stalls is reported by name. The
//! `pulse-chat-noreply` placeholder `from` is advertised only when no reply route can
//! be made (no hub, or the bind failed); the receipt then says replies are unavailable.
//! ACK readers stop after 10 s or 64 KiB; reply connections after 30 s, at most 64 at once.

use super::{ChatError, Envelope, LocalSession, Receipt, ReceiptState, Store, codex_hook};
#[cfg(windows)]
use rightkit_daemon::Listener;
#[cfg(any(unix, windows))]
use rightkit_daemon::{Accepted, Conn};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// The only `peerProtocol` this code speaks.
pub const PEER_PROTOCOL: u64 = 1;
/// How long to wait for the chat's accept / hold / refuse.
const ACK_WINDOW: Duration = Duration::from_secs(3);
/// How often the wait for a verdict looks for the chat's notice on the reply address, and
/// how long that notice may still take once the chat has closed the connection.
const STATUS_POLL: Duration = Duration::from_millis(50);
const STATUS_GRACE: Duration = Duration::from_millis(750);
/// Absolute limit on reading a chat's ACK frames, and the most bytes read.
const ACK_DEADLINE: Duration = Duration::from_secs(10);
const ACK_MAX_BYTES: usize = 64 * 1024;
/// Absolute limit on one reply connection, how many may be open at once.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const REPLY_DEADLINE: Duration = Duration::from_secs(30);
/// A reply connection that sends nothing for this long is cut.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const REPLY_IDLE: Duration = Duration::from_secs(5);
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const MAX_REPLY_CONNECTIONS: usize = 64;
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const MAX_REPLY_BYTES: usize = 1024 * 1024;
/// Hook route: how long a message is watched for its claim, the gap between checks, and how
/// recent the plugin's hook run must be for the session.
const HOOK_POLLS: u32 = 8;
const HOOK_POLL_GAP: Duration = Duration::from_millis(250);
const HOOK_SEEN_WITHIN: Duration = Duration::from_secs(6 * 3600);
/// How long `ReplyHub::stop` waits for its listener threads.
const STOP_JOIN: Duration = Duration::from_secs(2);
/// A per-peer reply socket nobody asked for in this long is closed (issue #501: one socket and one
/// accept thread per remote chat ever seen grew for the engine's whole life; 22 after 46 h). The
/// same 24 hours as the routes `hub::ROUTE_WINDOW_MS` restores on start, so a route a restart
/// would bring back is never closed early; the next message from that chat listens again.
#[cfg(unix)]
const REPLY_ROUTE_IDLE: Duration = Duration::from_secs(24 * 60 * 60);
/// Per-peer reply sockets closed as idle since the process started (`reply_route_counts`).
#[cfg(unix)]
static REPLY_ROUTES_CLOSED: AtomicUsize = AtomicUsize::new(0);

/// The few places this file touches the types `chat/mod.rs` owns. If those
/// types differ, adapt here only.
///
/// `chat/mod.rs` provides `Receipt::{delivered,held,refused,unsupported}`
/// and `ChatError::{SessionGone,AuthRejected,Io}`; the envelope's text is
/// `body`; `LocalSession::pid` is optional.
pub(super) mod shim {
    use super::*;

    use crate::chat::ReceiptState;

    fn receipt(state: ReceiptState, reason: &str) -> Receipt {
        Receipt {
            msg_id: String::new(),
            session: String::new(),
            state,
            detail: reason.to_string(),
        }
    }
    /// The chat confirmed it has the message.
    pub fn delivered(reason: &str) -> Receipt {
        receipt(ReceiptState::Delivered, reason)
    }
    /// The chat's native queue accepted it; whether it was consumed is unknown.
    pub fn queued(reason: &str) -> Receipt {
        receipt(ReceiptState::Queued, reason)
    }
    /// The transport succeeded; the chat gave no verdict.
    pub fn held(reason: &str) -> Receipt {
        receipt(ReceiptState::Held, reason)
    }
    pub fn sent(reason: &str) -> Receipt {
        receipt(ReceiptState::Sent, reason)
    }
    /// The dispatch may or may not have happened.
    pub fn unknown(reason: &str) -> Receipt {
        receipt(ReceiptState::Unknown, reason)
    }
    pub fn refused(reason: &str) -> Receipt {
        receipt(ReceiptState::Refused, reason)
    }
    pub fn unsupported(reason: &str) -> Receipt {
        receipt(ReceiptState::Unsupported, reason)
    }
    pub fn gone(reason: &str) -> ChatError {
        ChatError::SessionGone(reason.to_string())
    }
    pub fn auth_rejected(reason: &str) -> ChatError {
        ChatError::AuthRejected(reason.to_string())
    }
    pub fn io(reason: &str) -> ChatError {
        ChatError::Io(reason.to_string())
    }
    pub fn env_id(env: &Envelope) -> &str {
        &env.id
    }
    pub fn env_text(env: &Envelope) -> &str {
        &env.body
    }
    /// Names one remote chat; replies to it come back to `<device>:<session>`.
    pub fn env_peer_key(env: &Envelope) -> String {
        format!("{}:{}", env.from.device, env.from.session)
    }
    pub fn session_pid(session: &LocalSession) -> u32 {
        session.pid.unwrap_or(0)
    }
}

/// `~/.claude/sessions`, or `$CLAUDE_CONFIG_DIR/sessions` when that is set.
pub fn sessions_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("sessions"),
        None => rightkit_app_host::account_home()
            .join(".claude")
            .join("sessions"),
    }
}

/// A profile folder's name as people say it: `claude-b-home` is "Claude B".
fn profile_label(folder: &str) -> String {
    let base = folder.strip_suffix("-home").unwrap_or(folder);
    base.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The profile a Claude config folder belongs to: the profile's name for
/// `<home>/ClaudeProfiles/<name>/.claude` (or `claude-config`), the parent folder's name for a folder listed in
/// `claudeHomes`, and `None` for any other folder (the account's own `~/.claude`).
fn profile_of(config: &Path, account: &Path, listed: &[PathBuf]) -> Option<String> {
    let parent = config.parent()?;
    let in_profiles = parent.parent() == Some(account.join("ClaudeProfiles").as_path());
    if !in_profiles && !listed.iter().any(|l| l == config) {
        return None;
    }
    parent
        .file_name()
        .and_then(|n| n.to_str())
        .map(profile_label)
        .filter(|l| !l.is_empty())
}

/// Whether a folder is a Claude config: it has `sessions/` and one of `projects/`, `settings.json` or a
/// session record (`sessions/*.json`). Codex keeps `sessions/` too, with only dated subfolders.
fn is_claude_config(dir: &Path) -> bool {
    let sessions = dir.join("sessions");
    sessions.is_dir()
        && (dir.join("projects").is_dir()
            || dir.join("settings.json").is_file()
            || std::fs::read_dir(&sessions).is_ok_and(|mut l| {
                l.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json")))
            }))
}

/// Every Claude `sessions` folder the host app reads, each with its profile label: this
/// process's own (`sessions_dir`), the account's `~/.claude` when this process runs in a
/// profile, and, while `claudeProfiles` is on (the default), each
/// `<account home>/ClaudeProfiles/*/.claude` and `.../*/claude-config` plus the folders listed in `claudeHomes`.
/// Duplicates (the same folder reached twice) are listed once.
/// `sessions_dirs_now`, remembered for 30 s per (account, own config): the chat list is rebuilt every
/// few seconds and re-reading every profile folder each time was a large share of the Dell
/// engine's idle CPU. A new profile shows up within 30 s.
pub fn sessions_dirs() -> Vec<(PathBuf, Option<String>)> {
    type Key = (PathBuf, PathBuf);
    type Dirs = Vec<(PathBuf, Option<String>)>;
    static CACHE: std::sync::Mutex<Option<(Key, Instant, Dirs)>> = std::sync::Mutex::new(None);
    let key = (rightkit_app_host::account_home(), sessions_dir());
    if let Ok(cache) = CACHE.lock()
        && let Some((cached, at, dirs)) = cache.as_ref()
        && *cached == key
        && at.elapsed() < Duration::from_secs(30)
    {
        return dirs.clone();
    }
    let dirs = sessions_dirs_now();
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((key, Instant::now(), dirs.clone()));
    }
    dirs
}

fn sessions_dirs_now() -> Vec<(PathBuf, Option<String>)> {
    let own = sessions_dir();
    let account = rightkit_app_host::account_home();
    // An explicit `$CLAUDE_CONFIG_DIR` outside the account's profiles (a test fixture) names the one
    // Claude config this process works with. One inside `ClaudeProfiles` is a real second profile (the
    // Dell's Claude B), which must still see the account's other chats.
    if std::env::var_os("CLAUDE_CONFIG_DIR").is_some_and(|v| !v.is_empty())
        && !own.starts_with(account.join("ClaudeProfiles"))
    {
        return vec![(own, None)];
    }
    let mut listed: Vec<PathBuf> = Vec::new();
    let mut configs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = own.parent() {
        configs.push(parent.to_path_buf());
    }
    let store = Store::open_default().ok();
    if store.as_ref().is_none_or(Store::claude_profiles) {
        configs.push(account.join(".claude"));
        if let Ok(listing) = std::fs::read_dir(account.join("ClaudeProfiles")) {
            let mut found: Vec<PathBuf> = listing
                .flatten()
                // Found by scanning, not by name: any folder in a profile that is a Claude config
                // (`is_claude_config`) (`.claude` when HOME points at the profile, the Mac's Claude B;
                // `claude-config` when CLAUDE_CONFIG_DIR does, the Dell's).
                .filter_map(|e| std::fs::read_dir(e.path()).ok())
                .flat_map(|inner| inner.flatten().map(|c| c.path()))
                .filter(|p| is_claude_config(p))
                .collect();
            found.sort();
            configs.extend(found);
        }
        if let Some(store) = &store {
            listed = store.claude_homes();
            configs.extend(listed.iter().cloned());
        }
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for config in configs {
        let key = std::fs::canonicalize(&config).unwrap_or_else(|_| config.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let label = profile_of(&config, &account, &listed);
        out.push((config.join("sessions"), label));
    }
    // The Claude Desktop account-switcher slots (A/C, `docs/claude-account-switch.md`) run Claude
    // Code from Claude's default data folder or a slot's own folder, so a chat in a slot can have
    // its session record there and not in `~/.claude`. Each such folder is looked in for a
    // Claude config; the slot's name labels a chat found in its own folder.
    if store.as_ref().is_none_or(Store::claude_profiles) {
        for (root, label) in desktop_slot_roots(&account) {
            for config in claude_configs_in(&root) {
                let key = std::fs::canonicalize(&config).unwrap_or_else(|_| config.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                out.push((config.join("sessions"), label.clone()));
            }
        }
        // Chats found from their running process: the folder a live Claude's own
        // HOME / CLAUDE_CONFIG_DIR names, wherever that profile lives, with no list of
        // folders to keep. Folders above stay for chats that are not running.
        for config in running_claude_scan().configs {
            let key = std::fs::canonicalize(&config).unwrap_or_else(|_| config.clone());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let label = profile_of(&config, &account, &listed).or_else(|| scanned_label(&config));
            out.push((config.join("sessions"), label));
        }
    }
    out
}

/// A profile found only from its running process: named after the folder that holds its
/// config (`/Volumes/D/scratch/xprofile/.claude` is "Xprofile"); the account's own `~/.claude`
/// has no label.
fn scanned_label(config: &Path) -> Option<String> {
    let name = config.file_name()?.to_str()?;
    let folder = if matches!(name, ".claude" | "claude-config") {
        config.parent()?.file_name()?.to_str()?
    } else {
        name
    };
    Some(profile_label(folder)).filter(|l| !l.is_empty())
}

/// What the running Claude Code processes say about where their chats live.
#[derive(Clone, Debug, Default)]
pub struct RunningScan {
    /// The config folders (`<home>/.claude`, or a `CLAUDE_CONFIG_DIR`) that hold the session
    /// registry file of a running chat, read from that process's own environment.
    pub configs: Vec<PathBuf>,
    /// `(pid, why)` for each running chat whose registry the host app could not find or read: silence
    /// about these must never look like "no chat" (`<cli> chat status` prints them).
    pub unreadable: Vec<(u32, String)>,
}

/// How long one process scan serves every caller (the roster is read several times a second
/// while the hub runs; a scan reads the command line of every process and the environment of
/// each Claude one, measured on the Dell at 3 to 4 s of CPU per scan, #449). The scan only adds
/// profile folders outside the known ones, so a new one showing up to 30 s late costs little.
const SCAN_TTL: Duration = Duration::from_secs(30);
/// A process younger than this may not have written its registry file yet.
const SCAN_SETTLE_MS: u64 = 20_000;

/// Whether a process is a Claude Code CLI (the terminal chat, or the CLI Desktop hosts) and not
/// the Claude Desktop app itself, whose environment says nothing about its chats.
fn is_claude_code(exe: Option<&Path>, cmd: &[std::ffi::OsString]) -> bool {
    let stem = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    if exe.is_some_and(|e| rightkit_ai_accounts::sync::is_claude_exe(&e.to_string_lossy())) {
        return false;
    }
    if exe.is_some_and(|e| stem(e) == "claude") {
        return true;
    }
    // A shell running a script that merely mentions a Claude folder (Git Bash's `bash -c "source
    // …/claude-config/…/D--Claude-coderight/…"` on the Dell, pids 19628 and 31188) is not one: it
    // has no registry file and must not raise "a running Claude has a chat the host app cannot list".
    if exe.is_some_and(|e| {
        matches!(
            stem(e).as_str(),
            "bash" | "sh" | "zsh" | "fish" | "dash" | "cmd" | "powershell" | "pwsh"
        )
    }) {
        return false;
    }
    // The package folder `claude-code` is a whole path segment of the script being run, never a
    // piece of some longer name.
    let names_package = |a: &std::ffi::OsString| {
        a.to_string_lossy()
            .to_lowercase()
            .split(['/', '\\'])
            .any(|segment| segment == "claude-code")
    };
    // A native install's binary is named by version; its argv[0] is still `claude`. A Node
    // install runs `node …/@anthropic-ai/claude-code/cli.js`.
    cmd.first().is_some_and(|c| stem(Path::new(c)) == "claude")
        || cmd.iter().take(3).any(names_package)
}

/// Reads every running Claude Code process's HOME / CLAUDE_CONFIG_DIR (the same sysinfo
/// environment reader `claude_desktop::instances` uses: macOS KERN_PROCARGS2, Windows the
/// process's environment block) and finds the registry file `<config>/sessions/<pid>.json`
/// each one wrote. Cached for `SCAN_TTL`.
pub fn running_claude_scan() -> RunningScan {
    static CACHE: Mutex<Option<(Instant, RunningScan)>> = Mutex::new(None);
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, scan)) = cache.as_ref()
            && at.elapsed() < SCAN_TTL
        {
            return scan.clone();
        }
    }
    let scan = scan_running_claude();
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), scan.clone()));
    scan
}

fn scan_running_claude() -> RunningScan {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let Ok(mut system) = System::new() else {
        return RunningScan::default();
    };
    // Stage 1: the executable path only (cheap). Command lines (a remote memory read per
    // process) are read in stage 2 for the few that can be Claude Code: a path naming claude (a
    // native install's binary is named by version, under a `claude` folder) or a node/bun
    // runtime (the npm install).
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let own = std::process::id();
    let maybe: Vec<Pid> = system
        .processes()
        .iter()
        .filter(|(pid, p)| {
            pid.as_u32() != own
                && p.exe().is_some_and(|e| {
                    let lower = e.to_string_lossy().to_lowercase();
                    let stem = e
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    lower.contains("claude") || matches!(stem.as_str(), "node" | "bun")
                })
        })
        .map(|(pid, _)| *pid)
        .collect();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&maybe),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet),
    );
    let pids: Vec<Pid> = maybe
        .into_iter()
        .filter(|pid| {
            system
                .process(*pid)
                .is_some_and(|p| is_claude_code(p.exe(), p.cmd()))
        })
        .collect();
    let mut scan = RunningScan::default();
    if pids.is_empty() {
        return scan;
    }
    // Environments are read for the Claude processes only.
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
    );
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(0);
    for pid in pids {
        let Some(p) = system.process(pid) else {
            continue;
        };
        let pid = pid.as_u32();
        let mut env: HashMap<String, String> = HashMap::new();
        for var in p.environ() {
            let var = var.to_string_lossy();
            if let Some((k, v)) = var.split_once('=')
                && matches!(
                    k,
                    "CLAUDE_CONFIG_DIR" | "HOME" | "USERPROFILE" | "CFFIXED_USER_HOME"
                )
                && !v.is_empty()
            {
                env.insert(k.to_string(), v.to_string());
            }
        }
        if env.is_empty() {
            scan.unreadable.push((
                pid,
                "its environment (HOME, CLAUDE_CONFIG_DIR) cannot be read".to_string(),
            ));
            continue;
        }
        // CLAUDE_CONFIG_DIR wins when set; otherwise Claude uses `<home>/.claude`, and the
        // home may be HOME, USERPROFILE or (a Mac second profile) CFFIXED_USER_HOME.
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = env.get("CLAUDE_CONFIG_DIR") {
            candidates.push(PathBuf::from(dir));
        } else {
            for key in ["HOME", "USERPROFILE", "CFFIXED_USER_HOME"] {
                if let Some(home) = env.get(key) {
                    let dir = PathBuf::from(home).join(".claude");
                    if !candidates.contains(&dir) {
                        candidates.push(dir);
                    }
                }
            }
        }
        let file = format!("{pid}.json");
        if let Some(found) = candidates
            .iter()
            .find(|c| c.join("sessions").join(&file).is_file())
        {
            if !scan.configs.contains(found) {
                scan.configs.push(found.clone());
            }
            continue;
        }
        // No registry file for this process. A folder that exists but cannot be read is a
        // chat the host app cannot list; so is an interactive chat that has run long enough to have
        // written one. A one-shot run (`claude -p`), a helper subcommand or a fresh start is not.
        let unreadable_dir = candidates.iter().find(|c| {
            let sessions = c.join("sessions");
            sessions.exists() && std::fs::read_dir(&sessions).is_err()
        });
        if let Some(dir) = unreadable_dir {
            scan.unreadable.push((
                pid,
                format!(
                    "its session folder {} cannot be read",
                    dir.join("sessions").display()
                ),
            ));
            continue;
        }
        let args: Vec<String> = p
            .cmd()
            .iter()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let one_shot = args.iter().any(|a| {
            matches!(
                a.as_str(),
                "-p" | "--print" | "mcp" | "--version" | "-v" | "doctor" | "update" | "config"
            )
        });
        let settled = now_ms.saturating_sub(p.start_time().saturating_mul(1000)) > SCAN_SETTLE_MS;
        if !one_shot && settled {
            scan.unreadable.push((
                pid,
                format!(
                    "no session registry file {file} under {}",
                    candidates
                        .iter()
                        .map(|c| c.join("sessions").display().to_string())
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
            ));
        }
    }
    scan.configs.sort();
    scan.unreadable.sort();
    scan
}

/// Claude Desktop's data folders that can hold Claude Code session records: its default folder
/// (`~/Library/Application Support/Claude` on a Mac, `%APPDATA%\Claude` on Windows, no label) and
/// each account-switcher slot's own folder under the host app's state (`.../the host app/claude-desktop/<slot>`,
/// labelled with the slot's name). The same places `claude_desktop.rs` starts Desktop with.
fn desktop_slot_roots(account: &Path) -> Vec<(PathBuf, Option<String>)> {
    #[cfg(target_os = "macos")]
    let (main, slots) = {
        let support = account.join("Library").join("Application Support");
        (
            Some(support.join("Claude")),
            rightkit_app_host::state_root().map(|root| root.join("claude-desktop")),
        )
    };
    #[cfg(windows)]
    let (main, slots) = {
        let _ = account;
        // `home::env_var`, not the raw variables: from a Claude B shell they point into
        // ClaudeProfiles, which would list B's folder as Desktop's own.
        (
            rightkit_app_host::env_var("APPDATA").map(|a| PathBuf::from(a).join("Claude")),
            rightkit_app_host::state_root().map(|root| root.join("claude-desktop")),
        )
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let (main, slots): (Option<PathBuf>, Option<PathBuf>) = {
        let _ = account;
        (None, None)
    };
    let mut roots: Vec<(PathBuf, Option<String>)> = Vec::new();
    roots.extend(main.map(|m| (m, None)));
    if let Some(listing) = slots.and_then(|s| std::fs::read_dir(s).ok()) {
        let mut found: Vec<PathBuf> = listing
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        found.sort();
        for dir in found {
            let label = dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(profile_label)
                .filter(|l| !l.is_empty());
            roots.push((dir, label));
        }
    }
    roots
}

/// Every Claude Desktop data folder that can hold Code chat records, as
/// `(folder, profile label, is Desktop's default folder)`: the default folder, each
/// account-switcher slot's, and the data folder of each second profile under `ClaudeProfiles`
/// (found by scanning: `<profile>/Library/Application Support/Claude` on a Mac,
/// `<profile>/AppData/Roaming/Claude` on Windows, with the profile's own `home` folder tried
/// too). Only the default folder's Desktop takes the `claude://` link a wake uses.
pub fn desktop_chat_roots() -> Vec<(PathBuf, Option<String>, bool)> {
    let account = rightkit_app_host::account_home();
    let mut out: Vec<(PathBuf, Option<String>, bool)> = desktop_slot_roots(&account)
        .into_iter()
        .map(|(root, label)| {
            let main = label.is_none();
            (root, label, main)
        })
        .collect();
    if let Ok(listing) = std::fs::read_dir(account.join("ClaudeProfiles")) {
        let mut profiles: Vec<PathBuf> = listing.flatten().map(|e| e.path()).collect();
        profiles.sort();
        for profile in profiles.into_iter().filter(|p| p.is_dir()) {
            let label = profile
                .file_name()
                .and_then(|n| n.to_str())
                .map(profile_label)
                .filter(|l| !l.is_empty());
            for base in [profile.clone(), profile.join("home")] {
                for data in [
                    base.join("Library")
                        .join("Application Support")
                        .join("Claude"),
                    base.join("AppData").join("Roaming").join("Claude"),
                ] {
                    if data.join("claude-code-sessions").is_dir()
                        && !out.iter().any(|(r, _, _)| *r == data)
                    {
                        out.push((data, label.clone(), false));
                    }
                }
            }
        }
    }
    out
}

/// The Claude config folders (`is_claude_config`) in `root`: the folder itself, its `.claude` and
/// `claude-config`, and any folder directly inside it.
fn claude_configs_in(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = [
        root.to_path_buf(),
        root.join(".claude"),
        root.join("claude-config"),
    ]
    .into_iter()
    .filter(|p| is_claude_config(p))
    .collect();
    if let Ok(listing) = std::fs::read_dir(root) {
        let mut inner: Vec<PathBuf> = listing
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && is_claude_config(p) && !found.contains(p))
            .collect();
        inner.sort();
        found.extend(inner);
    }
    found
}

/// Is this process still running, and (where the key file records the start time)
/// is it the same process that wrote the files?
fn process_matches(pid: u32, recorded_start: Option<&str>, domain: Option<&str>) -> bool {
    let Ok(mut system) = sysinfo::System::new() else {
        return false;
    };
    let target = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    if system.process(target).is_none() {
        return false;
    }
    start_identity(pid, recorded_start, domain).unwrap_or(true)
}

/// Whether the running process `pid` started when the files say it did:
/// `Some(true)` it did, `Some(false)` another process has the pid, `None` the
/// start time was not recorded or can't be compared here.
pub(super) fn start_identity(
    pid: u32,
    recorded_start: Option<&str>,
    domain: Option<&str>,
) -> Option<bool> {
    start_check(pid, recorded_start, domain).map(|check| matches!(check, Ok(gap) if gap <= 2.0))
}

/// `start_identity`'s measurement: `None` when the file records nothing to compare (or another
/// platform's domain), `Some(Ok(gap))` the seconds between the recorded start and the live
/// process's, `Some(Err(why))` a start that could not be read or compared (not live).
pub(super) fn start_check(
    pid: u32,
    recorded_start: Option<&str>,
    domain: Option<&str>,
) -> Option<Result<f64, &'static str>> {
    #[cfg(any(target_os = "macos", windows))]
    {
        use rightkit_child_process::AdoptedIdentity;
        // Claude writes `procStart` in UTC (ctime shape) on macOS, and `pidDomain` as
        // `win32:<host>` on Windows.
        let ours = if cfg!(windows) {
            domain.is_none_or(|d| d == "windows" || d.starts_with("win32"))
        } else {
            domain.is_none_or(|d| d == "darwin")
        };
        if ours && let Some(recorded) = recorded_start.filter(|r| !r.is_empty()) {
            // A reused pid has another start time. An identity that can't be read or compared
            // is not live.
            let started = std::num::NonZeroU32::new(pid)
                .and_then(|pid| AdoptedIdentity::capture(pid).ok().flatten())
                .and_then(|identity| identity.start_time())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
            return Some(match (parse_start(recorded), started) {
                (Some(want), Some(have)) => Ok((want - have.as_secs_f64()).abs()),
                (None, _) => Err("the recorded start time could not be parsed"),
                (_, None) => Err("the live process's start time could not be read"),
            });
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (pid, recorded_start, domain);
    }
    None
}

struct SessionFiles {
    socket: String,
    #[cfg_attr(not(windows), allow(dead_code))]
    token: String,
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// A start time or domain as text, from a JSON string or integer.
pub(super) fn text_of(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .filter(|s| !s.is_empty())
}

/// What the roster saw of a chat: the delivery must find the same chat, started at
/// the same time, in the same pid domain.
struct Expected {
    session_id: String,
    start: Option<String>,
    domain: Option<String>,
}

fn expectation(session: &LocalSession) -> Expected {
    Expected {
        session_id: session.id.clone(),
        start: text_of(&session.raw["procStart"]),
        domain: text_of(&session.raw["pidDomain"]),
    }
}

/// The recorded process start in a key file (`procStart` on macOS, `procStartFt` on Windows).
fn key_start(key: &Value) -> Option<String> {
    text_of(&key["procStart"]).or_else(|| text_of(&key["procStartFt"]))
}

/// Why `open_session` refused. Only `KeyFileUnusable` may lead to the socket fallback.
enum OpenError {
    /// Any refusal that says the process or chat is not provably the listed one (different
    /// session id, start or domain; no registry file; no socket; no token): the refusal stands.
    Refused(ChatError),
    /// The registry file names this session and its start, but the key file is missing or
    /// unreadable, or belongs to another key generation. Carries the registry's socket.
    KeyFileUnusable { error: ChatError, socket: String },
}

/// The live messaging socket of a Claude chat whose key file could not be used, for a
/// delivery straight to it. Only for `OpenError::KeyFileUnusable`: never when the registry names
/// a different session for the pid, is missing, has no socket or a different start. Only off
/// Windows, where the socket takes the message without the key token that Windows checks. Only
/// while the `chatSocketFallback` switch is on (default on). The registry is read again here and
/// must still name this session for the pid, and the live process's start must be provably the
/// one the roster listed (`start_identity` must say `true`, so an unrecorded or unreadable start
/// is never taken; in practice that is the Mac). The log names the pid and why the key was
/// refused, with no message text and no token. `None` otherwise, and the refusal stands.
fn live_socket_fallback(
    session: &LocalSession,
    pid: u32,
    socket: &str,
    refusal: &str,
) -> Option<String> {
    if cfg!(windows) || session.kind != "claude" || socket.is_empty() {
        return None;
    }
    if !Store::open_default().ok()?.socket_fallback() {
        return None;
    }
    let start = text_of(&session.raw["procStart"]);
    let domain = text_of(&session.raw["pidDomain"]);
    // Re-confirm the registry now: a file for this pid that names this session, the same socket
    // and a start that does not differ. A different session id for the pid (after /clear or
    // /resume) or an unreadable registry never passes.
    let confirmed = sessions_dirs().into_iter().any(|(dir, _)| {
        read_json(&dir.join(format!("{pid}.json"))).is_some_and(|info| {
            info["sessionId"].as_str() == Some(session.id.as_str())
                && info["messagingSocketPath"].as_str() == Some(socket)
                && match (&start, text_of(&info["procStart"])) {
                    (Some(want), Some(have)) => *want == have,
                    _ => true,
                }
        })
    });
    if !confirmed {
        return None;
    }
    if start_identity(pid, start.as_deref(), domain.as_deref()) != Some(true) {
        return None;
    }
    super::sendlog::event(
        rightkit_logs::Level::Info,
        "chat.claude.socket_fallback",
        json!({
            "pid": pid,
            "refusal": refusal,
        }),
    );
    Some(socket.to_string())
}

/// Validate a session's files against what the roster listed and read what is
/// needed to talk to it. A registry or key file that now describes another session id,
/// start time or pid domain is refused, so a reused pid never receives the message.
fn open_session(pid: u32, expected: &Expected) -> Result<Result<SessionFiles, String>, OpenError> {
    let refused = OpenError::Refused;
    // The chat may live in another Claude profile: the folder whose registry file for
    // this pid names the expected session is the one; otherwise the first file found decides.
    let found: Vec<(PathBuf, Value)> = sessions_dirs()
        .into_iter()
        .filter_map(|(dir, _)| read_json(&dir.join(format!("{pid}.json"))).map(|info| (dir, info)))
        .collect();
    let (dir, info) = found
        .iter()
        .position(|(_, info)| info["sessionId"].as_str() == Some(expected.session_id.as_str()))
        .or(if found.is_empty() { None } else { Some(0) })
        .map(|i| found[i].clone())
        .ok_or_else(|| refused(shim::gone("This chat is no longer running.")))?;
    let changed = || {
        refused(shim::gone(
            "That chat's process now belongs to a different session.",
        ))
    };
    if info["sessionId"].as_str() != Some(expected.session_id.as_str()) {
        return Err(changed());
    }
    let info_start = text_of(&info["procStart"]);
    if let (Some(want), Some(have)) = (&expected.start, &info_start)
        && want != have
    {
        return Err(changed());
    }
    let protocol = info["peerProtocol"].as_u64().unwrap_or(0);
    if protocol != PEER_PROTOCOL {
        return Ok(Err(format!(
            "This chat speaks messaging protocol {protocol}; the app speaks {PEER_PROTOCOL}."
        )));
    }
    let socket = info["messagingSocketPath"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| refused(shim::gone("This chat has no messaging socket.")))?
        .to_string();
    // The key file name carries a hash the host app can't predict. Several generations can coexist
    // for one pid: take the one whose recorded start matches this session's, never just the
    // first filename.
    let start = expected.start.clone().or(info_start);
    let prefix = format!("{pid}.");
    let mut keys: Vec<(PathBuf, Value)> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".key"))
        })
        .filter_map(|p| read_json(&p).map(|k| (p, k)))
        .collect();
    if keys.is_empty() {
        return Err(OpenError::KeyFileUnusable {
            error: shim::gone("This chat's key file is missing or unreadable."),
            socket,
        });
    }
    let key = match &start {
        Some(start) => {
            keys.retain(|(_, k)| key_start(k).as_deref() == Some(start.as_str()));
            keys.into_iter().next()
        }
        None if keys.len() == 1 => keys.into_iter().next(),
        None => None,
    }
    .map(|(_, k)| k)
    .ok_or_else(|| OpenError::KeyFileUnusable {
        error: shim::gone("This chat's key file is for another generation of the process."),
        socket: socket.clone(),
    })?;
    let key_domain = text_of(&key["pidDomain"]);
    if let (Some(want), Some(have)) = (&expected.domain, &key_domain)
        && want != have
    {
        return Err(changed());
    }
    // With no start recorded in the registry, the key's own recorded start is the only
    // generation evidence: it must still match the live process (a reused pid does not).
    if start.is_none()
        && let Some(recorded) = key_start(&key)
        && start_identity(pid, Some(&recorded), key_domain.as_deref()) == Some(false)
    {
        return Err(changed());
    }
    let token = key["peerToken"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| refused(shim::gone("This chat's key file has no token.")))?
        .to_string();
    let domain = expected.domain.clone().or(key_domain);
    if !process_matches(pid, start.as_deref(), domain.as_deref()) {
        return Err(refused(shim::gone(
            "This chat has closed (its process is gone or was replaced).",
        )));
    }
    Ok(Ok(SessionFiles { socket, token }))
}

type Writer = Box<dyn Write + Send>;
type Reader = Box<dyn Read + Send>;

/// An open connection to a chat: its two halves and the handle that aborts it from another thread
/// (a blocked read or write then returns).
struct ChatStream {
    writer: Writer,
    reader: Reader,
    abort: Arc<dyn Fn() + Send + Sync>,
}

/// Opens the chat's socket (a named pipe on Windows) through the published transport
/// (`rightkit_daemon::open_stream`): the server is checked to run as this user before a byte is
/// written, and a busy pipe is retried for 3 s and an absent one for 1 s. The stream has no
/// timeouts; `watch` bounds each stage from outside.
#[cfg(any(unix, windows))]
fn connect(path: &str) -> std::io::Result<ChatStream> {
    let stream = rightkit_daemon::open_stream(path)?;
    let abort = stream.abort_handle();
    Ok(ChatStream {
        writer: stream.writer,
        reader: stream.reader,
        abort,
    })
}

#[cfg(not(any(unix, windows)))]
fn connect(_path: &str) -> std::io::Result<ChatStream> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no local messaging on this system",
    ))
}

/// What a `watch` thread and its stage share.
struct Watch {
    done: AtomicBool,
    fired: AtomicBool,
}

impl Watch {
    /// The stage ended by itself: the watcher stops without touching the connection.
    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
    }

    /// The watcher cut the connection because a bound passed.
    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

/// Bounds one stage of a connection from outside: when `deadline` passes, or, with `idle`, when
/// the instant behind it has not moved for that long, `abort` is called (which makes a blocked
/// read or write return) and `fired` says so. It ends when `finish` is called.
fn watch(
    abort: Arc<dyn Fn() + Send + Sync>,
    deadline: Instant,
    idle: Option<(Duration, Arc<Mutex<Instant>>)>,
) -> Arc<Watch> {
    let watch = Arc::new(Watch {
        done: AtomicBool::new(false),
        fired: AtomicBool::new(false),
    });
    let shared = Arc::clone(&watch);
    std::thread::spawn(move || {
        while !shared.done.load(Ordering::SeqCst) {
            let idle_over = idle.as_ref().is_some_and(|(limit, last)| {
                last.lock().unwrap_or_else(|e| e.into_inner()).elapsed() >= *limit
            });
            if Instant::now() >= deadline || idle_over {
                // A stage that finished a moment ago is left alone.
                if !shared.done.swap(true, Ordering::SeqCst) {
                    shared.fired.store(true, Ordering::SeqCst);
                    abort();
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    watch
}

/// A reader that notes when it last got bytes, for a `watch` idle bound.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
struct Tracked<R> {
    inner: R,
    last: Arc<Mutex<Instant>>,
}

impl<R: Read> Read for Tracked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        }
        Ok(n)
    }
}

enum Verdict {
    Delivered(String),
    /// Accepted by the chat's own queue; consumption is not confirmed.
    Queued(String),
    /// The chat holds the message for its user's approval.
    Held(String),
    Refused(String),
    AuthRejected(String),
    Protocol(String),
}

fn reason_of(frame: &Value) -> String {
    ["reason", "message", "detail", "error"]
        .iter()
        .find_map(|k| frame[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

/// What a hold means for the sender: the chat keeps the message until its user approves it.
fn held_reason(reason: &str) -> String {
    let base = "The receiving Claude chat is holding this message for its user's approval.";
    if reason.is_empty() {
        base.to_string()
    } else {
        format!("{base} {reason}")
    }
}

/// Read one control frame as an accept / hold / refuse, or None if it says nothing final.
fn classify(frame: &Value) -> Option<Verdict> {
    let kind = frame["type"].as_str().unwrap_or("");
    let action = frame["action"].as_str().unwrap_or("").to_ascii_lowercase();
    let reason = reason_of(frame);
    let lower = reason.to_ascii_lowercase();
    if kind == "error" || action.contains("auth") {
        if lower.contains("protocol") {
            return Some(Verdict::Protocol(reason));
        }
        return Some(Verdict::AuthRejected(reason));
    }
    if kind != "control" {
        return None;
    }
    if action == "peer_message_hold" || action.contains("hold") {
        return Some(Verdict::Held(held_reason(&reason)));
    }
    if action.contains("refus") || action.contains("reject") {
        return Some(Verdict::Refused(reason));
    }
    if action == "peer_message_status" {
        let status = ["status", "state", "result"]
            .iter()
            .find_map(|k| frame[*k].as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        return match status.as_str() {
            "delivered" | "injected" | "received" => Some(Verdict::Delivered(reason)),
            "accepted" | "ok" | "queued" => Some(Verdict::Queued(reason)),
            "held" | "hold" => Some(Verdict::Held(held_reason(&reason))),
            "refused" | "rejected" | "declined" | "denied" | "blocked" => {
                Some(Verdict::Refused(reason))
            }
            _ => None,
        };
    }
    None
}

/// The longest text `wrap_content` accepts: the envelope body limit plus room for the header
/// line `agent_text` puts before it.
const MAX_WRAPPED_TEXT_BYTES: usize = super::envelope::MAX_BODY_BYTES + 16 * 1024;

const TAG_OPEN: &str = "<cross-session-message";
const TAG_CLOSE: &str = "</cross-session-message>";

/// An attribute value that can't break out of its quotes.
fn attr_safe(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '<' | '>' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect()
}

/// Unicode format characters and the line and paragraph separators, which Claude
/// strips from a sender name.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// The sender name as Claude itself would write it: no control or format characters,
/// trimmed, 64 characters at most. A longer chat name is cut here so "via the host app" stays.
/// (Claude rebuilds the wrapper from what it parsed and drops the message unless the
/// result is the same text, so a name it would rewrite loses the message.)
pub(super) fn sender_label(name: &str) -> String {
    const MAX: usize = 64;
    let suffix = format!(" via {}", super::brand::product());
    let suffix = suffix.as_str();
    let clean: String = attr_safe(name)
        .chars()
        .filter(|&c| !c.is_control() && !is_format_char(c))
        .collect();
    let clean = clean.trim();
    let room = MAX.saturating_sub(suffix.chars().count());
    let label = if clean.chars().count() <= room {
        format!("{clean}{suffix}")
    } else {
        let head: String = clean.chars().take(room - 1).collect();
        format!("{}…{suffix}", head.trim_end())
    };
    label.trim().to_string()
}

/// The name a Claude chat is shown for a sender: `"<Harness> chat <chat> on <device>"` when the
/// message came from another computer, `"<Harness> chat <chat>"` when it came from this one
/// (the owner 2026-10-18, an upstream issue: same-device needs no device; cross-device names harness and
/// machine). The chat name is cut first to fit the 64 characters left after " via the host app"; the
/// harness and device are never cut. An unknown harness reads `"Chat <chat>"`.
pub(super) fn sender_name(origin: &super::envelope::Origin) -> String {
    let max = 64usize.saturating_sub(" via ".len() + super::brand::product().len());
    let clean = |s: &str| -> String {
        attr_safe(s)
            .chars()
            .filter(|&c| !c.is_control() && !is_format_char(c))
            .collect::<String>()
            .trim()
            .to_string()
    };
    let head = if origin.harness.is_empty() {
        "Chat".to_string()
    } else {
        format!("{} chat", clean(&origin.harness))
    };
    let tail = match &origin.machine {
        None => String::new(),
        Some(machine) => {
            let machine = clean(machine);
            let machine = if machine.is_empty() {
                "unknown computer".to_string()
            } else {
                machine
            };
            format!(" on {machine}")
        }
    };
    let chat = clean(&origin.chat);
    let chat = if chat.is_empty() {
        "unknown".to_string()
    } else {
        chat
    };
    let room = max.saturating_sub(head.chars().count() + 1 + tail.chars().count());
    let chat = if chat.chars().count() <= room || room == 0 {
        chat
    } else {
        let cut: String = chat.chars().take(room.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    };
    format!("{head} {chat}{tail}")
}

/// `text` with every wrapper tag inside it defused: a `<` whose following letters, ignoring
/// case and anything between them, spell the tag or its closing form. Claude rewrites such
/// a closing tag itself and then drops the message for differing from what was sent.
fn defuse_tags(text: &str) -> String {
    const CLOSING: &str = "/cross-session-message";
    let mut out = String::with_capacity(text.len());
    for (at, c) in text.char_indices() {
        if c == '<' {
            let ahead: String = text[at + 1..]
                .chars()
                .take(96)
                .filter(|&c| c.is_ascii_alphanumeric() || c == '/' || c == '-' || c == '_')
                .map(|c| {
                    if c == '_' {
                        '-'
                    } else {
                        c.to_ascii_lowercase()
                    }
                })
                .collect();
            if ahead.starts_with(CLOSING) || ahead.starts_with(&CLOSING[1..]) {
                out.push_str("&lt;");
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// The content a chat sees: the text inside the wrapper tag the real client uses, with
/// any wrapper tag inside the text neutralised. The sender attributes are labels the
/// sending side chose.
///
/// Claude (2.1.295) takes the wrapper only in its own exact form: these four attributes
/// in this order and no other, a `from-mode` it knows (`bypass` or `prompting`) that
/// matches the chat's own class, and a name it would not rewrite. Anything else is
/// parked for an approval and then dropped without a word to the sender, which loses the
/// message while the receipt says Sent. The owner's chats bypass permissions, so that is
/// the class; "via the host app" in the name is what marks the text as forwarded.
///
/// Text longer than [`MAX_WRAPPED_TEXT_BYTES`] is refused before anything is built: an
/// envelope body is at most 64 KiB and the header line adds well under 16 KiB, so a longer
/// text did not come from `agent_text` and would only grow the frame the chat has to parse.
pub(super) fn wrap_content(
    from: &str,
    session: &str,
    name: &str,
    text: &str,
) -> Result<String, ChatError> {
    if text.len() > MAX_WRAPPED_TEXT_BYTES {
        return Err(ChatError::Invalid(format!(
            "the message is {} bytes, too large to hand to the chat whole (limit {MAX_WRAPPED_TEXT_BYTES}); it was not cut and not delivered",
            text.len()
        )));
    }
    let text = defuse_tags(text);
    Ok(format!(
        "{TAG_OPEN} from=\"{}\" from-session=\"{}\" from-name=\"{}\" from-mode=\"bypass\">\n{text}\n{TAG_CLOSE}",
        attr_safe(from),
        attr_safe(session),
        attr_safe(name),
    ))
}

/// The text a Claude chat reads: the message header first (who sent it, that it is another
/// agent chat and not the user, how to reply), with the full the host app Chat introduction on the
/// chat's first message, then the body. A chat that sees only the bare text can take it for
/// something its user pasted and hand the user a reply to carry back.
pub(super) fn agent_text(env: &Envelope) -> String {
    let id: String = shim::env_id(env)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    if super::envelope::is_chatgpt(env) {
        return format!(
            "{}\n{}",
            super::envelope::chatgpt_header(&id),
            shim::env_text(env)
        );
    }
    let origin = super::origin_of(env);
    let first = super::first_contact_pending(&env.to.session);
    format!(
        "{}\n{}",
        super::envelope::message_header(&origin, &id, first),
        shim::env_text(env)
    )
}

/// Attributes and body of a `<cross-session-message ...>` wrapper, if `content` is one.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn parse_wrapper(content: &str) -> Option<(HashMap<String, String>, String)> {
    let rest = content.trim().strip_prefix(TAG_OPEN)?;
    if !rest.starts_with(char::is_whitespace) && !rest.starts_with('>') {
        return None;
    }
    let mut attrs = HashMap::new();
    let mut chars = rest.char_indices().peekable();
    let body_start = loop {
        let (i, c) = chars.next()?;
        if c == '>' {
            break i + 1;
        }
        if c.is_whitespace() {
            continue;
        }
        let mut key = String::from(c);
        loop {
            match chars.next()? {
                (_, '=') => break,
                (_, k) => key.push(k),
            }
        }
        if chars.next()?.1 != '"' {
            return None;
        }
        let mut value = String::new();
        loop {
            match chars.next()? {
                (_, '"') => break,
                (_, v) => value.push(v),
            }
        }
        attrs.insert(key.trim().to_string(), value);
    };
    let inner = &rest[body_start..];
    let end = inner.rfind(TAG_CLOSE).unwrap_or(inner.len());
    Some((attrs, inner[..end].trim().to_string()))
}

/// The user-message frame for a Claude peer: the wrapped text, the message id and, when
/// there is one, the reply address. `peer_key` is `<device>:<session>` of the sender.
pub(super) fn user_frame(
    env: &Envelope,
    peer_key: &str,
    reply_from: Option<&str>,
) -> Result<Value, ChatError> {
    let short: String = peer_key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    let sender = sender_label(&sender_name(&super::origin_of(env)));
    let session_id = format!("{}-{short}", super::brand::brand().session_prefix);
    let content = wrap_content(
        reply_from.unwrap_or(""),
        &session_id,
        &sender,
        &agent_text(env),
    )?;
    let mut user = json!({
        "msgV": 1,
        "msg_id": shim::env_id(env),
        "type": "user",
        "message": {"role": "user", "content": content},
        "priority": "next",
    });
    if let Some(address) = reply_from {
        user["from"] = json!(address);
    }
    Ok(user)
}

/// One frame as written to the chat's socket: compact JSON and a newline.
pub(super) fn frame_line(frame: &Value) -> Result<String, ChatError> {
    let mut line = serde_json::to_string(frame).map_err(|e| shim::io(&e.to_string()))?;
    line.push('\n');
    Ok(line)
}

/// Whether `deliver_via` leaves the message for the host app plugin's hook instead of the
/// chat's messaging socket: the opt-in setting (`Store::claude_hook_delivery`) is on, the
/// plugin's hook ran for this session within six hours, and the roster reports the chat
/// `busy` (a turn is running, so a hook will fire). The roster records nothing else that
/// tells a running turn from an idle chat, so an idle or unknown chat keeps the socket.
pub(super) fn hook_route_wanted(session: &LocalSession) -> bool {
    session.kind == "claude"
        && session.status == "busy"
        && Store::open_default().is_ok_and(|store| store.claude_hook_delivery())
        && codex_hook::claude_hook_seen_recently(&session.id, HOOK_SEEN_WITHIN)
}

/// The hook route; `None` when it does not apply or the inbox could not be written, in
/// which case nothing was left for the hook and the socket route runs.
fn deliver_by_hook(session: &LocalSession, env: &Envelope) -> Option<Receipt> {
    if !hook_route_wanted(session) {
        return None;
    }
    let id = shim::env_id(env);
    codex_hook::enqueue_claude(&session.id, id, &agent_text(env)).ok()?;
    for _ in 0..HOOK_POLLS {
        std::thread::sleep(HOOK_POLL_GAP);
        if !codex_hook::pending_claude(&session.id, id) {
            return Some(shim::delivered(
                "The Claude chat read the message through the app's Claude plugin hook.",
            ));
        }
    }
    Some(shim::queued(
        "Waiting in the app for this Claude chat; it reads the message after its next tool call, when its turn ends, or with its next prompt.",
    ))
}

/// How long a woken Desktop chat has to start its CLI (Desktop starts it with `--resume` when the
/// chat's page is shown; `claude_chats::FIRST_WAIT` allows a first start as long). This is the
/// hub's wait: the hub answers its caller `queued` after `HUB_VERDICT_WAIT` and finishes the wake
/// here, on its own worker, keeping the message in the chat's inbox if it fails or stops (F6).
const WAKE_WAIT: Duration = Duration::from_secs(60);
/// The wait when no hub runs (the CLI delivers in its own process and cannot go on after it
/// answers): inside the 60 s a linked computer's ssh command gives a post (`links::REMOTE_TIMEOUT`).
const WAKE_WAIT_DIRECT: Duration = Duration::from_secs(40);
const _: () =
    assert!(WAKE_WAIT_DIRECT.as_millis() + 5_000 <= super::links::REMOTE_TIMEOUT.as_millis());

/// A message for an idle Claude Desktop chat: the chat has no
/// process, so Desktop's own entry is used to start it. `claude://code/continue?session=local_<id>`
/// (the link `claude_chats::reopen` uses) shows the chat, and Desktop starts its CLI with
/// `--resume` without sending it anything. Once its registry file appears the message goes the
/// normal way, and the receipt says the chat was woken. A chat in another Desktop profile cannot
/// be reached by that link: it is `unsupported`, so the message waits in the chat's inbox.
fn wake_idle_desktop(
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Result<Receipt, ChatError> {
    let store = Store::open_default().ok();
    let woken = if store.as_ref().is_some_and(|s| s.claude_wake_idle()) {
        let wait = if store.as_ref().is_some_and(super::control::hub_running) {
            WAKE_WAIT
        } else {
            WAKE_WAIT_DIRECT
        };
        wake_idle_desktop_for(session, env, reply_socket, wait)
    } else {
        Ok(shim::held(
            "Waking idle Claude chats is off; the message waits for this chat's next turn.",
        ))
    };
    // A wake that did not deliver leaves the message where the target's next turn reads it: the
    // Claude plugin's hook inbox, not only the generic Chat inbox the caller falls back to (F10).
    // `unknown` is not kept here: the message may have reached the chat.
    let not_delivered = match &woken {
        Ok(receipt) => matches!(
            receipt.state,
            ReceiptState::Unsupported | ReceiptState::Refused | ReceiptState::Held
        ),
        Err(_) => true,
    };
    if not_delivered && codex_hook::claude_hook_seen_recently(&session.id, HOOK_SEEN_WITHIN) {
        let _ = codex_hook::enqueue_claude(&session.id, shim::env_id(env), &agent_text(env));
    }
    woken
}

/// The wake itself: open the chat, wait up to `wait` for its CLI, then deliver the normal way.
fn wake_idle_desktop_for(
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
    wait: Duration,
) -> Result<Receipt, ChatError> {
    if session.raw["desktopWakeable"].as_bool() != Some(true) {
        return Ok(shim::unsupported(
            "This idle chat is in another Claude Desktop profile, which the app cannot wake; the message waits in its Chat inbox.",
        ));
    }
    let Some(host) = session.raw["hostSessionId"].as_str() else {
        return Ok(shim::unsupported(
            "This idle chat has no Claude Desktop record.",
        ));
    };
    if !crate::claude_chats::open_link(host) {
        return Ok(shim::unsupported(
            "Claude Desktop did not take the link that wakes this idle chat; the message waits in its Chat inbox.",
        ));
    }
    let until = Instant::now() + wait;
    let mut last: Option<ChatError> = None;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
        let alive = super::roster::live_pids();
        let woken = sessions_dirs().into_iter().find_map(|(dir, _)| {
            super::roster::claude_sessions_in(&dir, &alive, true)
                .into_iter()
                .find(|s| s.raw["hostSessionId"].as_str() == Some(host))
        });
        let Some(woken) = woken else { continue };
        match deliver_via(&woken, env, reply_socket) {
            Ok(receipt) => {
                return Ok(Receipt {
                    detail: format!(
                        "Woke the idle Claude Desktop chat first. {}",
                        receipt.detail
                    )
                    .trim()
                    .to_string(),
                    ..receipt
                });
            }
            // Its registry file can be up a moment before its key file and socket.
            Err(e @ ChatError::SessionGone(_)) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    match last {
        Some(e) => Err(e),
        None => Ok(shim::unsupported(
            "Woke the idle Claude Desktop chat, but it did not start in time; the message waits in its Chat inbox.",
        )),
    }
}

/// Deliver one envelope to a chat on this computer and report what the chat
/// did with it. `Ok(Receipt::unsupported)` when the chat speaks another
/// protocol version; `Err` when it is gone or turned the host app away.
pub fn deliver(session: &LocalSession, env: &Envelope) -> Result<Receipt, ChatError> {
    deliver_via(session, env, None)
}

/// `deliver`, with `reply_socket` (the sending chat's own messaging socket)
/// as the address the chat replies to instead of a reply-hub socket.
pub fn deliver_via(
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Result<Receipt, ChatError> {
    if session.raw["desktopIdle"].as_bool() == Some(true) {
        return wake_idle_desktop(session, env, reply_socket);
    }
    if let Some(receipt) = deliver_by_hook(session, env) {
        return Ok(receipt);
    }
    let pid = shim::session_pid(session);
    let files = match open_session(pid, &expectation(session)) {
        Ok(Ok(files)) => files,
        Ok(Err(reason)) => return Ok(shim::unsupported(&reason)),
        // The registry names this session but its key file is unusable: deliver
        // straight to its messaging socket when that is allowed.
        Err(OpenError::Refused(error)) => return Err(error),
        Err(OpenError::KeyFileUnusable { error, socket }) => {
            match live_socket_fallback(session, pid, &socket, &error.to_string()) {
                Some(socket) => SessionFiles {
                    socket,
                    token: String::new(),
                },
                None => return Err(error),
            }
        }
    };
    #[cfg(unix)]
    note_chat_socket(&files.socket);
    let peer_key = shim::env_peer_key(env);
    let peer_key = peer_key.as_str();
    let route = match reply_socket {
        Some(address) => ReplyRoute::Listening(address.to_string()),
        None => {
            note_last_peer(&session.id, peer_key);
            // A Desktop chat signs its replies with its Desktop record id.
            if let Some(host) = session.raw["hostSessionId"].as_str() {
                note_last_peer(host, peer_key);
            }
            reply_route(peer_key)
        }
    };
    // A frame without a reply address is dropped by the chat without a word. When no
    // reply route exists, a stable placeholder is advertised so the message is still
    // deliverable; the receipt says replies are unavailable, and a chat that answers
    // the placeholder reaches nobody.
    let mut reply_note: Option<String> = None;
    let reply_from = Some(match route {
        ReplyRoute::Listening(address) => format!("uds:{address}"),
        ReplyRoute::Unavailable(why) => {
            eprintln!(
                "{}: reply route unavailable ({why}); advertising a placeholder",
                super::brand::brand().chat_marker()
            );
            reply_note = Some(format!("Replies are unavailable ({why})."));
            if cfg!(windows) {
                format!(
                    r"uds:\\.\pipe\{}-noreply",
                    super::brand::brand().reply_dir_name
                )
            } else {
                format!(
                    "uds:/tmp/{}-noreply.sock",
                    super::brand::brand().reply_dir_name
                )
            }
        }
    });
    let user = user_frame(env, peer_key, reply_from.as_deref())?;
    let mut frames = Vec::new();
    if cfg!(windows) {
        frames.push(json!({"type": "auth", "token": files.token}));
    }
    frames.push(user);
    let mut lines = Vec::new();
    for frame in frames {
        lines.push(frame_line(&frame)?);
    }
    let ChatStream {
        mut writer,
        reader,
        abort,
    } = connect(&files.socket).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::PermissionDenied => {
            shim::gone("This chat's messaging socket is not accepting connections.")
        }
        std::io::ErrorKind::TimedOut => shim::io(&format!("Couldn't reach the chat: {e}")),
        _ => shim::io(&format!("Couldn't reach the chat: {}", e.kind())),
    })?;

    // The write stage has 3 s. The stream has no timeout of its own, so a watcher aborts it when
    // the bound passes, and the failed write is then reported as the chat not taking the message.
    let writing = watch(Arc::clone(&abort), Instant::now() + ACK_WINDOW, None);
    let sent: Result<(), ChatError> = (|| {
        for line in lines {
            writer
                .write_all(line.as_bytes())
                .and_then(|_| writer.flush())
                .map_err(|e| {
                    if writing.fired() {
                        shim::io(&format!("The chat did not take the message: {e}"))
                    } else {
                        shim::gone("The chat closed the connection while the app was sending.")
                    }
                })?;
        }
        Ok(())
    })();
    writing.finish();
    sent?;

    let (tx, rx) = mpsc::channel::<Value>();
    // Ends at the chat's close, after 10 s or after 64 KiB, whichever comes first (a watcher
    // aborts the stream at the deadline: a blocked read cannot time out by itself).
    let reading = watch(abort, Instant::now() + ACK_DEADLINE, None);
    std::thread::spawn(move || {
        read_lines(
            reader,
            Instant::now() + ACK_DEADLINE,
            ACK_MAX_BYTES,
            |line| match serde_json::from_str::<Value>(line.trim()) {
                Ok(frame) => tx.send(frame).is_ok(),
                Err(_) => true,
            },
        );
        reading.finish();
    });
    let posted = Instant::now();
    let deadline = posted + ACK_WINDOW;
    let mut heard_anything = false;
    let finish = |receipt: Receipt| -> Receipt {
        match &reply_note {
            Some(note) => Receipt {
                detail: format!("{} {note}", receipt.detail).trim().to_string(),
                ..receipt
            },
            None => receipt,
        }
    };
    loop {
        // Claude answers on the reply address, not on this connection: nothing when it
        // takes the message, a notice within a moment when it holds or refuses it.
        if let Some(receipt) = status_receipt(shim::env_id(env)) {
            return Ok(finish(receipt));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(finish(shim::sent(unconfirmed(reply_socket))));
        }
        match rx.recv_timeout(left.min(STATUS_POLL)) {
            Ok(frame) => {
                heard_anything = true;
                match classify(&frame) {
                    Some(Verdict::Delivered(r)) => return Ok(finish(shim::delivered(&r))),
                    Some(Verdict::Queued(r)) => return Ok(finish(shim::queued(&r))),
                    Some(Verdict::Held(r)) => return Ok(finish(shim::held(&r))),
                    Some(Verdict::Refused(r)) => {
                        let r = if r.is_empty() {
                            "refused by that session's settings".to_string()
                        } else {
                            r
                        };
                        return Ok(finish(shim::refused(&r)));
                    }
                    Some(Verdict::Protocol(r)) => return Ok(shim::unsupported(&r)),
                    Some(Verdict::AuthRejected(r)) => return Err(shim::auth_rejected(&r)),
                    None => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let grace = (posted + STATUS_GRACE).min(deadline);
                while Instant::now() < grace {
                    if let Some(receipt) = status_receipt(shim::env_id(env)) {
                        return Ok(finish(receipt));
                    }
                    std::thread::sleep(STATUS_POLL);
                }
                return if heard_anything || !cfg!(windows) {
                    Ok(finish(shim::sent(unconfirmed(reply_socket))))
                } else {
                    Err(shim::auth_rejected(
                        "The chat closed the connection right after the handshake.",
                    ))
                };
            }
        }
    }
}

/// Why a `sent` receipt is not a confirmation. With the sending chat's own socket as the
/// reply address, Claude's hold / refuse notice goes to that chat, not to the host app, so a held
/// message looks the same as a taken one here.
fn unconfirmed(reply_socket: Option<&str>) -> &'static str {
    if reply_socket.is_some() {
        "Sent; delivery is not confirmed. Claude reports a hold or refusal to the sending chat, not to the app, so a message the receiving chat holds for its user's approval (for example in another permission mode) looks the same as one it took."
    } else {
        "Sent; the chat gave no verdict (silence is normal), so delivery is not confirmed."
    }
}

/// Lines from `reader` until `deadline`, end of input, an error, `cap` bytes, or
/// `on_line` returning false, read with the published sidecar line reader. The deadline is
/// checked between chunks; a read blocked on a silent peer is ended from outside (`watch`).
fn read_lines(
    reader: impl Read,
    deadline: Instant,
    cap: usize,
    mut on_line: impl FnMut(&str) -> bool,
) {
    use rightkit_sidecar::{ReadLimits, ReadOptions, read_line_bytes_observed};
    let mut reader = std::io::BufReader::new(reader);
    let mut buffer: Vec<u8> = Vec::new();
    let options = ReadOptions {
        limits: ReadLimits {
            deadline: Some(deadline),
            ..ReadLimits::default()
        },
        ..ReadOptions::default()
    };
    let mut total = 0usize;
    while total < cap {
        // The most one more line may be: what is left of the byte cap.
        let mut taken = 0usize;
        let line =
            read_line_bytes_observed(&mut reader, &mut buffer, cap - total, options, &mut |l| {
                taken = l.raw_len;
            });
        total = total.saturating_add(taken);
        match line {
            Ok(Some(bytes)) => {
                if !on_line(&String::from_utf8_lossy(&bytes)) {
                    return;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
}

// ---- the chat's own notice about a message ---------------------------------------

/// What chats said about messages posted to them: (message id, status, reason), newest
/// last. Claude sends this notice to the reply address, never on the connection the
/// message came in on: `held` (parked for its owner's approval), `denied`, `expired`,
/// `refused`, `dropped`, or `delivered` once a held one is let through. A message it
/// simply takes gets no notice.
static STATUSES: Mutex<Vec<(String, String, String)>> = Mutex::new(Vec::new());
const STATUSES_KEPT: usize = 256;

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn record_status(msg_id: &str, status: &str, reason: &str) {
    let mut all = STATUSES.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(id, _, _)| id != msg_id);
    if all.len() >= STATUSES_KEPT {
        all.remove(0);
    }
    let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    all.push((msg_id.to_string(), status.to_string(), reason));
}

/// The receipt a chat's notice about `msg_id` amounts to, once one has arrived. Anything
/// but `delivered` means the chat does not have the message.
fn status_receipt(msg_id: &str) -> Option<Receipt> {
    let all = STATUSES.lock().unwrap_or_else(|e| e.into_inner());
    let (_, status, reason) = all.iter().find(|(id, _, _)| id == msg_id)?;
    let detail = match status.as_str() {
        "delivered" => return Some(shim::delivered("The chat took the message.")),
        "held" => return Some(shim::held(&held_reason(reason))),
        other => format!("Not delivered: the chat reported it {other}. {reason}"),
    };
    Some(shim::refused(detail.trim()))
}

// ---- replies from chats ----------------------------------------------------------

/// The remote peer whose message was last posted to each chat here (chat id, peer key).
/// A reply that arrives on a pipe shared by every peer is matched to its peer with it.
static LAST_PEER: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn note_last_peer(chat: &str, peer_key: &str) {
    let mut all = LAST_PEER.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(id, _)| id != chat);
    if all.len() >= STATUSES_KEPT {
        all.remove(0);
    }
    all.push((chat.to_string(), peer_key.to_string()));
}

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn last_peer(chat: &str) -> Option<String> {
    let all = LAST_PEER.lock().unwrap_or_else(|e| e.into_inner());
    all.iter()
        .find(|(id, _)| id == chat)
        .map(|(_, peer)| peer.clone())
}

/// A chat's reply, arrived on one remote peer's reply socket.
#[derive(Clone, Debug)]
pub struct ReplyMessage {
    /// The remote peer this reply goes to (the `peer_key` the address was made for).
    pub peer_key: String,
    pub from_session_id: String,
    pub from_name: String,
    pub text: String,
    /// The frame's own message id.
    pub msg_id: String,
    /// The id of the message being answered, when the frame names one (`in_reply_to`,
    /// `reply_to` or `replyTo`; Claude's peer protocol is not known to send any).
    pub in_reply_to: Option<String>,
}

type OnReply = Arc<dyn Fn(ReplyMessage) + Send + Sync>;
type IsKnown = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Listens on one Unix socket (a named pipe on Windows) per remote peer and
/// turns frames from known local chats into `ReplyMessage`s.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
pub struct ReplyHub {
    on_reply: OnReply,
    is_known: IsKnown,
    stop: Arc<AtomicBool>,
    paths: Mutex<HashMap<String, String>>,
    /// Per peer key: when its route was last asked for, its listener and that listener's own
    /// stop flag, so an idle route can be closed alone (`close_idle_routes`).
    #[cfg(unix)]
    routes: Mutex<HashMap<String, RouteListener>>,
    /// The listeners this hub bound, woken at `stop` so their blocked accept returns.
    #[cfg(any(unix, windows))]
    listeners: Mutex<Vec<Arc<Listener>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// The store of Chat folder this hub serves, for receipts from a chat's own notices.
    /// None for a hub made without one (no receipts are recorded then).
    store: Option<Store>,
}

/// The Unix reply listener: an owner-only socket that is accepted from with a bounded wait, so
/// closing a route never depends on the socket file still existing (#570, C2: the older
/// `rightkit_daemon::Listener::accept` blocks until a connection arrives, and the only way to wake it
/// was to connect to its path, which fails once `/tmp/cc-socks` was cleaned up and left the accept
/// thread, and the socket it holds, alive). Same rules as the published listener: a path that is
/// not our own socket is never replaced, a dead socket of ours is, the mode is 0600, and the peer's
/// user is checked before a byte is read (`verify_peer_is_current_user`).
#[cfg(unix)]
struct Listener {
    listener: std::os::unix::net::UnixListener,
    path: PathBuf,
}

/// How long one `accept` waits before it returns so the caller can look at its stop flags.
#[cfg(unix)]
const ACCEPT_POLL: Duration = Duration::from_millis(500);

#[cfg(unix)]
impl Listener {
    fn bind(endpoint: &str, _max_instances: u32) -> std::io::Result<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        let path = PathBuf::from(endpoint);
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let me = unsafe { libc::geteuid() };
            if meta.uid() != me || !meta.file_type().is_socket() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "endpoint path is occupied by something that is not our socket",
                ));
            }
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "a live host already serves this endpoint",
                ));
            }
            let _ = std::fs::remove_file(&path);
        }
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path })
    }

    /// Waits up to `ACCEPT_POLL` for a connection. No connection in that time is
    /// `Accepted::Vanished` (nothing to do), so the caller's loop checks its flags and calls again.
    fn accept(&self) -> std::io::Result<Accepted> {
        use std::os::fd::AsRawFd;
        let mut fd = libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `fd` is one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut fd, 1, ACCEPT_POLL.as_millis() as libc::c_int) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::Interrupted {
                Ok(Accepted::Vanished)
            } else {
                Err(error)
            };
        }
        let stream = match self.listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(Accepted::Vanished);
            }
            Err(error) => return Err(error),
        };
        // The accepted stream is used with blocking reads (the watcher cuts it by `shutdown`).
        stream.set_nonblocking(false)?;
        match rightkit_daemon::verify_peer_is_current_user(&stream) {
            Ok(peer) => {
                let reader = stream.try_clone()?;
                let writer = stream.try_clone()?;
                Ok(Accepted::Conn(Conn {
                    peer,
                    reader: Box::new(reader),
                    writer: Box::new(writer),
                    // Not used by the reply handler.
                    peer_closed: Arc::new(|| false),
                    shutdown: Arc::new(move || {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                    }),
                }))
            }
            Err(rightkit_daemon::PeerError::Query(_)) => Ok(Accepted::Vanished),
            Err(error) => Ok(Accepted::Rejected(error.to_string())),
        }
    }

    /// Pokes a waiting accept so it returns at once; the bounded wait covers a missing socket file.
    fn wake(&self) {
        let _ = std::os::unix::net::UnixStream::connect(&self.path);
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One per-peer reply listener: when its route was last asked for, the flag that ends its accept
/// thread, and the listener (woken so a blocked accept sees the flag).
#[cfg(unix)]
struct RouteListener {
    used: Instant,
    closed: Arc<AtomicBool>,
    listener: Arc<Listener>,
}

/// Reply routes this process holds now (open per-peer sockets on Unix, routes on Windows) and the
/// per-peer sockets closed as idle since start. Counts only, for the engine's memory series.
pub fn reply_route_counts() -> (usize, usize) {
    let Some(hub) = current_hub() else {
        return (0, closed_routes());
    };
    let open = hub.paths.lock().map(|p| p.len()).unwrap_or(0);
    (open, closed_routes())
}

#[cfg(unix)]
fn closed_routes() -> usize {
    REPLY_ROUTES_CLOSED.load(Ordering::Relaxed)
}

#[cfg(not(unix))]
fn closed_routes() -> usize {
    0
}

/// Where a reply to a Chat message will arrive, or why it can't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyRoute {
    /// A socket (named pipe on Windows) this process listens on.
    Listening(String),
    /// No reply route could be made; the reason.
    Unavailable(String),
}

/// Open reply connections across every hub, and the guard that counts one.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
static OPEN_REPLIES: AtomicUsize = AtomicUsize::new(0);

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
struct ConnectionSlot;

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
impl ConnectionSlot {
    /// A slot, or None (logged) when 64 connections are already open.
    fn take() -> Option<ConnectionSlot> {
        let taken = OPEN_REPLIES
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_REPLY_CONNECTIONS).then_some(n + 1)
            })
            .is_ok();
        if taken {
            Some(ConnectionSlot)
        } else {
            eprintln!(
                "{}: too many open reply connections; refusing one",
                super::brand::brand().chat_marker()
            );
            None
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        OPEN_REPLIES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Wait up to `limit` for the threads to end; one still running is left behind.
fn join_bounded(handles: Vec<std::thread::JoinHandle<()>>, limit: Duration) {
    let deadline = Instant::now() + limit;
    for handle in handles {
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
}

/// Which listener generation owns each Unix socket path, so a stopped listener never
/// unlinks the socket a newer one bound at the same path.
#[cfg(unix)]
static LISTENERS: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());
#[cfg(unix)]
static LISTENER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

static HUB: OnceLock<Mutex<Option<Arc<ReplyHub>>>> = OnceLock::new();

fn hub_slot() -> &'static Mutex<Option<Arc<ReplyHub>>> {
    HUB.get_or_init(|| Mutex::new(None))
}

/// The hub `deliver` takes reply addresses from. The hub sets it while sharing runs.
pub fn set_reply_hub(hub: Option<Arc<ReplyHub>>) {
    let old = match hub_slot().lock() {
        Ok(mut slot) => std::mem::replace(&mut *slot, hub),
        Err(_) => return,
    };
    // Stopped outside the lock: it waits for the old listeners' threads.
    if let Some(old) = old {
        old.stop();
    }
}

fn current_hub() -> Option<Arc<ReplyHub>> {
    hub_slot().lock().ok().and_then(|s| s.clone())
}

fn fnv(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The hub's one reply pipe on Windows, the same for every remote peer. Claude sends its
/// notice about a message (held, refused, dropped) only to a pipe named like its own
/// (`cc-msg-<32 hex>`, in the same session-local `LOCAL\` namespace) that a registry
/// entry names exactly, and a process has one entry, so there cannot be a pipe per peer:
/// a chat's answer is matched to its peer from the last message it was sent.
#[cfg(windows)]
pub fn reply_pipe_name(_peer_key: &str) -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!(
        r"\\.\pipe\LOCAL\cc-msg-{}{}",
        fnv(&format!(
            "{}-hub-reply:{user}",
            super::brand::brand().session_prefix
        )),
        fnv(&format!(
            "{user}:{}-hub-reply",
            super::brand::brand().session_prefix
        ))
    )
}

/// The address a reply from `peer_key` goes to, listening on it first; None
/// when no reply hub is running or this system can't listen.
pub fn reply_address(peer_key: &str) -> Option<String> {
    current_hub()?.address_for(peer_key)
}

/// `reply_address` that says why there is no route.
pub fn reply_route(peer_key: &str) -> ReplyRoute {
    match current_hub() {
        Some(hub) => hub.route_for(peer_key),
        None => ReplyRoute::Unavailable("no reply hub is running".to_string()),
    }
}

/// The folder the chats' own messaging sockets live in, learnt from the last chat a
/// message was posted to.
#[cfg(unix)]
static CHAT_SOCKET_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Remember where `socket` (a chat's messaging socket) lives.
#[cfg(unix)]
fn note_chat_socket(socket: &str) {
    if let Some(dir) = Path::new(socket).parent().filter(|d| d.is_dir()) {
        *CHAT_SOCKET_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
    }
}

/// The host app's own private per-user folder for reply sockets.
#[cfg(unix)]
fn private_socket_dir() -> PathBuf {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!(
        "/tmp/{}-{uid}",
        super::brand::brand().reply_dir_name
    ))
}

#[cfg(not(unix))]
fn private_socket_dir() -> PathBuf {
    std::env::temp_dir().join(&super::brand::brand().reply_dir_name)
}

/// Where the reply socket for one remote peer lives (Unix socket paths are limited to
/// about 100 bytes, so the name is short). Claude sends its notice about a message
/// (held, refused, dropped) only to a reply address inside its own socket folder, and
/// only to a socket named like this one, so the socket goes beside the chats' sockets
/// when that folder exists; `ReplyHub::route_for` falls back to the host app's private folder
/// when it can't listen there.
pub fn reply_socket_path(peer_key: &str) -> PathBuf {
    #[cfg(unix)]
    let base = {
        let learnt = CHAT_SOCKET_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let usual = PathBuf::from("/tmp/cc-socks");
        match learnt {
            Some(dir) => dir,
            None if usual.is_dir() => usual,
            None => private_socket_dir(),
        }
    };
    #[cfg(not(unix))]
    let base = private_socket_dir();
    base.join(format!("{}.sock", fnv(peer_key)))
}

impl ReplyHub {
    /// `is_known` says whether a `from_session_id` is a chat on this computer.
    pub fn new(on_reply: OnReply, is_known: IsKnown) -> Arc<ReplyHub> {
        Self::build(None, on_reply, is_known)
    }

    /// `new`, with the store whose receipts a chat's own notices should update.
    pub fn new_with_store(store: Store, on_reply: OnReply, is_known: IsKnown) -> Arc<ReplyHub> {
        Self::build(Some(store), on_reply, is_known)
    }

    fn build(store: Option<Store>, on_reply: OnReply, is_known: IsKnown) -> Arc<ReplyHub> {
        Arc::new(ReplyHub {
            on_reply,
            is_known,
            stop: Arc::new(AtomicBool::new(false)),
            paths: Mutex::new(HashMap::new()),
            #[cfg(unix)]
            routes: Mutex::new(HashMap::new()),
            #[cfg(any(unix, windows))]
            listeners: Mutex::new(Vec::new()),
            threads: Mutex::new(Vec::new()),
            store,
        })
    }

    /// Stop listening and wait up to two seconds for the listener threads, which
    /// remove their own sockets (only if still theirs) as they end.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // A blocked accept returns when its own listener is poked.
        #[cfg(any(unix, windows))]
        {
            let listeners = self
                .listeners
                .lock()
                .map(|mut l| std::mem::take(&mut *l))
                .unwrap_or_default();
            for listener in listeners {
                listener.wake();
            }
        }
        let handles = self
            .threads
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default();
        join_bounded(handles, STOP_JOIN);
    }

    /// The socket path replies from this peer's messages should go to, listening
    /// on it first. None when no reply route can be made (see `route_for`).
    pub fn address_for(self: &Arc<Self>, peer_key: &str) -> Option<String> {
        match self.route_for(peer_key) {
            ReplyRoute::Listening(address) => Some(address),
            ReplyRoute::Unavailable(_) => None,
        }
    }

    /// `address_for`, saying why when the hub can't listen.
    pub fn route_for(self: &Arc<Self>, peer_key: &str) -> ReplyRoute {
        if self.stop.load(Ordering::Relaxed) {
            return ReplyRoute::Unavailable("the reply hub is stopped".to_string());
        }
        #[cfg(unix)]
        {
            let Ok(mut paths) = self.paths.lock() else {
                return ReplyRoute::Unavailable("the reply hub is unusable".to_string());
            };
            let now = Instant::now();
            self.close_idle_routes(&mut paths, now);
            if let Some(path) = paths.get(peer_key) {
                if let Some(route) = self
                    .routes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(peer_key)
                {
                    route.used = now;
                }
                return ReplyRoute::Listening(path.clone());
            }
            let mut path = reply_socket_path(peer_key);
            let closed = Arc::new(AtomicBool::new(false));
            let listener = match self.listen(
                peer_key.to_string(),
                &path.to_string_lossy(),
                Arc::clone(&closed),
            ) {
                Ok(listener) => listener,
                Err(_) => {
                    // Not beside the chats' sockets (no such folder, or the path is too long):
                    // The host app's own folder still takes replies, though not Claude's notices.
                    path = private_socket_dir().join(format!("{}.sock", fnv(peer_key)));
                    match self.listen(
                        peer_key.to_string(),
                        &path.to_string_lossy(),
                        Arc::clone(&closed),
                    ) {
                        Ok(listener) => listener,
                        Err(e) => {
                            return ReplyRoute::Unavailable(format!(
                                "couldn't listen: {}",
                                e.kind()
                            ));
                        }
                    }
                }
            };
            self.routes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    peer_key.to_string(),
                    RouteListener {
                        used: now,
                        closed,
                        listener,
                    },
                );
            let text = path.to_string_lossy().into_owned();
            paths.insert(peer_key.to_string(), text.clone());
            ReplyRoute::Listening(text)
        }
        #[cfg(windows)]
        {
            let Ok(mut paths) = self.paths.lock() else {
                return ReplyRoute::Unavailable("the reply hub is unusable".to_string());
            };
            if let Some(name) = paths.get(peer_key) {
                return ReplyRoute::Listening(name.clone());
            }
            // One pipe serves every peer, so it is opened once; a frame on it carries no
            // peer of its own (see `handle_line`).
            let name = reply_pipe_name(peer_key);
            if !paths.values().any(|open| *open == name)
                && let Err(e) = self.listen(String::new(), &name, Arc::new(AtomicBool::new(false)))
            {
                return ReplyRoute::Unavailable(format!("couldn't listen: {}", e.kind()));
            }
            paths.insert(peer_key.to_string(), name.clone());
            ReplyRoute::Listening(name)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = peer_key;
            ReplyRoute::Unavailable("this system can't listen".to_string())
        }
    }

    /// Listens on `endpoint` (a socket path; a named pipe on Windows) through the published
    /// `Listener`: owner-only (socket mode 0600, a current-user-only pipe DACL), the first
    /// instance of the name, remote clients refused, and the peer's user checked on every accept.
    /// A thread accepts until `stop`, which pokes the listener so its blocked accept returns.
    /// `closed` ends this one listener's accept loop (an idle route, `close_idle_routes`) without
    /// stopping the hub; the listener is returned so it can be woken.
    #[cfg(any(unix, windows))]
    fn listen(
        self: &Arc<Self>,
        peer_key: String,
        endpoint: &str,
        closed: Arc<AtomicBool>,
    ) -> std::io::Result<Arc<Listener>> {
        // Only the host app's own folder is made and locked down here; the chats' folder is theirs.
        #[cfg(unix)]
        if let Some(dir) = Path::new(endpoint)
            .parent()
            .filter(|d| *d == private_socket_dir())
        {
            rightkit_fs::ensure_private_dir(dir)?;
        }
        // The registry lock is held from claiming the path to binding it, so an older
        // listener's cleanup never sees the new socket as its own.
        #[cfg(unix)]
        let generation = LISTENER_GENERATION.fetch_add(1, Ordering::Relaxed);
        let listener = {
            #[cfg(unix)]
            let mut owners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
            // A socket an earlier listener of this process still holds is replaced, as it always was.
            #[cfg(unix)]
            if owners.iter().any(|(p, _)| p == Path::new(endpoint)) {
                let _ = std::fs::remove_file(endpoint);
            }
            let listener = Arc::new(Listener::bind(endpoint, MAX_REPLY_CONNECTIONS as u32 + 1)?);
            #[cfg(unix)]
            {
                owners.retain(|(p, _)| p != Path::new(endpoint));
                owners.push((PathBuf::from(endpoint), generation));
            }
            listener
        };
        let hub = Arc::clone(self);
        let accepting = Arc::clone(&listener);
        #[cfg(unix)]
        let path = PathBuf::from(endpoint);
        let handle = std::thread::spawn(move || {
            while !hub.stop.load(Ordering::Relaxed) && !closed.load(Ordering::Relaxed) {
                match accepting.accept() {
                    Ok(Accepted::Conn(conn)) => {
                        let Some(slot) = ConnectionSlot::take() else {
                            (conn.shutdown)();
                            continue;
                        };
                        let hub = Arc::clone(&hub);
                        let key = peer_key.clone();
                        std::thread::spawn(move || {
                            let _slot = slot;
                            hub.handle(conn, &key);
                        });
                    }
                    // A peer that is not this user, or that could not be identified, was closed
                    // unread; this loop has no event sink for it. One that left before it could
                    // be identified (a probe, the stop's wake-up) is nothing.
                    Ok(Accepted::Rejected(_) | Accepted::Unidentified(_) | Accepted::Vanished) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(500)),
                }
            }
            // Unlink only a socket this generation still owns.
            #[cfg(unix)]
            {
                let mut owners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
                if owners.iter().any(|(p, g)| *p == path && *g == generation) {
                    owners.retain(|(p, _)| *p != path);
                    accepting.cleanup();
                }
            }
        });
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(Arc::clone(&listener));
        }
        if let Ok(mut threads) = self.threads.lock() {
            // Threads of closed routes that have ended are let go here, so the list does not
            // keep a handle per route ever opened.
            threads.retain(|t| !t.is_finished());
            threads.push(handle);
        }
        Ok(listener)
    }

    /// Closes the per-peer reply sockets whose route nobody asked for in `REPLY_ROUTE_IDLE`
    /// (issue #501): each accept thread is flagged and woken, ends, and unlinks its socket if it
    /// still owns it. Called with `paths` held, so no lookup can hand out a route being closed.
    #[cfg(unix)]
    fn close_idle_routes(&self, paths: &mut HashMap<String, String>, now: Instant) {
        let idle: Vec<RouteListener> = {
            let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<String> = routes
                .iter()
                .filter(|(_, r)| now.saturating_duration_since(r.used) >= REPLY_ROUTE_IDLE)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter()
                .filter_map(|k| {
                    paths.remove(k);
                    routes.remove(k)
                })
                .collect()
        };
        if idle.is_empty() {
            return;
        }
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.retain(|l| !idle.iter().any(|r| Arc::ptr_eq(l, &r.listener)));
        }
        for route in &idle {
            route.closed.store(true, Ordering::Relaxed);
            route.listener.wake();
        }
        let total = REPLY_ROUTES_CLOSED.fetch_add(idle.len(), Ordering::Relaxed) + idle.len();
        eprintln!(
            "{}: closed {} idle reply sockets ({} open, {} closed since start)",
            super::brand::brand().chat_marker(),
            idle.len(),
            paths.len(),
            total
        );
    }

    /// One accepted connection. The published stream has no timeouts, so a watcher cuts it after
    /// `REPLY_IDLE` without bytes or `REPLY_DEADLINE` in all.
    #[cfg(any(unix, windows))]
    fn handle(&self, conn: Conn, peer_key: &str) {
        let last = Arc::new(Mutex::new(Instant::now()));
        let watching = watch(
            Arc::clone(&conn.shutdown),
            Instant::now() + REPLY_DEADLINE,
            Some((REPLY_IDLE, Arc::clone(&last))),
        );
        self.handle_lines(
            Tracked {
                inner: conn.reader,
                last,
            },
            peer_key,
        );
        watching.finish();
        (conn.shutdown)();
    }

    /// Frames from one connection, as `ReplyMessage`s for known local chats.
    #[cfg(any(unix, windows))]
    fn handle_lines(&self, reader: impl Read, peer_key: &str) {
        let deadline = Instant::now() + REPLY_DEADLINE;
        read_lines(reader, deadline, MAX_REPLY_BYTES, |line| {
            self.handle_line(line, peer_key);
            !self.stop.load(Ordering::Relaxed)
        });
    }

    /// One frame, as a `ReplyMessage` when it is from a known local chat.
    #[cfg(any(unix, windows))]
    fn handle_line(&self, line: &str, peer_key: &str) {
        let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        // A chat's notice about a message the host app posted to it ("expired" with the detail
        // "refused" is how Claude spells a refusal).
        if frame["type"] == "control" && frame["action"] == "peer_message_status" {
            if let (Some(id), Some(status)) =
                (frame["orig_msg_id"].as_str(), frame["status"].as_str())
            {
                let status = if status == "expired" && frame["status_detail"] == "refused" {
                    "refused"
                } else {
                    status
                };
                record_status(id, status, frame["reason"].as_str().unwrap_or(""));
                if let Some(store) = &self.store {
                    crate::chat::receipts::from_chat_status(
                        store,
                        id,
                        status,
                        frame["reason"].as_str().unwrap_or(""),
                    );
                }
            }
            return;
        }
        // The auth frame, if any, is ignored: what counts is that the sender is a known local chat.
        if frame["type"] != "user" {
            return;
        }
        let Some(raw) = content_text(&frame["message"]["content"]) else {
            return;
        };
        let (attrs, text) = parse_wrapper(&raw).unwrap_or_default();
        let text = if attrs.is_empty() { raw } else { text };
        let pick = |attr: &str, field: &str| {
            attrs
                .get(attr)
                .filter(|v| !v.is_empty())
                .cloned()
                .or_else(|| frame[field].as_str().map(str::to_string))
                .unwrap_or_default()
        };
        let from_session_id = pick("from-session", "from_session_id");
        if from_session_id.is_empty() || !(self.is_known)(&from_session_id) || text.is_empty() {
            return;
        }
        // A shared pipe names no peer: the reply goes to whoever last wrote to this chat.
        let peer_key = if peer_key.is_empty() {
            last_peer(&from_session_id).unwrap_or_default()
        } else {
            peer_key.to_string()
        };
        (self.on_reply)(ReplyMessage {
            peer_key,
            from_session_id,
            from_name: pick("from-name", "from_name"),
            text,
            msg_id: frame["msg_id"].as_str().unwrap_or("").to_string(),
            in_reply_to: ["in_reply_to", "reply_to", "replyTo"]
                .iter()
                .find_map(|k| frame[*k].as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_string),
        });
    }
}

/// A message's content as plain text: a string, or the text blocks of an array.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn content_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

// ---- recorded process start -------------------------------------------------------

/// A recorded `procStart` as epoch seconds. Claude's Windows form isn't fixed
/// here, so this takes a number (seconds, milliseconds, microseconds, FILETIME
/// ticks or nanoseconds, told apart by size), an ISO-8601 time, or the
/// `Www Mon DD HH:MM:SS YYYY` form macOS records. Anything else is unknown.
#[cfg(any(target_os = "macos", windows))]
fn parse_start(text: &str) -> Option<f64> {
    let t = text.trim();
    if !t.is_ascii() {
        return None;
    }
    if let Ok(v) = t.parse::<f64>() {
        return Some(if v >= 5e17 {
            v / 1e9
        } else if v >= 1e16 {
            (v - 116_444_736_000_000_000.0) / 1e7
        } else if v >= 1e14 {
            v / 1e6
        } else if v >= 1e11 {
            v / 1e3
        } else {
            v
        });
    }
    parse_iso(t).or_else(|| parse_ctime(t))
}

#[cfg(any(target_os = "macos", windows))]
fn epoch(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    rightkit_time::days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + s
}

#[cfg(any(target_os = "macos", windows))]
fn parse_iso(t: &str) -> Option<f64> {
    let b = t.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || (b[10] != b'T' && b[10] != b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| t.get(range)?.parse::<i64>().ok();
    let base = epoch(
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
    );
    let mut rest = &t[19..];
    let mut fraction = 0.0;
    if let Some(after) = rest.strip_prefix('.') {
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        fraction = format!("0.{}", &after[..digits]).parse().ok()?;
        rest = &after[digits..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        zone if zone.starts_with(['+', '-']) => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let digits: String = zone[1..].chars().filter(char::is_ascii_digit).collect();
            if digits.len() < 2 {
                return None;
            }
            let hours: i64 = digits[..2].parse().ok()?;
            let minutes: i64 = digits.get(2..4).map_or(Some(0), |m| m.parse().ok())?;
            sign * (hours * 3600 + minutes * 60)
        }
        _ => return None,
    };
    Some((base - offset) as f64 + fraction)
}

#[cfg(any(target_os = "macos", windows))]
fn parse_ctime(t: &str) -> Option<f64> {
    let parts: Vec<&str> = t.split_whitespace().collect();
    let [_, month, day, clock, year] = parts[..] else {
        return None;
    };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = i64::try_from(months.iter().position(|m| *m == month)?).ok()? + 1;
    let mut hms = clock.split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi, s) = (hms.next()??, hms.next()??, hms.next()??);
    Some(epoch(year.parse().ok()?, month, day.parse().ok()?, h, mi, s) as f64)
}
