//! Opt-in local metadata history. Target files are never changed by a scan.
//!
//! Every operation on the state directory goes through RightKit's `StateDir`
//! (rightkit-fs 0.3.4): on unix one `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` fd that is
//! `fstat`-verified (directory, owned by the effective user, not writable by group or others)
//! and used as the `*at` base for enumeration, checks (`AT_SYMLINK_NOFOLLOW`), reads
//! (`O_NOFOLLOW`) and publication (`linkat`, then `fsync` of the file and the directory); on
//! Windows a handle opened without following reparse points, with native handle-relative
//! operations. A renamed or swapped parent therefore cannot redirect reads or writes.
use crate::{ScanReport, rules::Finding};
use rightkit_fs::{EntryKind, FsError, StateDir, StateOptions, atomic::TEMP_MARKER};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Current (and only writable) snapshot schema version.
pub const SCHEMA_VERSION: u32 = 1;
/// Largest snapshot file that will be written or read.
pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// Largest number of snapshot candidates `history` will examine.
pub const MAX_SNAPSHOTS: usize = 1000;
/// Largest number of directory entries retained during one history scan.
pub const MAX_ENUMERATED_ENTRIES: usize = MAX_SNAPSHOTS * 2;
/// Longest accepted snapshot ID.
pub const MAX_ID_LEN: usize = 64;
const ID_PREFIX: &str = "scan-";

/// A snapshot file that `history_report` declined to load, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedSnapshot {
    pub file: String,
    pub reason: String,
}

/// Loaded snapshots (ordered by `created_at`, then `id`) plus explicit skips
/// (ordered by file name) and platform capability notes.
#[derive(Clone, Debug, Default)]
pub struct HistoryReport {
    pub snapshots: Vec<Snapshot>,
    pub skipped: Vec<SkippedSnapshot>,
    /// Platform limits that affect the privacy of the store (for example
    /// Windows ACL inheritance). Empty when nothing needs to be disclosed.
    pub capability_notes: Vec<String>,
}

/// Total bytes `history_report` will read across all snapshot files.
pub const MAX_AGGREGATE_READ_BYTES: u64 = 256 * 1024 * 1024;
/// Skip reason for files not read because the aggregate budget ran out.
pub const REASON_AGGREGATE_BUDGET: &str = "aggregate read budget";
/// Skip reason for leftover temp files of an unfinished `save`.
pub const REASON_INTERRUPTED: &str = "interrupted publication";
/// Skip reason for files whose names resolve to the same snapshot ID.
pub const REASON_DUPLICATE_ID: &str = "duplicate id";

/// Capability notes reported on Windows.
#[cfg(windows)]
const PLATFORM_NOTES: &[&str] = &[
    "windows: directories and files this store creates get an owner-only ACL from RightKit; \
     the ACL of an existing directory is never rewritten",
    "windows: state directory and child operations use a pinned handle without following \
     reparse points; an existing directory's ACL is never rewritten",
];
#[cfg(not(windows))]
const PLATFORM_NOTES: &[&str] = &[];

/// An ID is never a path: `scan-` prefix, then ASCII alphanumerics, `-` or `_`,
/// at most `MAX_ID_LEN` bytes in total.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.len() > MAX_ID_LEN {
        return Err(format!("snapshot id longer than {MAX_ID_LEN} bytes"));
    }
    let rest = id
        .strip_prefix(ID_PREFIX)
        .ok_or_else(|| format!("snapshot id lacks `{ID_PREFIX}` prefix"))?;
    if rest.is_empty()
        || !rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("snapshot id has characters outside [A-Za-z0-9_-]".into());
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn denied(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.into())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub id: String,
    pub created_at: u64,
    pub report: ScanReport,
    pub findings: Vec<Finding>,
}
impl Snapshot {
    pub fn new(report: ScanReport, findings: Vec<Finding>) -> Self {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            schema_version: SCHEMA_VERSION,
            id: format!("scan-{}", time.as_nanos()),
            created_at: time.as_secs(),
            report,
            findings,
        }
    }
}

/// How long a successful owner-only check of a folder is trusted. `ensure_private_dir` reads and
/// may rewrite the folder's security descriptor (GetNamedSecurityInfo / SetNamedSecurityInfo on
/// Windows: ntmarta, ntfs and the filter stack), and the chat store called it before every read-
/// modify-write, lock and append, so a 5 s tick kept the engine at ~29% of a core. The first use
/// of a folder is always checked; a later call re-checks when the folder is gone, when the
/// remembered check is older than this, or after `forget_private_dir` (a write that failed with
/// access denied).
const PRIVATE_DIR_TRUST: std::time::Duration = std::time::Duration::from_secs(300);

type DirIdentity = [u64; 3];

/// Identity of the folder a check was made on, so a folder deleted and recreated (possibly with a
/// weaker ACL) inside the trust window is a miss. Unix: (dev, ino, uid). Windows: creation time and
/// file attributes (std's file index is unstable). None when it cannot be read.
fn private_dir_identity(meta: &std::fs::Metadata) -> Option<DirIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some([meta.dev(), meta.ino(), u64::from(meta.uid())])
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let created = meta.created().ok()?;
        let nanos = created
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos() as u64;
        Some([nanos, u64::from(meta.file_attributes()), 0])
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        None
    }
}

type PrivateDirCache =
    std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::Instant, DirIdentity)>>;

fn private_dir_cache() -> &'static PrivateDirCache {
    static CACHE: std::sync::OnceLock<PrivateDirCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// True when `dir` passed the owner-only check recently in this process and still exists as the
/// same folder (one metadata call, no security descriptor read). A link is never trusted from the
/// cache, and a changed or unreadable folder identity is a miss.
pub fn private_dir_verified(dir: &Path) -> bool {
    let Some((at, identity)) = private_dir_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(dir).copied())
    else {
        return false;
    };
    if at.elapsed() >= PRIVATE_DIR_TRUST {
        return false;
    }
    std::fs::symlink_metadata(dir)
        .ok()
        .filter(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        .and_then(|meta| private_dir_identity(&meta))
        .is_some_and(|now| now == identity)
}

/// Remember that `dir` just passed `rightkit_fs::ensure_private_dir`. A folder whose identity
/// cannot be read is not remembered.
pub fn note_private_dir(dir: &Path) {
    let Some(identity) = std::fs::symlink_metadata(dir)
        .ok()
        .and_then(|meta| private_dir_identity(&meta))
    else {
        return;
    };
    if let Ok(mut cache) = private_dir_cache().lock() {
        if cache.len() > 512 {
            cache.clear();
        }
        cache.insert(dir.to_path_buf(), (std::time::Instant::now(), identity));
    }
}

/// Drop the remembered check for `dir` (a write there failed with access denied), so the next
/// call checks and repairs it again.
pub fn forget_private_dir(dir: &Path) {
    if let Ok(mut cache) = private_dir_cache().lock() {
        cache.remove(dir);
    }
}

/// Makes `dir` and its missing parents owner-only (RightKit's `ensure_private_dir`), so a run from an
/// elevated shell on Windows makes them owned by this user, never by Administrators (#391, #416). A
/// folder that already exists but is owned by another principal is still used as it was before,
/// with nothing moved: what the caller keeps there (a waiting message) stays where it is.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    if private_dir_verified(dir) {
        return Ok(());
    }
    match rightkit_fs::ensure_private_dir(dir) {
        Ok(()) => {
            note_private_dir(dir);
            Ok(())
        }
        Err(FsError::NotOwned(_)) if dir.is_dir() => Ok(()),
        Err(other) => Err(other.into()),
    }
}

/// `rightkit_fs::ensure_private_dir`, for a folder that must be usable by this process but may
/// have been created by an elevated one. On Windows a folder made by an elevated process (a
/// shell or agent started "as administrator", the installer, a hook run) is owned by
/// `BUILTIN\Administrators`, and RightKit refuses it for an unelevated caller
/// (`FsError::NotOwned`, rightkit-fs private.rs `restrict_owner_only`), so every later call
/// failed for good. The folder is then moved aside, never entered,
/// read or reused, and a new one is created owner-only by this user in its place; what it held is
/// left under the returned name for the user to look at or delete. The check keeps its purpose:
/// nothing owned by another principal is ever trusted, and the parent is the user's own folder, so
/// only that user, an administrator or the system could have made the folder at all. `Ok(Some(name))`
/// is a repair (callers log it); a folder that cannot be moved aside fails with the original reason
/// and the name of the folder.
pub fn ensure_private_dir_repairing(dir: &Path) -> io::Result<Option<PathBuf>> {
    if private_dir_verified(dir) {
        return Ok(None);
    }
    match rightkit_fs::ensure_private_dir(dir) {
        Ok(()) => {
            note_private_dir(dir);
            Ok(None)
        }
        Err(FsError::NotOwned(_)) if cfg!(windows) => {
            let Some(name) = dir.file_name() else {
                return Err(denied(format!(
                    "{} is not owned by the current user",
                    dir.display()
                )));
            };
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut aside_name = name.to_os_string();
            aside_name.push(format!(".admin-owned-{stamp}"));
            let aside = dir.with_file_name(aside_name);
            std::fs::rename(dir, &aside).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "{} is not owned by the current user (made by an elevated run?) and could not be moved aside: {error}",
                        dir.display()
                    ),
                )
            })?;
            rightkit_fs::ensure_private_dir(dir)?;
            Ok(Some(aside))
        }
        Err(other) => Err(other.into()),
    }
}

/// The `io::Error` this crate has always returned for a RightKit filesystem error, so callers and
/// tests keep matching the same kinds: ownership and permission refusals are
/// `PermissionDenied`, link and type refusals `InvalidInput`, an existing entry
/// `AlreadyExists`, a swapped directory `WouldBlock`, and I/O errors their own error.
fn fs_io(error: FsError) -> io::Error {
    match error {
        FsError::Io { source, .. } => source,
        // RightKit 0.3.4 reports a directory sync that failed after the change took effect as
        // its own variant; the underlying I/O error is what callers have always seen.
        FsError::DirSyncFailed { source, .. }
        | FsError::ReplacedButDirSyncFailed { source, .. }
        | FsError::ReplacedButPostStepFailed { source, .. } => source,
        FsError::AlreadyExists(path) => io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ),
        FsError::NotOwned(_) => denied(
            "metadata directory is not owned by the current user; \
             ownership is never changed by this crate",
        ),
        FsError::TooPermissive(_) => denied(
            "metadata directory is writable by group or others; \
             permissions are never changed by this crate",
        ),
        FsError::IdentityChanged(_) => io::Error::new(
            io::ErrorKind::WouldBlock,
            "metadata directory identity changed during pin",
        ),
        FsError::Symlink(_) => {
            invalid_input("metadata directory contains symlink or reparse point")
        }
        FsError::NotADirectory(_) => invalid_input("metadata path is not a directory"),
        FsError::TooManyEntries(_) => invalid_input("metadata directory entry cap exceeded"),
        FsError::TooLarge { .. } => invalid("state record exceeds size cap"),
        other => invalid_input(other.to_string()),
    }
}

/// Pin the state directory through RightKit (a verified, owner-checked descriptor on unix, a
/// reparse-refusing handle on Windows; every later operation is relative to it, so a swapped
/// path cannot redirect it), creating it when `create` is set. The pin itself refuses a symlink or
/// reparse point in any component and a path that is not a directory (`FsError::Symlink`,
/// `NotADirectory`, mapped by `fs_io`).
fn open_state_dir(directory: &Path, create: bool) -> io::Result<StateDir> {
    // As before the port, the folder handle shares delete access on Windows, so the folder
    // can still be renamed or removed by others while it is open; operations stay relative
    // to the pinned folder.
    let options = StateOptions {
        allow_rename_while_open: true,
        ..StateOptions::default()
    };
    let dir = if create {
        StateDir::create_with(directory, &options)
    } else {
        StateDir::open_with(directory, &options)
    };
    dir.map_err(fs_io)
}

/// Write `snapshot` as `<id>.json` inside the pinned state directory. Never
/// overwrites an existing snapshot or unrelated file (`AlreadyExists`). RightKit's
/// `StateDir::write_new` writes an exclusively created owner-only temporary file
/// (`O_NOFOLLOW`, mode 0o600 on unix), syncs it, then publishes it atomically and without
/// replacement (`linkat` on unix, a no-replace rename relative to the pinned directory
/// handle on Windows) and syncs the directory.
/// Swapping or renaming the directory between pin and publish cannot
/// redirect the write: every operation is relative to the pinned directory.
/// Newly created directories are owner-only; existing
/// directories must already be owned by the effective user and not writable
/// by group or others on unix; this crate refuses rather than changing ownership or
/// permissions of pre-existing paths. Leftover temp files from
/// interrupted writes are never parsed; `history_report` lists them as
/// "interrupted publication".
pub fn save(directory: &Path, snapshot: &Snapshot) -> io::Result<PathBuf> {
    validate_id(&snapshot.id).map_err(|m| io::Error::new(io::ErrorKind::InvalidInput, m))?;
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(invalid_input(
            "refusing to write unsupported snapshot schema version",
        ));
    }
    let bytes = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(invalid_input("snapshot exceeds size cap"));
    }
    let dir = open_state_dir(directory, true)?;
    let dest_name = format!("{}.json", snapshot.id);
    if dir.exists(&dest_name).map_err(fs_io)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "snapshot already exists",
        ));
    }
    dir.write_new(&dest_name, &bytes).map_err(fs_io)?;
    Ok(directory.join(&dest_name))
}

/// How many scan snapshots the CLI keeps after a scan. Each is several MB, one per scan, so
/// they were growing without bound; growth comparison needs only the recent few.
pub const KEEP_SNAPSHOTS: usize = 20;

/// Remove the oldest scan snapshots beyond `keep` (newest by id, which carries its creation
/// time). Only complete `scan-*.json` files with a valid id are touched; temp files and any
/// other name are left alone. Removal is descriptor-relative like every other operation here;
/// a file already gone is skipped. Returns how many were removed. Callers may ignore a failure:
/// a leftover old snapshot is only disk use.
pub fn prune(directory: &Path, keep: usize) -> io::Result<usize> {
    let dir = match open_state_dir(directory, false) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut names: Vec<String> = dir
        .list(MAX_ENUMERATED_ENTRIES)
        .map_err(fs_io)?
        .into_iter()
        .filter(|name| {
            !is_temp_name(name)
                && name.starts_with(ID_PREFIX)
                && name
                    .strip_suffix(".json")
                    .is_some_and(|stem| validate_id(stem).is_ok())
        })
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(keep);
    let mut removed = 0;
    for name in names.iter().take(excess) {
        if dir.remove(name).map_err(fs_io)? {
            removed += 1;
        }
    }
    Ok(removed)
}

#[derive(Deserialize)]
struct Header {
    schema_version: u64,
}

enum Fail {
    Budget,
    Reason(String),
}
impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Self {
        Fail::Reason(e.to_string())
    }
}

/// Read a snapshot file within the size cap and the remaining aggregate budget. RightKit opens
/// it relative to the pinned directory, never through a symlink, and re-checks the size on the
/// open handle, so the object read is the object that was checked.
fn read_bounded(dir: &StateDir, name: &str, remaining: &mut u64) -> Result<Vec<u8>, Fail> {
    let stat = dir
        .stat(name)
        .map_err(fs_io)?
        .ok_or_else(|| Fail::Reason("entry vanished during enumeration".into()))?;
    if stat.kind != EntryKind::File {
        return Err(Fail::Reason("not a regular file".into()));
    }
    if stat.size > MAX_SNAPSHOT_BYTES {
        return Err(Fail::Reason("snapshot exceeds size cap".into()));
    }
    if stat.size > *remaining {
        return Err(Fail::Budget);
    }
    let limit = MAX_SNAPSHOT_BYTES.min(*remaining);
    let bytes = match dir.read(name, limit) {
        Ok(bytes) => bytes,
        Err(FsError::TooLarge { .. }) => {
            return Err(if limit < MAX_SNAPSHOT_BYTES {
                Fail::Budget
            } else {
                Fail::Reason("snapshot exceeds size cap".into())
            });
        }
        Err(FsError::NotAFile(_)) => return Err(Fail::Reason("not a regular file".into())),
        Err(e) => return Err(fs_io(e).into()),
    };
    *remaining -= bytes.len() as u64;
    Ok(bytes)
}

fn load(dir: &StateDir, name: &str, stem: &str, remaining: &mut u64) -> Result<Snapshot, Fail> {
    let bytes = read_bounded(dir, name, remaining)?;
    parse(&bytes, stem)
}

fn parse(bytes: &[u8], stem: &str) -> Result<Snapshot, Fail> {
    let malformed = |e: serde_json::Error| Fail::Reason(format!("malformed snapshot: {e}"));
    let header: Header = serde_json::from_slice(bytes).map_err(malformed)?;
    if header.schema_version != u64::from(SCHEMA_VERSION) {
        return Err(Fail::Reason(format!(
            "unsupported schema version {} (supported: {SCHEMA_VERSION})",
            header.schema_version
        )));
    }
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(malformed)?;
    validate_id(&snapshot.id).map_err(Fail::Reason)?;
    if snapshot.id != stem {
        return Err(Fail::Reason("snapshot id does not match file name".into()));
    }
    Ok(snapshot)
}

/// Temp names left by an unfinished `save`: RightKit's `.scan-*.json.rktmp-*`, and the
/// earlier `.scan-*.tmp` and `scan-*.json.tmp`.
fn is_temp_name(name: &str) -> bool {
    name.starts_with(&format!(".{ID_PREFIX}")) && name.contains(TEMP_MARKER)
}

/// Load all snapshots, skipping (with a reason) any individual bad file:
/// malformed, oversized, unknown schema version, bad or mismatched id,
/// duplicate ids, symlink or non-regular entries named like snapshots, files
/// left unread once the aggregate read budget is spent, and leftover temp
/// files ("interrupted publication", never opened or parsed). Unrelated names
/// are ignored. Enumeration runs through the pinned directory (`StateDir::list`),
/// retains at most `MAX_ENUMERATED_ENTRIES` names, and stops as soon as more than
/// `MAX_SNAPSHOTS` candidate names (snapshots plus temp files) are seen, and then
/// errors. Errors are otherwise reserved for the
/// directory itself (link/reparse point, wrong ownership, group/other
/// writable, unreadable, swapped mid-check). Nothing is ever migrated or
/// deleted.
pub fn history_report(directory: &Path) -> io::Result<HistoryReport> {
    history_report_with_budget(directory, MAX_AGGREGATE_READ_BYTES)
}

/// `history_report` with an explicit aggregate read budget in bytes.
pub fn history_report_with_budget(directory: &Path, budget: u64) -> io::Result<HistoryReport> {
    let dir = match open_state_dir(directory, false) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HistoryReport::default()),
        Err(e) => return Err(e),
    };
    let mut report = HistoryReport::default();
    report
        .capability_notes
        .extend(PLATFORM_NOTES.iter().map(|s| s.to_string()));
    let mut names: Vec<String> = Vec::new();
    let mut candidates = 0usize;
    let entries = dir.list(MAX_ENUMERATED_ENTRIES).map_err(fs_io)?;
    for name in entries {
        if is_temp_name(&name) {
            candidates += 1;
            report.skipped.push(SkippedSnapshot {
                file: name,
                reason: REASON_INTERRUPTED.into(),
            });
        } else if name.starts_with(ID_PREFIX) && name.ends_with(".json") {
            candidates += 1;
            names.push(name);
        }
        if candidates > MAX_SNAPSHOTS {
            return Err(invalid("history exceeds snapshot count cap"));
        }
    }
    names.sort();
    // Names that differ only by ASCII case resolve to one ID on
    // case-insensitive filesystems; none of them is trusted.
    let mut groups: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for name in &names {
        *groups.entry(name.to_ascii_lowercase()).or_default() += 1;
    }
    let mut remaining = budget;
    let mut exhausted = false;
    for name in names {
        let skip = |reason: &str| SkippedSnapshot {
            file: name.clone(),
            reason: reason.to_owned(),
        };
        if groups[&name.to_ascii_lowercase()] > 1 {
            report.skipped.push(skip(REASON_DUPLICATE_ID));
            continue;
        }
        if exhausted {
            report.skipped.push(skip(REASON_AGGREGATE_BUDGET));
            continue;
        }
        let regular = match dir.stat(&name) {
            Ok(Some(stat)) => stat.kind == EntryKind::File,
            Ok(None) => {
                report
                    .skipped
                    .push(skip("entry vanished during enumeration"));
                continue;
            }
            Err(e) => {
                report.skipped.push(skip(&fs_io(e).to_string()));
                continue;
            }
        };
        if !regular {
            report.skipped.push(skip("not a regular file"));
            continue;
        }
        let stem = &name[..name.len() - ".json".len()];
        if let Err(reason) = validate_id(stem) {
            report.skipped.push(skip(&reason));
            continue;
        }
        let loaded = load(&dir, &name, stem, &mut remaining);
        match loaded {
            Ok(snapshot) => report.snapshots.push(snapshot),
            Err(Fail::Budget) => {
                exhausted = true;
                report.skipped.push(skip(REASON_AGGREGATE_BUDGET));
            }
            Err(Fail::Reason(reason)) => report.skipped.push(skip(&reason)),
        }
    }
    report
        .snapshots
        .sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    report
        .skipped
        .sort_by(|a, b| (&a.file, &a.reason).cmp(&(&b.file, &b.reason)));
    Ok(report)
}

/// Valid snapshots only, deterministically ordered. Bad files are skipped; use
/// `history_report` to see why.
pub fn history(directory: &Path) -> io::Result<Vec<Snapshot>> {
    history_report(directory).map(|r| r.snapshots)
}

// ---------------------------------------------------------------------------
// Versioned durable records.
// ---------------------------------------------------------------------------

/// Schema version for generic state records. Snapshot schema is deliberately
/// separate: records are journals/projections, never scan snapshots.
pub const STATE_SCHEMA_VERSION: u32 = 1;
/// Maximum serialized record size. Records are bounded independently from
/// snapshots so a corrupt journal cannot consume the snapshot read budget.
pub const MAX_STATE_RECORD_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum number of record files retained in one state namespace.
pub const MAX_STATE_RECORDS: usize = 10_000;
/// Maximum number of version files examined for one namespace.
pub const MAX_STATE_VERSIONS: usize = MAX_STATE_RECORDS * 32;
const MAX_STATE_ID_BYTES: usize = 256;
const STATE_FILE_PREFIX: &str = "record-";

/// A generic, versioned state value. The `id` is data, not a path; record
/// filenames use a byte-safe hexadecimal key so activity IDs may retain their
/// existing syntax without weakening path validation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VersionedRecord<T> {
    pub schema_version: u32,
    pub id: String,
    pub version: u64,
    pub state: T,
}

/// A pinned private child namespace below a caller-provided state directory.
/// Each publication is append-only and exact: an existing `(id, version)` can
/// never be replaced. Readers select the highest valid version for each id.
/// Superseded versions stay until a caller removes them with `remove_older`.
#[derive(Clone)]
pub struct StateStore {
    directory: PathBuf,
    namespace: String,
    pinned: Arc<StateDir>,
}

impl std::fmt::Debug for StateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateStore")
            .field("directory", &self.directory)
            .field("namespace", &self.namespace)
            .finish()
    }
}

impl StateStore {
    /// Open (and, when absent, create) a private child namespace. Existing
    /// paths are only accepted after the same pinned ownership/mode checks as
    /// snapshot storage; no permissions or ACLs are rewritten.
    pub fn open(directory: &Path, namespace: &str) -> io::Result<Self> {
        validate_namespace(namespace)?;
        // Pin parent first so a concurrently swapped parent cannot redirect
        // child creation. Child creation is descriptor-relative.
        let parent = open_state_dir(directory, true)?;
        let child = directory.join(namespace);
        let child_handle = parent.child(namespace).map_err(fs_io)?;
        Ok(Self {
            directory: child,
            namespace: namespace.to_owned(),
            pinned: Arc::new(child_handle),
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Publish one immutable version. `AlreadyExists` is returned for an
    /// exact existing claim; unrelated files are never overwritten.
    pub fn publish<T: Serialize>(&self, id: &str, version: u64, state: &T) -> io::Result<PathBuf> {
        validate_state_id(id)?;
        if version == 0 {
            return Err(invalid_input("state record version must be nonzero"));
        }
        let record = VersionedRecord {
            schema_version: STATE_SCHEMA_VERSION,
            id: id.to_owned(),
            version,
            state,
        };
        let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_STATE_RECORD_BYTES {
            return Err(invalid_input("state record exceeds size cap"));
        }
        let dir = self.pin()?;
        let destination = state_file_name(id, version)?;
        if dir.exists(&destination).map_err(fs_io)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "state record already exists",
            ));
        }
        let existing_names = state_names(&dir)?;
        if existing_names
            .iter()
            .filter(|name| parse_state_file_name(name).is_some())
            .count()
            >= MAX_STATE_VERSIONS
        {
            return Err(invalid_input("state record version cap exceeded"));
        }
        let encoded = encode_id(id);
        let has_id = existing_names
            .iter()
            .filter_map(|name| parse_state_file_name(name))
            .any(|(key, _)| key == encoded);
        if !has_id {
            let mut ids = std::collections::BTreeSet::new();
            for name in &existing_names {
                if let Some((key, _)) = parse_state_file_name(name) {
                    ids.insert(key);
                }
            }
            if ids.len() >= MAX_STATE_RECORDS {
                return Err(invalid_input("state record id cap exceeded"));
            }
        }
        // RightKit's exclusive create-new, sync, no-replace publication, directory sync.
        dir.write_new(&destination, &bytes).map_err(fs_io)?;
        Ok(self.directory.join(destination))
    }

    /// Remove versions of `id` older than `below`, never the newest one, so a
    /// caller whose newer version is already durable can drop the ones it
    /// superseded. Returns how many were removed. Removal is descriptor-relative
    /// like every other operation here; a version that is already gone is
    /// skipped and any other failure is returned (callers may ignore it: a
    /// leftover old version is never read while a newer one exists).
    pub fn remove_older(&self, id: &str, below: u64) -> io::Result<usize> {
        validate_state_id(id)?;
        let dir = self.pin()?;
        let encoded = encode_id(id);
        let versions: Vec<(u64, String)> = state_names(&dir)?
            .into_iter()
            .filter_map(|name| {
                let (key, version) = parse_state_file_name(&name)?;
                (key == encoded).then_some((version, name))
            })
            .collect();
        let newest = versions.iter().map(|(version, _)| *version).max();
        let mut removed = 0;
        for (version, name) in versions {
            if version >= below || Some(version) == newest {
                continue;
            }
            // A missing entry is `Ok(false)`; a removal whose directory sync failed is an
            // error (`DirSyncFailed`) even though the entry is already gone.
            if dir.remove(&name).map_err(fs_io)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Every record id that has at least one version, in no particular order.
    /// A file whose name does not decode to a valid id is not listed.
    pub fn ids(&self) -> io::Result<Vec<String>> {
        let dir = self.pin()?;
        let keys: std::collections::BTreeSet<String> = state_names(&dir)?
            .into_iter()
            .filter_map(|name| parse_state_file_name(&name).map(|(key, _)| key))
            .collect();
        Ok(keys.iter().filter_map(|key| decode_id(key).ok()).collect())
    }

    /// Remove every version of `id`, the newest included, so the id stops
    /// counting against the namespace's cap. Unlike `remove_older` this
    /// forgets the record: only for a caller that has settled it for good.
    /// Removal is descriptor-relative; a version already gone is skipped and
    /// any other failure is returned. Returns how many versions were removed.
    pub fn remove_id(&self, id: &str) -> io::Result<usize> {
        validate_state_id(id)?;
        let dir = self.pin()?;
        let encoded = encode_id(id);
        let names: Vec<String> = state_names(&dir)?
            .into_iter()
            .filter(|name| parse_state_file_name(name).is_some_and(|(key, _)| key == encoded))
            .collect();
        let mut removed = 0;
        for name in names {
            // A missing entry is `Ok(false)`; a removal whose directory sync failed is an
            // error (`DirSyncFailed`) even though the entry is already gone.
            if dir.remove(&name).map_err(fs_io)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Return newest record for `id`, or `None` if no record has been claimed.
    pub fn load<T: for<'de> Deserialize<'de>>(
        &self,
        id: &str,
    ) -> io::Result<Option<VersionedRecord<T>>> {
        validate_state_id(id)?;
        let dir = self.pin()?;
        let names = state_names(&dir)?;
        let encoded = encode_id(id);
        let mut newest: Option<(u64, String)> = None;
        for name in names {
            if let Some((key, version)) = parse_state_file_name(&name)
                && key == encoded
            {
                newest = match newest {
                    Some((v, n)) if v >= version => Some((v, n)),
                    _ => Some((version, name)),
                };
            }
        }
        newest
            .map(|(_, name)| read_state_record(&dir, &name, id))
            .transpose()
    }

    /// Load newest record for every id. Malformed records are an error: using
    /// an older journal silently would risk repeating an indeterminate effect.
    pub fn records<T: for<'de> Deserialize<'de>>(&self) -> io::Result<Vec<VersionedRecord<T>>> {
        let dir = self.pin()?;
        let names = state_names(&dir)?;
        let mut newest: std::collections::BTreeMap<String, (u64, String)> =
            std::collections::BTreeMap::new();
        for name in names {
            if let Some((key, version)) = parse_state_file_name(&name) {
                let id = decode_id(&key)?;
                let entry = newest.entry(id).or_insert((version, name.clone()));
                if version > entry.0 {
                    *entry = (version, name);
                }
            }
        }
        if newest.len() > MAX_STATE_RECORDS {
            return Err(invalid("state record id cap exceeded"));
        }
        newest
            .into_iter()
            .map(|(id, (_, name))| read_state_record(&dir, &name, &id))
            .collect()
    }

    fn pin(&self) -> io::Result<Arc<StateDir>> {
        Ok(Arc::clone(&self.pinned))
    }
}

fn validate_namespace(namespace: &str) -> io::Result<()> {
    if namespace.is_empty()
        || namespace.len() > 64
        || !namespace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid_input("invalid state namespace"));
    }
    Ok(())
}

fn validate_state_id(id: &str) -> io::Result<()> {
    if id.is_empty() || id.len() > MAX_STATE_ID_BYTES || !id.is_char_boundary(id.len()) {
        return Err(invalid_input("invalid state record id"));
    }
    Ok(())
}

fn encode_id(id: &str) -> String {
    rightkit_fs::digest::hex(id.as_bytes())
}

fn decode_id(key: &str) -> io::Result<String> {
    let bytes = rightkit_fs::digest::unhex(key)
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| invalid("invalid state record filename"))?;
    let id = String::from_utf8(bytes).map_err(|_| invalid("state record id is not UTF-8"))?;
    validate_state_id(&id)?;
    Ok(id)
}

fn state_file_name(id: &str, version: u64) -> io::Result<String> {
    validate_state_id(id)?;
    Ok(format!(
        "{}{}-v{}.json",
        STATE_FILE_PREFIX,
        encode_id(id),
        version
    ))
}

fn parse_state_file_name(name: &str) -> Option<(String, u64)> {
    let rest = name
        .strip_prefix(STATE_FILE_PREFIX)?
        .strip_suffix(".json")?;
    let (key, version) = rest.rsplit_once("-v")?;
    if key.is_empty() || version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let version = version.parse().ok()?;
    (version > 0).then(|| (key.to_owned(), version))
}

fn state_names(dir: &StateDir) -> io::Result<Vec<String>> {
    let names = dir.list(MAX_STATE_VERSIONS).map_err(fs_io)?;
    if names
        .iter()
        .filter(|name| parse_state_file_name(name).is_some())
        .count()
        > MAX_STATE_VERSIONS
    {
        return Err(invalid("state record count cap exceeded"));
    }
    Ok(names)
}

fn read_state_record<T: for<'de> Deserialize<'de>>(
    dir: &StateDir,
    name: &str,
    expected_id: &str,
) -> io::Result<VersionedRecord<T>> {
    let bytes = match dir.read(name, MAX_STATE_RECORD_BYTES) {
        Ok(bytes) => bytes,
        Err(FsError::NotAFile(_)) => return Err(invalid("state record is not a regular file")),
        Err(e) => return Err(fs_io(e)),
    };
    let record: VersionedRecord<T> = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(format!("malformed state record: {e}")))?;
    if record.schema_version != STATE_SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported state schema version {}",
            record.schema_version
        )));
    }
    validate_state_id(&record.id)?;
    if record.id != expected_id || record.version == 0 {
        return Err(invalid("state record identity does not match filename"));
    }
    Ok(record)
}
