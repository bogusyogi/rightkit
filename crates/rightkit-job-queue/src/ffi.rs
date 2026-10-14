//! C ABI over [`EngineSupervisor`] and `rightkit-daemon`, for a Swift host (`PulseNativeABI` in
//! `swift/RightKitSwift/Sources/RightKitPulse/PulseABI.swift`). Compiled with the `ffi` feature;
//! nothing links it into Swift yet.
//!
//! Design (READINESS, "LEGION CHOSEN Pulse Swift facade"):
//!
//! * **Handles are opaque `u64` ids**, issued from a counter that never repeats within the
//!   process, so a released or stale handle is refused (`RKP_STALE_HANDLE`) and can never alias a
//!   newer one. Rust owns every allocation: out buffers are freed with [`rkp_buf_free`], hosts and
//!   connections with `rkp_release_*` (idempotent). A call holds an `Arc` of its host, so a
//!   release during a call frees the host only after the call has reached its terminal result.
//! * **Every blocking call takes a relative `timeout_ms` fixed at entry** and turns it into one
//!   monotonic [`Instant`] deadline covering admission, connect, write and reply.
//! * **Event callbacks** receive borrowed bytes valid for the callback only. A call is
//!   quiescent when [`rkp_host_call`] returns: the callback slot is closed under the same lock a
//!   callback runs under, so no callback is running or can start afterwards and the caller's
//!   context may be freed. A callback must not call back into this ABI and must not block: the
//!   deadline cannot preempt a callback that is already running, so a blocked callback holds
//!   [`rkp_host_call`] past its deadline until the callback returns.
//! * **Out parameters** (`out_host`, `out_conn`, `out` buffers) are zeroed on entry once their
//!   pointer is validated, so a failed call never leaves a stale handle or buffer behind; a prior
//!   value is overwritten, not freed.
//! * **Generation fencing.** The snapshot registered with [`rkp_register_snapshot`] carries the
//!   caller's generation. A host records the generation it started with; once a newer one is
//!   registered the host reports `recovering` and refuses calls (`RKP_STALE_GENERATION`) until it is
//!   replaced. A registration older than the current one is refused. The engine process generation
//!   inside the supervisor is fenced separately by `EngineSupervisor` itself.
//! * **Bounded reaping.** A shutdown that outlives its budget reports `Unconfirmed` with the
//!   unreaped count and the host stays registered while a reaper thread retries a bounded number
//!   of times. At most [`MAX_HOSTS`] hosts (live plus unconfirmed) exist at once, so unreaped
//!   children cannot accumulate without bound: starting another is refused.
//! * **Resume / Never.** `Resume` keeps the engine alive when only the wait times out (the work
//!   may continue; cancelling a wait detaches the observer); `Never` lets the supervisor treat the
//!   timeout as a wedge and kill the engine.
//!
//! The engine command, service names and snapshot method are not part of the ABI: the embedding
//! native library installs a [`HostFactory`] with [`install_factory`] before the first start.

use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::{
    CallOptions, EngineSupervisor, JobError, Lifecycle, ShutdownOutcome, SupervisorConfig,
    Terminal, CONTRACT_VERSION,
};

/// ABI version of this file. Bump on any signature or semantic change.
pub const ABI_VERSION: u32 = 1;
/// Pulse schema this ABI was written against.
pub const SCHEMA_VERSION: u32 = 1;

/// Hosts (live, plus released-but-unreaped) that may exist at once.
pub const MAX_HOSTS: usize = 8;
/// Service connections that may exist at once.
pub const MAX_CONNECTIONS: usize = 16;
/// Largest method name or params payload accepted.
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Largest single event delivered to a callback; a larger one is dropped.
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;
/// Events delivered to a callback per call; later ones are dropped.
pub const MAX_EVENTS_PER_CALL: usize = 4096;
/// Reap retries after a shutdown left an engine process unwaited.
pub const REAP_ATTEMPTS: u32 = 4;

pub const RKP_OK: i32 = 0;
pub const RKP_INVALID_ARGUMENT: i32 = 1;
pub const RKP_NOT_READY: i32 = 2;
pub const RKP_DEADLINE: i32 = 3;
pub const RKP_SHUTTING_DOWN: i32 = 4;
pub const RKP_INCOMPATIBLE: i32 = 5;
pub const RKP_STALE_HANDLE: i32 = 6;
/// An engine or service error; the message is JSON `{"code","message","retryable"}`.
pub const RKP_NATIVE: i32 = 7;
pub const RKP_PANIC: i32 = 8;
pub const RKP_NOT_CONFIGURED: i32 = 9;
pub const RKP_STALE_GENERATION: i32 = 10;
pub const RKP_TOO_MANY_HOSTS: i32 = 11;
/// Service connection cap ([`MAX_CONNECTIONS`]) reached.
pub const RKP_TOO_MANY_CONNECTIONS: i32 = 12;

/// `PulseResumePolicy`: the job survives a detached wait.
pub const RKP_RESUME: u32 = 0;
/// `PulseResumePolicy`: the job dies with its caller.
pub const RKP_NEVER: u32 = 1;

/// Lifecycle codes, in `PulseLifecycle` order.
pub const RKP_BOOTING: u32 = 0;
pub const RKP_READY: u32 = 1;
pub const RKP_RECOVERING: u32 = 2;
pub const RKP_DOWN: u32 = 3;

/// Shutdown outcome codes, in `PulseShutdownOutcome` order.
pub const RKP_SHUTDOWN_NO_ENGINE: u32 = 0;
pub const RKP_SHUTDOWN_ALREADY: u32 = 1;
pub const RKP_SHUTDOWN_EXITED: u32 = 2;
pub const RKP_SHUTDOWN_KILLED: u32 = 3;
pub const RKP_SHUTDOWN_UNCONFIRMED: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RkpVersions {
    pub abi: u32,
    pub contract: u32,
    pub schema: u32,
}

/// Rust-allocated bytes; release with [`rkp_buf_free`]. `ptr` is null when `len` is zero.
#[repr(C)]
#[derive(Debug)]
pub struct RkpBuf {
    pub ptr: *mut u8,
    pub len: usize,
}

impl RkpBuf {
    const EMPTY: RkpBuf = RkpBuf {
        ptr: std::ptr::null_mut(),
        len: 0,
    };
}

#[repr(C)]
#[derive(Debug)]
pub struct RkpError {
    pub code: i32,
    pub message: RkpBuf,
}

/// Event callback: borrowed bytes, valid until it returns.
pub type RkpEventFn = unsafe extern "C" fn(ctx: *mut c_void, ptr: *const u8, len: usize);

/// What the embedding native library supplies: the engine command, the service to talk to, and
/// the method that replays the registered snapshot into each engine generation.
pub trait HostFactory: Send + Sync {
    /// A fresh supervisor configuration for each host.
    fn supervisor_config(&self) -> SupervisorConfig;
    /// Engine method that receives the registered snapshot (default `init`).
    fn snapshot_method(&self) -> String {
        "init".into()
    }
    /// How long `rkp_host_start` waits for the first `Ready` (default 60 s).
    fn start_budget(&self) -> Duration {
        Duration::from_secs(60)
    }
    /// Connect the service client, bounded by `timeout`.
    fn connect_service(
        &self,
        timeout: Duration,
    ) -> Result<rightkit_daemon::Client, rightkit_daemon::ServiceError>;
}

struct Fail {
    code: i32,
    message: String,
}

impl Fail {
    fn new(code: i32, message: impl Into<String>) -> Fail {
        Fail {
            code,
            message: message.into(),
        }
    }
}

struct HostEntry {
    sup: EngineSupervisor,
    /// Snapshot generation this host was started with.
    applied_generation: u64,
    shut: AtomicBool,
    reaping: AtomicBool,
}

struct ConnEntry {
    host: u64,
    client: Client,
}

type Client = rightkit_daemon::Client;

#[derive(Default)]
struct Global {
    factory: Option<Arc<dyn HostFactory>>,
    registered: Option<(u64, Vec<u8>)>,
    next_id: u64,
    hosts: HashMap<u64, Arc<HostEntry>>,
    conns: HashMap<u64, Arc<ConnEntry>>,
    /// Released hosts whose engine process was not confirmed reaped.
    retained: Vec<Arc<HostEntry>>,
    /// `rkp_host_start` calls that passed admission and have not yet inserted (or given up).
    starting: usize,
}

fn global() -> MutexGuard<'static, Global> {
    static G: OnceLock<Mutex<Global>> = OnceLock::new();
    G.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

impl Global {
    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
    /// Reserve a start slot under the same lock that checks the cap, so concurrent starts cannot
    /// all pass admission and then all insert.
    fn reserve_start(&mut self) -> Result<(), Fail> {
        self.prune_retained();
        if !admit(self.hosts.len() + self.starting, self.retained.len()) {
            return Err(Fail::new(
                RKP_TOO_MANY_HOSTS,
                "too many hosts, or engines not yet confirmed reaped",
            ));
        }
        self.starting += 1;
        Ok(())
    }
    fn prune_retained(&mut self) {
        self.retained.retain(|e| e.sup.unreaped_children() > 0);
    }
}

/// Whether one more host fits: live hosts plus unconfirmed released ones stay under the cap.
fn admit(hosts: usize, retained: usize) -> bool {
    hosts + retained < MAX_HOSTS
}

/// Marks one reserved start slot. Dropped on any early exit it releases the reservation; the
/// success path releases it under the insert lock and forgets it.
struct StartSlot;

impl Drop for StartSlot {
    fn drop(&mut self) {
        let mut g = global();
        g.starting = g.starting.saturating_sub(1);
    }
}

/// Install the factory the next `rkp_host_start` uses. Replaces an earlier one.
pub fn install_factory(factory: Arc<dyn HostFactory>) {
    global().factory = Some(factory);
}

/// Hosts released while their engine was still unreaped, and not yet confirmed gone.
#[doc(hidden)]
pub fn retained_count() -> usize {
    let mut g = global();
    g.prune_retained();
    g.retained.len()
}

/// Forget the registered snapshot (tests only; hosts are untouched).
#[doc(hidden)]
pub fn reset_registration() {
    global().registered = None;
}

// ---- buffers and argument plumbing --------------------------------------------------------

fn make_buf(v: Vec<u8>) -> RkpBuf {
    if v.is_empty() {
        return RkpBuf::EMPTY;
    }
    let b = v.into_boxed_slice();
    let len = b.len();
    RkpBuf {
        ptr: Box::into_raw(b) as *mut u8,
        len,
    }
}

/// Free a buffer this library returned and zero it, so freeing twice is a no-op.
///
/// # Safety
/// `buf` is null or points to an `RkpBuf` this library filled (or a zeroed one).
#[no_mangle]
pub unsafe extern "C" fn rkp_buf_free(buf: *mut RkpBuf) {
    if buf.is_null() {
        return;
    }
    let b = &mut *buf;
    if !b.ptr.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            b.ptr, b.len,
        )));
    }
    b.ptr = std::ptr::null_mut();
    b.len = 0;
}

unsafe fn input<'a>(ptr: *const u8, len: usize, what: &str) -> Result<&'a [u8], Fail> {
    if len > MAX_INPUT_BYTES {
        return Err(Fail::new(
            RKP_INVALID_ARGUMENT,
            format!("{what} is too large"),
        ));
    }
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(Fail::new(RKP_INVALID_ARGUMENT, format!("{what} is null")));
    }
    Ok(std::slice::from_raw_parts(ptr, len))
}

fn out_ref<'a, T>(p: *mut T, what: &str) -> Result<&'a mut T, Fail> {
    if p.is_null() {
        Err(Fail::new(RKP_INVALID_ARGUMENT, format!("{what} is null")))
    } else {
        Ok(unsafe { &mut *p })
    }
}

fn utf8(b: &[u8], what: &str) -> Result<String, Fail> {
    std::str::from_utf8(b)
        .map(str::to_owned)
        .map_err(|_| Fail::new(RKP_INVALID_ARGUMENT, format!("{what} is not UTF-8")))
}

fn json_params(b: &[u8]) -> Result<Value, Fail> {
    if b.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(b)
        .map_err(|e| Fail::new(RKP_INVALID_ARGUMENT, format!("params are not JSON: {e}")))
}

/// Run `f` with panics contained and the error (if any) written to `err`.
fn run(err: *mut RkpError, f: impl FnOnce() -> Result<(), Fail>) -> i32 {
    if !err.is_null() {
        unsafe {
            std::ptr::write(
                err,
                RkpError {
                    code: RKP_OK,
                    message: RkpBuf::EMPTY,
                },
            )
        };
    }
    let fail = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => return RKP_OK,
        Ok(Err(f)) => f,
        Err(_) => Fail::new(RKP_PANIC, "panic in native call"),
    };
    if !err.is_null() {
        unsafe {
            std::ptr::write(
                err,
                RkpError {
                    code: fail.code,
                    message: make_buf(fail.message.into_bytes()),
                },
            )
        };
    }
    fail.code
}

fn deadline_in(timeout_ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(timeout_ms.max(1))
}

fn job_fail(e: JobError) -> Fail {
    match e.code.as_str() {
        "timeout" => Fail::new(RKP_DEADLINE, e.message),
        "not_ready" => Fail::new(RKP_NOT_READY, e.message),
        "down" if e.message.contains("shut down") => Fail::new(RKP_SHUTTING_DOWN, e.message),
        "down" => Fail::new(RKP_NOT_READY, e.message),
        _ => Fail::new(
            RKP_NATIVE,
            json!({"code": e.code, "message": e.message, "retryable": e.retryable}).to_string(),
        ),
    }
}

fn service_fail(e: rightkit_daemon::ServiceError) -> Fail {
    if e.code == rightkit_daemon::ERROR_TIMEOUT {
        return Fail::new(RKP_DEADLINE, e.message);
    }
    Fail::new(
        RKP_NATIVE,
        json!({"code": e.code, "message": e.message, "retryable": e.retryable}).to_string(),
    )
}

fn lookup_host(id: u64) -> Result<Arc<HostEntry>, Fail> {
    global()
        .hosts
        .get(&id)
        .cloned()
        .ok_or_else(|| Fail::new(RKP_STALE_HANDLE, "unknown or released host handle"))
}

fn lifecycle_code(l: Lifecycle) -> u32 {
    match l {
        Lifecycle::Booting => RKP_BOOTING,
        Lifecycle::Ready => RKP_READY,
        Lifecycle::Recovering => RKP_RECOVERING,
        Lifecycle::Down => RKP_DOWN,
    }
}

/// The registered snapshot generation moved past the one this host started with.
fn fenced(e: &HostEntry) -> bool {
    global()
        .registered
        .as_ref()
        .is_some_and(|(generation, _)| *generation != e.applied_generation)
}

/// A fenced host is never reported `Ready`: its engine runs an out-of-date snapshot.
fn effective_lifecycle(e: &HostEntry) -> u32 {
    let l = lifecycle_code(e.sup.lifecycle());
    if l == RKP_READY && fenced(e) {
        RKP_RECOVERING
    } else {
        l
    }
}

// ---- ABI entry points ---------------------------------------------------------------------

/// Versions of this ABI, the `rightkit-job-queue` contract and the Pulse schema.
///
/// # Safety
/// `out` is null or valid for one `RkpVersions`.
#[no_mangle]
pub unsafe extern "C" fn rkp_versions(out: *mut RkpVersions) -> i32 {
    run(std::ptr::null_mut(), || {
        *out_ref(out, "out")? = RkpVersions {
            abi: ABI_VERSION,
            contract: CONTRACT_VERSION,
            schema: SCHEMA_VERSION,
        };
        Ok(())
    })
}

/// Register the complete initialization snapshot (JSON) before `rkp_host_start`. A generation
/// older than the registered one, or the same generation with different bytes, is refused.
///
/// # Safety
/// `snapshot` points to `len` readable bytes; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_register_snapshot(
    snapshot: *const u8,
    len: usize,
    generation: u64,
    err: *mut RkpError,
) -> i32 {
    run(err, || {
        let bytes = input(snapshot, len, "snapshot")?;
        json_params(bytes)?;
        let mut g = global();
        if let Some((current, existing)) = &g.registered {
            if generation < *current || (generation == *current && existing != bytes) {
                return Err(Fail::new(
                    RKP_STALE_GENERATION,
                    format!("snapshot generation {generation} is behind registered {current}"),
                ));
            }
        }
        g.registered = Some((generation, bytes.to_vec()));
        Ok(())
    })
}

/// Start the host and return once its engine is `Ready` with the registered snapshot replayed.
///
/// # Safety
/// `out_host` is valid for one `u64`; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_host_start(out_host: *mut u64, err: *mut RkpError) -> i32 {
    run(err, || {
        let out = out_ref(out_host, "out_host")?;
        *out = 0;
        let (factory, generation, snapshot, _slot) = {
            let mut g = global();
            g.prune_retained();
            let factory = g
                .factory
                .clone()
                .ok_or_else(|| Fail::new(RKP_NOT_CONFIGURED, "no host factory installed"))?;
            let (generation, bytes) = g
                .registered
                .clone()
                .ok_or_else(|| Fail::new(RKP_NOT_READY, "register the snapshot before starting"))?;
            g.reserve_start()?;
            (factory, generation, bytes, StartSlot)
        };
        let params = json_params(&snapshot)?;
        let sup = EngineSupervisor::new(factory.supervisor_config());
        sup.set_snapshot("rkp-init", &factory.snapshot_method(), params)
            .map_err(job_fail)?;
        let readiness = sup.start_background();
        let entry = Arc::new(HostEntry {
            sup,
            applied_generation: generation,
            shut: AtomicBool::new(false),
            reaping: AtomicBool::new(false),
        });
        match readiness.wait_timeout(factory.start_budget()) {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                retire(entry);
                return Err(job_fail(e));
            }
            None => {
                retire(entry);
                return Err(Fail::new(
                    RKP_DEADLINE,
                    "engine did not become ready in time",
                ));
            }
        }
        let mut g = global();
        let id = g.fresh_id();
        g.hosts.insert(id, entry);
        // The slot moves from "starting" to "inserted" under this one lock; `_slot` then drops
        // without releasing again.
        g.starting = g.starting.saturating_sub(1);
        std::mem::forget(_slot);
        *out = id;
        Ok(())
    })
}

/// Snapshot generation, lifecycle code and a JSON status payload.
///
/// # Safety
/// Out pointers are valid for one value each; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_host_snapshot(
    host: u64,
    out_generation: *mut u64,
    out_lifecycle: *mut u32,
    out_payload: *mut RkpBuf,
    err: *mut RkpError,
) -> i32 {
    run(err, || {
        let (og, ol, op) = (
            out_ref(out_generation, "out_generation")?,
            out_ref(out_lifecycle, "out_lifecycle")?,
            out_ref(out_payload, "out_payload")?,
        );
        let e = lookup_host(host)?;
        let st = e.sup.status();
        let terminal = match &st.terminal {
            None => Value::Null,
            Some(Terminal::Stopped) => json!({"state": "stopped"}),
            Some(Terminal::Failed { reason }) => json!({"state": "failed", "reason": reason}),
        };
        let payload = json!({
            "engine_generation": st.generation,
            "attempt": st.attempt,
            "restarts": st.restarts,
            "pid": st.pid,
            "terminal": terminal,
            "last_exit_reason": st.last_exit_reason,
            "unreaped": e.sup.unreaped_children(),
            "fenced": fenced(&e),
        });
        *og = e.applied_generation;
        *ol = effective_lifecycle(&e);
        *op = make_buf(payload.to_string().into_bytes());
        Ok(())
    })
}

/// Event delivery for one call. A callback runs under `state`'s lock; `close` takes the same
/// lock, so once `close` returns no callback is running or can start.
struct CbSlot {
    state: Mutex<SlotState>,
}

struct SlotState {
    open: bool,
    cb: Option<RkpEventFn>,
    ctx: usize,
    delivered: usize,
}

impl CbSlot {
    fn deliver(&self, v: &Value) {
        let Ok(bytes) = serde_json::to_vec(v) else {
            return;
        };
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.open || bytes.len() > MAX_EVENT_BYTES || s.delivered >= MAX_EVENTS_PER_CALL {
            return;
        }
        if let Some(cb) = s.cb {
            s.delivered += 1;
            // SAFETY: the caller of `rkp_host_call` promised `cb` and `ctx` stay valid until it
            // returns, and `close` (which it returns through) waits for this lock.
            unsafe { cb(s.ctx as *mut c_void, bytes.as_ptr(), bytes.len()) };
        }
    }
    fn close(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).open = false;
    }
}

/// Call an engine method under one deadline. `resume` is [`RKP_RESUME`] or [`RKP_NEVER`].
/// `on_event` (may be null) receives each progress event's JSON, borrowed for the callback only.
/// On success `out` holds the result JSON.
///
/// # Safety
/// Byte pointers are valid for their lengths; `on_event` and `ctx` stay valid until this returns;
/// `out` is valid for one `RkpBuf`; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_host_call(
    host: u64,
    method: *const u8,
    method_len: usize,
    params: *const u8,
    params_len: usize,
    resume: u32,
    timeout_ms: u64,
    on_event: Option<RkpEventFn>,
    ctx: *mut c_void,
    out: *mut RkpBuf,
    err: *mut RkpError,
) -> i32 {
    run(err, || {
        let deadline = deadline_in(timeout_ms);
        let out = out_ref(out, "out")?;
        *out = RkpBuf::EMPTY;
        let method = utf8(input(method, method_len, "method")?, "method")?;
        let params = json_params(input(params, params_len, "params")?)?;
        if resume != RKP_RESUME && resume != RKP_NEVER {
            return Err(Fail::new(RKP_INVALID_ARGUMENT, "unknown resume policy"));
        }
        let e = lookup_host(host)?;
        if e.shut.load(Ordering::SeqCst) {
            return Err(Fail::new(RKP_SHUTTING_DOWN, "host is shutting down"));
        }
        if fenced(&e) {
            return Err(Fail::new(
                RKP_STALE_GENERATION,
                "host runs an older snapshot generation than the one registered",
            ));
        }
        let slot = Arc::new(CbSlot {
            state: Mutex::new(SlotState {
                open: true,
                cb: on_event,
                ctx: ctx as usize,
                delivered: 0,
            }),
        });
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (tx, rx) = mpsc::channel();
        let (worker_entry, worker_slot) = (Arc::clone(&e), Arc::clone(&slot));
        let spawned = std::thread::Builder::new()
            .name("rkp-call".into())
            .spawn(move || {
                let opts = CallOptions {
                    timeout: Some(remaining.max(Duration::from_millis(1))),
                    on_event: Some(Box::new(move |v: &Value| worker_slot.deliver(v))),
                    keep_alive_on_timeout: resume == RKP_RESUME,
                    ..CallOptions::default()
                };
                let _ = tx.send(worker_entry.sup.call_with(&method, params, opts));
            });
        if spawned.is_err() {
            return Err(Fail::new(RKP_NATIVE, "could not start the call thread"));
        }
        let result = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        // Quiescence: after this no callback is running or can start.
        slot.close();
        match result {
            Ok(Ok(v)) => {
                *out = make_buf(v.to_string().into_bytes());
                Ok(())
            }
            Ok(Err(e)) => Err(job_fail(e)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(Fail::new(RKP_DEADLINE, "call exceeded its deadline"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Fail::new(RKP_PANIC, "call thread ended without a result"))
            }
        }
    })
}

/// Block until the lifecycle differs from `since` (`-1` = any) or `timeout_ms` passes
/// (`RKP_DEADLINE`). `out_lifecycle` is `-1` when the host was released.
///
/// # Safety
/// `out_lifecycle` is valid for one `i32`; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_host_lifecycle(
    host: u64,
    since: i32,
    timeout_ms: u64,
    out_lifecycle: *mut i32,
    err: *mut RkpError,
) -> i32 {
    run(err, || {
        let deadline = deadline_in(timeout_ms);
        let out = out_ref(out_lifecycle, "out_lifecycle")?;
        loop {
            let entry = global().hosts.get(&host).cloned();
            let Some(e) = entry else {
                *out = -1;
                return Ok(());
            };
            let now = effective_lifecycle(&e) as i32;
            if now != since {
                *out = now;
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Fail::new(RKP_DEADLINE, "lifecycle did not change in time"));
            }
            std::thread::sleep(left.min(Duration::from_millis(10)));
        }
    })
}

fn outcome_code(o: ShutdownOutcome) -> u32 {
    match o {
        ShutdownOutcome::NoEngine => RKP_SHUTDOWN_NO_ENGINE,
        ShutdownOutcome::AlreadyShuttingDown => RKP_SHUTDOWN_ALREADY,
        ShutdownOutcome::Exited => RKP_SHUTDOWN_EXITED,
        ShutdownOutcome::Killed => RKP_SHUTDOWN_KILLED,
        ShutdownOutcome::Unconfirmed => RKP_SHUTDOWN_UNCONFIRMED,
    }
}

/// Shut the shared host down, answering within `budget_ms`. A process still unwaited at the
/// budget reports `RKP_SHUTDOWN_UNCONFIRMED` with its count; the host stays registered and a
/// bounded reaper keeps trying. The first caller shuts down; later ones report `ALREADY`.
///
/// # Safety
/// Out pointers are valid for one value each; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_host_shutdown(
    host: u64,
    budget_ms: u64,
    out_outcome: *mut u32,
    out_unreaped: *mut u64,
    err: *mut RkpError,
) -> i32 {
    run(err, || {
        let deadline = deadline_in(budget_ms);
        let (oo, ou) = (
            out_ref(out_outcome, "out_outcome")?,
            out_ref(out_unreaped, "out_unreaped")?,
        );
        let e = lookup_host(host)?;
        if e.shut.swap(true, Ordering::SeqCst) {
            *oo = RKP_SHUTDOWN_ALREADY;
            *ou = e.sup.unreaped_children() as u64;
            return Ok(());
        }
        let (tx, rx) = mpsc::channel();
        spawn_reaper(Arc::clone(&e), Some(tx));
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(rep) => {
                *oo = outcome_code(rep.outcome);
                *ou = rep.unreaped as u64;
            }
            Err(_) => {
                *oo = RKP_SHUTDOWN_UNCONFIRMED;
                *ou = e.sup.unreaped_children().max(1) as u64;
            }
        }
        Ok(())
    })
}

/// Shut `e` down on its own thread and retry the reap a bounded number of times; the first
/// report goes to `first`. The thread owns an `Arc`, so the host outlives a released handle.
fn spawn_reaper(e: Arc<HostEntry>, first: Option<mpsc::Sender<crate::ShutdownReport>>) {
    if e.reaping.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("rkp-reap".into()).spawn({
        let e = Arc::clone(&e);
        move || {
            let mut report = e.sup.shutdown_detailed();
            if let Some(tx) = first {
                let _ = tx.send(report.clone());
            }
            let mut pause = Duration::from_millis(250);
            for _ in 0..REAP_ATTEMPTS {
                if report.unreaped == 0 && e.sup.unreaped_children() == 0 {
                    break;
                }
                std::thread::sleep(pause);
                pause *= 2;
                report = e.sup.shutdown_detailed();
            }
        }
    });
    if spawned.is_err() {
        e.reaping.store(false, Ordering::SeqCst);
    }
}

/// Stop `e` in the background and keep it counted until its engine is confirmed reaped.
fn retire(e: Arc<HostEntry>) {
    e.shut.store(true, Ordering::SeqCst);
    spawn_reaper(Arc::clone(&e), None);
    let mut g = global();
    g.retained.push(e);
    g.prune_retained();
}

/// Connect the service client for this host, bounded by `timeout_ms`.
///
/// # Safety
/// `out_conn` is valid for one `u64`; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_service_connect(
    host: u64,
    timeout_ms: u64,
    out_conn: *mut u64,
    err: *mut RkpError,
) -> i32 {
    run(err, || {
        let deadline = deadline_in(timeout_ms);
        let out = out_ref(out_conn, "out_conn")?;
        *out = 0;
        let e = lookup_host(host)?;
        if e.shut.load(Ordering::SeqCst) {
            return Err(Fail::new(RKP_SHUTTING_DOWN, "host is shutting down"));
        }
        let factory = global()
            .factory
            .clone()
            .ok_or_else(|| Fail::new(RKP_NOT_CONFIGURED, "no host factory installed"))?;
        if global().conns.len() >= MAX_CONNECTIONS {
            return Err(Fail::new(
                RKP_TOO_MANY_CONNECTIONS,
                "too many service connections",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("rkp-connect".into())
            .spawn(move || {
                let _ = tx.send(factory.connect_service(remaining));
            });
        if spawned.is_err() {
            return Err(Fail::new(RKP_NATIVE, "could not start the connect thread"));
        }
        let client = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return Err(service_fail(e)),
            // The helper finishes on its own (connect is bounded); its client is dropped there.
            Err(_) => {
                return Err(Fail::new(
                    RKP_DEADLINE,
                    "service connect exceeded its deadline",
                ))
            }
        };
        let mut g = global();
        if !g.hosts.contains_key(&host) {
            return Err(Fail::new(
                RKP_STALE_HANDLE,
                "host was released during connect",
            ));
        }
        // Re-check under the insert lock: the early check above is only a fast refusal.
        if g.conns.len() >= MAX_CONNECTIONS {
            return Err(Fail::new(
                RKP_TOO_MANY_CONNECTIONS,
                "too many service connections",
            ));
        }
        let id = g.fresh_id();
        g.conns.insert(id, Arc::new(ConnEntry { host, client }));
        *out = id;
        Ok(())
    })
}

/// Call a service method under one deadline covering waiting for the connection, the write and
/// the reply. On success `out` holds the result JSON.
///
/// # Safety
/// Byte pointers are valid for their lengths; `out` is valid for one `RkpBuf`; `err` is null or
/// valid.
#[no_mangle]
pub unsafe extern "C" fn rkp_service_call(
    conn: u64,
    method: *const u8,
    method_len: usize,
    params: *const u8,
    params_len: usize,
    timeout_ms: u64,
    out: *mut RkpBuf,
    err: *mut RkpError,
) -> i32 {
    run(err, || {
        let deadline = deadline_in(timeout_ms);
        let out = out_ref(out, "out")?;
        *out = RkpBuf::EMPTY;
        let method = utf8(input(method, method_len, "method")?, "method")?;
        let params = json_params(input(params, params_len, "params")?)?;
        let c =
            global().conns.get(&conn).cloned().ok_or_else(|| {
                Fail::new(RKP_STALE_HANDLE, "unknown or released connection handle")
            })?;
        let host = global().hosts.get(&c.host).cloned();
        match host {
            None => {
                return Err(Fail::new(
                    RKP_STALE_HANDLE,
                    "the connection's host was released",
                ))
            }
            Some(h) if h.shut.load(Ordering::SeqCst) => {
                return Err(Fail::new(RKP_SHUTTING_DOWN, "host is shutting down"))
            }
            Some(_) => {}
        }
        let v = c
            .client
            .call_events_until(&method, params, Some(deadline), &mut |_| {})
            .map_err(service_fail)?;
        *out = make_buf(v.to_string().into_bytes());
        Ok(())
    })
}

/// Release a host handle. Idempotent. The host is stopped in the background if it was not shut
/// down, and stays counted until its engine is confirmed reaped.
#[no_mangle]
pub extern "C" fn rkp_release_host(host: u64) {
    let _ = catch_unwind(|| {
        let entry = {
            let mut g = global();
            g.conns.retain(|_, c| c.host != host);
            g.hosts.remove(&host)
        };
        if let Some(e) = entry {
            if e.sup.unreaped_children() > 0 || !e.shut.load(Ordering::SeqCst) {
                retire(e);
            }
        }
    });
}

/// Release a service connection handle. Idempotent.
#[no_mangle]
pub extern "C" fn rkp_release_connection(conn: u64) {
    let _ = catch_unwind(|| {
        global().conns.remove(&conn);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_counts_live_and_unconfirmed_hosts() {
        assert!(admit(0, 0));
        assert!(admit(MAX_HOSTS - 1, 0));
        assert!(!admit(MAX_HOSTS, 0));
        assert!(!admit(MAX_HOSTS - 2, 2));
        assert!(admit(3, 2));
    }

    #[test]
    fn concurrent_starts_cannot_overshoot_the_host_cap() {
        let mut g = Global::default();
        // Reservations count before any host is inserted.
        for _ in 0..MAX_HOSTS {
            assert!(g.reserve_start().is_ok());
        }
        let err = g.reserve_start().unwrap_err();
        assert_eq!(err.code, RKP_TOO_MANY_HOSTS);
        assert_eq!(g.starting, MAX_HOSTS);
    }

    #[test]
    fn connection_cap_has_its_own_code() {
        assert_ne!(RKP_TOO_MANY_CONNECTIONS, RKP_TOO_MANY_HOSTS);
    }

    #[test]
    fn buffers_round_trip_and_free_twice_is_a_no_op() {
        let mut b = make_buf(b"abc".to_vec());
        assert_eq!(b.len, 3);
        assert_eq!(unsafe { std::slice::from_raw_parts(b.ptr, b.len) }, b"abc");
        unsafe {
            rkp_buf_free(&mut b);
            rkp_buf_free(&mut b);
        }
        assert!(b.ptr.is_null() && b.len == 0);
        let e = make_buf(Vec::new());
        assert!(e.ptr.is_null() && e.len == 0);
    }

    #[test]
    fn handle_ids_never_repeat() {
        let mut g = Global::default();
        let a = g.fresh_id();
        let b = g.fresh_id();
        assert!(a >= 1 && b > a);
    }

    #[test]
    fn job_errors_map_to_distinct_codes() {
        assert_eq!(
            job_fail(JobError::supervisor("timeout", "t")).code,
            RKP_DEADLINE
        );
        assert_eq!(
            job_fail(JobError::supervisor("not_ready", "n")).code,
            RKP_NOT_READY
        );
        assert_eq!(
            job_fail(JobError::supervisor("down", "engine is shut down")).code,
            RKP_SHUTTING_DOWN
        );
        assert_eq!(
            job_fail(JobError::supervisor("down", "engine is down")).code,
            RKP_NOT_READY
        );
        assert_eq!(job_fail(JobError::supervisor("boom", "b")).code, RKP_NATIVE);
    }
}
