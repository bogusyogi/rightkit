//! Files Chat keeps under `<the host app state dir>/chat`: per-chat inboxes
//! (JSON lines with a read cursor), linked computers (`links.json`),
//! registered chats, the hub's heartbeat, the last send/receive times
//! (`activity.json`, which owner app rings pulse on), the duplicate journal
//! (`journal.jsonl`), the per-chat delivery record (`delivered/`), persisted reply routes and
//! the outbound retry queue.
//!
//! Every replacement is a temporary file renamed into place; appends and
//! read-modify-write cycles happen under a lock. The lock is RightKit's `FileLock` on a
//! per-target lock file: an operating-system advisory lock (a POSIX record lock on unix,
//! `LockFileEx` on Windows) that the kernel drops when the holder dies, so nothing is ever
//! stolen and the lock file is never deleted. Folders are created 0o700 and files 0o600 on unix, files are opened
//! without following links, and on Windows a link or reparse point is refused.

use super::envelope::{Envelope, now_ms};
use super::links::Link;
use super::receipts::Update;
use rightkit_fs::digest::sha256_bytes;
use rightkit_fs::{FileLock, FsError, LockOptions};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Per-inbox size cap; the oldest messages go first.
pub const INBOX_CAP_BYTES: u64 = 10 * 1024 * 1024;
/// A hub that has not written its heartbeat for this long counts as stopped.
pub const RELAY_STALE_MS: u64 = 30_000;
/// How often an unchanged relay heartbeat is rewritten: a third of `RELAY_STALE_MS`.
const RELAY_HEARTBEAT_MS: u64 = 10_000;
/// The last relay heartbeat this process wrote: (file, alias) and its `ts`.
static LAST_RELAY_WRITE: std::sync::Mutex<Option<((PathBuf, String), u64)>> =
    std::sync::Mutex::new(None);
/// The duplicate journal keeps this many entries; the oldest are dropped.
pub const JOURNAL_CAP: usize = 10_000;
/// Receipt updates kept per sending chat; the oldest are dropped.
pub const RECEIPT_CAP: usize = 500;
/// Receipt lists kept at most; past it the least recently written are removed.
pub const RECEIPT_FILES_CAP: usize = 500;
/// A receipt list not written for this long is removed.
pub const RECEIPT_FILE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Refused held message ids kept per chat.
/// The chats that have been handed a host app Chat message (a JSON set of chat ids).
const CHATS_MET_FILE: &str = "chats-met.json";
const REFUSED_CAP: usize = 1_000;
/// At most this many undelivered replies wait in the outbound queue.
const OUTBOUND_CAP: usize = 1_000;
/// Reply routes older than this are forgotten.
const ROUTE_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Reply route files kept at most, by count. Past it the least recently written go first. At the
/// Mac's rate (about 410 a day) the 30-day TTL binds at about 12,000 files, so this only caps a burst.
const ROUTE_MAX_FILES: usize = 10_000;
/// The reply-routes folder is scanned for expired routes at most this often.
const ROUTE_PRUNE_EVERY: Duration = Duration::from_secs(10 * 60);
/// Messages remembered per receiving chat in its delivery record (`begin_delivery`); the
/// oldest go first.
pub const DELIVERED_CAP: usize = 2_000;
/// A delivery record entry older than this is forgotten.
pub const DELIVERED_MAX_AGE_MS: u64 = 7 * 24 * 3600 * 1000;
/// A delivery still marked in progress after this long is taken as abandoned even when a
/// process with its pid runs (the pid was reused).
const DELIVERY_IN_PROGRESS_MAX_MS: u64 = 5 * 60 * 1000;

/// When this computer last sent or received a chat message, and how many so far.
/// The hub publishes it; owner app pulses the Send ring for a few seconds after either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(default)]
    pub sent_ms: u64,
    #[serde(default)]
    pub received_ms: u64,
    #[serde(default)]
    pub sent: u64,
    #[serde(default)]
    pub received: u64,
    /// The receipt state word (`delivered`, `held`, ...) of the latest send or receive.
    #[serde(default)]
    pub last_outcome: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxEntry {
    /// Position in this inbox, growing by one for every message.
    pub seq: u64,
    /// When this computer received it (ms).
    pub received: u64,
    pub env: Envelope,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cursor {
    /// Highest `seq` already read.
    #[serde(default)]
    read: u64,
    /// The `seq` the next message gets.
    #[serde(default)]
    next: u64,
}

/// A chat's receipt bookkeeping: what it has read, and what a hook notice already named.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ReceiptCursor {
    /// Highest receipt `seq` the chat has read.
    #[serde(default)]
    read: u64,
    /// Highest receipt `seq` a hook notice has already named.
    #[serde(default)]
    announced: u64,
    /// When the last hook notice went out (ms); 0 when none has.
    #[serde(default)]
    notice_ms: u64,
}

/// Unread messages of a chat, and how many unread ones the size cap has dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadDetail {
    pub unread: usize,
    /// Messages evicted by the inbox cap before they were read (all time).
    pub evicted: u64,
}

/// What the duplicate journal says about a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// Never seen from that origin.
    New,
    /// Seen with the same content.
    Duplicate,
    /// Seen with this id but different content.
    Conflict,
    /// The journal could not be read or written, so the message could not be recorded: the
    /// caller must not accept it (the sender retries the same id), or a retry could be delivered twice.
    Unavailable,
}

/// Where a reply to a delivered message goes: the sender's chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyRoute {
    /// The id of the message that was delivered here.
    pub msg_id: String,
    /// The sending computer's name.
    pub from_device: String,
    /// The sending chat.
    pub from_session: String,
    /// The chat here that received it (a reply must come from it).
    pub to_session: String,
    /// That chat's name when it was routed (empty for routes made before names were kept).
    #[serde(default)]
    pub to_name: String,
    pub created_ms: u64,
    /// The registry device id of the sender: the paired device the engine stamped
    /// (`Envelope::mesh`), or the device link that is an ssh sender's twin; `"local"` for a chat on
    /// this computer (`confirm::LOCAL_SENDER`); `None` when it cannot be resolved (an ssh link with
    /// no device-link twin, or a route saved before this field existed).
    /// `chat::confirm` reads the sender's role from the registry by this id.
    #[serde(default)]
    pub mesh_device_id: Option<String>,
    #[serde(default)]
    pub payload_hash: String,
    /// The process id of the Claude chat here that received it. On its own it proves
    /// nothing (pids are reused); it counts only together with `to_start`. `None` for a route
    /// saved before this field or for a chat that is not Claude.
    #[serde(default)]
    pub to_pid: Option<u32>,
    /// The receiving process's recorded start (`procStart` of its registry file) when the route
    /// was saved. A chat whose session id changed (compaction, resume) in that same live process
    /// is the same chat only when pid AND start both match the proven caller. `None` (no start
    /// recorded, or a route saved before this field) is never accepted as a successor.
    #[serde(default)]
    pub to_start: Option<String>,
}

/// A chat that is not discovered from disk (Codex, or Claude without a
/// session file) and registered here; also the chats of a hook harness (Muse) that
/// `<cli> chat hook` registers on every hook run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredSession {
    pub id: String,
    /// "claude", "codex" or a hook harness ("muse").
    pub kind: String,
    pub name: String,
    pub cwd: String,
    /// The registering process; the chat is gone when it is.
    pub pid: u32,
    pub updated: u64,
}

/// What the hub says about itself while its chat runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayStatus {
    pub pid: u32,
    pub ts: u64,
    /// This computer's name, as chats are shown (`"<chat> on <alias>"`).
    pub alias: String,
}

/// What a receiving chat's delivery record says about a message (`Store::begin_delivery`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Not delivered to this chat before: it is now marked in progress, and the caller delivers
    /// it and then calls `finish_delivery` (or `forget_delivery` when nothing was kept).
    New,
    /// Delivered or kept before. `msg_id` is the message's id, `state` and `detail` the receipt
    /// it got then.
    Done {
        msg_id: String,
        state: String,
        detail: String,
    },
    /// Being delivered right now by a running process.
    InProgress { msg_id: String },
    /// This id was delivered to this chat with other content.
    Conflict,
}

/// One message in a receiving chat's delivery record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DeliveryRecord {
    id: String,
    /// `payload_hash` of the envelope.
    h: String,
    ts: u64,
    /// The receipt state word; empty while the delivery is in progress.
    #[serde(default)]
    state: String,
    #[serde(default)]
    detail: String,
    /// The process delivering it, while in progress.
    #[serde(default)]
    pid: u32,
}

/// Whether a process with this pid runs.
fn process_alive(pid: u32) -> bool {
    let mut system = sysinfo::System::new();
    let target = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    system.process(target).is_some()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalLine {
    o: String,
    id: String,
    h: String,
    ts: u64,
}

fn safe_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed.to_string()
    }
}

fn fnv(text: &str) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325u64;
    for b in text.bytes() {
        state ^= u64::from(b);
        state = state.wrapping_mul(0x0100_0000_01b3);
    }
    state
}

/// Remove one receipt list and its read cursor.
fn remove_receipt_file(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("cursor.json"));
}

/// The file stem for a chat id: a readable prefix plus a hash of the whole id, so two
/// ids that sanitize alike never share files.
fn session_stem(id: &str) -> String {
    let prefix: String = safe_name(id).chars().take(40).collect();
    format!("{prefix}-{:016x}", fnv(id))
}

/// What names the sender in the dedupe journal: the stable device id the mesh proved when the
/// envelope came over the device link (a name can change or repeat), else the stated device name.
pub fn origin_key(env: &Envelope) -> &str {
    if env.mesh.is_empty() {
        &env.from.device
    } else {
        &env.mesh
    }
}

/// The content hash `journal_seen` and the delivery record compare: sender chat, target and body
/// of an envelope. It leaves out the transport-dependent origin (`origin_key`: the mesh's device
/// id over the device link, the stated name over ssh), so a retry of one id over the other
/// transport (C-D19) matches the delivery record instead of reading as a reused id; who sent it
/// stays in the journal's own key.
pub fn payload_hash(env: &Envelope) -> String {
    let mut data = Vec::new();
    for part in [
        env.from.session.as_str(),
        env.to.device.as_str(),
        env.to.session.as_str(),
        env.body.as_str(),
    ] {
        data.extend_from_slice(part.as_bytes());
        data.push(0);
    }
    sha256_bytes(&data)
}

// ---- files that stay private ---------------------------------------------------

fn refuse_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is a link; the chat store does not follow links",
                path.display()
            ),
        )),
        _ => Ok(()),
    }
}

/// Create `dir` owner-only (and harden it when it exists), refusing a link. When `dir` is
/// directly inside Chat folder, that folder too.
/// The owner-only check (a security descriptor read) runs on first use of a folder and again only
/// after the trust window, when the folder is gone, or after a write there was denied
/// (`crate::store::private_dir_verified`); every store call used to repeat it.
fn secure_chain(dir: &Path) -> io::Result<()> {
    secure_one(dir)?;
    if let Some(parent) = dir.parent()
        && parent.file_name().is_some_and(|n| n == "chat")
    {
        secure_one(parent)?;
    }
    Ok(())
}

fn secure_one(dir: &Path) -> io::Result<()> {
    if rightkit_fs::private_cached::private_dir_verified(dir) {
        return Ok(());
    }
    rightkit_fs::ensure_private_dir(dir).map_err(io::Error::from)?;
    rightkit_fs::private_cached::note_private_dir(dir);
    Ok(())
}

/// A denied write drops the remembered check of its folder (and Chat folder above it).
fn forget_on_denied<T>(dir: Option<&Path>, result: io::Result<T>) -> io::Result<T> {
    if let (Some(dir), Err(error)) = (dir, &result)
        && error.kind() == io::ErrorKind::PermissionDenied
    {
        rightkit_fs::private_cached::forget_private_dir(dir);
        if let Some(parent) = dir.parent() {
            rightkit_fs::private_cached::forget_private_dir(parent);
        }
    }
    result
}

/// Open an existing regular file for reading, refusing a link at `path` (RightKit's
/// `open_read_nofollow`: `O_NOFOLLOW` on unix, the reparse point itself on Windows).
fn open_read(path: &Path) -> io::Result<fs::File> {
    rightkit_fs::open_read_nofollow(path, &rightkit_fs::ReadOptions::new()).map_err(io::Error::from)
}

fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = open_read(path)?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

/// The last `max` bytes of a file, starting at a line boundary.
fn read_tail(path: &Path, max: u64) -> Vec<u8> {
    let Ok(mut file) = open_read(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    if start > 0 {
        match buf.iter().position(|b| *b == b'\n') {
            Some(i) => buf.drain(..=i),
            None => buf.drain(..),
        };
    }
    buf
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&read_bytes(path).ok()?).ok()
}

/// A JSON list a writer replaces whole: a missing file is empty, but a file that cannot be
/// read (a sharing violation, permissions) is an error, so the caller never rewrites it from
/// an empty list and erases its history. A file that does not parse is empty.
fn read_records<T: DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    match read_bytes(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Every readable line of a JSON-lines file; lines that do not parse are skipped.
fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Vec<T> {
    read_bytes(path)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if let Some(parent) = path.parent() {
        secure_chain(parent)?;
    }
    let result = rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| io::Write::write_all(file, &bytes),
    )
    .map_err(io::Error::from);
    forget_on_denied(path.parent(), result)
}

/// The on/off switches `policy.json` holds. A policy file that cannot be trusted (not JSON, not an
/// object, or unreadable for any reason but being absent) turns every one of them OFF, in the getters
/// and in the base a write starts from: Chat fails closed, never back to its defaults.
fn harness_policy_key(kind: &str) -> String {
    if kind == "muse" {
        "muse".to_string()
    } else {
        format!("harness:{kind}")
    }
}

const POLICY_FLAGS: [&str; 11] = [
    "enabled",
    "claudeHookDelivery",
    "terminalChats",
    "claudeProfiles",
    "codexWakeIdle",
    "claudeWakeIdle",
    "museWakeIdle",
    "codexAppDelivery",
    "codexHiddenDelivery",
    "chatReceiptNotices",
    "chatSocketFallback",
];

/// The window in which a host app Claude plugin hook run counts as "hooks running" when
/// `claudeHookDelivery` is absent. Same as `deliver_codex`'s `HOOKS_RUNNING_WITHIN` (7 days).
const CLAUDE_HOOK_RUNNING_WITHIN_SECS: u64 = 7 * 24 * 3600;

/// Whether any Claude chat on this computer ran the host app's plugin hook within the window above.
fn claude_hook_running() -> bool {
    super::codex_hook::newest_claude_heartbeat().is_some_and(|secs| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(secs) <= CLAUDE_HOOK_RUNNING_WITHIN_SECS
    })
}

/// What `policy.json` holds right now.
enum PolicyFile {
    /// No file: each switch has its documented default.
    Absent,
    /// A JSON object.
    Object(serde_json::Map<String, serde_json::Value>),
    /// Present but not an object, or not readable (a link, a permission error): every switch is off.
    Untrusted,
}

/// What identifies one state of `policy.json` on disk: length, modification time (ns) and, on unix,
/// device and inode (every publish is an atomic rename, so it changes the inode as well).
type PolicyStamp = (u64, u128, u64, u64);

/// A stamp younger than this is not trusted to tell two writes apart (a coarse file-system clock can
/// give two writes in one tick the same length and time): such a read is not kept.
const POLICY_STAMP_SETTLE: Duration = Duration::from_secs(2);

/// How many `policy.json` paths are kept (one entry each; the chat store has one root, tests a few).
const POLICY_CACHE_PATHS: usize = 8;

/// The parsed `policy.json` per path, reused while its stamp is unchanged. Every switch check read the
/// file again, and the chat hub checks several switches on every five-second scan (#444: repeated opens
/// in a 10 s sample of the installed Mac engine). Pattern of `muse.rs` `index_stamp` (82f3d649).
static POLICY_CACHE: std::sync::Mutex<Vec<(PathBuf, PolicyStamp, std::sync::Arc<PolicyFile>)>> =
    std::sync::Mutex::new(Vec::new());

/// The stamp of the file itself (a link is not followed; a link, a missing file or any error has none
/// and is read the slow way, with today's admission and result).
fn policy_stamp(path: &Path) -> Option<PolicyStamp> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified = meta.modified().ok()?;
    // Too young, or dated in the future: not kept.
    if !matches!(modified.elapsed(), Ok(age) if age >= POLICY_STAMP_SETTLE) {
        return None;
    }
    let nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0, 0);
    Some((meta.len(), nanos, dev, ino))
}

/// Forget the cached `policy.json` of `path` (this process just wrote it).
fn policy_cache_forget(path: &Path) {
    POLICY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(p, _, _)| p != path);
}

fn read_policy(path: &Path) -> std::sync::Arc<PolicyFile> {
    // Stamp before the read: a write between the two leaves a stale stamp on newer content, which the
    // next call sees as changed and reads again; the reverse (new stamp on old content) cannot happen.
    let stamp = policy_stamp(path);
    if let Some(stamp) = stamp {
        let cache = POLICY_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, _, policy)) = cache.iter().find(|(p, s, _)| p == path && *s == stamp) {
            return policy.clone();
        }
    }
    let (policy, read_failed) = read_policy_file(path);
    let policy = std::sync::Arc::new(policy);
    // A read error (descriptor pressure, permissions, I/O) is answered as all-off for this call only;
    // caching it would keep every switch off until the file changed, which a chmod does not do.
    if let (Some(stamp), false) = (stamp, read_failed) {
        let mut cache = POLICY_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|(p, _, _)| p != path);
        if cache.len() >= POLICY_CACHE_PATHS {
            cache.remove(0);
        }
        cache.push((path.to_path_buf(), stamp, policy.clone()));
    }
    policy
}

/// The parsed file, and whether reading it failed (an error other than not-found).
fn read_policy_file(path: &Path) -> (PolicyFile, bool) {
    match read_bytes(path) {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(map)) => (PolicyFile::Object(map), false),
            _ => (PolicyFile::Untrusted, false),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => (PolicyFile::Absent, false),
        Err(_) => (PolicyFile::Untrusted, true),
    }
}

/// `policy.json` storage for `rightkit_app_settings::Store::with_backend`. A plain `FileBackend` is not
/// equivalent: this keeps the chat store's own admission and publication.
/// - Admission: the file is read through `read_bytes` (a link is refused, `O_NOFOLLOW` on unix) and the folder
///   is made owner-only with `secure_chain` first; the lock file is refused when it is a link.
/// - Publication: `atomic_write_with_options` with `Privacy::OwnerOnlyUnixInheritWindows`, as `write_json`.
/// - Exclusion: the cross-process `policy.lock` (the file the setters locked before), held across read, edit and publish.
/// - A file that is missing loads as "nothing stored". One that is not JSON or not an object loads as every
///   switch OFF (`POLICY_FLAGS`), so a write keeps everything else off plus the one key it sets (fail closed).
///   A read failure other than not-found is an error, so the write is refused.
/// - Revision: a flat file keeps none. `load` reports 0 and `patch` returns `revision + 1`; nothing passes an
///   expected revision to this store, so no compare-and-set is relied on (a later `update_at` would never conflict).
struct PolicyBackend {
    path: PathBuf,
}

impl PolicyBackend {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn read(&self) -> Result<Option<rightkit_app_settings::Stored>, rightkit_app_settings::Error> {
        let bytes = match read_bytes(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let values = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(values)) => values,
            // Not JSON, or not an object: every switch off, nothing else kept.
            _ => POLICY_FLAGS
                .iter()
                .map(|key| ((*key).to_string(), serde_json::Value::Bool(false)))
                .collect(),
        };
        Ok(Some(rightkit_app_settings::Stored {
            version: None,
            namespace: None,
            values,
            revision: 0,
            needs_rewrite: false,
        }))
    }
}

impl rightkit_app_settings::SettingsBackend for PolicyBackend {
    fn load(&self) -> Result<Option<rightkit_app_settings::Stored>, rightkit_app_settings::Error> {
        self.read()
    }

    fn patch(
        &self,
        edit: &mut dyn FnMut(
            Option<&rightkit_app_settings::Stored>,
        )
            -> Result<rightkit_app_settings::Edit, rightkit_app_settings::Error>,
    ) -> Result<Option<u64>, rightkit_app_settings::Error> {
        if let Some(parent) = self.path.parent() {
            secure_chain(parent)?;
        }
        let _guard = lock_target(&self.path)?;
        let current = self.read()?;
        match edit(current.as_ref())? {
            rightkit_app_settings::Edit::Keep => Ok(None),
            rightkit_app_settings::Edit::Commit(doc) => {
                let bytes = serde_json::to_vec(&doc.values)?;
                rightkit_fs::atomic_write_with_options(
                    &self.path,
                    rightkit_fs::AtomicWriteOptions {
                        privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                        ..Default::default()
                    },
                    |file| io::Write::write_all(file, &bytes),
                )
                .map_err(io::Error::from)?;
                Ok(Some(doc.revision + 1))
            }
        }
    }
}

/// The lock behind [`Store::lock`] and [`PolicyBackend`].
fn lock_target(target: &Path) -> io::Result<LockFile> {
    let path = target.with_extension("lock");
    if let Some(parent) = path.parent() {
        secure_chain(parent)?;
    }
    refuse_link(&path)?;
    let options = LockOptions {
        timeout: Duration::from_secs(3),
        poll: Duration::from_millis(10),
        label: String::new(),
    };
    match FileLock::acquire(&path, &options) {
        Ok(guard) => Ok(LockFile { _guard: guard }),
        Err(FsError::LockTimeout { .. } | FsError::Locked { .. }) => {
            let name = target.file_stem().map_or_else(
                || "chat store".to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{name} is busy"),
            ))
        }
        Err(other) => Err(io::Error::from(other)),
    }
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|listing| {
            listing
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Whether a file's last byte is not a line break (a write was cut off).
fn ends_torn(path: &Path) -> bool {
    let Ok(mut file) = open_read(path) else {
        return false;
    };
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// Append `text` to a file, creating it private; a torn last line is closed first.
fn append_text(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        secure_chain(parent)?;
    }
    let mut record = Vec::with_capacity(text.len() + 1);
    if ends_torn(path) {
        record.push(b'\n');
    }
    record.extend_from_slice(text.as_bytes());
    match rightkit_fs::append_record(path, &record, rightkit_fs::AppendOptions::private()) {
        // The record is written and flushed; only the folder sync failed. Reporting an error here
        // would make a retry append the message twice.
        Ok(_) | Err(rightkit_fs::FsError::DirSyncFailed { .. }) => Ok(()),
        Err(error) => forget_on_denied(path.parent(), Err(io::Error::from(error))),
    }
}

// ---- locks -------------------------------------------------------------------

/// Held until dropped; RightKit releases the lock when the guard drops or the process dies.
struct LockFile {
    _guard: FileLock,
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    inbox_cap: u64,
}

impl Store {
    /// Chat folder inside `state_dir` (the host app state directory).
    pub fn new(state_dir: &Path) -> Store {
        Store {
            root: state_dir.join("chat"),
            inbox_cap: INBOX_CAP_BYTES,
        }
    }

    /// The store whose chat folder is `chat` itself: for code that holds the folder its
    /// message files were read from (the chat hooks), so it works in that same folder.
    pub fn at_chat_folder(chat: &Path) -> Store {
        Store {
            root: chat.to_path_buf(),
            inbox_cap: INBOX_CAP_BYTES,
        }
    }

    /// The store in the host app's default state directory.
    pub fn open_default() -> io::Result<Store> {
        let store = Store::new(&rightkit_app_host::state_directory()?);
        // Temporary files a killed writer left behind; RightKit removes only its own generated
        // names, and only when their writer is confirmed gone.
        let _ = rightkit_fs::sweep_stale_temps(&store.root, std::time::Duration::from_secs(3600));
        Ok(store)
    }

    pub fn with_inbox_cap(mut self, bytes: u64) -> Store {
        self.inbox_cap = bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Exclusive cross-process lock on the per-target `.lock` file, waiting up to three
    /// seconds for a busy one.
    fn lock(&self, target: &Path) -> io::Result<LockFile> {
        lock_target(target)
    }

    // ---- generic state files -------------------------------------------------

    /// A JSON file directly in Chat folder (`name` is a plain file name).
    pub fn read_state<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        read_json(&self.root.join(safe_name(name)))
    }

    /// Read-modify-write a JSON file in Chat folder under its lock.
    pub fn update_state<T, F>(&self, name: &str, change: F) -> io::Result<()>
    where
        T: Serialize + DeserializeOwned + Default,
        F: FnOnce(&mut T),
    {
        let path = self.root.join(safe_name(name));
        let _guard = self.lock(&path)?;
        let mut value: T = read_json(&path).unwrap_or_default();
        change(&mut value);
        write_json(&path, &value)
    }

    // ---- inboxes -----------------------------------------------------------

    fn inbox_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.jsonl", session_stem(session)))
    }

    fn cursor_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.cursor.json", session_stem(session)))
    }

    fn evicted_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.evicted.jsonl", session_stem(session)))
    }

    fn read_cursor(&self, session: &str) -> Cursor {
        read_json(&self.cursor_path(session)).unwrap_or_default()
    }

    fn read_lines(path: &Path) -> Vec<InboxEntry> {
        read_bytes(path)
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `seq` of the last complete line of an inbox file.
    fn last_seq(path: &Path) -> u64 {
        let tail = read_tail(path, 512 * 1024);
        String::from_utf8_lossy(&tail)
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<InboxEntry>(line).ok())
            .map_or(0, |e| e.seq)
    }

    /// Add a message to a chat's inbox; returns its `seq`. When the file would
    /// pass the cap, the oldest messages are dropped first (and noted, see
    /// `inbox_evicted`). The `seq` follows the last committed line of the file.
    pub fn append_inbox(&self, session: &str, env: &Envelope) -> io::Result<u64> {
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        // Idempotent on the message id (checked under this inbox lock): an id already in the
        // inbox, or with a delivery record, is not written again. Returns 0 when skipped.
        if Self::read_lines(&path).iter().any(|e| e.env.id == env.id)
            || self.has_delivery_record(session, &env.id)
        {
            return Ok(0);
        }
        let mut cursor = self.read_cursor(session);
        let seq = (Self::last_seq(&path) + 1).max(cursor.next).max(1);
        let entry = InboxEntry {
            seq,
            received: now_ms(),
            env: env.clone(),
        };
        let line = serde_json::to_string(&entry).map_err(io::Error::other)? + "\n";
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size + line.len() as u64 > self.inbox_cap {
            let entries = Self::read_lines(&path);
            let mut kept: Vec<String> = entries
                .iter()
                .filter_map(|e| serde_json::to_string(e).ok())
                .map(|l| l + "\n")
                .collect();
            let mut total: u64 =
                kept.iter().map(|l| l.len() as u64).sum::<u64>() + line.len() as u64;
            let mut drop_count = 0;
            while total > self.inbox_cap && drop_count < kept.len() {
                total -= kept[drop_count].len() as u64;
                drop_count += 1;
            }
            kept.drain(..drop_count);
            kept.push(line);
            if let Some(parent) = path.parent() {
                secure_chain(parent)?;
            }
            rightkit_fs::atomic_write_with_options(
                &path,
                rightkit_fs::AtomicWriteOptions {
                    privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                    ..Default::default()
                },
                |file| io::Write::write_all(file, kept.concat().as_bytes()),
            )
            .map_err(io::Error::from)?;
            let lost = entries
                .iter()
                .take(drop_count)
                .filter(|e| e.seq > cursor.read)
                .count();
            // A held message dropped unread has expired for its sender.
            for dropped in entries
                .iter()
                .take(drop_count)
                .filter(|e| e.seq > cursor.read)
            {
                super::receipts::expired(self, session, &dropped.env);
            }
            if drop_count > 0 {
                let note = serde_json::json!({
                    "ts": now_ms(),
                    "count": lost,
                    "dropped": drop_count,
                });
                let _ = append_text(&self.evicted_path(session), &format!("{note}\n"));
            }
        } else {
            append_text(&path, &line)?;
        }
        cursor.next = seq + 1;
        write_json(&self.cursor_path(session), &cursor)?;
        Ok(seq)
    }

    /// Every stored message of a chat, oldest first.
    pub fn read_inbox(&self, session: &str) -> Vec<InboxEntry> {
        Self::read_lines(&self.inbox_path(session))
    }

    /// Unread messages (received after `since`, ms, when given), marked read.
    pub fn take_unread(&self, session: &str, since: Option<u64>) -> io::Result<Vec<InboxEntry>> {
        self.take_unread_ack(session, since, true)
    }

    /// Unread messages (received after `since`, ms, when given). Only with `ack`
    /// are they marked read; otherwise they stay unread until `ack_inbox`.
    pub fn take_unread_ack(
        &self,
        session: &str,
        since: Option<u64>,
        ack: bool,
    ) -> io::Result<Vec<InboxEntry>> {
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_cursor(session);
        let entries = Self::read_lines(&path);
        let newest = entries.iter().map(|e| e.seq).max().unwrap_or(cursor.read);
        let unread: Vec<InboxEntry> = entries
            .into_iter()
            .filter(|e| e.seq > cursor.read && since.is_none_or(|s| e.received > s))
            .collect();
        if ack && newest > cursor.read {
            cursor.read = newest;
            cursor.next = cursor.next.max(newest + 1);
            write_json(&self.cursor_path(session), &cursor)?;
            for entry in &unread {
                self.record_inbox_delivery(session, &entry.env);
            }
        }
        Ok(unread)
    }

    /// Mark every message up to and including `up_to_seq` read.
    pub fn ack_inbox(&self, session: &str, up_to_seq: u64) -> io::Result<()> {
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_cursor(session);
        if up_to_seq > cursor.read {
            let newly_read: Vec<InboxEntry> = Self::read_lines(&path)
                .into_iter()
                .filter(|e| e.seq > cursor.read && e.seq <= up_to_seq)
                .collect();
            cursor.read = up_to_seq;
            cursor.next = cursor.next.max(up_to_seq + 1);
            write_json(&self.cursor_path(session), &cursor)?;
            for entry in &newly_read {
                self.record_inbox_delivery(session, &entry.env);
            }
        }
        Ok(())
    }

    /// How many messages of a chat are still unread.
    pub fn unread_count(&self, session: &str) -> usize {
        let read = self.read_cursor(session).read;
        let refused = self.refused_inbox(session);
        self.read_inbox(session)
            .iter()
            .filter(|e| e.seq > read && !refused.contains(&e.env.id))
            .count()
    }

    /// Unread count plus how many unread messages the size cap has dropped.
    pub fn unread_detail(&self, session: &str) -> UnreadDetail {
        UnreadDetail {
            unread: self.unread_count(session),
            evicted: self.inbox_evicted(session),
        }
    }

    /// How many messages the inbox size cap dropped before they were read.
    pub fn inbox_evicted(&self, session: &str) -> u64 {
        read_bytes(&self.evicted_path(session))
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| v["count"].as_u64())
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Held messages of a chat that were refused by its owner (`inbox --refuse`), by id.
    pub fn refused_inbox(&self, session: &str) -> Vec<String> {
        read_json(&self.refused_path(session)).unwrap_or_default()
    }

    /// Refuse a held message: it is no longer shown in the chat's inbox.
    pub fn refuse_inbox(&self, session: &str, msg_id: &str) -> io::Result<()> {
        let path = self.refused_path(session);
        let _guard = self.lock(&path)?;
        let mut ids: Vec<String> = read_json(&path).unwrap_or_default();
        if !ids.iter().any(|id| id == msg_id) {
            ids.push(msg_id.to_string());
        }
        if ids.len() > REFUSED_CAP {
            let kept = ids.split_off(ids.len() - REFUSED_CAP);
            ids = kept;
        }
        write_json(&path, &ids)
    }

    fn refused_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.refused.json", session_stem(session)))
    }

    // ---- generic records (one JSON file each, in a chat subfolder) -------

    /// The file a record named `key` lives in, under Chat folder's `dir`.
    pub fn record_path(&self, dir: &str, key: &str) -> PathBuf {
        self.root
            .join(dir)
            .join(format!("{}.json", session_stem(key)))
    }

    /// Write a record (private, atomically).
    pub fn save_record<T: Serialize>(&self, path: &Path, value: &T) -> io::Result<()> {
        write_json(path, value)
    }

    /// One record, or None when it is missing or unreadable.
    pub fn read_record<T: DeserializeOwned>(&self, path: &Path) -> Option<T> {
        read_json(path)
    }

    /// Every readable record in `dir`, in file-name order, with its path.
    pub fn list_records<T: DeserializeOwned>(&self, dir: &str) -> Vec<(PathBuf, T)> {
        json_files(&self.root.join(dir))
            .into_iter()
            .filter_map(|path| read_json::<T>(&path).map(|value| (path, value)))
            .collect()
    }

    /// Delete a record file; a missing one is fine.
    pub fn remove_record(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }

    // ---- receipts for the sending chat -------------------------------------

    fn receipts_dir(&self) -> PathBuf {
        self.root.join("receipts")
    }

    fn receipt_file(&self, session: &str) -> PathBuf {
        self.receipts_dir()
            .join(format!("{}.jsonl", session_stem(session)))
    }

    fn receipt_cursor_path(&self, session: &str) -> PathBuf {
        self.receipts_dir()
            .join(format!("{}.cursor.json", session_stem(session)))
    }

    /// Remove the receipt lists (and their cursors) not written for `RECEIPT_FILE_MAX_AGE`, then
    /// the least recently written beyond `RECEIPT_FILES_CAP`. Returns how many were removed.
    pub fn prune_receipt_files(&self) -> usize {
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(self.receipts_dir())
            .map(|listing| {
                listing
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                    .filter_map(|p| {
                        let written = fs::metadata(&p).and_then(|m| m.modified()).ok()?;
                        Some((written, p))
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        let now = std::time::SystemTime::now();
        let mut removed = 0;
        let mut kept: Vec<PathBuf> = Vec::new();
        for (written, path) in files {
            let old = now
                .duration_since(written)
                .is_ok_and(|age| age > RECEIPT_FILE_MAX_AGE);
            if old {
                remove_receipt_file(&path);
                removed += 1;
            } else {
                kept.push(path);
            }
        }
        let excess = kept.len().saturating_sub(RECEIPT_FILES_CAP);
        for path in kept.iter().take(excess) {
            remove_receipt_file(path);
            removed += 1;
        }
        removed
    }

    fn read_receipt_cursor(&self, session: &str) -> ReceiptCursor {
        read_json(&self.receipt_cursor_path(session)).unwrap_or_default()
    }

    /// Store a receipt update for the sending chat `session`, with the next receipt number.
    /// Returns false when the same message and state were already stored. Keeps the newest
    /// `RECEIPT_CAP` updates.
    pub fn append_receipt(&self, session: &str, update: &Update) -> io::Result<bool> {
        let path = self.receipt_file(session);
        let _guard = self.lock(&path)?;
        let is_new = !path.exists();
        let mut lines: Vec<Update> = read_jsonl(&path);
        if lines
            .iter()
            .any(|u| u.msg_id == update.msg_id && u.state == update.state)
        {
            return Ok(false);
        }
        let mut stored = update.clone();
        stored.seq = lines.iter().map(|u| u.seq).max().unwrap_or(0) + 1;
        if lines.len() < RECEIPT_CAP {
            let line = serde_json::to_string(&stored).map_err(io::Error::other)? + "\n";
            append_text(&path, &line)?;
        } else {
            lines.push(stored);
            let skip = lines.len() - RECEIPT_CAP;
            let body: String = lines
                .iter()
                .skip(skip)
                .filter_map(|u| serde_json::to_string(u).ok())
                .map(|l| l + "\n")
                .collect();
            if let Some(parent) = path.parent() {
                secure_chain(parent)?;
            }
            rightkit_fs::atomic_write_with_options(
                &path,
                rightkit_fs::AtomicWriteOptions {
                    privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                    ..Default::default()
                },
                |file| io::Write::write_all(file, body.as_bytes()),
            )
            .map_err(io::Error::from)?;
        }
        if is_new {
            self.prune_receipt_files();
        }
        Ok(true)
    }

    /// The chat's receipt updates numbered after `after`, oldest first.
    pub fn receipts_after(&self, session: &str, after: u64) -> Vec<Update> {
        read_jsonl::<Update>(&self.receipt_file(session))
            .into_iter()
            .filter(|u| u.seq > after)
            .collect()
    }

    /// The highest receipt number the chat has marked read (0 when none).
    pub fn receipt_read_seq(&self, session: &str) -> u64 {
        self.read_receipt_cursor(session).read
    }

    /// Mark the chat's receipt updates up to `up_to` read.
    pub fn ack_receipts(&self, session: &str, up_to: u64) -> io::Result<()> {
        let path = self.receipt_cursor_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_receipt_cursor(session);
        if up_to > cursor.read {
            cursor.read = up_to;
            write_json(&path, &cursor)?;
        }
        Ok(())
    }

    /// The updates a hook notice should name now: not yet announced, at most `max` (the oldest),
    /// and none at all when the last notice was less than `gap_ms` before `now`. Marks them
    /// announced.
    pub fn take_receipt_notice(
        &self,
        session: &str,
        now: u64,
        gap_ms: u64,
        max: usize,
    ) -> io::Result<Vec<Update>> {
        let quiet = self.read_receipt_cursor(session);
        if quiet.notice_ms > 0 && now.saturating_sub(quiet.notice_ms) < gap_ms {
            return Ok(Vec::new());
        }
        if read_jsonl::<Update>(&self.receipt_file(session))
            .iter()
            .all(|u| u.seq <= quiet.announced)
        {
            return Ok(Vec::new());
        }
        let path = self.receipt_cursor_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_receipt_cursor(session);
        if cursor.notice_ms > 0 && now.saturating_sub(cursor.notice_ms) < gap_ms {
            return Ok(Vec::new());
        }
        let pending: Vec<Update> = read_jsonl::<Update>(&self.receipt_file(session))
            .into_iter()
            .filter(|u| u.seq > cursor.announced)
            .take(max)
            .collect();
        if let Some(last) = pending.last() {
            cursor.announced = last.seq;
            cursor.notice_ms = now;
            write_json(&path, &cursor)?;
        }
        Ok(pending)
    }

    // ---- linked computers --------------------------------------------------

    fn links_path(&self) -> PathBuf {
        self.root.join("links.json")
    }

    pub fn links(&self) -> Vec<Link> {
        read_json(&self.links_path()).unwrap_or_default()
    }

    pub fn save_links(&self, links: &[Link]) -> io::Result<()> {
        let path = self.links_path();
        let _guard = self.lock(&path)?;
        write_json(&path, &links)
    }

    // ---- registered chats ---------------------------

    fn session_file(&self, id: &str) -> PathBuf {
        self.root
            .join("sessions")
            .join(format!("{}.json", session_stem(id)))
    }

    pub fn register_session(&self, session: &RegisteredSession) -> io::Result<()> {
        write_json(&self.session_file(&session.id), session)?;
        Ok(())
    }

    /// Registered chats.
    pub fn registered_sessions(&self) -> Vec<RegisteredSession> {
        json_files(&self.root.join("sessions"))
            .iter()
            .filter_map(|p| read_json::<RegisteredSession>(p))
            .collect()
    }

    /// One registered chat by its exact id.
    pub fn registered_session(&self, id: &str) -> Option<RegisteredSession> {
        read_json::<RegisteredSession>(&self.session_file(id))
    }

    pub fn forget_session(&self, id: &str) {
        let _ = fs::remove_file(self.session_file(id));
    }

    /// A chat id for a working folder that has none from its host: made once
    /// and kept, so the same folder is the same chat next time.
    pub fn cwd_session_id(&self, cwd: &str, kind: &str) -> io::Result<String> {
        let path = self
            .root
            .join("sessions")
            .join(format!("cwd-{:016x}.id", fnv(cwd)));
        if let Ok(bytes) = read_bytes(&path) {
            let id = String::from_utf8_lossy(&bytes).trim().to_string();
            if !id.is_empty() {
                return Ok(id);
            }
        }
        let id = format!("{kind}-{}", super::random_hex(8));
        if let Some(parent) = path.parent() {
            secure_chain(parent)?;
        }
        rightkit_fs::atomic_write_with_options(
            &path,
            rightkit_fs::AtomicWriteOptions {
                privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                ..Default::default()
            },
            |file| io::Write::write_all(file, id.as_bytes()),
        )
        .map_err(io::Error::from)?;
        Ok(id)
    }

    // ---- duplicate journal -------------------------------------------------

    fn journal_path(&self) -> PathBuf {
        self.root.join("journal.jsonl")
    }

    /// Record `(origin_device, msg_id)` with its content hash (see `payload_hash`) and
    /// say whether it was seen before. The journal keeps the last `JOURNAL_CAP`
    /// entries. A journal that cannot be locked, read or written reports `Unavailable`, never
    /// `New` (F6: a failing journal must not fail open).
    pub fn journal_seen(&self, origin_device: &str, msg_id: &str, payload_hash: &str) -> Seen {
        let path = self.journal_path();
        let Ok(_guard) = self.lock(&path) else {
            return Seen::Unavailable;
        };
        let mut lines: Vec<JournalLine> = match read_bytes(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
            // No journal yet is an empty one; any other failure is not.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Seen::Unavailable,
        };
        if let Some(hit) = lines
            .iter()
            .find(|l| l.o == origin_device && l.id == msg_id)
        {
            return if hit.h == payload_hash {
                Seen::Duplicate
            } else {
                Seen::Conflict
            };
        }
        let line = JournalLine {
            o: origin_device.to_string(),
            id: msg_id.to_string(),
            h: payload_hash.to_string(),
            ts: now_ms(),
        };
        let text = serde_json::to_string(&line).unwrap_or_default() + "\n";
        if lines.len() < JOURNAL_CAP {
            if append_text(&path, &text).is_err() {
                return Seen::Unavailable;
            }
        } else {
            lines.push(line);
            let skip = lines.len() - JOURNAL_CAP;
            let body: String = lines
                .iter()
                .skip(skip)
                .filter_map(|l| serde_json::to_string(l).ok())
                .map(|l| l + "\n")
                .collect();
            if let Some(parent) = path.parent()
                && secure_chain(parent).is_ok()
            {
                if rightkit_fs::atomic_write_with_options(
                    &path,
                    rightkit_fs::AtomicWriteOptions {
                        privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                        ..Default::default()
                    },
                    |file| io::Write::write_all(file, body.as_bytes()),
                )
                .is_err()
                {
                    return Seen::Unavailable;
                }
            } else {
                return Seen::Unavailable;
            }
        }
        Seen::New
    }

    // ---- delivery record (at most once per receiving chat) ------------------

    fn delivered_path(&self, session: &str) -> PathBuf {
        self.root
            .join("delivered")
            .join(format!("{}.json", session_stem(session)))
    }

    /// Check `env` against the delivery record of the chat `session` and, when it is new there,
    /// mark it in progress, all under the record's lock, so two deliveries of one message (a
    /// control request the hub took again, a post retried after a late receipt, a resend of the
    /// same id after `unknown`) never both reach the chat. A message matches an entry with its
    /// id only (distinct ids with the same text are distinct messages). An
    /// entry in progress whose process is gone (or older than five minutes) was abandoned and is
    /// closed as `unknown`, never delivered again. The record keeps `DELIVERED_CAP` entries per chat, at most seven days.
    pub fn begin_delivery(&self, session: &str, env: &Envelope) -> io::Result<Delivery> {
        let path = self.delivered_path(session);
        let _guard = self.lock(&path)?;
        let now = now_ms();
        let mut records: Vec<DeliveryRecord> = read_records(&path)?;
        records.retain(|r| now.saturating_sub(r.ts) < DELIVERED_MAX_AGE_MS);
        let hash = payload_hash(env);
        let hit = records.iter().position(|r| r.id == env.id);
        if let Some(i) = hit {
            let record = &records[i];
            if record.h != hash {
                return Ok(Delivery::Conflict);
            }
            if !record.state.is_empty() {
                return Ok(Delivery::Done {
                    msg_id: record.id.clone(),
                    state: record.state.clone(),
                    detail: record.detail.clone(),
                });
            }
            let running = record.pid != 0
                && now.saturating_sub(record.ts) < DELIVERY_IN_PROGRESS_MAX_MS
                && process_alive(record.pid);
            if running {
                return Ok(Delivery::InProgress {
                    msg_id: record.id.clone(),
                });
            }
            // The delivering process is gone: the push may have reached the chat before it
            // stopped, so the id is closed as `unknown`, never delivered again (at most once);
            // the sender resends as a new message.
            let record = &mut records[i];
            record.state = "unknown".to_string();
            record.detail = "its delivery stopped midway; not delivered again".to_string();
            record.pid = 0;
            let done = Delivery::Done {
                msg_id: record.id.clone(),
                state: record.state.clone(),
                detail: record.detail.clone(),
            };
            write_json(&path, &records)?;
            return Ok(done);
        }
        records.push(DeliveryRecord {
            id: env.id.clone(),
            h: hash,
            ts: now,
            state: String::new(),
            detail: String::new(),
            pid: std::process::id(),
        });
        let excess = records.len().saturating_sub(DELIVERED_CAP);
        records.drain(..excess);
        write_json(&path, &records)?;
        Ok(Delivery::New)
    }

    /// What the delivery record of `session` says about `env`, without marking anything:
    /// `New` when it has no entry for it.
    pub fn peek_delivery(&self, session: &str, env: &Envelope) -> Delivery {
        let now = now_ms();
        let records: Vec<DeliveryRecord> =
            read_json(&self.delivered_path(session)).unwrap_or_default();
        let hash = payload_hash(env);
        match records.iter().find(|r| r.id == env.id) {
            Some(r) if r.h != hash => Delivery::Conflict,
            Some(r) if !r.state.is_empty() => Delivery::Done {
                msg_id: r.id.clone(),
                state: r.state.clone(),
                detail: r.detail.clone(),
            },
            Some(r)
                if r.pid != 0
                    && now.saturating_sub(r.ts) < DELIVERY_IN_PROGRESS_MAX_MS
                    && process_alive(r.pid) =>
            {
                Delivery::InProgress {
                    msg_id: r.id.clone(),
                }
            }
            _ => Delivery::New,
        }
    }

    /// Record the receipt a delivery that `begin_delivery` started ended with; a repeat of the
    /// message is answered with it.
    pub fn finish_delivery(
        &self,
        session: &str,
        msg_id: &str,
        state: &str,
        detail: &str,
    ) -> io::Result<()> {
        let path = self.delivered_path(session);
        let _guard = self.lock(&path)?;
        let mut records: Vec<DeliveryRecord> = read_records(&path)?;
        let Some(record) = records.iter_mut().find(|r| r.id == msg_id) else {
            return Ok(());
        };
        record.state = state.to_string();
        record.detail = detail.to_string();
        record.pid = 0;
        write_json(&path, &records)
    }

    /// Drop the entry `begin_delivery` made: nothing reached the chat or its inbox, so a resend
    /// is delivered afresh.
    pub fn forget_delivery(&self, session: &str, msg_id: &str) {
        let path = self.delivered_path(session);
        let Ok(_guard) = self.lock(&path) else {
            return;
        };
        let Ok(mut records) = read_records::<DeliveryRecord>(&path) else {
            return;
        };
        let before = records.len();
        records.retain(|r| r.id != msg_id);
        if records.len() != before {
            let _ = write_json(&path, &records);
        }
    }

    /// Whether the delivery record of `session` has a finished entry for message `msg_id`. An
    /// entry still in progress is this delivery's own, so it does not count.
    /// Read without the delivery lock: the caller holds the inbox lock (inbox, then record).
    fn has_delivery_record(&self, session: &str, msg_id: &str) -> bool {
        read_records::<DeliveryRecord>(&self.delivered_path(session))
            .map(|records| {
                records
                    .iter()
                    .any(|r| r.id == msg_id && !r.state.is_empty())
            })
            .unwrap_or(false)
    }

    /// Record that the chat read an inbox message (a drain), so a later resend of the same id
    /// gets this receipt and is not delivered again. Best effort: a failed write is not retried.
    pub fn record_inbox_delivery(&self, session: &str, env: &Envelope) {
        let path = self.delivered_path(session);
        let Ok(_guard) = self.lock(&path) else {
            return;
        };
        let Ok(mut records) = read_records::<DeliveryRecord>(&path) else {
            return;
        };
        if records.iter().any(|r| r.id == env.id) {
            return;
        }
        records.push(DeliveryRecord {
            id: env.id.clone(),
            h: payload_hash(env),
            ts: now_ms(),
            state: "held".to_string(),
            detail: "read from the chat inbox".to_string(),
            pid: 0,
        });
        let excess = records.len().saturating_sub(DELIVERED_CAP);
        records.drain(..excess);
        let _ = write_json(&path, &records);
    }

    // ---- reply routes and the outbound queue -------------------------------

    fn route_path(&self, msg_id: &str) -> PathBuf {
        self.root
            .join("reply-routes")
            .join(format!("{}.json", session_stem(msg_id)))
    }

    /// Remember where a reply to delivered message `route.msg_id` goes.
    pub fn save_reply_route(&self, route: &ReplyRoute) -> io::Result<()> {
        write_json(&self.route_path(&route.msg_id), route)?;
        // The folder scan runs at most once per `ROUTE_PRUNE_EVERY` in this process; a route
        // is only removed when its own age passes `ROUTE_TTL`, so an active route stays.
        static LAST_PRUNE: std::sync::Mutex<Option<std::time::Instant>> =
            std::sync::Mutex::new(None);
        {
            let mut last = LAST_PRUNE.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed() < ROUTE_PRUNE_EVERY) {
                return Ok(());
            }
            *last = Some(std::time::Instant::now());
        }
        let mut kept: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        if let Ok(listing) = fs::read_dir(self.root.join("reply-routes")) {
            for entry in listing.flatten() {
                let Ok(written) = entry.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                let old = written.elapsed().is_ok_and(|age| age > ROUTE_TTL);
                if old {
                    let _ = fs::remove_file(entry.path());
                } else {
                    kept.push((written, entry.path()));
                }
            }
        }
        // Count cap: the newest routes always survive, so an active reply keeps its route.
        if kept.len() > ROUTE_MAX_FILES {
            kept.sort();
            for (_, path) in kept.iter().take(kept.len() - ROUTE_MAX_FILES) {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }

    /// The route saved for message `msg_id`, when there is one.
    pub fn reply_route(&self, msg_id: &str) -> Option<ReplyRoute> {
        read_json::<ReplyRoute>(&self.route_path(msg_id)).filter(|r| r.msg_id == msg_id)
    }

    /// Every saved route created at or after `since_ms`.
    pub fn reply_routes_since(&self, since_ms: u64) -> Vec<ReplyRoute> {
        let mut routes: Vec<ReplyRoute> = fs::read_dir(self.root.join("reply-routes"))
            .map(|listing| {
                listing
                    .flatten()
                    .filter_map(|e| read_json::<ReplyRoute>(&e.path()))
                    .filter(|r| r.created_ms >= since_ms)
                    .collect()
            })
            .unwrap_or_default();
        routes.sort_by_key(|r| r.created_ms);
        routes
    }

    fn outbound_dir(&self) -> PathBuf {
        self.root.join("outbound")
    }

    /// Keep an envelope that could not be sent so the hub can retry it.
    pub fn queue_outbound(&self, env: &Envelope) -> io::Result<()> {
        let dir = self.outbound_dir();
        let queued = |e: &fs::DirEntry| e.path().extension().is_some_and(|x| x == "json");
        let count = fs::read_dir(&dir)
            .map(|l| l.flatten().filter(queued).count())
            .unwrap_or(0);
        if count >= OUTBOUND_CAP {
            return Err(io::Error::other("the outbound queue is full"));
        }
        write_json(&dir.join(format!("{:016x}.json", fnv(&env.id))), env)
    }

    /// Every queued envelope, oldest first. The files stay where they are until
    /// `remove_outbound`; a file that cannot be read as an envelope is removed.
    pub fn outbound(&self) -> Vec<Envelope> {
        let mut out: Vec<Envelope> = Vec::new();
        for file in json_files(&self.outbound_dir()) {
            match read_json::<Envelope>(&file) {
                Some(env) => out.push(env),
                None => {
                    let _ = fs::remove_file(&file);
                }
            }
        }
        out.sort_by_key(|e| e.ts);
        out
    }

    /// Take one envelope out of the queue (it was sent, or is no longer wanted).
    pub fn remove_outbound(&self, id: &str) {
        let _ = fs::remove_file(self.outbound_dir().join(format!("{:016x}.json", fnv(id))));
    }

    // ---- relay heartbeat ---------------------------------------------------

    fn relay_path(&self) -> PathBuf {
        self.root.join("relay.json")
    }

    /// Called every two seconds (by the `chat-heartbeat` thread of `chat::hub`, not by the tick, so a
    /// slow tick cannot stop it), but written only when the alias or this file changed hands, or
    /// `RELAY_HEARTBEAT_MS` after the last write: readers call a heartbeat older than
    /// `RELAY_STALE_MS` (30 s) gone, and both notches restart a hub whose beat is 90 s old.
    /// Not forced to disk: a heartbeat lost to a power cut is stale before anything reads it.
    pub fn write_relay_status(&self, alias: &str) -> io::Result<()> {
        let path = self.relay_path();
        let mut last = LAST_RELAY_WRITE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = last.as_ref().is_some_and(|(written, at)| {
            written.0 == path
                && written.1 == alias
                && now_ms().saturating_sub(*at) < RELAY_HEARTBEAT_MS
        });
        if fresh {
            return Ok(());
        }
        let status = RelayStatus {
            pid: std::process::id(),
            ts: now_ms(),
            alias: alias.to_string(),
        };
        let ts = status.ts;
        let bytes = serde_json::to_vec(&status).map_err(io::Error::other)?;
        if let Some(parent) = path.parent() {
            secure_chain(parent)?;
        }
        rightkit_fs::atomic_write_with_options(
            &path,
            rightkit_fs::AtomicWriteOptions {
                durable: false,
                privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                ..Default::default()
            },
            |file| io::Write::write_all(file, &bytes),
        )?;
        *last = Some(((path, alias.to_string()), ts));
        Ok(())
    }

    /// Forget the heartbeat (the hub's chat stopped).
    pub fn clear_relay_status(&self) {
        *LAST_RELAY_WRITE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let _ = fs::remove_file(self.relay_path());
    }

    pub fn relay_status(&self) -> Option<RelayStatus> {
        read_json(&self.relay_path())
    }

    /// Whether the hub's chat is running.
    pub fn relay_alive(&self) -> bool {
        self.relay_status()
            .is_some_and(|s| now_ms().saturating_sub(s.ts) < RELAY_STALE_MS)
    }

    // ---- Chat switch -------------------------------------------------

    fn policy_path(&self) -> PathBuf {
        self.root.join("policy.json")
    }

    /// One switch of `policy.json`: its documented `default` when the file is absent or the key is
    /// missing or not a boolean; OFF when the file is present but corrupt, not an object, or
    /// unreadable (a link, a permission error).
    fn policy_flag(&self, key: &str, default: bool) -> bool {
        match &*read_policy(&self.policy_path()) {
            PolicyFile::Absent => default,
            PolicyFile::Object(map) => map
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default),
            PolicyFile::Untrusted => false,
        }
    }

    /// Whether Chat is on here (`policy.json`; on when there is no file).
    pub fn chat_enabled(&self) -> bool {
        self.policy_flag("enabled", true)
    }

    /// Turn Chat on or off on this computer.
    pub fn set_chat_enabled(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("enabled", on)
    }

    /// Whether a message for a busy Claude chat goes through the host app plugin's hook
    /// (`policy.json` key `claudeHookDelivery`). An explicit `true`/`false` decides; when the key
    /// is absent it is on once a host app Claude plugin hook ran on this computer within
    /// `CLAUDE_HOOK_RUNNING_WITHIN_SECS`; a policy file that cannot be trusted is off.
    pub fn claude_hook_delivery(&self) -> bool {
        match self.claude_hook_delivery_mode() {
            "on" => true,
            "off" => false,
            _ => claude_hook_running(),
        }
    }

    /// Whether a host app Claude plugin hook ran on this computer within the window `auto` uses.
    pub fn claude_plugin_hook_recent(&self) -> bool {
        claude_hook_running()
    }

    /// The setting as written: `"on"`, `"off"`, or `"auto"` (the key is absent, or not a boolean).
    pub fn claude_hook_delivery_mode(&self) -> &'static str {
        match &*read_policy(&self.policy_path()) {
            PolicyFile::Object(map) => {
                match map
                    .get("claudeHookDelivery")
                    .and_then(serde_json::Value::as_bool)
                {
                    Some(true) => "on",
                    Some(false) => "off",
                    None => "auto",
                }
            }
            PolicyFile::Absent => "auto",
            PolicyFile::Untrusted => "off",
        }
    }

    /// Turn hook delivery into Claude chats on or off on this computer.
    pub fn set_claude_hook_delivery(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("claudeHookDelivery", on)
    }

    /// Set hook delivery to `"on"`, `"off"`, or `"auto"` (removes the key, so the hook decides).
    pub fn set_claude_hook_delivery_mode(&self, mode: &str) -> io::Result<()> {
        match mode {
            "on" => self.write_policy_key("claudeHookDelivery", true),
            "off" => self.write_policy_key("claudeHookDelivery", false),
            "auto" => self.clear_policy_key("claudeHookDelivery"),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hook-delivery takes on, off or auto",
            )),
        }
    }

    /// Whether terminal chats (an interactive `claude` or Codex CLI session) that were active
    /// within the last 6 hours are listed with the app chats (`policy.json` key
    /// `terminalChats`; on when absent).
    pub fn terminal_chats(&self) -> bool {
        self.policy_flag("terminalChats", true)
    }

    /// Turn the listing of recently active terminal chats on or off on this computer.
    pub fn set_terminal_chats(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("terminalChats", on)
    }

    /// Whether Claude chats in other Claude profiles (other home folders, `~/ClaudeProfiles/*/.claude`)
    /// are listed and reached as well (`policy.json` key `claudeProfiles`; on when absent; #350).
    pub fn claude_profiles(&self) -> bool {
        self.policy_flag("claudeProfiles", true)
    }

    /// Turn the other Claude profiles on or off on this computer.
    pub fn set_claude_profiles(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("claudeProfiles", on)
    }

    /// Extra Claude config folders listed by hand (`policy.json` key `claudeHomes`, a list of
    /// paths to `.claude` folders), read in addition to the `~/ClaudeProfiles/*/.claude` default.
    pub fn claude_homes(&self) -> Vec<PathBuf> {
        read_json::<serde_json::Value>(&self.policy_path())
            .and_then(|v| v["claudeHomes"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    /// Whether Muse chats are listed, read and reached, and the host app's Muse hook does anything
    /// (`policy.json` key `muse`; on when absent).
    pub fn muse_chats(&self) -> bool {
        self.harness_chats("muse")
    }

    /// Whether chats of the registered hook harness `kind` are listed, read and reached
    /// (`policy.json` key `muse` for Muse, `harness:<kind>` for any other; on when absent, #434).
    pub fn harness_chats(&self, kind: &str) -> bool {
        self.policy_flag(&harness_policy_key(kind), true)
    }

    /// Turn the chats of the registered hook harness `kind` on or off on this computer.
    pub fn set_harness_chats(&self, kind: &str, on: bool) -> io::Result<()> {
        self.write_policy_key(&harness_policy_key(kind), on)
    }

    /// Where harness plugins write their registration records (`harness::kinds`).
    pub fn harnesses_dir(&self) -> PathBuf {
        self.root.join("harnesses")
    }

    /// Whether a message this computer sends carries the user's latest turns from the sending
    /// chat's transcript (`said::stamp_for`, #400; `policy.json` key `stampUserWords`; on when absent).
    pub fn stamp_user_words(&self) -> bool {
        self.policy_flag("stampUserWords", true)
    }

    /// Turn the stamp of the user's typed words into messages on or off on this computer.
    pub fn set_stamp_user_words(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("stampUserWords", on)
    }

    /// Turn Muse chats on or off on this computer.
    pub fn set_muse_chats(&self, on: bool) -> io::Result<()> {
        self.set_harness_chats("muse", on)
    }

    /// Whether a message for an idle Codex chat starts a turn in it
    /// (`policy.json` key `codexWakeIdle`; on when absent: the owner 2026-10-22, "Wake by default",
    /// #410, #140). The turn is spent in the woken chat, and every receipt says one was started.
    pub fn codex_wake_idle(&self) -> bool {
        self.policy_flag("codexWakeIdle", true)
    }

    /// Whether a message for an idle Claude Desktop chat opens it so it starts a turn
    /// (`policy.json` key `claudeWakeIdle`; on when absent). Off: the message waits in the
    /// chat's hook inbox and Chat inbox.
    pub fn claude_wake_idle(&self) -> bool {
        self.policy_flag("claudeWakeIdle", true)
    }

    /// Turn starting a turn in idle Claude Desktop chats on or off on this computer.
    pub fn set_claude_wake_idle(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("claudeWakeIdle", on)
    }

    /// Whether a message for an idle Muse chat starts a turn in it
    /// (`policy.json` key `museWakeIdle`; on when absent). Muse has no supported wake entry yet
    /// (`muse::muse_wake_command`), so the receipt for an idle Muse chat says it is held.
    pub fn muse_wake_idle(&self) -> bool {
        self.policy_flag("museWakeIdle", true)
    }

    /// Turn starting a turn in idle Muse chats on or off on this computer.
    pub fn set_muse_wake_idle(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("museWakeIdle", on)
    }

    /// Turn starting a turn in idle Codex chats on or off on this computer.
    pub fn set_codex_wake_idle(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("codexWakeIdle", on)
    }

    /// Whether a message for a Codex chat the desktop app has loaded is handed to the app
    /// over its IPC socket (`policy.json` key `codexAppDelivery`; on when absent).
    pub fn codex_app_delivery(&self) -> bool {
        self.policy_flag("codexAppDelivery", true)
    }

    /// Whether a message to a Codex chat the app has loaded goes in as hidden context: the idle
    /// chat is woken with one short line and the host app's hook hands it the full text (`policy.json`
    /// key `codexHiddenDelivery`; on when absent, an upstream issue). Off: the whole message is
    /// the turn's visible prompt.
    pub fn codex_hidden_delivery(&self) -> bool {
        self.policy_flag("codexHiddenDelivery", true)
    }

    /// Turn hidden Codex delivery on or off on this computer.
    pub fn set_codex_hidden_delivery(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("codexHiddenDelivery", on)
    }

    /// Turn handing messages to the Codex app over its IPC socket on or off on this computer.
    pub fn set_codex_app_delivery(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("codexAppDelivery", on)
    }

    /// Set one key of `policy.json`, keeping the others, through the owner's `Store::update` over
    /// [`PolicyBackend`]: the backend takes the cross-process `policy.lock`, reads the file
    /// no-follow, and publishes with the owner-only privacy this store uses everywhere else, so
    /// no outer lock is needed. Keys this build does not know stay as written. A file that is not
    /// JSON or not an object is the all-OFF base (`POLICY_FLAGS`), so the write keeps every other switch
    /// off; a file that cannot be read at all refuses the write.
    fn write_policy_key(&self, key: &str, on: bool) -> io::Result<()> {
        self.edit_policy(|policy| policy[key] = serde_json::json!(on))
    }

    /// Remove a key from `policy.json`, so its default applies again.
    fn clear_policy_key(&self, key: &str) -> io::Result<()> {
        self.edit_policy(|policy| {
            if let Some(map) = policy.as_object_mut() {
                map.remove(key);
            }
        })
    }

    /// Edit `policy.json` through the store's own writer (lock, admission, publication).
    fn edit_policy(&self, edit: impl FnOnce(&mut serde_json::Value)) -> io::Result<()> {
        let path = self.policy_path();
        // Forget before and after: a reader between the write and the second call re-reads from disk.
        policy_cache_forget(&path);
        let result = self.edit_policy_file(edit);
        policy_cache_forget(&path);
        result
    }

    fn edit_policy_file(&self, edit: impl FnOnce(&mut serde_json::Value)) -> io::Result<()> {
        use rightkit_app_settings::{FileFormat, Schema, Store, StoreOptions};
        let options = StoreOptions::new(Schema::new(&super::brand::brand().policy_schema, 1))
            .format(FileFormat::Flat)
            .defaults_provider(|| serde_json::json!({}));
        let store = Store::<serde_json::Value>::with_backend(
            PolicyBackend::new(self.policy_path()),
            options,
        )
        .map_err(io::Error::other)?;
        store
            .update(|policy| {
                if !policy.is_object() {
                    // The backend already hands over every switch off for an untrusted file.
                    *policy = serde_json::Value::Object(
                        POLICY_FLAGS
                            .iter()
                            .map(|k| ((*k).to_string(), serde_json::Value::Bool(false)))
                            .collect(),
                    );
                }
                edit(policy);
                Ok(())
            })
            .map_err(io::Error::other)
    }

    /// Whether a chat's hook run shows its owner the receipts of its sent messages
    /// (`policy.json` key `chatReceiptNotices`; on when absent).
    pub fn receipt_notices(&self) -> bool {
        self.policy_flag("chatReceiptNotices", true)
    }

    /// Turn receipt notices in chat hooks on or off on this computer.
    pub fn set_receipt_notices(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("chatReceiptNotices", on)
    }

    /// Whether a message to a Claude chat whose registry route is refused goes straight to that
    /// chat's live messaging socket.
    pub fn socket_fallback(&self) -> bool {
        self.policy_flag("chatSocketFallback", true)
    }

    /// Turn the live-socket fallback for Claude chats on or off on this computer.
    pub fn set_socket_fallback(&self, on: bool) -> io::Result<()> {
        self.write_policy_key("chatSocketFallback", on)
    }

    // ---- activity ----------------------------------------------------------

    fn activity_path(&self) -> PathBuf {
        self.root.join("activity.json")
    }

    /// The last send and receive on this computer (zeros when there were none).
    pub fn activity(&self) -> Activity {
        read_json(&self.activity_path()).unwrap_or_default()
    }

    /// Whether a chat has already been handed a host app Chat message. The first one carries the
    /// full introduction; later ones carry the short header only. An unreadable record counts
    /// as not met.
    pub fn chat_met(&self, session: &str) -> bool {
        let met: std::collections::BTreeSet<String> =
            self.read_state(CHATS_MET_FILE).unwrap_or_default();
        met.contains(session)
    }

    /// Record that a chat has been handed a host app Chat message (see `chat_met`).
    pub fn note_chat_met(&self, session: &str) {
        let _ = self.update_state(
            CHATS_MET_FILE,
            |met: &mut std::collections::BTreeSet<String>| {
                met.insert(session.to_string());
            },
        );
    }

    /// A chat message left this computer (or went between two chats here);
    /// `outcome` is its receipt state word.
    pub fn note_sent(&self, outcome: &str) {
        self.note_activity(true, outcome);
    }

    /// A chat message arrived from a linked computer; `outcome` as for `note_sent`.
    pub fn note_received(&self, outcome: &str) {
        self.note_activity(false, outcome);
    }

    fn note_activity(&self, sent: bool, outcome: &str) {
        let path = self.activity_path();
        let Ok(_lock) = self.lock(&path) else { return };
        let mut activity = self.activity();
        if sent {
            activity.sent_ms = now_ms();
            activity.sent += 1;
        } else {
            activity.received_ms = now_ms();
            activity.received += 1;
        }
        activity.last_outcome = outcome.to_string();
        let _ = write_json(&path, &activity);
    }
}
