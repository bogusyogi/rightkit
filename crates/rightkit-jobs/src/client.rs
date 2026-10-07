use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rightkit_framed_sidecar::{
    read_json_line_or_oversized, write_json_line, FrameError, Incoming, ReadLimits,
};
use rightkit_process::{OwnedChild, OwnedCommand, ReapOutcome};
use serde_json::{json, Value};

use crate::contract::DEFAULT_FRAME_LIMIT;
use crate::error::{JobError, JobResult};

type OnEvent = Box<dyn Fn(&Value) + Send>;

/// What the reader thread hands a waiting caller. Callbacks run on the
/// caller's own thread, never on the reader and never under a lock.
enum Msg {
    Event(Value),
    Done(JobResult<Value>),
}
struct Pending {
    tx: Sender<Msg>,
    wants_events: bool,
}
type PendingMap = Arc<Mutex<HashMap<u64, Pending>>>;

/// Connection tuning for [`EngineClient::connect_opts`].
#[derive(Debug, Clone, Copy)]
pub struct ClientOptions {
    /// How long to wait for the engine's hello frame.
    pub hello_timeout: Duration,
    /// Largest frame accepted in either direction. A larger *response* answers
    /// the call it belongs to with `frame_too_large` instead of ending the session.
    pub frame_limit: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            hello_timeout: DEFAULT_HELLO_TIMEOUT,
            frame_limit: DEFAULT_FRAME_LIMIT,
        }
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
}

/// Shell-side client for an engine speaking the stdio contract. Multiplexes
/// concurrent calls; events of a call stream to its callback (on the calling
/// thread) before it returns. Restart/health policy stays with
/// `rightkit-process`; this only talks frames.
pub struct EngineClient {
    core: Arc<Core>,
    hello: Value,
}

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
        } = options;
        let mut reader = reader;
        let pending: PendingMap = Arc::default();
        let map = Arc::clone(&pending);
        let closed: Arc<Closed> = Arc::default();
        let closed_signal = Arc::clone(&closed);
        let subscribers: Subscribers = Arc::default();
        let subs = Arc::clone(&subscribers);
        let (hello_tx, hello_rx) = channel::<JobResult<Value>>();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let limits = ReadLimits::default();
            // Whatever ends this thread, mark the connection closed and release
            // every waiter and subscriber.
            struct OnExit(Arc<Closed>, PendingMap, Subscribers);
            impl Drop for OnExit {
                fn drop(&mut self) {
                    for (_, p) in self.1.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                        let _ = p.tx.send(Msg::Done(Err(JobError::new(
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
                Ok(Some(Incoming::Frame(h))) if h["type"] == "hello" => Ok(h),
                Ok(Some(Incoming::Frame(_))) => Err(JobError::new(
                    "engine_protocol",
                    "first frame was not hello",
                )),
                Ok(Some(Incoming::Oversized(_))) => Err(JobError::new(
                    "engine_protocol",
                    "hello frame exceeded the frame limit",
                )),
                Ok(None) => Err(JobError::new("engine_exited", "engine closed before hello")),
                Err(e) => Err(JobError::new("engine_protocol", e.to_string())),
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
                    Ok(Some(Incoming::Frame(v))) => dispatch(&map, &subs, &v),
                    Ok(Some(Incoming::Oversized(over))) => {
                        // Answer the call this frame belongs to; the child is healthy.
                        if let Some(id) = over.id() {
                            let removed = map.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                            if let Some(p) = removed {
                                let _ = p.tx.send(Msg::Done(Err(JobError::new(
                                    "frame_too_large",
                                    format!("engine frame of {} bytes exceeded the {frame_limit}-byte limit", over.bytes),
                                ))));
                            }
                        }
                    }
                    Ok(None) | Err(FrameError::Io(_)) => break,
                    Err(_) => continue, // malformed line: resynchronised on the next frame
                }
            }
        });
        let hello = match hello_rx.recv_timeout(hello_timeout) {
            Ok(r) => r?,
            Err(_) => {
                return Err(JobError::new(
                    "hello_timeout",
                    format!("engine sent no hello within {hello_timeout:?}"),
                ));
            }
        };
        let (wtx, wrx) = channel::<(Vec<u8>, Sender<std::io::Result<()>>)>();
        let mut writer = writer;
        std::thread::spawn(move || {
            for (bytes, reply) in wrx {
                let r = writer.write_all(&bytes).and_then(|_| writer.flush());
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
        *self.core.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
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
    pub fn kill(&self, bound: Duration) -> Option<std::process::ExitStatus> {
        let mut child = self
            .core
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        match child.terminate_tree_bounded(bound) {
            Ok(ReapOutcome::Exited(status)) => Some(status),
            _ => None,
        }
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
            return Some(status);
        }
        match child.terminate_tree_bounded(patience) {
            Ok(ReapOutcome::Exited(status)) => Some(status),
            _ => None,
        }
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
            .ok_or_else(|| JobError::new("io", "no child stdin"))?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| JobError::new("io", "no child stdout"))?;
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
        self.core.call(method, params, on_event, None)
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
        self.core.call(method, params, on_event, Some(timeout))
    }

    /// Ask the engine to stop, then terminate an owned child that outlives
    /// `grace`. Bounded regardless of engine behaviour: the shutdown request is
    /// sent and awaited on a helper thread for at most `grace` (a hung engine
    /// never replies, and a full pipe can block the write), then an owned child
    /// gets SIGTERM and, after another `grace`, is killed. Worst case about
    /// `2 * grace`. Killing the child also unblocks the helper.
    pub fn shutdown(&self, grace: Duration) {
        let core = Arc::clone(&self.core);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(core.call("shutdown", json!({}), None, Some(grace)));
        });
        let _ = rx.recv_timeout(grace);
        if let Some(mut c) = self
            .core
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = c.wait_or_kill(grace);
        }
    }
}

impl Core {
    fn call(
        &self,
        method: &str,
        params: Value,
        on_event: Option<OnEvent>,
        timeout: Option<Duration>,
    ) -> JobResult<Value> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id,
                Pending {
                    tx,
                    wants_events: on_event.is_some(),
                },
            );
        let frame = json!({"type": "request", "id": id, "method": method, "params": params});
        let mut bytes = Vec::new();
        if let Err(e) = write_json_line(&mut bytes, &frame, self.frame_limit) {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(JobError::new("engine_write", e.to_string()));
        }
        let (rtx, rrx) = channel();
        let queued = self
            .writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send((bytes, rtx))
            .is_ok();
        let written = if !queued {
            Err(JobError::new("engine_write", "writer stopped"))
        } else {
            let r = match deadline {
                None => rrx.recv().map_err(|_| ()),
                Some(d) => rrx
                    .recv_timeout(d.saturating_duration_since(Instant::now()))
                    .map_err(|_| ()),
            };
            match r {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(JobError::from(e)),
                Err(()) => {
                    // Pipe full / engine not reading: kill the owned child so the
                    // blocked write fails and the writer thread exits.
                    if let Some(mut c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take()
                    {
                        let _ = c.terminate_tree();
                    }
                    Err(JobError::new(
                        "timeout",
                        format!(
                            "engine did not accept {method} within {:?}",
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
        loop {
            let msg = match deadline {
                None => rx
                    .recv()
                    .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
            };
            match msg {
                Ok(Msg::Event(ev)) => {
                    if let Some(cb) = &on_event {
                        cb(&ev);
                    }
                }
                Ok(Msg::Done(r)) => return r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    self.pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id);
                    return Err(JobError::new(
                        "timeout",
                        format!(
                            "engine did not answer {method} within {:?}",
                            timeout.unwrap_or_default()
                        ),
                    ));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(JobError::new(
                        "engine_exited",
                        "engine closed the connection",
                    ));
                }
            }
        }
    }
}

fn dispatch(map: &PendingMap, subs: &Subscribers, v: &Value) {
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
                let _ = tx.send(Msg::Event(v["event"].clone()));
            }
        }
        Some("response") => {
            let removed = map.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            if let Some(p) = removed {
                let r = if v["ok"] == true {
                    Ok(v["result"].clone())
                } else {
                    let mut e = JobError::new(
                        v["error"]["code"].as_str().unwrap_or("error"),
                        v["error"]["message"].as_str().unwrap_or(""),
                    );
                    e.retryable = v["error"]["retryable"].as_bool().unwrap_or(false);
                    Err(e)
                };
                let _ = p.tx.send(Msg::Done(r));
            }
        }
        _ => {}
    }
}
