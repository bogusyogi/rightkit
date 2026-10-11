//! macOS-only DNS-SD (dns_sd C API) advertise and browse for the Pulse send service.
//!
//! The service type is `_pulse-send._tcp`. TXT records are an announcement only: no
//! identity is taken from them. Identity is proven by the TLS handshake certificate
//! elsewhere. Every DNS-SD operation owns one `DNSServiceRef` driven by its own thread,
//! which polls the ref's socket, processes replies, and deallocates the ref on exit.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char, c_void};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// DNS-SD service type advertised and browsed by Pulse send (checked by the tests).
#[cfg(test)]
pub const SERVICE_TYPE: &str = "_pulse-send._tcp";

const REGTYPE: &CStr = c"_pulse-send._tcp";

const FLAGS_ADD: u32 = 0x2;
const PROTOCOL_IPV4: u32 = 0x1;
const INTERFACE_ANY: u32 = 0;
/// kDNSServiceErr_Unknown, used when a reply arrives without required data.
const ERR_UNKNOWN: i32 = -65537;

const POLL_MS: i32 = 200;
const START_TIMEOUT: Duration = Duration::from_secs(5);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const ADDR_TIMEOUT: Duration = Duration::from_secs(5);

const MAX_ENTRY: usize = 255;
const MAX_ALIAS_CHARS: usize = 80;
const MAX_LABEL_BYTES: usize = 63;

type DNSServiceRef = *mut c_void;

type RegisterReply = extern "C" fn(
    DNSServiceRef,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
type BrowseReply = extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
type ResolveReply = extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    u16,
    u16,
    *const u8,
    *mut c_void,
);
type AddrInfoReply = extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const libc::sockaddr,
    u32,
    *mut c_void,
);

unsafe extern "C" {
    fn DNSServiceRegister(
        sd_ref: *mut DNSServiceRef,
        flags: u32,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt_record: *const c_void,
        callback: RegisterReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceBrowse(
        sd_ref: *mut DNSServiceRef,
        flags: u32,
        interface_index: u32,
        regtype: *const c_char,
        domain: *const c_char,
        callback: BrowseReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceResolve(
        sd_ref: *mut DNSServiceRef,
        flags: u32,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        callback: ResolveReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceGetAddrInfo(
        sd_ref: *mut DNSServiceRef,
        flags: u32,
        interface_index: u32,
        protocol: u32,
        hostname: *const c_char,
        callback: AddrInfoReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(sd_ref: DNSServiceRef) -> i32;
    fn DNSServiceProcessResult(sd_ref: DNSServiceRef) -> i32;
    fn DNSServiceRefDeallocate(sd_ref: DNSServiceRef);
}

thread_local! {
    /// True on threads spawned by this module; Drop never joins from such a thread.
    static OWN_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn dns_err(what: &str, code: i32) -> io::Error {
    io::Error::other(format!("{what} failed: DNS-SD error {code}"))
}

fn check(what: &str, code: i32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(dns_err(what, code))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Truncate `s` to at most `max` bytes at a char boundary.
fn truncate_at(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate a DNS-SD instance name to the 63-byte DNS label limit.
fn truncate_label(s: &str) -> &str {
    truncate_at(s, MAX_LABEL_BYTES)
}

fn push_entry(out: &mut Vec<u8>, key: &str, value: &str) {
    let room = MAX_ENTRY.saturating_sub(key.len() + 1);
    let value = truncate_at(value, room);
    let Ok(len) = u8::try_from(key.len() + 1 + value.len()) else {
        return;
    };
    out.push(len);
    out.extend_from_slice(key.as_bytes());
    out.push(b'=');
    out.extend_from_slice(value.as_bytes());
}

/// Encode DNS-SD TXT entries: `fp`, `alias` (at most 80 chars), `v`=2.2, `type`,
/// `model` (only when present and non-empty), `px`=1. Each entry is at most 255 bytes.
pub fn txt_encode(
    fingerprint: &str,
    alias: &str,
    device_type: &str,
    model: Option<&str>,
) -> Vec<u8> {
    let alias: String = alias.chars().take(MAX_ALIAS_CHARS).collect();
    let mut out = Vec::new();
    push_entry(&mut out, "fp", fingerprint);
    push_entry(&mut out, "alias", &alias);
    push_entry(&mut out, "v", "2.2");
    push_entry(&mut out, "type", device_type);
    if let Some(model) = model.filter(|m| !m.is_empty()) {
        push_entry(&mut out, "model", model);
    }
    push_entry(&mut out, "px", "1");
    out
}

/// Decode a TXT record into (key, value) pairs. Empty entries are skipped, an entry
/// without `=` yields an empty value, and decoding stops at an overrunning length.
pub fn txt_decode(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&len) = bytes.get(i) {
        i += 1;
        let len = usize::from(len);
        if len == 0 {
            continue;
        }
        let Some(entry) = bytes.get(i..i + len) else {
            break;
        };
        i += len;
        match entry.iter().position(|&b| b == b'=') {
            Some(p) => out.push((
                String::from_utf8_lossy(&entry[..p]).into_owned(),
                String::from_utf8_lossy(&entry[p + 1..]).into_owned(),
            )),
            None => out.push((String::from_utf8_lossy(entry).into_owned(), String::new())),
        }
    }
    out
}

/// Owns a DNSServiceRef and deallocates it exactly once on drop.
struct RefGuard(DNSServiceRef);

impl Drop for RefGuard {
    fn drop(&mut self) {
        // SAFETY: the ref was created by a successful DNSServiceXxx call and is
        // deallocated only here.
        unsafe { DNSServiceRefDeallocate(self.0) };
    }
}

fn ctx_ptr<T>(r: &T) -> *mut c_void {
    ptr::from_ref(r).cast_mut().cast()
}

/// # Safety
/// `p` must be null or point to a live `T` that outlives the returned reference.
unsafe fn ctx_ref<'a, T>(p: *mut c_void) -> Option<&'a T> {
    // SAFETY: guaranteed by the caller.
    unsafe { p.cast::<T>().as_ref() }
}

/// Poll the ref's socket and process replies until `finished()` or the stop flag
/// (both give Ok), the deadline (TimedOut), or a DNS-SD / socket error.
fn pump(
    sd: DNSServiceRef,
    stop: &AtomicBool,
    deadline: Option<Instant>,
    finished: &dyn Fn() -> bool,
) -> io::Result<()> {
    // SAFETY: `sd` is a live ref owned by the calling thread.
    let fd = unsafe { DNSServiceRefSockFD(sd) };
    if fd < 0 {
        return Err(io::Error::other(
            "DNSServiceRefSockFD returned an invalid descriptor",
        ));
    }
    loop {
        if stop.load(Ordering::Acquire) || finished() {
            return Ok(());
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "DNS-SD reply timed out",
                ));
            }
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid single-element pollfd array.
        let n = unsafe { libc::poll(&mut pfd, 1, POLL_MS) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            continue;
        }
        if pfd.revents & libc::POLLIN != 0 {
            // SAFETY: `sd` is live; callbacks run on this thread with live contexts.
            check("DNSServiceProcessResult", unsafe {
                DNSServiceProcessResult(sd)
            })?;
        } else if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("DNS-SD socket closed"));
        }
    }
}

/// A background thread plus its stop flag; dropping it stops and joins the thread.
struct Runner {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            // Joining from one of our own threads (a callback dropping its owner) would deadlock.
            if !OWN_THREAD.with(Cell::get) {
                let _ = h.join();
            }
        }
    }
}

fn await_start(rx: &Receiver<io::Result<()>>) -> io::Result<()> {
    match rx.recv_timeout(START_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err(io::Error::other(
            "DNS-SD start timed out waiting for first reply",
        )),
    }
}

// ---------------------------------------------------------------- advertise

/// A running advertisement. Dropping it stops it and deallocates its DNS-SD ref.
pub struct Advertiser {
    _runner: Runner,
}

struct RegisterCtx {
    tx: mpsc::Sender<io::Result<()>>,
}

extern "C" fn register_reply(
    _sd: DNSServiceRef,
    _flags: u32,
    err: i32,
    _name: *const c_char,
    _regtype: *const c_char,
    _domain: *const c_char,
    context: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the context is a live RegisterCtx owned by the advertise thread.
        let Some(ctx) = (unsafe { ctx_ref::<RegisterCtx>(context) }) else {
            return;
        };
        let _ = ctx.tx.send(check("register reply", err));
    }));
}

fn advertise_thread(
    name: CString,
    port: u16,
    txt: Vec<u8>,
    tx: mpsc::Sender<io::Result<()>>,
    stop: Arc<AtomicBool>,
) {
    OWN_THREAD.with(|c| c.set(true));
    let ctx = Box::new(RegisterCtx { tx: tx.clone() });
    let Ok(txt_len) = u16::try_from(txt.len()) else {
        let _ = tx.send(Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TXT record too long",
        )));
        return;
    };
    let txt_ptr: *const c_void = if txt.is_empty() {
        ptr::null()
    } else {
        txt.as_ptr().cast()
    };
    let mut sd: DNSServiceRef = ptr::null_mut();
    // SAFETY: all pointers are valid for the call; `ctx` outlives the ref (the guard
    // below is declared later and therefore dropped first).
    let rc = unsafe {
        DNSServiceRegister(
            &mut sd,
            0,
            INTERFACE_ANY,
            name.as_ptr(),
            REGTYPE.as_ptr(),
            ptr::null(),
            ptr::null(),
            port.to_be(),
            txt_len,
            txt_ptr,
            register_reply,
            ctx_ptr(&*ctx),
        )
    };
    if let Err(e) = check("DNSServiceRegister", rc) {
        let _ = tx.send(Err(e));
        return;
    }
    let guard = RefGuard(sd);
    if let Err(e) = pump(guard.0, &stop, None, &|| false) {
        let _ = tx.send(Err(e));
    }
}

impl Advertiser {
    /// Register `_pulse-send._tcp` as `instance` (truncated to 63 bytes) on `port`
    /// (host byte order) with the given TXT bytes. Fails if the register call or its
    /// first reply reports an error, or no reply arrives within 5 s.
    pub fn start(instance: &str, port: u16, txt: &[u8]) -> io::Result<Advertiser> {
        let name = CString::new(truncate_label(instance)).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "instance name contains NUL")
        })?;
        let txt = txt.to_vec();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("pulse-send-advertise".into())
            .spawn(move || advertise_thread(name, port, txt, tx, thread_stop))?;
        let runner = Runner {
            stop,
            handle: Some(handle),
        };
        await_start(&rx)?;
        Ok(Advertiser { _runner: runner })
    }
}

// ------------------------------------------------------------------- browse

/// One peer heard on the network, fully resolved to an IPv4 address.
#[derive(Debug, Clone)]
pub struct Found {
    /// The DNS-SD instance name as advertised.
    pub instance: String,
    /// An IPv4 address (`IpAddr::V4`).
    pub host: IpAddr,
    /// Port in host byte order.
    pub port: u16,
    pub txt: Vec<(String, String)>,
}

type FoundCb = Arc<dyn Fn(Found) + Send + Sync>;
type LostCb = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Default)]
struct State {
    /// Interfaces on which each instance is currently visible.
    ifaces: HashMap<String, HashSet<u32>>,
    /// Instances being resolved, mapped to the generation of their worker.
    pending: HashMap<String, u64>,
    /// Instances reported through `on_found`.
    reported: HashSet<String>,
    next_gen: u64,
}

struct BrowseCtx {
    own: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    on_found: FoundCb,
    on_lost: LostCb,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

/// A running browse. Dropping it stops it.
pub struct Browser {
    _runner: Runner,
}

extern "C" fn browse_reply(
    _sd: DNSServiceRef,
    flags: u32,
    ifindex: u32,
    err: i32,
    name: *const c_char,
    regtype: *const c_char,
    domain: *const c_char,
    context: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the context is a live BrowseCtx owned by the browse thread.
        let Some(ctx) = (unsafe { ctx_ref::<BrowseCtx>(context) }) else {
            return;
        };
        if err != 0 || name.is_null() || regtype.is_null() || domain.is_null() {
            return;
        }
        // SAFETY: non-null NUL-terminated strings valid for the callback duration.
        let (name_c, regtype_c, domain_c) = unsafe {
            (
                CStr::from_ptr(name).to_owned(),
                CStr::from_ptr(regtype).to_owned(),
                CStr::from_ptr(domain).to_owned(),
            )
        };
        let name_s = name_c.to_string_lossy().into_owned();
        if flags & FLAGS_ADD != 0 {
            handle_add(ctx, name_s, name_c, regtype_c, domain_c, ifindex);
        } else {
            handle_remove(ctx, &name_s, ifindex);
        }
    }));
}

struct Job {
    name: String,
    name_c: CString,
    regtype: CString,
    domain: CString,
    ifindex: u32,
    generation: u64,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    on_found: FoundCb,
}

fn clear_pending(state: &Mutex<State>, name: &str, generation: u64) {
    let mut st = lock(state);
    if st.pending.get(name) == Some(&generation) {
        st.pending.remove(name);
    }
}

fn handle_add(
    ctx: &BrowseCtx,
    name: String,
    name_c: CString,
    regtype: CString,
    domain: CString,
    ifindex: u32,
) {
    if name == ctx.own {
        return;
    }
    let generation = {
        let mut st = lock(&ctx.state);
        st.ifaces.entry(name.clone()).or_default().insert(ifindex);
        if st.reported.contains(&name) || st.pending.contains_key(&name) {
            return;
        }
        st.next_gen += 1;
        let g = st.next_gen;
        st.pending.insert(name.clone(), g);
        g
    };
    let job = Job {
        name: name.clone(),
        name_c,
        regtype,
        domain,
        ifindex,
        generation,
        state: Arc::clone(&ctx.state),
        stop: Arc::clone(&ctx.stop),
        on_found: Arc::clone(&ctx.on_found),
    };
    match thread::Builder::new()
        .name("pulse-send-resolve".into())
        .spawn(move || run_worker(job))
    {
        Ok(h) => {
            let mut workers = lock(&ctx.workers);
            workers.retain(|w| !w.is_finished());
            workers.push(h);
        }
        Err(_) => clear_pending(&ctx.state, &name, generation),
    }
}

fn handle_remove(ctx: &BrowseCtx, name: &str, ifindex: u32) {
    let mut st = lock(&ctx.state);
    let Some(set) = st.ifaces.get_mut(name) else {
        return;
    };
    set.remove(&ifindex);
    if !set.is_empty() {
        return;
    }
    st.ifaces.remove(name);
    st.pending.remove(name);
    // The lock is held across on_lost so it cannot be reordered before on_found.
    if st.reported.remove(name) {
        let _ = catch_unwind(AssertUnwindSafe(|| (ctx.on_lost)(name.to_owned())));
    }
}

type Slot<T> = Mutex<Option<Result<T, i32>>>;

fn fill<T>(slot: &Slot<T>, v: Result<T, i32>) {
    let mut g = lock(slot);
    if g.is_none() {
        *g = Some(v);
    }
}

/// Wait up to `timeout` for `slot` to be filled by a callback of the ref `sd`.
fn await_slot<T>(
    sd: DNSServiceRef,
    stop: &AtomicBool,
    slot: &Slot<T>,
    timeout: Duration,
    what: &str,
) -> io::Result<T> {
    pump(sd, stop, Some(Instant::now() + timeout), &|| {
        lock(slot).is_some()
    })?;
    match lock(slot).take() {
        Some(Ok(v)) => Ok(v),
        Some(Err(code)) => Err(dns_err(what, code)),
        None => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "DNS-SD query stopped",
        )),
    }
}

struct Resolved {
    host: CString,
    port: u16,
    txt: Vec<u8>,
}

extern "C" fn resolve_reply(
    _sd: DNSServiceRef,
    _flags: u32,
    _ifindex: u32,
    err: i32,
    _fullname: *const c_char,
    host: *const c_char,
    port: u16,
    txt_len: u16,
    txt: *const u8,
    context: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the context is a live Slot owned by the worker thread.
        let Some(slot) = (unsafe { ctx_ref::<Slot<Resolved>>(context) }) else {
            return;
        };
        if err != 0 || host.is_null() {
            fill(slot, Err(if err != 0 { err } else { ERR_UNKNOWN }));
            return;
        }
        let txt = if txt.is_null() || txt_len == 0 {
            Vec::new()
        } else {
            // SAFETY: the daemon provides `txt_len` readable bytes at `txt`.
            unsafe { std::slice::from_raw_parts(txt, usize::from(txt_len)) }.to_vec()
        };
        // SAFETY: `host` is a non-null NUL-terminated string.
        let host = unsafe { CStr::from_ptr(host) }.to_owned();
        fill(
            slot,
            Ok(Resolved {
                host,
                port: u16::from_be(port),
                txt,
            }),
        );
    }));
}

extern "C" fn addrinfo_reply(
    _sd: DNSServiceRef,
    flags: u32,
    _ifindex: u32,
    err: i32,
    _hostname: *const c_char,
    address: *const libc::sockaddr,
    _ttl: u32,
    context: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the context is a live Slot owned by the worker thread.
        let Some(slot) = (unsafe { ctx_ref::<Slot<Ipv4Addr>>(context) }) else {
            return;
        };
        if err != 0 {
            fill(slot, Err(err));
            return;
        }
        if flags & FLAGS_ADD == 0 || address.is_null() {
            return;
        }
        // SAFETY: `address` is non-null and points to a sockaddr from the daemon.
        let family = unsafe { (*address).sa_family };
        if i32::from(family) != libc::AF_INET {
            return;
        }
        let sin = address.cast::<libc::sockaddr_in>();
        // SAFETY: AF_INET sockaddrs are sockaddr_in; s_addr is in network byte order.
        let raw = unsafe { ptr::addr_of!((*sin).sin_addr.s_addr).read_unaligned() };
        fill(slot, Ok(Ipv4Addr::from(raw.to_ne_bytes())));
    }));
}

fn query_resolve(job: &Job) -> io::Result<Resolved> {
    let slot: Box<Slot<Resolved>> = Box::new(Mutex::new(None));
    let mut sd: DNSServiceRef = ptr::null_mut();
    // SAFETY: valid pointers; `slot` outlives the ref (guard declared later, dropped first).
    let rc = unsafe {
        DNSServiceResolve(
            &mut sd,
            0,
            job.ifindex,
            job.name_c.as_ptr(),
            job.regtype.as_ptr(),
            job.domain.as_ptr(),
            resolve_reply,
            ctx_ptr(&*slot),
        )
    };
    check("DNSServiceResolve", rc)?;
    let guard = RefGuard(sd);
    await_slot(guard.0, &job.stop, &slot, RESOLVE_TIMEOUT, "resolve reply")
}

fn query_addr(job: &Job, host: &CStr) -> io::Result<Ipv4Addr> {
    let slot: Box<Slot<Ipv4Addr>> = Box::new(Mutex::new(None));
    let mut sd: DNSServiceRef = ptr::null_mut();
    // SAFETY: valid pointers; `slot` outlives the ref (guard declared later, dropped first).
    let rc = unsafe {
        DNSServiceGetAddrInfo(
            &mut sd,
            0,
            INTERFACE_ANY,
            PROTOCOL_IPV4,
            host.as_ptr(),
            addrinfo_reply,
            ctx_ptr(&*slot),
        )
    };
    check("DNSServiceGetAddrInfo", rc)?;
    let guard = RefGuard(sd);
    await_slot(guard.0, &job.stop, &slot, ADDR_TIMEOUT, "getaddrinfo reply")
}

fn run_worker(job: Job) {
    OWN_THREAD.with(|c| c.set(true));
    let result = query_resolve(&job).and_then(|r| {
        let addr = query_addr(&job, &r.host)?;
        Ok((r, addr))
    });
    let Ok((resolved, addr)) = result else {
        clear_pending(&job.state, &job.name, job.generation);
        return;
    };
    let found = Found {
        instance: job.name.clone(),
        host: IpAddr::V4(addr),
        port: resolved.port,
        txt: txt_decode(&resolved.txt),
    };
    let mut st = lock(&job.state);
    if job.stop.load(Ordering::Acquire) || st.pending.get(&job.name) != Some(&job.generation) {
        return;
    }
    st.pending.remove(&job.name);
    st.reported.insert(job.name.clone());
    // The lock is held across on_found so a concurrent Remove cannot overtake it.
    let _ = catch_unwind(AssertUnwindSafe(|| (job.on_found)(found)));
}

fn browse_thread(
    own: String,
    on_found: FoundCb,
    on_lost: LostCb,
    tx: mpsc::Sender<io::Result<()>>,
    stop: Arc<AtomicBool>,
) {
    OWN_THREAD.with(|c| c.set(true));
    let ctx = Box::new(BrowseCtx {
        own,
        state: Arc::new(Mutex::new(State::default())),
        stop: Arc::clone(&stop),
        on_found,
        on_lost,
        workers: Mutex::new(Vec::new()),
    });
    let mut sd: DNSServiceRef = ptr::null_mut();
    // SAFETY: valid pointers; `ctx` outlives the ref (guard declared later, dropped first).
    let rc = unsafe {
        DNSServiceBrowse(
            &mut sd,
            0,
            INTERFACE_ANY,
            REGTYPE.as_ptr(),
            ptr::null(),
            browse_reply,
            ctx_ptr(&*ctx),
        )
    };
    if let Err(e) = check("DNSServiceBrowse", rc) {
        let _ = tx.send(Err(e));
        return;
    }
    let guard = RefGuard(sd);
    let _ = tx.send(Ok(()));
    let _ = pump(guard.0, &stop, None, &|| false);
    // Release the ref first so no callback can run, then let workers finish.
    drop(guard);
    stop.store(true, Ordering::Release);
    let workers = std::mem::take(&mut *lock(&ctx.workers));
    for w in workers {
        let _ = w.join();
    }
}

impl Browser {
    /// Browse `_pulse-send._tcp` in the default domain, ignoring `own_instance`.
    /// `on_found` runs once per instance after SRV+TXT resolve and an IPv4 lookup
    /// succeed; `on_lost` runs when a previously found instance is removed. Both run
    /// on background threads.
    pub fn start(
        own_instance: String,
        on_found: Box<dyn Fn(Found) + Send + Sync>,
        on_lost: Box<dyn Fn(String) + Send + Sync>,
    ) -> io::Result<Browser> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let on_found: FoundCb = Arc::from(on_found);
        let on_lost: LostCb = Arc::from(on_lost);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("pulse-send-browse".into())
            .spawn(move || browse_thread(own_instance, on_found, on_lost, tx, thread_stop))?;
        let runner = Runner {
            stop,
            handle: Some(handle),
        };
        await_start(&rx)?;
        Ok(Browser { _runner: runner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(d: &'a [(String, String)], key: &str) -> Option<&'a str> {
        d.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn roundtrip_all_fields() {
        let alias = "\u{65E5}".repeat(80);
        let txt = txt_encode("AB:CD:EF", &alias, "laptop", Some("MacBookPro18,1"));
        let d = txt_decode(&txt);
        assert_eq!(get(&d, "fp"), Some("AB:CD:EF"));
        assert_eq!(get(&d, "alias"), Some(alias.as_str()));
        assert_eq!(get(&d, "type"), Some("laptop"));
        assert_eq!(get(&d, "model"), Some("MacBookPro18,1"));
        assert_eq!(get(&d, "px"), Some("1"));
        let keys: Vec<&str> = d.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["fp", "alias", "v", "type", "model", "px"]);
    }

    #[test]
    fn model_omitted_when_none_or_empty() {
        for model in [None, Some("")] {
            let d = txt_decode(&txt_encode("fp", "a", "phone", model));
            assert_eq!(get(&d, "model"), None);
            let keys: Vec<&str> = d.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(keys, vec!["fp", "alias", "v", "type", "px"]);
        }
    }

    #[test]
    fn version_is_2_2() {
        let d = txt_decode(&txt_encode("fp", "a", "phone", None));
        assert_eq!(get(&d, "v"), Some("2.2"));
    }

    #[test]
    fn alias_capped_at_80_chars() {
        let d = txt_decode(&txt_encode("fp", &"x".repeat(100), "phone", None));
        assert_eq!(get(&d, "alias").map(str::len), Some(80));
    }

    #[test]
    fn long_values_truncate_to_255_byte_entries() {
        let fp = "a".repeat(400);
        let alias = "\u{1F600}".repeat(80);
        let txt = txt_encode(&fp, &alias, "laptop", None);
        let mut i = 0;
        while i < txt.len() {
            let len = usize::from(txt[i]);
            assert_eq!(len <= 255 && i + 1 + len <= txt.len(), true);
            i += 1 + len;
        }
        assert_eq!(i, txt.len());
        let d = txt_decode(&txt);
        assert_eq!(get(&d, "fp").map(str::len), Some(252));
        assert_eq!(get(&d, "alias").map(|a| a.chars().count()), Some(62));
        assert_eq!(get(&d, "px"), Some("1"));
    }

    #[test]
    fn decode_truncated_input_returns_prefix() {
        let mut bytes = vec![3, b'a', b'=', b'b', 0, 5, b'k', b'=', b'v'];
        assert_eq!(txt_decode(&bytes), vec![("a".to_owned(), "b".to_owned())]);
        bytes.truncate(2);
        assert_eq!(txt_decode(&bytes), Vec::<(String, String)>::new());
    }

    #[test]
    fn decode_entry_without_equals() {
        let bytes = [3, b'a', b'b', b'c'];
        assert_eq!(txt_decode(&bytes), vec![("abc".to_owned(), String::new())]);
    }

    #[test]
    fn label_truncates_at_char_boundary() {
        let long = "\u{65E5}".repeat(30);
        let t = truncate_label(&long);
        assert_eq!(t.len(), 63);
        let long = format!("a{}", "\u{65E5}".repeat(30));
        let t = truncate_label(&long);
        assert_eq!(t.len() <= 63 && long.is_char_boundary(t.len()), true);
        assert_eq!(truncate_label("short"), "short");
    }

    #[test]
    fn regtype_matches_service_type() {
        assert_eq!(REGTYPE.to_bytes(), SERVICE_TYPE.as_bytes());
    }
}
