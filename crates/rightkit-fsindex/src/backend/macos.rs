// Portions adapted from fsearch (https://github.com/noahdunnagan/fsearch),
// Copyright (c) 2026 Noah Dunnagan, MIT License; see THIRD-PARTY-NOTICES.
// Adapted: the getattrlistbulk entry parser and fd-limit handling (src/walk.rs)
// and the FSEvents stream binding (src/fsevents.rs). Changes: path-based
// std-thread work queue instead of rayon/openat, 64-bit logical and allocated
// sizes, hard-link and APFS clone attributes (ATTR_CMNEXT_*), per-entry error
// handling, exclusion flags, multi-root streams, flush, and no leaked state.

//! macOS backend: getattrlistbulk crawl and FSEvents change feed.
//!
//! Only directories are opened (O_DIRECTORY). File contents are never opened.

use super::*;
use crate::error::Error;
use std::cell::RefCell;
use std::ffi::{c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub struct MacBackend;

const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const DIR_MNTSTATUS_TRIGGER: u32 = 0x2;
const OPEN_DIR: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

thread_local! {
    static BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; 256 * 1024]);
}

fn rd32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_ne_bytes(s.try_into().unwrap()))
}
fn rd64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .map_or(0, |s| u64::from_ne_bytes(s.try_into().unwrap()))
}

fn raise_fd_limit() {
    let mut r: libc::rlimit = unsafe { std::mem::zeroed() };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) };
    r.rlim_cur = r.rlim_max.min(65536).max(r.rlim_cur);
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) };
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(dir.len() + 1 + name.len());
    p.extend_from_slice(dir);
    if dir != b"/" {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// List one open directory into `l`. Returns the error kind on failure.
fn list_fd(fd: i32, l: &mut Listing) -> Option<std::io::ErrorKind> {
    let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
    al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    al.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | ATTR_CMN_ERROR
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_FLAGS
        | libc::ATTR_CMN_FILEID;
    al.dirattr = libc::ATTR_DIR_MOUNTSTATUS;
    al.fileattr = libc::ATTR_FILE_LINKCOUNT
        | libc::ATTR_FILE_ALLOCSIZE
        | libc::ATTR_FILE_DATALENGTH
        | libc::ATTR_FILE_RSRCLENGTH;
    al.forkattr = libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID;
    let mut opts = libc::FSOPT_ATTR_CMN_EXTENDED as u64;
    BUF.with_borrow_mut(|buf| loop {
        let n = unsafe {
            libc::getattrlistbulk(
                fd,
                &mut al as *mut _ as *mut c_void,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                opts,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINVAL) && opts != 0 {
                // Filesystem without extended attributes: retry plain.
                opts = 0;
                al.forkattr = 0;
                continue;
            }
            return Some(e.kind());
        }
        if n == 0 {
            return None;
        }
        let mut p = 0usize;
        for _ in 0..n {
            let len = rd32(buf, p) as usize;
            if len < 24 || p + len > buf.len() {
                return Some(std::io::ErrorKind::InvalidData);
            }
            parse_entry(&buf[p..p + len], l);
            p += len;
        }
    })
}

fn parse_entry(b: &[u8], l: &mut Listing) {
    let (common, dirattr, fileattr, forkattr) = (rd32(b, 4), rd32(b, 12), rd32(b, 16), rd32(b, 20));
    let mut f = 24;
    if common & ATTR_CMN_ERROR != 0 {
        f += 4;
    }
    if common & libc::ATTR_CMN_NAME == 0 {
        return;
    }
    let off = rd32(b, f) as i32 as isize;
    let nlen = rd32(b, f + 4) as usize;
    let start = (f as isize + off) as usize;
    let Some(name) = b.get(start..start + nlen.saturating_sub(1)) else {
        return;
    };
    f += 8;
    let mut d = EntryData {
        kind: KIND_OTHER,
        ..EntryData::default()
    };
    if common & libc::ATTR_CMN_DEVID != 0 {
        d.dev = rd32(b, f);
        f += 4;
    }
    if common & libc::ATTR_CMN_OBJTYPE != 0 {
        d.kind = match rd32(b, f) {
            1 => KIND_FILE,
            2 => KIND_DIR,
            5 => KIND_LINK,
            _ => KIND_OTHER,
        };
        f += 4;
    }
    if common & libc::ATTR_CMN_MODTIME != 0 {
        d.mtime = rd64(b, f) as i64;
        f += 16;
    }
    if common & libc::ATTR_CMN_FLAGS != 0 {
        if rd32(b, f) & libc::UF_HIDDEN != 0 {
            d.flags |= FLAG_HIDDEN;
        }
        f += 4;
    }
    if common & libc::ATTR_CMN_FILEID != 0 {
        d.fileid = rd64(b, f);
        f += 8;
    }
    if dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        if rd32(b, f) & (libc::DIR_MNTSTATUS_MNTPOINT | DIR_MNTSTATUS_TRIGGER) != 0 {
            d.flags |= FLAG_MOUNT;
        }
        f += 4;
    }
    d.nlink = 1;
    if fileattr & libc::ATTR_FILE_LINKCOUNT != 0 {
        d.nlink = rd32(b, f).max(1);
        f += 4;
    }
    let mut have_alloc = false;
    if fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 {
        d.alloc = rd64(b, f);
        have_alloc = true;
        f += 8;
    }
    if fileattr & libc::ATTR_FILE_DATALENGTH != 0 {
        d.logical = rd64(b, f);
        f += 8;
    }
    if fileattr & libc::ATTR_FILE_RSRCLENGTH != 0 {
        d.logical = d.logical.saturating_add(rd64(b, f));
        f += 8;
    }
    let mut have_private = false;
    if forkattr & libc::ATTR_CMNEXT_PRIVATESIZE != 0 {
        d.private = rd64(b, f);
        have_private = true;
        f += 8;
    }
    if forkattr & libc::ATTR_CMNEXT_CLONEID != 0 {
        d.cloneid = rd64(b, f);
    }
    if !have_private || !have_alloc {
        d.private = d.alloc;
    }
    d.private = d.private.min(d.alloc);
    if name.is_empty() || name.len() > u16::MAX as usize {
        return;
    }
    l.ents.push(RawEnt {
        name_off: l.names.len() as u32,
        name_len: name.len() as u16,
        data: d,
        child: NONE,
    });
    l.names.extend_from_slice(name);
}

fn mark_excluded(l: &mut Listing, dir: &[u8], excludes: &Excludes) {
    if excludes.prefixes.is_empty() {
        return;
    }
    for i in 0..l.ents.len() {
        if l.ents[i].data.kind == KIND_DIR {
            let name = &l.names[l.ents[i].name_off as usize
                ..l.ents[i].name_off as usize + l.ents[i].name_len as usize];
            if excludes.matches(&join(dir, name)) {
                l.ents[i].data.flags |= FLAG_EXCLUDED;
            }
        }
    }
}

fn list_path(path: &[u8], l: &mut Listing) -> Option<std::io::ErrorKind> {
    let Ok(c) = CString::new(path) else {
        return Some(std::io::ErrorKind::InvalidInput);
    };
    let fd = unsafe { libc::open(c.as_ptr(), OPEN_DIR) };
    if fd < 0 {
        return Some(std::io::Error::last_os_error().kind());
    }
    let r = list_fd(fd, l);
    unsafe { libc::close(fd) };
    r
}

struct Job {
    id: u32,
    path: Vec<u8>,
}

struct Queue {
    stack: Vec<Job>,
    active: usize,
}

impl Backend for MacBackend {
    fn name(&self) -> &'static str {
        "macos-getattrlistbulk-fsevents"
    }

    fn check_supported(&self) -> Result<()> {
        Ok(())
    }

    fn stat(&self, path: &Path) -> std::io::Result<EntryData> {
        let m = std::fs::symlink_metadata(path)?;
        let ft = m.file_type();
        Ok(EntryData {
            kind: if ft.is_dir() {
                KIND_DIR
            } else if ft.is_symlink() {
                KIND_LINK
            } else if ft.is_file() {
                KIND_FILE
            } else {
                KIND_OTHER
            },
            nlink: m.nlink() as u32,
            mtime: m.mtime(),
            dev: m.dev() as u32,
            fileid: m.ino(),
            ..EntryData::default()
        })
    }

    fn crawl(&self, root: &Path, excludes: &Excludes, threads: usize, sink: &mut dyn FnMut(Listing)) {
        raise_fd_limit();
        let threads = threads.max(1);
        let q = (Mutex::new(Queue { stack: vec![Job { id: 0, path: root.as_os_str().as_bytes().to_vec() }], active: 0 }), Condvar::new());
        let next_id = AtomicU32::new(1);
        // Bounded: workers block when the consumer falls behind, so a whole-disk crawl never
        // holds more than a few hundred directory listings in memory.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Listing>(256);
        std::thread::scope(|s| {
            for _ in 0..threads {
                let tx = tx.clone();
                let (q, next_id) = (&q, &next_id);
                s.spawn(move || loop {
                    let job = {
                        let mut g = q.0.lock().unwrap();
                        loop {
                            if let Some(j) = g.stack.pop() {
                                g.active += 1;
                                break Some(j);
                            }
                            if g.active == 0 {
                                q.1.notify_all();
                                break None;
                            }
                            g = q.1.wait(g).unwrap();
                        }
                    };
                    let Some(job) = job else { return };
                    let mut l = Listing { id: job.id, ..Listing::default() };
                    l.error = list_path(&job.path, &mut l);
                    mark_excluded(&mut l, &job.path, excludes);
                    let mut kids = Vec::new();
                    for i in 0..l.ents.len() {
                        let e = l.ents[i];
                        if e.data.kind == KIND_DIR && e.data.flags & (FLAG_MOUNT | FLAG_EXCLUDED) == 0 {
                            let id = next_id.fetch_add(1, Ordering::Relaxed);
                            l.ents[i].child = id;
                            kids.push(Job { id, path: join(&job.path, l.name(&e)) });
                        }
                    }
                    // A parent's listing is sent before any child job exists, so the
                    // consumer always sees parents first.
                    let _ = tx.send(l);
                    let mut g = q.0.lock().unwrap();
                    g.stack.extend(kids);
                    g.active -= 1;
                    q.1.notify_all();
                });
            }
            drop(tx);
            for l in rx {
                sink(l);
            }
        });
    }

    fn list_dir(&self, dir: &Path, excludes: &Excludes) -> std::io::Result<Listing> {
        let p = dir.as_os_str().as_bytes();
        let mut l = Listing::default();
        if let Some(k) = list_path(p, &mut l) {
            return Err(k.into());
        }
        mark_excluded(&mut l, p, excludes);
        Ok(l)
    }

    fn watch(
        &self,
        roots: &[PathBuf],
        since: Option<u64>,
        latency: Duration,
        tx: Sender<WatchMsg>,
    ) -> Result<Box<dyn Watcher>> {
        Ok(Box::new(Stream::start(
            roots,
            since.unwrap_or(SINCE_NOW),
            latency.as_secs_f64(),
            tx,
        )?))
    }

    fn current_event_id(&self) -> u64 {
        unsafe { FSEventsGetCurrentEventId() }
    }

    fn has_full_disk_access(&self) -> bool {
        // Directory probes only: opening a protected directory fails with EPERM
        // without Full Disk Access and raises no consent prompt.
        let Some(home) = std::env::var_os("HOME") else {
            return true;
        };
        for rel in ["Library/Safari", "Library/Mail", "Library/Messages"] {
            let p = Path::new(&home).join(rel);
            if std::fs::symlink_metadata(&p).is_ok() {
                let mut l = Listing::default();
                return !matches!(
                    list_path(p.as_os_str().as_bytes(), &mut l),
                    Some(std::io::ErrorKind::PermissionDenied)
                );
            }
        }
        true
    }
}

// ---------------------------------------------------------------- FSEvents

const SINCE_NOW: u64 = u64::MAX;
const CREATE_FLAG_NO_DEFER: u32 = 0x2;
const CREATE_FLAG_WATCH_ROOT: u32 = 0x4;

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type Callback = extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

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
    fn FSEventStreamFlushSync(s: *mut c_void);
    fn FSEventStreamStop(s: *mut c_void);
    fn FSEventStreamInvalidate(s: *mut c_void);
    fn FSEventStreamRelease(s: *mut c_void);
    fn FSEventsGetCurrentEventId() -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const i8, enc: u32) -> *const c_void;
    fn CFArrayCreate(
        alloc: *const c_void,
        vals: *const *const c_void,
        n: isize,
        cbs: *const c_void,
    ) -> *const c_void;
    fn CFRelease(o: *const c_void);
    static kCFTypeArrayCallBacks: c_void;
}

extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
    fn dispatch_release(o: *mut c_void);
}

extern "C" fn on_events(
    _s: *mut c_void,
    info: *mut c_void,
    n: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    let tx = unsafe { &*(info as *const Sender<WatchMsg>) };
    let paths = paths as *const *const i8;
    let batch = (0..n)
        .map(|i| unsafe {
            Event {
                path: CStr::from_ptr(*paths.add(i)).to_bytes().to_vec(),
                flags: *flags.add(i),
                id: *ids.add(i),
            }
        })
        .collect();
    let _ = tx.send(WatchMsg::Events(batch));
}

struct Stream {
    s: *mut c_void,
    q: *mut c_void,
    info: *mut Sender<WatchMsg>,
}

// The stream is started once, then only flushed or stopped.
unsafe impl Send for Stream {}

impl Stream {
    fn start(roots: &[PathBuf], since: u64, latency: f64, tx: Sender<WatchMsg>) -> Result<Stream> {
        unsafe {
            let strs: Vec<*const c_void> = roots
                .iter()
                .filter_map(|r| CString::new(r.as_os_str().as_bytes()).ok())
                .map(|c| CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), 0x0800_0100))
                .collect();
            if strs.is_empty() {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "no watch roots",
                )));
            }
            let arr = CFArrayCreate(
                std::ptr::null(),
                strs.as_ptr(),
                strs.len() as isize,
                &kCFTypeArrayCallBacks as *const c_void,
            );
            let info = Box::into_raw(Box::new(tx));
            let ctx = Context {
                version: 0,
                info: info as *mut c_void,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            // Not IgnoreSelf: the host app's own renames and moves must reach the index.
            let s = FSEventStreamCreate(
                std::ptr::null(),
                on_events,
                &ctx,
                arr,
                since,
                latency,
                CREATE_FLAG_NO_DEFER | CREATE_FLAG_WATCH_ROOT,
            );
            CFRelease(arr);
            for r in strs {
                CFRelease(r);
            }
            let q = dispatch_queue_create(c"rightkit.fsindex.fsevents".as_ptr(), std::ptr::null());
            FSEventStreamSetDispatchQueue(s, q);
            if FSEventStreamStart(s) == 0 {
                FSEventStreamInvalidate(s);
                FSEventStreamRelease(s);
                dispatch_release(q);
                drop(Box::from_raw(info));
                return Err(Error::Io(std::io::Error::other(
                    "FSEventStreamStart failed",
                )));
            }
            Ok(Stream { s, q, info })
        }
    }
}

impl Watcher for Stream {
    fn flush(&self) {
        unsafe { FSEventStreamFlushSync(self.s) };
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            FSEventStreamStop(self.s);
            FSEventStreamInvalidate(self.s);
            FSEventStreamRelease(self.s);
            dispatch_release(self.q);
            // The sender box (16 bytes) is leaked on purpose: a callback may still be
            // in flight on the queue after the stream stops (same choice as upstream).
            let _ = self.info;
        }
    }
}
