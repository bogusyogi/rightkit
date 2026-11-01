//! Persistent snapshot store — writes content under
//! `<workspace>/<state_dir>/history/<session-id>/<sanitized-path>/<timestamp_ns>.snap`.

use std::path::{Path, PathBuf};

/// Directory inside a session's history holding the stash of the last undo.
const REDO_DIR: &str = "redo";

/// Error code a write fails with when no restore point can be stored.
pub const HISTORY_UNAVAILABLE: &str = "history_unavailable";

pub struct HistoryStore {
    /// `<workspace>/<state_dir>/history/<session-id>`: every session owns its
    /// own snapshots (sessions rules 41 and 42).
    base: PathBuf,
    /// `<workspace>/<state_dir>/history`, the parent of every session's slots.
    root: PathBuf,
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

impl HistoryStore {
    /// Store the snapshots of one session under
    /// `<workspace_root>/<state_dir>/history/<session_id>`.
    ///
    /// `state_dir` is the host app's hidden per-workspace directory name (for
    /// example `.myapp`); it is created on first snapshot and git-ignored.
    pub fn new(workspace_root: &Path, state_dir: &str, session_id: &str) -> Self {
        let root = workspace_root.join(state_dir).join("history");
        let session = sanitize(Path::new(session_id));
        Self {
            base: root.join(&session),
            root,
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
        for slot in std::fs::read_dir(&parent.base)? {
            let slot = slot?.path();
            let Some(name) = slot.file_name() else {
                continue;
            };
            if !slot.is_dir() || name == "redo" {
                continue;
            }
            let target = child.base.join(name);
            create_private_dir_all(&target)?;
            if let Some(state_dir) = child.root.parent() {
                let gi = state_dir.join(".gitignore");
                if !gi.exists() {
                    write_private(&gi, b"*\n")?;
                }
            }
            for file in std::fs::read_dir(&slot)? {
                let file = file?.path();
                let keep = match file.file_stem().and_then(|s| s.to_str()) {
                    Some("PATH") => true,
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

    /// Record that `path` did not exist before the edit (the edit created it).
    /// Rewinding past this entry deletes the file again.
    pub fn snapshot_absent(&self, path: &Path) -> std::io::Result<()> {
        self.write_entry(path, "new", b"")
    }

    /// Record the content `path` has after the edit (`None` when the edit
    /// deleted it) against the newest entry for `path`, so a later rewind can
    /// tell that another process changed the file since.
    pub fn record_after(&self, path: &Path, content: Option<&str>) -> std::io::Result<()> {
        let dir = self.slot_dir(path);
        let Some((stem, _)) = Self::entries_in_dir(&dir)?.pop() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no history entry to attach the post-edit state to",
            ));
        };
        write_private(
            &dir.join(format!("{stem}.after")),
            fingerprint(content).as_bytes(),
        )
    }

    fn write_entry(&self, path: &Path, ext: &str, bytes: &[u8]) -> std::io::Result<()> {
        let dir = self.slot_dir(path);
        create_private_dir_all(&dir)?;
        // Keep the snapshot store out of the user's VCS: the state dir is
        // internal state, not project content. (Tool file-discovery already
        // prunes the state dir; this keeps `git status` clean too.)
        if let Some(state_dir) = self.root.parent() {
            let gi = state_dir.join(".gitignore");
            if !gi.exists() {
                write_private(&gi, b"*\n")?;
            }
        }
        // The flat key is lossy, so the slot remembers the path it belongs to.
        let origin = dir.join("PATH");
        if !origin.exists() {
            write_private(&origin, path.to_string_lossy().as_bytes())?;
        }
        let ts = timestamp_ns();
        write_private(&dir.join(format!("{ts:020}.{ext}")), bytes)
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
        // Only the most recent undo can be redone: a new rewind drops the old stash.
        let redo_root = self.base.join(REDO_DIR);
        let _ = std::fs::remove_dir_all(&redo_root);
        let mut dirs: Vec<PathBuf> = slots
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|path| path.file_name().is_some_and(|name| name != REDO_DIR))
            .collect();
        dirs.sort();
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
                .filter(|(stem, _)| stem.parse::<u128>().is_ok_and(|ts| ts >= cutoff_ns))
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
            if let Ok(recorded) = std::fs::read_to_string(dir.join(format!("{}.after", last.0))) {
                let now = fingerprint(std::fs::read_to_string(&target).ok().as_deref());
                if recorded.trim() != now {
                    report.failed.push((
                        origin,
                        "changed since by another process; left as it is".to_string(),
                    ));
                    continue;
                }
            }
            // The undone state is stashed first so `redo_since` can put it back.
            let stash = redo_root.join(dir.file_name().unwrap_or_default());
            let restored_fingerprint = match first.1 {
                EntryKind::Snap => fingerprint(
                    std::fs::read_to_string(dir.join(format!("{}.snap", first.0)))
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
                EntryKind::Snap => std::fs::read_to_string(dir.join(format!("{}.snap", first.0)))
                    .and_then(|content| {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&target, content)
                    }),
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
            let now = fingerprint(std::fs::read_to_string(&target).ok().as_deref());
            if expected.trim() != now {
                report
                    .failed
                    .push((origin, "changed since the undo; left as it is".to_string()));
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
                    std::fs::write(&target, bytes)
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

    fn workspace_root(&self) -> PathBuf {
        // `<workspace>/<state_dir>/history` is `root`.
        self.root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// Delete this session's snapshots (session deletion: sessions rule 41).
    pub fn delete_session(&self) -> std::io::Result<()> {
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
        Ok(Some(HistorySnapshot {
            content: std::fs::read_to_string(&latest)?,
            path: latest,
        }))
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

    fn slot_dir(&self, path: &Path) -> PathBuf {
        // Turn the path into a flat, filesystem-safe key.
        let key = sanitize(path);
        self.base.join(key)
    }
}

/// Replace path separators and other special chars with `_` so the result
/// is a single valid directory name component.
fn sanitize(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
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
fn fingerprint(content: Option<&str>) -> String {
    let Some(content) = content else {
        return "absent".to_string();
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit", "s1");
        let p = Path::new("src/main.rs");

        store.snapshot(p, "version 1").unwrap();
        let restored = store.restore_latest(p).unwrap();
        assert_eq!(restored, Some("version 1".to_string()));

        // No more snapshots.
        let second = store.restore_latest(p).unwrap();
        assert_eq!(second, None);
    }

    #[test]
    fn multiple_snapshots_restore_in_lifo_order() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit", "s1");
        let p = Path::new("file.txt");

        store.snapshot(p, "v1").unwrap();
        // Ensure timestamps differ.
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.snapshot(p, "v2").unwrap();

        // Most recent is v2.
        let r1 = store.restore_latest(p).unwrap();
        assert_eq!(r1, Some("v2".to_string()));

        // Next is v1.
        let r2 = store.restore_latest(p).unwrap();
        assert_eq!(r2, Some("v1".to_string()));

        // Empty.
        let r3 = store.restore_latest(p).unwrap();
        assert_eq!(r3, None);
    }

    #[test]
    fn no_snapshot_returns_none() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit", "s1");
        let result = store.restore_latest(Path::new("nonexistent.rs")).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn absolute_path_snapshots_correctly() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit", "s1");
        let abs = dir.path().join("src").join("lib.rs");
        store.snapshot(&abs, "abs content").unwrap();
        let r = store.restore_latest(&abs).unwrap();
        assert_eq!(r, Some("abs content".to_string()));
    }

    #[test]
    fn peek_keeps_snapshot_until_explicit_consume() {
        let dir = tempdir().unwrap();
        let store = HistoryStore::new(dir.path(), ".rightkit", "s1");
        let path = dir.path().join("transactional.txt");
        store.snapshot(&path, "before").unwrap();

        let snapshot = store.peek_latest(&path).unwrap().expect("snapshot");
        assert_eq!(snapshot.content, "before");
        assert!(store.peek_latest(&path).unwrap().is_some());

        store.consume_snapshot(&snapshot).unwrap();
        assert!(store.peek_latest(&path).unwrap().is_none());
    }
}
