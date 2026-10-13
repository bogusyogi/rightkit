//! The device side of `rk-link/1`: a blocking client that keeps one outbound link to the host
//! alive, runs the tasks it is leased and delivers their results.
//!
//! No async runtime: std sockets and rustls only, so a `link`-only build has no tokio, axum or
//! hyper (a manifest test pins this). Threads: one connection thread (it owns the reconnect
//! loop and, while connected, blocks reading the socket; the read timeout is half a heartbeat,
//! so it wakes about every ten seconds), and one short-lived thread per running task.
//!
//! Delivery:
//! * A task is written to the journal (fsync) before it is acknowledged, so a crash after the
//!   `ack` never loses it. With a journal path, tasks that were accepted but not finished are
//!   run again when the client starts (at-least-once: handlers key on `id` and `epoch`).
//! * A result is kept until the host answers `settled`; it is sent again on every reconnect.
//! * After a drop the client reconnects with capped exponential backoff and jitter, signs a
//!   fresh upgrade, and sends `resume_seq` (the highest task sequence number it holds durably).
//!   The host keeps the lease across the drop and sends only what the client does not hold.
//!
//! TLS only: the client trusts the roots it was configured with, never the system roots, and
//! there is no plaintext fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::frame::{
    c2s_line, open_s2c, DEAD_AFTER_SECS, HEARTBEAT_SECS, H_LINK_ID, H_LINK_KEY, MAX_LINE,
};
use crate::record::write_atomic;
use crate::remote::client::{DeviceKey, DeviceSession, Error, ErrorCode, Refusal};
use crate::remote::util::{hex, random_bytes, unhex};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A task leased to this device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkTask {
    pub id: String,
    /// Host-assigned, strictly increasing; the highest one held is the `resume_seq`.
    pub seq: u64,
    /// Lease epoch. A result is accepted only for the current epoch.
    pub epoch: u64,
    pub method: String,
    pub params: Value,
    pub lease_ttl: u64,
}

/// A task failure reported to the host (the task becomes `failed`).
#[derive(Debug, Clone, PartialEq)]
pub struct TaskError {
    pub code: String,
    pub message: String,
}

impl TaskError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Runs leased tasks. It is called on its own thread per task and may block; it must be
/// idempotent per `(task.id, task.epoch)`, because delivery is at-least-once.
pub trait TaskHandler: Send + Sync + 'static {
    fn run(&self, task: &LinkTask) -> Result<Value, TaskError>;
}

impl<F> TaskHandler for F
where
    F: Fn(&LinkTask) -> Result<Value, TaskError> + Send + Sync + 'static,
{
    fn run(&self, task: &LinkTask) -> Result<Value, TaskError> {
        self(task)
    }
}

/// Link timing and persistence.
#[derive(Debug, Clone)]
pub struct LinkConfig {
    /// Write-ahead journal of accepted tasks. `None` keeps it in memory: a restart then forgets
    /// unfinished tasks, and their leases lapse and re-queue on the host.
    pub journal: Option<PathBuf>,
    /// A ping is sent when nothing else was sent for this long.
    pub heartbeat: Duration,
    /// The link is dropped after this long without a frame from the host.
    pub dead_after: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            journal: None,
            heartbeat: Duration::from_secs(HEARTBEAT_SECS),
            dead_after: Duration::from_secs(DEAD_AFTER_SECS),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }
}

// ---- journal ---------------------------------------------------------------------------------

#[derive(Default)]
struct JournalState {
    high_seq: u64,
    /// `(id, epoch)` of tasks accepted and not yet settled.
    seen: BTreeSet<(String, u64)>,
    open: BTreeMap<(String, u64), LinkTask>,
}

struct Journal {
    path: Option<PathBuf>,
    state: Mutex<JournalState>,
}

impl Journal {
    fn open(path: Option<PathBuf>) -> io::Result<Self> {
        let mut st = JournalState::default();
        if let Some(p) = &path {
            match std::fs::read_to_string(p) {
                Ok(text) => {
                    for line in text.lines() {
                        // A line cut off by a crash does not parse and is skipped.
                        let Ok(v) = serde_json::from_str::<Value>(line) else {
                            continue;
                        };
                        if let Some(h) = v.get("high").and_then(Value::as_u64) {
                            st.high_seq = st.high_seq.max(h);
                        } else if let Some(t) = v.get("task") {
                            if let Ok(t) = serde_json::from_value::<LinkTask>(t.clone()) {
                                st.high_seq = st.high_seq.max(t.seq);
                                let key = (t.id.clone(), t.epoch);
                                st.seen.insert(key.clone());
                                st.open.insert(key, t);
                            }
                        } else if let Some(d) = v.get("done") {
                            if let (Some(id), Some(epoch)) = (
                                d.get(0).and_then(Value::as_str),
                                d.get(1).and_then(Value::as_u64),
                            ) {
                                let key = (id.to_string(), epoch);
                                st.seen.remove(&key);
                                st.open.remove(&key);
                            }
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            // Compact: the high-water mark and the tasks still open.
            let mut out = format!("{}\n", json!({ "high": st.high_seq }));
            for t in st.open.values() {
                out.push_str(&format!("{}\n", json!({ "task": t })));
            }
            write_atomic(p, out.as_bytes())?;
        }
        Ok(Self {
            path,
            state: Mutex::new(st),
        })
    }

    fn append(&self, v: &Value) -> io::Result<()> {
        let Some(p) = &self.path else {
            return Ok(());
        };
        let mut f = OpenOptions::new().create(true).append(true).open(p)?;
        writeln!(f, "{v}")?;
        f.sync_all()
    }

    fn high_seq(&self) -> u64 {
        lock(&self.state).high_seq
    }

    /// Record a task as accepted, durably. `Ok(false)` when it was already held.
    fn accept(&self, t: &LinkTask) -> io::Result<bool> {
        let mut st = lock(&self.state);
        let key = (t.id.clone(), t.epoch);
        if st.seen.contains(&key) {
            return Ok(false);
        }
        self.append(&json!({ "task": t }))?;
        st.high_seq = st.high_seq.max(t.seq);
        st.seen.insert(key.clone());
        st.open.insert(key, t.clone());
        Ok(true)
    }

    fn done(&self, id: &str, epoch: u64) {
        let mut st = lock(&self.state);
        let key = (id.to_string(), epoch);
        let known = st.seen.remove(&key);
        st.open.remove(&key);
        if known {
            let _ = self.append(&json!({ "done": [id, epoch] }));
        }
    }

    fn unfinished(&self) -> Vec<LinkTask> {
        lock(&self.state).open.values().cloned().collect()
    }
}

// ---- connection ------------------------------------------------------------------------------

/// One upgraded connection. The TLS state sits behind a mutex and the socket is read through a
/// second handle, so a reader blocked on the socket never holds a lock a writer needs.
struct Conn {
    tls: Mutex<rustls::ClientConnection>,
    wsock: Mutex<TcpStream>,
    key: Arc<DeviceKey>,
    link_id: String,
    s2c_key: [u8; 32],
    seq: Mutex<u64>,
    last_tx: Mutex<Instant>,
}

enum Plain {
    Data(Vec<u8>),
    Eof,
}

impl Conn {
    /// Number, sign and write one frame. The sequence lock is held across the write, so frames
    /// reach the wire in sequence order.
    fn send(&self, mut frame: Value) -> io::Result<()> {
        let mut seq = lock(&self.seq);
        *seq += 1;
        frame["n"] = json!(*seq);
        let line = c2s_line(&self.key, &self.link_id, &frame);
        let mut tls = lock(&self.tls);
        tls.writer().write_all(line.as_bytes())?;
        let mut w = lock(&self.wsock);
        while tls.wants_write() {
            tls.write_tls(&mut *w)?;
        }
        w.flush()?;
        *lock(&self.last_tx) = Instant::now();
        Ok(())
    }

    /// Block (up to the socket's read timeout) for decrypted bytes from the host.
    fn read_plain(&self, rsock: &mut TcpStream) -> io::Result<Plain> {
        let mut raw = [0u8; 16 * 1024];
        loop {
            let n = rsock.read(&mut raw)?;
            if n == 0 {
                return Ok(Plain::Eof);
            }
            let mut tls = lock(&self.tls);
            let mut cur = io::Cursor::new(&raw[..n]);
            let mut out = Vec::new();
            let mut tmp = [0u8; 4096];
            // Drain plaintext after every `read_tls`: rustls caps buffered plaintext at 16 KiB,
            // so a chunk completing several records must not be fed in one go.
            while (cur.position() as usize) < n {
                let m = tls.read_tls(&mut cur)?;
                if m == 0 {
                    break;
                }
                tls.process_new_packets()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                loop {
                    match tls.reader().read(&mut tmp) {
                        Ok(0) => {
                            if out.is_empty() {
                                return Ok(Plain::Eof);
                            }
                            break;
                        }
                        Ok(k) => out.extend_from_slice(&tmp[..k]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                            if out.is_empty() {
                                return Ok(Plain::Eof);
                            }
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            {
                let mut w = lock(&self.wsock);
                while tls.wants_write() {
                    tls.write_tls(&mut *w)?;
                }
            }
            if !out.is_empty() {
                return Ok(Plain::Data(out));
            }
        }
    }
}

#[derive(Default)]
struct LineBuf {
    buf: Vec<u8>,
}

impl LineBuf {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The next complete line, `Ok(None)` when more bytes are needed, `Err` for a frame over the
    /// size bound or one that is not UTF-8.
    fn next_line(&mut self) -> Result<Option<String>, ()> {
        match self.buf.iter().position(|b| *b == b'\n') {
            Some(i) if i <= MAX_LINE => {
                let rest = self.buf.split_off(i + 1);
                let mut line = std::mem::replace(&mut self.buf, rest);
                line.pop();
                String::from_utf8(line).map(Some).map_err(|_| ())
            }
            Some(_) => Err(()),
            None if self.buf.len() > MAX_LINE => Err(()),
            None => Ok(None),
        }
    }
}

// ---- shared state ------------------------------------------------------------------------------

struct Shared {
    journal: Journal,
    conn: Mutex<Option<Arc<Conn>>>,
    /// Result frames not yet settled by the host.
    pending: Mutex<Vec<Value>>,
    calls: Mutex<BTreeMap<String, mpsc::Sender<Value>>>,
    sock: Mutex<Option<TcpStream>>,
    stop: AtomicBool,
    welcomes: AtomicU64,
}

impl Shared {
    /// A task finished: keep its result until settled and try to send it now.
    fn finish(&self, result: Value) {
        lock(&self.pending).push(result.clone());
        let conn = lock(&self.conn).clone();
        if let Some(c) = conn {
            let _ = c.send(result);
        }
    }
}

fn spawn_worker(shared: Arc<Shared>, handler: Arc<dyn TaskHandler>, task: LinkTask) {
    let spawned = std::thread::Builder::new()
        .name("rk-link-task".into())
        .spawn(move || {
            let ran = catch_unwind(AssertUnwindSafe(|| handler.run(&task)));
            let frame = match ran {
                Ok(Ok(result)) => json!({
                    "t": "result", "id": task.id, "epoch": task.epoch, "ok": true, "result": result,
                }),
                Ok(Err(e)) => json!({
                    "t": "result", "id": task.id, "epoch": task.epoch, "ok": false,
                    "error": { "code": e.code, "message": e.message },
                }),
                Err(_) => json!({
                    "t": "result", "id": task.id, "epoch": task.epoch, "ok": false,
                    "error": { "code": "panic", "message": "task handler panicked" },
                }),
            };
            shared.finish(frame);
        });
    // A thread that cannot start leaves the task journalled and its lease to lapse: it runs again.
    let _ = spawned;
}

enum Step {
    Continue,
    Welcome,
    End,
}

fn handle(
    shared: &Arc<Shared>,
    conn: &Arc<Conn>,
    handler: &Arc<dyn TaskHandler>,
    in_seq: &mut u64,
    line: &str,
) -> Step {
    let Ok(v) = open_s2c(&conn.s2c_key, &conn.link_id, line) else {
        return Step::End;
    };
    let Some(n) = v.get("n").and_then(Value::as_u64) else {
        return Step::End;
    };
    // Strictly the next frame: a replayed, duplicated or dropped frame ends the link.
    if n != *in_seq + 1 {
        return Step::End;
    }
    *in_seq = n;
    match v.get("t").and_then(Value::as_str).unwrap_or("") {
        "welcome" => {
            *lock(&shared.conn) = Some(conn.clone());
            shared.welcomes.fetch_add(1, Ordering::SeqCst);
            let pending = lock(&shared.pending).clone();
            for result in pending {
                if conn.send(result).is_err() {
                    return Step::End;
                }
            }
            Step::Welcome
        }
        "task" => {
            let Ok(task) = serde_json::from_value::<LinkTask>(v.clone()) else {
                return Step::End;
            };
            match shared.journal.accept(&task) {
                Ok(fresh) => {
                    let ack = json!({ "t": "ack", "id": task.id, "epoch": task.epoch });
                    if conn.send(ack).is_err() {
                        return Step::End;
                    }
                    if fresh {
                        spawn_worker(shared.clone(), handler.clone(), task);
                    }
                    Step::Continue
                }
                // A task that cannot be held durably is not acknowledged.
                Err(_) => Step::End,
            }
        }
        "settled" => {
            if let (Some(id), Some(epoch)) = (
                v.get("id").and_then(Value::as_str),
                v.get("epoch").and_then(Value::as_u64),
            ) {
                // The host could not store the result: keep it and send it on the next link.
                if v.get("code").and_then(Value::as_str) != Some("storage_error") {
                    lock(&shared.pending).retain(|r| {
                        !(r["id"].as_str() == Some(id) && r["epoch"].as_u64() == Some(epoch))
                    });
                    shared.journal.done(id, epoch);
                }
            }
            Step::Continue
        }
        "response" => {
            if let Some(id) = v.get("id").and_then(Value::as_str) {
                if let Some(tx) = lock(&shared.calls).remove(id) {
                    let _ = tx.send(v.clone());
                }
            }
            Step::Continue
        }
        "ping" => Step::Continue,
        _ => Step::End,
    }
}

fn timed_out(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// One connection, from upgrade to drop. Returns whether the host welcomed this link.
fn run_connection(
    shared: &Arc<Shared>,
    session: &DeviceSession,
    cfg: &LinkConfig,
    handler: &Arc<dyn TaskHandler>,
) -> bool {
    let Ok((stream, reply)) = session.open_link() else {
        return false;
    };
    let Some(link_id) = reply.header(H_LINK_ID).map(str::to_string) else {
        return false;
    };
    let Some(s2c_key) = reply
        .header(H_LINK_KEY)
        .and_then(unhex)
        .and_then(|k| <[u8; 32]>::try_from(k).ok())
    else {
        return false;
    };
    let rustls::StreamOwned { conn: tls, sock } = stream;
    let Ok(wsock) = sock.try_clone() else {
        return false;
    };
    let mut rsock = sock;
    let half = (cfg.heartbeat / 2).max(Duration::from_millis(1));
    let _ = rsock.set_read_timeout(Some(half));
    let _ = rsock.set_write_timeout(Some(Duration::from_secs(30)));
    if let Ok(c) = rsock.try_clone() {
        *lock(&shared.sock) = Some(c);
    }
    let conn = Arc::new(Conn {
        tls: Mutex::new(tls),
        wsock: Mutex::new(wsock),
        key: session.device_key(),
        link_id,
        s2c_key,
        seq: Mutex::new(0),
        last_tx: Mutex::new(Instant::now()),
    });
    let welcomed = if shared.stop.load(Ordering::SeqCst) {
        false
    } else {
        pump(shared, session, cfg, handler, &conn, &mut rsock)
    };
    {
        let mut cur = lock(&shared.conn);
        if cur.as_ref().is_some_and(|c| Arc::ptr_eq(c, &conn)) {
            *cur = None;
        }
    }
    *lock(&shared.sock) = None;
    let _ = rsock.shutdown(Shutdown::Both);
    welcomed
}

fn pump(
    shared: &Arc<Shared>,
    session: &DeviceSession,
    cfg: &LinkConfig,
    handler: &Arc<dyn TaskHandler>,
    conn: &Arc<Conn>,
    rsock: &mut TcpStream,
) -> bool {
    let hello = json!({
        "t": "hello",
        "device": session.device_id(),
        "resume_seq": shared.journal.high_seq(),
    });
    if conn.send(hello).is_err() {
        return false;
    }
    let mut lines = LineBuf::default();
    let mut in_seq = 0u64;
    let mut last_rx = Instant::now();
    let mut welcomed = false;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return welcomed;
        }
        match conn.read_plain(rsock) {
            Ok(Plain::Data(data)) => {
                last_rx = Instant::now();
                lines.push(&data);
                loop {
                    match lines.next_line() {
                        Ok(Some(line)) => match handle(shared, conn, handler, &mut in_seq, &line) {
                            Step::Continue => {}
                            Step::Welcome => welcomed = true,
                            Step::End => return welcomed,
                        },
                        Ok(None) => break,
                        Err(()) => return welcomed,
                    }
                }
            }
            Ok(Plain::Eof) => return welcomed,
            Err(e) if timed_out(&e) => {
                if last_rx.elapsed() >= cfg.dead_after {
                    return welcomed;
                }
            }
            Err(_) => return welcomed,
        }
        let due = lock(&conn.last_tx).elapsed() >= cfg.heartbeat;
        if due && conn.send(json!({ "t": "ping" })).is_err() {
            return welcomed;
        }
    }
}

/// Reconnect delay: `backoff_min` after a link that was up, doubling per failed attempt up to
/// `backoff_max`, with 50-100% jitter.
fn backoff(cfg: &LinkConfig, attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(16);
    let base = cfg
        .backoff_min
        .saturating_mul(1u32 << exp)
        .min(cfg.backoff_max);
    let half = base / 2;
    let jitter = u64::from(u16::from_le_bytes(random_bytes::<2>()));
    let extra = (half.as_nanos() as u64).saturating_mul(jitter) / 65_536;
    half + Duration::from_nanos(extra)
}

fn run_loop(
    shared: Arc<Shared>,
    session: DeviceSession,
    cfg: LinkConfig,
    handler: Arc<dyn TaskHandler>,
    stop_rx: mpsc::Receiver<()>,
) {
    let mut attempt: u32 = 0;
    while !shared.stop.load(Ordering::SeqCst) {
        let up = run_connection(&shared, &session, &cfg, &handler);
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        attempt = if up { 0 } else { attempt.saturating_add(1) };
        match stop_rx.recv_timeout(backoff(&cfg, attempt)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => break,
        }
    }
}

// ---- public handle -----------------------------------------------------------------------------

/// Starts links.
pub struct LinkClient;

impl LinkClient {
    /// Start the link thread for an enrolled device (scope `link`). It connects, reconnects and
    /// runs tasks until the returned handle is stopped or dropped.
    pub fn start(
        session: DeviceSession,
        cfg: LinkConfig,
        handler: Arc<dyn TaskHandler>,
    ) -> Result<LinkHandle, Error> {
        let journal = Journal::open(cfg.journal.clone())?;
        let unfinished = journal.unfinished();
        let shared = Arc::new(Shared {
            journal,
            conn: Mutex::new(None),
            pending: Mutex::new(Vec::new()),
            calls: Mutex::new(BTreeMap::new()),
            sock: Mutex::new(None),
            stop: AtomicBool::new(false),
            welcomes: AtomicU64::new(0),
        });
        // Tasks accepted before a restart and never settled run again.
        for task in unfinished {
            spawn_worker(shared.clone(), handler.clone(), task);
        }
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("rk-link".into())
                .spawn(move || run_loop(shared, session, cfg, handler, stop_rx))?
        };
        Ok(LinkHandle {
            shared,
            stop_tx,
            thread: Some(thread),
        })
    }
}

/// A running link. Dropping it stops the link.
pub struct LinkHandle {
    shared: Arc<Shared>,
    stop_tx: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl LinkHandle {
    /// Whether a welcomed connection is up now.
    pub fn connected(&self) -> bool {
        lock(&self.shared.conn).is_some()
    }

    /// How many times the host has welcomed this client since it started.
    pub fn connections(&self) -> u64 {
        self.shared.welcomes.load(Ordering::SeqCst)
    }

    /// Results waiting for the host's `settled`.
    pub fn pending_results(&self) -> usize {
        lock(&self.shared.pending).len()
    }

    /// Call a method of the host over the link, as this device (the same contract as
    /// `POST /v1/call/{method}`). Fails at once while the link is down.
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, Error> {
        let conn = lock(&self.shared.conn)
            .clone()
            .ok_or_else(|| Error::Protocol("link is down".into()))?;
        let id = hex(&random_bytes::<8>());
        let (tx, rx) = mpsc::channel();
        lock(&self.shared.calls).insert(id.clone(), tx);
        let frame = json!({ "t": "request", "id": id, "method": method, "params": params });
        let reply = match conn.send(frame) {
            Ok(()) => rx.recv_timeout(timeout),
            Err(e) => {
                lock(&self.shared.calls).remove(&id);
                return Err(Error::Io(e));
            }
        };
        lock(&self.shared.calls).remove(&id);
        let v = reply.map_err(|_| Error::Protocol("no response before the deadline".into()))?;
        let status = v.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
        let body = v.get("body").cloned().unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(body);
        }
        let code = body
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Err(Error::Refused(Refusal {
            status,
            code: ErrorCode::from_code(&code),
            body,
        }))
    }

    /// Stop the link and wait for its thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.stop_tx.send(());
        if let Some(s) = lock(&self.shared.sock).take() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for LinkHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
