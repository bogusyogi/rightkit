//! A bounded set of managed entries in one directory, with all-or-nothing retirement.
//!
//! For a folder of things the app keeps a limited number of (the archive of each catalog import,
//! staged copies, rotated files): [`Retention`] keeps at most a count and a total size, retires the
//! oldest, never retires a protected entry, and never leaves the set half-trimmed.
//!
//! [`Retention::commit`] is called after the new entry has been published into the directory. It
//! picks the entries to retire (oldest first, by the order key) until both caps hold, then:
//!
//! 1. renames every victim aside to a hidden name in the same directory (`.<name>.rkretire-<pid>-<n>`),
//!    which is atomic and reversible;
//! 2. if any rename fails, renames the ones already moved back, **withdraws the entry that was just
//!    added** (removes it), and returns the error, so the set is as it was before the add and the
//!    old entries stay;
//! 3. only when every victim was moved aside, removes the hidden copies. A hidden copy that cannot be
//!    removed (a file in use on Windows) is reported in [`RetentionReport::leftovers`] and swept by a
//!    later call once the process that left it is gone.
//!
//! The added entry and every protected entry are never victims. If they alone exceed a cap, the set
//! stays over that cap: retention bounds what it may retire, it never retires what it must keep.
//!
//! Managed entries are the direct children of the directory whose names do not start with `.`
//! (hidden names are this module's own scratch names and quarantined entries). [`quarantine`] moves
//! an entry that must not be lost, or deleted, out of the managed set under a `.recovered-<ms>`
//! name that is never overwritten.

use crate::error::{FsError, Result};
use crate::tree::{remove_tree, RemoveOptions};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// What the order key and the size caps see of one managed entry.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Candidate {
    /// The entry's name in the directory.
    pub name: String,
    /// A file's length, or the summed length of the regular files in a directory tree (links are
    /// not followed and count as zero).
    pub bytes: u64,
    /// Modification time, milliseconds since the Unix epoch (zero when unreadable).
    pub modified_ms: u64,
}

type OrderKey = Box<dyn Fn(&Candidate) -> i128 + Send + Sync>;

/// A retention policy over one directory. Build with [`Retention::new`].
pub struct Retention {
    dir: PathBuf,
    max_entries: Option<usize>,
    max_bytes: Option<u64>,
    protected: Vec<String>,
    order: OrderKey,
}

impl std::fmt::Debug for Retention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Retention")
            .field("dir", &self.dir)
            .field("max_entries", &self.max_entries)
            .field("max_bytes", &self.max_bytes)
            .field("protected", &self.protected)
            .finish_non_exhaustive()
    }
}

/// What [`Retention::commit`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetentionReport {
    /// Names of the entries retired (removed), oldest first.
    pub retired: Vec<String>,
    /// Managed entries left in the directory, the added one included.
    pub kept: usize,
    /// Total bytes of the managed entries left.
    pub kept_bytes: u64,
    /// Hidden retired copies that could not be removed yet.
    pub leftovers: Vec<PathBuf>,
}

impl Retention {
    /// A policy over `dir` with no caps and the default order (oldest modification time first,
    /// then name).
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            max_entries: None,
            max_bytes: None,
            protected: Vec::new(),
            order: Box::new(|c| i128::from(c.modified_ms)),
        }
    }

    /// Keep at most `count` managed entries (the newest).
    #[must_use]
    pub fn keep_newest(mut self, count: usize) -> Self {
        self.max_entries = Some(count);
        self
    }

    /// Keep the managed entries' total size at or under `bytes`, retiring the oldest.
    #[must_use]
    pub fn max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = Some(bytes);
        self
    }

    /// Never retire the entry called `name`. May be called more than once.
    #[must_use]
    pub fn protect(mut self, name: &str) -> Self {
        self.protected.push(name.to_string());
        self
    }

    /// Order entries by `key` instead of the modification time: a smaller key is older and is
    /// retired first. Ties are broken by name.
    #[must_use]
    pub fn order_by(mut self, key: impl Fn(&Candidate) -> i128 + Send + Sync + 'static) -> Self {
        self.order = Box::new(key);
        self
    }

    /// Apply the policy after `added` (a direct child of the directory, already published) was
    /// added. See the module docs for the all-or-nothing behaviour. `added` must be a managed name
    /// that exists; it is never retired.
    pub fn commit(&self, added: &str) -> Result<RetentionReport> {
        validate_child_name(added)?;
        if !is_managed(added) || self.dir.join(added).symlink_metadata().is_err() {
            return Err(FsError::BadName(added.to_string()));
        }
        let leftovers_before = sweep_dead_hidden(&self.dir);
        let mut candidates = self.list()?;
        candidates.sort_by(|a, b| {
            (self.order)(a)
                .cmp(&(self.order)(b))
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut count = candidates.len();
        let mut bytes: u64 = candidates.iter().map(|c| c.bytes).sum();
        let mut victims: Vec<&Candidate> = Vec::new();
        for candidate in &candidates {
            let over_count = self.max_entries.is_some_and(|max| count > max);
            let over_bytes = self.max_bytes.is_some_and(|max| bytes > max);
            if !over_count && !over_bytes {
                break;
            }
            if candidate.name == added || self.protected.iter().any(|p| *p == candidate.name) {
                continue;
            }
            count -= 1;
            bytes -= candidate.bytes;
            victims.push(candidate);
        }

        // Move every victim aside; undo and withdraw the added entry if one cannot be.
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        for victim in &victims {
            let from = self.dir.join(&victim.name);
            let to = hidden_name(&self.dir, &victim.name);
            if let Err(error) = fs::rename(&from, &to) {
                for (original, hidden) in moved.iter().rev() {
                    let _ = fs::rename(hidden, original);
                }
                let _ = remove_entry(&self.dir.join(added));
                return Err(FsError::io(&from, error));
            }
            moved.push((from, to));
        }
        let mut leftovers = leftovers_before;
        for (_, hidden) in &moved {
            match remove_entry(hidden) {
                Ok(true) => {}
                Ok(false) | Err(_) => leftovers.push(hidden.clone()),
            }
        }
        Ok(RetentionReport {
            retired: victims.iter().map(|v| v.name.clone()).collect(),
            kept: count,
            kept_bytes: bytes,
            leftovers,
        })
    }

    /// The managed entries now in the directory.
    fn list(&self) -> Result<Vec<Candidate>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir).map_err(|e| FsError::io(&self.dir, e))? {
            let entry = entry.map_err(|e| FsError::io(&self.dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_managed(&name) {
                continue;
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| FsError::io(&path, e))?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            out.push(Candidate {
                name,
                bytes: tree_bytes(&path, &meta),
                modified_ms,
            });
        }
        Ok(out)
    }
}

/// Move the entry at `path` out of the way without deleting it: to a sibling named
/// `<name>.recovered-<unix-ms>` (a number is appended if that name is taken), never overwriting.
/// Returns the new path. For an artefact that must not be lost (a damaged log, a record that
/// differs from its backup). The new name starts with the old one, so a [`Retention`] over that
/// directory still sees it as a managed entry; to keep it out of the set, use [`quarantine_hidden`].
pub fn quarantine(path: &Path) -> Result<PathBuf> {
    quarantine_as(path, false)
}

/// [`quarantine`] to `.<name>.recovered-<unix-ms>`, which [`Retention`] never counts, retires or
/// removes.
pub fn quarantine_hidden(path: &Path) -> Result<PathBuf> {
    quarantine_as(path, true)
}

fn quarantine_as(path: &Path, hidden: bool) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| FsError::BadName(path.display().to_string()))?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let prefix = if hidden { "." } else { "" };
    for attempt in 0..1000u32 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let target = dir.join(format!("{prefix}{name}.recovered-{ms}{suffix}"));
        match crate::atomic::rename_exclusive(path, &target) {
            Ok(()) => return Ok(target),
            Err(FsError::AlreadyExists(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(FsError::AlreadyExists(path.to_path_buf()))
}

fn validate_child_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(FsError::BadName(name.to_string()));
    }
    Ok(())
}

fn is_managed(name: &str) -> bool {
    !name.starts_with('.')
}

fn hidden_name(dir: &Path, name: &str) -> PathBuf {
    let short: String = name.chars().take(100).collect();
    dir.join(format!(
        ".{short}.rkretire-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Remove a file, a link (as itself) or a directory tree. `Ok(false)` when part of a tree was
/// retained.
fn remove_entry(path: &Path) -> Result<bool> {
    let meta = fs::symlink_metadata(path).map_err(|e| FsError::io(path, e))?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        let report = remove_tree(path, &RemoveOptions::new())?;
        return Ok(report.retained == 0);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        // A directory symlink on Windows is removed with `remove_dir`.
        Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied => {
            fs::remove_dir(path)
                .map(|()| true)
                .map_err(|e| FsError::io(path, e))
        }
        Err(e) => Err(FsError::io(path, e)),
    }
}

/// Sizes without following links.
fn tree_bytes(path: &Path, meta: &fs::Metadata) -> u64 {
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let child = entry.path();
            fs::symlink_metadata(&child).map_or(0, |m| tree_bytes(&child, &m))
        })
        .sum()
}

/// Remove hidden retired copies whose process is confirmed gone. Returns those that could not be
/// removed. A copy owned by a live or unknown process is left alone and not listed.
fn sweep_dead_hidden(dir: &Path) -> Vec<PathBuf> {
    let mut remaining = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return remaining;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((_, rest)) = name.split_once(".rkretire-") else {
            continue;
        };
        if !name.starts_with('.') {
            continue;
        }
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid != std::process::id()
            && crate::process_liveness(pid) == crate::ProcessLiveness::Dead
            && !matches!(remove_entry(&entry.path()), Ok(true))
        {
            remaining.push(entry.path());
        }
    }
    remaining
}
