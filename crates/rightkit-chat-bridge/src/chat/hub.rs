//! What the host app hub runs for Chat: a heartbeat, a rescan of this
//! computer's chats, replies routed back to linked computers, and the CLI's
//! control requests.
//!
//! Why the hub posts into Claude on behalf of the CLI: a Claude chat accepts a
//! peer message only when the posting process descends from a registered
//! session (one with files in `~/.claude/sessions`). A `<cli>` started by sshd
//! descends from nothing, so its posts are dropped without a word. The hub
//! therefore registers itself there (`<pid>.json` plus a key file, named like
//! Claude's own, entrypoint `pulse-hub`) and the CLI hands local deliveries to
//! it through `control` when it runs. On Windows the chat also checks the auth
//! token, so the CLI can post directly; the hub is registered there too
//! (`pidDomain` windows, its own reply pipe as `messagingSocketPath`) so chats
//! list it and can answer it.

use super::control::{self, Request};
use super::deliver_claude::ReplyMessage;
use super::envelope::{Envelope, Sender, Target};
use super::links::{self, Link};
use super::roster::{self, LocalSession};
use super::store::Store;
use super::{ReceiptState, deliver_local_via};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const SESSION_SCAN_EVERY: Duration = Duration::from_secs(5);
/// The registry entry's name, as other Claude chats list it.
pub fn registered_name() -> &'static str {
    &super::brand::brand().hub_name
}
/// `entrypoint` in the registry entry; the roster hides it.
pub fn entrypoint() -> &'static str {
    &super::brand::brand().hub_entrypoint
}
/// Control requests handled at once; more are answered "busy".
const MAX_CONTROL_WORKERS: usize = 8;
/// Posts of one queued reply tried before it is left for the next hub start.
const MAX_REPLY_ATTEMPTS: u32 = 3;
/// How far back reply routes are restored on start.
const ROUTE_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Default)]
struct State {
    last_error: Option<String>,
    sessions: Vec<LocalSession>,
    scanned: Option<Instant>,
    registered: Option<std::path::PathBuf>,
    key_file: Option<std::path::PathBuf>,
    /// The same registration written into the other Claude profiles' folders.
    extra_files: Vec<std::path::PathBuf>,
    /// What each link answered last (chats, or the error).
    link_state: Vec<StatusLink>,
    /// The chats each link listed last, for the page's list.
    remote_chats: Vec<StatusChat>,
    links_asked: Option<Instant>,
    /// Queued replies being posted right now (by message id).
    outbound_inflight: HashSet<String>,
    /// Post attempts per queued reply.
    outbound_attempts: HashMap<String, u32>,
    /// Reply listeners for stored routes were recreated since the hub started.
    listeners_restored: bool,
    /// Reply-route listener keys from the last scan of the routes folder, with the folder's
    /// modification time and when it was scanned; reused while the folder is unchanged.
    route_keys: Option<(Option<std::time::SystemTime>, Instant, Vec<String>)>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(State::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn store() -> Option<&'static Store> {
    static STORE: OnceLock<Option<Store>> = OnceLock::new();
    STORE.get_or_init(|| Store::open_default().ok()).as_ref()
}

fn note(error: String) {
    state().last_error = Some(error);
}

/// This computer's chats, rescanned at most every few seconds. `pub(super)`: `chat.peers`
/// (`mesh`) takes the hub's own recent scan instead of sweeping the folders again.
pub(super) fn local_chats(store: &Store, max_age: Duration) -> Vec<LocalSession> {
    {
        let state = state();
        if let Some(at) = state.scanned
            && at.elapsed() < max_age
        {
            return state.sessions.clone();
        }
    }
    let sessions = roster::local_sessions(store);
    let mut state = state();
    state.sessions = sessions.clone();
    state.scanned = Some(Instant::now());
    sessions
}

/// Whether `id` is a chat on this computer (for `ReplyHub`).
pub fn is_known_local_session(id: &str) -> bool {
    store().is_some_and(|s| {
        local_chats(s, SESSION_SCAN_EVERY)
            .iter()
            .any(|c| is_chat(c, id))
    })
}

/// Is `id` this chat? A Claude Desktop chat signs its replies with the Desktop record id
/// (`hostSessionId` in its registry entry, `local_…`), not the session id the roster uses.
fn is_chat(chat: &LocalSession, id: &str) -> bool {
    chat.id == id || chat.raw["hostSessionId"].as_str() == Some(id)
}

// ---- registration ------------------------------------------------------------------

/// Put this process in Claude's session registry so its posts are accepted: in this
/// process's own folder and in every other Claude profile's (`deliver_claude::sessions_dirs`),
/// because a chat in another profile only accepts posts from a session registered in its own
/// folder.
#[cfg(any(unix, windows))]
fn register(alias: &str) {
    let own = super::deliver_claude::sessions_dir();
    register_in(&own, alias, true);
    let own_key = std::fs::canonicalize(&own).unwrap_or(own);
    for (dir, _) in super::deliver_claude::sessions_dirs() {
        if std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone()) != own_key {
            register_in(&dir, alias, false);
        }
    }
}

#[cfg(any(unix, windows))]
fn register_in(dir: &std::path::Path, alias: &str, primary: bool) {
    let dir = dir.to_path_buf();
    // Another profile's folder is used only when that profile exists; it is never created.
    if if primary {
        rightkit_fs::private_cached::ensure_dir(&dir).is_err()
    } else {
        !dir.is_dir()
    } {
        return;
    }
    let pid = std::process::id();
    // Leftovers: an earlier hub that was killed (an install) never unregistered, and a
    // file with this pid could be ours from before. Only entries this hub wrote
    // (`entrypoint` pulse-hub) whose process is gone are removed.
    let mut stale: Vec<u32> = vec![pid];
    if let Ok(listing) = std::fs::read_dir(&dir) {
        for entry in listing.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Some(value) = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            else {
                continue;
            };
            if value["entrypoint"] == entrypoint()
                && let Some(old) = value["pid"].as_u64().and_then(|p| u32::try_from(p).ok())
                && old != pid
                && !roster::live_pids()(old)
            {
                stale.push(old);
            }
        }
    }
    if let Ok(listing) = std::fs::read_dir(&dir) {
        for entry in listing.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let ours = stale.iter().any(|p| {
                name == format!("{p}.json")
                    || (name.starts_with(&format!("{p}.")) && name.ends_with(".key"))
            });
            if ours {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let now = super::envelope::now_ms();
    #[cfg(unix)]
    let reply_dir = super::deliver_claude::reply_socket_path("hub")
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    let reply_dir = super::deliver_claude::reply_pipe_name("hub");
    #[cfg(unix)]
    let domain = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    // Claude's own entries say `win32:<host>`; match that so readers treat ours alike.
    #[cfg(windows)]
    let domain_owned = format!(
        "win32:{}",
        sysinfo::System::host_name()
            .unwrap_or_else(|_| "pc".to_string())
            .to_lowercase()
    );
    #[cfg(windows)]
    let domain = domain_owned.as_str();
    let json_path = dir.join(format!("{pid}.json"));
    let key_hash = super::random_hex(32);
    let key_path = dir.join(format!("{pid}.{key_hash}.key"));
    let record = json!({
        "pid": pid,
        "sessionId": rightkit_logs::identity::random_uuid(),
        "cwd": rightkit_app_host::account_home().to_string_lossy().into_owned(),
        "startedAt": now,
        "version": format!("{} {}", super::brand::brand().cli, super::brand::brand().app_version),
        "peerProtocol": super::deliver_claude::PEER_PROTOCOL,
        "peerFeatures": [],
        "kind": "interactive",
        "entrypoint": entrypoint(),
        "hostSessionId": format!("{}-hub-{alias}", super::brand::brand().session_prefix),
        "pidDomain": domain,
        "messagingSocketPath": reply_dir,
        "name": registered_name(),
        "nameSince": now,
        "updatedAt": now,
        "status": "idle",
        "statusUpdatedAt": now,
    });
    // No `procStart`: the hub doesn't record a start time, and a reader without one
    // checks only that the pid is running.
    let key = json!({
        "peerToken": super::random_hex(32),
        "pidDomain": domain,
    });
    // Owner-only from creation: the token is never in a file others can read, even briefly.
    let _ = rightkit_fs::atomic_write_with_options(
        &key_path,
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, key.to_string().as_bytes()),
    );
    if rightkit_fs::atomic_write(&json_path, record.to_string().as_bytes()).is_ok() {
        let mut state = state();
        if primary {
            state.registered = Some(json_path);
            state.key_file = Some(key_path);
        } else {
            // `register` runs again on every tick while the primary write keeps failing; the same
            // two files are listed once, not once per tick (issue #501).
            for path in [json_path, key_path] {
                if !state.extra_files.contains(&path) {
                    state.extra_files.push(path);
                }
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn register(_alias: &str) {}

fn unregister() {
    let (json_path, key_path, extra) = {
        let mut state = state();
        (
            state.registered.take(),
            state.key_file.take(),
            std::mem::take(&mut state.extra_files),
        )
    };
    for path in [json_path, key_path].into_iter().flatten().chain(extra) {
        let _ = std::fs::remove_file(path);
    }
}

/// Chat became active: register and write the first heartbeat.
pub fn start(alias: &str) {
    let alias = super::clean_alias(alias);
    let begun = std::time::Instant::now();
    register(&alias);
    let registered = begun.elapsed().as_millis();
    {
        let mut st = state();
        st.listeners_restored = false;
        st.route_keys = None;
    }
    progress().alias.clone_from(&alias);
    if let Some(store) = store() {
        let written = {
            let mut gate = gate();
            *gate = true;
            store.write_relay_status(&alias)
        };
        if let Err(e) = written {
            note(format!("Couldn't write the heartbeat: {e}"));
        }
        let beat = begun.elapsed().as_millis();
        restore_reply_listeners(store);
        super::receipts::prune(store);
        // A slow start leaves chats' replies refused for that long: say where it went.
        let done = begun.elapsed().as_millis();
        if done > 1_000 {
            note(format!(
                "Chat took {done} ms to start (registered {registered} ms, heartbeat {beat} ms)"
            ));
        }
    }
}

/// Chat went inactive or the hub exits.
pub fn stop() {
    unregister();
    state().listeners_restored = false;
    // Closed first, under the gate, so the heartbeat thread cannot write a beat after the clear.
    progress().last_begin = None;
    let mut gate = gate();
    *gate = false;
    if let Some(store) = store() {
        store.clear_relay_status();
    }
}

/// Recreate the reply listener of every reply route from the last 24 hours, so a chat
/// that answers after a hub restart still reaches the socket it was told. Marks itself
/// done once a listener exists (the reply hub may not be set when `start` runs).
fn restore_reply_listeners(store: &Store) {
    if state().listeners_restored {
        return;
    }
    let keys = reply_route_keys(store);
    let mut any = keys.is_empty();
    for key in keys {
        any |= super::deliver_claude::reply_address(&key).is_some();
    }
    if any {
        state().listeners_restored = true;
    }
}

/// The distinct `device:chat` keys of the reply routes from the last 24 hours. The routes
/// folder is rescanned only when its modification time changed (or ten minutes passed, so
/// the window moves on); a route file whose own modification time is older than the window is
/// skipped unopened (a file is never modified before it was created), and at most
/// `ROUTE_SCAN_FILES` of the newest files are opened per scan.
fn reply_route_keys(store: &Store) -> Vec<String> {
    const ROUTE_SCAN_FILES: usize = 1000;
    let dir = store.root().join("reply-routes");
    let folder = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
    if let Some((seen, at, keys)) = &state().route_keys
        && folder.is_some()
        && *seen == folder
        && at.elapsed() < Duration::from_secs(600)
    {
        return keys.clone();
    }
    let since = super::envelope::now_ms().saturating_sub(ROUTE_WINDOW_MS);
    let window_start = std::time::UNIX_EPOCH + Duration::from_millis(since);
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .map(|listing| {
            listing
                .flatten()
                .filter_map(|e| {
                    // Unknown modification time: open it (when in doubt, read).
                    let m = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::now());
                    (m >= window_start).then(|| (m, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    if files.len() > ROUTE_SCAN_FILES {
        eprintln!(
            "{}: {} reply routes in the last 24 hours; reading the newest {ROUTE_SCAN_FILES}",
            super::brand::brand().chat_marker(),
            files.len()
        );
    }
    files.truncate(ROUTE_SCAN_FILES);
    let mut keys: Vec<String> = files
        .iter()
        .filter_map(|(_, path)| std::fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<super::store::ReplyRoute>(&bytes).ok())
        .filter(|r| r.created_ms >= since)
        .map(|r| format!("{}:{}", r.from_device, r.from_session))
        .collect();
    keys.sort();
    keys.dedup();
    state().route_keys = Some((folder, Instant::now(), keys.clone()));
    keys
}

// ---- heartbeat thread and tick stages ---------------------------------------------------

/// A tick (or a background pass) running this long is logged once, with the stage it is in (T5).
const SLOW_TICK: Duration = Duration::from_secs(30);
/// How often the heartbeat thread looks (the writer itself rewrites at most every 10 s).
const HEARTBEAT_EVERY: Duration = Duration::from_secs(2);
/// The heartbeat thread keeps writing while a tick has begun within this long. A slow or blocked
/// tick (a registry write, a pipe) therefore cannot fake the hub's death, but a tick thread that
/// is gone, or stuck for this long, still lets the beat go stale.
const TICK_PROOF: Duration = Duration::from_secs(300);

#[derive(Default)]
struct Lane {
    began: Option<Instant>,
    stage: &'static str,
    slow_logged: bool,
}

#[derive(Default)]
struct TickProgress {
    alias: String,
    /// The most recent time a tick began: the proof the tick thread is alive.
    last_begin: Option<Instant>,
    /// The foreground tick (`tick`) and the background pass it spawns (`background`).
    fg: Lane,
    bg: Lane,
    withheld_logged: bool,
}

fn progress() -> MutexGuard<'static, TickProgress> {
    static PROGRESS: OnceLock<Mutex<TickProgress>> = OnceLock::new();
    PROGRESS
        .get_or_init(|| Mutex::new(TickProgress::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Whether the heartbeat may be written. Held across the write and the clear so `stop` cannot be
/// followed by a late beat from the heartbeat thread.
fn gate() -> MutexGuard<'static, bool> {
    static GATE: Mutex<bool> = Mutex::new(false);
    GATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// One line of `<state>/logs/hub.jsonl`, the format `sendlog` and the engine's `hublog` share.
fn log_event(level: rightkit_logs::Level, event: &str, fields: Value) {
    let Some(dir) = rightkit_app_host::state_root() else {
        return;
    };
    if !rightkit_app_host::setting_bool("hubLog", true) {
        return;
    }
    let mut line = rightkit_logs::EventRecord::new(super::envelope::now_ms() as i64, level, event);
    if let Value::Object(map) = fields {
        for (key, value) in map {
            line = line.with_field(key, value);
        }
    }
    rightkit_logs::JsonlStore::new(dir.join("logs").join("hub.jsonl"), 5 * 1024 * 1024)
        .with_generations(2)
        .with_cross_process_lock()
        .append_best_effort(&line);
}

/// Marks a lane running, and records its stage as it goes; dropping it (also on a panic) marks it
/// done and, if it was logged slow, says it recovered.
struct LaneGuard {
    bg: bool,
}

impl LaneGuard {
    fn begin(bg: bool, stage: &'static str) -> Self {
        let mut p = progress();
        if !bg {
            p.last_begin = Some(Instant::now());
        }
        let lane = if bg { &mut p.bg } else { &mut p.fg };
        *lane = Lane {
            began: Some(Instant::now()),
            stage,
            slow_logged: false,
        };
        LaneGuard { bg }
    }

    fn stage(&self, stage: &'static str) {
        let mut p = progress();
        let lane = if self.bg { &mut p.bg } else { &mut p.fg };
        lane.stage = stage;
    }
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        let mut p = progress();
        let lane = if self.bg { &mut p.bg } else { &mut p.fg };
        let was = std::mem::take(lane);
        drop(p);
        if was.slow_logged {
            let elapsed = was.began.map_or(0, |at| at.elapsed().as_secs());
            log_event(
                rightkit_logs::Level::Info,
                "chat.tick.recovered",
                json!({ "lane": lane_name(self.bg), "last_stage": was.stage, "elapsed_s": elapsed }),
            );
        }
    }
}

fn lane_name(bg: bool) -> &'static str {
    if bg { "background" } else { "foreground" }
}

/// Starts the thread that writes the Chat heartbeat on its own timer (T4), once per process.
fn ensure_heartbeat_thread() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("chat-heartbeat".into())
        .spawn(|| {
            loop {
                std::thread::sleep(HEARTBEAT_EVERY);
                heartbeat_pass();
            }
        });
    if spawned.is_err() {
        STARTED.store(false, Ordering::Release);
    }
}

/// One look of the heartbeat thread: log a tick that has run past `SLOW_TICK` (with its stage),
/// then write the beat while a tick has begun recently.
fn heartbeat_pass() {
    let mut slow: Vec<(&'static str, &'static str, u64)> = Vec::new();
    let (alias, proof, withhold_now) = {
        let mut p = progress();
        for bg in [false, true] {
            let lane = if bg { &mut p.bg } else { &mut p.fg };
            if let Some(began) = lane.began
                && !lane.slow_logged
                && began.elapsed() >= SLOW_TICK
            {
                lane.slow_logged = true;
                slow.push((lane_name(bg), lane.stage, began.elapsed().as_secs()));
            }
        }
        let proof = p.last_begin.is_some_and(|at| at.elapsed() < TICK_PROOF);
        let withhold_now = !proof && p.last_begin.is_some() && !p.withheld_logged;
        if withhold_now {
            p.withheld_logged = true;
        }
        if proof {
            p.withheld_logged = false;
        }
        (p.alias.clone(), proof, withhold_now)
    };
    for (lane, stage, elapsed) in slow {
        log_event(
            rightkit_logs::Level::Warn,
            "chat.tick.slow",
            json!({ "lane": lane, "stage": stage, "elapsed_s": elapsed }),
        );
    }
    if withhold_now {
        log_event(
            rightkit_logs::Level::Error,
            "chat.heartbeat.withheld",
            json!({ "reason": "no tick began for 300 s", "tick_proof_s": TICK_PROOF.as_secs() }),
        );
    }
    if !proof || alias.is_empty() {
        return;
    }
    let gate = gate();
    if !*gate {
        return;
    }
    if let Some(store) = store()
        && let Err(e) = store.write_relay_status(&alias)
    {
        drop(gate);
        note(format!("Couldn't write the heartbeat: {e}"));
    }
}

/// Every couple of seconds while active: heartbeat first, then (off this thread) the chat
/// rescan, the slow poll of linked computers for the page, and queued reply retries.
/// Nothing here waits on ssh.
pub fn tick(alias: &str) {
    let alias = super::clean_alias(alias);
    let Some(store) = store() else { return };
    // The heartbeat is written by its own thread (`ensure_heartbeat_thread`), so a tick held up
    // below cannot make the hub look dead. The first tick of a stretch writes it at once.
    let first = {
        let mut p = progress();
        let first = p
            .last_begin
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(10));
        p.alias.clone_from(&alias);
        first
    };
    let lane = LaneGuard::begin(false, "heartbeat");
    {
        let mut gate = gate();
        *gate = true;
        if first && let Err(e) = store.write_relay_status(&alias) {
            drop(gate);
            note(format!("Couldn't write the heartbeat: {e}"));
        }
    }
    ensure_heartbeat_thread();
    lane.stage("recover_stale_claims");
    control::recover_stale_claims(store);
    let registered = state().registered.is_some();
    if cfg!(any(unix, windows)) && !registered {
        lane.stage("register");
        register(&alias);
    }
    // The registered pipe has to be listening: a chat that finds the hub in the
    // registry may post to it. (Idempotent; a no-op until the reply hub exists.)
    #[cfg(windows)]
    {
        if state().registered.is_some() {
            lane.stage("reply_address");
            let _ = super::deliver_claude::reply_address("hub");
        }
    }
    lane.stage("spawn_background");
    static BACKGROUND: AtomicBool = AtomicBool::new(false);
    if BACKGROUND
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("chat-tick".into())
        .spawn(move || {
            background(store);
            BACKGROUND.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        BACKGROUND.store(false, Ordering::Release);
    }
}

/// The slow part of a tick: may take as long as ssh does.
fn background(store: &Store) {
    let lane = LaneGuard::begin(true, "local_chats");
    let _ = local_chats(store, SESSION_SCAN_EVERY);
    lane.stage("restore_reply_listeners");
    restore_reply_listeners(store);
    lane.stage("retry_outbound");
    retry_outbound(store);
    lane.stage("receipts");
    super::receipts::background(store);
    lane.stage("links");
    let due = state()
        .links_asked
        .is_none_or(|at| at.elapsed() >= Duration::from_secs(60));
    if due {
        state().links_asked = Some(Instant::now());
        let answers = links::list_all(store);
        let mut remote: Vec<StatusChat> = Vec::new();
        for r in &answers {
            if let Ok(listing) = &r.listing {
                remote.extend(listing.chats.iter().map(|c| StatusChat {
                    name: c.name.clone(),
                    kind: c.kind.clone(),
                    status: chat_status(&c.kind, &c.status),
                    device: r.link.device.clone(),
                    local: false,
                    cwd: c.cwd.clone(),
                    updated_ms: c.updated_ms,
                    liveness: c.liveness.clone(),
                    unread: 0,
                    evicted: 0,
                }));
            }
        }
        let recorded = links::link_status(store);
        let polled: Vec<StatusLink> = answers
            .into_iter()
            .map(|r| {
                let seen = recorded
                    .iter()
                    .find(|s| s.device.eq_ignore_ascii_case(&r.link.device));
                // A computer on the device link has no ssh destination to show.
                let ssh = if super::mesh::mesh_id(&r.link).is_some() {
                    "device link".to_string()
                } else {
                    r.link.ssh.clone()
                };
                StatusLink {
                    last_ok_ms: seen.and_then(|s| s.last_ok_ms),
                    last_error_ms: seen.and_then(|s| s.last_error_ms),
                    device: r.link.device,
                    ssh,
                    chats: r.listing.as_ref().ok().map(|l| l.chats.len()),
                    error: r.listing.err(),
                }
            })
            .collect();
        let mut st = state();
        st.link_state = polled;
        st.remote_chats = remote;
    }
}

// ---- replies -------------------------------------------------------------------------

/// The link a reply to `device` goes over: matched by device id, else by name. There is
/// no "only link" guess: an unmatched reply is reported, not sent to a stranger.
fn link_for(store: &Store, device: &str) -> Option<Link> {
    links::find_by_device_id(store, device).or_else(|| links::find(store, device))
}

/// Serializes queue edits (the outbox is a list the store drains and refills).
fn outbox() -> MutexGuard<'static, ()> {
    static OUTBOX: Mutex<()> = Mutex::new(());
    OUTBOX.lock().unwrap_or_else(|p| p.into_inner())
}

fn queue_reply(store: &Store, env: &Envelope) {
    let _lock = outbox();
    let _ = store.queue_outbound(env);
}

fn unqueue_reply(store: &Store, id: &str) {
    let _lock = outbox();
    store.remove_outbound(id);
}

/// Post one reply over its link, signed for it (`chat::seal`). Ok means a receipt came back
/// (so it is not retried).
fn post_reply(store: &Store, link: &Link, envelope: &Envelope) -> Result<(), String> {
    let receipt = links::post(link, &super::seal(store, link, envelope.clone()))?;
    let status = receipt["status"].as_str().unwrap_or("");
    let parsed = ReceiptState::parse(status);
    if parsed != Some(ReceiptState::Refused) {
        store.note_sent(parsed.unwrap_or(ReceiptState::Unknown).as_str());
    }
    if parsed != Some(ReceiptState::Delivered) {
        note(format!(
            "{}: reply {}: {}",
            link.device,
            status,
            receipt["detail"].as_str().unwrap_or("")
        ));
    }
    Ok(())
}

/// Retry queued replies that were never posted (at most three attempts each, counted in
/// memory; a hub restart starts the count again).
fn retry_outbound(store: &Store) {
    // The queue is read, not emptied: a reply stays in its file until it is sent. Taking every
    // file out and writing back the ones not sent was a forced write per waiting reply on
    // every tick, for as long as it waited.
    let pending = {
        let _lock = outbox();
        store.outbound()
    };
    // Attempt counts of replies no longer queued (posted, or their file removed) are dropped, so
    // the map holds at most the queued replies, not every reply that ever failed (issue #501).
    {
        let queued: HashSet<&str> = pending.iter().map(|env| env.id.as_str()).collect();
        state()
            .outbound_attempts
            .retain(|id, _| queued.contains(id.as_str()));
    }
    for env in pending {
        let skip = {
            let mut st = state();
            let tries = st.outbound_attempts.get(&env.id).copied().unwrap_or(0);
            if st.outbound_inflight.contains(&env.id) || tries >= MAX_REPLY_ATTEMPTS {
                true
            } else {
                st.outbound_attempts.insert(env.id.clone(), tries + 1);
                st.outbound_inflight.insert(env.id.clone());
                false
            }
        };
        if skip {
            continue;
        }
        let result = match link_for(store, &env.to.device) {
            Some(link) => post_reply(store, &link, &env),
            None => Err(format!(
                "No link named {} to send a reply over.",
                env.to.device
            )),
        };
        let mut st = state();
        st.outbound_inflight.remove(&env.id);
        match result {
            Ok(()) => {
                st.outbound_attempts.remove(&env.id);
                drop(st);
                unqueue_reply(store, &env.id);
            }
            Err(e) => {
                let tries = st.outbound_attempts.get(&env.id).copied().unwrap_or(0);
                if tries >= MAX_REPLY_ATTEMPTS {
                    st.last_error = Some(format!("Gave up sending a reply: {e}"));
                }
            }
        }
    }
}

/// A local chat answered a message it was pushed (`ReplyHub`): send the
/// answer to the chat that wrote, wherever it runs. The chat that wrote is found
/// from the route saved when the message was delivered (by the id the reply answers),
/// else from the address the reply arrived on.
pub fn on_local_reply(reply: ReplyMessage) {
    let Some(store) = store() else { return };
    if let Some(off) = super::chat_off_detail(store) {
        return note(off);
    }
    let sessions = local_chats(store, Duration::from_secs(10));
    let Some(from) = sessions.iter().find(|s| is_chat(s, &reply.from_session_id)) else {
        return note(
            "A reply came from a chat the app does not list here; it was not relayed.".to_string(),
        );
    };
    let routed = [reply.in_reply_to.as_deref(), Some(reply.msg_id.as_str())]
        .into_iter()
        .flatten()
        .filter(|id| !id.is_empty())
        .find_map(|id| store.reply_route(id))
        .filter(|r| r.to_session == from.id);
    let (device, session) = match routed {
        Some(route) => (route.from_device, route.from_session),
        None => match reply.peer_key.rsplit_once(':') {
            Some((device, session)) => (device.to_string(), session.to_string()),
            // A reply on a pipe shared by every peer after a hub restart: the one chat that
            // wrote to this chat in the route window. A reply that names a message (a route
            // that is not ours), or a window with more than one writer, cannot say which chat
            // it answers, and is never sent to a guess.
            None => {
                let named = [reply.in_reply_to.as_deref(), Some(reply.msg_id.as_str())]
                    .into_iter()
                    .flatten()
                    .any(|id| !id.is_empty() && store.reply_route(id).is_some());
                let since = super::envelope::now_ms().saturating_sub(ROUTE_WINDOW_MS);
                let mut writers: Vec<(String, String)> = store
                    .reply_routes_since(since)
                    .into_iter()
                    .filter(|r| r.to_session == from.id)
                    .map(|r| (r.from_device, r.from_session))
                    .collect();
                writers.sort();
                writers.dedup();
                match writers.as_slice() {
                    [(device, session)] if !named => (device.clone(), session.clone()),
                    [] => return,
                    _ => {
                        return note(super::brand::branded(
                            "A reply could not be matched to one chat (it names no message \
                             the app delivered, and more than one chat wrote to this one); \
                             it was not relayed. Use `@cli@ chat reply <message-id>`.",
                        ));
                    }
                }
            }
        },
    };
    let alias = store
        .relay_status()
        .map(|s| s.alias)
        .unwrap_or_else(|| "this computer".to_string());
    let mut envelope = match Envelope::new(
        Sender {
            device: alias.clone(),
            session: from.id.clone(),
            name: format!("{} on {}", from.name, alias),
            harness: super::harness_of_chat(store, &from.id),
            device_id: super::local_device_id(store),
        },
        Target {
            device: device.clone(),
            session: session.clone(),
        },
        reply.text,
    ) {
        Ok(envelope) => envelope,
        Err(e) => return note(format!("Couldn't send a reply: {e}")),
    };
    // No user words and not marked verified: the reply listener takes the replying
    // chat from the frame's own `from-session`, and nothing proves which process wrote the
    // frame, so the reply is shown as unverified. (A reply sent with `<cli> chat reply` from
    // inside the chat is proven from its process and carries them.)
    if device == alias {
        match sessions.iter().find(|s| s.id == session) {
            Some(target) => {
                let receipt = deliver_local_via(store, target, &envelope, None);
                if receipt.state != ReceiptState::Refused {
                    store.note_sent(receipt.state.as_str());
                }
            }
            None => note("A reply's chat is no longer open.".to_string()),
        }
        return;
    }
    let link = match link_for(store, &device) {
        Some(link) => link,
        None => {
            // The sender's own name for itself can differ from this computer's name for
            // the link: resolve as `<cli> chat reply` does (this runs on the reply's own
            // thread, so a slow ssh listing blocks neither the listener nor deliveries).
            let me = super::Identity {
                alias: alias.clone(),
            };
            let address = match super::reply_address(store, &me, &device, &session) {
                Ok(address) => address,
                Err(why) => {
                    return note(format!(
                        "No link named {device} to send a reply over. {why}"
                    ));
                }
            };
            let Some(link) = address
                .rsplit_once(':')
                .and_then(|(name, _)| links::find(store, name))
            else {
                return note(format!("No link named {device} to send a reply over."));
            };
            envelope.to.device = link.device.clone();
            link
        }
    };
    queue_reply(store, &envelope);
    state().outbound_inflight.insert(envelope.id.clone());
    let result = post_reply(store, &link, &envelope);
    state().outbound_inflight.remove(&envelope.id);
    match result {
        Ok(()) => unqueue_reply(store, &envelope.id),
        Err(e) => note(format!("Couldn't send a reply (it will be retried): {e}")),
    }
}

// ---- control -------------------------------------------------------------------------

/// Counts control requests being handled; the guard frees its slot on drop.
struct WorkerSlot;

static WORKERS: AtomicUsize = AtomicUsize::new(0);

impl WorkerSlot {
    fn take() -> Option<WorkerSlot> {
        if WORKERS.fetch_add(1, Ordering::AcqRel) >= MAX_CONTROL_WORKERS {
            WORKERS.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(WorkerSlot)
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One lock per target chat: requests for the same chat run one at a time.
fn session_lock(session: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if map.len() > 256 {
        map.retain(|_, lock| Arc::strong_count(lock) > 1);
    }
    map.entry(session.to_string()).or_default().clone()
}

/// Answer a request from the CLI. `deliver` `{session, envelope, reply_socket?}`
/// pushes an envelope into a chat here and returns `{ok, status, detail}`. Refused
/// while Chat is off; at most eight requests run at once (more answer
/// "unknown: hub busy"), and requests for one chat run one at a time.
pub fn handle_control(request: &Request) -> Value {
    let value = handle_control_inner(request);
    // A delivery that kept the message nowhere: when the CLI already told its sender the hub
    // had it, keep it in the chat's inbox (once) rather than lose it under that claim.
    if request.op == "deliver"
        && let Some(store) = store()
    {
        let kept = value["ok"].as_bool() == Some(true)
            && matches!(
                value["status"].as_str(),
                Some("delivered" | "queued" | "sent" | "held")
            );
        if !kept {
            control::settle_unkept(store, request);
        }
    }
    value
}

fn handle_control_inner(request: &Request) -> Value {
    let Some(store) = store() else {
        return json!({"ok": false, "error": "The chat store is not available."});
    };
    if let Some(detail) = super::chat_off_detail(store) {
        return json!({"ok": true, "status": "refused", "detail": detail});
    }
    let Some(_slot) = WorkerSlot::take() else {
        return json!({"ok": true, "status": "unknown", "detail": "hub busy, try again"});
    };
    match request.op.as_str() {
        "deliver" => {
            let session_id = request.args["session"].as_str().unwrap_or("");
            let envelope = match request.args["envelope"]
                .as_str()
                .ok_or_else(|| "missing envelope".to_string())
                .and_then(links::decode_envelope_local)
            {
                Ok(env) => env,
                Err(e) => return json!({"ok": false, "error": format!("Bad request: {e}")}),
            };
            let lock = session_lock(session_id);
            let _turn = lock.lock().unwrap_or_else(|p| p.into_inner());
            let sessions = local_chats(store, Duration::from_secs(2));
            let Some(session) = sessions.iter().find(|s| s.id == session_id) else {
                return json!({"ok": true, "status": "refused",
                              "detail": "That chat isn't open on this computer any more."});
            };
            let reply_socket = request.args["reply_socket"].as_str();
            let receipt = deliver_local_via(store, session, &envelope, reply_socket);
            json!({"ok": true, "status": receipt.state.as_str(), "detail": receipt.detail})
        }
        other => json!({"ok": false, "error": format!("Unknown request: {other}")}),
    }
}

// ---- status ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusLink {
    pub device: String,
    pub ssh: String,
    /// Chats listed there at the last poll, or None when it could not be asked.
    pub chats: Option<usize>,
    pub error: Option<String>,
    /// When the link last answered (ms), when it ever did.
    pub last_ok_ms: Option<u64>,
    /// When the link last failed (ms), when it ever did.
    pub last_error_ms: Option<u64>,
}

/// One chat as the page lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChat {
    pub name: String,
    /// "claude" or "codex".
    pub kind: String,
    /// Claude: busy or idle (live). Codex: active (written to in the last 10 min) or idle.
    pub status: String,
    pub device: String,
    pub local: bool,
    pub cwd: String,
    /// Milliseconds since the epoch of the last activity, when known.
    pub updated_ms: Option<u64>,
    /// "live", "stale" or "unknown".
    pub liveness: String,
    /// Unread messages in this chat's chat inbox (chats here only).
    pub unread: usize,
    /// Messages the inbox size cap dropped before they were read (chats here only).
    pub evicted: u64,
}

/// Claude: busy/idle from its registry. Codex: active when its thread was written to in the
/// last ten minutes, else idle (the roster decides; no open-chat registry exists).
fn chat_status(_kind: &str, status: &str) -> String {
    status.to_string()
}

/// What the hub's page shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub local_chats: usize,
    pub links: Vec<StatusLink>,
    /// Every chat, here and on linked computers, as `"<name> on <device>"` material.
    pub chats: Vec<StatusChat>,
    pub last_error: Option<String>,
    /// Last send/receive times (ms) and counts; owner app pulses its Send ring on a change.
    pub activity: super::store::Activity,
}

/// A hash of what `status` shows from memory (local chats, linked chats, link results, the last
/// error). The hub's tick compares it before marking the sharing state dirty, so a tick that
/// changed nothing builds and writes nothing. Read-only; it touches no file and starts no process.
pub fn status_fingerprint() -> u64 {
    use std::hash::{Hash, Hasher};
    let state = state();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", state.sessions).hash(&mut hasher);
    format!("{:?}", state.remote_chats).hash(&mut hasher);
    format!("{:?}", state.link_state).hash(&mut hasher);
    state.last_error.hash(&mut hasher);
    hasher.finish()
}

pub fn status() -> Status {
    let state = state();
    let alias = store()
        .and_then(|s| s.relay_status())
        .map(|s| s.alias)
        .unwrap_or_else(|| "this computer".to_string());
    let mut chats: Vec<StatusChat> = state
        .sessions
        .iter()
        .map(|s| {
            let detail = store()
                .map(|st| st.unread_detail(&s.id))
                .unwrap_or_default();
            StatusChat {
                name: s.name.clone(),
                kind: s.kind.clone(),
                status: chat_status(&s.kind, &s.status),
                device: alias.clone(),
                local: true,
                cwd: s.cwd.clone(),
                updated_ms: s.updated_ms,
                liveness: s.liveness.clone(),
                unread: detail.unread,
                evicted: detail.evicted,
            }
        })
        .collect();
    chats.extend(state.remote_chats.iter().cloned());
    Status {
        chats,
        local_chats: state.sessions.len(),
        links: state.link_state.clone(),
        last_error: state.last_error.clone(),
        activity: store().map(|s| s.activity()).unwrap_or_default(),
    }
}
