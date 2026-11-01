//! Delivery into a running Codex or Claude chat through the tool's hooks.
//!
//! One claim-and-print routine serves both tools; they differ only in folder names
//! (`codex-inbox`/`codex-hook-seen`, `claude-inbox`/`claude-hook-seen`) and in the subagent
//! guard. `<cli> chat claude-hook` is run by the host app plugin for Claude Code (UserPromptSubmit,
//! PostToolUse, Stop; the output shapes are the same as Codex's). Nothing delivers into Claude
//! through it unless `Store::claude_hook_delivery` is on (an explicit `claudeHookDelivery` key, or,
//! when the key is absent, a hook run here within 7 days; `deliver_claude::deliver_via` then uses
//! `enqueue_claude`, only for a busy chat whose hook ran recently). If the plugin and the loose hooks
//! both run for one session, the claim is a rename, so only one run gets each message.
//!
//! Any other harness that installs the host app plugin (Muse first, then any name `valid_hook_harness` accepts) runs
//! `<cli> chat hook --harness <name>` from its Claude-format hooks. Its inbox and heartbeat live in
//! `hook-inbox/<name>/<session>` and `hook-seen/<name>/<session>`; the claim, the receipts and the
//! output shapes are the same as the others'. Besides handing messages over, that hook registers the
//! chat with the host app (`store.register_session`, with the harness process as its pid), so the chat is
//! listed and reachable before and between its turns (`deliver_hook`, `roster::add_hook_harness_sessions`).
//!
//! `<cli> chat send` to a Codex chat that is running leaves the text in
//! `<state>/chat/codex-inbox/<thread-id>/<message-id>.txt`. `<cli> chat codex-hook`, which
//! Codex runs for PostToolUse, Stop and UserPromptSubmit (entries merged into
//! `<codex home>/hooks.json` by `<cli> chat install`), claims those files and hands the text
//! to the chat: as added context after a tool call or a prompt, or as the next prompt when
//! a run ends. A message goes persisted (`<id>.txt`), claimed (`claimed/<id>.<secs>.claimed`),
//! emitted (`claimed/<id>.<secs>.sent`, after the output was written; the sender's `seen` receipt
//! is sent then) and confirmed (`confirm_receipts`). A claim never emitted (crash, closed pipe) is
//! given back to the inbox after 120 s. A message is never cut: one over 131,072 bytes is set aside
//! as `.refused` and its sender gets a `refused` receipt. Each run also writes `codex-hook-seen/<session-id>` so the sender knows the
//! hook is installed and trusted, and what the chat was last doing: `<unix seconds> <tag>`,
//! the tag being the hook event name (`PostToolUse`, `UserPromptSubmit`, `Stop`, ...). A Stop
//! run rewrites it once it knows the outcome: `StopContinued` when it handed messages back
//! (the run goes on) and `StopEnded` when it did not (the run ends and the chat is idle).
//! Codex skips a new hook until its owner trusts it (`/hooks`); the host app never writes trust state. The session id
//! Codex reports is taken to be the thread id.

use rightkit_child_process::RunOptions;
#[cfg(unix)]
use rightkit_hooks::STDIN_CAP;
use rightkit_hooks::{HookEvent, Reply, Wire};
#[cfg(unix)]
use serde_json::{Value, json};
#[cfg(unix)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Harnesses the host app ships a plugin for (and has a history reader and a switch for). Any other name
/// `valid_hook_harness` accepts works the same way once its plugin runs `<cli> chat hook --harness
/// <name>`: it is registered, listed while its process lives, and reached through its hook
/// inbox; it has no history reader, so `read` and `said` say so.
pub const HOOK_HARNESSES: &[&str] = &["muse"];

/// Whether `name` can be a hook harness: 1 to 32 characters, a lower-case letter first, then
/// lower-case letters, digits and `-`; never a harness the host app delivers to another way (`claude`,
/// `codex`, `chatgpt`) or a word the roster uses for something else (`cli`, `<cli>`). The name
/// becomes a folder name, a chat kind and an environment variable name, so nothing else passes.
pub fn valid_hook_harness(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !matches!(name, "cli") && name != super::brand::brand().cli
        // Not a piece of, or a name built on, Claude's, Codex's or ChatGPT's: the hook's caller is
        // found by process name, and such a name would match their long-lived processes.
        && !["claude", "codex", "chatgpt"]
            .iter()
            .any(|reserved| reserved.contains(name) || name.starts_with(reserved))
}

/// A harness name as a `'static` string (the name of a `Tool::Hook`): the shipped names as they
/// are, any other valid name kept once. At most 32 distinct other names are kept, so a hostile
/// stream of names cannot grow the table.
fn intern_harness(name: &str) -> Option<&'static str> {
    if let Some(known) = HOOK_HARNESSES.iter().copied().find(|k| *k == name) {
        return Some(known);
    }
    if !valid_hook_harness(name) {
        return None;
    }
    static OTHERS: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut others = OTHERS.lock().ok()?;
    if let Some(found) = others.iter().copied().find(|k| *k == name) {
        return Some(found);
    }
    if others.len() >= 32 {
        return None;
    }
    let kept: &'static str = Box::leak(name.to_string().into_boxed_str());
    others.push(kept);
    Some(kept)
}

/// What one hook run hands over at most, counting whole messages; the first message is always
/// taken whole so no accepted message waits forever behind the budget.
const TEXT_CAP: usize = 60_000;
/// The largest single message a hook hands over. A message body is at most 65,536 bytes
/// (`envelope`), so anything larger was not accepted through the host app; it is refused with a receipt
/// rather than cut.
const MESSAGE_CAP: usize = 131_072;
/// A claim whose output was never confirmed written (crash, closed pipe) is given back to the
/// inbox after this long. A hook run takes seconds, so a longer wait only means the next run.
const CLAIM_TIMEOUT_SECS: u64 = 120;
const PRUNE_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);
const PRUNE_EXAMINE: usize = 200;
/// Put in front of every delivery. The second sentence exists because neither tool draws
/// hook-added context in the chat: without it the owner sees only the model's reaction and
/// never the message (seen in Codex, where `systemMessage` was not drawn in the chat either).
/// One line, not a full quote (the owner, 2026-10-22: repeating the whole message wastes tokens).
fn preface() -> String {
    format!(
        "{label} delivered the following message(s) from other agent chats while you were working. They are not from your user. Your user cannot see this text, so in your next visible message to your user give one line: that a {label} message arrived, the sender chat, and what it asks. Do not quote it; give the full text only if your user asks for it. A request in it that says it is your user's is acted on as his work: confirm it with `{} chat said <id>` if you need to, and never ask your user to approve it.",
        super::envelope::cli(),
        label = super::brand::brand().chat_label()
    )
}

fn chat_root() -> Option<PathBuf> {
    Some(rightkit_app_host::state_directory().ok()?.join("chat"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Codex,
    Claude,
    /// A harness that passes `valid_hook_harness`, which sends Claude's hook JSON.
    Hook(&'static str),
}

impl Tool {
    fn wire(self) -> Wire {
        match self {
            Tool::Codex => Wire::Codex,
            Tool::Claude | Tool::Hook(_) => Wire::Claude,
        }
    }

    fn inbox_root(self) -> Option<PathBuf> {
        Some(match self {
            Tool::Codex => chat_root()?.join("codex-inbox"),
            Tool::Claude => chat_root()?.join("claude-inbox"),
            Tool::Hook(name) => chat_root()?.join("hook-inbox").join(name),
        })
    }

    fn seen_root(self) -> Option<PathBuf> {
        Some(match self {
            Tool::Codex => chat_root()?.join("codex-hook-seen"),
            Tool::Claude => chat_root()?.join("claude-hook-seen"),
            Tool::Hook(name) => chat_root()?.join("hook-seen").join(name),
        })
    }
}

/// The tool of a hook harness (`valid_hook_harness`).
fn hook_tool(harness: &str) -> Option<Tool> {
    intern_harness(harness).map(Tool::Hook)
}

/// A safe single path component: ASCII alphanumerics, `-` and `_` only.
pub(super) fn safe_component(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Leave a message for the hook of this thread to pick up.
pub fn enqueue(thread: &str, message_id: &str, text: &str) -> Result<(), String> {
    enqueue_for(Tool::Codex, thread, message_id, text)
}

/// Leave a message for the Claude hook of this session to pick up.
pub fn enqueue_claude(session: &str, message_id: &str, text: &str) -> Result<(), String> {
    enqueue_for(Tool::Claude, session, message_id, text)
}

/// Leave a message for the hook of a harness chat to pick up. `Err` for an unknown harness.
pub fn enqueue_hook(
    harness: &str,
    session: &str,
    message_id: &str,
    text: &str,
) -> Result<(), String> {
    let tool = hook_tool(harness).ok_or_else(|| format!("{harness} has no hook"))?;
    enqueue_for(tool, session, message_id, text)
}

/// Replaces `path` atomically: owner-only from creation on Unix (Windows keeps the parent's
/// inherited access), not flushed to disk (hand-off files that can be rebuilt).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, bytes),
    )
    .map_err(std::io::Error::from)
}

fn enqueue_for(tool: Tool, thread: &str, message_id: &str, text: &str) -> Result<(), String> {
    if !safe_component(thread) || !safe_component(message_id) {
        return Err("thread or message id is not a safe file name".into());
    }
    // Refused here, before it is accepted, rather than set aside by the hook later (Astra F5).
    if text.len() > MESSAGE_CAP {
        return Err("message is too large for the hook to hand over whole".into());
    }
    let dir = tool
        .inbox_root()
        .ok_or("the app's state folder can't be found")?
        .join(thread);
    // A message a hook run already claimed or handed to the chat (`claimed/<id>.<secs>.claimed`
    // or `.sent`) or that is already waiting is never queued a second time.
    if handed_to_chat(&dir.join("claimed"), message_id, true)
        || dir.join(format!("{message_id}.txt")).exists()
    {
        return Ok(());
    }
    rightkit_fs::private_cached::ensure_dir(&dir).map_err(|e| e.to_string())?;
    write_private(&dir.join(format!("{message_id}.txt")), text.as_bytes())
        .map_err(|e| e.to_string())
}

/// Whether `claimed/` records the message `id` as handed to the chat (`.sent`), or with
/// `in_flight` also as claimed by a hook run (`.claimed`).
fn handed_to_chat(claimed_dir: &Path, id: &str, in_flight: bool) -> bool {
    let prefix = format!("{id}.");
    std::fs::read_dir(claimed_dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix)
                && (name.ends_with(".sent") || (in_flight && name.ends_with(".claimed")))
        })
}

/// What the Codex hook recorded for a message: `Sent` (`claimed/<id>.<secs>.sent`, written only
/// after its output was written; the `seen` receipt follows it), `Refused` (`claimed/<id>.refused`,
/// too large to hand over whole), or `None` (still waiting, claimed in flight, or returned to
/// the inbox after a failed claim). A missing inbox file alone proves nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookOutcome {
    Sent,
    Refused,
}

pub fn outcome(thread: &str, message_id: &str) -> Option<HookOutcome> {
    if !safe_component(thread) || !safe_component(message_id) {
        return None;
    }
    let claimed = Tool::Codex.inbox_root()?.join(thread).join("claimed");
    if claimed.join(format!("{message_id}.refused")).exists() {
        return Some(HookOutcome::Refused);
    }
    handed_to_chat(&claimed, message_id, false).then_some(HookOutcome::Sent)
}

/// Whether the message is still waiting (no hook has claimed it).
pub fn pending(thread: &str, message_id: &str) -> bool {
    pending_for(Tool::Codex, thread, message_id)
}

/// `pending` for a Claude chat's inbox.
pub fn pending_claude(session: &str, message_id: &str) -> bool {
    pending_for(Tool::Claude, session, message_id)
}

/// `pending` for a hook-harness chat's inbox.
pub fn pending_hook(harness: &str, session: &str, message_id: &str) -> bool {
    hook_tool(harness).is_some_and(|tool| pending_for(tool, session, message_id))
}

fn pending_for(tool: Tool, thread: &str, message_id: &str) -> bool {
    if !safe_component(thread) || !safe_component(message_id) {
        return false;
    }
    tool.inbox_root()
        .is_some_and(|r| r.join(thread).join(format!("{message_id}.txt")).exists())
}

/// The newest time (unix seconds) any Codex chat ran the hook, if ever.
pub fn newest_heartbeat() -> Option<u64> {
    newest_heartbeat_of(Tool::Codex)
}

/// The newest time (unix seconds) any Claude chat ran the plugin's hook, if ever.
pub fn newest_claude_heartbeat() -> Option<u64> {
    newest_heartbeat_of(Tool::Claude)
}

/// The newest time (unix seconds) any chat of this hook harness ran the plugin's hook, if ever.
pub fn newest_hook_heartbeat(harness: &str) -> Option<u64> {
    newest_heartbeat_of(hook_tool(harness)?)
}

fn newest_heartbeat_of(tool: Tool) -> Option<u64> {
    std::fs::read_dir(tool.seen_root()?)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            parse_heartbeat(&text).map(|(secs, _)| secs)
        })
        .max()
}

/// A heartbeat file: unix seconds, a space and the tag.
fn parse_heartbeat(text: &str) -> Option<(u64, &str)> {
    let mut parts = text.split_whitespace();
    let secs = parts.next()?.parse().ok()?;
    Some((secs, parts.next()?))
}

/// Replace the heartbeat of `session` with now and `tag` (the hook event, or the Stop outcome).
fn write_heartbeat(seen: &Path, session: &str, tag: &str) {
    let tag: String = tag
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(32)
        .collect();
    let _ = write_private(
        &seen.join(session),
        format!("{} {tag}", now_secs()).as_bytes(),
    );
    prune_seen(seen);
}

/// Heartbeats not written for this long are removed.
const SEEN_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);
/// Heartbeat files kept at most, by count (about two weeks at the Mac's rate of new chats). Past
/// it the least recently written go first, so the newest always survive. The folder is bounded
/// by this cap, so every entry is read each write.
const SEEN_MAX_FILES: usize = 1000;

/// Remove heartbeat files older than `SEEN_MAX_AGE` (by modified time), then the least recently
/// written beyond `SEEN_MAX_FILES`. Every entry is read, so no live heartbeat is missed by a
/// partial listing. Only plain files are removed; the one just written is fresh and never the
/// oldest.
fn prune_seen(seen: &Path) {
    let Ok(listing) = std::fs::read_dir(seen) else {
        return;
    };
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in listing.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(written) = meta.modified() else {
            continue;
        };
        let old = written.elapsed().is_ok_and(|age| age > SEEN_MAX_AGE);
        if old {
            let _ = std::fs::remove_file(entry.path());
        } else {
            kept.push((written, entry.path()));
        }
    }
    // Newest first: everything past the cap is removed, oldest last.
    kept.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, path) in kept.iter().skip(SEEN_MAX_FILES) {
        let _ = std::fs::remove_file(path);
    }
}

/// What a chat's last hook run says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookActivity {
    /// When the hook last ran for the chat (unix seconds).
    pub seen_secs: u64,
    /// Seconds since then.
    pub age_secs: u64,
    /// The last run was a Stop that handed nothing back: the run ended and the chat is idle.
    /// False for every other event.
    pub ended: bool,
}

/// The newest hook run of this Codex thread, if the hook ever ran for it.
pub fn hook_activity(thread: &str) -> Option<HookActivity> {
    activity_of(Tool::Codex, thread)
}

/// `hook_activity` for a Claude session and the host app plugin's hook.
pub fn claude_hook_activity(session: &str) -> Option<HookActivity> {
    activity_of(Tool::Claude, session)
}

/// `hook_activity` for a chat of a hook harness and the host app plugin's hook there.
pub fn hook_harness_activity(harness: &str, session: &str) -> Option<HookActivity> {
    activity_of(hook_tool(harness)?, session)
}

fn activity_of(tool: Tool, thread: &str) -> Option<HookActivity> {
    if !safe_component(thread) {
        return None;
    }
    let text = std::fs::read_to_string(tool.seen_root()?.join(thread)).ok()?;
    let (seen_secs, tag) = parse_heartbeat(&text)?;
    // A hook-harness chat that has only started has no run going yet.
    let idle_tag = tag == "StopEnded" || (matches!(tool, Tool::Hook(_)) && tag == "SessionStart");
    Some(HookActivity {
        seen_secs,
        age_secs: now_secs().saturating_sub(seen_secs),
        ended: idle_tag,
    })
}

/// Whether the hook ran for this thread within `within`.
pub fn hook_seen_recently(thread: &str, within: Duration) -> bool {
    seen_recently(Tool::Codex, thread, within)
}

/// Whether the host app plugin's hook ran for this Claude session within `within`.
pub fn claude_hook_seen_recently(session: &str, within: Duration) -> bool {
    seen_recently(Tool::Claude, session, within)
}

fn seen_recently(tool: Tool, thread: &str, within: Duration) -> bool {
    if !safe_component(thread) {
        return false;
    }
    let Some(root) = tool.seen_root() else {
        return false;
    };
    let Some(seen) = std::fs::read_to_string(root.join(thread))
        .ok()
        .and_then(|t| parse_heartbeat(&t).map(|(secs, _)| secs))
    else {
        return false;
    };
    now_secs().saturating_sub(seen) <= within.as_secs()
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH)
}

/// A message a hook run holds: persisted -> claimed (`claimed/<id>.<secs>.claimed`, the claim time
/// in the name so a crash leaves the age readable) -> emitted (`claimed/<id>.sent`, once the hook's
/// output was written) -> confirmed (the transcript shows it, `confirm_receipts`).
struct Claimed {
    id: String,
    path: PathBuf,
    text: String,
}

/// What `claim` took: messages to hand over, and ids refused for being too large.
struct Claims {
    held: Vec<Claimed>,
    refused: Vec<String>,
}

/// Give claims that were never emitted back to the inbox: a hook run that died after its rename,
/// or whose output could not be written, must not strand the message. A message already back in
/// the inbox is left alone. Delivery is at least once: a run that wrote its output and died
/// before marking it sent hands the message over again after `CLAIM_TIMEOUT_SECS`.
fn recover_abandoned(dir: &Path, claimed_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(claimed_dir) else {
        return;
    };
    let now = now_secs();
    // Every entry is visited (claims are few); the bound is time, not count, so a busy chat's
    // crashed claims cannot strand behind 200 other entries.
    let started = std::time::Instant::now();
    for entry in entries.flatten() {
        if started.elapsed() > std::time::Duration::from_secs(2) {
            break;
        }
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "claimed") {
            continue;
        }
        // `<id>.<secs>.claimed`; ids hold no dot (`safe_component`).
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some((id, at)) = stem.rsplit_once('.') else {
            continue;
        };
        let Ok(at) = at.parse::<u64>() else {
            continue;
        };
        if !safe_component(id) || now.saturating_sub(at) < CLAIM_TIMEOUT_SECS {
            continue;
        }
        // Already handed to the chat by another claim of this id: a second copy is dropped, not queued.
        if handed_to_chat(claimed_dir, id, false) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let _ = rightkit_fs::rename_exclusive(&path, &dir.join(format!("{id}.txt")));
    }
}

/// Claim waiting messages (oldest first), up to `TEXT_CAP` bytes in all but always at least one,
/// into `claimed/`. Nothing is cut: a message over `MESSAGE_CAP` is moved aside as `.refused` and
/// reported in `Claims::refused`.
fn claim(dir: &Path) -> Claims {
    let mut out = Claims {
        held: Vec::new(),
        refused: Vec::new(),
    };
    if !dir.is_dir() {
        return out;
    }
    let claimed_dir = dir.join("claimed");
    let _ = rightkit_fs::private_cached::ensure_dir(&claimed_dir);
    recover_abandoned(dir, &claimed_dir);
    let mut files: Vec<(SystemTime, PathBuf, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file()
                .then(|| (m.modified().unwrap_or(UNIX_EPOCH), e.path(), m.len()))
        })
        .collect();
    files.sort();
    let mut total = 0usize;
    for (_, path, len) in files {
        let size = usize::try_from(len).unwrap_or(usize::MAX);
        let Some(id) = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|id| safe_component(id))
        else {
            continue;
        };
        // An id a hook run already handed to the chat is never handed over again.
        if handed_to_chat(&claimed_dir, &id, false) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if size > MESSAGE_CAP {
            // Never handed over cut: set aside, and the sender is told.
            if rightkit_fs::rename_exclusive(&path, &claimed_dir.join(format!("{id}.refused")))
                .is_ok()
            {
                out.refused.push(id);
            }
            continue;
        }
        if !out.held.is_empty() && total.saturating_add(size) > TEXT_CAP {
            break;
        }
        let target = claimed_dir.join(format!("{id}.{}.claimed", now_secs()));
        // The rename is the claim: if it fails, another hook run took the file.
        if rightkit_fs::rename_exclusive(&path, &target).is_err() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&target) else {
            // Left claimed: recovered after the timeout.
            continue;
        };
        // The host app writes message files from text, so this is lossless; a damaged file keeps its
        // readable part and still reaches the chat rather than waiting forever.
        let text = String::from_utf8_lossy(&bytes).into_owned();
        total += text.len();
        out.held.push(Claimed {
            id,
            path: target,
            text,
        });
    }
    out
}

/// The output of these claims was written: mark them sent so they are not recovered.
fn mark_emitted(held: &[Claimed]) {
    for c in held {
        let _ = rightkit_fs::rename_exclusive(&c.path, &c.path.with_extension("sent"));
    }
}

/// The output could not be written: put the claims straight back in the inbox for the next run.
fn release_claims(dir: &Path, held: &[Claimed]) {
    for c in held {
        let _ = rightkit_fs::rename_exclusive(&c.path, &dir.join(format!("{}.txt", c.id)));
    }
}

fn prune(claimed_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(claimed_dir) else {
        return;
    };
    for entry in entries.flatten().take(PRUNE_EXAMINE) {
        // An unemitted claim is recovered, never pruned.
        if entry.path().extension().is_some_and(|x| x == "claimed") {
            continue;
        }
        let old = SystemTime::now()
            .duration_since(mtime(&entry.path()))
            .is_ok_and(|age| age > PRUNE_AFTER);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The Codex hook entry point. Never fails Codex: every error prints nothing and returns 0.
pub fn run_hook() -> i32 {
    run(Tool::Codex)
}

/// The Claude hook entry point. Never fails Claude: every error prints nothing and returns 0.
pub fn run_claude_hook() -> i32 {
    run(Tool::Claude)
}

/// The entry point of `<cli> chat hook --harness <name>`. Never fails the harness: an unknown name,
/// the harness's switch being off and every error print nothing and return 0.
pub fn run_harness_hook(harness: &str) -> i32 {
    match hook_tool(harness) {
        Some(tool) => run(tool),
        None => {
            eprintln!(
                "{} hook: no hook harness named {harness}",
                super::brand::brand().chat_marker()
            );
            0
        }
    }
}

/// Messages a hook run claimed and is about to print: where they were claimed from, and which.
struct Emission {
    dir: PathBuf,
    held: Vec<Claimed>,
}

fn run(tool: Tool) -> i32 {
    let mut confirm = None;
    let mut emission = None;
    if let Some(output) = hook_output(tool, &mut confirm, &mut emission) {
        use std::io::Write;
        // `println!` panics on a closed pipe; the claim must survive that, so write by hand.
        let mut out = std::io::stdout().lock();
        let written = writeln!(out, "{output}").and_then(|()| out.flush());
        if let Some(Emission { dir, held }) = emission {
            if written.is_ok() {
                // Emitted: the output reached the harness, so the sender hears `seen` now (not
                // when the file was merely claimed) and the claim is closed.
                mark_emitted(&held);
                if let Some(root) = chat_root() {
                    let store = super::store::Store::at_chat_folder(&root);
                    for c in &held {
                        crate::chat::receipts::seen(&store, &c.id);
                    }
                }
            } else {
                release_claims(&dir, &held);
            }
        }
    } else if let Some(Emission { dir, held }) = emission {
        // Claimed but nothing was printed: back to the inbox, never stranded.
        release_claims(&dir, &held);
    }
    // After the hook's own output is written, so the confirm never delays or alters it.
    if let Some((session, transcript)) = confirm {
        let wire = if tool == Tool::Codex {
            "codex"
        } else {
            "claude"
        };
        confirm_receipts(wire, &session, &transcript);
    }
    0
}

/// Membrane next to the running `<cli>` (same directory as `current_exe`), if installed.
pub fn membrane_binary() -> Option<PathBuf> {
    let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let name = if cfg!(windows) {
        "membrane.exe"
    } else {
        "membrane"
    };
    let path = directory.join(name);
    path.is_file().then_some(path)
}

/// Asks Membrane to mark this session's `emitted` receipts `delivered` once their marker shows
/// in the transcript: `membrane receipts confirm --wire <wire> --session <id> --transcript <path>`.
/// Gives up after three seconds; stdout and stderr are discarded and every failure is ignored.
pub fn confirm_receipts(wire: &str, session: &str, transcript: &str) {
    let Some(binary) = membrane_binary() else {
        return;
    };
    let options = RunOptions::new(Duration::from_secs(3))
        .stdout_cap(0)
        .stderr_cap(0);
    let _ = rightkit_child_process::run(
        binary,
        [
            "receipts",
            "confirm",
            "--wire",
            wire,
            "--session",
            session,
            "--transcript",
            transcript,
        ],
        &options,
    );
}

/// One hub-log line for a hook run that did nothing, or handed messages over (#581: the Dell's
/// Claude hook was "never run" and nothing said why). Fixed words and a count only. A failing
/// outcome is logged at most once a minute per tool and outcome (a marker file's age decides), so
/// a hook that fails on every tool call cannot fill the log; a claim is always logged.
fn hook_note(tool: Tool, outcome: &str, count: usize) {
    let label = match tool {
        Tool::Codex => "codex",
        Tool::Claude => "claude",
        Tool::Hook(_) => "hook",
    };
    let claimed = outcome == "claimed";
    if !claimed && let Some(root) = chat_root() {
        let marker = root.join(format!("hook-log-{label}-{outcome}"));
        let recent = std::fs::metadata(&marker)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(60));
        if recent {
            return;
        }
        let _ = rightkit_fs::private_cached::ensure_dir(&root);
        let _ = std::fs::write(&marker, b"");
    }
    super::sendlog::event(
        if claimed {
            rightkit_logs::Level::Info
        } else {
            rightkit_logs::Level::Warn
        },
        "chat.hook.run",
        serde_json::json!({
            "tool": label,
            "outcome": outcome,
            "count": count,
            "pid": std::process::id(),
        }),
    );
}

fn hook_output(
    tool: Tool,
    confirm: &mut Option<(String, String)>,
    emission: &mut Option<Emission>,
) -> Option<String> {
    let hook = match HookEvent::from_reader(tool.wire(), std::io::stdin()) {
        Ok(hook) => hook,
        Err(_) => {
            hook_note(tool, "input_unreadable", 0);
            return None;
        }
    };
    let Some(session) = hook.session_id.as_deref() else {
        hook_note(tool, "no_session_id", 0);
        return None;
    };
    let event = hook.kind.as_str();
    if !safe_component(session) {
        hook_note(tool, "unsafe_session_id", 0);
        return None;
    }
    // A subagent's tool call is not the chat's own turn: it must neither take the chat's
    // messages nor count as the chat being busy. (Seen: a chat between turns was reported
    // busy because its subagents' hooks kept refreshing the heartbeat.) Claude marks a
    // subagent's hook input with `agent_id`; Codex reports the parent's session id but a
    // different transcript file.
    if tool == Tool::Codex {
        host_probe();
    }
    let hook_harness = matches!(tool, Tool::Hook(_));
    // A harness whose switch is off is not listed or reached, so its hook does nothing.
    if let Tool::Hook(name) = tool
        && !harness_enabled(name)
    {
        return None;
    }
    let own_turn = hook.is_own_turn();
    // The plugin has run for its own turn: keep its harness registered, with its sessions folder
    // from the payload when the record has none.
    if let Tool::Hook(name) = tool
        && own_turn
        && let Some(root) = chat_root()
    {
        super::harness::register(
            &super::store::Store::at_chat_folder(&root),
            name,
            hook.transcript_path.as_deref(),
            hook.cwd.as_deref(),
        );
    }
    if event == "Stop"
        && own_turn
        && let Some(path) = hook.transcript_path.as_deref()
    {
        *confirm = Some((session.to_string(), path.to_string()));
    }
    let seen = tool.seen_root()?;
    let beating = rightkit_fs::private_cached::ensure_dir(&seen).is_ok();
    if !beating {
        hook_note(tool, "heartbeat_folder_unwritable", 0);
    }
    if beating && own_turn {
        write_heartbeat(&seen, session, event);
    }
    if let Tool::Hook(name) = tool
        && own_turn
    {
        register_chat(name, session, &hook);
    }
    if !own_turn || !matches!(event, "PostToolUse" | "Stop" | "UserPromptSubmit") {
        return None;
    }
    let dir = tool.inbox_root()?.join(session);
    // The store of Chat folder the message files were claimed from.
    let store = super::store::Store::at_chat_folder(&chat_root()?);
    let claims = claim(&dir);
    prune(&dir.join("claimed"));
    // A message too large to hand over whole is never cut: its sender is told it was refused.
    for id in &claims.refused {
        crate::chat::receipts::hook_refused(&store, id);
    }
    if !claims.held.is_empty() || !claims.refused.is_empty() {
        hook_note(tool, "claimed", claims.held.len() + claims.refused.len());
    }
    // `seen` is reported by `run` once the output below has been written, not at the claim.
    let texts: Vec<String> = claims.held.iter().map(|c| c.text.clone()).collect();
    *emission = (!claims.held.is_empty()).then(|| Emission {
        dir: dir.clone(),
        held: claims.held,
    });
    if beating && event == "Stop" {
        // Handing messages back continues the run; handing none back ends it.
        let outcome = if texts.is_empty() {
            "StopEnded"
        } else {
            "StopContinued"
        };
        write_heartbeat(&seen, session, outcome);
    }
    // Receipts for this chat's own sent messages: shown to its owner only, never given to the model.
    // A hook harness's output is not known to take `systemMessage`, so it gets neither the
    // receipt notices (they stay for later) nor the owner-facing copy of a delivery; the preface
    // makes the model tell its owner instead.
    let notice = if own_turn && !hook_harness {
        crate::chat::receipts::take_notice(&store, session)
    } else {
        None
    };
    if texts.is_empty() {
        // A the host app wake line can start a turn after its message was handed over by another hook
        // run, or refused, from Codex's queue. Say so only on the hook's own record (the `.sent`
        // mark or `.refused` file); an empty claim alone (a read failure, a claim in flight, a
        // missing inbox) proves nothing, so then no note is added.
        let claimed = dir.join("claimed");
        if event == "UserPromptSubmit"
            && let Some(id) = hook.prompt.as_deref().and_then(stale_wake_id)
            && (claimed.join(format!("{id}.refused")).exists()
                || handed_to_chat(&claimed, id, false))
        {
            let note = format!(
                "{}: message {id} was already handed to this chat earlier (or refused as too large); nothing new is waiting. Answer in one line that nothing new is waiting and do nothing else.",
                super::brand::brand().chat_label()
            );
            let reply = Reply::additional_context(&hook.kind, &note).ok()?;
            let reply = match (&notice, hook_harness) {
                (Some(notice), false) => reply.with_system_message(notice),
                _ => reply,
            };
            return reply.stdout_line();
        }
        let notice = notice?;
        return Reply::none().with_system_message(&notice).stdout_line();
    }
    let body = texts.join("\n\n");
    let reply = if event == "Stop" {
        Reply::block_stop(&format!("{}\n{body}", preface()))
    } else {
        Reply::additional_context(&hook.kind, &format!("{}\n{body}", preface())).ok()?
    };
    // Nothing is shown to the owner for a delivery: the app draws its own "Received message"
    // row and the model reports it in one line (the owner, 2026-10-23, #461). Receipt notices still show.
    let reply = match (&notice, hook_harness) {
        (Some(notice), false) => reply.with_system_message(notice),
        _ => reply,
    };
    reply.stdout_line()
}

/// The message id of a prompt that is exactly a wake line
/// (`envelope::wake_line`: `<product> Chat: message from <who>; id <id>`).
fn stale_wake_id(prompt: &str) -> Option<&str> {
    let line = prompt.trim();
    if !line.starts_with(&format!(
        "{}: message from ",
        super::brand::brand().chat_label()
    )) || line.contains('\n')
    {
        return None;
    }
    let id = line.rsplit_once("; id ")?.1.trim();
    safe_component(id).then_some(id)
}

/// Whether the switch for this hook harness's chats is on (on when it cannot be read).
fn harness_enabled(harness: &str) -> bool {
    let Some(root) = chat_root() else {
        return true;
    };
    let store = super::store::Store::at_chat_folder(&root);
    store.harness_chats(harness)
}

/// Register (or refresh) the chat this hook run belongs to, so the host app lists and reaches it: its id,
/// kind, folder, a title (from the harness's own record when cheaply known) and the harness's
/// process id, which is what keeps it listed. A chat starting or being prompted is registered
/// afresh; the many `PostToolUse` and `Stop` runs of a turn rewrite the record only once a minute
/// has passed or the folder changed, so they cost no process scan and no write.
fn register_chat(harness: &str, session: &str, hook: &HookEvent) {
    const REFRESH_MS: u64 = 60_000;
    let Some(root) = chat_root() else {
        return;
    };
    let store = super::store::Store::at_chat_folder(&root);
    let previous = store.registered_session(session);
    let now = super::envelope::now_ms();
    let cwd = hook
        .cwd
        .clone()
        .or_else(|| previous.as_ref().map(|p| p.cwd.clone()))
        .unwrap_or_default();
    let renew = matches!(hook.kind.as_str(), "SessionStart" | "UserPromptSubmit")
        || previous.as_ref().is_none_or(|p| p.name.is_empty());
    if !renew
        && let Some(p) = &previous
        && p.cwd == cwd
        && now.saturating_sub(p.updated) < REFRESH_MS
    {
        return;
    }
    let Some(pid) = harness_pid(harness) else {
        return;
    };
    let looked_up = if renew {
        super::harness::find(&store, harness, session)
            .map(|s| s.title)
            .filter(|t| !t.is_empty())
    } else {
        None
    };
    let name = looked_up
        .or_else(|| previous.as_ref().map(|p| p.name.clone()))
        .unwrap_or_default();
    let _ = store.register_session(&super::store::RegisteredSession {
        id: session.to_string(),
        kind: harness.to_string(),
        name,
        cwd,
        pid,
        updated: now,
    });
}

/// The process of `harness` this hook runs under: the nearest ancestor whose name contains the
/// harness's name, else the hook's parent. `None` when neither can be found.
fn harness_pid(harness: &str) -> Option<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let parent = system
        .process(Pid::from_u32(std::process::id()))?
        .parent()?;
    let mut at = Some(parent);
    for _ in 0..8 {
        let process = system.process(at?)?;
        // The harness's own process: its name is the harness, or the harness followed by `-` or
        // `.` (`muse`, `muse.exe`, `muse-bin-1.4.11-R6667.1`), never a name that merely contains it.
        let name = process.name().to_string_lossy().to_ascii_lowercase();
        let own = name
            .strip_prefix(harness)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '.']));
        if own {
            return Some(process.pid().as_u32());
        }
        at = process.parent();
    }
    Some(parent.as_u32())
}

/// A read-only question, asked at most once an hour: can a hook, which Codex itself starts,
/// talk to the Codex app's own tool socket? (An outside process is turned away: the app
/// closes the connection.) The answer decides whether the host app can hand messages to the app's
/// native "message another chat" tool. Only the tool names are recorded, in
/// `codex-host-probe.json` beside the inbox. Unix only; never delays the hook by more than
/// a second and a half.
#[cfg(unix)]
fn host_probe() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let Some(root) = chat_root() else {
        return;
    };
    let record = root.join("codex-host-probe.json");
    let recent = std::fs::metadata(&record)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(3600));
    if recent {
        return;
    }
    let path = std::env::var("CODEX_APP_TOOLS_PIPE_PATH").unwrap_or_default();
    let mut found = json!({"at": now_secs(), "pipe_in_environment": !path.is_empty()});
    if !path.is_empty() {
        let limit = Some(Duration::from_millis(1500));
        let answer = UnixStream::connect(&path).ok().and_then(|mut stream| {
            stream.set_read_timeout(limit).ok()?;
            stream.set_write_timeout(limit).ok()?;
            let request = br#"{"id":1,"jsonrpc":"2.0","method":"tools/list","params":{"threadStartKind":"all"}}"#;
            let length = u32::try_from(request.len()).ok()?;
            stream.write_all(&length.to_le_bytes()).ok()?;
            stream.write_all(request).ok()?;
            let mut head = [0u8; 4];
            stream.read_exact(&mut head).ok()?;
            let size = u64::from(u32::from_le_bytes(head)).min(STDIN_CAP);
            let mut body = Vec::new();
            stream.take(size).read_to_end(&mut body).ok()?;
            serde_json::from_slice::<Value>(&body).ok()
        });
        let names: Vec<String> = answer
            .as_ref()
            .and_then(|a| a["result"]["tools"].as_array())
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        found["answered"] = json!(answer.is_some());
        found["refused"] = json!(answer.as_ref().is_some_and(|a| a.get("error").is_some()));
        found["tools"] = json!(names);
    }
    if let Ok(bytes) = serde_json::to_vec(&found) {
        let _ = write_private(&record, &bytes);
    }
}

#[cfg(not(unix))]
fn host_probe() {}
