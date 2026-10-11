//! Public FSEvents watcher: typed events, event-id resume, and the last id to persist.
//!
//! The default is directory granularity, the same feed the index uses. With
//! [`WatchFlags::file_events`] each event also says what happened to the item
//! (created, removed, renamed, modified, metadata). That costs more per change.
//!
//! The watcher delivers events over an unbounded channel. Drain it promptly; the
//! stream never blocks on a slow consumer.

use super::{Sink, Stream, SINCE_NOW};
use crate::backend::{
    Event, EV_HISTORY_DONE, EV_KERNEL_DROPPED, EV_MOUNT, EV_MUST_SCAN_SUBDIRS, EV_ROOT_CHANGED,
    EV_UNMOUNT, EV_USER_DROPPED,
};
use crate::error::Result;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const EV_IDS_WRAPPED: u32 = 0x8;
const EV_ITEM_CREATED: u32 = 0x100;
const EV_ITEM_REMOVED: u32 = 0x200;
const EV_ITEM_INODE_META: u32 = 0x400;
const EV_ITEM_RENAMED: u32 = 0x800;
const EV_ITEM_MODIFIED: u32 = 0x1000;
const EV_ITEM_FINDER_INFO: u32 = 0x2000;
const EV_ITEM_CHANGE_OWNER: u32 = 0x4000;
const EV_ITEM_XATTR_MOD: u32 = 0x8000;
const EV_ITEM_IS_FILE: u32 = 0x10000;
const EV_ITEM_IS_DIR: u32 = 0x20000;
const EV_ITEM_IS_SYMLINK: u32 = 0x40000;

const FLAG_NO_DEFER: u32 = 0x2;
const FLAG_WATCH_ROOT: u32 = 0x4;
const FLAG_IGNORE_SELF: u32 = 0x8;
const FLAG_FILE_EVENTS: u32 = 0x10;

/// What happened to one item, reported with [`WatchFlags::file_events`].
///
/// All fields are false in directory-granularity mode.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemFlags {
    pub created: bool,
    pub removed: bool,
    pub renamed: bool,
    pub modified: bool,
    /// Inode, Finder info, owner, or extended-attribute change without a content change.
    pub metadata: bool,
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// A typed change-feed event. Each carries the FSEvents event id it came from.
///
/// Event ids are monotonic within one stream. Persist [`FsEventsWatcher::last_event_id`]
/// only after the events up to it have been applied, then pass it as
/// [`WatchOptions::since_event_id`] to resume.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsEvent {
    /// `path` changed. With directory granularity `item` is all false: something under
    /// `path` changed, and the caller must re-read it.
    Changed {
        path: PathBuf,
        id: u64,
        item: ItemFlags,
    },
    /// FSEvents asks for the whole subtree under `path` to be rescanned.
    MustScanSubDirs { path: PathBuf, id: u64 },
    /// A watched root was renamed, moved, deleted, or unmounted. Events for that root can no
    /// longer be trusted; re-crawl it and watch again.
    RootChanged { path: PathBuf, id: u64 },
    /// Events were lost, so the caller's state may be stale. `must_rescan` is true when
    /// FSEvents also flagged the affected subtree for rescan. Rescan either way.
    Dropped { must_rescan: bool, id: u64 },
    /// Event ids wrapped. Ids persisted earlier are not comparable with new ones.
    IdsWrapped { id: u64 },
    /// Replay from `since_event_id` is complete; the events that follow are live.
    HistoryDone { id: u64 },
    /// A volume was mounted at `path`.
    Mount { path: PathBuf, id: u64 },
    /// A volume was unmounted from `path`.
    Unmount { path: PathBuf, id: u64 },
}

impl FsEvent {
    /// The FSEvents event id this event came from.
    pub fn id(&self) -> u64 {
        match self {
            FsEvent::Changed { id, .. }
            | FsEvent::MustScanSubDirs { id, .. }
            | FsEvent::RootChanged { id, .. }
            | FsEvent::Dropped { id, .. }
            | FsEvent::IdsWrapped { id }
            | FsEvent::HistoryDone { id }
            | FsEvent::Mount { id, .. }
            | FsEvent::Unmount { id, .. } => *id,
        }
    }
}

/// FSEvents stream flags. The defaults match what the index uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchFlags {
    /// Deliver without deferring to the end of the latency window. Default true.
    pub no_defer: bool,
    /// Report changes to the watched roots themselves (`RootChanged`). Default true.
    pub watch_root: bool,
    /// Do not report this process's own writes. Default false: the host's own renames
    /// and moves must reach the watcher.
    pub ignore_self: bool,
    /// Per-item events instead of per-directory events. Default false.
    pub file_events: bool,
}

impl Default for WatchFlags {
    fn default() -> Self {
        WatchFlags {
            no_defer: true,
            watch_root: true,
            ignore_self: false,
            file_events: false,
        }
    }
}

impl WatchFlags {
    fn bits(self) -> u32 {
        let mut bits = 0;
        if self.no_defer {
            bits |= FLAG_NO_DEFER;
        }
        if self.watch_root {
            bits |= FLAG_WATCH_ROOT;
        }
        if self.ignore_self {
            bits |= FLAG_IGNORE_SELF;
        }
        if self.file_events {
            bits |= FLAG_FILE_EVENTS;
        }
        bits
    }
}

/// Options for [`FsEventsWatcher::start`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchOptions {
    /// Replay changes after this event id, then continue live. `None`: live from now.
    /// A `since` older than FSEvents' retained history replays what is kept and gives no
    /// signal for the gap.
    pub since_event_id: Option<u64>,
    /// How long FSEvents may batch events before delivery. Default 50 ms.
    pub latency: Duration,
    pub flags: WatchFlags,
}

impl Default for WatchOptions {
    fn default() -> Self {
        WatchOptions {
            since_event_id: None,
            latency: Duration::from_millis(50),
            flags: WatchFlags::default(),
        }
    }
}

struct Shared {
    /// Taken on drop, which disconnects the receiver. A callback still in flight finds `None`.
    tx: Mutex<Option<Sender<FsEvent>>>,
    /// Highest event id handed to the receiver, or the starting id before any event.
    last: AtomicU64,
}

/// A running FSEvents stream. Dropping it stops the stream and disconnects the receiver.
pub struct FsEventsWatcher {
    stream: Option<Stream>,
    shared: Arc<Shared>,
}

impl FsEventsWatcher {
    /// Starts watching `roots` and returns the watcher with the receiver for its events.
    ///
    /// Fails if `roots` is empty or a root contains a NUL byte. A root that does not exist
    /// is not an error here; FSEvents reports it as a `RootChanged` event.
    pub fn start(
        roots: &[PathBuf],
        opts: WatchOptions,
    ) -> Result<(FsEventsWatcher, Receiver<FsEvent>)> {
        let start_id = match opts.since_event_id {
            Some(id) => id,
            None => Self::current_event_id(),
        };
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            tx: Mutex::new(Some(tx)),
            last: AtomicU64::new(start_id),
        });
        let sink_shared = Arc::clone(&shared);
        let sink: Sink = Box::new(move |batch: Vec<Event>| deliver(&sink_shared, batch));
        let stream = Stream::start(
            roots,
            opts.since_event_id.unwrap_or(SINCE_NOW),
            opts.latency.as_secs_f64(),
            opts.flags.bits(),
            sink,
        )?;
        Ok((
            FsEventsWatcher {
                stream: Some(stream),
                shared,
            },
            rx,
        ))
    }

    /// The highest event id this watcher has delivered, or the starting id before any event.
    ///
    /// Delivery means the event is queued on the receiver, not that the consumer has
    /// applied it. Persist this only after applying the events up to it.
    pub fn last_event_id(&self) -> u64 {
        self.shared.last.load(Ordering::SeqCst)
    }

    /// The FSEvents position now. Read it before a scan: a watch started with this as
    /// `since_event_id` replays every change made during the scan.
    pub fn current_event_id() -> u64 {
        unsafe { super::FSEventsGetCurrentEventId() }
    }
}

impl Drop for FsEventsWatcher {
    fn drop(&mut self) {
        // Stop the stream first, then disconnect the receiver.
        drop(self.stream.take());
        let mut tx = self.shared.tx.lock().unwrap_or_else(|e| e.into_inner());
        tx.take();
    }
}

fn deliver(shared: &Shared, batch: Vec<Event>) {
    let guard = shared.tx.lock().unwrap_or_else(|e| e.into_inner());
    let Some(tx) = guard.as_ref() else {
        return;
    };
    for ev in batch {
        // History sentinels do not advance the persisted position.
        if ev.id != 0 && ev.flags & EV_HISTORY_DONE == 0 {
            shared.last.fetch_max(ev.id, Ordering::SeqCst);
        }
        for event in typed(&ev) {
            if tx.send(event).is_err() {
                return;
            }
        }
    }
}

/// Maps one raw FSEvents record to typed events. A record can carry several flags and
/// yields several events; a record with none of them is a plain directory change.
fn typed(ev: &Event) -> Vec<FsEvent> {
    let flags = ev.flags;
    let id = ev.id;
    if flags & EV_HISTORY_DONE != 0 {
        return vec![FsEvent::HistoryDone { id }];
    }
    let path = path_of(&ev.path);
    let mut out = Vec::new();
    if flags & (EV_USER_DROPPED | EV_KERNEL_DROPPED) != 0 {
        out.push(FsEvent::Dropped {
            must_rescan: flags & EV_MUST_SCAN_SUBDIRS != 0,
            id,
        });
    } else if flags & EV_MUST_SCAN_SUBDIRS != 0 {
        out.push(FsEvent::MustScanSubDirs {
            path: path.clone(),
            id,
        });
    }
    if flags & EV_IDS_WRAPPED != 0 {
        out.push(FsEvent::IdsWrapped { id });
    }
    if flags & EV_ROOT_CHANGED != 0 {
        out.push(FsEvent::RootChanged {
            path: path.clone(),
            id,
        });
    }
    if flags & EV_MOUNT != 0 {
        out.push(FsEvent::Mount {
            path: path.clone(),
            id,
        });
    }
    if flags & EV_UNMOUNT != 0 {
        out.push(FsEvent::Unmount {
            path: path.clone(),
            id,
        });
    }
    let item = item_flags(flags);
    if out.is_empty() || item != ItemFlags::default() {
        out.push(FsEvent::Changed { path, id, item });
    }
    out
}

fn item_flags(f: u32) -> ItemFlags {
    ItemFlags {
        created: f & EV_ITEM_CREATED != 0,
        removed: f & EV_ITEM_REMOVED != 0,
        renamed: f & EV_ITEM_RENAMED != 0,
        modified: f & EV_ITEM_MODIFIED != 0,
        metadata: f
            & (EV_ITEM_INODE_META | EV_ITEM_FINDER_INFO | EV_ITEM_CHANGE_OWNER | EV_ITEM_XATTR_MOD)
            != 0,
        is_file: f & EV_ITEM_IS_FILE != 0,
        is_dir: f & EV_ITEM_IS_DIR != 0,
        is_symlink: f & EV_ITEM_IS_SYMLINK != 0,
    }
}

/// The path as FSEvents reported it, without trailing slashes (the root keeps its `/`).
fn path_of(bytes: &[u8]) -> PathBuf {
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }
    PathBuf::from(OsStr::from_bytes(&bytes[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(path: &str, flags: u32, id: u64) -> Event {
        Event {
            path: path.as_bytes().to_vec(),
            flags,
            id,
        }
    }

    #[test]
    fn plain_directory_event_is_a_changed_event_with_no_item_flags() {
        let out = typed(&raw("/data/dir/", 0, 10));
        assert_eq!(
            out,
            vec![FsEvent::Changed {
                path: PathBuf::from("/data/dir"),
                id: 10,
                item: ItemFlags::default(),
            }]
        );
    }

    #[test]
    fn item_flags_are_read_for_file_events() {
        let out = typed(&raw("/data/a.txt", EV_ITEM_CREATED | EV_ITEM_IS_FILE, 11));
        match out.as_slice() {
            [FsEvent::Changed { path, id, item }] => {
                assert_eq!(path, &PathBuf::from("/data/a.txt"));
                assert_eq!(*id, 11);
                assert!(item.created && item.is_file && !item.modified && !item.removed);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn drop_with_must_scan_is_one_dropped_event_that_asks_for_rescan() {
        let out = typed(&raw("/", EV_USER_DROPPED | EV_MUST_SCAN_SUBDIRS, 12));
        assert_eq!(
            out,
            vec![FsEvent::Dropped {
                must_rescan: true,
                id: 12
            }]
        );
    }

    #[test]
    fn drop_without_must_scan_reports_loss_without_rescan_request() {
        let out = typed(&raw("/", EV_KERNEL_DROPPED, 13));
        assert_eq!(
            out,
            vec![FsEvent::Dropped {
                must_rescan: false,
                id: 13
            }]
        );
    }

    #[test]
    fn root_change_is_typed_and_is_not_a_plain_change() {
        let out = typed(&raw("/Volumes/x", EV_ROOT_CHANGED, 14));
        assert_eq!(
            out,
            vec![FsEvent::RootChanged {
                path: PathBuf::from("/Volumes/x"),
                id: 14
            }]
        );
    }

    #[test]
    fn history_done_is_its_own_event() {
        assert_eq!(
            typed(&raw("", EV_HISTORY_DONE, 15)),
            vec![FsEvent::HistoryDone { id: 15 }]
        );
    }

    #[test]
    fn must_scan_subdirs_is_typed() {
        let out = typed(&raw("/data/tree", EV_MUST_SCAN_SUBDIRS, 16));
        assert_eq!(
            out,
            vec![FsEvent::MustScanSubDirs {
                path: PathBuf::from("/data/tree"),
                id: 16
            }]
        );
    }

    #[test]
    fn watch_flags_map_to_fsevents_bits() {
        assert_eq!(
            WatchFlags::default().bits(),
            FLAG_NO_DEFER | FLAG_WATCH_ROOT
        );
        let all = WatchFlags {
            no_defer: true,
            watch_root: true,
            ignore_self: true,
            file_events: true,
        };
        assert_eq!(all.bits(), 0x2 | 0x4 | 0x8 | 0x10);
    }

    #[test]
    fn id_accessor_matches_every_variant() {
        assert_eq!(FsEvent::IdsWrapped { id: 3 }.id(), 3);
        assert_eq!(
            FsEvent::Unmount {
                path: PathBuf::from("/x"),
                id: 4
            }
            .id(),
            4
        );
    }
}
