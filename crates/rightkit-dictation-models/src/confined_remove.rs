//! Deletion confined to a verified directory, through pinned handles, and capped one-handle reads.
//!
//! Every place in this crate that removes a directory tree, a file, a marker link or a lock
//! leftover goes through [`ConfinedDir`] (or the `remove_*_at` helpers below, which pin the parent
//! first). The capped reads of this crate's small state files ([`read_capped_at`],
//! [`open_file_at`]) are here too. What it guarantees, and what it does not:
//!
//! * **The root is verified once.** [`ConfinedDir::open`] is `rightkit_fs::StateDir::open_shared`:
//!   every component of the path is opened without following a link (`O_NOFOLLOW` on Unix, no
//!   reparse point on Windows), the last one must be a real directory owned by the current user
//!   (Unix; Windows checks no owner), and the pinned inode must be the one the path names. A link
//!   anywhere in the root's own path is refused, not resolved: the path is deliberately not
//!   canonicalized first, because that would turn a planted link into a trusted target. A data
//!   folder reached through a link or junction is therefore never cleaned up (the helpers report
//!   `link refused`).
//! * **Nothing is addressed by path again.** Every later step is an `*at` call relative to a
//!   pinned handle: a child is opened with `stat` (kind, without following) and then `child`
//!   (opened without following), a removal is `unlinkat` / handle-relative delete on the pinned
//!   parent. Renaming or swapping a path component after the root was pinned cannot redirect a
//!   removal.
//! * **Only real directories are entered.** A link at any level is removed as the link itself (a
//!   directory entry inside a tree) or refused (the named top of a tree removal); its target is
//!   never entered. A directory that is not owned by the current user cannot be pinned and is
//!   retained.
//! * **Bounded.** At most [`MAX_DEPTH`] nested directories and [`MAX_ENTRIES`] entries per
//!   removal; past either bound the remainder is retained and counted, never walked. Each removal
//!   flushes the containing directory (`StateDir` makes removals durable), so the cost of a tree
//!   is proportional to its entry count; that is why the entry bound is 20 000.
//! * **Reads are one handle and a byte cap.** [`read_capped_at`] and [`open_file_at`] are
//!   `rightkit_fs::open_read_nofollow`: the leaf is opened without following a link and anything
//!   that is not a regular file is refused; the length cap is applied to the bytes read, not to a
//!   metadata check made before a second open. Unlike a delete, a read does not pin the parent
//!   chain: the folder holding this crate's own state files is the app's, and a data folder that
//!   is reached through a link must keep working.
//!
//! Remaining windows, stated: `StateDir::child` creates an empty directory if the name vanished
//! between the `stat` and the open (inside the verified parent, empty, owner-only); the final
//! `remove_dir` of a directory removes whatever real, empty directory has that name at that
//! moment (a non-empty one is refused by the OS); an entry that appears after a directory was
//! listed keeps that directory in place (retained). The module never runs a path-based
//! `remove_dir_all`. Windows deletes are not retried on a sharing violation (the owner's
//! `remove_tree` retries; this walker counts the entry as retained and the next run tries again).

// Each platform uses a different subset (the ANE cache is macOS, the retired-model reclaim differs
// per platform).
#![allow(dead_code)]

use rightkit_fs::{EntryKind, FsError, ReadOptions, StateDir};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Deepest directory nesting a removal descends into, counted from the directory being removed
/// (which is depth 0). The deepest tree this crate removes (a compiled Core ML bundle) is far
/// shallower; 32 leaves room and still bounds the recursion at 32 frames and 32 open handles.
pub(crate) const MAX_DEPTH: usize = 32;
/// Most entries (files, links and directories together) one removal visits. A Core ML cache or a
/// retired model holds hundreds at most; 20 000 bounds the work and the memory of one listing.
pub(crate) const MAX_ENTRIES: usize = 20_000;
/// Longest listing [`ConfinedDir::dir_names`] returns.
const MAX_LISTING: usize = 10_000;

/// What one removal did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Removal {
    /// Entries removed: files, links (as themselves) and directories.
    pub removed: usize,
    /// Summed length of the regular files removed.
    pub bytes_freed: u64,
    /// Entries (or whole trees) left in place: too recent, over a bound, or refused by the OS.
    pub retained: usize,
}

/// A directory pinned by handle. See the module docs.
pub(crate) struct ConfinedDir {
    dir: StateDir,
}

/// A stable reason; never a path, a name or an OS message (no user content in logs).
fn reason(error: FsError) -> &'static str {
    match error {
        FsError::Symlink(_) => "link refused",
        FsError::NotADirectory(_) => "not a directory",
        FsError::NotAFile(_) => "not a regular file",
        FsError::NotOwned(_) | FsError::TooPermissive(_) => "not owned by the current user",
        FsError::IdentityChanged(_) => "directory changed while pinning",
        FsError::BadName(_) => "invalid entry name",
        FsError::TooManyEntries(_) => "too many entries",
        FsError::TooLarge { .. } => "file too large",
        _ => "I/O failure",
    }
}

/// `path` as its parent directory and its final component (which must be valid Unicode).
fn split(path: &Path) -> Result<(&Path, &str), &'static str> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("path has no parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid entry name")?;
    Ok((parent, name))
}

impl ConfinedDir {
    /// Pin the directory at `path`. `Ok(None)` when it does not exist; a link anywhere in the
    /// path, a path that is not a directory, or one the current user does not own is an error.
    pub(crate) fn open(path: &Path) -> Result<Option<Self>, &'static str> {
        match StateDir::open_shared(path) {
            Ok(dir) => Ok(Some(Self { dir })),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(reason(error)),
        }
    }

    /// Pin the existing real directory `name` directly below this one. `Ok(None)` when absent.
    pub(crate) fn open_child(&self, name: &str) -> Result<Option<Self>, &'static str> {
        match self.dir.stat(name).map_err(reason)? {
            None => Ok(None),
            Some(stat) if stat.kind == EntryKind::Dir => self
                .dir
                .child(name)
                .map(|dir| Some(Self { dir }))
                .map_err(reason),
            Some(stat) if stat.kind == EntryKind::Symlink => Err("link refused"),
            Some(_) => Err("not a directory"),
        }
    }

    /// Pin the chain of existing real directories `names` below this one, one verified step each.
    pub(crate) fn open_chain(&self, names: &[&str]) -> Result<Option<Self>, &'static str> {
        let Some((first, rest)) = names.split_first() else {
            return Err("empty chain");
        };
        let Some(mut current) = self.open_child(first)? else {
            return Ok(None);
        };
        for name in rest {
            let Some(next) = current.open_child(name)? else {
                return Ok(None);
            };
            current = next;
        }
        Ok(Some(current))
    }

    /// Names of the real directories directly inside (links and files are not listed). At most
    /// [`MAX_LISTING`] entries; more is an error.
    pub(crate) fn dir_names(&self) -> Result<Vec<String>, &'static str> {
        Ok(self
            .dir
            .list_entries(MAX_LISTING)
            .map_err(reason)?
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::Dir)
            .map(|entry| entry.name)
            .collect())
    }

    /// Number of direct entries, at most [`MAX_ENTRIES`].
    pub(crate) fn entry_count(&self) -> Result<usize, &'static str> {
        self.dir
            .list_entries(MAX_ENTRIES)
            .map(|entries| entries.len())
            .map_err(reason)
    }

    /// True when anything (a link included, not followed) has this name.
    pub(crate) fn exists(&self, name: &str) -> Result<bool, &'static str> {
        self.dir
            .stat(name)
            .map(|stat| stat.is_some())
            .map_err(reason)
    }

    /// True when a regular file (not a link, not a directory) has this name.
    pub(crate) fn is_regular_file(&self, name: &str) -> Result<bool, &'static str> {
        self.dir
            .stat(name)
            .map(|stat| stat.is_some_and(|stat| stat.kind == EntryKind::File))
            .map_err(reason)
    }

    /// Remove the regular file `name` (a link or a directory of that name is refused).
    pub(crate) fn remove_regular_file(&self, name: &str) -> Result<bool, &'static str> {
        match self.dir.stat(name).map_err(reason)? {
            None => Ok(false),
            Some(stat) if stat.kind == EntryKind::File => self.dir.remove(name).map_err(reason),
            Some(_) => Err("not a regular file"),
        }
    }

    /// Remove the entry `name` when it is a regular file, or a link as the link itself (never its
    /// target). A directory is refused. Absent is `Ok(false)`.
    pub(crate) fn remove_file_or_link(&self, name: &str) -> Result<bool, &'static str> {
        match self.dir.stat(name).map_err(reason)? {
            None => Ok(false),
            Some(stat) if stat.kind == EntryKind::Symlink => {
                self.dir.remove_link(name).map_err(reason)
            }
            Some(stat) if stat.kind == EntryKind::File || stat.kind == EntryKind::Other => {
                self.dir.remove(name).map_err(reason)
            }
            Some(_) => Err("not a file"),
        }
    }

    /// Remove the symlink or junction `name` as the link itself (anything else is refused).
    pub(crate) fn remove_link(&self, name: &str) -> Result<bool, &'static str> {
        match self.dir.stat(name).map_err(reason)? {
            None => Ok(false),
            Some(stat) if stat.kind == EntryKind::Symlink => {
                self.dir.remove_link(name).map_err(reason)
            }
            Some(_) => Err("not a link"),
        }
    }

    /// Remove the real directory `name` and everything inside it, within [`MAX_DEPTH`] and
    /// [`MAX_ENTRIES`]. A link or a file of that name is refused (`Err`); an absent name is an
    /// empty [`Removal`]. With `min_age`, a directory whose own modification time is younger (or
    /// unknown, or in the future) is kept whole and counted as retained.
    pub(crate) fn remove_dir_tree(
        &self,
        name: &str,
        min_age: Option<Duration>,
    ) -> Result<Removal, &'static str> {
        let Some(stat) = self.dir.stat(name).map_err(reason)? else {
            return Ok(Removal::default());
        };
        match stat.kind {
            EntryKind::Dir => {}
            EntryKind::Symlink => return Err("link refused"),
            _ => return Err("not a directory"),
        }
        let mut removal = Removal::default();
        if let Some(min_age) = min_age {
            if !old_enough(stat.modified_unix_ms, min_age) {
                removal.retained = 1;
                return Ok(removal);
            }
        }
        let mut budget = MAX_ENTRIES;
        remove_tree_in(&self.dir, name, 0, &mut budget, &mut removal);
        Ok(removal)
    }

    /// [`remove_dir_tree`](Self::remove_dir_tree), except that a link of that name is removed as
    /// the link itself (its target is neither entered nor touched) and counted as one removal.
    pub(crate) fn remove_tree_or_link(
        &self,
        name: &str,
        min_age: Option<Duration>,
    ) -> Result<Removal, &'static str> {
        match self.dir.stat(name).map_err(reason)? {
            Some(stat) if stat.kind == EntryKind::Symlink => {
                let removed = usize::from(self.dir.remove_link(name).map_err(reason)?);
                Ok(Removal {
                    removed,
                    ..Removal::default()
                })
            }
            _ => self.remove_dir_tree(name, min_age),
        }
    }
}

/// Remove the file at `path` through its pinned parent: a regular file, or a link as the link
/// itself. `Ok(false)` when the parent or the entry is absent; a directory is refused.
pub(crate) fn remove_file_at(path: &Path) -> Result<bool, &'static str> {
    let (parent, name) = split(path)?;
    match ConfinedDir::open(parent)? {
        Some(dir) => dir.remove_file_or_link(name),
        None => Ok(false),
    }
}

/// Remove the directory tree at `path` through its pinned parent (see
/// [`ConfinedDir::remove_dir_tree`]). An absent parent or entry is an empty [`Removal`].
pub(crate) fn remove_tree_at(
    path: &Path,
    min_age: Option<Duration>,
) -> Result<Removal, &'static str> {
    let (parent, name) = split(path)?;
    match ConfinedDir::open(parent)? {
        Some(dir) => dir.remove_dir_tree(name, min_age),
        None => Ok(Removal::default()),
    }
}

/// Open the regular file at `path` for reading: one handle, the leaf not followed if it is a
/// link, anything that is not a regular file refused. `Ok(None)` when absent.
pub(crate) fn open_file_at(path: &Path) -> Result<Option<File>, &'static str> {
    match rightkit_fs::open_read_nofollow(path, &ReadOptions::new()) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(reason(error)),
    }
}

/// Read the regular file at `path` through one handle, refusing more than `max_bytes` (at most
/// `max_bytes + 1` bytes are read). `Ok(None)` when absent.
pub(crate) fn read_capped_at(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, &'static str> {
    let Some(file) = open_file_at(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    (&file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "I/O failure")?;
    if bytes.len() as u64 > max_bytes {
        return Err("file too large");
    }
    Ok(Some(bytes))
}

/// The modification time is known, not in the future, and at least `min_age` ago.
fn old_enough(modified_unix_ms: u64, min_age: Duration) -> bool {
    if modified_unix_ms == 0 {
        return false;
    }
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return false;
    };
    let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    now_ms
        .checked_sub(modified_unix_ms)
        .is_some_and(|age_ms| Duration::from_millis(age_ms) >= min_age)
}

/// Empty and remove the directory `name` of the pinned `parent`. Failures are counted in `out`,
/// never raised, so one stubborn entry does not stop its siblings.
fn remove_tree_in(
    parent: &StateDir,
    name: &str,
    depth: usize,
    budget: &mut usize,
    out: &mut Removal,
) {
    if depth >= MAX_DEPTH {
        out.retained += 1;
        return;
    }
    let Ok(child) = parent.child(name) else {
        out.retained += 1;
        return;
    };
    // One past the budget: a listing longer than what is left is an error, not an allocation.
    let Ok(entries) = child.list_entries(budget.saturating_add(1)) else {
        out.retained += 1;
        return;
    };
    let retained_before = out.retained;
    for entry in entries {
        if *budget == 0 {
            out.retained += 1;
            break;
        }
        *budget -= 1;
        match entry.kind {
            EntryKind::Dir => remove_tree_in(&child, &entry.name, depth + 1, budget, out),
            EntryKind::Symlink => match child.remove_link(&entry.name) {
                Ok(true) => out.removed += 1,
                Ok(false) | Err(_) => out.retained += 1,
            },
            EntryKind::File | EntryKind::Other => {
                let size = match child.stat(&entry.name) {
                    Ok(Some(stat)) if stat.kind == EntryKind::File => stat.size,
                    _ => 0,
                };
                match child.remove(&entry.name) {
                    Ok(true) => {
                        out.removed += 1;
                        out.bytes_freed = out.bytes_freed.saturating_add(size);
                    }
                    Ok(false) | Err(_) => out.retained += 1,
                }
            }
        }
    }
    // Release the handle before removing the entry it pins (Windows refuses to delete a directory
    // that is held open without delete sharing).
    drop(child);
    if out.retained != retained_before {
        return;
    }
    match parent.remove_dir(name) {
        Ok(true) => out.removed += 1,
        Ok(false) | Err(_) => out.retained += 1,
    }
}
