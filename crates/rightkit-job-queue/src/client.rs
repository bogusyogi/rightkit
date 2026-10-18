use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rightkit_child_process::{OwnedChild, OwnedCommand, ReapOutcome, WaitOutcome};
use rightkit_sidecar::{
    read_json_line_or_oversized, write_json_line, FrameError, Incoming, ReadLimits,
};
use serde_json::{json, Value};

use crate::error::{ErrorOrigin, JobError, JobResult, DELIVERY_UNKNOWN};
use crate::wire::{finite_frame_limit, DEFAULT_FRAME_LIMIT};

type OnEvent = Box<dyn Fn(&Value) + Send>;

/// One progress event of a call as the engine framed it: the request `id` the
/// frame carries, and its `event` payload. A host that needs the whole frame
/// (`{"id", "event"}`) rebuilds it from these two fields.
#[derive(Debug, Clone, Copy)]
pub struct EventFrame<'a> {
    pub id: u64,
    pub event: &'a Value,
}

/// Progress callback that also receives the request id of each event. Runs on
/// the calling thread, like the payload-only callback it complements.
pub type OnEventFrame = Box<dyn for<'a> Fn(&EventFrame<'a>) + Send>;

/// A call's time budget when it is not one absolute deadline: the call fails with `timeout` when
/// the engine has sent no event *tagged with this call's request id* for `idle`, or (when set) when
/// `max_total` has passed since the request was sent. Only events correlated to the call count as
/// progress: unsolicited events, and events of other requests, never extend it. Use it for a call
/// that is long by design and reports progress; the default for every other call stays one
/// absolute deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressDeadline {
    /// Longest silence tolerated between progress events (and before the first one).
    pub idle: Duration,
    /// Optional absolute cap on the whole call.
    pub max_total: Option<Duration>,
}

impl ProgressDeadline {
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            max_total: None,
        }
    }
    pub fn with_max_total(mut self, max_total: Duration) -> Self {
        self.max_total = Some(max_total);
        self
    }
}

/// Internal: how long a call may wait.
#[derive(Clone, Copy)]
pub(crate) struct Budget {
    absolute: Option<Duration>,
    idle: Option<Duration>,
}

impl Budget {
    pub(crate) fn absolute(timeout: Option<Duration>) -> Self {
        Self {
            absolute: timeout,
            idle: None,
        }
    }
    fn progress(p: ProgressDeadline) -> Self {
        Self {
            absolute: p.max_total,
            idle: Some(p.idle),
        }
    }
}

/// What the reader thread hands a waiting caller. Callbacks run on the
/// caller's own thread, never on the reader and never under a lock.
enum Msg {
    Event(u64, Value),
    Done(JobResult<Value>),
}
struct Pending {
    tx: Sender<Msg>,
    wants_events: bool,
}
type PendingMap = Arc<Mutex<HashMap<u64, Pending>>>;

/// Engine contract version a shell requires, checked against the hello frame's
/// `contract_version`. Matching is by JSON text, so `1u32` and `"1"` both accept
/// an engine that advertises the integer `1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractRequirement(String);

impl ContractRequirement {
    fn accepts(&self, hello: &Value) -> bool {
        match &hello["contract_version"] {
            Value::Number(n) => n.to_string() == self.0,
            Value::String(s) => *s == self.0,
            _ => false,
        }
    }
}

impl From<u32> for ContractRequirement {
    fn from(version: u32) -> Self {
        Self(version.to_string())
    }
}

impl From<&str> for ContractRequirement {
    fn from(version: &str) -> Self {
        Self(version.to_string())
    }
}

impl From<String> for ContractRequirement {
    fn from(version: String) -> Self {
        Self(version)
    }
}

/// Code given to an engine error reply that carries no `code`, unless the host
/// sets another with [`ClientOptions::with_fallback_error_code`].
const ENGINE_ERROR: &str = "engine_error";
/// Message of an engine error reply that carries no usable message.
const NO_DETAIL: &str = "engine reported failure without detail";

/// A stdout line the engine sent that is not a JSON frame. The reader drops it
/// and resynchronises on the next line; a sink sees it first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    /// The malformed line, lossily decoded, without its terminator.
    pub line: String,
    /// Why it was rejected.
    pub message: String,
}

/// Host callback for [`ProtocolError`]s. Runs on the connection's reader
/// thread, so keep it quick (push to a channel, do not block).
#[derive(Clone)]
pub struct ProtocolErrorSink(Arc<dyn Fn(&ProtocolError) + Send + Sync>);

impl ProtocolErrorSink {
    pub fn new(f: impl Fn(&ProtocolError) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
    fn report(&self, error: &ProtocolError) {
        (self.0)(error)
    }
}

impl std::fmt::Debug for ProtocolErrorSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtocolErrorSink")
    }
}

/// Connection tuning for [`EngineClient::connect_opts`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// How long to wait for the engine's hello frame.
    pub hello_timeout: Duration,
    /// Largest frame accepted in either direction (bounded to `MAX_FRAME_LIMIT`). A larger *response* answers
    /// the call it belongs to with `frame_too_large` instead of ending the session.
    pub frame_limit: usize,
    /// When set, a hello frame that does not advertise this contract version
    /// fails the connect with `contract_version_mismatch`.
    pub contract_version: Option<ContractRequirement>,
    /// Code for an engine error reply that has none. Default `engine_error`.
    pub fallback_error_code: String,
    /// Receives each malformed stdout line. Default: dropped silently.
    pub protocol_error_sink: Option<ProtocolErrorSink>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            hello_timeout: DEFAULT_HELLO_TIMEOUT,
            frame_limit: DEFAULT_FRAME_LIMIT,
            contract_version: None,
            fallback_error_code: ENGINE_ERROR.to_string(),
            protocol_error_sink: None,
        }
    }
}

impl ClientOptions {
    /// Refuse an engine whose hello frame does not advertise `version`.
    pub fn require_contract_version(mut self, version: impl Into<ContractRequirement>) -> Self {
        self.contract_version = Some(version.into());
        self
    }
    /// Code for an engine error reply that carries none.
    pub fn with_fallback_error_code(mut self, code: impl Into<String>) -> Self {
        self.fallback_error_code = code.into();
        self
    }
    /// Receives each malformed stdout line instead of dropping it silently.
    pub fn with_protocol_error_sink(
        mut self,
        f: impl Fn(&ProtocolError) + Send + Sync + 'static,
    ) -> Self {
        self.protocol_error_sink = Some(ProtocolErrorSink::new(f));
        self
    }
}

/// Reader-thread signal: set once the engine's stdout has ended.
#[derive(Default)]
struct Closed {
    flag: Mutex<bool>,
    cv: Condvar,
}

/// One frame for the writer thread and the channel its write result goes back on.
type WriteRequest = (Vec<u8>, Sender<std::io::Result<()>>);
type Subscribers = Arc<Mutex<Vec<Sender<Value>>>>;

/// State shared with the reader thread and the bounded-shutdown helper.
struct Core {
    frame_limit: usize,
    closed: Arc<Closed>,
    subscribers: Subscribers,
    /// Frames go to a dedicated writer thread so a full pipe to a wedged engine
    /// blocks that thread, never a caller that has a deadline.
    writes: Mutex<Sender<WriteRequest>>,
    pending: PendingMap,
    next_id: AtomicU64,
    child: Mutex<Option<OwnedChild>>,
    /// Dropped (after `exit` is recorded) once the child has been waited. Declared after
    /// `child` so a client dropped with its child still attached reaps before releasing.
    release: Mutex<Option<Box<dyn Send>>>,
    exit: Mutex<ChildExit>,
    exit_cv: Condvar,
    /// Bound, in milliseconds, on the kill of a child whose request write stalled.
    kill_bound_ms: AtomicU64,
    /// Set before the child is killed for a stalled request write, so whoever observes the exit
    /// can tell it from a plain EOF.
    stalled_write: std::sync::atomic::AtomicBool,
}

/// What is known about the engine process an [`EngineClient`] owns.
///
/// `reaped` means the process was waited: it is neither running nor a zombie. Do not use
/// `kill -0 <pid>` (or any signal-0 probe) to decide that a child of this process is gone:
/// an exited child that has not been waited is a zombie and still answers it. Read this
/// record, [`EngineClient::is_reaped`] or `ShutdownReport` from
/// `EngineSupervisor::shutdown_detailed` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChildExit {
    /// Process id of the attached engine; `None` when no process was ever attached.
    pub pid: Option<u32>,
    /// True once the process has been waited (reaped).
    pub reaped: bool,
    /// True when the tree had to be killed rather than leaving by itself.
    pub killed: bool,
    /// Exit status when the wait observed one.
    pub status: Option<std::process::ExitStatus>,
}

/// Shell-side client for an engine speaking the stdio contract. Multiplexes
/// concurrent calls; events of a call stream to its callback (on the calling
/// thread) before it returns. Restart/health policy stays with
/// `rightkit-child-process`; this only talks frames.
pub struct EngineClient {
    core: Arc<Core>,
    hello: Value,
}

/// How long a stalled request write waits for the owned child to be reaped after it is killed,
/// unless [`EngineClient::set_kill_bound`] says otherwise.
pub const DEFAULT_KILL_BOUND: Duration = Duration::from_secs(2);

/// How long `spawn`/`connect` wait for the engine's hello frame by default.
pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(30);

impl EngineClient {
    /// Connect over an existing duplex; reads the engine's hello frame first
    /// (waiting at most [`DEFAULT_HELLO_TIMEOUT`]).
    pub fn connect(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> JobResult<Self> {
        Self::connect_with(reader, writer, DEFAULT_HELLO_TIMEOUT)
    }

    /// As [`Self::connect`] with an explicit hello timeout; expiry returns a
    /// `hello_timeout` error.
    pub fn connect_with(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        hello_timeout: Duration,
    ) -> JobResult<Self> {
        Self::connect_opts(
            reader,
            writer,
            ClientOptions {
                hello_timeout,
                ..ClientOptions::default()
            },
        )
    }

    /// As [`Self::connect`] with explicit [`ClientOptions`].
    pub fn connect_opts(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        options: ClientOptions,
    ) -> JobResult<Self> {
        let ClientOptions {
            hello_timeout,
            frame_limit,
            contract_version,
            fallback_error_code,
            protocol_error_sink,
        } = options;
        let frame_limit = finite_frame_limit(frame_limit);
        let mut reader = reader;
        let pending: PendingMap = Arc::default();
        let map = Arc::clone(&pending);
        let closed: Arc<Closed> = Arc::default();
        let closed_signal = Arc::clone(&closed);
        let subscribers: Subscribers = Arc::default();
        let subs = Arc::clone(&subscribers);
        let (hello_tx, hello_rx) = channel::<JobResult<Value>>();
        let required = contract_version;
        let fallback = fallback_error_code;
        let sink = protocol_error_sink;
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let limits = ReadLimits::default();
            // Whatever ends this thread, mark the connection closed and release
            // every waiter and subscriber.
            struct OnExit(Arc<Closed>, PendingMap, Subscribers);
            impl Drop for OnExit {
                fn drop(&mut self) {
                    for (_, p) in self.1.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                        let _ = p.tx.send(Msg::Done(Err(JobError::supervisor(
                            "engine_exited",
                            "engine closed the connection",
                        ))));
                    }
                    self.2.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    *self.0.flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    self.0.cv.notify_all();
                }
            }
            let _on_exit = OnExit(closed_signal, Arc::clone(&map), subs.clone());
            let first =
                read_json_line_or_oversized::<_, Value>(&mut reader, &mut buf, frame_limit, limits);
            let hello = match first {
                Ok(Some(Incoming::Frame(h))) if h["type"] == "hello" => match &required {
                    Some(req) if !req.accepts(&h) => {
                        Err(JobError::contract_mismatch(&h["contract_version"], &req.0))
                    }
                    _ => Ok(h),
                },
                Ok(Some(Incoming::Frame(_))) => Err(JobError::supervisor(
                    "engine_protocol",
                    "first frame was not hello",
                )),
                Ok(Some(Incoming::Oversized(_))) => Err(JobError::supervisor(
                    "engine_protocol",
                    "hello frame exceeded the frame limit",
                )),
                Ok(None) => Err(JobError::supervisor(
                    "engine_exited",
                    "engine closed before hello",
                )),
                Err(e) => {
                    report_protocol(&sink, &buf, &e);
                    Err(JobError::supervisor("engine_protocol", e.to_string()))
                }
            };
            let ok = hello.is_ok();
            let _ = hello_tx.send(hello);
            if !ok {
                return;
            }
            loop {
                match read_json_line_or_oversized::<_, Value>(
                    &mut reader,
                    &mut buf,
                    frame_limit,
                    limits,
                ) {
                    Ok(Some(Incoming::Frame(v))) => dispatch(&map, &subs, &v, &fallback),
                    Ok(Some(Incoming::Oversized(over))) => {
                        // Answer the call this frame belongs to; the child is healthy.
                        if let Some(id) = over.id() {
                            let removed = map.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                            if let Some(p) = removed {
                                let _ = p.tx.send(Msg::Done(Err(JobError::supervisor(
                                    "frame_too_large",
                                    format!("engine frame of {} bytes exceeded the {frame_limit}-byte limit", over.bytes),
                                ))));
                            }
                        }
                    }
                    Ok(None) | Err(FrameError::Io(_)) => break,
                    // Malformed line: the reader resynchronises on the next frame.
                    Err(e) => report_protocol(&sink, &buf, &e),
                }
            }
        });
        let hello = match hello_rx.recv_timeout(hello_timeout) {
            Ok(r) => r?,
            Err(_) => {
                return Err(JobError::supervisor(
                    "hello_timeout",
                    format!("engine sent no hello within {hello_timeout:?}"),
                ));
            }
        };
        let (wtx, wrx) = channel::<(Vec<u8>, Sender<std::io::Result<()>>)>();
        let mut writer = writer;
        std::thread::spawn(move || {
            for (mut bytes, reply) in wrx {
                let r = writer.write_all(&bytes).and_then(|_| writer.flush());
                // Outbound frames may carry secrets in params: wipe before drop.
                bytes.iter_mut().for_each(|b| *b = 0);
                std::hint::black_box(&bytes);
                let _ = reply.send(r);
            }
        });
        Ok(Self {
            core: Arc::new(Core {
                frame_limit,
                closed,
                subscribers,
                writes: Mutex::new(wtx),
                pending,
                next_id: AtomicU64::new(1),
                child: Mutex::new(None),
                release: Mutex::new(None),
                exit: Mutex::new(ChildExit::default()),
                exit_cv: Condvar::new(),
                kill_bound_ms: AtomicU64::new(DEFAULT_KILL_BOUND.as_millis() as u64),
                stalled_write: std::sync::atomic::AtomicBool::new(false),
            }),
            hello,
        })
    }

    /// Unsolicited events (`{"type":"event","event":..}` frames with no `id`,
    /// e.g. HeardRight's `recording_level` keyed by `session_id`). Each frame is
    /// delivered whole so correlation fields survive. The receiver ends when the
    /// connection does.
    pub fn subscribe(&self) -> Receiver<Value> {
        let (tx, rx) = channel();
        if !self.is_closed() {
            self.core
                .subscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(tx);
        }
        rx
    }

    /// True once the engine's stdout has ended (exit, crash, or pipe error).
    pub fn is_closed(&self) -> bool {
        *self
            .core
            .closed
            .flag
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Block until the connection ends or `timeout` passes; true if it ended.
    pub fn wait_closed(&self, timeout: Duration) -> bool {
        let g = self
            .core
            .closed
            .flag
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (g, _) = self
            .core
            .closed
            .cv
            .wait_timeout_while(g, timeout, |closed| !*closed)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }

    /// Take ownership of the engine process so shutdown/kill/reap act on it.
    pub fn attach_child(&self, child: OwnedChild) {
        self.core.exit.lock().unwrap_or_else(|e| e.into_inner()).pid = Some(child.id());
        *self.core.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    }

    /// As [`Self::attach_child`]; `release` is dropped once the child has been waited.
    pub(crate) fn attach_child_with(&self, child: OwnedChild, release: Box<dyn Send>) {
        *self.core.release.lock().unwrap_or_else(|e| e.into_inner()) = Some(release);
        self.attach_child(child);
    }

    /// The attached engine process's state: pid, whether it was waited, whether it was
    /// killed, and its exit status when observed. See [`ChildExit`] for why this, and not
    /// `kill -0`, is the liveness check for a child of this process.
    pub fn child_exit(&self) -> ChildExit {
        *self.core.exit.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Exit status of the attached engine once it was waited and a status was observed.
    pub fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.child_exit().status
    }

    /// True when no process is attached or the attached one has been waited (not a zombie).
    pub fn is_reaped(&self) -> bool {
        let e = self.child_exit();
        e.pid.is_none() || e.reaped
    }

    /// Block until the attached engine has been waited or `timeout` passes; true when
    /// [`Self::is_reaped`]. Returns at once when no process is attached.
    pub fn wait_reaped(&self, timeout: Duration) -> bool {
        let g = self.core.exit.lock().unwrap_or_else(|e| e.into_inner());
        let (g, _) = self
            .core
            .exit_cv
            .wait_timeout_while(g, timeout, |e| e.pid.is_some() && !e.reaped)
            .unwrap_or_else(|e| e.into_inner());
        g.pid.is_none() || g.reaped
    }

    /// Bound the kill of an owned child whose request write stalled (default
    /// [`DEFAULT_KILL_BOUND`]). The supervisor sets it to its `shutdown_grace`.
    pub fn set_kill_bound(&self, bound: Duration) {
        self.core.kill_bound_ms.store(
            bound.as_millis().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    /// PID of the attached engine process, if any.
    pub fn child_id(&self) -> Option<u32> {
        self.core
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(OwnedChild::id)
    }

    /// Kill the attached engine tree now and wait at most `bound` for the OS to
    /// report it. Returns the exit status when observed. Used to recover from a
    /// wedged engine; the connection then closes and waiters are released.
    /// True once a stalled request write made this client kill its engine.
    pub fn write_stalled(&self) -> bool {
        self.core.stalled_write.load(Ordering::SeqCst)
    }

    pub fn kill(&self, bound: Duration) -> Option<std::process::ExitStatus> {
        let mut child = self
            .core
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        let status = match child.terminate_tree_bounded(bound) {
            Ok(ReapOutcome::Exited(status)) => Some(status),
            _ => None,
        };
        self.core.retire(child, status, true);
        status
    }

    /// Collect the engine's exit: wait up to `patience` for it to leave on its
    /// own, then kill the tree. Returns the exit status when observed.
    pub fn reap(&self, patience: Duration) -> Option<std::process::ExitStatus> {
        let mut child = self
            .core
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        if let Ok(Some(status)) = child.wait_timeout(patience) {
            self.core.retire(child, Some(status), false);
            return Some(status);
        }
        let status = match child.terminate_tree_bounded(patience) {
            Ok(ReapOutcome::Exited(status)) => Some(status),
            _ => None,
        };
        self.core.retire(child, status, true);
        status
    }

    /// Spawn an engine child (stdin/stdout are piped here) and connect, waiting
    /// at most [`DEFAULT_HELLO_TIMEOUT`] for hello.
    pub fn spawn(cmd: OwnedCommand) -> JobResult<Self> {
        Self::spawn_with(cmd, DEFAULT_HELLO_TIMEOUT)
    }

    /// As [`Self::spawn`]; if no hello arrives within `hello_timeout` the child
    /// is killed and a `hello_timeout` error returned.
    pub fn spawn_with(mut cmd: OwnedCommand, hello_timeout: Duration) -> JobResult<Self> {
        use std::process::Stdio;
        cmd.command_mut()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdin = child
            .take_stdin()
            .ok_or_else(|| JobError::supervisor("io", "no child stdin"))?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| JobError::supervisor("io", "no child stdout"))?;
        match Self::connect_with(BufReader::new(stdout), stdin, hello_timeout) {
            Ok(client) => {
                *client.core.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                Ok(client)
            }
            Err(e) => {
                let _ = child.terminate_tree();
                Err(e)
            }
        }
    }

    pub fn hello(&self) -> &Value {
        &self.hello
    }

    pub fn call(&self, method: &str, params: Value) -> JobResult<Value> {
        self.core.call(method, params, None, None)
    }

    pub fn call_with_events(
        &self,
        method: &str,
        params: Value,
        on_event: Option<OnEvent>,
    ) -> JobResult<Value> {
        self.core
            .call(method, params, frame_callback(on_event), None)
    }

    /// As [`Self::call_with_events`], with each event's request id.
    pub fn call_with_frames(
        &self,
        method: &str,
        params: Value,
        on_frame: Option<OnEventFrame>,
    ) -> JobResult<Value> {
        self.core.call(method, params, on_frame, None)
    }

    /// As [`Self::call_with_events`] but gives up after `timeout` with a
    /// `timeout` error (the engine may still finish the work; its late reply is
    /// dropped). The deadline covers waiting for the reply, not
    pub fn call_timeout(
        &self,
        method: &str,
        params: Value,
        on_event: Option<OnEvent>,
        timeout: Duration,
    ) -> JobResult<Value> {
        self.core
            .call(method, params, frame_callback(on_event), Some(timeout))
    }

    /// As [`Self::call_timeout`], with each event's request id.
    pub fn call_timeout_frames(
        &self,
        method: &str,
        params: Value,
        on_frame: Option<OnEventFrame>,
        timeout: Duration,
    ) -> JobResult<Value> {
        self.core.call(method, params, on_frame, Some(timeout))
    }

    /// As [`Self::call_timeout_frames`], with a [`ProgressDeadline`] instead of one absolute
    /// timeout. `on_frame` still receives each event; the call's own events keep it alive.
    pub fn call_progress_frames(
        &self,
        method: &str,
        params: Value,
        on_frame: Option<OnEventFrame>,
        deadline: ProgressDeadline,
    ) -> JobResult<Value> {
        self.core
            .call_budget(method, params, on_frame, Budget::progress(deadline))
    }

    /// Ask the engine to stop, then terminate an owned child that outlives
    /// `grace`. Bounded regardless of engine behaviour: the shutdown request is
    /// sent and awaited on a helper thread for at most `grace` (a hung engine
    /// never replies, and a full pipe can block the write), then an owned child
    /// gets SIGTERM and, after another `grace`, is killed. Worst case about
    /// `2 * grace`. Killing the child also unblocks the helper.
    pub fn shutdown(&self, grace: Duration) {
        let _ = self.shutdown_detailed(grace);
    }

    /// As [`Self::shutdown`], returning what became of the process. Returns only after the
    /// process has been waited, within about `2 * grace` when this call owns the child, or
    /// within `3 * grace + 500 ms` when another path (a monitor reaping it) already owns
    /// it and is itself bounded. [`ChildExit::reaped`] is false only if that bound passed.
    pub fn shutdown_detailed(&self, grace: Duration) -> ChildExit {
        let core = Arc::clone(&self.core);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(core.call("shutdown", json!({}), None, Some(grace)));
        });
        let _ = rx.recv_timeout(grace);
        let taken = self
            .core
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match taken {
            Some(mut c) => match c.wait_or_kill(grace) {
                Ok(WaitOutcome::Exited(status)) => self.core.retire(c, Some(status), false),
                Ok(WaitOutcome::Terminated(status)) => self.core.retire(c, Some(status), true),
                Err(_) => self.core.retire(c, None, true),
            },
            None => {
                self.wait_reaped(grace * 3 + Duration::from_millis(500));
            }
        }
        self.child_exit()
    }
}

impl Core {
    /// Record how `child` ended and release the owner's token. The child is confirmed gone only
    /// when a status was observed (`status`, or a `try_wait` now): then it is dropped and recorded
    /// `reaped`. Otherwise the kill went out but the OS has not reported the exit within the
    /// caller's bound; the child moves to a reaper thread, the record says `reaped: false` (so
    /// `shutdown_detailed` reports `Unconfirmed`) and the token is held until that thread has
    /// waited it. A stuck process never blocks the caller and is never reported gone.
    fn retire(
        self: &Arc<Self>,
        mut child: OwnedChild,
        status: Option<std::process::ExitStatus>,
        killed: bool,
    ) {
        let status = status.or_else(|| child.try_wait().ok().flatten());
        if status.is_some() {
            drop(child);
            self.finish_exit(status, killed);
            return;
        }
        {
            let mut e = self.exit.lock().unwrap_or_else(|e| e.into_inner());
            e.killed = killed;
        }
        let core = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("rightkit-jobs-reaper".into())
            .spawn(move || {
                let status = loop {
                    match child.wait_timeout(Duration::from_millis(250)) {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) => {
                            // The kill may have been refused before; ask again, never wait unbounded here.
                            let _ = child.terminate_tree_bounded(Duration::from_millis(250));
                        }
                        // The wait itself failed: the exit is not observed, so it is not confirmed.
                        Err(_) => break None,
                    }
                };
                let confirmed = status.is_some();
                drop(child);
                core.finish_exit_with(status, killed, confirmed);
            });
        if spawned.is_err() {
            // No thread to hand it to: the child was moved into the closure that was never run, so
            // it is dropped (kill sent, bounded reap, background reaper) and the token is released.
            // The exit is not observed, so it is not confirmed (same as a wait error).
            self.finish_exit_with(None, killed, false);
        }
    }

    fn finish_exit(&self, status: Option<std::process::ExitStatus>, killed: bool) {
        self.finish_exit_with(status, killed, true);
    }

    /// Record the exit. With `confirmed == false` (the wait failed, no status seen) the record
    /// says `reaped: false` (so shutdown reports `Unconfirmed`) and the live token stays held, so
    /// the process keeps counting against the unreaped cap instead of being reported gone.
    fn finish_exit_with(
        &self,
        status: Option<std::process::ExitStatus>,
        killed: bool,
        confirmed: bool,
    ) {
        {
            let mut e = self.exit.lock().unwrap_or_else(|e| e.into_inner());
            e.reaped = confirmed;
            e.killed = killed;
            if e.status.is_none() {
                e.status = status;
            }
        }
        if confirmed {
            drop(
                self.release
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(),
            );
        }
        self.exit_cv.notify_all();
    }

    fn call(
        self: &Arc<Self>,
        method: &str,
        params: Value,
        on_event: Option<OnEventFrame>,
        timeout: Option<Duration>,
    ) -> JobResult<Value> {
        self.call_budget(method, params, on_event, Budget::absolute(timeout))
    }

    fn call_budget(
        self: &Arc<Self>,
        method: &str,
        params: Value,
        on_event: Option<OnEventFrame>,
        budget: Budget,
    ) -> JobResult<Value> {
        let timeout = budget.absolute.or(budget.idle);
        let started = Instant::now();
        let mut deadline = timeout.map(|t| started + t);
        let hard_deadline = budget.absolute.map(|t| started + t);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id,
                Pending {
                    tx,
                    wants_events: on_event.is_some() || budget.idle.is_some(),
                },
            );
        let frame = json!({"type": "request", "id": id, "method": method, "params": params});
        let mut bytes = Vec::new();
        if let Err(e) = write_json_line(&mut bytes, &frame, self.frame_limit) {
            // Local encode/oversize refusal: nothing was queued or written (zero bytes
            // reached the engine), so this is nonretryable validation (`bad_input`),
            // never `engine_write` — a shell must not restart/retry a request the
            // engine never saw. Attempted transport failures below stay distinct.
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(JobError::supervisor("bad_input", e.to_string()));
        }
        let (rtx, rrx) = channel();
        let queued = self
            .writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send((bytes, rtx))
            .is_ok();
        let written = if !queued {
            Err(JobError::supervisor("engine_write", "writer stopped"))
        } else {
            let r = match deadline {
                None => rrx.recv().map_err(|_| ()),
                Some(d) => rrx
                    .recv_timeout(d.saturating_duration_since(Instant::now()))
                    .map_err(|_| ()),
            };
            match r {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(JobError::from(e).with_origin(ErrorOrigin::Supervisor)),
                Err(()) => {
                    // Pipe full / engine not reading: kill the owned child so the blocked write
                    // fails and the writer thread exits. The kill and reap are bounded; a child
                    // the OS has not reported gone moves to the reaper (see `retire`). The
                    // request may have been partly or wholly delivered: say so.
                    let bound = Duration::from_millis(self.kill_bound_ms.load(Ordering::Relaxed));
                    self.stalled_write.store(true, Ordering::SeqCst);
                    let taken = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
                    if let Some(mut c) = taken {
                        let status = match c.terminate_tree_bounded(bound) {
                            Ok(ReapOutcome::Exited(status)) => Some(status),
                            _ => None,
                        };
                        self.retire(c, status, true);
                    }
                    Err(JobError::supervisor(
                        DELIVERY_UNKNOWN,
                        format!(
                            "engine did not accept {method} within {:?}; whether it received the request is unknown",
                            timeout.unwrap_or_default()
                        ),
                    ))
                }
            }
        };
        if let Err(e) = written {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(e);
        }
        if let Some(idle) = budget.idle {
            // The wait for the reply starts a fresh silence window; the write is done.
            deadline = Some(soonest(Instant::now() + idle, hard_deadline));
        }
        loop {
            let msg = match deadline {
                None => rx
                    .recv()
                    .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
            };
            match msg {
                Ok(Msg::Event(id, event)) => {
                    if let Some(idle) = budget.idle {
                        // Progress of this call (the reader only forwards events carrying its id).
                        deadline = Some(soonest(Instant::now() + idle, hard_deadline));
                    }
                    if let Some(cb) = &on_event {
                        cb(&EventFrame { id, event: &event });
                    }
                }
                Ok(Msg::Done(r)) => return r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    self.pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id);
                    let message = match budget.idle {
                        Some(idle) if hard_deadline.is_none_or(|h| Instant::now() < h) => {
                            format!("engine sent no progress for {method} within {idle:?}")
                        }
                        _ => format!(
                            "engine did not answer {method} within {:?}",
                            budget.absolute.or(budget.idle).unwrap_or_default()
                        ),
                    };
                    return Err(JobError::supervisor("timeout", message));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(JobError::supervisor(
                        "engine_exited",
                        "engine closed the connection",
                    ));
                }
            }
        }
    }
}

fn soonest(a: Instant, b: Option<Instant>) -> Instant {
    b.map_or(a, |b| a.min(b))
}

/// Adapts a payload-only callback to the frame form every call runs.
pub(crate) fn frame_callback(on_event: Option<OnEvent>) -> Option<OnEventFrame> {
    on_event.map(|cb| -> OnEventFrame { Box::new(move |frame: &EventFrame<'_>| cb(frame.event)) })
}

/// One callback for a call that may have both a payload and a frame callback.
pub(crate) fn merge_event_callbacks(
    on_event: Option<OnEvent>,
    on_frame: Option<OnEventFrame>,
) -> Option<OnEventFrame> {
    match (on_event, on_frame) {
        (None, frame) => frame,
        (Some(cb), None) => frame_callback(Some(cb)),
        (Some(cb), Some(frame)) => Some(Box::new(move |f: &EventFrame<'_>| {
            cb(f.event);
            frame(f);
        })),
    }
}

/// Hands a malformed stdout line to the host's sink, if one is set. The line is
/// whatever the frame reader left buffered when it rejected the frame.
fn report_protocol(sink: &Option<ProtocolErrorSink>, buf: &[u8], err: &FrameError) {
    if let Some(sink) = sink {
        sink.report(&ProtocolError {
            line: String::from_utf8_lossy(buf).into_owned(),
            message: err.to_string(),
        });
    }
}

fn dispatch(map: &PendingMap, subs: &Subscribers, v: &Value, fallback_code: &str) {
    let Some(id) = v["id"].as_u64() else {
        if v["type"] == "event" {
            let mut list = subs.lock().unwrap_or_else(|e| e.into_inner());
            list.retain(|tx| tx.send(v.clone()).is_ok());
        }
        return;
    };
    // Never run user code (or block) while the pending map is locked.
    match v["type"].as_str() {
        Some("event") => {
            let tx = {
                let m = map.lock().unwrap_or_else(|e| e.into_inner());
                m.get(&id).filter(|p| p.wants_events).map(|p| p.tx.clone())
            };
            if let Some(tx) = tx {
                let _ = tx.send(Msg::Event(id, v["event"].clone()));
            }
        }
        Some("response") => {
            let removed = map.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            if let Some(p) = removed {
                let r = if v["ok"] == true {
                    Ok(v["result"].clone())
                } else {
                    // An error reply without a usable code or message still answers the call.
                    let code = v["error"]["code"]
                        .as_str()
                        .filter(|c| !c.is_empty())
                        .unwrap_or(fallback_code);
                    let message = v["error"]["message"]
                        .as_str()
                        .filter(|m| !m.is_empty())
                        .unwrap_or(NO_DETAIL);
                    let mut e = JobError::new(code, message);
                    e.retryable = v["error"]["retryable"].as_bool().unwrap_or(false);
                    // Optional structured details; absent/`null` keeps prior behavior.
                    if let Some(d) = v["error"].get("details").filter(|d| !d.is_null()) {
                        e = e.with_details(d.clone());
                    }
                    Err(e)
                };
                let _ = p.tx.send(Msg::Done(r));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod exit_record_tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn core() -> Arc<Core> {
        let (wtx, _wrx) = channel::<WriteRequest>();
        Arc::new(Core {
            frame_limit: 1024,
            closed: Arc::default(),
            subscribers: Arc::default(),
            writes: Mutex::new(wtx),
            pending: Arc::default(),
            next_id: AtomicU64::new(1),
            child: Mutex::new(None),
            release: Mutex::new(None),
            exit: Mutex::new(ChildExit::default()),
            exit_cv: Condvar::new(),
            kill_bound_ms: AtomicU64::new(100),
            stalled_write: std::sync::atomic::AtomicBool::new(false),
        })
    }

    struct Flag(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn a_failed_wait_is_recorded_unconfirmed_and_keeps_its_token() {
        let c = core();
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *c.release.lock().unwrap() = Some(Box::new(Flag(released.clone())));
        c.exit.lock().unwrap().pid = Some(4242);
        c.finish_exit_with(None, true, false);
        let e = c.exit.lock().unwrap();
        assert!(
            !e.reaped,
            "a wait that errored must not be recorded as reaped"
        );
        assert!(e.killed);
        drop(e);
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));

        c.finish_exit_with(None, true, true);
        assert!(c.exit.lock().unwrap().reaped);
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
    }
}
