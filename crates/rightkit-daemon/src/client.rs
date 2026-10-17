use crate::error::*;
use crate::host_identity::{HostConnectError, HostExpectation, HostProcess};
use crate::private::PrivateSecret;
use crate::record::{try_read_record, ServiceRecord, PROTOCOL_MAJOR};
use crate::suite::{Suite, SuiteError};
use crate::transport::{connect, connect_expecting, Conn};
use rightkit_sidecar::{read_json_line, write_json_line, FrameError};
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

/// Lock `m`, giving up at `deadline` (`None` = wait without bound). Polls with a short sleep:
/// `std::sync::Mutex` has no timed lock, and the wait is bounded by the deadline.
fn lock_until<T>(m: &Mutex<T>, deadline: Option<Instant>) -> Option<std::sync::MutexGuard<'_, T>> {
    use std::sync::TryLockError;
    let Some(deadline) = deadline else {
        return Some(m.lock().unwrap_or_else(|e| e.into_inner()));
    };
    let mut pause = Duration::from_micros(200);
    loop {
        match m.try_lock() {
            Ok(g) => return Some(g),
            Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        std::thread::sleep(pause.min(left));
        pause = (pause * 2).min(Duration::from_millis(5));
    }
}

struct Io {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    buf: Vec<u8>,
}

/// What a client needs to find and call one service, including how much it trusts its
/// environment. Pass it to [`Client::connect_expecting`] (verified host) or
/// [`Client::connect_config`] (hardened root, unverified host).
///
/// The suite root is chosen like this:
///
/// * [`suite`](Self::suite) given: that suite, as is. With
///   [`ignore_env_overrides`](Self::ignore_env_overrides) on, a suite that came from the
///   environment ([`Suite::resolve`], see [`Suite::env_derived`]) is refused, so the switch cannot
///   be defeated by passing `Suite::resolve()` in.
/// * not given, `ignore_env_overrides` off: [`Suite::resolve`], which honours
///   `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR` and `HOME`/`XDG_DATA_HOME`/
///   `LOCALAPPDATA`. For QA builds and isolated dev profiles.
/// * not given, `ignore_env_overrides` on: [`Suite::per_user_default`], from the account database.
///   For release builds. A launcher that sets environment variables cannot redirect the client.
///
/// A host that wants the same switch calls [`Suite::resolve_ignoring_env_overrides`]; the host
/// functions themselves take the suite as an argument and read no environment variable.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    service: String,
    app: String,
    suite: Option<Suite>,
    deadline: Option<Duration>,
    ignore_env_overrides: bool,
    secret: Option<PrivateSecret>,
}

impl ClientConfig {
    /// Talk to `service` as `app`, with the [`DEFAULT_DEADLINE`], the suite resolved from the
    /// environment, and no hardening. Release builds should add
    /// [`ignore_env_overrides(true)`](Self::ignore_env_overrides).
    pub fn new(service: impl Into<String>, app: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            app: app.into(),
            suite: None,
            deadline: Some(DEFAULT_DEADLINE),
            ignore_env_overrides: false,
            secret: None,
        }
    }

    /// Send `secret` on the `hello` and on every request, for a host that requires one (see
    /// [`ServiceConfig::private_secret`](crate::ServiceConfig::private_secret)). Pair it with
    /// [`Client::connect_expecting`]: a secret goes to whatever answered at the endpoint.
    pub fn secret(mut self, secret: PrivateSecret) -> Self {
        self.secret = Some(secret);
        self
    }

    /// Use this suite instead of resolving one.
    pub fn suite(mut self, suite: Suite) -> Self {
        self.suite = Some(suite);
        self
    }

    /// Per-request deadline, as for [`Client::connect_with`] (`None` = unbounded).
    pub fn deadline(mut self, deadline: Option<Duration>) -> Self {
        self.deadline = deadline;
        self
    }

    /// Refuse environment redirection: the suite root comes only from the per-user default
    /// ([`Suite::per_user_default`]), and an explicit [`suite`](Self::suite) that was derived from
    /// the environment is an error. Release builds of an app and of its command-line engine set
    /// this; QA builds and isolated test runs do not (they need `RIGHTKIT_SUITE_ROOT`).
    pub fn ignore_env_overrides(mut self, ignore: bool) -> Self {
        self.ignore_env_overrides = ignore;
        self
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn app(&self) -> &str {
        &self.app
    }

    /// Whether [`ignore_env_overrides`](Self::ignore_env_overrides) is on.
    pub fn ignores_env_overrides(&self) -> bool {
        self.ignore_env_overrides
    }

    /// The suite this configuration connects through, per the rules on [`ClientConfig`].
    #[allow(deprecated)] // kept on the legacy switch until hosts adopt `mode::roots`
    pub fn resolve_suite(&self) -> Result<Suite, SuiteError> {
        match (&self.suite, self.ignore_env_overrides) {
            (Some(suite), true) if suite.env_derived() => Err(SuiteError::Insecure(format!(
                "the suite {} was derived from the environment, which this client ignores; \
                 pass Suite::at(..) or leave the suite unset",
                suite.root().display()
            ))),
            (Some(suite), _) => Ok(suite.clone()),
            (None, ignore) => Suite::resolve_ignoring_env_overrides(ignore),
        }
    }
}

fn suite_error(e: SuiteError) -> ServiceError {
    ServiceError::new(ERROR_SUITE_UNRESOLVED, e.to_string())
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
    /// The host the OS identified, for a client opened with [`Client::connect_expecting`].
    host: Option<HostProcess>,
    /// Sent on every request frame when the host requires one.
    secret: Option<PrivateSecret>,
}

/// Add the per-run secret to an outgoing frame.
fn with_secret(mut frame: Value, secret: Option<&PrivateSecret>) -> Value {
    if let Some(secret) = secret {
        frame["secret"] = Value::String(secret.expose().to_string());
    }
    frame
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
    ///
    /// **The host is not authenticated.** Discovery is a file (`service.json`) and a socket path
    /// that any program running as the same user can write or bind, and `suite` itself may have
    /// come from an environment variable. The transport proves only that whatever answered runs
    /// as the current user, so the hello and every request, including any secret in one, go to
    /// that program. A client that sends secrets, or that acts on what the host answers, must use
    /// [`Client::connect_expecting`] instead.
    pub fn connect(suite: &Suite, service: &str, app: &str) -> Result<Client, ServiceError> {
        Self::connect_with(suite, service, app, Some(DEFAULT_DEADLINE))
    }

    /// Like [`Client::connect`] with an explicit deadline for connect + hello,
    /// which also becomes the client's per-request deadline (`None` = unbounded).
    /// The host is not authenticated: see [`Client::connect`].
    pub fn connect_with(
        suite: &Suite,
        service: &str,
        app: &str,
        deadline: Option<Duration>,
    ) -> Result<Client, ServiceError> {
        Self::connect_bounded(suite, service, app, deadline, None, None)
            .map_err(ConnectError::into_service_error)
    }

    /// Connect through a [`ClientConfig`]: the suite and deadline come from the configuration,
    /// and [`ClientConfig::ignore_env_overrides`] is honoured. The host is still not
    /// authenticated (see [`Client::connect`]); this hardens only where the client looks.
    pub fn connect_config(config: &ClientConfig) -> Result<Client, ServiceError> {
        let suite = config.resolve_suite().map_err(suite_error)?;
        Self::connect_bounded(
            &suite,
            &config.service,
            &config.app,
            config.deadline,
            None,
            config.secret.clone(),
        )
        .map_err(ConnectError::into_service_error)
    }

    /// Discover the host of the configured service, verify it is the program `expect` names, and
    /// only then say hello.
    ///
    /// The OS is asked who is on the listening end of the connected socket: its uid must equal
    /// this process's, and its executable (read from the kernel for that process, canonicalised)
    /// must be one of `expect`'s. The process id is proven stable across the read (start time,
    /// and on macOS the audit token, before and after, and the pid re-read from the socket). A
    /// caller-supplied [`HostExpectation::with_verifier`] check then runs. Platforms: macOS
    /// (`getpeereid`, `LOCAL_PEERPID`, `proc_pidpath`), Linux (`SO_PEERCRED`, `/proc/<pid>/exe`),
    /// Windows (the pipe's server process: its token user, a held process handle,
    /// `QueryFullProcessImageNameW`); elsewhere the refusal is
    /// [`HostIdentityReason::Unverified`](crate::HostIdentityReason::Unverified) and nothing is
    /// sent. See the README, "Verifying the host".
    ///
    /// **Order.** The check runs after the kernel has completed the socket `connect` and before
    /// anything is written. Nothing application-level precedes it: no hello, no request, no
    /// length prefix. The only thing the other side can observe of a refused attempt is a
    /// connection that closes without a byte. The deadline in `config` bounds the whole attempt.
    ///
    /// A refusal is [`ConnectError::HostIdentity`], distinct from [`ConnectError::Service`]
    /// (no record, unreachable, timeout, hello refused). It is not retryable. The check is made
    /// once, at connect, for the life of the connection.
    pub fn connect_expecting(
        config: &ClientConfig,
        expect: &HostExpectation,
    ) -> Result<Client, ConnectError> {
        let suite = config
            .resolve_suite()
            .map_err(|e| ConnectError::Service(suite_error(e)))?;
        Self::connect_bounded(
            &suite,
            &config.service,
            &config.app,
            config.deadline,
            Some(expect.clone()),
            config.secret.clone(),
        )
    }

    /// Run [`Client::handshake`] under `deadline`. A connect() that never completes cannot be
    /// cancelled, so it runs on a helper thread; a client that arrives after the deadline is
    /// dropped there.
    fn connect_bounded(
        suite: &Suite,
        service: &str,
        app: &str,
        deadline: Option<Duration>,
        expect: Option<HostExpectation>,
        secret: Option<PrivateSecret>,
    ) -> Result<Client, ConnectError> {
        let Some(d) = deadline else {
            return Self::handshake(suite, service, app, None, expect.as_ref(), secret);
        };
        let (tx, rx) = mpsc::channel();
        let (s, sv, ap) = (suite.clone(), service.to_string(), app.to_string());
        std::thread::spawn(move || {
            let _ = tx.send(Self::handshake(
                &s,
                &sv,
                &ap,
                Some(d),
                expect.as_ref(),
                secret,
            ));
        });
        match rx.recv_timeout(d + Duration::from_millis(250)) {
            Ok(r) => r,
            Err(_) => Err(ConnectError::Service(timed_out("connect", deadline))),
        }
    }

    fn handshake(
        suite: &Suite,
        service: &str,
        app: &str,
        deadline: Option<Duration>,
        expect: Option<&HostExpectation>,
        secret: Option<PrivateSecret>,
    ) -> Result<Client, ConnectError> {
        let started = Instant::now();
        // Absent and unreadable are both retryable `service_unavailable`, but they are not the
        // same statement: an unreadable record says nothing about whether the host is gone.
        let record = match try_read_record(suite, service) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Err(ServiceError::retryable(
                    ERROR_UNAVAILABLE,
                    format!("no service.json for {service}"),
                )
                .into())
            }
            Err(e) => {
                return Err(ServiceError::retryable(
                    ERROR_UNAVAILABLE,
                    format!("could not read service.json for {service} (try again): {e}"),
                )
                .into())
            }
        };
        if record.protocol_major != PROTOCOL_MAJOR {
            return Err(ServiceError::new(
                "protocol_mismatch",
                format!("host speaks protocol {}", record.protocol_major),
            )
            .into());
        }
        let unreachable_host = |e: std::io::Error| {
            ServiceError::retryable(
                ERROR_UNAVAILABLE,
                format!("{service} host unreachable: {e}"),
            )
        };
        // With an expectation, the host is identified here, before the first byte is written.
        let (conn, host) = match expect {
            None => (connect(&record.endpoint).map_err(unreachable_host)?, None),
            Some(expect) => match connect_expecting(&record.endpoint, expect) {
                Ok((conn, host)) => (conn, Some(host)),
                Err(HostConnectError::Io(e)) => return Err(unreachable_host(e).into()),
                Err(HostConnectError::Identity(e)) => return Err(ConnectError::HostIdentity(e)),
            },
        };
        let Conn {
            reader,
            writer,
            shutdown,
            ..
        } = conn;
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
                &with_secret(
                    json!({"type": "hello", "app": app, "protocol_major": PROTOCOL_MAJOR}),
                    secret.as_ref(),
                ),
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
                let e = if t { timed_out("hello", deadline) } else { e };
                return Err(e.into());
            }
        };
        drop(wd);
        if hello["ok"] != true {
            return Err(ServiceError::from_value(&hello["error"]).into());
        }
        Ok(Client {
            io: Mutex::new(io),
            next_id: AtomicU64::new(1),
            dead: AtomicBool::new(false),
            hello,
            record,
            shutdown,
            deadline_ms: AtomicU64::new(deadline.map(|d| d.as_millis() as u64).unwrap_or(0)),
            host,
            secret,
        })
    }

    /// The host process the OS identified and this client verified, for a client opened with
    /// [`Client::connect_expecting`]; `None` for one opened with [`Client::connect`] or
    /// [`Client::connect_with`], whose host was never checked.
    pub fn verified_host(&self) -> Option<&HostProcess> {
        self.host.as_ref()
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
    ///
    /// The per-request deadline starts when this method is entered and covers waiting for the
    /// connection (another call in flight), the write, and the reply: one monotonic deadline.
    pub fn call_events(
        &self,
        method: &str,
        params: Value,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        let deadline = match self.deadline_ms.load(Ordering::SeqCst) {
            0 => None,
            ms => Some(Instant::now() + Duration::from_millis(ms)),
        };
        self.call_events_until(method, params, deadline, on_event)
    }

    /// Like [`Client::call_events`], bounded by an absolute monotonic `deadline` (`None` =
    /// unbounded) that the caller fixed earlier. Waiting for a request already in flight on this
    /// connection counts against it: when it passes first, this returns the typed timeout without
    /// aborting the connection, because the request that holds it is not this one's.
    pub fn call_events_until(
        &self,
        method: &str,
        params: Value,
        deadline: Option<Instant>,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        let mut frame = self.call_frame_until(method, params, false, deadline, on_event)?;
        Ok(frame["result"].take())
    }

    /// One request and its whole response frame. `chunked_ok` tells the host it may answer a
    /// result larger than a frame in pieces (see the `chunk` module).
    fn call_frame_until(
        &self,
        method: &str,
        params: Value,
        chunked_ok: bool,
        deadline: Option<Instant>,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        let started = Instant::now();
        let total = deadline.map(|d| d.saturating_duration_since(started));
        if self.dead.load(Ordering::SeqCst) {
            return Err(closed());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let Some(mut io) = lock_until(&self.io, deadline) else {
            return Err(timed_out(
                &format!("request `{method}` waiting for the connection"),
                total,
            ));
        };
        if self.dead.load(Ordering::SeqCst) {
            return Err(closed());
        }
        let wd = Watchdog::arm(
            deadline.map(|d| d.saturating_duration_since(Instant::now())),
            self.shutdown.clone(),
        );
        let res = (|| {
            let mut frame =
                json!({"type": "request", "id": id, "method": method, "params": params});
            if chunked_ok {
                frame["chunked_ok"] = Value::Bool(true);
            }
            write_json_line(
                &mut io.writer,
                &with_secret(frame, self.secret.as_ref()),
                LIMIT,
            )
            .map_err(|e| match e {
                FrameError::TooLarge { .. } => ServiceError::new(
                    ERROR_REQUEST_TOO_LARGE,
                    "request exceeds the frame limit (Client::call_large sends it in pieces)",
                ),
                _ => closed(),
            })?;
            io.writer.flush().map_err(|_| closed())?;
            loop {
                let Io { reader, buf, .. } = &mut *io;
                match read_json_line::<_, Value>(reader, buf, LIMIT) {
                    Ok(Some(v)) if v["id"].as_u64() == Some(id) => {
                        if v["type"] == "event" {
                            on_event(&v["event"]);
                        } else if v["type"] == "response" {
                            return if v["ok"] == true {
                                Ok(v)
                            } else {
                                Err(ServiceError::from_value(&v["error"]))
                            };
                        }
                    }
                    Ok(Some(_)) => {} // frame for another request or a connection-level error
                    Ok(None) | Err(FrameError::Io(_)) => return Err(closed()),
                    // The frame was drained through its newline, so the stream is still on a
                    // frame boundary and the connection stays usable. Skipping it silently left
                    // the caller waiting for the full deadline for a reply that already came.
                    Err(FrameError::TooLarge { .. }) => {
                        return Err(ServiceError::new(
                            ERROR_RESPONSE_TOO_LARGE,
                            "the host's reply exceeds the client's frame limit",
                        ));
                    }
                    Err(_) => {}
                }
            }
        })();
        if wd.fired() && res.is_err() {
            self.dead.store(true, Ordering::SeqCst);
            return Err(timed_out(&format!("request `{method}`"), total));
        }
        if matches!(&res, Err(e) if e.code == ERROR_CONNECTION_CLOSED) {
            self.dead.store(true, Ordering::SeqCst);
        }
        res
    }
}

impl Client {
    /// One frame of a chunked transfer: its own per-request deadline, the whole response frame.
    fn call_step(
        &self,
        method: &str,
        params: Value,
        chunked_ok: bool,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        let deadline = match self.deadline_ms.load(Ordering::SeqCst) {
            0 => None,
            ms => Some(Instant::now() + Duration::from_millis(ms)),
        };
        self.call_frame_until(method, params, chunked_ok, deadline, on_event)
    }

    /// Like [`Client::call_events`] for a request or a result that may be larger than one frame
    /// (issue #294). The frame limit stays the cap on every frame; a message above it is cut into
    /// pieces and assembled by the other side under the host's separate message bound
    /// ([`HostOptions::max_message`](crate::HostOptions::max_message)), as the `chunk` module
    /// describes.
    ///
    /// * Params that fit in half the host's frame go as one ordinary request, and a result that
    ///   does not fit one frame comes back in pieces (this call opts in to that).
    /// * Larger params are uploaded in pieces through the reserved `rk.upload` method; the host
    ///   runs `method` once the last piece arrives, and its response answers this call. A host
    ///   that predates chunking (no `max_message` in its `hello`) or has it off refuses with
    ///   `request_too_large`, and so does a message beyond the host's bound, before any piece is
    ///   sent.
    ///
    /// Each frame is its own request with its own deadline (the per-request deadline set on the
    /// client), so a long transfer is not cut by one deadline for the whole. A failure part-way
    /// leaves nothing in the host after the next call or the end of the connection; resubmitting
    /// the whole call is safe for the transfer itself, but whether `method` may run twice is the
    /// caller's call, as for [`Client::call`]. Events of the call (`on_event`) arrive with the
    /// final upload piece's request.
    pub fn call_large(
        &self,
        method: &str,
        params: Value,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        use crate::chunk::{boundaries, CHUNK_TEXT_BYTES, METHOD_FETCH, METHOD_UPLOAD};
        let advertised = |key: &str| {
            self.hello[key]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
        };
        let max_message = advertised("max_message").unwrap_or(0);
        let frame_limit = advertised("frame_limit").unwrap_or(LIMIT).min(LIMIT);
        let chunk = advertised("chunk_bytes")
            .unwrap_or(CHUNK_TEXT_BYTES)
            .clamp(16, CHUNK_TEXT_BYTES);
        let text = serde_json::to_string(&params)
            .map_err(|e| ServiceError::new("bad_request", e.to_string()))?;
        let single_max = if max_message == 0 {
            // Leave room for the request envelope, but never more than half the frame: a host
            // with a small frame limit still takes small calls (the margin was a flat 64 KiB,
            // which made every call over a frame under 64 KiB `request_too_large`).
            frame_limit - (64 * 1024).min(frame_limit / 2)
        } else {
            frame_limit / 2
        };
        let mut frame = if text.len() <= single_max {
            drop(text);
            self.call_step(method, params, max_message > 0, &mut *on_event)?
        } else {
            if max_message == 0 || text.len() > max_message {
                return Err(ServiceError::new(
                    ERROR_REQUEST_TOO_LARGE,
                    if max_message == 0 {
                        "the request exceeds the frame limit and the host does not chunk"
                            .to_string()
                    } else {
                        format!(
                            "the request is {} bytes, over the host's {max_message}-byte message limit",
                            text.len()
                        )
                    },
                ));
            }
            drop(params);
            let ends = boundaries(&text, chunk);
            let upload_id = format!("u{}", self.next_id.fetch_add(1, Ordering::SeqCst));
            let mut start = 0;
            let mut answer = Value::Null;
            for (seq, end) in ends.iter().enumerate() {
                let last = seq + 1 == ends.len();
                let mut piece = json!({
                    "id": upload_id, "seq": seq, "last": last, "text": &text[start..*end],
                });
                if seq == 0 {
                    piece["method"] = Value::String(method.to_string());
                }
                start = *end;
                let frame = self.call_step(METHOD_UPLOAD, piece, last, &mut *on_event)?;
                if last {
                    answer = frame;
                }
            }
            answer
        };
        let held = frame.get_mut("chunked").map(Value::take);
        let Some(held) = held else {
            return Ok(frame["result"].take());
        };
        // The result came back in pieces.
        let bad = |why: &str| ServiceError::new(ERROR_BAD_CHUNK, why.to_string());
        let id = held["id"]
            .as_str()
            .ok_or_else(|| bad("the chunked result has no id"))?
            .to_string();
        let bytes = held["bytes"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| bad("the chunked result has no size"))?;
        let count = held["count"]
            .as_u64()
            .ok_or_else(|| bad("the chunked result has no count"))?;
        if bytes > max_message || count > bytes.max(1) as u64 {
            return Err(bad(
                "the chunked result is larger than the host's own message limit",
            ));
        }
        let mut joined = String::with_capacity(bytes);
        for seq in 0..count {
            let request = json!({"id": id, "seq": seq});
            let piece = self.call_step(METHOD_FETCH, request, false, &mut *on_event)?;
            let part = piece["result"]["text"]
                .as_str()
                .ok_or_else(|| bad("a result piece has no text"))?;
            if joined.len() + part.len() > bytes {
                return Err(bad("the result pieces add up to more than announced"));
            }
            joined.push_str(part);
        }
        if joined.len() != bytes {
            return Err(bad("the result pieces add up to less than announced"));
        }
        serde_json::from_str(&joined)
            .map_err(|e| bad(&format!("the assembled result is not JSON: {e}")))
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
    /// the request was written is returned as-is, and a timeout is returned as-is.
    /// When true, a connection that closed after the write and a timeout are resubmitted too.
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

/// One attempt: `call_large`, and when `stop` is given a watcher thread that aborts the connection
/// once `stop` returns `true` while the call is in flight.
fn run_attempt(
    c: &Client,
    method: &str,
    params: &Value,
    stop: Option<&(dyn Fn() -> bool + Sync)>,
    on_event: &mut dyn FnMut(&Value),
) -> Result<Value, ServiceError> {
    let Some(stop) = stop else {
        return c.call_large(method, params.clone(), on_event);
    };
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                if stop() {
                    c.abort();
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        let result = c.call_large(method, params.clone(), on_event);
        done.store(true, Ordering::SeqCst);
        result
    })
}

/// Sleep for `d`, in slices of at most 25 ms when a stop hook is given, returning early once it
/// reports `true`.
fn sleep_unless_stopped(d: Duration, stop: Option<&(dyn Fn() -> bool + Sync)>) {
    let Some(stop) = stop else {
        std::thread::sleep(d);
        return;
    };
    let end = Instant::now() + d;
    loop {
        if stop() {
            return;
        }
        let now = Instant::now();
        if now >= end {
            return;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(25)));
    }
}

impl Client {
    /// Connect, call, and on a retryable error (host handoff, closed connection,
    /// service unavailable) re-read discovery and resubmit the whole request.
    /// The host is not authenticated: see [`Client::connect`].
    pub fn call_retrying(
        suite: &Suite,
        service: &str,
        app: &str,
        method: &str,
        params: Value,
        policy: RetryPolicy,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        Self::retry_loop(
            &|deadline| Self::connect_bounded(suite, service, app, deadline, None, None),
            method,
            params,
            policy,
            None,
            on_event,
        )
    }

    /// [`Client::call_retrying`] that verifies the host on every attempt, as
    /// [`Client::connect_expecting`] does. A host identity refusal is not retryable and is
    /// returned at once as a [`ServiceError`] with code `host_identity`
    /// ([`ERROR_HOST_IDENTITY`]); it is never replayed against another attempt. The per-attempt
    /// deadline is `policy.request_deadline`, not the configuration's.
    pub fn call_retrying_expecting(
        config: &ClientConfig,
        expect: &HostExpectation,
        method: &str,
        params: Value,
        policy: RetryPolicy,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        let suite = config.resolve_suite().map_err(suite_error)?;
        Self::retry_loop(
            &|deadline| {
                Self::connect_bounded(
                    &suite,
                    &config.service,
                    &config.app,
                    deadline,
                    Some(expect.clone()),
                    config.secret.clone(),
                )
            },
            method,
            params,
            policy,
            None,
            on_event,
        )
    }

    /// Retry over a connection source the caller owns, instead of a fresh connection per call:
    /// `connect` returns a client for each attempt (a held client, a pool, or a closure that
    /// connects with its own verification). The request goes through [`Client::call_large`], so a
    /// request or result larger than one frame works, and the same retry rules as
    /// [`Client::call_retrying`] apply (see [`RetryPolicy::idempotent`]). `connect` gets the
    /// per-attempt deadline from `policy.request_deadline`.
    pub fn call_retrying_over<F>(
        connect: F,
        method: &str,
        params: Value,
        policy: RetryPolicy,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError>
    where
        F: Fn(Option<Duration>) -> Result<Client, ConnectError>,
    {
        Self::retry_loop(&connect, method, params, policy, None, on_event)
    }

    /// [`Client::call_retrying_over`] for a call that runs until it is stopped, such as a long event
    /// subscription. `stop` is checked before each attempt, during the backoff, and while an
    /// attempt is in flight (a watcher thread aborts the connection once it returns `true`, which
    /// ends that attempt with `connection_closed` and no further attempt is made). For a call
    /// with no end, also set `policy.give_up_after` to `Duration::MAX`: the give-up limit counts
    /// from the first attempt and would otherwise end it.
    pub fn call_retrying_until<F>(
        connect: F,
        method: &str,
        params: Value,
        policy: RetryPolicy,
        stop: &(dyn Fn() -> bool + Sync),
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError>
    where
        F: Fn(Option<Duration>) -> Result<Client, ConnectError>,
    {
        Self::retry_loop(&connect, method, params, policy, Some(stop), on_event)
    }

    fn retry_loop(
        connect: &dyn Fn(Option<Duration>) -> Result<Client, ConnectError>,
        method: &str,
        params: Value,
        policy: RetryPolicy,
        stop: Option<&(dyn Fn() -> bool + Sync)>,
        on_event: &mut dyn FnMut(&Value),
    ) -> Result<Value, ServiceError> {
        let started = Instant::now();
        let stopped = || stop.is_some_and(|f| f());
        let mut last = ServiceError::retryable(ERROR_UNAVAILABLE, "no attempt made");
        for attempt in 0..policy.max_attempts.max(1) {
            if attempt > 0 {
                if stopped() || started.elapsed() > policy.give_up_after {
                    break;
                }
                sleep_unless_stopped(policy.backoff * attempt.min(8), stop);
                if stopped() {
                    break;
                }
            }
            let c = match connect(policy.request_deadline) {
                Ok(c) => c,
                Err(ConnectError::Service(e)) if e.retryable => {
                    last = e; // never reached the host's request path
                    continue;
                }
                Err(e) => return Err(e.into_service_error()),
            };
            match run_attempt(&c, method, &params, stop, on_event) {
                Ok(v) => return Ok(v),
                // Handoff means the host declined the request. A closed connection or a timeout
                // may have run it: replayed only when the caller says the request is idempotent.
                Err(e)
                    if (e.retryable && (e.code == ERROR_HOST_HANDOFF || policy.idempotent))
                        || (e.code == ERROR_TIMEOUT && policy.idempotent) =>
                {
                    last = e
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_unhardened_resolve_with_the_default_deadline() {
        let cfg = ClientConfig::new("svc", "app");
        assert_eq!((cfg.service(), cfg.app()), ("svc", "app"));
        assert!(!cfg.ignores_env_overrides());
        assert_eq!(cfg.deadline, Some(DEFAULT_DEADLINE));
    }

    #[test]
    fn an_explicit_suite_is_used_as_is_unless_it_came_from_the_environment_and_the_switch_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = Suite::at(dir.path()).unwrap();
        for ignore in [false, true] {
            let cfg = ClientConfig::new("svc", "app")
                .suite(explicit.clone())
                .ignore_env_overrides(ignore);
            assert_eq!(cfg.resolve_suite().unwrap().root(), explicit.root());
        }
        // `Suite::resolve` fails only where the environment names no root at all.
        #[cfg(feature = "test-mode")]
        if let Ok(from_env) = Suite::resolve() {
            let lenient = ClientConfig::new("svc", "app").suite(from_env);
            assert!(lenient.resolve_suite().is_ok());
            let strict = lenient.ignore_env_overrides(true);
            assert!(matches!(
                strict.resolve_suite(),
                Err(SuiteError::Insecure(_))
            ));
        }
    }

    #[test]
    fn a_missing_record_is_a_retryable_service_error_not_an_identity_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ClientConfig::new("nobody-home", "app").suite(Suite::at(dir.path()).unwrap());
        let err = Client::connect_expecting(&cfg, &HostExpectation::executable("/x"))
            .err()
            .expect("there is no host");
        match err {
            ConnectError::Service(e) => {
                assert_eq!(e.code, ERROR_UNAVAILABLE);
                assert!(e.retryable);
            }
            other => panic!("expected a service error, got {other:?}"),
        }
    }
}
