//! A tiny local control channel from the `<cli> chat` CLI to the running
//! The host app hub.
//!
//! Files in the user's own the host app state folder, so only that user can use it:
//! the CLI drops `control/requests/<id>.json` (`{id, op, args, ts}`), the hub
//! claims it by renaming it into `control/claimed/` (it is not deleted until
//! the work is answered) and writes `control/replies/<id>.json` atomically,
//! which the CLI reads and removes. A claim the hub never answered (it died
//! mid-request) is moved back by `recover_stale_claims` after 60 s, so a request
//! is processed at least once; a delivery reaches its chat at most once all the same, since the
//! chat's delivery record answers a repeat with the first receipt (`chat::deliver_local_via`).
//! A caller that stops waiting while the hub holds the claim is told the request was accepted
//! (`ACCEPTED`), not that its outcome is unknown. Same on macOS and Windows; no sockets or pipes.
//!
//! The only operation today is `deliver` (args `{session, envelope,
//! reply_socket?}`): on macOS a Claude chat accepts posts only from a
//! registered peer, which the hub is (see `hub`). `peers`, `send` to other
//! computers, `status` and `inbox` need no hub.

use super::envelope::now_ms;
use super::store::Store;
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// A request nobody answered in this long is dropped.
const REQUEST_TTL_MS: u64 = 300_000;
/// The hub must claim a request within this long, else it is not listening.
const CLAIM_WITHIN: Duration = Duration::from_secs(10);
/// A claimed request with no reply after this long is given back to the queue.
const CLAIM_STALE: Duration = Duration::from_secs(60);
/// Replies nobody collected are removed after this long.
const REPLY_KEEP: Duration = Duration::from_secs(300);
/// The hub's heartbeat (written every ~2 s while its chat runs) must be this fresh.
const HUB_FRESH_MS: u64 = 30_000;
/// The hub's request reader must have beaten this recently for the CLI to call it alive.
const READER_FRESH_MS: u64 = 20_000;
/// The reader's beat is written at most this often.
const READER_BEAT_EVERY_MS: u64 = 2_000;
/// A request file larger than this is discarded unread.
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

/// The payload-free wake-up the CLI posts after it drops a request, so the hub claims it at
/// once instead of polling the folder: a Darwin notification on macOS, the named auto-reset event
/// `Local\dev.orthic.pulse.chat.control` on Windows. The hub (engine/src/share.rs) listens for
/// it and still looks in the folder every few seconds, so a missed wake-up delays a request, never loses it.
pub fn notification() -> &'static str {
    &super::brand::brand().control_notification
}

#[cfg(target_os = "macos")]
fn wake_hub() {
    unsafe extern "C" {
        fn notify_post(name: *const std::ffi::c_char) -> u32;
    }
    if let Ok(name) = std::ffi::CString::new(notification()) {
        // SAFETY: a NUL-terminated name; notify_post takes no other input.
        unsafe { notify_post(name.as_ptr()) };
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        attributes: *const std::ffi::c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn SetEvent(event: *mut std::ffi::c_void) -> i32;
    fn CloseHandle(object: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
fn wake_hub() {
    let name: Vec<u16> = format!("Local\\{}", notification())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: a NUL-terminated name; the handle is closed straight after the signal. The hub
    // keeps its own handle to the same event open, so the signal is not lost.
    unsafe {
        let event = CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
        if !event.is_null() {
            SetEvent(event);
            CloseHandle(event);
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn wake_hub() {}

/// Errors from `call` that say the outcome is not known start with this; the request
/// may still be (or have been) carried out. Callers must not report such a send as
/// held or refused.
pub const UNKNOWN_OUTCOME: &str = "Unknown outcome";

/// Whether a `call` error means the request's outcome is not known.
pub fn is_unknown_outcome(error: &str) -> bool {
    error.starts_with(UNKNOWN_OUTCOME)
}

/// Errors from `call` that say the hub took the request and is still carrying it out start
/// with this. A claim is a file the hub gives itself back if it dies before answering, and a
/// delivery that expires unanswered, or ends in an outcome that kept nothing after the CLI
/// stopped waiting, is kept in its chat's inbox (`recover_stale_claims`, `settle_unkept`), so
/// the request is not lost; only its final outcome was not awaited.
pub const ACCEPTED: &str = "Accepted";

/// Whether a `call` error means the hub took the request and is still carrying it out.
pub fn is_accepted(error: &str) -> bool {
    error.starts_with(ACCEPTED)
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: String,
    pub op: String,
    pub args: Value,
}

fn control_dir(store: &Store) -> PathBuf {
    store.root().join("control")
}

fn requests_dir(store: &Store) -> PathBuf {
    control_dir(store).join("requests")
}

fn claimed_dir(store: &Store) -> PathBuf {
    control_dir(store).join("claimed")
}

fn replies_dir(store: &Store) -> PathBuf {
    control_dir(store).join("replies")
}

fn reader_beat_path(store: &Store) -> PathBuf {
    control_dir(store).join("reader.json")
}

/// Hub side: the request reader (the thread that runs `take_requests`) says it is alive. The
/// relay heartbeat comes from another thread, so it cannot say this: a reader that died or hung
/// left the heartbeat fresh while no request was ever claimed. Written at
/// most every two seconds.
pub fn beat_reader(store: &Store) {
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = now_ms();
    let last = LAST.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(last) < READER_BEAT_EVERY_MS {
        return;
    }
    LAST.store(now, std::sync::atomic::Ordering::Relaxed);
    let dir = control_dir(store);
    if rightkit_fs::private_cached::ensure_private_dir_repairing(&dir).is_err() {
        return;
    }
    let body = json!({"pid": std::process::id(), "ts": now}).to_string();
    let _ = rightkit_fs::atomic_write_with_options(
        &reader_beat_path(store),
        rightkit_fs::AtomicWriteOptions {
            durable: false,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| io::Write::write_all(file, body.as_bytes()),
    );
}

/// CLI side: how long ago the hub's request reader last beat, `None` when it never wrote one
/// (a hub from before the beat existed, which is then not judged by it).
fn reader_silence_ms(store: &Store) -> Option<u64> {
    let bytes = fs::read(reader_beat_path(store)).ok()?;
    let ts = serde_json::from_slice::<Value>(&bytes).ok()?["ts"].as_u64()?;
    Some(now_ms().saturating_sub(ts))
}

/// Whether the host app hub's chat is running (its relay heartbeat is fresh).
pub fn hub_running(store: &Store) -> bool {
    store
        .relay_status()
        .is_some_and(|s| now_ms().saturating_sub(s.ts) < HUB_FRESH_MS)
}

/// Ask the hub to run `op` and wait up to `wait` for its reply. An `Err` that
/// `is_unknown_outcome` means the request may still have been carried out.
pub fn call(store: &Store, op: &str, args: Value, wait: Duration) -> Result<Value, String> {
    if !hub_running(store) {
        return Err("Start the app and try again.".to_string());
    }
    let id = format!(
        "{}-{:x}-{}",
        std::process::id(),
        now_ms(),
        super::random_hex(3)
    );
    let request = requests_dir(store).join(format!("{id}.json"));
    let claimed = claimed_dir(store).join(format!("{id}.json"));
    let reply = replies_dir(store).join(format!("{id}.json"));
    let body = json!({"id": id, "op": op, "args": args, "ts": now_ms()});
    let request_dir = request.parent().unwrap_or(&request);
    // A folder an elevated run made (owned by Administrators) is moved aside and made again
    // rather than refused for good.
    rightkit_fs::private_cached::ensure_private_dir_repairing(request_dir)
        .map_err(|e| e.to_string())?;
    if let Some(parent) = request_dir.parent() {
        let _ = rightkit_fs::ensure_private_dir(parent);
    }
    // Owner-only from creation on Unix; Windows keeps the parent's inherited access.
    rightkit_fs::atomic_write_with_options(
        &request,
        rightkit_fs::AtomicWriteOptions {
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| io::Write::write_all(file, body.to_string().as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    wake_hub();
    let started = Instant::now();
    loop {
        if let Ok(bytes) = fs::read(&reply)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            let _ = fs::remove_file(&reply);
            return Ok(value);
        }
        let elapsed = started.elapsed();
        if elapsed >= wait {
            // Never claimed: withdraw it. Claimed: the hub has it and is still working on it.
            if fs::remove_file(&request).is_ok() {
                return Err(format!(
                    "{UNKNOWN_OUTCOME}: The app did not answer within {} s; the request was \
                     withdrawn before the hub took it.",
                    wait.as_secs()
                ));
            }
            // The answer may have landed between the last look and the withdrawal.
            if let Ok(bytes) = fs::read(&reply)
                && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            {
                let _ = fs::remove_file(&reply);
                return Ok(value);
            }
            if claimed.exists() {
                // One exclusive marker settles who speaks last: the CLI (it now reports the
                // message accepted, and the hub keeps it in the chat's inbox should its
                // delivery then fail) or the hub (it is answering, so the CLI reports that).
                if settle(store, &id) {
                    return Err(format!(
                        "{ACCEPTED}: The app here has the message and is still delivering it (no \
                         verdict from the chat within {} s); it reaches the chat once, or is \
                         kept in the chat's inbox.",
                        wait.as_secs()
                    ));
                }
                let grace = Instant::now();
                while grace.elapsed() < SETTLE_GRACE {
                    if let Ok(bytes) = fs::read(&reply)
                        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    {
                        let _ = fs::remove_file(&reply);
                        return Ok(value);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            return Err(format!(
                "{UNKNOWN_OUTCOME}: The app did not answer within {} s; the request may still be \
                 carried out.",
                wait.as_secs()
            ));
        }
        if elapsed >= CLAIM_WITHIN && request.exists() {
            let withdrawn = fs::remove_file(&request).is_ok();
            // Logged on the CLI's side, where the Dell had no trace of it: the hub did not
            // claim the request in time, whether its reader had beaten lately, and whether the
            // request was withdrawn. Numbers and flags only.
            let silent_ms = reader_silence_ms(store);
            super::sendlog::event(
                rightkit_logs::Level::Warn,
                "chat.control.unclaimed",
                json!({
                    "waitedMs": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                    "readerSilentMs": silent_ms,
                    "withdrawn": withdrawn,
                    "pid": std::process::id(),
                }),
            );
            if let Some(silent) = silent_ms.filter(|s| *s >= READER_FRESH_MS) {
                return Err(format!(
                    "{UNKNOWN_OUTCOME}: the app hub is running but its request reader has \
                     not answered for {} s, so it is not picking up requests ({}).",
                    silent / 1000,
                    if withdrawn {
                        "the request was withdrawn, so nothing was done"
                    } else {
                        "the hub took it at the last moment and may still carry it out"
                    }
                ));
            }
            return Err(format!(
                "{UNKNOWN_OUTCOME}: the app hub did not pick the request up within {} s ({}).",
                CLAIM_WITHIN.as_secs(),
                if withdrawn {
                    "it was withdrawn, so nothing was done"
                } else {
                    "the hub took it at the last moment and may still carry it out"
                }
            ));
        }
        // A reply usually lands within a second (a local delivery); a slow one (ssh) is awaited
        // with longer and longer pauses, so a long wait is not a busy one.
        let pause = match elapsed.as_secs() {
            0 => 100,
            1..=9 => 250,
            _ => 500,
        };
        std::thread::sleep(Duration::from_millis(pause));
    }
}

/// How long the CLI waits for an answer the hub has begun writing.
const SETTLE_GRACE: Duration = Duration::from_secs(3);

fn settled_marker(store: &Store, id: &str) -> PathBuf {
    claimed_dir(store).join(format!("{id}.settled"))
}

/// Create the exclusive marker that settles a request: true for the first caller only. The CLI
/// takes it when it stops waiting on a claimed request; the hub takes it before it answers
/// with an outcome that kept nothing. Whoever loses knows the other already decided.
fn settle(store: &Store, id: &str) -> bool {
    let path = settled_marker(store, id);
    if rightkit_fs::private_cached::ensure_private_dir_repairing(&claimed_dir(store)).is_err() {
        return false;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .is_ok()
}

/// Hub side: a request ended in an outcome that kept the message nowhere (the chat closed, the
/// hub busy, a refusal). When the CLI already stopped waiting and told its sender the hub had
/// the message, the message is kept in the chat's inbox, once, so that claim stays true.
/// Otherwise the CLI is still waiting and reports the outcome itself.
pub fn settle_unkept(store: &Store, request: &Request) {
    if !settle(store, &request.id) {
        let value = json!({"op": request.op, "args": request.args});
        keep_dropped(store, &value);
    }
}

/// Request ids this process claimed and has not answered yet. Stale-claim recovery never gives
/// one of these back, however long its handler runs, so a slow request is not run twice.
/// The lock guards only this list, never a file call, so a stalled disk cannot hold up the hub's
/// tick. An id is registered before its rename into `claimed/` and dropped if the claim fails or
/// is not run (a `ClaimGuard`, so a panic mid-claim drops it too). Recovery reads the list without
/// holding it across I/O: a file in `claimed/` can only become live again by being renamed in
/// from `requests/`, which cannot happen while it sits in `claimed/` (ids are unique).
static LIVE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn unregister(id: &str) {
    if let Ok(mut ids) = LIVE.lock() {
        ids.retain(|known| known != id);
    }
}

fn is_live(id: &str) -> bool {
    // A poisoned list counts as live: recovery then leaves the claim alone.
    match LIVE.lock() {
        Ok(ids) => ids.iter().any(|known| known == id),
        Err(_) => true,
    }
}

/// A claim registered as live until it is handed to a handler (`kept`); dropped otherwise,
/// including by a panic, so no id stays live without a handler.
struct ClaimGuard {
    id: String,
    kept: bool,
}

impl ClaimGuard {
    fn register(id: String) -> Option<Self> {
        LIVE.lock().ok()?.push(id.clone());
        Some(Self { id, kept: false })
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        if !self.kept {
            unregister(&self.id);
        }
    }
}

/// What one claiming pass took: the requests it claimed, always to be run, and the error that
/// stopped it early, if any (claims made before the error are not lost).
pub struct Taken {
    pub requests: Vec<Request>,
    pub error: Option<io::Error>,
}

/// Hub side: claim every waiting request by renaming it into `claimed/` (the claim
/// stays until `reply` answers it, or `recover_stale_claims` gives it back). An error reading
/// the requests folder or making the claims folder is returned, never read as "nothing waiting",
/// so the reader is not counted alive while it cannot pick anything up. A folder that does
/// not exist yet means nothing was ever sent.
pub fn take_requests(store: &Store) -> Taken {
    // Each claim stays guarded until the whole pass returns: a panic anywhere in it drops every
    // guard, so no id stays live without a handler (its file is returned by stale recovery).
    let mut claimed = Vec::new();
    let error = claim_into(store, &mut claimed).err();
    let requests = claimed
        .into_iter()
        .map(|(request, mut guard): (Request, ClaimGuard)| {
            guard.kept = true;
            request
        })
        .collect();
    Taken { requests, error }
}

fn claim_into(store: &Store, out: &mut Vec<(Request, ClaimGuard)>) -> io::Result<()> {
    let listing = match fs::read_dir(requests_dir(store)) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in listing {
        let path = entry?.path();
        if path.extension().is_some_and(|x| x == "json") {
            files.push(path);
        }
    }
    files.sort();
    let claims = claimed_dir(store);
    if !files.is_empty() {
        rightkit_fs::private_cached::ensure_private_dir_repairing(&claims)
            .map_err(io::Error::other)?;
    }
    for file in files {
        let Some(name) = file.file_name() else {
            continue;
        };
        let claim = claims.join(name);
        // The rename is the claim: only one claimer wins, and the CLI's own
        // withdrawal (a delete) loses cleanly to it or beats it.
        // Losing the race (the CLI withdrew it, or it is already claimed) is normal; any other
        // failure is an error the reader reports, never an empty pass.
        // Registered as live before the rename; the guard drops it again unless the request is run.
        let Some(stem) = claim.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(guard) = ClaimGuard::register(stem) else {
            return Err(io::Error::other("claim list poisoned"));
        };
        // Losing the race (the CLI withdrew it, or it is already claimed) is normal; any other
        // failure is an error the reader reports, never an empty pass.
        if let Err(error) = rightkit_fs::rename_exclusive(&file, &claim) {
            match error.kind() {
                Some(io::ErrorKind::NotFound | io::ErrorKind::AlreadyExists) => continue,
                _ => return Err(io::Error::other(error.to_string())),
            }
        }
        if let Ok(opened) = fs::OpenOptions::new().write(true).open(&claim) {
            let _ = opened.set_modified(SystemTime::now());
        }
        let oversize = fs::metadata(&claim).is_ok_and(|m| m.len() > MAX_REQUEST_BYTES);
        let value: Option<Value> = if oversize {
            None
        } else {
            fs::read(&claim)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        };
        let id = value
            .as_ref()
            .and_then(|v| v["id"].as_str())
            .unwrap_or("")
            .to_string();
        let stem_matches = claim
            .file_stem()
            .is_some_and(|s| s.to_string_lossy() == id.as_str());
        let safe = !id.is_empty()
            && stem_matches
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let fresh = value.as_ref().is_some_and(|v| {
            v["ts"]
                .as_u64()
                .is_some_and(|ts| now_ms().saturating_sub(ts) < REQUEST_TTL_MS)
        });
        if safe
            && !fresh
            && let Some(value) = &value
        {
            keep_dropped(store, value);
        }
        let Some(value) = value.filter(|_| safe && fresh) else {
            let _ = fs::remove_file(&claim);
            continue;
        };
        out.push((
            Request {
                id,
                op: value["op"].as_str().unwrap_or("").to_string(),
                args: value["args"].clone(),
            },
            guard,
        ));
    }
    Ok(())
}

/// Hub side: answer a claimed request (the reply is written atomically, then the claim
/// is released).
pub fn reply(store: &Store, id: &str, value: &Value) {
    let path = replies_dir(store).join(format!("{id}.json"));
    let dir = path.parent().unwrap_or(&path);
    if rightkit_fs::private_cached::ensure_private_dir_repairing(dir).is_ok() {
        if let Some(parent) = dir.parent() {
            let _ = rightkit_fs::ensure_private_dir(parent);
        }
        let _ = rightkit_fs::atomic_write_with_options(
            &path,
            rightkit_fs::AtomicWriteOptions {
                privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                ..Default::default()
            },
            |file| io::Write::write_all(file, value.to_string().as_bytes()),
        );
    }
    let _ = fs::remove_file(claimed_dir(store).join(format!("{id}.json")));
    unregister(id);
}

/// A request dropped unanswered (it expired while the hub was stopped or behind): a delivery's
/// message is kept in its chat's inbox, once (`chat::keep_unanswered`), since its sender may
/// have been told the hub had it. Nothing is kept while Chat is off.
fn keep_dropped(store: &Store, value: &Value) {
    if value["op"] != "deliver" || !store.chat_enabled() {
        return;
    }
    let Some(session) = value["args"]["session"].as_str().filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(env) = value["args"]["envelope"]
        .as_str()
        .and_then(|text| super::links::decode_envelope_local(text).ok())
    else {
        return;
    };
    super::keep_unanswered(store, session, &env);
}

fn older_than(path: &Path, limit: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > limit)
}

/// Hub side, once per tick: give back every request claimed more than 60 s ago that
/// has no reply (the hub that claimed it died or hung), drop claims that are answered
/// or past their time to live, and sweep replies nobody collected.
pub fn recover_stale_claims(store: &Store) {
    if let Ok(listing) = fs::read_dir(claimed_dir(store)) {
        for entry in listing.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|x| x == "settled") && older_than(&path, REPLY_KEEP) {
                let _ = fs::remove_file(&path);
                continue;
            }
            if path.extension().is_none_or(|x| x != "json") || !older_than(&path, CLAIM_STALE) {
                continue;
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            // A claim this process is still handling is not stale, however old.
            if path
                .file_stem()
                .is_some_and(|stem| is_live(&stem.to_string_lossy()))
            {
                continue;
            }
            let answered = replies_dir(store).join(name).exists();
            let value = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let expired = value
                .as_ref()
                .and_then(|v| v["ts"].as_u64())
                .is_none_or(|ts| now_ms().saturating_sub(ts) >= REQUEST_TTL_MS);
            if answered || expired {
                if !answered && let Some(value) = &value {
                    keep_dropped(store, value);
                }
                let _ = fs::remove_file(&path);
            } else {
                let _ = rightkit_fs::rename_exclusive(&path, &requests_dir(store).join(name));
            }
        }
    }
    if let Ok(listing) = fs::read_dir(replies_dir(store)) {
        for entry in listing.flatten() {
            if older_than(&entry.path(), REPLY_KEEP) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
