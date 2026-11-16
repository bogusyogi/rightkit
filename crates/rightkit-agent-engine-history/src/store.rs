//! Persistent snapshot store — writes content under
//! `<workspace>/<state_dir>/history/<session-key>/<path-key>/<timestamp_ns>.snap`,
//! where both keys come from [`slot_key`], an injective escaping that always
//! yields one plain directory name. Slots written before the injective key
//! (flat `_` keys) are still read; see [`HistoryStore::slot_dir`].

use std::path::{Path, PathBuf};

/// Directory inside a session's history holding the stash of the last undo.
const REDO_DIR: &str = "redo";

/// Why a rewind leaves a path that became a symlink after its snapshot.
const SYMLINK_REFUSED: &str = "target is a symlink; left as it is";
/// Why a rewind leaves a path whose parent directory now leaves the workspace.
const PARENT_ESCAPES_REFUSED: &str =
    "a parent directory is a symlink that leaves the workspace; left as it is";

/// Error code a write fails with when no restore point can be stored.
pub const HISTORY_UNAVAILABLE: &str = "history_unavailable";

/// Retention per file slot: at most this many restore points are kept...
const MAX_ENTRIES_PER_SLOT: usize = 200;
/// ...holding at most this many snapshot bytes. The newest entry is always
/// kept, whatever its size; older ones are pruned oldest first.
const MAX_SLOT_BYTES: u64 = 512 * 1024 * 1024;
/// File in a slot naming the newest entry retention pruned; a rewind that
/// would need a pruned entry is refused instead of restoring a later state.
const PRUNED_FILE: &str = "PRUNED";
/// File in every slot written under the injective key. A slot without it was
/// written by an earlier version under the flat `_` key.
const SLOT_MARKER: &str = "SLOT";
/// Marker a fork writes into its copy of a flat-keyed slot, so the copy keeps
/// the shared-slot check of [`HistoryStore::restore_since`] although it is
/// filed under the injective key.
const LEGACY_SLOT_MARKER: &str = "LEGACY";
/// Separates a key from its probe number when the plain name of a slot is
/// taken by another path. `%+` occurs in no key, so such a name is never the
/// plain key of any path.
const PROBE_SEPARATOR: &str = "%+";
/// Longest directory name [`slot_key`] returns: well under the 255-byte
/// `NAME_MAX` of common filesystems.
const MAX_KEY_BYTES: usize = 200;

pub struct HistoryStore {
    /// `<workspace>/<state_dir>/history/<session-id>`: every session owns its
    /// own snapshots (sessions rules 41 and 42).
    base: PathBuf,
    /// `<workspace>/<state_dir>/history`, the parent of every session's slots.
    root: PathBuf,
    /// The session's directory name below `root` (one plain component).
    session: String,
}

/// A snapshot selected for a transactional restore.
///
/// The file remains in history until [`HistoryStore::consume_snapshot`] is
/// called after the caller's mutation authority has committed the restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySnapshot {
    path: PathBuf,
    pub content: String,
}

impl HistorySnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// [`HistorySnapshot`] for a snapshot read as raw bytes: whatever was stored
/// comes back exactly, UTF-8 or not.
///
/// The file remains in history until [`HistoryStore::consume_snapshot_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySnapshotBytes {
    path: PathBuf,
    pub content: Vec<u8>,
}

impl HistorySnapshotBytes {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl HistoryStore {
    /// Store the snapshots of one session under
    /// `<workspace_root>/<state_dir>/history/<session_id>`.
    ///
    /// `state_dir` is the host app's hidden per-workspace directory name (for
    /// example `.myapp`); it is created on first snapshot and git-ignored.
    pub fn new(workspace_root: &Path, state_dir: &str, session_id: &str) -> Self {
        let root = workspace_root.join(state_dir).join("history");
        let session = slot_key(std::ffi::OsStr::new(session_id));
        Self {
            base: root.join(&session),
            root,
            session,
        }
    }

    /// A fork starts with its own copy of the parent's snapshots, up to and
    /// including `up_to_ns` (the fork point). The copies are independent files:
    /// a rewind in the fork restores from the fork's slots only, and a rewind
    /// in the parent never reads the fork's (sessions rule 42).
    /// `parent_root` is where the parent ran (its isolation worktree, when it had
    /// one) and `child_root` where the fork will run; each keeps its history there.
    pub fn fork_from(
        parent_root: &Path,
        child_root: &Path,
        state_dir: &str,
        parent_session_id: &str,
        child_session_id: &str,
        up_to_ns: u128,
    ) -> std::io::Result<()> {
        let parent = Self::new(parent_root, state_dir, parent_session_id);
        let child = Self::new(child_root, state_dir, child_session_id);
        if !parent.base.exists() {
            return Ok(());
        }
        // The fork's own root (its isolation worktree) must exist before its history is
        // written into it: creating `<child_root>/<state_dir>/...` first would leave a
        // non-empty directory that `git worktree add` then refuses, and the fork would run
        // in place while its copy sat orphaned. The host creates the worktree first, then
        // copies.
        if parent_root != child_root && !child_root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "the fork's root {} does not exist yet; create it (its worktree) before copying history",
                    child_root.display()
                ),
            ));
        }
        let canonical_parent_root = std::fs::canonicalize(parent_root).ok();
        for slot in std::fs::read_dir(&parent.base)? {
            let slot = slot?.path();
            let Some(name) = slot.file_name() else {
                continue;
            };
            if !slot.is_dir() || name == REDO_DIR {
                continue;
            }
            // A slot records the absolute path it belongs to. When the fork runs in its own
            // worktree, a path inside the parent's root is moved to the same place under the
            // child's root, so a rewind in the fork never writes into the parent's files.
            let origin = std::fs::read_to_string(slot.join("PATH")).ok();
            let rebased = origin.as_deref().and_then(|origin| {
                let origin = Path::new(origin);
                (parent_root != child_root && origin.is_absolute())
                    .then(|| {
                        // A tool may record the canonical spelling of a path (`/private/var/...`
                        // for `/var/...`, or through a symlinked parent), so the parent's root
                        // is matched in that spelling too.
                        origin.strip_prefix(parent_root).ok().or_else(|| {
                            canonical_parent_root
                                .as_deref()
                                .and_then(|root| origin.strip_prefix(root).ok())
                        })
                    })
                    .flatten()
                    .map(|relative| child_root.join(relative))
            });
            let target = match &rebased {
                Some(path) => child.slot_dir(path),
                None => child.base.join(name),
            };
            let target_is_new = !target.exists();
            create_private_dir_all(&target)?;
            if let Some(path) = &rebased {
                write_private(&target.join("PATH"), path.to_string_lossy().as_bytes())?;
                if target_is_new {
                    write_private(&target.join(SLOT_MARKER), b"")?;
                }
                if origin
                    .as_deref()
                    .is_some_and(|origin| Self::is_possibly_shared_slot(&slot, origin))
                {
                    write_private(&target.join(LEGACY_SLOT_MARKER), b"")?;
                }
            }
            if let Some(state_dir) = child.root.parent() {
                let gi = state_dir.join(".gitignore");
                if !gi.exists() {
                    write_private(&gi, b"*\n")?;
                }
            }
            for file in std::fs::read_dir(&slot)? {
                let file = file?.path();
                let keep = match file.file_stem().and_then(|s| s.to_str()) {
                    Some("PATH") => rebased.is_none(),
                    Some(PRUNED_FILE | LEGACY_SLOT_MARKER) => true,
                    Some(SLOT_MARKER) => rebased.is_none(),
                    Some(stem) => stem.parse::<u128>().is_ok_and(|ts| ts <= up_to_ns),
                    None => false,
                };
                if keep && file.is_file() {
                    let bytes = std::fs::read(&file)?;
                    if let Some(file_name) = file.file_name() {
                        write_private(&target.join(file_name), &bytes)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Snapshot `content` as the pre-edit state of `path`.
    /// `path` may be relative (to workspace) or absolute.
    pub fn snapshot(&self, path: &Path, content: &str) -> std::io::Result<()> {
        self.write_entry(path, "snap", content.as_bytes())
    }

    /// Snapshot the raw `bytes` as the pre-edit state of `path`. A snapshot
    /// is a `.snap` file holding the bytes as they are (the same file
    /// [`Self::snapshot`] writes for text), so content that is not valid UTF-8
    /// gets a restore point too and comes back exactly. Read it back with
    /// [`Self::peek_latest_bytes`] or [`Self::restore_since`]; the text readers
    /// ([`Self::peek_latest`], [`Self::restore_latest`]) fail on such a file.
    pub fn snapshot_bytes(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        self.write_entry(path, "snap", bytes)
    }

    /// Whether a restore point for `path` can be recorded in this store
    /// without mixing it with another file's history. Slots are keyed
    /// injectively (`a/b_c` and `a_b/c` have slots of their own), so this is
    /// `true` for any path except two cases, which a caller that must not lose
    /// data reports as having no restore point:
    ///
    /// - the path is not valid UTF-8 (a slot records its path as text, so a
    ///   rewind could not name the file again);
    /// - the path has no slot of its own yet and a slot written by an earlier
    ///   version under the flat `_` key, owned by a *different* path, still
    ///   holds restore points. Such a slot may hold some of this path's older
    ///   restore points too, and nothing in it says whose they are. It frees up
    ///   once it holds no restore point (every one consumed by a rewind or
    ///   pruned) or when the session is deleted.
    pub fn can_record(&self, path: &Path) -> bool {
        self.recording_refusal(path).is_none()
    }

    /// Why `path` cannot be recorded (see [`Self::can_record`]).
    fn recording_refusal(&self, path: &Path) -> Option<&'static str> {
        if path.to_str().is_none() {
            return Some("the path is not valid UTF-8, so a history slot could not name it again");
        }
        if !self.slot_dir(path).exists() && self.shares_name_with_older_slot(path) {
            return Some(
                "its history slot name is taken by another file's restore points from an earlier version, which may include this file's own; they cannot be told apart",
            );
        }
        None
    }

    /// Whether the flat-keyed slot named by the old key of `path` belongs to a
    /// different path and still holds restore points (or a pending redo).
    fn shares_name_with_older_slot(&self, path: &Path) -> bool {
        let name = legacy_key(path);
        if !is_plain_component(&name) || name == REDO_DIR {
            return false;
        }
        let dir = self.base.join(&name);
        if !dir.is_dir() || dir.join(SLOT_MARKER).exists() {
            return false;
        }
        match std::fs::read_to_string(dir.join("PATH")) {
            Ok(owner) if owner != path.to_string_lossy() => {}
            _ => return false,
        }
        let held = |dir: &Path| Self::entries_in_dir(dir).map(|e| !e.is_empty());
        held(&dir).unwrap_or(true) || held(&self.base.join(REDO_DIR).join(&name)).unwrap_or(true)
    }

    /// Record that `path` did not exist before the edit (the edit created it).
    /// Rewinding past this entry deletes the file again.
    pub fn snapshot_absent(&self, path: &Path) -> std::io::Result<()> {
        self.write_entry(path, "new", b"")
    }

    /// Record the content `path` has after the edit (`None` when the edit
    /// deleted it) against the newest entry for `path`, so a later rewind can
    /// tell that another process changed the file since.
    pub fn record_after(&self, path: &Path, content: Option<&str>) -> std::io::Result<()> {
        self.record_after_bytes(path, content.map(str::as_bytes))
    }

    /// [`Self::record_after`] for raw bytes (any encoding). A text record and a
    /// byte record of the same content are the same record.
    ///
    /// An entry whose call changed nothing (a failed or no-op edit: the file
    /// still holds exactly its pre-edit state) is dropped instead, so a model
    /// retrying a mismatched edit cannot push real restore points out of the
    /// retention window.
    pub fn record_after_bytes(&self, path: &Path, content: Option<&[u8]>) -> std::io::Result<()> {
        self.record_after_fingerprint(path, fingerprint(content))
    }

    /// [`Self::record_after_bytes`] for the file as it is on disk now, read in
    /// bounded chunks instead of into memory (a file that does not exist is
    /// recorded as deleted). For a file too large to hold.
    pub fn record_after_file(&self, path: &Path) -> std::io::Result<()> {
        let now = match fingerprint_file(path) {
            Ok(now) => now,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fingerprint(None),
            Err(error) => return Err(error),
        };
        self.record_after_fingerprint(path, now)
    }

    fn record_after_fingerprint(&self, path: &Path, now: String) -> std::io::Result<()> {
        let dir = self.slot_dir(path);
        let Some((stem, kind)) = Self::entries_in_dir(&dir)?.pop() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no history entry to attach the post-edit state to",
            ));
        };
        let after = dir.join(format!("{stem}.after"));
        // Only a fresh entry (no post-edit record yet) can be this call's own.
        if !after.exists() {
            let entry = dir.join(format!("{stem}.{}", kind.ext()));
            let before = match kind {
                EntryKind::Snap => fingerprint_file(&entry).ok(),
                EntryKind::New => Some(fingerprint(None)),
            };
            if before.as_deref() == Some(now.as_str()) {
                return std::fs::remove_file(&entry);
            }
        }
        write_private(&after, now.as_bytes())?;
        // Bounded retention: a failed prune never fails the edit.
        let _ = Self::prune_slot(&dir, MAX_ENTRIES_PER_SLOT, MAX_SLOT_BYTES);
        Ok(())
    }

    fn write_entry(&self, path: &Path, ext: &str, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(reason) = self.recording_refusal(path) {
            return Err(std::io::Error::other(reason));
        }
        let dir = self.slot_dir(path);
        let fresh = !dir.exists();
        create_private_dir_all(&dir)?;
        if fresh {
            write_private(&dir.join(SLOT_MARKER), b"")?;
        }
        // Keep the snapshot store out of the user's VCS: the state dir is
        // internal state, not project content. (Tool file-discovery already
        // prunes the state dir; this keeps `git status` clean too.)
        if let Some(state_dir) = self.root.parent() {
            let gi = state_dir.join(".gitignore");
            if !gi.exists() {
                write_private(&gi, b"*\n")?;
            }
        }
        // The slot remembers the path it belongs to; a rewind reads it back.
        let origin = dir.join("PATH");
        if !origin.exists() {
            write_private(&origin, path.to_string_lossy().as_bytes())?;
        }
        let ts = timestamp_ns();
        write_private(&dir.join(format!("{ts:020}.{ext}")), bytes)?;
        // Bounded retention. The entry just written does not count yet: it may
        // turn out to be a no-op that `record_after_bytes` drops, and only then
        // is retention applied to the real limits. This bound keeps a slot
        // whose post-edit record never arrives from growing without end.
        // A failed prune never fails the edit.
        let _ = Self::prune_slot(
            &dir,
            MAX_ENTRIES_PER_SLOT + 1,
            MAX_SLOT_BYTES.saturating_add(bytes.len() as u64),
        );
        Ok(())
    }

    /// Drop the oldest entries of a slot until it holds at most `max_entries`
    /// entries and `max_bytes` of snapshots ([`MAX_ENTRIES_PER_SLOT`] and
    /// [`MAX_SLOT_BYTES`] on the write path).
    ///
    /// The newest entry always stays, however large, so the loop ends after at
    /// most one pass over the entries and a single huge snapshot never makes it
    /// spin. Entries stashed for a redo are not in the slot and are never
    /// touched. The newest pruned stem is recorded in [`PRUNED_FILE`] before
    /// anything is deleted, so a rewind that needs a pruned entry reports it
    /// instead of restoring a later state in silence.
    fn prune_slot(dir: &Path, max_entries: usize, max_bytes: u64) -> std::io::Result<()> {
        let entries = Self::entries_in_dir(dir)?;
        let sizes: Vec<u64> = entries
            .iter()
            .map(|(stem, kind)| {
                std::fs::metadata(dir.join(format!("{stem}.{}", kind.ext())))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .collect();
        let mut total: u64 = sizes.iter().sum();
        let mut count = entries.len();
        for ((stem, kind), size) in entries.iter().zip(&sizes) {
            if count <= 1 || (count <= max_entries && total <= max_bytes) {
                break;
            }
            // Mark first, so a crash between the two steps over-reports.
            let newest_pruned = Self::pruned_stem(dir);
            if newest_pruned.is_none_or(|old| stem.parse::<u128>().is_ok_and(|ts| ts > old)) {
                write_private(&dir.join(PRUNED_FILE), stem.as_bytes())?;
            }
            std::fs::remove_file(dir.join(format!("{stem}.{}", kind.ext())))?;
            let _ = std::fs::remove_file(dir.join(format!("{stem}.after")));
            total = total.saturating_sub(*size);
            count -= 1;
        }
        Ok(())
    }

    /// The timestamp of the newest entry retention pruned from `dir`, if any.
    fn pruned_stem(dir: &Path) -> Option<u128> {
        std::fs::read_to_string(dir.join(PRUNED_FILE))
            .ok()
            .and_then(|text| text.trim().parse::<u128>().ok())
    }

    /// Restore every file the rewound turns changed to its content before the
    /// first change at or after `cutoff_ns` (nanoseconds since the Unix epoch,
    /// the clock the entries are named by). `workspace_root` resolves relative
    /// paths. A file whose newest post-edit record no longer matches the file
    /// is left alone and reported (another process changed it). Restored files
    /// have their entries from the cutoff on removed; reported files keep
    /// theirs. Never partial in silence: every touched slot lands in exactly
    /// one of `restored` and `failed`.
    pub fn restore_since(&self, workspace_root: &Path, cutoff_ns: u128) -> RestoreReport {
        let mut report = RestoreReport::default();
        let Ok(slots) = std::fs::read_dir(&self.base) else {
            return report;
        };
        // Only the most recent undo can be redone: a new rewind drops the old
        // stash, but only once it is about to restore something, so a rewind
        // that matches nothing (or refuses every file) keeps the redo.
        let redo_root = self.base.join(REDO_DIR);
        let mut old_stash_dropped = false;
        let mut dirs: Vec<PathBuf> = slots
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|path| path.file_name().is_some_and(|name| name != REDO_DIR))
            .collect();
        dirs.sort();
        // How many slots name each path among those a rewind reaches. A path in
        // two slots (an older version wrote the flat slot after this one wrote
        // the injective slot) has its history split, so no order of the two is
        // known to be right.
        let mut slots_per_path: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for dir in &dirs {
            let Ok(origin) = std::fs::read_to_string(dir.join("PATH")) else {
                continue;
            };
            let reached = Self::entries_in_dir(dir)
                .map(|entries| {
                    entries
                        .iter()
                        .any(|(stem, _)| Self::stem_at_or_after(stem, cutoff_ns))
                })
                .unwrap_or(true);
            if reached {
                *slots_per_path.entry(origin).or_default() += 1;
            }
        }
        for dir in dirs {
            let Ok(origin) = std::fs::read_to_string(dir.join("PATH")) else {
                continue;
            };
            let entries = match Self::entries_in_dir(&dir) {
                Ok(entries) => entries,
                Err(error) => {
                    report
                        .failed
                        .push((origin, format!("history unreadable: {error}")));
                    continue;
                }
            };
            let mut rewound: Vec<&(String, EntryKind)> = entries
                .iter()
                .filter(|(stem, _)| Self::stem_at_or_after(stem, cutoff_ns))
                .collect();
            rewound.sort();
            let (Some(first), Some(last)) = (rewound.first().copied(), rewound.last().copied())
            else {
                continue;
            };
            let target = {
                let path = PathBuf::from(&origin);
                if path.is_absolute() {
                    path
                } else {
                    workspace_root.join(path)
                }
            };
            if Self::pruned_stem(&dir).is_some_and(|pruned| pruned >= cutoff_ns) {
                report.failed.push((
                    origin,
                    format!(
                        "the restore point is older than the history kept for this file (the newest {MAX_ENTRIES_PER_SLOT} edits and {} MiB per file); left as it is",
                        MAX_SLOT_BYTES / (1024 * 1024)
                    ),
                ));
                continue;
            }
            if slots_per_path.get(&origin).is_some_and(|count| *count > 1) {
                report.failed.push((
                    origin,
                    "the history of this file is split over more than one slot, so the order of its restore points is unknown; left as it is"
                        .to_string(),
                ));
                continue;
            }
            // A slot from before the injective key may hold the entries of two
            // files (`a/b_c` and `a_b/c` shared one), while PATH names only the
            // first writer. Restore it only when its rewound entries read as
            // one file's unbroken history; otherwise one file's old bytes would
            // be written over the other.
            if Self::is_possibly_shared_slot(&dir, &origin)
                && !Self::is_one_files_history(&dir, &rewound)
            {
                report.failed.push((
                    origin,
                    "this history predates per-file keys and may mix the edits of more than one file; left as it is"
                        .to_string(),
                ));
                continue;
            }
            // Fail closed: without the post-edit record nothing shows the file
            // still holds what the engine wrote, so it is reported unverified.
            match std::fs::read_to_string(dir.join(format!("{}.after", last.0))) {
                Ok(recorded) => {
                    let now = fingerprint_or_absent(&target);
                    if recorded.trim() != now {
                        report.failed.push((
                            origin,
                            "changed since by another process; left as it is".to_string(),
                        ));
                        continue;
                    }
                }
                Err(_) => {
                    report.failed.push((
                        origin,
                        "unverified: the post-edit state was never recorded, so a later change cannot be ruled out; left as it is"
                            .to_string(),
                    ));
                    continue;
                }
            }
            if is_symlink(&target) {
                report.failed.push((origin, SYMLINK_REFUSED.to_string()));
                continue;
            }
            if parent_escapes_workspace(&target, workspace_root) {
                report
                    .failed
                    .push((origin, PARENT_ESCAPES_REFUSED.to_string()));
                continue;
            }
            if !old_stash_dropped {
                let _ = std::fs::remove_dir_all(&redo_root);
                old_stash_dropped = true;
            }
            // The undone state is stashed first so `redo_since` can put it back.
            let stash = redo_root.join(dir.file_name().unwrap_or_default());
            let restored_fingerprint = match first.1 {
                EntryKind::Snap => fingerprint(
                    std::fs::read(dir.join(format!("{}.snap", first.0)))
                        .ok()
                        .as_deref(),
                ),
                EntryKind::New => fingerprint(None),
            };
            let stashed = (|| -> std::io::Result<()> {
                create_private_dir_all(&stash)?;
                write_private(&stash.join("PATH"), origin.as_bytes())?;
                write_private(&stash.join("restored.fp"), restored_fingerprint.as_bytes())?;
                match std::fs::read(&target) {
                    Ok(bytes) => write_private(&stash.join("current"), &bytes),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        write_private(&stash.join("current.absent"), b"")
                    }
                    Err(error) => Err(error),
                }
            })();
            if let Err(error) = stashed {
                let _ = std::fs::remove_dir_all(&stash);
                report.failed.push((
                    origin,
                    format!("could not keep the undone state for redo: {error}"),
                ));
                continue;
            }
            let outcome = match first.1 {
                EntryKind::Snap => {
                    std::fs::read(dir.join(format!("{}.snap", first.0))).and_then(|content| {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        write_replacing(&target, &content)
                    })
                }
                EntryKind::New => match std::fs::remove_file(&target) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                    _ => Ok(()),
                },
            };
            match outcome {
                Ok(()) => {
                    // Move the consumed entries into the stash (redo puts them back).
                    for (stem, kind) in rewound {
                        for name in [format!("{stem}.{}", kind.ext()), format!("{stem}.after")] {
                            let _ = std::fs::rename(dir.join(&name), stash.join(&name));
                        }
                    }
                    report.restored.push(origin);
                }
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&stash);
                    report
                        .failed
                        .push((origin, format!("could not restore: {error}")));
                }
            }
        }
        report
    }

    /// The engine's write path (sessions rule 41): before a tool call runs,
    /// write a restore point for every file it may write. An existing file is
    /// snapshotted with its content, a file that does not exist yet is recorded
    /// as absent. Any failure returns the `history_unavailable` error text and
    /// the caller must not run the call: no file is edited without a restore
    /// point.
    pub fn snapshot_targets(&self, targets: &[PathBuf]) -> Result<(), String> {
        for path in targets {
            let outcome = match std::fs::read(path) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => self.snapshot(path, &text),
                    Err(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "the file is not valid UTF-8",
                    )),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.snapshot_absent(path)
                }
                Err(error) => Err(error),
            };
            outcome.map_err(|error| {
                format!(
                    "{HISTORY_UNAVAILABLE}: no restore point could be stored for {} ({error}); the call was not run and the file is unchanged",
                    path.display()
                )
            })?;
        }
        Ok(())
    }

    /// After the call: record the state of each target so a later rewind can
    /// tell whether another process changed it since. Returns the paths whose
    /// record could not be written (a rewind will report them as unverified).
    pub fn record_after_targets(&self, targets: &[PathBuf]) -> Vec<PathBuf> {
        let mut failed = Vec::new();
        for path in targets {
            let content = match std::fs::read_to_string(path) {
                Ok(text) => Some(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => {
                    failed.push(path.clone());
                    continue;
                }
            };
            if self.record_after(path, content.as_deref()).is_err() {
                failed.push(path.clone());
            }
        }
        failed
    }

    /// Re-apply the most recent `restore_since` (the redo of an undo): every
    /// stashed file goes back to the state it had before the undo, and its
    /// entries return to the slot so a later rewind still works. A file that
    /// no longer holds what the undo restored (another process, or a newer
    /// edit) is left alone and reported. The stash is spent either way.
    pub fn redo_since(&self) -> RestoreReport {
        let mut report = RestoreReport::default();
        let redo_root = self.base.join(REDO_DIR);
        let Ok(stashes) = std::fs::read_dir(&redo_root) else {
            return report;
        };
        let mut dirs: Vec<PathBuf> = stashes.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        dirs.sort();
        for stash in dirs {
            let Ok(origin) = std::fs::read_to_string(stash.join("PATH")) else {
                continue;
            };
            let target = {
                let path = PathBuf::from(&origin);
                if path.is_absolute() {
                    path
                } else {
                    self.workspace_root().join(path)
                }
            };
            let expected = std::fs::read_to_string(stash.join("restored.fp")).unwrap_or_default();
            let now = fingerprint_or_absent(&target);
            if expected.trim() != now {
                report
                    .failed
                    .push((origin, "changed since the undo; left as it is".to_string()));
                continue;
            }
            if is_symlink(&target) {
                report.failed.push((origin, SYMLINK_REFUSED.to_string()));
                continue;
            }
            if parent_escapes_workspace(&target, &self.workspace_root()) {
                report
                    .failed
                    .push((origin, PARENT_ESCAPES_REFUSED.to_string()));
                continue;
            }
            let outcome = if stash.join("current.absent").exists() {
                match std::fs::remove_file(&target) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                    _ => Ok(()),
                }
            } else {
                std::fs::read(stash.join("current")).and_then(|bytes| {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    write_replacing(&target, &bytes)
                })
            };
            match outcome {
                Ok(()) => {
                    let slot = self.base.join(stash.file_name().unwrap_or_default());
                    if let Ok(files) = std::fs::read_dir(&stash) {
                        for file in files.filter_map(|e| e.ok()).map(|e| e.path()) {
                            let keep = file
                                .extension()
                                .and_then(|x| x.to_str())
                                .is_some_and(|ext| matches!(ext, "snap" | "new" | "after"));
                            if keep {
                                if let Some(name) = file.file_name() {
                                    let _ = std::fs::rename(&file, slot.join(name));
                                }
                            }
                        }
                    }
                    report.restored.push(origin);
                }
                Err(error) => report
                    .failed
                    .push((origin, format!("could not redo: {error}"))),
            }
        }
        let _ = std::fs::remove_dir_all(&redo_root);
        report
    }

    /// Drop the redo stash without applying it.
    pub fn discard_redo(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(self.base.join(REDO_DIR)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Whether an undo can still be redone (a stash exists).
    pub fn has_redo(&self) -> bool {
        self.base.join(REDO_DIR).is_dir()
    }

    /// `path` as a file on disk: a relative path is the workspace's.
    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root().join(path)
        }
    }

    fn workspace_root(&self) -> PathBuf {
        // `<workspace>/<state_dir>/history` is `root`.
        self.root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// Delete this session's snapshots (session deletion: sessions rule 41).
    /// Refuses unless the session directory is exactly one plain directory
    /// below the history root, so it can never remove the root itself or the
    /// workspace's state directory.
    pub fn delete_session(&self) -> std::io::Result<()> {
        if !is_plain_component(&self.session) || self.base.parent() != Some(self.root.as_path()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session history directory is not a plain child of the history root",
            ));
        }
        match std::fs::remove_dir_all(&self.base) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Delete every session's snapshots of this workspace (the workspace folder
    /// being forgotten: sessions rule 41).
    pub fn delete_workspace(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(&self.root) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Read the most-recent snapshot for `path` without consuming it.
    /// Returns `Ok(None)` if no snapshot exists.
    pub fn peek_latest(&self, path: &Path) -> std::io::Result<Option<HistorySnapshot>> {
        let Some(latest) = self.latest_path(path)? else {
            return Ok(None);
        };
        self.require_attributed(path, &latest)?;
        Ok(Some(HistorySnapshot {
            content: std::fs::read_to_string(&latest)?,
            path: latest,
        }))
    }

    /// [`Self::peek_latest`] as raw bytes, for a snapshot of any encoding.
    ///
    /// When the newest entry of the slot records that an edit created the file
    /// and an older snapshot lies beneath it (from before the file was deleted
    /// and created again), that snapshot is not returned, since writing it over
    /// the created file would destroy it: that is an error. A created-file entry
    /// with no snapshot beneath it is `Ok(None)`, as for [`Self::peek_latest`].
    pub fn peek_latest_bytes(&self, path: &Path) -> std::io::Result<Option<HistorySnapshotBytes>> {
        let dir = self.slot_dir(path);
        let Some(latest) = Self::latest_entry_in_dir(&dir)? else {
            return Ok(None);
        };
        if latest.extension().and_then(|x| x.to_str()) == Some("new") {
            if Self::latest_path_in_dir(&dir)?.is_none() {
                return Ok(None);
            }
            return Err(std::io::Error::other(
                "the newest edit created this file; there is no earlier content to restore",
            ));
        }
        self.require_attributed(path, &latest)?;
        Ok(Some(HistorySnapshotBytes {
            content: std::fs::read(&latest)?,
            path: latest,
        }))
    }

    /// [`Self::consume_snapshot`] for a snapshot from [`Self::peek_latest_bytes`].
    pub fn consume_snapshot_bytes(&self, snapshot: &HistorySnapshotBytes) -> std::io::Result<()> {
        let missing = || {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "history snapshot no longer exists",
            )
        };
        let dir = snapshot.path().parent().ok_or_else(missing)?;
        let latest = Self::latest_entry_in_dir(dir)?.ok_or_else(missing)?;
        if latest != snapshot.path() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "a newer history snapshot exists",
            ));
        }
        std::fs::remove_file(snapshot.path())
    }

    /// [`Self::restore_latest`] as raw bytes.
    pub fn restore_latest_bytes(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        let Some(snapshot) = self.peek_latest_bytes(path)? else {
            return Ok(None);
        };
        self.consume_snapshot_bytes(&snapshot)?;
        Ok(Some(snapshot.content))
    }

    /// Consume exactly the snapshot previously returned by `peek_latest`.
    /// Refuse to remove it if a newer snapshot has appeared meanwhile.
    pub fn consume_snapshot(&self, snapshot: &HistorySnapshot) -> std::io::Result<()> {
        let Some(dir) = snapshot.path().parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "history snapshot no longer exists",
            ));
        };
        let Some(latest) = Self::latest_path_in_dir(dir)? else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "history snapshot no longer exists",
            ));
        };
        if latest != snapshot.path() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "a newer history snapshot exists",
            ));
        }
        std::fs::remove_file(snapshot.path())
    }

    /// Return and remove the most-recent snapshot for `path`.
    /// Returns `Ok(None)` if no snapshot exists.
    pub fn restore_latest(&self, path: &Path) -> std::io::Result<Option<String>> {
        let Some(snapshot) = self.peek_latest(path)? else {
            return Ok(None);
        };
        let content = snapshot.content.clone();
        self.consume_snapshot(&snapshot)?;
        Ok(Some(content))
    }

    /// A flat-keyed slot written by an earlier version may hold another file's
    /// entries, so its newest entry is returned for `path` only when its
    /// post-edit record equals the file now (the entry then reads as the last
    /// edit of this very file). Any other slot is this path's alone.
    fn require_attributed(&self, path: &Path, entry: &Path) -> std::io::Result<()> {
        let Some(dir) = entry.parent() else {
            return Ok(());
        };
        if !Self::is_possibly_shared_slot(dir, &path.to_string_lossy()) {
            return Ok(());
        }
        let recorded = entry.file_stem().and_then(|stem| {
            std::fs::read_to_string(dir.join(format!("{}.after", stem.to_string_lossy()))).ok()
        });
        match recorded {
            Some(recorded) if recorded.trim() == fingerprint_or_absent(&self.resolve(path)) => Ok(()),
            _ => Err(std::io::Error::other(
                "this history predates per-file keys and may hold another file's edit; left as it is",
            )),
        }
    }

    fn latest_path(&self, path: &Path) -> std::io::Result<Option<PathBuf>> {
        Self::latest_path_in_dir(&self.slot_dir(path))
    }

    fn latest_path_in_dir(dir: &Path) -> std::io::Result<Option<PathBuf>> {
        if !dir.exists() {
            return Ok(None);
        }

        // Collect .snap files, sort descending (latest first by name).
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("snap"))
            .collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        Ok(entries.into_iter().next())
    }

    /// The newest `.snap` or `.new` file of a slot.
    fn latest_entry_in_dir(dir: &Path) -> std::io::Result<Option<PathBuf>> {
        if !dir.exists() {
            return Ok(None);
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| matches!(p.extension().and_then(|x| x.to_str()), Some("snap" | "new")))
            .collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        Ok(entries.into_iter().next())
    }

    /// `(timestamp stem, kind)` of every snapshot or created-file marker in a
    /// slot, oldest first.
    fn entries_in_dir(dir: &Path) -> std::io::Result<Vec<(String, EntryKind)>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let kind = match path.extension().and_then(|x| x.to_str()) {
                Some("snap") => EntryKind::Snap,
                Some("new") => EntryKind::New,
                _ => continue,
            };
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push((stem.to_string(), kind));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Whether the entry stem `stem` is a timestamp at or after `cutoff_ns`.
    fn stem_at_or_after(stem: &str, cutoff_ns: u128) -> bool {
        stem.parse::<u128>().is_ok_and(|ts| ts >= cutoff_ns)
    }

    /// Whether `dir` may be a slot that two paths shared under the old flat
    /// key: a fork's copy of one (marked), or a slot written by an earlier
    /// version (no [`SLOT_MARKER`]) named by the flat key of its PATH where
    /// that key holds a `_` (the only character the flat key produced for
    /// several inputs; a key without one had a single source).
    fn is_possibly_shared_slot(dir: &Path, origin: &str) -> bool {
        if dir.join(LEGACY_SLOT_MARKER).exists() {
            return true;
        }
        if dir.join(SLOT_MARKER).exists() {
            return false;
        }
        let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        name.contains('_') && name == legacy_key(Path::new(origin))
    }

    /// Whether the `rewound` entries of `dir` (oldest first) read as one file's
    /// history: each entry's post-edit record is exactly the next entry's
    /// pre-edit state. Entries of two files interleaved in one slot break that
    /// chain. A link that holds no content (absent, or an empty file) does not
    /// count: two unrelated files can both be absent or empty at once, so such
    /// a link cannot tell one file's history from two.
    fn is_one_files_history(dir: &Path, rewound: &[&(String, EntryKind)]) -> bool {
        let empty = fingerprint(Some(b""));
        rewound.windows(2).all(|pair| {
            let (earlier, later) = (pair[0], pair[1]);
            let Ok(recorded) = std::fs::read_to_string(dir.join(format!("{}.after", earlier.0)))
            else {
                return false;
            };
            let before_later = match later.1 {
                EntryKind::Snap => match fingerprint_file(&dir.join(format!("{}.snap", later.0))) {
                    Ok(fp) => fp,
                    Err(_) => return false,
                },
                EntryKind::New => fingerprint(None),
            };
            let link = recorded.trim();
            link == before_later && link != fingerprint(None) && link != empty
        })
    }

    /// The slot directory of `path`. Slots are looked up in this order:
    ///
    /// 1. The slot named by [`slot_key`] (`name`), then `name%+1`, `name%+2`...
    ///    A slot with [`SLOT_MARKER`] under its plain key is this path's. Any
    ///    other existing slot is used only when its PATH is exactly this path;
    ///    one that names another path is skipped (that is how a flat-keyed slot
    ///    of an earlier version, or a hashed key that collided, is stepped
    ///    over). The first name that does not exist ends the search.
    /// 2. A flat-keyed slot of an earlier version whose PATH is exactly this
    ///    path (a path with no slot of its own continues its old one).
    /// 3. Otherwise the first free name of step 1: where a new slot goes.
    ///
    /// A path therefore has one slot, and a slot another file shares under the
    /// flat key is never taken for this one.
    fn slot_dir(&self, path: &Path) -> PathBuf {
        let key = slot_key(path.as_os_str());
        let hashed = is_hashed_key(&key);
        let text = path.to_string_lossy();
        let mut n = 0u32;
        let free = loop {
            let candidate = if n == 0 {
                self.base.join(&key)
            } else {
                self.base.join(format!("{key}{PROBE_SEPARATOR}{n}"))
            };
            if !candidate.exists() {
                break candidate;
            }
            let owner = std::fs::read_to_string(candidate.join("PATH")).ok();
            let plain_current_slot = n == 0 && !hashed && candidate.join(SLOT_MARKER).exists();
            if plain_current_slot || owner.as_deref() == Some(&*text) {
                return candidate;
            }
            n += 1;
        };
        let old_key = legacy_key(path);
        if is_plain_component(&old_key) && old_key != REDO_DIR {
            let legacy = self.base.join(&old_key);
            if legacy != free
                && !legacy.join(SLOT_MARKER).exists()
                && std::fs::read_to_string(legacy.join("PATH")).is_ok_and(|origin| origin == text)
            {
                return legacy;
            }
        }
        free
    }
}

/// Injective, filesystem-safe directory name for a path or session id.
///
/// Characters no platform accepts in a name (`/ \ : * ? " < > |`), control
/// characters, `%` itself, bytes that are not UTF-8, and a trailing `.` or
/// space (Windows strips those) become `%XX` per byte; everything else stays
/// as it is. Because `%` is always escaped, distinct inputs give distinct
/// names (`a/b_c` and `a_b/c` no longer share a slot). The empty input maps
/// to a lone `%`, which no other input produces, and the reserved redo
/// directory name gets its `d` escaped. The result is never empty, `.`, `..`
/// or anything containing a separator. An input whose escaped form is longer
/// than [`MAX_KEY_BYTES`] becomes a bounded, hashed name instead.
fn slot_key(text: &std::ffi::OsStr) -> String {
    use std::fmt::Write as _;
    let bytes = text.as_encoded_bytes();
    if bytes.is_empty() {
        return "%".to_string();
    }
    let mut out = String::with_capacity(bytes.len());
    let mut offset = 0;
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            let start = offset;
            offset += c.len_utf8();
            let trailing = offset == bytes.len();
            let escape = matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '%'
            ) || c.is_control()
                || (trailing && matches!(c, '.' | ' '));
            if escape {
                for byte in &bytes[start..offset] {
                    let _ = write!(out, "%{byte:02X}");
                }
            } else {
                out.push(c);
            }
        }
        for byte in chunk.invalid() {
            offset += 1;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    if out == REDO_DIR {
        return "re%64o".to_string();
    }
    if out.len() > MAX_KEY_BYTES {
        return hashed_key(&out, bytes);
    }
    out
}

/// Marker only a hashed key holds: an escaped key has `%` followed by two hex
/// digits only, so no short key contains `%~`.
const HASHED_KEY_MARKER: &str = "%~";

/// Bounded form of a key longer than [`MAX_KEY_BYTES`]: a readable prefix,
/// [`HASHED_KEY_MARKER`] and the 128-bit FNV-1a of the input as 32 hex
/// digits. Short and hashed keys never collide; two hashed keys collide only
/// on a hash collision, which [`HistoryStore::slot_dir`] resolves through the
/// slot's PATH record.
fn hashed_key(escaped: &str, input: &[u8]) -> String {
    let budget = MAX_KEY_BYTES - HASHED_KEY_MARKER.len() - 32;
    let mut cut = budget.min(escaped.len());
    while !escaped.is_char_boundary(cut) {
        cut -= 1;
    }
    // Never end the prefix inside a `%XX` escape.
    if let Some(percent) = escaped[..cut].rfind('%') {
        if cut - percent < 3 {
            cut = percent;
        }
    }
    let mut hash: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for byte in input {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    format!("{}{HASHED_KEY_MARKER}{hash:032x}", &escaped[..cut])
}

fn is_hashed_key(key: &str) -> bool {
    key.contains(HASHED_KEY_MARKER)
}

/// The lossy flat key slots were named by before [`slot_key`]; read only to
/// find slots written by earlier versions.
fn legacy_key(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Whether `name` is exactly one ordinary path component (not empty, `.`,
/// `..`, a root or prefix, and holding no separator).
fn is_plain_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(part)), None) if part == std::ffi::OsStr::new(name)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EntryKind {
    Snap,
    New,
}

impl EntryKind {
    fn ext(self) -> &'static str {
        match self {
            EntryKind::Snap => "snap",
            EntryKind::New => "new",
        }
    }
}

/// What [`HistoryStore::restore_since`] did, file by file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Paths put back to their content before the first rewound change.
    pub restored: Vec<String>,
    /// `(path, reason)` for paths left alone or that could not be restored.
    pub failed: Vec<(String, String)>,
}

/// Stable content fingerprint (FNV-1a 64): not a security primitive, only an
/// equality check between "what the engine wrote" and "what is on disk now".
///
/// Hashed over raw bytes, so a UTF-8 file fingerprints exactly as it did when
/// this took `&str`, and the `.after` and `restored.fp` records written before
/// stay valid.
fn fingerprint(content: Option<&[u8]>) -> String {
    let Some(content) = content else {
        return "absent".to_string();
    };
    let mut hash = FNV_OFFSET;
    fnv_update(&mut hash, content);
    format!("{hash:016x}")
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// [`fingerprint`] of a file read in bounded chunks. An error is the caller's
/// to judge: `NotFound` means the file is absent.
fn fingerprint_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hash = FNV_OFFSET;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        fnv_update(&mut hash, &chunk[..n]);
    }
    Ok(format!("{hash:016x}"))
}

/// What a rewind compares a record with: the fingerprint of the file now, and
/// `absent` for one that is missing or cannot be read.
fn fingerprint_or_absent(path: &Path) -> String {
    fingerprint_file(path).unwrap_or_else(|_| fingerprint(None))
}

/// Crash-atomic replace of `target` that keeps the old file's permissions.
fn write_replacing(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let permissions = std::fs::metadata(target)
        .map(|meta| meta.permissions())
        .ok();
    rightkit_fs::atomic_write(target, bytes).map_err(std::io::Error::other)?;
    if let Some(permissions) = permissions {
        // Best effort: the content is already restored.
        let _ = std::fs::set_permissions(target, permissions);
    }
    Ok(())
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Whether a target lexically inside the workspace resolves, through a symlinked
/// parent directory, to somewhere outside it. Fails closed when a path cannot be
/// resolved. Targets lexically outside the workspace are not judged here.
fn parent_escapes_workspace(target: &Path, workspace_root: &Path) -> bool {
    if !target.starts_with(workspace_root) {
        return false;
    }
    let Some(mut ancestor) = target.parent() else {
        return false;
    };
    while std::fs::symlink_metadata(ancestor).is_err() {
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return true,
        }
    }
    match (ancestor.canonicalize(), workspace_root.canonicalize()) {
        (Ok(ancestor), Ok(root)) => !ancestor.starts_with(root),
        _ => true,
    }
}

/// Owner-only on Unix (sessions rule 41); other platforms keep the default
/// per-user ACL of the profile directory.
fn create_private_dir_all(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut current = Some(dir);
        while let Some(path) = current {
            if path.file_name().is_some_and(|name| name == "history") {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
                break;
            }
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            current = path.parent();
        }
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Nanoseconds since the Unix epoch of an RFC 3339 UTC timestamp such as the
/// daemon's `now_iso()` (`2026-10-31T10:45:46.123Z`). `None` for anything else.
pub fn rfc3339_utc_to_ns(text: &str) -> Option<u128> {
    let text = text.trim().strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (clock, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = clock.split(':');
    let (h, mi, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u128::try_from(days * 86_400 + h * 3_600 + mi * 60 + sec).ok()?;
    let mut nanos: u128 = 0;
    let digits: String = frac.chars().take(9).collect();
    if !digits.is_empty() {
        nanos = digits.parse::<u128>().ok()? * 10u128.pow(9 - digits.len() as u32);
    }
    Some(secs * 1_000_000_000 + nanos)
}

/// Monotonic timestamp in nanoseconds.  Falls back to seconds*1e9 if the
/// platform doesn't support sub-second precision (extremely unlikely).
fn timestamp_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
