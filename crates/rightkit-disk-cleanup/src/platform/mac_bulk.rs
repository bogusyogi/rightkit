//! Bulk directory reads for macOS. The listing is RightKit's `rightkit_fsindex::list_dir_fd`
//! (one `getattrlistbulk` per batch, descriptor-relative). This module keeps the shape the scanner
//! uses: each child with the facts of a regular file, where RightKit reports them complete.

use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Directory children with their bulk-read file facts, and whether more were left unread.
pub type BulkChildren = (Vec<(PathBuf, Option<BulkFile>)>, bool);

/// Facts about one regular file, read in the same call as its name.
#[derive(Clone, Debug)]
pub struct BulkFile {
    /// st_dev of the file.
    pub dev: u64,
    /// st_ino of the file.
    pub ino: u64,
    /// Logical size (st_size).
    pub logical: u64,
    /// Allocated bytes (st_blocks * 512).
    pub allocation: u64,
    /// Creation and modification times, Unix seconds (both non-negative).
    pub created: u64,
    pub modified: u64,
}

/// Reads up to `limit` entries of the directory open as `fd`. Returns the
/// children (joined to `parent`) with their file facts where complete, and
/// whether more entries were left unread. `.` and `..` are never returned.
pub(super) fn read_entries(fd: RawFd, parent: &Path, limit: usize) -> io::Result<BulkChildren> {
    // SAFETY: the caller keeps `fd` open for the whole call (it owns the pinned directory).
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let options = rightkit_fsindex::ListOptions {
        limit: Some(limit),
        want_creation_time: true,
        refuse_dataless: false,
    };
    let listing = rightkit_fsindex::list_dir_fd(fd, options).map_err(|error| match error {
        rightkit_fsindex::Error::Io(error) => error,
        other => io::Error::other(other.to_string()),
    })?;
    let children = listing
        .entries
        .into_iter()
        .map(|entry| {
            let file = bulk_file(&entry);
            (parent.join(&entry.name), file)
        })
        .collect();
    Ok((children, listing.truncated))
}

/// The facts of a regular file that is on disk and has a complete size and times. Anything else
/// is `None`, and the caller inspects that entry the usual way.
fn bulk_file(entry: &rightkit_fsindex::Entry) -> Option<BulkFile> {
    if entry.kind != rightkit_fsindex::EntryKind::File || entry.dataless {
        return None;
    }
    let rightkit_fsindex::EntrySize::Bytes { logical, allocated } = entry.size else {
        return None;
    };
    let created = entry.created?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let modified = entry.modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(BulkFile {
        dev: entry.dev,
        ino: entry.ino,
        logical,
        allocation: allocated,
        created,
        modified,
    })
}
