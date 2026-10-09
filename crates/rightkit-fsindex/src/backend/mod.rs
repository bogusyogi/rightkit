//! Platform backend: enumeration plus change feed.
//!
//! macOS: getattrlistbulk crawl + FSEvents (`macos`). Windows: an NTFS MFT
//! enumeration with a USN journal feed is planned (see the README); only the
//! `UnsupportedBackend` stub exists today. Everything above this trait is
//! platform-neutral.

use crate::error::Result;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::Arc;

#[cfg(target_os = "macos")]
pub mod macos;
pub mod windows_stub;

pub const NONE: u32 = u32::MAX;

pub const KIND_FILE: u8 = 0;
pub const KIND_DIR: u8 = 1;
pub const KIND_LINK: u8 = 2;
pub const KIND_OTHER: u8 = 3;

pub const FLAG_PACKAGE: u8 = 1;
/// Hard link whose size is owned (counted) by another link of the same inode.
pub const FLAG_SECONDARY_LINK: u8 = 2;
/// Directory deliberately not indexed (protected store, consent-gated, configured exclusion).
pub const FLAG_EXCLUDED: u8 = 4;
pub const FLAG_HIDDEN: u8 = 8;
/// Mount point not descended into.
pub const FLAG_MOUNT: u8 = 16;
/// Directory that could not be read (permissions, Full Disk Access).
pub const FLAG_UNREADABLE: u8 = 32;

/// Per-entry metadata as reported by the platform.
#[derive(Clone, Copy, Debug, Default)]
pub struct EntryData {
    pub kind: u8,
    pub flags: u8,
    pub nlink: u32,
    pub mtime: i64,
    pub dev: u32,
    pub fileid: u64,
    /// data fork + resource fork length
    pub logical: u64,
    /// total allocated bytes (all forks)
    pub alloc: u64,
    /// bytes freed if only this entry were deleted (APFS PRIVATESIZE); equals `alloc` when unknown
    pub private: u64,
    /// APFS clone id; 0 when none
    pub cloneid: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct RawEnt {
    pub name_off: u32,
    pub name_len: u16,
    pub data: EntryData,
    /// Id of the listing of this directory in the same crawl, or NONE.
    pub child: u32,
}

#[derive(Debug, Default)]
pub struct Listing {
    pub id: u32,
    pub names: Vec<u8>,
    pub ents: Vec<RawEnt>,
    pub error: Option<std::io::ErrorKind>,
}

impl Listing {
    pub fn name(&self, e: &RawEnt) -> &[u8] {
        &self.names[e.name_off as usize..e.name_off as usize + e.name_len as usize]
    }
}

/// Path prefixes never descended into.
#[derive(Clone, Debug, Default)]
pub struct Excludes {
    pub prefixes: Vec<Vec<u8>>,
}

impl Excludes {
    pub fn matches(&self, path: &[u8]) -> bool {
        self.prefixes
            .iter()
            .any(|s| path.starts_with(s) && (path.len() == s.len() || path[s.len()] == b'/'))
    }
}

pub const EV_MUST_SCAN_SUBDIRS: u32 = 0x1;
pub const EV_USER_DROPPED: u32 = 0x2;
pub const EV_KERNEL_DROPPED: u32 = 0x4;
pub const EV_HISTORY_DONE: u32 = 0x10;
pub const EV_ROOT_CHANGED: u32 = 0x20;
pub const EV_MOUNT: u32 = 0x40;
pub const EV_UNMOUNT: u32 = 0x80;

#[derive(Clone, Debug)]
pub struct Event {
    pub path: Vec<u8>,
    pub flags: u32,
    pub id: u64,
}

pub enum WatchMsg {
    Events(Vec<Event>),
    /// Worker acknowledges after everything queued before it was applied.
    Flush(Sender<()>),
    Stop,
}

pub trait Watcher: Send {
    /// Push out events the platform has not delivered yet (blocking).
    fn flush(&self);
}

pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Err(Unsupported) on platforms without an implementation.
    fn check_supported(&self) -> Result<()>;
    /// Metadata for one path without following symlinks.
    fn stat(&self, path: &Path) -> std::io::Result<EntryData>;
    /// Recursive crawl; listing id 0 is `root` itself.
    fn crawl(&self, root: &Path, excludes: &Excludes, threads: usize) -> Vec<Listing>;
    /// One directory, no recursion; subdirectories have `child == NONE`.
    fn list_dir(&self, dir: &Path, excludes: &Excludes) -> std::io::Result<Listing>;
    /// Start the change feed. `since`: resume from this event id (None: from now).
    fn watch(
        &self,
        roots: &[std::path::PathBuf],
        since: Option<u64>,
        latency: std::time::Duration,
        tx: Sender<WatchMsg>,
    ) -> Result<Box<dyn Watcher>>;
    /// Current change-feed position.
    fn current_event_id(&self) -> u64;
    /// Whether the process can read protected stores (Full Disk Access on macOS).
    fn has_full_disk_access(&self) -> bool;
}

pub fn default_backend() -> Arc<dyn Backend> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacBackend)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(windows_stub::UnsupportedBackend)
    }
}
