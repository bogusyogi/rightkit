use crate::error::*;
use crate::record::{read_record, ServiceRecord, PROTOCOL_MAJOR};
use crate::suite::Suite;
use crate::transport::{connect, Conn};
use rightkit_framed_sidecar::{read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const LIMIT: usize = 16 * 1024 * 1024;

/// Default per-request deadline (connect + hello, and each call's write + read).
/// Override with [`Client::connect_with`] / [`Client::set_deadline`]; `None` disables it.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(120);

/// Aborts the connection when the deadline passes; disarmed on drop. Aborting
/// goes through the transport `shutdown`, which wakes a blocked Unix read/write
/// and issues CancelIoEx on a Windows pipe.
struct Watchdog {
    fired: Arc<AtomicBool>,
    _disarm: mpsc::Sender<()>,
}

impl Watchdog {
    fn arm(after: Option<Duration>, shutdown: Arc<dyn Fn() + Send + Sync>) -> Watchdog {
        let fired = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<()>();
        if let Some(d) = after {
            let f = fired.clone();
            std::thread::spawn(move || {
                if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(d) {
                    f.store(true, Ordering::SeqCst);
                    shutdown();
                }
            });
        }
        Watchdog { fired, _disarm: tx }
    }
    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

fn timed_out(what: &str, d: Option<Duration>) -> ServiceError {
    ServiceError::new(
        ERROR_TIMEOUT,
        format!(
            "{what} exceeded the {} ms deadline; connection aborted",
            d.map(|d| d.as_millis()).unwrap_or(0)
        ),
    )
}

struct Io {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    buf: Vec<u8>,
}

/// A connection to one host of a service. One request in flight at a time;
/// open a second `Client` for parallel calls. Dropping it closes the
/// connection, which the host treats as a disconnect (jobs started with
/// `cancel_on_disconnect` are cancelled).
pub struct Client {
    io: Mutex<Io>,
    next_id: AtomicU64,
    dead: AtomicBool,
    hello: Value,
    record: ServiceRecord,
    shutdown: Arc<dyn Fn() + Send + Sync>,
    /// Per-request deadline in ms; 0 = none.
    deadline_ms: AtomicU64,
}

fn closed() -> ServiceError {
    ServiceError::retryable(
        ERROR_CONNECTION_CLOSED,
        "connection closed before a response arrived",
    )
}

impl Client {
    /// Discover the live host of `service` on `suite` and say hello as `app`,
    /// bounded by [`DEFAULT_DEADLINE`].
    pub fn connect(suite: &Suite, service: &str, app: &str) -> Result<Client, ServiceError> {
        Self::connect_with(suite, service, app, Some(DEFAULT_DEADLINE))
    }

    /// Like [`Client::connect`] with an explicit deadline for connect + hello,
    /// which also becomes the client's per-request deadline (`None` = unbounded).
    pub fn connect_with(
        suite: &Suite,
        service: &str,
        app: &str,
        deadline: Option<Duration>,
    ) -> Result<Client, ServiceError> {
        let Some(d) = deadline else {
            return Self::handshake(suite, service, app, None);
        };
        // A connect() that never completes cannot be cancelled, so it runs on a
        // helper thread; a client that arrives after the deadline is dropped there.
        let (tx, rx) = mpsc::channel();
        let (s, sv, ap) = (suite.clone(), service.to_string(), app.to_string());
        std::thread::spawn(move || {
            let _ = tx.send(Self::handshake(&s, &sv, &ap, Some(d)));
        });
        match rx.recv_timeout(d + Duration::from_millis(250)) {
            Ok(r) => r,
            Err(_) => Err(timed_out("connect", deadline)),
        }
    }

    fn handshake(
        suite: &Suite,
        service: &str,
        app: &str,
        deadline: Option<Duration>,
    ) -> Result<Client, ServiceError> {
        let started = Instant::now();
        let record = read_record(suite, service).ok_or_else(|| {
            ServiceError::retryable(ERROR_UNAVAILABLE, format!("no service.json for {service}"))
        })?;
        if record.protocol_major != PROTOCOL_MAJOR {
            return Err(ServiceError::new(
                "protocol_mismatch",
                format!("host speaks protocol {}", record.protocol_major),
            ));
        }
        let Conn {
            reader,
            writer,
            shutdown,
            ..
        } = connect(&record.endpoint).map_err(|e| {
            ServiceError::retryable(
                ERROR_UNAVAILABLE,
                format!("{service} host unreachable: {e}"),
            )
        })?;
        let mut io = Io {
            reader: BufReader::new(reader),
            writer,
            buf: Vec::new(),
        };
        let wd = Watchdog::arm(
            deadline.map(|d| d.saturating_sub(started.elapsed())),
            shutdown.clone(),
        );
        let hello_res = (|| {
            write_json_line(
                &mut io.writer,
                &json!({"type": "hello", "app": app, "protocol_major": PROTOCOL_MAJOR}),
                LIMIT,
            )
            .map_err(|e| ServiceError::new("io", e.to_string()))?;
            io.writer.flush()?;
            read_json_line::<_, Value>(&mut io.reader, &mut io.buf, LIMIT)
                .map_err(|e| ServiceError::new("io", e.to_string()))?
                .ok_or_else(closed)
        })();
        let hello = match hello_res {
            Ok(h) => h,
            Err(e) => {
                let t = wd.fired();
                (shutdown)();
                return Err(if t { timed_out("hello", deadline) } else { e });
            }
        };
        drop(wd);
        if hello["ok"] != true {
            return Err(ServiceError::from_value(&hello["error"]));
        }
        Ok(Client {
            io: Mutex::new(io),
            next_id: AtomicU64::new(1),
            dead: AtomicBool::new(false),
            hello,
            record,
            shutdown,
            deadline_ms: AtomicU64::new(deadline.map(|d| d.as_millis() as u64).unwrap_or(0)),
        })
    }

    /// Set the per-request deadline for later calls (`None` = unbounded).
    pub fn set_deadline(&self, deadline: Option<Duration>) {
        self.deadline_ms.store(
            deadline.map(|d| (d.as_millis() as u64).max(1)).unwrap_or(0),
            Ordering::SeqCst,
        );
    }

    pub fn record(&self) -> &ServiceRecord {
        &self.record
    }
    /// The host's hello: service, role, rank, version, and the methods this app may call.
    pub fn hello(&self) -> &Value {
        &self.hello
    }

    /// Close the connection from any thread. A request blocked in
    /// [`Client::call`] returns `connection_closed`; the host sees the disconnect.
    pub fn abort(&self) {
        self.dead.store(true, Ordering::SeqCst);
        (self.shutdown)();
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        self.call_events(method, params, &mut |_| {})
    }

    /// Like [`Client::call`], streaming the request's `event` frames to `on_event`.
    pub fn call_events(
        &self,
        method: &str,
        params: Value,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(closed());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = match self.deadline_ms.load(Ordering::SeqCst) {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        };
        let wd = Watchdog::arm(deadline, self.shutdown.clone());
        let res = (|| {
            write_json_line(
                &mut io.writer,
                &json!({"type": "request", "id": id, "method": method, "params": params}),
                LIMIT,
            )
            .map_err(|e| match e {
                FrameError::TooLarge { .. } => {
                    ServiceError::new("request_too_large", "request exceeds the frame limit")
                }
                _ => closed(),
            })?;
            io.writer.flush().map_err(|_| closed())?;
            loop {
                let Io { reader, buf, .. } = &mut *io;
                match read_json_line::<_, Value>(reader, buf, LIMIT) {
                    Ok(Some(v)) if v["id"].as_u64() == Some(id) => match v["type"].as_str() {
                        Some("event") => on_event(&v["event"]),
                        Some("response") => {
                            return if v["ok"] == true {
                                Ok(v["result"].clone())
                            } else {
                                Err(ServiceError::from_value(&v["error"]))
                            };
                        }
                        _ => {}
                    },
                    Ok(Some(_)) => {} // frame for another request or a connection-level error
                    Ok(None) | Err(FrameError::Io(_)) => return Err(closed()),
                    Err(_) => {}
                }
            }
        })();
        if wd.fired() && res.is_err() {
            self.dead.store(true, Ordering::SeqCst);
            return Err(timed_out(&format!("request `{method}`"), deadline));
        }
        if matches!(&res, Err(e) if e.code == ERROR_CONNECTION_CLOSED) {
            self.dead.store(true, Ordering::SeqCst);
        }
        res
    }
}

/// Reconnect-and-resubmit policy for retryable errors (handoff, host restart).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: Duration,
    /// Give up after this long regardless of attempts. Not a request timeout:
    /// a single call may run as long as the host keeps it alive.
    pub give_up_after: Duration,
    /// Per-attempt connect + request deadline (`None` = unbounded).
    pub request_deadline: Option<Duration>,
    /// The request is safe to run twice. When false (the default) a request is
    /// resubmitted only if the host provably did not run it (service unavailable
    /// at connect, or an explicit host handoff); a connection that closed after
    /// the request was written is returned as-is. A timeout is never replayed.
    pub idempotent: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            backoff: Duration::from_millis(150),
            give_up_after: Duration::from_secs(120),
            request_deadline: Some(DEFAULT_DEADLINE),
            idempotent: false,
        }
    }
}

impl Client {
    /// Connect, call, and on a retryable error (host handoff, closed connection,
    /// service unavailable) re-read discovery and resubmit the whole request.
    pub fn call_retrying(
        suite: &Suite,
        service: &str,
        app: &str,
        method: &str,
        params: Value,
        policy: RetryPolicy,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        let started = Instant::now();
        let mut last = ServiceError::retryable(ERROR_UNAVAILABLE, "no attempt made");
        for attempt in 0..policy.max_attempts.max(1) {
            if attempt > 0 {
                if started.elapsed() > policy.give_up_after {
                    break;
                }
                std::thread::sleep(policy.backoff * attempt.min(8));
            }
            let c = match Client::connect_with(suite, service, app, policy.request_deadline) {
                Ok(c) => c,
                Err(e) if e.retryable => {
                    last = e; // never reached the host's request path
                    continue;
                }
                Err(e) => return Err(e),
            };
            match c.call_events(method, params.clone(), on_event) {
                Ok(v) => return Ok(v),
                // Handoff means the host declined the request; anything else may have run.
                Err(e) if e.retryable && (e.code == ERROR_HOST_HANDOFF || policy.idempotent) => {
                    last = e
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}
