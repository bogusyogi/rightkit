//! Windows native directory reader: `ReadDirectoryChangesW` with overlapped IO.
//!
//! Why this exists (0.4.1): notify 8.2.0's Windows backend also uses `ReadDirectoryChangesW`, but
//! it allocates a fixed 16 KiB buffer per watch and its completion routine ignores the byte count.
//! Windows signals a buffer overflow as a *successful* completion with 0 bytes (or, on some
//! redirectors, `ERROR_NOTIFY_ENUM_DIR`), so notify never reports one and no `Flag::Rescan` event
//! ever reaches this crate. [`DirWatcher`] reads the directory itself and turns that signal into a
//! `Flag::Rescan` event, which the existing delivery path reports as `WatchEvent::Overflow`
//! (`DirBatch::lost`).
//!
//! It produces `notify::Event` values into the same channel notify's callback would use, so
//! debounce, quiet window, max hold, rename pairing, reconcile and gap rescans are the existing
//! `options::run_callback` path, unchanged. Event kinds match notify's Windows backend (added to
//! `Create(Any)`, removed to `Remove(Any)`, modified to `Modify(Any)`, rename halves to
//! `Modify(Name(From|To))`), paths are the watch root joined with the reported relative name.
//!
//! # Safety argument
//!
//! * The OS writes into the buffer and the `OVERLAPPED` until the request completes, so both live
//!   on the heap (`Vec<u32>`, `Box<Overlapped>`) in [`Resources`], whose addresses do not change
//!   when the owning [`Reader`] moves between threads. `Vec<u32>` makes the buffer DWORD-aligned
//!   as `FILE_NOTIFY_INFORMATION` requires. While a request is pending nothing reads or writes the
//!   buffer; it is read only after `GetOverlappedResult` reports completion.
//! * [`Reader`] tracks whether a request is pending. Its `Drop` is the single place resources are
//!   released: if a request is pending it calls `CancelIoEx` and then `GetOverlappedResult` with
//!   `bWait = TRUE`, which returns only once the request has completed (cancelled or finished),
//!   and only then frees the buffer, the `OVERLAPPED`, the event and the directory handle. If
//!   cancel cannot be confirmed (it fails with anything but `ERROR_NOT_FOUND`, meaning the
//!   request already completed), the resources are leaked on purpose rather than freed under a
//!   live request.
//! * Stop: [`DirWatcher::drop`] signals a stop event and joins the reader thread. The thread waits
//!   on `[io event, stop event]`, so it wakes for either; on stop (or a failed wait) it returns,
//!   dropping the `Reader`, which cancels and waits as above. No handle outlives the join.
//! * The first request is armed synchronously in `start`, so changes after `start` returns are
//!   captured and an open or arm failure is returned to the caller (which falls back to notify).
//!
//! The typed `watch_with` stub (`Handle`, `start`) below is unchanged: `WatchBackend::Native` on
//! the typed stream still returns [`WatchError::Backend`], and `Auto` uses notify.

use std::ffi::{c_void, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use notify::event::{CreateKind, Flag, ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind};

use super::{BackendSink, TypedOptions, WatchCursor};
use crate::WatchError;

/// Win32 `OVERLAPPED`; the kernel reads and writes every field.
#[allow(dead_code)]
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: *mut c_void,
}

#[allow(clippy::too_many_arguments)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn ReadDirectoryChangesW(
        directory: *mut c_void,
        buffer: *mut c_void,
        buffer_len: u32,
        watch_subtree: i32,
        notify_filter: u32,
        bytes_returned: *mut u32,
        overlapped: *mut Overlapped,
        completion_routine: *mut c_void,
    ) -> i32;
    fn GetOverlappedResult(
        file: *mut c_void,
        overlapped: *mut Overlapped,
        transferred: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(file: *mut c_void, overlapped: *mut Overlapped) -> i32;
    fn CreateEventW(
        attributes: *mut c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> *mut c_void;
    fn SetEvent(event: *mut c_void) -> i32;
    fn ResetEvent(event: *mut c_void) -> i32;
    fn WaitForMultipleObjects(
        count: u32,
        handles: *const *mut c_void,
        wait_all: i32,
        milliseconds: u32,
    ) -> u32;
}

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_SHARE_ALL: u32 = 0x0000_0007; // READ | WRITE | DELETE
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

// FILE_NOTIFY_CHANGE_{FILE_NAME,DIR_NAME,ATTRIBUTES,SIZE,LAST_WRITE,CREATION,SECURITY}: the same
// set notify's Windows backend subscribes to.
const NOTIFY_FILTER: u32 = 0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x40 | 0x100;

const FILE_ACTION_ADDED: u32 = 1;
const FILE_ACTION_REMOVED: u32 = 2;
const FILE_ACTION_MODIFIED: u32 = 3;
const FILE_ACTION_RENAMED_OLD_NAME: u32 = 4;
const FILE_ACTION_RENAMED_NEW_NAME: u32 = 5;

const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_OPERATION_ABORTED: u32 = 995;
const ERROR_IO_INCOMPLETE: u32 = 996;
const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;

const WAIT_OBJECT_0: u32 = 0;
const INFINITE: u32 = 0xFFFF_FFFF;

/// Buffer for each request: 64 KiB, the most `ReadDirectoryChangesW` accepts on a network share.
const NATIVE_BUFFER_BYTES: u32 = 64 * 1024;

/// Size of the fixed part of a `FILE_NOTIFY_INFORMATION` record: `NextEntryOffset`, `Action` and
/// `FileNameLength` (each a `u32`), followed by `FileNameLength` bytes of UTF-16.
const RECORD_HEADER: usize = 12;

/// One parsed `FILE_NOTIFY_INFORMATION` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawChange {
    pub(crate) action: u32,
    /// Relative to the watched directory.
    pub(crate) name: OsString,
}

fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
    let bytes = buf.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// Parses a `ReadDirectoryChangesW` buffer (`bytes` is the completed length). Records are chained
/// by `NextEntryOffset` (0 ends the chain). A truncated or inconsistent record ends parsing and
/// keeps what was read before it: this never reads out of bounds and always terminates, because
/// every step advances by at least `RECORD_HEADER` bytes.
pub(crate) fn parse_notify_buffer(buf: &[u8]) -> Vec<RawChange> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    loop {
        let (Some(next), Some(action), Some(len)) = (
            read_u32(buf, offset),
            offset.checked_add(4).and_then(|o| read_u32(buf, o)),
            offset.checked_add(8).and_then(|o| read_u32(buf, o)),
        ) else {
            break;
        };
        let (next, len) = (next as usize, len as usize);
        let name_start = offset + RECORD_HEADER;
        let Some(name_end) = name_start.checked_add(len).filter(|end| *end <= buf.len()) else {
            break;
        };
        if len % 2 != 0 {
            break;
        }
        let units: Vec<u16> = buf[name_start..name_end]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        out.push(RawChange {
            action,
            name: OsString::from_wide(&units),
        });
        if next == 0 || next < RECORD_HEADER + len {
            break;
        }
        match offset.checked_add(next) {
            Some(following) => offset = following,
            None => break,
        }
    }
    out
}

/// The notify event for one record, or `None` for an action this crate does not know.
pub(crate) fn event_for(root: &Path, change: &RawChange) -> Option<Event> {
    let kind = match change.action {
        FILE_ACTION_ADDED => EventKind::Create(CreateKind::Any),
        FILE_ACTION_REMOVED => EventKind::Remove(RemoveKind::Any),
        FILE_ACTION_MODIFIED => EventKind::Modify(ModifyKind::Any),
        FILE_ACTION_RENAMED_OLD_NAME => EventKind::Modify(ModifyKind::Name(RenameMode::From)),
        FILE_ACTION_RENAMED_NEW_NAME => EventKind::Modify(ModifyKind::Name(RenameMode::To)),
        _ => return None,
    };
    let path = if change.name.is_empty() {
        root.to_path_buf()
    } else {
        root.join(&change.name)
    };
    Some(Event::new(kind).add_path(path))
}

/// The event that tells the delivery path changes were lost: it becomes `WatchEvent::Overflow`.
pub(crate) fn rescan_event() -> Event {
    Event::new(EventKind::Other).set_flag(Flag::Rescan)
}

/// What one completed request means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Completion {
    /// This many bytes of `FILE_NOTIFY_INFORMATION` records.
    Changes(usize),
    /// The kernel's buffer overflowed and changes were dropped.
    Lost,
    /// The request was cancelled (`ERROR_OPERATION_ABORTED`).
    Cancelled,
    /// The request had not completed (`ERROR_IO_INCOMPLETE`): wait again.
    Incomplete,
    /// Any other failure, e.g. the watched directory was deleted. Carries the Win32 error.
    Failed(u32),
}

/// Pure decision for a `GetOverlappedResult` outcome. A successful completion with zero bytes, and
/// `ERROR_NOTIFY_ENUM_DIR`, both mean the change buffer overflowed.
pub(crate) fn classify(success: bool, error: u32, bytes: u32) -> Completion {
    if success {
        return if bytes == 0 {
            Completion::Lost
        } else {
            Completion::Changes(bytes as usize)
        };
    }
    match error {
        ERROR_NOTIFY_ENUM_DIR => Completion::Lost,
        ERROR_OPERATION_ABORTED => Completion::Cancelled,
        ERROR_IO_INCOMPLETE => Completion::Incomplete,
        other => Completion::Failed(other),
    }
}

fn last_error() -> u32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

fn create_event() -> io::Result<OwnedHandle> {
    // SAFETY: plain call; a null result is checked, a non-null handle is owned by the result.
    let raw = unsafe { CreateEventW(ptr::null_mut(), 1, 0, ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a valid handle we own and nothing else closes.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Everything the kernel may touch while a request is pending. Freed only by `Reader::drop`.
struct Resources {
    dir: File,
    event: OwnedHandle,
    buf: Vec<u32>,
    overlapped: Box<Overlapped>,
}

/// One directory handle with at most one pending `ReadDirectoryChangesW` request.
struct Reader {
    res: ManuallyDrop<Resources>,
    recursive: bool,
    pending: bool,
}

// SAFETY: `Overlapped` holds a raw event handle, which is a process-wide kernel handle value and
// valid on any thread. The buffer and OVERLAPPED are only accessed by the thread that owns the
// `Reader`; a pending request is the kernel's, and `Drop` waits it out.
unsafe impl Send for Reader {}

impl Reader {
    fn open(root: &Path, recursive: bool, buffer_bytes: u32) -> io::Result<Self> {
        if !std::fs::metadata(root)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory",
            ));
        }
        let dir = OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED)
            .open(root)?;
        let event = create_event()?;
        let words = (buffer_bytes as usize / 4).max(1024);
        Ok(Self {
            res: ManuallyDrop::new(Resources {
                dir,
                event,
                buf: vec![0u32; words],
                overlapped: Box::new(Overlapped {
                    internal: 0,
                    internal_high: 0,
                    offset: 0,
                    offset_high: 0,
                    h_event: ptr::null_mut(),
                }),
            }),
            recursive,
            pending: false,
        })
    }

    fn dir_handle(&self) -> RawHandle {
        self.res.dir.as_raw_handle()
    }

    fn event_handle(&self) -> RawHandle {
        self.res.event.as_raw_handle()
    }

    /// Issues the next request. On error no request is pending.
    fn arm(&mut self) -> io::Result<()> {
        debug_assert!(!self.pending);
        let event = self.event_handle();
        let dir = self.dir_handle();
        let len_bytes = (self.res.buf.len() * 4) as u32;
        // SAFETY: the event handle is valid. No request is pending, so resetting it and
        // rewriting the OVERLAPPED is not racing the kernel.
        unsafe { ResetEvent(event) };
        *self.res.overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            h_event: event,
        };
        let buffer = self.res.buf.as_mut_ptr() as *mut c_void;
        let overlapped: *mut Overlapped = &mut *self.res.overlapped;
        // SAFETY: `dir` was opened with FILE_FLAG_OVERLAPPED and FILE_LIST_DIRECTORY. `buffer` is
        // `len_bytes` long and DWORD-aligned; both it and `overlapped` are heap allocations owned
        // by `Resources`, which `Drop` keeps alive until a pending request has completed.
        let ok = unsafe {
            ReadDirectoryChangesW(
                dir,
                buffer,
                len_bytes,
                self.recursive as i32,
                NOTIFY_FILTER,
                ptr::null_mut(),
                overlapped,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        self.pending = true;
        Ok(())
    }

    /// Reads the outcome of the request after its event was signalled.
    fn complete(&mut self) -> Completion {
        let dir = self.dir_handle();
        let overlapped: *mut Overlapped = &mut *self.res.overlapped;
        let mut bytes = 0u32;
        // SAFETY: `overlapped` is the structure the pending request was issued with; bWait is
        // FALSE, so this only reads its state.
        let ok = unsafe { GetOverlappedResult(dir, overlapped, &mut bytes, 0) } != 0;
        let error = if ok { 0 } else { last_error() };
        let outcome = classify(ok, error, bytes);
        if outcome != Completion::Incomplete {
            self.pending = false;
        }
        outcome
    }

    /// The records of a completed `Changes(n)` outcome.
    fn records(&self, bytes: usize) -> Vec<RawChange> {
        let available = self.res.buf.len() * 4;
        let n = bytes.min(available);
        // SAFETY: the request has completed (`pending` is false), so the kernel no longer writes
        // the buffer; `n` is within its allocation and `u8` has no alignment requirement.
        let slice = unsafe { std::slice::from_raw_parts(self.res.buf.as_ptr() as *const u8, n) };
        parse_notify_buffer(slice)
    }

    /// Cancels the pending request and waits for it to finish. False if that could not be
    /// confirmed, in which case the caller must not free the buffer.
    fn cancel_and_wait(&mut self) -> bool {
        let dir = self.dir_handle();
        let overlapped: *mut Overlapped = &mut *self.res.overlapped;
        // SAFETY: `overlapped` identifies our one pending request on `dir`.
        let cancelled = unsafe { CancelIoEx(dir, overlapped) } != 0;
        if !cancelled && last_error() != ERROR_NOT_FOUND {
            return false;
        }
        // ERROR_NOT_FOUND means the request had already completed; either way this returns once
        // it has (bWait = TRUE), and only then may the buffer be reused or freed.
        let mut bytes = 0u32;
        // SAFETY: as above.
        unsafe { GetOverlappedResult(dir, overlapped, &mut bytes, 1) };
        self.pending = false;
        true
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let settled = !self.pending || self.cancel_and_wait();
        if settled {
            // SAFETY: dropped exactly once, here, with no request pending.
            unsafe { ManuallyDrop::drop(&mut self.res) };
        }
        // Otherwise the request may still be live: leak the buffer, OVERLAPPED, event and handle
        // instead of freeing memory the kernel can still write.
    }
}

/// A native recursive-or-not directory watch. Dropping it stops and joins the reader thread.
pub(crate) struct DirWatcher {
    stop: Arc<OwnedHandle>,
    thread: Option<thread::JoinHandle<()>>,
}

impl DirWatcher {
    /// Opens `root`, arms the first request, and starts the reader thread. `tx` receives notify
    /// events; the thread keeps a sender until it is stopped, so the channel stays open even
    /// after a fatal read error (which is sent as an `Err`).
    pub(crate) fn start(
        root: &Path,
        recursive: bool,
        tx: mpsc::Sender<notify::Result<Event>>,
    ) -> io::Result<Self> {
        Self::start_with(root, recursive, NATIVE_BUFFER_BYTES, tx)
    }

    fn start_with(
        root: &Path,
        recursive: bool,
        buffer_bytes: u32,
        tx: mpsc::Sender<notify::Result<Event>>,
    ) -> io::Result<Self> {
        let mut reader = Reader::open(root, recursive, buffer_bytes)?;
        reader.arm()?;
        let stop = Arc::new(create_event()?);
        let thread_stop = Arc::clone(&stop);
        let root = root.to_path_buf();
        let thread = thread::Builder::new()
            .name("rightkit-watch-rdcw".into())
            .spawn(move || run(reader, &thread_stop, &root, &tx))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        // SAFETY: `stop` is a valid event handle kept alive by the Arc.
        unsafe { SetEvent(self.stop.as_raw_handle()) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    mut reader: Reader,
    stop: &OwnedHandle,
    root: &Path,
    tx: &mpsc::Sender<notify::Result<Event>>,
) {
    let stop_handle = stop.as_raw_handle();
    let fail = |error: io::Error| {
        let _ = tx.send(Err(notify::Error::io(error).add_path(root.to_path_buf())));
    };
    // Waits until stopped, keeping `tx` alive so the consumer does not see a disconnect.
    let park = || {
        let handles = [stop_handle];
        // SAFETY: one valid handle.
        unsafe { WaitForMultipleObjects(1, handles.as_ptr(), 0, INFINITE) };
    };
    loop {
        let handles = [reader.event_handle(), stop_handle];
        // SAFETY: two valid handles that outlive the call.
        let woke = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
        if woke != WAIT_OBJECT_0 {
            // Stop requested (or the wait failed): dropping `reader` cancels and waits.
            return;
        }
        match reader.complete() {
            Completion::Incomplete => continue,
            Completion::Changes(bytes) => {
                for change in reader.records(bytes) {
                    if let Some(event) = event_for(root, &change) {
                        if tx.send(Ok(event)).is_err() {
                            return;
                        }
                    }
                }
            }
            Completion::Lost => {
                if tx.send(Ok(rescan_event())).is_err() {
                    return;
                }
            }
            Completion::Cancelled => {
                fail(io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32));
                park();
                return;
            }
            Completion::Failed(code) => {
                fail(io::Error::from_raw_os_error(code as i32));
                park();
                return;
            }
        }
        if let Err(error) = reader.arm() {
            fail(error);
            park();
            return;
        }
    }
}

// ---- typed `watch_with` stub (unchanged behaviour) ----

pub(crate) struct Handle;

impl Handle {
    pub(crate) fn volume(&self) -> Option<[u8; 16]> {
        None
    }
}

pub(crate) fn start(
    root: &Path,
    options: &TypedOptions,
    sink: BackendSink,
) -> Result<Handle, WatchError> {
    // Referenced so the seam stays exercised by the compiler.
    let _ = (
        options.windows_buffer(),
        options.windows_network_buffer(),
        sink.queue(),
    );
    Err(WatchError::Backend {
        message:
            "the Windows native typed watch backend is not implemented yet; use WatchBackend::Notify"
                .into(),
        paths: vec![root.to_path_buf()],
    })
}

pub(crate) fn current_cursor(_path: &Path) -> Option<WatchCursor> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// Encodes records the way the kernel lays them out: header, UTF-16 name, padded to 4 bytes,
    /// chained by `NextEntryOffset` (0 on the last).
    fn encode(records: &[(u32, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, (action, name)) in records.iter().enumerate() {
            let units: Vec<u16> = name.encode_utf16().collect();
            let name_bytes = units.len() * 2;
            let size = (RECORD_HEADER + name_bytes + 3) & !3;
            let next = if index + 1 == records.len() { 0 } else { size };
            out.extend_from_slice(&(next as u32).to_le_bytes());
            out.extend_from_slice(&action.to_le_bytes());
            out.extend_from_slice(&(name_bytes as u32).to_le_bytes());
            for unit in units {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out.resize(out.len() + (size - RECORD_HEADER - name_bytes), 0);
        }
        out
    }

    fn change(action: u32, name: &str) -> RawChange {
        RawChange {
            action,
            name: OsString::from(name),
        }
    }

    #[test]
    fn parser_reads_chained_records_and_utf16_names() {
        // "a" pads to a 16-byte record; "caf\u{e9}\\\u{4e2d}.txt" has a non-ASCII name and a
        // subdirectory separator; the astral char exercises a surrogate pair.
        let buffer = encode(&[
            (FILE_ACTION_ADDED, "a"),
            (FILE_ACTION_RENAMED_OLD_NAME, "caf\u{e9}\\\u{4e2d}.txt"),
            (FILE_ACTION_RENAMED_NEW_NAME, "new\u{1F600}"),
            (FILE_ACTION_REMOVED, "z.tmp"),
        ]);
        assert_eq!(
            parse_notify_buffer(&buffer),
            vec![
                change(FILE_ACTION_ADDED, "a"),
                change(FILE_ACTION_RENAMED_OLD_NAME, "caf\u{e9}\\\u{4e2d}.txt"),
                change(FILE_ACTION_RENAMED_NEW_NAME, "new\u{1F600}"),
                change(FILE_ACTION_REMOVED, "z.tmp"),
            ]
        );
    }

    #[test]
    fn parser_honours_next_entry_offset_and_stops_at_zero() {
        // A gap between records: NextEntryOffset skips padding, and trailing bytes after the
        // chain end are ignored.
        let mut buffer = encode(&[(FILE_ACTION_MODIFIED, "one")]);
        let first_size = buffer.len();
        buffer[0..4].copy_from_slice(&((first_size + 8) as u32).to_le_bytes());
        buffer.extend_from_slice(&[0xAA; 8]);
        buffer.extend(encode(&[(FILE_ACTION_ADDED, "two")]));
        buffer.extend_from_slice(&[0xBB; 16]);
        assert_eq!(
            parse_notify_buffer(&buffer),
            vec![
                change(FILE_ACTION_MODIFIED, "one"),
                change(FILE_ACTION_ADDED, "two")
            ]
        );
    }

    #[test]
    fn parser_survives_truncated_and_inconsistent_buffers() {
        assert!(parse_notify_buffer(&[]).is_empty());
        assert!(parse_notify_buffer(&[0u8; 8]).is_empty());
        let good = encode(&[(FILE_ACTION_ADDED, "abc"), (FILE_ACTION_ADDED, "def")]);
        // Cut inside the second record: the first survives, the second is dropped.
        let cut = &good[..good.len() - 4];
        assert_eq!(
            parse_notify_buffer(cut),
            vec![change(FILE_ACTION_ADDED, "abc")]
        );
        // A name length running past the end.
        let mut long = encode(&[(FILE_ACTION_ADDED, "abc")]);
        long[8..12].copy_from_slice(&1000u32.to_le_bytes());
        assert!(parse_notify_buffer(&long).is_empty());
        // An odd name length is not UTF-16.
        let mut odd = encode(&[(FILE_ACTION_ADDED, "abc")]);
        odd[8..12].copy_from_slice(&5u32.to_le_bytes());
        assert!(parse_notify_buffer(&odd).is_empty());
        // A NextEntryOffset that points back must not loop.
        let mut back = encode(&[(FILE_ACTION_ADDED, "abc")]);
        back[0..4].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(parse_notify_buffer(&back).len(), 1);
    }

    #[test]
    fn actions_map_to_the_notify_event_kinds() {
        let root = Path::new(r"C:\watched");
        let kind = |action| event_for(root, &change(action, "f.txt")).map(|e| e.kind);
        assert_eq!(
            kind(FILE_ACTION_ADDED),
            Some(EventKind::Create(CreateKind::Any))
        );
        assert_eq!(
            kind(FILE_ACTION_REMOVED),
            Some(EventKind::Remove(RemoveKind::Any))
        );
        assert_eq!(
            kind(FILE_ACTION_MODIFIED),
            Some(EventKind::Modify(ModifyKind::Any))
        );
        assert_eq!(
            kind(FILE_ACTION_RENAMED_OLD_NAME),
            Some(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
        );
        assert_eq!(
            kind(FILE_ACTION_RENAMED_NEW_NAME),
            Some(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
        );
        assert_eq!(kind(99), None);
        let event = event_for(root, &change(FILE_ACTION_ADDED, r"sub\f.txt")).unwrap();
        assert_eq!(event.paths, vec![PathBuf::from(r"C:\watched\sub\f.txt")]);
    }

    #[test]
    fn zero_bytes_and_enum_dir_mean_lost() {
        assert_eq!(classify(true, 0, 0), Completion::Lost);
        assert_eq!(classify(false, ERROR_NOTIFY_ENUM_DIR, 0), Completion::Lost);
        assert_eq!(classify(true, 0, 48), Completion::Changes(48));
        assert_eq!(
            classify(false, ERROR_OPERATION_ABORTED, 0),
            Completion::Cancelled
        );
        assert_eq!(
            classify(false, ERROR_IO_INCOMPLETE, 0),
            Completion::Incomplete
        );
        assert_eq!(classify(false, 5, 0), Completion::Failed(5));
        // A failure that happens to report bytes is still a failure.
        assert_eq!(classify(false, 5, 16), Completion::Failed(5));
    }

    #[test]
    fn lost_becomes_a_rescan_event() {
        assert!(rescan_event().need_rescan());
    }

    fn recv_until(
        rx: &mpsc::Receiver<notify::Result<Event>>,
        mut done: impl FnMut(&Event) -> bool,
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Ok(event)) if done(&event) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    #[test]
    fn native_reader_reports_a_real_change_and_stops_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (tx, rx) = mpsc::channel();
        let watcher = DirWatcher::start(&root, true, tx).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("hello.txt"), b"hi").unwrap();
        assert!(recv_until(&rx, |event| event
            .paths
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == "hello.txt"))));
        let began = Instant::now();
        drop(watcher);
        assert!(began.elapsed() < Duration::from_secs(5));
        // The thread has exited and released its sender: the channel is now closed.
        while rx.recv_timeout(Duration::from_secs(5)).is_ok() {}
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn non_recursive_reader_ignores_nested_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        let (tx, rx) = mpsc::channel();
        let _watcher = DirWatcher::start(&root, false, tx).unwrap();
        std::fs::write(root.join("sub").join("deep.txt"), b"x").unwrap();
        std::fs::write(root.join("top.txt"), b"x").unwrap();
        let mut saw_deep = false;
        let found_top = recv_until(&rx, |event| {
            saw_deep |= event
                .paths
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "deep.txt"));
            event
                .paths
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "top.txt"))
        });
        assert!(found_top);
        assert!(!saw_deep);
    }

    #[test]
    fn start_fails_for_missing_directories_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        assert!(DirWatcher::start(&dir.path().join("missing"), false, tx.clone()).is_err());
        let file = dir.path().join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(DirWatcher::start(&file, false, tx).is_err());
    }

    #[test]
    fn dropping_many_watchers_leaves_no_pending_io() {
        // Cancel-and-wait on every drop, including a drop straight after start.
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..50 {
            let (tx, _rx) = mpsc::channel();
            drop(DirWatcher::start(dir.path(), true, tx).unwrap());
        }
        // The directory can be removed only if every handle was closed.
        dir.close().unwrap();
    }

    #[test]
    fn watch_batches_delivers_a_real_change_through_the_native_arm() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let set = crate::DirWatchSet::new(4, Duration::from_millis(50));
        let (tx, rx) = mpsc::channel();
        set.watch_batches(&root, move |batch| {
            let _ = tx.send(batch);
        })
        .unwrap();
        std::fs::write(root.join("batch.txt"), b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = false;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(batch) => {
                    if batch
                        .changes
                        .changed
                        .iter()
                        .any(|p| p.file_name().is_some_and(|n| n == "batch.txt"))
                    {
                        seen = true;
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        assert!(seen, "no batch with batch.txt arrived");
        set.unwatch(&root);
        // unwatch joined the reader thread, so the folder is no longer held open.
        drop(set);
        dir.close().unwrap();
    }
}
