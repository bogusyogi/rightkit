// The FSEvents binding below (extern declarations, stream creation, serial dispatch queue,
// start/stop sequence) is adapted from rightkit-fsindex src/backend/macos.rs, itself adapted
// from fsearch (https://github.com/noahdunnagan/fsearch), Copyright (c) 2026 Noah Dunnagan,
// MIT License; see THIRD-PARTY-NOTICES. The flag-to-event mapping follows notify 8.2.0
// src/fsevent.rs (CC0 1.0).
//
//! macOS backend: one FSEvents stream on a serial dispatch queue. No crate thread: the callback
//! translates, excludes and enqueues. Event ids and the volume UUID feed a resumable cursor.
//!
//! Header facts this module relies on (FSEvents.h, macOS SDK):
//! * `FSEventsCopyUUIDForDevice(dev_t)` returns a retained `CFUUIDRef`, or NULL when the device
//!   has no event history (read-only volumes); a UUID that differs from a stored one means the
//!   event stream is different (purged, erased, or wrapped), so stored ids are void.
//! * `sinceWhen` supplies events "after the given event ID"; history arrives first, then one
//!   event flagged `HistoryDone`, then live events.
//! * With `kFSEventStreamCreateFlagNoDefer` the first event after a quiet period is delivered
//!   immediately and `latency` only spaces later ones, so it is set only for zero latency.

use std::ffi::{c_char, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use notify::event::{
    CreateKind, DataChange, Flag, MetadataKind, ModifyKind, RemoveKind, RenameMode,
};
use notify::{Event, EventKind};

use super::{BackendSink, Entry, Granularity, TypedOptions, WatchCursor};
use crate::WatchError;

// Create flags.
const CREATE_NO_DEFER: u32 = 0x2;
const CREATE_WATCH_ROOT: u32 = 0x4;
const CREATE_FILE_EVENTS: u32 = 0x10;

// Event flags (only the bits this module reads; unknown bits are ignored, never decoded).
const EVT_MUST_SCAN_SUBDIRS: u32 = 0x1;
const EVT_USER_DROPPED: u32 = 0x2;
const EVT_KERNEL_DROPPED: u32 = 0x4;
const EVT_IDS_WRAPPED: u32 = 0x8;
const EVT_HISTORY_DONE: u32 = 0x10;
const EVT_ROOT_CHANGED: u32 = 0x20;
const EVT_MOUNT: u32 = 0x40;
const EVT_UNMOUNT: u32 = 0x80;
const EVT_ITEM_CREATED: u32 = 0x100;
const EVT_ITEM_REMOVED: u32 = 0x200;
const EVT_ITEM_INODE_META_MOD: u32 = 0x400;
const EVT_ITEM_RENAMED: u32 = 0x800;
const EVT_ITEM_MODIFIED: u32 = 0x1000;
const EVT_ITEM_FINDER_INFO_MOD: u32 = 0x2000;
const EVT_ITEM_CHANGE_OWNER: u32 = 0x4000;
const EVT_ITEM_XATTR_MOD: u32 = 0x8000;
const EVT_ITEM_IS_FILE: u32 = 0x1_0000;
const EVT_ITEM_IS_DIR: u32 = 0x2_0000;

const RESCAN: u32 = EVT_MUST_SCAN_SUBDIRS | EVT_USER_DROPPED | EVT_KERNEL_DROPPED | EVT_IDS_WRAPPED;

/// `kFSEventStreamEventIdSinceNow`.
const SINCE_NOW: u64 = u64::MAX;

type Callback = extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);
type RetainCallback = extern "C" fn(*const c_void) -> *const c_void;
type ReleaseCallback = extern "C" fn(*const c_void);

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<RetainCallback>,
    release: Option<ReleaseCallback>,
    copy_description: *const c_void,
}

/// `CFUUIDBytes`: sixteen `UInt8` fields, returned by value.
#[repr(C)]
struct CfUuidBytes([u8; 16]);

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventStreamCreate(
        alloc: *const c_void,
        cb: Callback,
        ctx: *const Context,
        paths: *const c_void,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(s: *mut c_void, q: *mut c_void);
    fn FSEventStreamStart(s: *mut c_void) -> u8;
    #[allow(dead_code)]
    fn FSEventStreamFlushSync(s: *mut c_void);
    fn FSEventStreamStop(s: *mut c_void);
    fn FSEventStreamInvalidate(s: *mut c_void);
    fn FSEventStreamRelease(s: *mut c_void);
    fn FSEventsGetCurrentEventId() -> u64;
    fn FSEventsCopyUUIDForDevice(dev: i32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32)
        -> *const c_void;
    fn CFArrayCreate(
        alloc: *const c_void,
        vals: *const *const c_void,
        n: isize,
        cbs: *const c_void,
    ) -> *const c_void;
    fn CFRelease(o: *const c_void);
    fn CFUUIDGetUUIDBytes(uuid: *const c_void) -> CfUuidBytes;
    static kCFTypeArrayCallBacks: c_void;
}

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_release(o: *mut c_void);
}

/// UUID of the event stream that covers `path`'s volume; `None` when history is unavailable.
fn volume_uuid(path: &Path) -> Option<[u8; 16]> {
    // dev_t is int32_t on macOS.
    let dev = std::fs::metadata(path).ok()?.dev() as i32;
    // SAFETY: a plain call; a non-NULL result is ours to release (Copy rule) after reading it.
    unsafe {
        let uuid = FSEventsCopyUUIDForDevice(dev);
        if uuid.is_null() {
            return None;
        }
        let bytes = CFUUIDGetUUIDBytes(uuid).0;
        CFRelease(uuid);
        Some(bytes)
    }
}

pub(crate) fn current_cursor(path: &Path) -> Option<WatchCursor> {
    let path = path.canonicalize().ok()?;
    let volume = volume_uuid(&path)?;
    // SAFETY: no arguments, no preconditions.
    Some(WatchCursor::new(volume, unsafe {
        FSEventsGetCurrentEventId()
    }))
}

/// Maps one FSEvents flag word to notify events (path attached by the caller). `exists` is asked
/// only for rename events, to tell the old name (gone) from the new one (present).
fn events_for_flags(
    flags: u32,
    granularity: Granularity,
    exists: impl FnOnce() -> bool,
) -> Vec<Event> {
    let mut evs = Vec::new();
    if flags & RESCAN != 0 {
        let info = if flags & EVT_USER_DROPPED != 0 {
            "rescan: user dropped"
        } else if flags & EVT_KERNEL_DROPPED != 0 {
            "rescan: kernel dropped"
        } else if flags & EVT_IDS_WRAPPED != 0 {
            "rescan: event ids wrapped"
        } else {
            "rescan: must scan subdirs"
        };
        evs.push(
            Event::new(EventKind::Other)
                .set_flag(Flag::Rescan)
                .set_info(info),
        );
        return evs;
    }
    if granularity == Granularity::Directory {
        // Without the FileEvents flag every event names a directory that changed.
        evs.push(Event::new(EventKind::Any));
        return evs;
    }
    if flags & EVT_MOUNT != 0 {
        evs.push(Event::new(EventKind::Create(CreateKind::Other)).set_info("mount"));
    }
    if flags & EVT_UNMOUNT != 0 {
        evs.push(Event::new(EventKind::Remove(RemoveKind::Other)).set_info("unmount"));
    }
    if flags & EVT_ITEM_RENAMED != 0 {
        // FSEvents reports each side of a rename as its own event with no correlation id. The
        // old name no longer exists and the new one does; the translator pairs adjacent halves.
        // Other bits on a renamed path are subsumed by the rename itself.
        let present = exists();
        if present || flags & EVT_ITEM_REMOVED == 0 {
            let mode = if present {
                RenameMode::To
            } else {
                RenameMode::From
            };
            evs.push(Event::new(EventKind::Modify(ModifyKind::Name(mode))));
            return evs;
        }
    }
    let is_dir = flags & EVT_ITEM_IS_DIR != 0;
    let is_file = flags & EVT_ITEM_IS_FILE != 0;
    if flags & EVT_ITEM_CREATED != 0 {
        evs.push(Event::new(EventKind::Create(if is_dir {
            CreateKind::Folder
        } else if is_file {
            CreateKind::File
        } else {
            CreateKind::Any
        })));
    }
    if flags & EVT_ITEM_REMOVED != 0 {
        evs.push(Event::new(EventKind::Remove(if is_dir {
            RemoveKind::Folder
        } else if is_file {
            RemoveKind::File
        } else {
            RemoveKind::Any
        })));
    }
    if flags & EVT_ITEM_INODE_META_MOD != 0 {
        evs.push(Event::new(EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::Any,
        ))));
    }
    if flags & EVT_ITEM_FINDER_INFO_MOD != 0 {
        evs.push(
            Event::new(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Other)))
                .set_info("meta: finder info"),
        );
    }
    if flags & EVT_ITEM_CHANGE_OWNER != 0 {
        evs.push(Event::new(EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::Ownership,
        ))));
    }
    if flags & EVT_ITEM_XATTR_MOD != 0 {
        evs.push(Event::new(EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::Extended,
        ))));
    }
    if flags & EVT_ITEM_MODIFIED != 0 {
        evs.push(Event::new(EventKind::Modify(ModifyKind::Data(
            DataChange::Content,
        ))));
    }
    evs
}

struct RawEvent {
    path: PathBuf,
    flags: u32,
    id: u64,
}

struct State {
    sink: BackendSink,
    root: PathBuf,
    granularity: Granularity,
    /// The stream has ended (root changed, consumer gone, or the callback failed).
    dead: bool,
}

/// Callback context. Owned by an `Arc`; the stream holds one count through the retain/release
/// callbacks, so a late callback can never see freed state.
struct Shared {
    closed: AtomicBool,
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn handle(&self, events: &[RawEvent]) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut guard = self.lock();
        let State {
            sink,
            root,
            granularity,
            dead,
        } = &mut *guard;
        if *dead {
            return;
        }
        let mut max_id = 0;
        let mut history_done = false;
        for event in events {
            if event.flags & EVT_HISTORY_DONE != 0 {
                // The path of this sentinel is meaningless; the stream is live from here on.
                history_done = true;
                continue;
            }
            if event.flags & EVT_ROOT_CHANGED != 0 {
                // Event id zero, path is the watched root: the root moved or was removed.
                sink.event(
                    Err(
                        notify::Error::generic("watched root changed or was removed")
                            .add_path(root.clone()),
                    ),
                    None,
                );
                sink.finish();
                *dead = true;
                return;
            }
            max_id = max_id.max(event.id);
            let produced = events_for_flags(event.flags, *granularity, || {
                event.path.symlink_metadata().is_ok()
            });
            let count = produced.len();
            for (i, notify_event) in produced.into_iter().enumerate() {
                let id = (i + 1 == count && event.id != 0).then_some(event.id);
                sink.event(Ok(notify_event.add_path(event.path.clone())), id);
            }
        }
        // No timer exists to expire an unpaired rename half later: settle it with the batch.
        sink.flush((max_id != 0).then_some(max_id));
        let alive = sink.commit();
        if history_done {
            sink.queue().set_replaying(false);
        }
        if !alive {
            *dead = true;
        }
    }

    /// The callback panicked or hit an impossible state: end the stream visibly.
    fn fail(&self, message: &str) {
        let mut guard = self.lock();
        if guard.dead {
            return;
        }
        let root = guard.root.clone();
        guard
            .sink
            .event(Err(notify::Error::generic(message).add_path(root)), None);
        guard.sink.finish();
        guard.dead = true;
    }
}

extern "C" fn retain_shared(info: *const c_void) -> *const c_void {
    // SAFETY: `info` is `Arc::as_ptr` of a live `Shared` whose count the creator holds.
    unsafe { Arc::increment_strong_count(info as *const Shared) };
    info
}

extern "C" fn release_shared(info: *const c_void) {
    // SAFETY: pairs with `retain_shared`.
    unsafe { Arc::decrement_strong_count(info as *const Shared) };
}

/// Runs on the stream's serial dispatch queue. Must never unwind into C.
extern "C" fn on_events(
    _stream: *mut c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    if info.is_null() || paths.is_null() || flags.is_null() || ids.is_null() || count == 0 {
        return;
    }
    // SAFETY: `info` stays valid while the stream lives (retain/release callbacks).
    let shared = unsafe { &*(info as *const Shared) };
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: FSEvents passes `count` C-string pointers (no UseCFTypes), flags and ids.
        let events: Vec<RawEvent> = unsafe {
            let paths = std::slice::from_raw_parts(paths as *const *const c_char, count);
            let flags = std::slice::from_raw_parts(flags, count);
            let ids = std::slice::from_raw_parts(ids, count);
            paths
                .iter()
                .zip(flags.iter().zip(ids))
                .filter(|(path, _)| !path.is_null())
                .map(|(path, (flags, id))| RawEvent {
                    // Raw bytes, so a non-UTF-8 name survives intact instead of panicking.
                    path: PathBuf::from(std::ffi::OsStr::from_bytes(
                        CStr::from_ptr(*path).to_bytes(),
                    )),
                    flags: *flags,
                    id: *id,
                })
                .collect()
        };
        shared.handle(&events);
    }));
    if outcome.is_err() {
        // A second panic here would abort; fail() only locks and pushes.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            shared.fail("panic in the FSEvents callback")
        }));
    }
}

/// A running FSEvents stream. Dropping it stops the stream and waits out any callback in flight.
pub(crate) struct Handle {
    stream: *mut c_void,
    dispatch_queue: *mut c_void,
    shared: Arc<Shared>,
    volume: Option<[u8; 16]>,
}

// The stream is started once, then only stopped; its callback state is behind a mutex.
unsafe impl Send for Handle {}

impl Handle {
    pub(crate) fn volume(&self) -> Option<[u8; 16]> {
        self.volume
    }

    /// Blocks until FSEvents has delivered everything it already buffered.
    #[allow(dead_code)]
    pub(crate) fn flush(&self) {
        // SAFETY: the stream is live and started for the lifetime of the handle.
        unsafe { FSEventStreamFlushSync(self.stream) };
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Order follows rightkit-fsindex: stop, invalidate, release the stream, then the queue.
        // Unlike fsindex nothing is leaked: the stream's release callback frees our context, and
        // `closed` plus the state lock fence off a callback that was already running.
        self.shared.closed.store(true, Ordering::Release);
        // SAFETY: each call is made once, on a stream created and started by `create`.
        unsafe {
            FSEventStreamStop(self.stream);
            drop(self.shared.lock());
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.dispatch_queue);
        }
    }
}

/// Creates and starts a stream over `roots`. `flags` are FSEvents create flags.
fn create(
    roots: &[PathBuf],
    since: u64,
    latency: f64,
    flags: u32,
    shared: &Arc<Shared>,
) -> Result<(*mut c_void, *mut c_void), WatchError> {
    let fail = |message: &str| WatchError::Backend {
        message: message.to_string(),
        paths: roots.to_vec(),
    };
    let names = roots
        .iter()
        .map(|root| CString::new(root.as_os_str().as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| fail("watch root contains a NUL byte"))?;
    // SAFETY: CF objects are created, checked, and released here; the context outlives the call
    // and its info is kept alive by the retain callback.
    unsafe {
        let strings: Vec<*const c_void> = names
            .iter()
            // 0x0800_0100 = kCFStringEncodingUTF8
            .map(|name| CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), 0x0800_0100))
            .collect();
        let release_strings = |strings: &[*const c_void]| {
            for string in strings.iter().filter(|s| !s.is_null()) {
                CFRelease(*string);
            }
        };
        if strings.iter().any(|s| s.is_null()) {
            release_strings(&strings);
            return Err(fail("watch root is not valid UTF-8"));
        }
        let array = CFArrayCreate(
            std::ptr::null(),
            strings.as_ptr(),
            strings.len() as isize,
            &kCFTypeArrayCallBacks as *const c_void,
        );
        release_strings(&strings);
        if array.is_null() {
            return Err(fail("CFArrayCreate failed"));
        }
        let context = Context {
            version: 0,
            info: Arc::as_ptr(shared) as *mut c_void,
            retain: Some(retain_shared),
            release: Some(release_shared),
            copy_description: std::ptr::null(),
        };
        let stream = FSEventStreamCreate(
            std::ptr::null(),
            on_events,
            &context,
            array,
            since,
            latency,
            flags,
        );
        CFRelease(array);
        if stream.is_null() {
            return Err(fail("FSEventStreamCreate failed"));
        }
        let dispatch_queue =
            dispatch_queue_create(c"rightkit.watch.fsevents".as_ptr(), std::ptr::null());
        FSEventStreamSetDispatchQueue(stream, dispatch_queue);
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            dispatch_release(dispatch_queue);
            return Err(fail("FSEventStreamStart failed"));
        }
        Ok((stream, dispatch_queue))
    }
}

pub(crate) fn start(
    root: &Path,
    options: &TypedOptions,
    sink: BackendSink,
) -> Result<Handle, WatchError> {
    let queue = sink.queue().clone();
    let volume = volume_uuid(root);
    // SAFETY: no arguments, no preconditions.
    let current = unsafe { FSEventsGetCurrentEventId() };

    let mut since = SINCE_NOW;
    let mut replaying = false;
    let mut cursor_id = current;
    if let Some(cursor) = options.replay_cursor() {
        // Honoured only for the same event stream, and never from the future.
        if volume == Some(cursor.volume()) && cursor.event_id() <= current {
            since = cursor.event_id();
            cursor_id = since;
            replaying = true;
        } else {
            queue.push_all(
                vec![Entry::new(super::unusable_cursor_overflow(), None)],
                false,
            );
        }
    }
    if volume.is_some() {
        queue.seed_cursor(cursor_id);
    }
    queue.set_replaying(replaying);

    let granularity = options.granularity_get();
    let latency = options.latency_get();
    let mut flags = CREATE_WATCH_ROOT;
    if latency.is_zero() {
        flags |= CREATE_NO_DEFER;
    }
    if granularity == Granularity::File {
        flags |= CREATE_FILE_EVENTS;
    }
    let shared = Arc::new(Shared {
        closed: AtomicBool::new(false),
        state: Mutex::new(State {
            sink,
            root: root.to_path_buf(),
            granularity,
            dead: false,
        }),
    });
    let (stream, dispatch_queue) = create(
        std::slice::from_ref(&root.to_path_buf()),
        since,
        latency.as_secs_f64(),
        flags,
        &shared,
    )
    .inspect_err(|_| queue.set_replaying(false))?;
    Ok(Handle {
        stream,
        dispatch_queue,
        shared,
        volume,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(flags: u32, granularity: Granularity, exists: bool) -> Vec<EventKind> {
        events_for_flags(flags, granularity, || exists)
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn unknown_flag_bits_are_ignored_not_decoded() {
        let flags = EVT_ITEM_CREATED | EVT_ITEM_IS_FILE | 0x8000_0000 | 0x0800_0000;
        assert_eq!(
            kinds(flags, Granularity::File, true),
            vec![EventKind::Create(CreateKind::File)]
        );
    }

    #[test]
    fn dropped_events_and_wrapped_ids_become_rescans() {
        for flag in [
            EVT_MUST_SCAN_SUBDIRS,
            EVT_USER_DROPPED,
            EVT_KERNEL_DROPPED,
            EVT_IDS_WRAPPED,
        ] {
            let evs = events_for_flags(flag, Granularity::File, || true);
            assert_eq!(evs.len(), 1);
            assert!(evs[0].need_rescan());
        }
    }

    #[test]
    fn renames_split_by_existence_and_directory_mode_is_one_any_event() {
        let renamed = EVT_ITEM_RENAMED | EVT_ITEM_IS_FILE | EVT_ITEM_MODIFIED;
        assert_eq!(
            kinds(renamed, Granularity::File, true),
            vec![EventKind::Modify(ModifyKind::Name(RenameMode::To))]
        );
        assert_eq!(
            kinds(renamed, Granularity::File, false),
            vec![EventKind::Modify(ModifyKind::Name(RenameMode::From))]
        );
        // Renamed then deleted before the callback ran: a removal, not a rename half.
        assert_eq!(
            kinds(renamed | EVT_ITEM_REMOVED, Granularity::File, false),
            vec![
                EventKind::Remove(RemoveKind::File),
                EventKind::Modify(ModifyKind::Data(DataChange::Content))
            ]
        );
        assert_eq!(kinds(0, Granularity::Directory, true), vec![EventKind::Any]);
        assert_eq!(
            kinds(EVT_ITEM_CREATED, Granularity::Directory, true),
            vec![EventKind::Any]
        );
    }

    /// The stream must hand back every count it takes: no leaked callback state.
    #[test]
    fn dropping_a_handle_releases_the_callback_context() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let options = TypedOptions::new();
        let queue = Arc::new(super::super::BoundedQueue::new(8));
        let sink = BackendSink::new(queue, &options, false);
        let handle = start(&root, &options, sink).unwrap();
        let shared = handle.shared.clone();
        assert!(
            Arc::strong_count(&shared) >= 3,
            "handle + stream + this clone"
        );
        drop(handle);
        for _ in 0..100 {
            if Arc::strong_count(&shared) == 1 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "callback context leaked: {} counts left",
            Arc::strong_count(&shared)
        );
    }
}
