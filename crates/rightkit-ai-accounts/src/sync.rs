//! Claude Desktop Code-session sync across accounts.
//!
//! Claude Desktop keeps one real folder of session metadata per
//! `<accountUUID>/<orgUUID>` under `claude-code-sessions/`. Symlinks between
//! those folders do not work (Claude opens them with `O_NOFOLLOW`), so this
//! module merges the *metadata* of every folder into every other one, while
//! Claude is fully closed. Transcripts (the shared conversation content under
//! `~/.claude`) are never read or written here. Layout and rules:
//! `docs/claude-account-switch.md`.
//!
//! Per folder the merge understands exactly three kinds of file:
//! `local_<uuid>.json` (a session record), `deleted_<uuid>` (a deletion marker
//! whose content is a millisecond timestamp) and `archived-sessions.idx`
//! (`{"v":1,"archived":[...ids]}`, rebuilt from the merged records). Anything
//! else (`backlog/`, `scheduled-tasks.json`, `waiting-input`) is left alone.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_fs::atomic::sync_dir;
use rightkit_fs::{AppendOptions, append_record, ensure_private_dir};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How many backups are kept; older ones are pruned after a successful apply.
pub const KEEP_BACKUPS: usize = 10;
const SESSIONS_DIR: &str = "claude-code-sessions";
const INDEX_FILE: &str = "archived-sessions.idx";

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Claude Desktop is not supported on this platform or has no data folder here: {0}")]
    Unsupported(String),
    #[error("Claude is running. Quit it completely, then sync.")]
    Running,
    #[error("sync is blocked: {}", .0.join("; "))]
    Blocked(Vec<String>),
    #[error("{0} changed since the plan was made; nothing was written")]
    Changed(String),
    #[error("{0}")]
    Io(String),
    #[error("unknown backup {0}")]
    NoBackup(String),
    #[error("files changed since this sync; restore with --force to overwrite them: {}", .0.join(", "))]
    RestoreMismatch(Vec<String>),
}

impl SyncError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "unsupported",
            Self::Running => "claude_running",
            Self::Blocked(_) => "blocked",
            Self::Changed(_) => "changed",
            Self::Io(_) => "io",
            Self::NoBackup(_) => "no_backup",
            Self::RestoreMismatch(_) => "restore_mismatch",
        }
    }
}

fn io<E: std::fmt::Display>(context: &str, error: E) -> SyncError {
    SyncError::Io(format!("{context}: {error}"))
}

// ---------------------------------------------------------------- locations

/// The default Claude Desktop data folder for this platform, whether or not it exists.
/// The account's own Claude Desktop folder. It comes from the account's home in the password
/// database, not `HOME`: a second profile (Claude B) runs with `HOME` pointed at
/// `~/ClaudeProfiles/<name>`, and its chats are never synced.
fn default_location() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let root = Some(
        rightkit_app_host::account_home()
            .join("Library")
            .join("Application Support")
            .join("Claude"),
    );
    #[cfg(windows)]
    let root = rightkit_app_host::env_var("APPDATA").map(|a| PathBuf::from(a).join("Claude"));
    #[cfg(not(any(target_os = "macos", windows)))]
    let root: Option<PathBuf> = None;
    root
}

/// Claude Desktop's data folder, when this platform has one that exists.
pub fn default_root() -> Result<PathBuf, SyncError> {
    let root = default_location();
    let root = root.ok_or_else(|| SyncError::Unsupported("no known location".into()))?;
    // Windows (and anything else) is only supported when it has the same layout.
    if !root.join(SESSIONS_DIR).is_dir() {
        return Err(SyncError::Unsupported(format!(
            "{} has no {SESSIONS_DIR} folder; not supported yet",
            root.display()
        )));
    }
    Ok(root)
}

pub fn default_backups() -> Result<PathBuf, SyncError> {
    #[cfg(any(target_os = "macos", windows))]
    let base = rightkit_app_host::state_root();
    #[cfg(not(any(target_os = "macos", windows)))]
    let base: Option<PathBuf> = None;
    base.map(|b| b.join("claude-sync-backups"))
        .ok_or_else(|| SyncError::Unsupported("no known location".into()))
}

// ------------------------------------------------------------ running check

pub(crate) const USER_DATA_DIR: &str = "--user-data-dir=";

/// A data folder compared by identity: canonical when it exists, no trailing separator, and
/// case-folded on Windows.
pub(crate) fn data_dir_key(path: &Path) -> String {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy().into_owned();
    let text = text.trim_end_matches(['/', '\\']).to_string();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn data_dir_of(cmd: &[std::ffi::OsString]) -> Option<String> {
    cmd.iter().find_map(|arg| {
        arg.to_string_lossy()
            .strip_prefix(USER_DATA_DIR)
            .map(|dir| data_dir_key(Path::new(dir.trim_matches('"'))))
    })
}

pub(crate) fn is_helper(cmd: &[std::ffi::OsString]) -> bool {
    cmd.iter()
        .any(|a| a.to_string_lossy().starts_with("--type="))
}

/// The Claude Desktop instances that use `root` as their data folder. A main process names its
/// folder through its own `--user-data-dir` or its helpers'. One whose folder is unknown (no
/// helper yet, just launching) counts as using `root`: sync never writes into a folder that a
/// running Desktop might own.
fn instances_for(root: &Path) -> Vec<u32> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let procs = system.processes();
    let wanted = data_dir_key(root);
    let claude = |p: &sysinfo::Process| {
        p.exe()
            .is_some_and(|exe| is_claude_exe(&exe.to_string_lossy()))
    };
    let mains: Vec<u32> = procs
        .iter()
        .filter(|(_, p)| claude(p) && !is_helper(p.cmd()))
        .map(|(pid, _)| pid.as_u32())
        .collect();
    let mut found = Vec::new();
    for &main in &mains {
        let mut tree = vec![main];
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        if let Some(dir) = procs
            .get(&sysinfo::Pid::from_u32(main))
            .and_then(|p| data_dir_of(p.cmd()))
        {
            dirs.insert(dir);
        }
        let mut i = 0;
        while i < tree.len() {
            let parent = tree[i];
            for (pid, p) in procs {
                let pid = pid.as_u32();
                if p.parent().map(|x| x.as_u32()) == Some(parent) && !tree.contains(&pid) {
                    if claude(p)
                        && let Some(dir) = data_dir_of(p.cmd())
                    {
                        dirs.insert(dir);
                    }
                    tree.push(pid);
                }
            }
            i += 1;
        }
        let ours = dirs.is_empty() || dirs.contains(&wanted);
        if ours {
            found.push(main);
        }
    }
    found
}

/// Whether the Claude Desktop instance that uses `root` is running. Looks at the process table
/// by executable path and `--user-data-dir`; no AppleScript, no Launch Services.
pub fn claude_running_for(root: &Path) -> bool {
    // Test seam (test-overrides builds only): journeys decide explicitly, whatever the host runs.
    #[cfg(feature = "test-overrides")]
    if let Some(forced) = std::env::var_os("RIGHTKIT_TEST_CLAUDE_RUNNING") {
        return forced == "1";
    }
    !instances_for(root).is_empty()
}

#[cfg(target_os = "macos")]
pub fn is_claude_exe(path: &str) -> bool {
    path.contains("/Claude.app/Contents/")
}

#[cfg(windows)]
pub fn is_claude_exe(path: &str) -> bool {
    let lower = path.to_lowercase();
    // The website install, or the Microsoft Store app (`WindowsApps\Claude_*\app\Claude.exe`).
    lower.contains("\\anthropicclaude\\")
        || lower.ends_with("\\claude\\claude.exe")
        || (lower.contains("\\windowsapps\\claude_") && lower.ends_with("\\claude.exe"))
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_claude_exe(_path: &str) -> bool {
    false
}

// ------------------------------------------------------------------ signatures

/// What "unchanged" means: size, modification time and a content hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Sig {
    size: u64,
    mtime_ns: u64,
    hash: u64,
}

impl Sig {
    fn same_content(&self, other: &Sig) -> bool {
        self.size == other.size && self.hash == other.hash
    }
}

/// FNV-1a, 64 bit: change detection, not security.
fn fnv(bytes: &[u8], mut state: u64) -> u64 {
    for b in bytes {
        state ^= u64::from(*b);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
    state
}
const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

fn hash_bytes(bytes: &[u8]) -> u64 {
    fnv(bytes, FNV_START)
}

/// [`hash_bytes`] for the switcher's login snapshot: tells a torn copy from a whole one.
pub(crate) fn fingerprint(bytes: &[u8]) -> u64 {
    hash_bytes(bytes)
}

fn mtime_ns(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Open a regular file for reading without following a final symlink or reparse point
/// (RightKit's `open_read_nofollow`).
fn open_read(path: &Path) -> std::io::Result<fs::File> {
    rightkit_fs::open_read_nofollow(path, &rightkit_fs::ReadOptions::new())
        .map_err(std::io::Error::from)
}

/// The signature of a regular file; `Ok(None)` when it does not exist, an
/// error for a symlink or anything that is not a regular file.
fn sig_of(path: &Path) -> Result<Option<Sig>, SyncError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(&path.display().to_string(), e)),
    };
    if !meta.file_type().is_file() {
        return Err(SyncError::Io(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut file = open_read(path).map_err(|e| io(&path.display().to_string(), e))?;
    let mut hash = FNV_START;
    let mut size = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| io(&path.display().to_string(), e))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hash = fnv(&buffer[..n], hash);
    }
    Ok(Some(Sig {
        size,
        mtime_ns: mtime_ns(&meta),
        hash,
    }))
}

fn read_all(path: &Path) -> Result<Vec<u8>, SyncError> {
    let mut file = open_read(path).map_err(|e| io(&path.display().to_string(), e))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| io(&path.display().to_string(), e))?;
    Ok(bytes)
}

// -------------------------------------------------------------------- discovery

pub(crate) fn looks_like_uuid(name: &str) -> bool {
    name.len() == 36 && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountInfo {
    pub id: String,
    /// Short form for display: no email or name is stored in Claude's
    /// non-secret files, so this is the first block of the account id.
    pub label: String,
    /// Claude's last signed-in account (`lastKnownAccountUuid` in config.json).
    pub active: bool,
    /// Real `<org>` folders under `claude-code-sessions/<account>/`.
    pub folders: usize,
    pub sessions: usize,
    pub archived: usize,
    pub deleted_markers: usize,
    /// False when the account has no Code-sessions folder yet, so there is
    /// nothing to sync into until Claude creates it on its first sign-in.
    pub syncable: bool,
}

struct Folder {
    account: String,
    org: String,
    key: String,
    path: PathBuf,
    recs: BTreeMap<String, Rec>,
    tombs: BTreeMap<String, Tomb>,
    index: Option<Index>,
}

struct Rec {
    sig: Sig,
    /// Newest of lastActivityAt / lastFocusedAt / createdAt, milliseconds.
    ts: i64,
    /// Which copy is further along: completed turns, then the last user
    /// message time, then `ts`. Claude Desktop bumps `lastActivityAt` on a
    /// stale copy merely by listing it after a sign-in, so time alone would let
    /// a copy with a hundred fewer turns (and no archive flag) overwrite the
    /// real one; progress cannot be faked by a touch.
    rank: (i64, i64, i64),
    archived: bool,
    readable: bool,
}

struct Tomb {
    sig: Sig,
    ms: Option<i64>,
}

struct Index {
    sig: Sig,
    archived: BTreeSet<String>,
    compatible: bool,
}

#[derive(Deserialize)]
struct RecordMeta {
    #[serde(rename = "lastActivityAt")]
    last_activity_at: Option<f64>,
    #[serde(rename = "lastFocusedAt")]
    last_focused_at: Option<f64>,
    #[serde(rename = "createdAt")]
    created_at: Option<f64>,
    #[serde(rename = "isArchived")]
    is_archived: Option<bool>,
    #[serde(rename = "completedTurns")]
    completed_turns: Option<f64>,
    #[serde(rename = "latestUserFrameAt")]
    latest_user_frame_at: Option<f64>,
}

pub fn read_active_account(root: &Path) -> Option<String> {
    // Only this one non-secret key is read; the document (which also holds
    // sign-in material) is dropped immediately and never logged or copied.
    let bytes = fs::read(root.join("config.json")).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("lastKnownAccountUuid")?
        .as_str()
        .filter(|s| looks_like_uuid(s))
        .map(str::to_string)
}

fn sorted_dirs(path: &Path, blockers: &mut Vec<String>) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !looks_like_uuid(&name) {
            continue;
        }
        let p = entry.path();
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => blockers.push(format!(
                "{} is a symlink; Claude cannot write through one. Replace it with a real folder first",
                p.display()
            )),
            Ok(m) if m.is_dir() => out.push((name, p)),
            _ => {}
        }
    }
    out.sort();
    out
}

fn load_folders(root: &Path, blockers: &mut Vec<String>) -> Result<Vec<Folder>, SyncError> {
    let sessions = root.join(SESSIONS_DIR);
    let mut folders = Vec::new();
    for (account, account_path) in sorted_dirs(&sessions, blockers) {
        for (org, path) in sorted_dirs(&account_path, blockers) {
            folders.push(load_folder(&account, &org, &path, blockers)?);
        }
    }
    Ok(folders)
}

fn load_folder(
    account: &str,
    org: &str,
    path: &Path,
    blockers: &mut Vec<String>,
) -> Result<Folder, SyncError> {
    load_folder_inner(account, org, path, blockers, &mut None, None)
}

/// Read one body. With `skipped` set (the mirror, which reads a folder Claude
/// is writing) a file that cannot be read is counted and left out instead of
/// failing the whole read.
fn read_or_skip(file: &Path, skipped: &mut Option<usize>) -> Result<Option<Vec<u8>>, SyncError> {
    match read_all(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) => match skipped {
            Some(n) => {
                *n += 1;
                Ok(None)
            }
            None => Err(e),
        },
    }
}

fn parse_record(sig: Sig, bytes: &[u8]) -> Rec {
    match serde_json::from_slice::<RecordMeta>(bytes) {
        Ok(meta) => {
            let ts = [meta.last_activity_at, meta.last_focused_at, meta.created_at]
                .into_iter()
                .flatten()
                .fold(0f64, f64::max) as i64;
            Rec {
                sig,
                ts,
                rank: (
                    meta.completed_turns.unwrap_or(0.0) as i64,
                    meta.latest_user_frame_at.unwrap_or(0.0) as i64,
                    ts,
                ),
                archived: meta.is_archived.unwrap_or(false),
                readable: true,
            }
        }
        Err(_) => Rec {
            sig,
            ts: 0,
            rank: (0, 0, 0),
            archived: false,
            readable: false,
        },
    }
}

fn parse_index(sig: Sig, bytes: &[u8]) -> Index {
    let parsed = serde_json::from_slice::<Value>(bytes).ok();
    let (archived, compatible) = match parsed.as_ref().and_then(Value::as_object) {
        Some(map)
            if map.get("v") == Some(&Value::from(1))
                && map.len() == 2
                && map.get("archived").is_some_and(Value::is_array) =>
        {
            let set = map["archived"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            (set, true)
        }
        _ => (BTreeSet::new(), false),
    };
    Index {
        sig,
        archived,
        compatible,
    }
}

fn load_folder_inner(
    account: &str,
    org: &str,
    path: &Path,
    blockers: &mut Vec<String>,
    skipped: &mut Option<usize>,
    mut cache: Option<&mut MirrorCache>,
) -> Result<Folder, SyncError> {
    let mut folder = Folder {
        account: account.to_string(),
        org: org.to_string(),
        key: format!("{account}/{org}"),
        path: path.to_path_buf(),
        recs: BTreeMap::new(),
        tombs: BTreeMap::new(),
        index: None,
    };
    let entries = fs::read_dir(path).map_err(|e| io(&path.display().to_string(), e))?;
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let record = name
            .strip_prefix("local_")
            .and_then(|s| s.strip_suffix(".json"));
        let tomb = name.strip_prefix("deleted_");
        let is_index = name == INDEX_FILE;
        if record.is_none() && tomb.is_none() && !is_index {
            continue;
        }
        let file = path.join(&name);
        // The mirror's path: a record or marker whose size and modification
        // time are unchanged is not read again.
        if let (Some(c), false) = (cache.as_deref_mut(), is_index) {
            match cached_entry(c, &file, record.is_some()) {
                Ok(Some(entry)) => {
                    if let Some(id) = record {
                        folder.recs.insert(id.to_string(), entry.rec());
                    } else if let Some(id) = tomb {
                        folder.tombs.insert(
                            id.to_string(),
                            Tomb {
                                sig: entry.sig(),
                                ms: entry.ms,
                            },
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => match skipped {
                    Some(n) => *n += 1,
                    None => blockers.push(e.to_string()),
                },
            }
            continue;
        }
        let sig = match sig_of(&file) {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                match skipped {
                    Some(n) => *n += 1,
                    None => blockers.push(e.to_string()),
                }
                continue;
            }
        };
        let Some(bytes) = read_or_skip(&file, skipped)? else {
            continue;
        };
        if let Some(id) = record {
            folder
                .recs
                .insert(id.to_string(), parse_record(sig, &bytes));
        } else if let Some(id) = tomb {
            let ms = String::from_utf8(bytes)
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok());
            folder.tombs.insert(id.to_string(), Tomb { sig, ms });
        } else {
            folder.index = Some(parse_index(sig, &bytes));
        }
    }
    Ok(folder)
}

/// Accounts Claude Desktop knows on this Mac, with the active one marked.
pub fn accounts(root: &Path) -> Result<Vec<AccountInfo>, SyncError> {
    let mut blockers = Vec::new();
    let folders = load_folders(root, &mut blockers)?;
    let active = read_active_account(root);
    let mut ids: BTreeSet<String> = folders.iter().map(|f| f.account.clone()).collect();
    // Accounts that only appear in agent-mode sessions are listed, not synced.
    for (id, _) in sorted_dirs(&root.join("local-agent-mode-sessions"), &mut Vec::new()) {
        ids.insert(id);
    }
    if let Some(a) = &active {
        ids.insert(a.clone());
    }
    Ok(ids
        .into_iter()
        .map(|id| {
            let mine: Vec<&Folder> = folders.iter().filter(|f| f.account == id).collect();
            let sessions: BTreeSet<&String> = mine.iter().flat_map(|f| f.recs.keys()).collect();
            let archived: BTreeSet<&String> = mine
                .iter()
                .flat_map(|f| f.recs.iter().filter(|(_, r)| r.archived).map(|(k, _)| k))
                .collect();
            let deleted: BTreeSet<&String> = mine.iter().flat_map(|f| f.tombs.keys()).collect();
            AccountInfo {
                label: id.chars().take(8).collect(),
                active: active.as_deref() == Some(id.as_str()),
                folders: mine.len(),
                sessions: sessions.len(),
                archived: archived.len(),
                deleted_markers: deleted.len(),
                syncable: !mine.is_empty(),
                id,
            }
        })
        .collect())
}

// ------------------------------------------------------------------------ plan

#[derive(Clone, Debug, Default, Serialize)]
pub struct FolderPlan {
    pub folder: String,
    pub account: String,
    pub org: String,
    pub sessions_before: usize,
    pub add: Vec<String>,
    pub update: Vec<String>,
    pub delete: Vec<String>,
    pub deletion_markers_added: Vec<String>,
    pub deletion_markers_removed: Vec<String>,
    pub archive_index_rewritten: bool,
}

impl FolderPlan {
    fn changes(&self) -> usize {
        self.add.len()
            + self.update.len()
            + self.delete.len()
            + self.deletion_markers_added.len()
            + self.deletion_markers_removed.len()
            + usize::from(self.archive_index_rewritten)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Conflict {
    pub session_id: String,
    pub reason: String,
    pub folders: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Totals {
    pub sessions: usize,
    pub add: usize,
    pub update: usize,
    pub delete: usize,
    pub deletion_markers: usize,
    pub indexes: usize,
    pub files: usize,
    pub conflicts: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub root: String,
    pub accounts: Vec<AccountInfo>,
    pub folders: Vec<FolderPlan>,
    pub conflicts: Vec<Conflict>,
    /// Anything here makes `apply` refuse.
    pub blockers: Vec<String>,
    pub totals: Totals,
    #[serde(skip)]
    ops: Vec<Op>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Add,
    Update,
    Remove,
}

#[derive(Clone, Debug)]
enum Content {
    Inline(Vec<u8>),
    /// Another folder's file, verified against its planned signature.
    Copy {
        path: PathBuf,
        sig: Sig,
    },
}

#[derive(Clone, Debug)]
struct Op {
    folder: String,
    dir: PathBuf,
    name: String,
    kind: Kind,
    /// The target as planned: `None` means it must not exist.
    expect: Option<Sig>,
    content: Option<Content>,
}

impl Op {
    fn target(&self) -> PathBuf {
        self.dir.join(&self.name)
    }
    fn rel(&self) -> String {
        format!("{}/{}", self.folder, self.name)
    }
}

/// Build the merge plan across every account's real folders. Reads only.
pub fn plan(root: &Path) -> Result<Plan, SyncError> {
    plan_for(root, None)
}

/// [`plan`] limited to the accounts in `only` (the sync set); `None` is all.
pub fn plan_for(root: &Path, only: Option<&BTreeSet<String>>) -> Result<Plan, SyncError> {
    let mut blockers = Vec::new();
    let mut folders = load_folders(root, &mut blockers)?;
    folders.retain(|f| only.is_none_or(|set| set.contains(&f.account)));
    let accounts = accounts(root)?;
    let mut conflicts = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut plans: Vec<FolderPlan> = folders
        .iter()
        .map(|f| FolderPlan {
            folder: f.key.clone(),
            account: f.account.clone(),
            org: f.org.clone(),
            sessions_before: f.recs.len(),
            ..FolderPlan::default()
        })
        .collect();
    // The archive state each folder ends up with, by record id.
    let mut final_archived: Vec<BTreeSet<String>> = vec![BTreeSet::new(); folders.len()];

    let keys: BTreeSet<&String> = folders
        .iter()
        .flat_map(|f| f.recs.keys().chain(f.tombs.keys()))
        .collect();
    let session_count = keys.len();

    for key in keys {
        let session_id = format!("local_{key}");
        let holders: Vec<(usize, &Rec)> = folders
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.recs.get(key).map(|r| (i, r)))
            .collect();
        let marks: Vec<(usize, &Tomb)> = folders
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.tombs.get(key).map(|t| (i, t)))
            .collect();
        let keep_own = |final_archived: &mut Vec<BTreeSet<String>>, sid: &str| {
            for (i, r) in &holders {
                if r.archived {
                    final_archived[*i].insert(sid.to_string());
                }
            }
        };
        let involved = |list: Vec<usize>| -> Vec<String> {
            list.into_iter().map(|i| folders[i].key.clone()).collect()
        };

        if holders.iter().any(|(_, r)| !r.readable) || marks.iter().any(|(_, t)| t.ms.is_none()) {
            conflicts.push(Conflict {
                session_id: session_id.clone(),
                reason: "a record or deletion marker could not be read; left untouched".into(),
                folders: involved(
                    holders
                        .iter()
                        .map(|(i, _)| *i)
                        .chain(marks.iter().map(|(i, _)| *i))
                        .collect(),
                ),
            });
            keep_own(&mut final_archived, &session_id);
            continue;
        }

        let best_rank = holders.iter().map(|(_, r)| r.rank).max();
        let best_ts = holders.iter().map(|(_, r)| r.ts).max();
        if let Some(best_rank) = best_rank {
            let newest: Vec<&(usize, &Rec)> = holders
                .iter()
                .filter(|(_, r)| r.rank == best_rank)
                .collect();
            let distinct: BTreeSet<(u64, u64)> = newest
                .iter()
                .map(|(_, r)| (r.sig.size, r.sig.hash))
                .collect();
            if distinct.len() > 1 {
                conflicts.push(Conflict {
                    session_id: session_id.clone(),
                    reason: "same progress and timestamp, different content; both left untouched"
                        .into(),
                    folders: involved(newest.iter().map(|(i, _)| *i).collect()),
                });
                keep_own(&mut final_archived, &session_id);
                continue;
            }
        }
        let deleted_at = marks.iter().filter_map(|(_, t)| t.ms).max();

        let deletion_wins = match (deleted_at, best_ts) {
            (Some(_), None) => true,
            (Some(d), Some(r)) => d >= r,
            _ => false,
        };

        if deletion_wins {
            let d = deleted_at.unwrap_or_default();
            for (i, f) in folders.iter().enumerate() {
                if let Some(r) = f.recs.get(key) {
                    plans[i].delete.push(session_id.clone());
                    ops.push(Op {
                        folder: f.key.clone(),
                        dir: f.path.clone(),
                        name: format!("local_{key}.json"),
                        kind: Kind::Remove,
                        expect: Some(r.sig),
                        content: None,
                    });
                }
                let name = format!("deleted_{key}");
                match f.tombs.get(key) {
                    None => {
                        plans[i].deletion_markers_added.push(session_id.clone());
                        ops.push(Op {
                            folder: f.key.clone(),
                            dir: f.path.clone(),
                            name,
                            kind: Kind::Add,
                            expect: None,
                            content: Some(Content::Inline(d.to_string().into_bytes())),
                        });
                    }
                    Some(t) if t.ms.is_some_and(|m| m < d) => {
                        plans[i].deletion_markers_added.push(session_id.clone());
                        ops.push(Op {
                            folder: f.key.clone(),
                            dir: f.path.clone(),
                            name,
                            kind: Kind::Update,
                            expect: Some(t.sig),
                            content: Some(Content::Inline(d.to_string().into_bytes())),
                        });
                    }
                    Some(_) => {}
                }
            }
            continue;
        }

        // A record is the newest word on this session: the furthest-along copy.
        let (winner_folder, winner) = holders
            .iter()
            .filter(|(_, r)| Some(r.rank) == best_rank)
            .map(|(i, r)| (*i, *r))
            .next()
            .expect("a record exists when deletion did not win");
        let source = Content::Copy {
            path: folders[winner_folder]
                .path
                .join(format!("local_{key}.json")),
            sig: winner.sig,
        };
        for (i, f) in folders.iter().enumerate() {
            if let Some(t) = f.tombs.get(key) {
                // An older deletion: the session was used after it.
                plans[i].deletion_markers_removed.push(session_id.clone());
                ops.push(Op {
                    folder: f.key.clone(),
                    dir: f.path.clone(),
                    name: format!("deleted_{key}"),
                    kind: Kind::Remove,
                    expect: Some(t.sig),
                    content: None,
                });
            }
            let name = format!("local_{key}.json");
            match f.recs.get(key) {
                None => {
                    plans[i].add.push(session_id.clone());
                    ops.push(Op {
                        folder: f.key.clone(),
                        dir: f.path.clone(),
                        name,
                        kind: Kind::Add,
                        expect: None,
                        content: Some(source.clone()),
                    });
                }
                Some(r) if !r.sig.same_content(&winner.sig) => {
                    plans[i].update.push(session_id.clone());
                    ops.push(Op {
                        folder: f.key.clone(),
                        dir: f.path.clone(),
                        name,
                        kind: Kind::Update,
                        expect: Some(r.sig),
                        content: Some(source.clone()),
                    });
                }
                Some(_) => {}
            }
            if winner.archived {
                final_archived[i].insert(session_id.clone());
            }
        }
    }

    // The archive index, rebuilt from what each folder will hold.
    for (i, f) in folders.iter().enumerate() {
        let wanted = &final_archived[i];
        let body = index_bytes(wanted);
        match &f.index {
            Some(ix) if !ix.compatible => blockers.push(format!(
                "{}/{INDEX_FILE} has an unrecognised format; not rewriting it",
                f.key
            )),
            Some(ix) if ix.archived == *wanted => {}
            Some(ix) => {
                plans[i].archive_index_rewritten = true;
                ops.push(Op {
                    folder: f.key.clone(),
                    dir: f.path.clone(),
                    name: INDEX_FILE.into(),
                    kind: Kind::Update,
                    expect: Some(ix.sig),
                    content: Some(Content::Inline(body)),
                });
            }
            None => {
                plans[i].archive_index_rewritten = true;
                ops.push(Op {
                    folder: f.key.clone(),
                    dir: f.path.clone(),
                    name: INDEX_FILE.into(),
                    kind: Kind::Add,
                    expect: None,
                    content: Some(Content::Inline(body)),
                });
            }
        }
    }
    // Records and markers first, the index last: a crash leaves an index that
    // restore (or the next sync) repairs, never records no index describes.
    ops.sort_by_key(|o| o.name == INDEX_FILE);

    let totals = Totals {
        sessions: session_count,
        add: plans.iter().map(|p| p.add.len()).sum(),
        update: plans.iter().map(|p| p.update.len()).sum(),
        delete: plans.iter().map(|p| p.delete.len()).sum(),
        deletion_markers: plans
            .iter()
            .map(|p| p.deletion_markers_added.len() + p.deletion_markers_removed.len())
            .sum(),
        indexes: plans.iter().filter(|p| p.archive_index_rewritten).count(),
        files: ops.len(),
        conflicts: conflicts.len(),
    };
    debug_assert_eq!(
        ops.len(),
        plans.iter().map(FolderPlan::changes).sum::<usize>()
    );
    Ok(Plan {
        root: root.display().to_string(),
        accounts,
        folders: plans,
        conflicts,
        blockers,
        totals,
        ops,
    })
}

/// `{"v":1,"archived":[...]}`, ids sorted, no trailing newline: byte-for-byte
/// what Claude writes.
fn index_bytes(ids: &BTreeSet<String>) -> Vec<u8> {
    let list = ids
        .iter()
        .map(|id| Value::String(id.clone()).to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"v\":1,\"archived\":[{list}]}}").into_bytes()
}

// ------------------------------------------------------------------ atomic I/O

/// Which mode a replaced file gets, for RightKit's `atomic_write_with_options` (a temp file in the
/// same folder, fsynced, renamed over the target, never written through a symlink): the mode
/// `like` has (0600 when `like` is absent or missing). `Privacy::Inherit` creates the temp with
/// the existing file's exact mode on Unix, and a new file is owner-only (Windows keeps the
/// parent's inherited access in both cases).
fn replace_privacy(like: Option<&Path>) -> rightkit_fs::Privacy {
    if like.is_some_and(Path::exists) {
        rightkit_fs::Privacy::Inherit
    } else {
        rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows
    }
}

fn remove_file(target: &Path) -> Result<(), SyncError> {
    match fs::remove_file(target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(&target.display().to_string(), e)),
    }
    if let Some(dir) = target.parent() {
        sync_dir(dir);
    }
    Ok(())
}

// -------------------------------------------------------------------- backups

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ManifestOp {
    folder: String,
    dir: String,
    name: String,
    kind: String,
    before: Option<Sig>,
    after: Option<Sig>,
    /// Path of the saved copy relative to the backup folder, when one exists.
    saved: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    ts: String,
    created_ms: u64,
    root: String,
    /// pending | applied | failed | restored
    status: String,
    ops: Vec<ManifestOp>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackupInfo {
    pub ts: String,
    pub created_ms: u64,
    pub status: String,
    pub files: usize,
}

fn valid_ts(ts: &str) -> bool {
    let b = ts.as_bytes();
    b.len() == 19
        && b[8] == b'-'
        && b[15] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 8 || i == 15 || c.is_ascii_digit())
}

/// `YYYYMMDD-HHMMSS-mmm` in UTC.
fn timestamp(now_ms: u64) -> String {
    let secs = (now_ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}-{:03}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        now_ms % 1000
    )
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_manifest(dir: &Path) -> Result<Manifest, SyncError> {
    let bytes = read_all(&dir.join("manifest.json"))?;
    serde_json::from_slice(&bytes).map_err(|e| io("manifest.json", e))
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), SyncError> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|e| io("manifest", e))?;
    let target = dir.join("manifest.json");
    rightkit_fs::atomic_write_with_options(
        &target,
        rightkit_fs::AtomicWriteOptions {
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| file.write_all(&bytes),
    )
    .map_err(|e| io(&target.display().to_string(), std::io::Error::from(e)))
}

/// Backups, newest first.
pub fn backups(backups_dir: &Path) -> Vec<BackupInfo> {
    let Ok(entries) = fs::read_dir(backups_dir) else {
        return Vec::new();
    };
    let mut list: Vec<BackupInfo> = entries
        .flatten()
        .filter_map(|e| {
            let ts = e.file_name().to_string_lossy().into_owned();
            if !valid_ts(&ts) {
                return None;
            }
            let manifest = read_manifest(&e.path()).ok()?;
            Some(BackupInfo {
                ts,
                created_ms: manifest.created_ms,
                status: manifest.status,
                files: manifest.ops.len(),
            })
        })
        .collect();
    list.sort_by(|a, b| b.ts.cmp(&a.ts));
    list
}

/// Total size the backups may take; past it the oldest go first (never the newest).
pub const BACKUPS_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

fn prune(backups_dir: &Path) {
    let list = backups(backups_dir);
    for old in list.iter().skip(KEEP_BACKUPS) {
        let _ = fs::remove_dir_all(backups_dir.join(&old.ts));
    }
    // Newest first: keep walking until the cap is reached, then drop every older one.
    let mut total = 0u64;
    for (i, b) in list.iter().take(KEEP_BACKUPS).enumerate() {
        total = total.saturating_add(dir_bytes(&backups_dir.join(&b.ts)));
        if i > 0 && total > BACKUPS_MAX_BYTES {
            let _ = fs::remove_dir_all(backups_dir.join(&b.ts));
        }
    }
}

// ----------------------------------------------------------------------- apply

#[derive(Clone, Debug, Serialize)]
pub struct ApplyResult {
    /// The backup this apply made; `None` when there was nothing to change.
    pub backup: Option<String>,
    pub files_changed: usize,
    pub totals: Totals,
    pub conflicts: Vec<Conflict>,
    pub folders: Vec<FolderPlan>,
    /// What the cross-folder phase of [`apply_scoped`] did, per data folder; its files are in
    /// `files_changed` and the same backup.
    #[serde(skip)]
    pub across: Vec<CrossResult>,
}

/// One data folder that the cross-folder phase of [`apply_scoped`] brings up to `sources`.
#[derive(Clone, Debug)]
pub struct CrossTarget {
    /// The switcher's slot name ("main" or an account folder's name): one path segment.
    pub label: String,
    pub dir: PathBuf,
    pub sources: Vec<PathBuf>,
}

/// What the cross-folder phase did for one [`CrossTarget`].
#[derive(Clone, Debug, Serialize)]
pub struct CrossResult {
    pub slot: String,
    pub folders: usize,
    pub copied: usize,
    pub skipped: usize,
}

/// Apply the plan. Refuses while Claude runs (`running` is injectable for
/// tests; the CLI passes `claude_running`).
pub fn apply(
    root: &Path,
    backups_dir: &Path,
    running: &dyn Fn() -> bool,
    now: u64,
) -> Result<ApplyResult, SyncError> {
    apply_for(root, backups_dir, running, now, None)
}

/// [`apply`] limited to the accounts in `only`.
pub fn apply_for(
    root: &Path,
    backups_dir: &Path,
    running: &dyn Fn() -> bool,
    now: u64,
    only: Option<&BTreeSet<String>>,
) -> Result<ApplyResult, SyncError> {
    apply_scoped(root, backups_dir, running, now, only, &[], &BTreeSet::new())
}

/// [`apply_for`], then each data folder in `across` brings in the chats of its sources (Claude's
/// folder and the switcher's account folders, #427), as one operation: one backup whose manifest
/// holds every file either phase changes, so `restore <ts>` reverses both and a failure in either
/// rolls both back. Accounts in `excluded` are neither read nor written by the cross-folder phase,
/// as `only` leaves them out of the first. The caller decides which folders are in scope: nothing
/// outside `root` and `across` is touched.
pub fn apply_scoped(
    root: &Path,
    backups_dir: &Path,
    running: &dyn Fn() -> bool,
    now: u64,
    only: Option<&BTreeSet<String>>,
    across: &[CrossTarget],
    excluded: &BTreeSet<String>,
) -> Result<ApplyResult, SyncError> {
    if running() {
        return Err(SyncError::Running);
    }
    let plan = plan_for(root, only)?;
    if !plan.blockers.is_empty() {
        return Err(SyncError::Blocked(plan.blockers));
    }
    let summary =
        |backup: Option<String>, files: usize, plan: Plan, across: Vec<CrossResult>| ApplyResult {
            backup,
            files_changed: files,
            totals: plan.totals,
            conflicts: plan.conflicts,
            folders: plan.folders,
            across,
        };
    if plan.ops.is_empty() && across.is_empty() {
        return Ok(summary(None, 0, plan, Vec::new()));
    }

    // 1. Validate every target and copy source before anything is touched.
    let mut payloads: Vec<Option<Vec<u8>>> = Vec::with_capacity(plan.ops.len());
    for op in &plan.ops {
        validate(op)?;
        payloads.push(match &op.content {
            None => None,
            Some(Content::Inline(bytes)) => Some(bytes.clone()),
            Some(Content::Copy { path, sig }) => {
                let bytes = read_all(path)?;
                if bytes.len() as u64 != sig.size || hash_bytes(&bytes) != sig.hash {
                    return Err(SyncError::Changed(path.display().to_string()));
                }
                Some(bytes)
            }
        });
    }

    // 2. Backup every file that will change, and record the manifest first.
    let ts = timestamp(now);
    let dir = backups_dir.join(&ts);
    fs::create_dir_all(backups_dir).map_err(|e| io("backups folder", e))?;
    fs::create_dir(&dir).map_err(|e| io("backup folder", e))?;
    let _ = ensure_private_dir(&dir);
    let mut manifest = Manifest {
        version: 1,
        ts: ts.clone(),
        created_ms: now,
        root: plan.root.clone(),
        status: "pending".into(),
        ops: Vec::new(),
    };
    for (op, payload) in plan.ops.iter().zip(&payloads) {
        let mut saved = None;
        if op.expect.is_some() {
            let rel = op.rel();
            let copy = dir.join("files").join(&rel);
            fs::create_dir_all(copy.parent().expect("copy has a parent"))
                .map_err(|e| io("backup folder", e))?;
            let bytes = read_all(&op.target())?;
            rightkit_fs::atomic_write_with_options(
                &copy,
                rightkit_fs::AtomicWriteOptions {
                    privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                    ..Default::default()
                },
                |file| file.write_all(&bytes),
            )
            .map_err(|e| io(&copy.display().to_string(), std::io::Error::from(e)))?;
            saved = Some(format!("files/{rel}"));
        }
        manifest.ops.push(ManifestOp {
            folder: op.folder.clone(),
            dir: op.dir.display().to_string(),
            name: op.name.clone(),
            kind: match op.kind {
                Kind::Add => "add",
                Kind::Update => "update",
                Kind::Remove => "remove",
            }
            .into(),
            before: op.expect,
            after: payload.as_ref().map(|b| Sig {
                size: b.len() as u64,
                mtime_ns: 0,
                hash: hash_bytes(b),
            }),
            saved,
        });
    }
    write_manifest(&dir, &manifest)?;

    // 3. Write, file by file, atomically; roll back what was done on failure.
    if running() {
        manifest.status = "failed".into();
        let _ = write_manifest(&dir, &manifest);
        return Err(SyncError::Running);
    }
    let journal = dir.join("journal.log");
    let mut done = 0usize;
    let outcome: Result<(), SyncError> = (|| {
        for (op, payload) in plan.ops.iter().zip(&payloads) {
            validate(op)?;
            match payload {
                Some(bytes) => rightkit_fs::atomic_write_with_options(
                    &op.target(),
                    rightkit_fs::AtomicWriteOptions {
                        privacy: replace_privacy(Some(&op.target())),
                        ..Default::default()
                    },
                    |file| file.write_all(bytes),
                )
                .map_err(|e| io(&op.target().display().to_string(), std::io::Error::from(e)))?,
                None => remove_file(&op.target())?,
            }
            done += 1;
            let line = format!("done {done} {}\n", op.rel());
            let _ = append_record(&journal, line.as_bytes(), AppendOptions::private());
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        let applied = manifest.ops[..done].to_vec();
        return Err(roll_back(&dir, &mut manifest, &applied, error));
    }

    // 4. The other data folders in scope, recorded in the same manifest before each is written.
    let mut crossed = Vec::new();
    let outcome: Result<(), SyncError> = (|| {
        for target in across {
            if target.label.is_empty()
                || target.label.starts_with('.')
                || target.label.contains(['/', '\\'])
            {
                return Err(SyncError::Io(format!(
                    "{} is not a folder name",
                    target.label
                )));
            }
            let mut recorder = Recorder {
                dir: &dir,
                manifest: &mut manifest,
                label: &target.label,
                start: 0,
            };
            let merged =
                merge_folders(&target.dir, &target.sources, excluded, Some(&mut recorder))?;
            crossed.push(CrossResult {
                slot: target.label.clone(),
                folders: merged.folders,
                copied: merged.copied,
                skipped: merged.skipped,
            });
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        let applied = manifest.ops.clone();
        return Err(roll_back(&dir, &mut manifest, &applied, error));
    }
    if manifest.ops.is_empty() {
        // Nothing changed in any phase: no backup to keep.
        let _ = fs::remove_dir_all(&dir);
        return Ok(summary(None, 0, plan, crossed));
    }
    manifest.status = "applied".into();
    write_manifest(&dir, &manifest)?;
    prune(backups_dir);
    let files = manifest.ops.len();
    Ok(summary(Some(ts), files, plan, crossed))
}

/// Undo `ops` (what an operation wrote before `error`) from the backup and record the outcome.
fn roll_back(
    dir: &Path,
    manifest: &mut Manifest,
    ops: &[ManifestOp],
    error: SyncError,
) -> SyncError {
    let rolled = restore_ops(dir, ops, true).is_ok();
    manifest.status = if rolled { "restored" } else { "failed" }.into();
    let _ = write_manifest(dir, manifest);
    SyncError::Io(format!(
        "{error}; {}",
        if rolled {
            "the files written so far were rolled back from the backup"
        } else {
            "rollback was incomplete: restore this backup before reopening Claude"
        }
    ))
}

/// Records the cross-folder phase's writes in the operation's manifest. Before a destination
/// folder is written, every file the merge may touch there (each source session's record and
/// deletion marker, and the archive index) is entered with what it holds now, a saved copy when
/// it exists and no "after" yet, and the manifest is written: a crash mid-folder leaves a backup
/// that `restore --force` reverses, a created file included. Once the folder is done, each entry
/// gets what the file holds now, and the ones left unchanged are dropped with their copies.
struct Recorder<'a> {
    dir: &'a Path,
    manifest: &'a mut Manifest,
    label: &'a str,
    /// Where the current folder's entries start in `manifest.ops`.
    start: usize,
}

impl Recorder<'_> {
    fn before(&mut self, dest: &Folder, sources: &[Folder]) -> Result<(), SyncError> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        names.insert(INDEX_FILE.to_string());
        for source in sources {
            for id in source.recs.keys().chain(source.tombs.keys()) {
                names.insert(format!("local_{id}.json"));
                names.insert(format!("deleted_{id}"));
            }
        }
        self.start = self.manifest.ops.len();
        let folder = format!("{}:{}", self.label, dest.key);
        for name in names {
            let target = dest.path.join(&name);
            // Read once, and the signature taken from the bytes saved: Claude may be writing
            // this folder (a sync never closes it).
            let found = match fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_file() => Some((read_all(&target)?, meta)),
                Ok(_) => {
                    return Err(SyncError::Io(format!(
                        "{} is not a regular file",
                        target.display()
                    )));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(io(&target.display().to_string(), e)),
            };
            let before = found.as_ref().map(|(bytes, meta)| Sig {
                size: bytes.len() as u64,
                mtime_ns: mtime_ns(meta),
                hash: hash_bytes(bytes),
            });
            let mut saved = None;
            if let Some((bytes, _)) = &found {
                let rel = format!("files/across/{}/{}/{name}", self.label, dest.key);
                let copy = self.dir.join(&rel);
                fs::create_dir_all(copy.parent().expect("copy has a parent"))
                    .map_err(|e| io("backup folder", e))?;
                rightkit_fs::atomic_write_with_options(
                    &copy,
                    rightkit_fs::AtomicWriteOptions {
                        privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                        ..Default::default()
                    },
                    |file| file.write_all(bytes),
                )
                .map_err(|e| io(&copy.display().to_string(), std::io::Error::from(e)))?;
                saved = Some(rel);
            }
            self.manifest.ops.push(ManifestOp {
                folder: folder.clone(),
                dir: dest.path.display().to_string(),
                name,
                kind: "pending".into(),
                before,
                after: None,
                saved,
            });
        }
        write_manifest(self.dir, &*self.manifest)
    }

    fn after(&mut self) -> Result<(), SyncError> {
        let mut kept = Vec::new();
        for op in &self.manifest.ops[self.start..] {
            let current = sig_of(&Path::new(&op.dir).join(&op.name))?;
            if same(&current, &op.before) {
                continue;
            }
            let mut op = op.clone();
            op.kind = match (&op.before, &current) {
                (None, _) => "add",
                (Some(_), Some(_)) => "update",
                (Some(_), None) => "remove",
            }
            .into();
            op.after = current;
            kept.push(op);
        }
        let dropped: Vec<String> = self.manifest.ops[self.start..]
            .iter()
            .filter(|op| !kept.iter().any(|k| k.name == op.name))
            .filter_map(|op| op.saved.clone())
            .collect();
        self.manifest.ops.truncate(self.start);
        self.manifest.ops.extend(kept);
        write_manifest(self.dir, &*self.manifest)?;
        for rel in dropped {
            let _ = fs::remove_file(self.dir.join(rel));
        }
        Ok(())
    }
}

/// The target must still be exactly what the plan read.
fn validate(op: &Op) -> Result<(), SyncError> {
    let current = sig_of(&op.target())?;
    if current != op.expect {
        return Err(SyncError::Changed(op.rel()));
    }
    Ok(())
}

// --------------------------------------------------------------------- restore

#[derive(Clone, Debug, Serialize)]
pub struct RestoreResult {
    pub backup: String,
    pub restored: usize,
    pub already: usize,
}

/// Put a backup's files back. Refuses while Claude runs, and refuses when a
/// file changed since the sync (unless `force`).
pub fn restore(
    backups_dir: &Path,
    ts: &str,
    force: bool,
    running: &dyn Fn() -> bool,
) -> Result<RestoreResult, SyncError> {
    if !valid_ts(ts) {
        return Err(SyncError::NoBackup(ts.to_string()));
    }
    if running() {
        return Err(SyncError::Running);
    }
    let dir = backups_dir.join(ts);
    let mut manifest = read_manifest(&dir).map_err(|_| SyncError::NoBackup(ts.to_string()))?;
    let (restored, already) = restore_ops(&dir, &manifest.ops, force)?;
    manifest.status = "restored".into();
    write_manifest(&dir, &manifest)?;
    Ok(RestoreResult {
        backup: ts.to_string(),
        restored,
        already,
    })
}

fn same(current: &Option<Sig>, wanted: &Option<Sig>) -> bool {
    match (current, wanted) {
        (None, None) => true,
        (Some(a), Some(b)) => a.same_content(b),
        _ => false,
    }
}

/// Idempotent: a file already equal to its "before" is skipped, one equal to
/// its "after" is restored, anything else is a mismatch. Undone newest first:
/// a file that two phases of one operation changed goes back through each
/// change in turn, ending at what it was before the first.
fn restore_ops(dir: &Path, ops: &[ManifestOp], force: bool) -> Result<(usize, usize), SyncError> {
    let mut plan: Vec<(&ManifestOp, PathBuf)> = Vec::new();
    let mut mismatched = Vec::new();
    let mut already = 0;
    // What each file holds once the later ops are undone.
    let mut state: BTreeMap<PathBuf, Option<Sig>> = BTreeMap::new();
    for op in ops.iter().rev() {
        let target = Path::new(&op.dir).join(&op.name);
        let current = match state.get(&target) {
            Some(sig) => *sig,
            None => sig_of(&target)?,
        };
        if same(&current, &op.before) {
            already += 1;
            state.insert(target, current);
        } else if same(&current, &op.after) || force {
            state.insert(target.clone(), op.before);
            plan.push((op, target));
        } else {
            mismatched.push(format!("{}/{}", op.folder, op.name));
            state.insert(target, current);
        }
    }
    if !mismatched.is_empty() {
        return Err(SyncError::RestoreMismatch(mismatched));
    }
    let restored = plan.len();
    for (op, target) in plan {
        match &op.saved {
            Some(saved) => {
                let bytes = read_all(&dir.join(saved))?;
                rightkit_fs::atomic_write_with_options(
                    &target,
                    rightkit_fs::AtomicWriteOptions {
                        privacy: replace_privacy(Some(&target)),
                        ..Default::default()
                    },
                    |file| file.write_all(&bytes),
                )
                .map_err(|e| io(&target.display().to_string(), std::io::Error::from(e)))?;
            }
            None => remove_file(&target)?,
        }
    }
    Ok((restored, already))
}

// -------------------------------------------------------------------- registry

/// An account the host app has seen, and whether it takes part in the sync.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnownAccount {
    pub id: String,
    pub first_seen_ms: u64,
    /// Claude keeps no name or email in its non-secret files, so this stays
    /// empty unless one is set; display falls back to the short id.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "yes")]
    pub included: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LastSync {
    pub at_ms: u64,
    pub backup: Option<String>,
    pub files_changed: usize,
    pub conflicts: usize,
    pub totals: Totals,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub accounts: Vec<KnownAccount>,
    #[serde(default)]
    pub last_sync: Option<LastSync>,
}

impl Registry {
    /// Every account found on disk takes part in the sync unless the registry
    /// explicitly excludes it (`host claude exclude <id>`). Not being in the
    /// registry never keeps an account out.
    pub fn sync_set(&self, found: &[AccountInfo]) -> BTreeSet<String> {
        let excluded: BTreeSet<&str> = self
            .accounts
            .iter()
            .filter(|a| !a.included)
            .map(|a| a.id.as_str())
            .collect();
        found
            .iter()
            .filter(|a| !excluded.contains(a.id.as_str()))
            .map(|a| a.id.clone())
            .collect()
    }

    /// The registered accounts that are not excluded.
    pub fn included(&self) -> BTreeSet<String> {
        self.accounts
            .iter()
            .filter(|a| a.included)
            .map(|a| a.id.clone())
            .collect()
    }

    /// The accounts explicitly excluded (`host claude exclude <id>`): never read or written by
    /// any phase of a sync.
    pub fn excluded(&self) -> BTreeSet<String> {
        self.accounts
            .iter()
            .filter(|a| !a.included)
            .map(|a| a.id.clone())
            .collect()
    }
}

/// `~/Library/Application Support/the host app/claude-accounts.json`.
pub fn registry_path() -> Result<PathBuf, SyncError> {
    default_backups()?
        .parent()
        .map(|p| p.join("claude-accounts.json"))
        .ok_or_else(|| SyncError::Unsupported("no known location".into()))
}

/// The saved registry for display; empty when there is none or it cannot be read. Anything that
/// writes reads it with [`read_registry`].
pub fn load_registry(path: &Path) -> Registry {
    read_all(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The saved registry, empty only when there is none. One that cannot be read or parsed is an
/// error: read as empty, its exclusions would vanish and an excluded account would be written (or
/// a fresh registry saved over it).
pub fn read_registry(path: &Path) -> Result<Registry, SyncError> {
    let mut file = match open_read(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
        Err(e) => return Err(io(&path.display().to_string(), e)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| io(&path.display().to_string(), e))?;
    serde_json::from_slice(&bytes).map_err(|e| {
        SyncError::Io(format!(
            "{} cannot be read ({e}), so the excluded accounts are unknown; nothing was written",
            path.display()
        ))
    })
}

pub fn save_registry(path: &Path, registry: &Registry) -> Result<(), SyncError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| io("The host app folder", e))?;
    }
    let bytes = serde_json::to_vec_pretty(registry).map_err(|e| io("registry", e))?;
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            privacy: replace_privacy(Some(path)),
            ..Default::default()
        },
        |file| file.write_all(&bytes),
    )
    .map_err(|e| io(&path.display().to_string(), std::io::Error::from(e)))
}

#[derive(Clone, Debug, Serialize)]
pub struct Discovery {
    /// Accounts seen for the first time just now (empty on the very first scan,
    /// which only records what already exists).
    pub new_accounts: Vec<String>,
    pub registry: Registry,
}

/// Look for account folders under `claude-code-sessions/` and
/// `local-agent-mode-sessions/`, add unknown ones (included by default) and
/// save the registry.
pub fn discover(root: &Path, registry_file: &Path, now: u64) -> Result<Discovery, SyncError> {
    let first_scan = !registry_file.exists();
    let mut registry = read_registry(registry_file)?;
    let mut new_accounts = Vec::new();
    for info in accounts(root)? {
        if registry.accounts.iter().any(|a| a.id == info.id) {
            continue;
        }
        registry.accounts.push(KnownAccount {
            id: info.id.clone(),
            first_seen_ms: now,
            name: None,
            included: true,
        });
        if !first_scan {
            new_accounts.push(info.id);
        }
    }
    registry.version = 1;
    if first_scan || !new_accounts.is_empty() {
        save_registry(registry_file, &registry)?;
    }
    Ok(Discovery {
        new_accounts,
        registry,
    })
}

pub fn set_included(registry_file: &Path, id: &str, included: bool) -> Result<Registry, SyncError> {
    let mut registry = read_registry(registry_file)?;
    match registry.accounts.iter_mut().find(|a| a.id == id) {
        Some(a) => a.included = included,
        None => return Err(SyncError::Io(format!("unknown account {id}"))),
    }
    save_registry(registry_file, &registry)?;
    Ok(registry)
}

#[derive(Clone, Debug, Serialize)]
pub struct AutoResult {
    pub new_accounts: Vec<String>,
    pub claude_running: bool,
    /// Set when a sync ran and changed files (a backup was made).
    pub synced: Option<ApplyResult>,
    /// A sync was due but refused (blocked, changed underfoot, ...).
    pub error: Option<String>,
}

/// What the notch runs when Claude quits, when a new account appears and now
/// and then: discover accounts, and when Claude is closed, sync the included
/// ones. While Claude runs it only discovers; it never reads session files.
pub fn auto_sync(
    root: &Path,
    backups_dir: &Path,
    registry_file: &Path,
    running: &dyn Fn() -> bool,
    now: u64,
) -> Result<AutoResult, SyncError> {
    let found = discover(root, registry_file, now)?;
    let mut result = AutoResult {
        new_accounts: found.new_accounts,
        claude_running: running(),
        synced: None,
        error: None,
    };
    if result.claude_running {
        return Ok(result);
    }
    let only = found.registry.sync_set(&accounts(root)?);
    match apply_for(root, backups_dir, running, now, Some(&only)) {
        Ok(applied) => {
            if applied.files_changed > 0 {
                let mut registry = read_registry(registry_file)?;
                registry.last_sync = Some(LastSync {
                    at_ms: now,
                    backup: applied.backup.clone(),
                    files_changed: applied.files_changed,
                    conflicts: applied.conflicts.len(),
                    totals: applied.totals.clone(),
                });
                save_registry(registry_file, &registry)?;
                result.synced = Some(applied);
            }
        }
        Err(SyncError::Running) => result.claude_running = true,
        Err(error) => result.error = Some(error.to_string()),
    }
    Ok(result)
}

// ---------------------------------------------------------------------- mirror

/// What one [`mirror`] pass did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MirrorResult {
    /// The signed-in account the pass copied from.
    pub active: Option<String>,
    /// Records copied into, or removed from, other accounts' folders.
    pub copied: usize,
    /// Files left alone because they could not be read or written, changed
    /// underfoot, tie with different content, have an unknown format, or are
    /// an archive index written in the last 5 s.
    pub skipped: usize,
    /// Destination folders that were written to.
    pub folders: usize,
    /// Why the pass stopped early, when it did.
    pub aborted: Option<String>,
    /// One line per conflict decision: id, source/destination progress and
    /// mtime, and what was chosen.
    pub decisions: Vec<String>,
}

/// The mirror's memory of the files it has read, beside the registry. A run
/// that finds a file's size and modification time as recorded uses the
/// recorded signature and fields instead of reading the file again.
const MIRROR_CACHE_FILE: &str = "claude-mirror-cache.json";
const MIRROR_CACHE_VERSION: u32 = 1;
/// A file modified this recently is not remembered: a second write inside the
/// clock's granularity could leave the same size and time.
const RACY_NS: u64 = 2_000_000_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime_ns: u64,
    hash: u64,
    ts: i64,
    rank: (i64, i64, i64),
    archived: bool,
    readable: bool,
    /// A deletion marker's timestamp; absent for records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ms: Option<i64>,
}

impl CacheEntry {
    fn sig(&self) -> Sig {
        Sig {
            size: self.size,
            mtime_ns: self.mtime_ns,
            hash: self.hash,
        }
    }

    fn rec(&self) -> Rec {
        Rec {
            sig: self.sig(),
            ts: self.ts,
            rank: self.rank,
            archived: self.archived,
            readable: self.readable,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    v: u32,
    files: BTreeMap<String, CacheEntry>,
}

struct MirrorCache {
    old: BTreeMap<String, CacheEntry>,
    seen: BTreeMap<String, CacheEntry>,
    now_ns: u64,
}

impl MirrorCache {
    /// The saved cache; empty when there is none, it cannot be read or it is
    /// another version.
    fn load(path: &Path, now_ns: u64) -> Self {
        let old = read_all(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
            .filter(|f| f.v == MIRROR_CACHE_VERSION)
            .map(|f| f.files)
            .unwrap_or_default();
        MirrorCache {
            old,
            seen: BTreeMap::new(),
            now_ns,
        }
    }

    /// Keep what this pass saw (files that are gone drop out); written only
    /// when it differs from what was loaded, and best effort: a cache that
    /// cannot be written is rebuilt next time.
    fn save(self, path: &Path) {
        if self.seen == self.old {
            return;
        }
        let file = CacheFile {
            v: MIRROR_CACHE_VERSION,
            files: self.seen,
        };
        let Ok(bytes) = serde_json::to_vec(&file) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = rightkit_fs::atomic_write_with_options(
            path,
            rightkit_fs::AtomicWriteOptions {
                durable: false,
                privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                ..Default::default()
            },
            |file| file.write_all(&bytes),
        );
    }
}

/// A record's or deletion marker's signature and fields. A file whose size and
/// modification time match the cache costs one `lstat`; any other is read once
/// (hash and fields from the same bytes). `Ok(None)` when it does not exist.
fn cached_entry(
    cache: &mut MirrorCache,
    file: &Path,
    is_record: bool,
) -> Result<Option<CacheEntry>, SyncError> {
    let meta = match fs::symlink_metadata(file) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(&file.display().to_string(), e)),
    };
    if !meta.file_type().is_file() {
        return Err(SyncError::Io(format!(
            "{} is not a regular file",
            file.display()
        )));
    }
    let key = file.to_string_lossy().into_owned();
    let (size, mtime) = (meta.len(), mtime_ns(&meta));
    if let Some(hit) = cache
        .old
        .get(&key)
        .filter(|e| e.size == size && e.mtime_ns == mtime)
    {
        let entry = hit.clone();
        cache.seen.insert(key, entry.clone());
        return Ok(Some(entry));
    }
    let bytes = read_all(file)?;
    let sig = Sig {
        size: bytes.len() as u64,
        mtime_ns: mtime,
        hash: hash_bytes(&bytes),
    };
    let entry = if is_record {
        let rec = parse_record(sig, &bytes);
        CacheEntry {
            size: sig.size,
            mtime_ns: sig.mtime_ns,
            hash: sig.hash,
            ts: rec.ts,
            rank: rec.rank,
            archived: rec.archived,
            readable: rec.readable,
            ms: None,
        }
    } else {
        CacheEntry {
            size: sig.size,
            mtime_ns: sig.mtime_ns,
            hash: sig.hash,
            ts: 0,
            rank: (0, 0, 0),
            archived: false,
            readable: true,
            ms: String::from_utf8(bytes)
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok()),
        }
    };
    if mtime.saturating_add(RACY_NS) < cache.now_ns {
        cache.seen.insert(key, entry.clone());
    }
    Ok(Some(entry))
}

/// `config.json`'s size and modification time.
fn config_stamp(root: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(root.join("config.json")).ok()?;
    Some((meta.len(), mtime_ns(&meta)))
}

/// Whether `active` is still the signed-in account. `config.json` holds
/// sign-in material and is large to parse, so an unchanged, not recently
/// written one (`before`, taken ahead of the first read) is taken as unchanged.
fn still_active(root: &Path, active: &str, before: Option<(u64, u64)>, now_ns: u64) -> bool {
    let settled = before.is_some_and(|(_, mtime)| mtime.saturating_add(RACY_NS) < now_ns);
    if settled && config_stamp(root) == before {
        return true;
    }
    read_active_account(root).as_deref() == Some(active)
}

fn account_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    sorted_dirs(&root.join(SESSIONS_DIR), &mut Vec::new())
}

/// One-way copy from the signed-in account's folders into every other included
/// account's existing org folders. Safe while Claude runs: it never writes the
/// signed-in account's folder, which is the only one Claude writes. Accounts
/// and org folders are never created, and nothing is backed up per pass; the
/// full two-way merge is `host claude sync --apply`, which never closes Claude. Included means what
/// [`Registry::sync_set`] means: on disk and not explicitly excluded.
pub fn mirror(root: &Path, registry_file: &Path) -> Result<MirrorResult, SyncError> {
    let now_ns = now_ms().saturating_mul(1_000_000);
    let config_before = config_stamp(root);
    let mut result = MirrorResult {
        active: read_active_account(root),
        ..MirrorResult::default()
    };
    let Some(active) = result.active.clone() else {
        result.aborted = Some("no signed-in account".into());
        return Ok(result);
    };
    let excluded = read_registry(registry_file)?.excluded();
    if excluded.contains(&active) {
        result.aborted = Some("the signed-in account is excluded".into());
        return Ok(result);
    }
    let accounts = account_dirs(root);
    let mut skipped = 0usize;
    let Some((_, active_path)) = accounts.iter().find(|(id, _)| *id == active) else {
        result.aborted = Some("the signed-in account has no sessions folder".into());
        return Ok(result);
    };
    let cache_file = registry_file.with_file_name(MIRROR_CACHE_FILE);
    let mut cache = MirrorCache::load(&cache_file, now_ns);
    let (sources, bad) = mirror_scan(&active, active_path, &mut cache);
    skipped += bad;
    let mut moved = false;
    'dest: for (account, path) in accounts
        .iter()
        .filter(|(id, _)| *id != active && !excluded.contains(id))
    {
        let (dests, bad) = mirror_scan(account, path, &mut cache);
        skipped += bad;
        for dest in dests {
            if !still_active(root, &active, config_before, now_ns) {
                moved = true;
                break 'dest;
            }
            let (copied, bad, wrote) = mirror_into(&sources, &dest, &mut result.decisions);
            result.copied += copied;
            skipped += bad;
            result.folders += usize::from(wrote);
        }
    }
    if !moved {
        cache.save(&cache_file);
    }
    result.skipped = skipped;
    if moved || read_active_account(root).as_deref() != Some(active.as_str()) {
        result.aborted = Some("the signed-in account changed during the pass".into());
    }
    Ok(result)
}

/// An account's org folders, read leniently (Claude may be writing them), and
/// how many files could not be read.
fn mirror_scan(account: &str, path: &Path, cache: &mut MirrorCache) -> (Vec<Folder>, usize) {
    let mut blockers = Vec::new();
    let mut loose = Some(0usize);
    let mut skipped = 0usize;
    let mut out = Vec::new();
    for (org, org_path) in sorted_dirs(path, &mut blockers) {
        match load_folder_inner(
            account,
            &org,
            &org_path,
            &mut blockers,
            &mut loose,
            Some(&mut *cache),
        ) {
            Ok(f) => out.push(f),
            Err(_) => skipped += 1,
        }
    }
    (out, skipped + blockers.len() + loose.unwrap_or(0))
}

/// Bring one destination folder up to the sources. Returns (records copied or
/// removed, files skipped, whether anything was written).
fn mirror_into(
    sources: &[Folder],
    dest: &Folder,
    decisions: &mut Vec<String>,
) -> (usize, usize, bool) {
    let (mut copied, mut skipped) = (0usize, 0usize);
    let mut ids: BTreeSet<&String> = BTreeSet::new();
    for s in sources {
        ids.extend(s.recs.keys().chain(s.tombs.keys()));
    }
    let mut archived: BTreeMap<String, bool> = dest
        .recs
        .iter()
        .map(|(id, r)| (id.clone(), r.archived))
        .collect();
    let mut changed = false;
    for id in ids {
        // The source's word on this session: its furthest-along live record,
        // or a deletion at least as new as that record.
        let live = sources
            .iter()
            .filter_map(|s| s.recs.get(id).map(|r| (s, r)))
            .filter(|(_, r)| r.readable)
            .max_by_key(|(_, r)| r.rank);
        let unreadable = sources.iter().any(|s| {
            s.recs.get(id).is_some_and(|r| !r.readable)
                || s.tombs.get(id).is_some_and(|t| t.ms.is_none())
        });
        let deleted = sources
            .iter()
            .filter_map(|s| s.tombs.get(id).and_then(|t| t.ms))
            .max()
            .filter(|d| live.is_none_or(|(_, r)| *d >= r.ts));
        if unreadable && live.is_none() && deleted.is_none() {
            skipped += 1;
            continue;
        }
        let name = format!("local_{id}.json");
        let target = dest.path.join(&name);
        let tomb_target = dest.path.join(format!("deleted_{id}"));
        if let Some(d) = deleted {
            let Some(rec) = dest.recs.get(id) else {
                continue;
            };
            if !rec.readable {
                skipped += 1;
                continue;
            }
            if d < rec.ts {
                continue;
            }
            // Marker first: a crash leaves marker and record, and the marker
            // is the newer word, so the next pass or merge finishes the job.
            let older = dest
                .tombs
                .get(id)
                .is_none_or(|t| t.ms.is_some_and(|m| m < d));
            let still = sig_of(&target)
                .ok()
                .flatten()
                .is_some_and(|s| s.same_content(&rec.sig));
            if !still {
                skipped += 1;
                continue;
            }
            let unmarked = older
                && rightkit_fs::atomic_write_with_options(
                    &tomb_target,
                    rightkit_fs::AtomicWriteOptions {
                        privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
                        ..Default::default()
                    },
                    |file| file.write_all(d.to_string().as_bytes()),
                )
                .is_err();
            if unmarked || remove_file(&target).is_err() {
                skipped += 1;
                continue;
            }
            archived.remove(id);
            copied += 1;
            changed = true;
            continue;
        }
        let Some((src, rec)) = live else {
            continue;
        };
        let newer_tomb = dest
            .tombs
            .get(id)
            .is_some_and(|t| t.ms.is_none_or(|m| m >= rec.ts));
        let wanted = match dest.recs.get(id) {
            None => !newer_tomb,
            Some(d) if !d.readable => {
                skipped += 1;
                false
            }
            Some(d) if rec.rank > d.rank => true,
            Some(d) if rec.rank == d.rank && !d.sig.same_content(&rec.sig) => {
                skipped += 1;
                false
            }
            Some(_) => false,
        };
        if let Some(d) = dest.recs.get(id).filter(|d| !d.sig.same_content(&rec.sig)) {
            decisions.push(format!(
                "{id}: src {}/{} rank {:?} mtime_ns {} size {}; dst {}/{} rank {:?} mtime_ns {} size {}; chosen {}",
                src.account,
                src.org,
                rec.rank,
                rec.sig.mtime_ns,
                rec.sig.size,
                dest.account,
                dest.org,
                d.rank,
                d.sig.mtime_ns,
                d.sig.size,
                if wanted { "source" } else { "destination kept" }
            ));
        }
        if !wanted {
            continue;
        }
        let from = src.path.join(&name);
        let bytes = match read_all(&from) {
            Ok(b) if b.len() as u64 == rec.sig.size && hash_bytes(&b) == rec.sig.hash => b,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let expect = dest.recs.get(id).map(|d| d.sig);
        let now = sig_of(&target).ok();
        let untouched = match (now, expect) {
            (Some(None), None) => true,
            (Some(Some(n)), Some(e)) => n.same_content(&e),
            _ => false,
        };
        if !untouched
            || rightkit_fs::atomic_write_with_options(
                &target,
                rightkit_fs::AtomicWriteOptions {
                    privacy: replace_privacy(expect.map(|_| target.as_path())),
                    ..Default::default()
                },
                |file| file.write_all(&bytes),
            )
            .is_err()
        {
            skipped += 1;
            continue;
        }
        archived.insert(id.clone(), rec.archived);
        copied += 1;
        changed = true;
    }
    if changed && mirror_index(dest, &archived) == Some(false) {
        skipped += 1;
    }
    (copied, skipped, changed)
}

/// Rebuild the folder's archive index from the records it holds. `None` when
/// nothing needed writing, `Some(false)` when it was left alone (unrecognised
/// format or a failed write).
fn mirror_index(dest: &Folder, archived: &BTreeMap<String, bool>) -> Option<bool> {
    if dest.index.as_ref().is_some_and(|ix| !ix.compatible) {
        return Some(false);
    }
    let mut wanted: BTreeSet<String> = archived
        .iter()
        .filter(|(_, a)| **a)
        .map(|(id, _)| format!("local_{id}"))
        .collect();
    // A record this pass could not read keeps whatever the old index said.
    if let Some(ix) = &dest.index {
        for (id, r) in &dest.recs {
            let key = format!("local_{id}");
            if !r.readable && ix.archived.contains(&key) {
                wanted.insert(key);
            }
        }
    }
    if dest.index.as_ref().is_some_and(|ix| ix.archived == wanted) {
        return None;
    }
    // Desktop rewrites the index of the account it just left a moment after a
    // switch; a fresh index is its write in flight, so leave it this pass.
    let now_ns = now_ms().saturating_mul(1_000_000);
    if dest
        .index
        .as_ref()
        .is_some_and(|ix| ix.sig.mtime_ns.saturating_add(5_000_000_000) > now_ns)
    {
        return Some(false);
    }
    let target = dest.path.join(INDEX_FILE);
    let like = dest.index.as_ref().map(|_| target.as_path());
    Some(
        rightkit_fs::atomic_write_with_options(
            &target,
            rightkit_fs::AtomicWriteOptions {
                privacy: replace_privacy(like),
                ..Default::default()
            },
            |file| file.write_all(&index_bytes(&wanted)),
        )
        .is_ok(),
    )
}

// ------------------------------------------------------- merge between data folders

/// What [`merge_into_root`] did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RootMerge {
    /// Destination org folders of the target's signed-in account.
    pub folders: usize,
    /// Records copied in or removed by a newer deletion marker.
    pub copied: usize,
    /// Files left alone (unreadable, a tie with different content, a symlink).
    pub skipped: usize,
    /// The destination org folders, for backup and repair.
    pub dest_dirs: Vec<PathBuf>,
    /// One line per conflict decision (see [`MirrorResult::decisions`]).
    pub decisions: Vec<String>,
}

/// The org folders `<root>/claude-code-sessions/<account>/<org>` of one data folder, as
/// `(account, org folder)`.
pub fn org_folders(root: &Path) -> Vec<(String, PathBuf)> {
    let mut blockers = Vec::new();
    let mut out = Vec::new();
    for (account, account_path) in sorted_dirs(&root.join(SESSIONS_DIR), &mut blockers) {
        for (_, path) in sorted_dirs(&account_path, &mut blockers) {
            out.push((account.clone(), path));
        }
    }
    out
}

/// Bring the closed data folder `target` up to the chats in `sources` (other data folders):
/// for each session the furthest-along copy wins, deletion markers carry, and the archive
/// index is rebuilt, exactly as the mirror does. Only the org folders of the account that is
/// signed in at `target` are written; nothing is created. An account in `excluded` is neither
/// read nor written: when it is the signed-in one, nothing is. The caller guarantees no Claude is
/// using `target`.
pub fn merge_into_root(
    target: &Path,
    sources: &[PathBuf],
    excluded: &BTreeSet<String>,
) -> Result<RootMerge, SyncError> {
    merge_folders(target, sources, excluded, None)
}

/// [`merge_into_root`], recording each destination folder's writes when `recorder` is given.
fn merge_folders(
    target: &Path,
    sources: &[PathBuf],
    excluded: &BTreeSet<String>,
    mut recorder: Option<&mut Recorder<'_>>,
) -> Result<RootMerge, SyncError> {
    let mut result = RootMerge::default();
    let Some(active) = read_active_account(target) else {
        return Ok(result);
    };
    if excluded.contains(&active) {
        return Ok(result);
    }
    let mut blockers = Vec::new();
    let mut loose = Some(0usize);
    let load =
        |account: &str, path: &Path, blockers: &mut Vec<String>, loose: &mut Option<usize>| {
            let org = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            load_folder_inner(account, &org, path, blockers, loose, None)
        };
    let mut dests = Vec::new();
    let mut srcs = Vec::new();
    for (account, path) in org_folders(target) {
        if excluded.contains(&account) {
            continue;
        }
        let folder = load(&account, &path, &mut blockers, &mut loose)?;
        if account == active {
            dests.push(folder);
        } else {
            srcs.push(folder);
        }
    }
    for root in sources {
        for (account, path) in org_folders(root) {
            if excluded.contains(&account) {
                continue;
            }
            srcs.push(load(&account, &path, &mut blockers, &mut loose)?);
        }
    }
    result.skipped += blockers.len() + loose.unwrap_or(0);
    // Desktop rewrites an archive index moments after it quits; wait that out so the rebuild
    // is not skipped as an in-flight write.
    let now_ns = now_ms().saturating_mul(1_000_000);
    let youngest = dests
        .iter()
        .filter_map(|d| d.index.as_ref().map(|ix| ix.sig.mtime_ns))
        .max();
    if let Some(m) = youngest {
        let ready = m.saturating_add(5_100_000_000);
        if ready > now_ns {
            std::thread::sleep(std::time::Duration::from_nanos(ready - now_ns));
        }
    }
    for dest in &dests {
        if let Some(recorder) = recorder.as_deref_mut() {
            recorder.before(dest, &srcs)?;
        }
        result.folders += 1;
        result.dest_dirs.push(dest.path.clone());
        let (copied, skipped, _) = mirror_into(&srcs, dest, &mut result.decisions);
        result.copied += copied;
        result.skipped += skipped;
        if let Some(recorder) = recorder.as_deref_mut() {
            recorder.after()?;
        }
    }
    Ok(result)
}
