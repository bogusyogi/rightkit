use crate::error::*;
use crate::host_identity::{HostConnectError, HostExpectation, HostProcess};
use crate::record::{try_read_record, ServiceRecord, PROTOCOL_MAJOR};
use crate::suite::{Suite, SuiteError};
use crate::transport::{connect, connect_expecting, Conn};
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
        }
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
        Self::connect_bounded(suite, service, app, deadline, None)
            .map_err(ConnectError::into_service_error)
    }

    /// Connect through a [`ClientConfig`]: the suite and deadline come from the configuration,
    /// and [`ClientConfig::ignore_env_overrides`] is honoured. The host is still not
    /// authenticated (see [`Client::connect`]); this hardens only where the client looks.
    pub fn connect_config(config: &ClientConfig) -> Result<Client, ServiceError> {
        let suite = config.resolve_suite().map_err(suite_error)?;
        Self::connect_bounded(&suite, &config.service, &config.app, config.deadline, None)
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
    ) -> Result<Client, ConnectError> {
        let Some(d) = deadline else {
            return Self::handshake(suite, service, app, None, expect.as_ref());
        };
        let (tx, rx) = mpsc::channel();
        let (s, sv, ap) = (suite.clone(), service.to_string(), app.to_string());
        std::thread::spawn(move || {
            let _ = tx.send(Self::handshake(&s, &sv, &ap, Some(d), expect.as_ref()));
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
            return Err(timed_out(&format!("request `{method}`"), total));
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
            &|deadline| Self::connect_bounded(suite, service, app, deadline, None),
            method,
            params,
            policy,
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
                )
            },
            method,
            params,
            policy,
            on_event,
        )
    }

    fn retry_loop(
        connect: &dyn Fn(Option<Duration>) -> Result<Client, ConnectError>,
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
            let c = match connect(policy.request_deadline) {
                Ok(c) => c,
                Err(ConnectError::Service(e)) if e.retryable => {
                    last = e; // never reached the host's request path
                    continue;
                }
                Err(e) => return Err(e.into_service_error()),
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
