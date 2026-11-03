//! The client half of the loopback host (rightkit#1119): find or wait for the one daemon, probe
//! its `GET /health`, and keep a presence attachment alive.
//!
//! Any of a desktop shell, a CLI or a TUI may need the daemon. Exactly one daemon must result,
//! later clients adopt it, a client that lost the spawn claim waits for the winner's record and
//! never starts a second child, and every long-lived client attaches, heartbeats at the cadence
//! the daemon granted and detaches on exit so the daemon can leave after the last one.
//!
//! * [`adopt_or_spawn`] reads the owner's `daemon.port` ([`LoopbackRecord`]) and answers
//!   [`LoopbackAdopt::Adopt`] only for a record whose process is the one that started (pid plus
//!   birth time, so a recycled pid is not adopted) and that the caller's probe, normally
//!   [`probe_health`], says answers.
//! * [`wait_for_claim_winner`] polls the record path for such a record until a timeout.
//! * [`PresenceClient`] attaches (`POST /clients`), heartbeats (`POST /clients/{id}/heartbeat`),
//!   re-attaches once when the daemon forgot it (404), retries transport failures a bounded number
//!   of times, and detaches (`DELETE /clients/{id}`).
//!
//! The transport is plain HTTP/1.1 to `127.0.0.1` with the record's bearer token, on std sockets:
//! no tokio and no TLS, the same as the host.

use super::record::LoopbackRecord;
use crate::stop::StopToken;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Largest response body this client reads.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Largest header block.
const MAX_HEAD: usize = 64 * 1024;

/// One HTTP reply (the client sends `Connection: close`).
pub(crate) struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// An open request whose response head was read and whose body has not been.
pub(crate) struct Open {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub reader: BufReader<TcpStream>,
}

impl Open {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn chunked(&self) -> bool {
        self.header("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    }
}

/// Send one request and read the response head.
pub(crate) fn open(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    extra: &[(&str, String)],
    body: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Open> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    let mut writer = stream.try_clone()?;
    writer.write_all(head.as_bytes())?;
    if let Some(body) = body {
        writer.write_all(body)?;
    }
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    read_line(&mut reader, &mut line, MAX_HEAD)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed status line"))?;
    let mut headers = Vec::new();
    let mut total = line.len();
    loop {
        line.clear();
        read_line(&mut reader, &mut line, MAX_HEAD)?;
        total += line.len();
        if total > MAX_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response head too large",
            ));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(Open {
        status,
        headers,
        reader,
    })
}

fn read_line(reader: &mut impl BufRead, out: &mut String, limit: usize) -> io::Result<()> {
    let mut bytes = Vec::new();
    let n = reader.take(limit as u64).read_until(b'\n', &mut bytes)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    *out = String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response head is not UTF-8"))?;
    Ok(())
}

/// Send a request and read the whole (bounded) reply.
pub(crate) fn request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Reply> {
    let mut open = open(port, method, path, token, &[], body, timeout)?;
    let mut out = Vec::new();
    if open.chunked() {
        read_chunked(&mut open.reader, &mut out)?;
    } else if let Some(len) = open
        .header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        if len > MAX_BODY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response body too large",
            ));
        }
        out.resize(len, 0);
        open.reader.read_exact(&mut out)?;
    } else {
        (&mut open.reader)
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut out)?;
        if out.len() > MAX_BODY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response body too large",
            ));
        }
    }
    Ok(Reply {
        status: open.status,
        body: out,
    })
}

/// Read a chunked body to its end (bounded).
fn read_chunked(reader: &mut impl BufRead, out: &mut Vec<u8>) -> io::Result<()> {
    loop {
        let mut line = String::new();
        read_line(reader, &mut line, 1024)?;
        let size_text = line.trim().split(';').next().unwrap_or("");
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            return Ok(());
        }
        if out.len() + size > MAX_BODY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response body too large",
            ));
        }
        let start = out.len();
        out.resize(start + size, 0);
        reader.read_exact(&mut out[start..])?;
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf)?;
    }
}

/// `GET /health` (no credentials) answers 200 within `timeout`.
pub fn probe_health(record: &LoopbackRecord, timeout: Duration) -> bool {
    request(record.port, "GET", "/health", None, None, timeout)
        .map(|reply| reply.status == 200)
        .unwrap_or(false)
}

/// [`probe_health`] up to `attempts` times, `interval` apart, true on the first success. A daemon
/// that is busy starting is not declared dead by one missed probe.
pub fn probe_health_repeated(
    record: &LoopbackRecord,
    attempts: u32,
    interval: Duration,
    timeout: Duration,
) -> bool {
    for i in 0..attempts.max(1) {
        if probe_health(record, timeout) {
            return true;
        }
        if i + 1 < attempts {
            std::thread::sleep(interval);
        }
    }
    false
}

/// What [`adopt_or_spawn`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopbackAdopt {
    /// A live daemon of the recorded process: use it.
    Adopt(LoopbackRecord),
    /// No usable daemon: this client may take the spawn claim and start one.
    Spawn,
}

/// Read the record at `record_path` and decide. A missing, malformed or stale record (its process
/// is not the one that started: a dead or recycled pid) is [`LoopbackAdopt::Spawn`]; a record of
/// the right process that `probe` says does not answer is [`LoopbackAdopt::Spawn`] too. The
/// caller holds the spawn claim ([`SpawnClaim`](crate::SpawnClaim)) before it acts on `Spawn`.
pub fn adopt_or_spawn(
    record_path: &Path,
    probe: impl Fn(&LoopbackRecord) -> bool,
) -> LoopbackAdopt {
    let Some(record) = std::fs::read_to_string(record_path)
        .ok()
        .and_then(|text| LoopbackRecord::parse(&text))
    else {
        return LoopbackAdopt::Spawn;
    };
    if record.process_matches() && probe(&record) {
        LoopbackAdopt::Adopt(record)
    } else {
        LoopbackAdopt::Spawn
    }
}

/// The caller lost the spawn claim: wait up to `timeout` for the winner's record to appear and
/// answer `probe`, checking every `poll`. `None` at the timeout; the caller then reports the
/// failure and does not start a second child. An unreadable record is "not yet".
pub fn wait_for_claim_winner(
    record_path: &Path,
    timeout: Duration,
    poll: Duration,
    probe: impl Fn(&LoopbackRecord) -> bool,
) -> Option<LoopbackRecord> {
    let deadline = Instant::now() + timeout;
    loop {
        if let LoopbackAdopt::Adopt(record) = adopt_or_spawn(record_path, &probe) {
            return Some(record);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        std::thread::sleep(poll.max(Duration::from_millis(5)).min(remaining));
    }
}

/// Why a presence call failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum PresenceError {
    /// An `app` client is already live (409), also after the one retry.
    Conflict,
    /// The daemon holds its maximum of clients (503).
    Full,
    /// The daemon refused the kind or the body (400).
    Rejected,
    /// The bearer token was refused (401, 403).
    Unauthorized,
    /// Some other status.
    Status(u16),
    /// The daemon could not be reached, also after the bounded retries.
    Transport(io::Error),
    /// The answer was not the attach grant.
    BadReply,
}

impl std::fmt::Display for PresenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => write!(f, "another app client is attached"),
            Self::Full => write!(f, "the daemon holds its maximum number of clients"),
            Self::Rejected => write!(f, "the daemon refused the attach request"),
            Self::Unauthorized => write!(f, "the daemon refused the token"),
            Self::Status(s) => write!(f, "the daemon answered {s}"),
            Self::Transport(e) => write!(f, "the daemon is unreachable: {e}"),
            Self::BadReply => write!(f, "the attach answer was malformed"),
        }
    }
}

impl std::error::Error for PresenceError {}

/// How many times a transport failure is retried, and how long a request may take.
const TRANSPORT_RETRIES: u32 = 3;
const RETRY_PAUSE: Duration = Duration::from_millis(200);
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// The pause before the one retry of an attach that met a live `app` client.
const CONFLICT_RETRY: Duration = Duration::from_millis(500);

struct Attachment {
    client_id: String,
    heartbeat: Duration,
}

/// A client's presence at a loopback daemon. Dropping it detaches (best effort).
pub struct PresenceClient {
    record: LoopbackRecord,
    kind: String,
    label: Option<String>,
    state: Mutex<Option<Attachment>>,
}

impl PresenceClient {
    /// Attach as `kind` (`"app"` or `"cli"`; the daemon refuses another). A 409 (a live `app`
    /// client holds the slot) is retried once after a short pause, then [`PresenceError::Conflict`].
    pub fn attach(
        record: &LoopbackRecord,
        kind: &str,
        label: Option<&str>,
    ) -> Result<Self, PresenceError> {
        let client = Self {
            record: record.clone(),
            kind: kind.to_string(),
            label: label.map(str::to_string),
            state: Mutex::new(None),
        };
        let first = client.attach_once();
        let attachment = match first {
            Err(PresenceError::Conflict) => {
                std::thread::sleep(CONFLICT_RETRY);
                client.attach_once()?
            }
            other => other?,
        };
        *client.state.lock().unwrap_or_else(|e| e.into_inner()) = Some(attachment);
        Ok(client)
    }

    fn call(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<Reply, PresenceError> {
        let mut last = None;
        for attempt in 0..=TRANSPORT_RETRIES {
            match request(
                self.record.port,
                method,
                path,
                Some(&self.record.token),
                body,
                CALL_TIMEOUT,
            ) {
                Ok(reply) => return Ok(reply),
                Err(e) => {
                    last = Some(e);
                    if attempt < TRANSPORT_RETRIES {
                        std::thread::sleep(RETRY_PAUSE);
                    }
                }
            }
        }
        Err(PresenceError::Transport(
            last.unwrap_or_else(|| io::Error::other("no attempt was made")),
        ))
    }

    fn attach_once(&self) -> Result<Attachment, PresenceError> {
        let body = json!({
            "kind": self.kind,
            "pid": std::process::id(),
            "label": self.label,
        })
        .to_string();
        let reply = self.call("POST", "/clients", Some(body.as_bytes()))?;
        match reply.status {
            200 => {
                let value: Value =
                    serde_json::from_slice(&reply.body).map_err(|_| PresenceError::BadReply)?;
                let client_id = value["clientId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or(PresenceError::BadReply)?
                    .to_string();
                let seconds = value["heartbeatSeconds"].as_u64().unwrap_or(10).max(1);
                Ok(Attachment {
                    client_id,
                    heartbeat: Duration::from_secs(seconds),
                })
            }
            400 => Err(PresenceError::Rejected),
            401 | 403 => Err(PresenceError::Unauthorized),
            409 => Err(PresenceError::Conflict),
            503 => Err(PresenceError::Full),
            other => Err(PresenceError::Status(other)),
        }
    }

    /// The id the daemon granted.
    pub fn client_id(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|a| a.client_id.clone())
    }

    /// The cadence the daemon asked for.
    pub fn heartbeat_interval(&self) -> Option<Duration> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|a| a.heartbeat)
    }

    /// One heartbeat. A 404 (the daemon forgot this client, for example after a long suspend)
    /// attaches again once and is not an error; transport failures are retried a bounded number of
    /// times first.
    pub fn heartbeat(&self) -> Result<(), PresenceError> {
        let Some(id) = self.client_id() else {
            return self.reattach();
        };
        let reply = self.call("POST", &format!("/clients/{id}/heartbeat"), None)?;
        match reply.status {
            204 | 200 => Ok(()),
            404 => self.reattach(),
            401 | 403 => Err(PresenceError::Unauthorized),
            other => Err(PresenceError::Status(other)),
        }
    }

    fn reattach(&self) -> Result<(), PresenceError> {
        let attachment = self.attach_once()?;
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = Some(attachment);
        Ok(())
    }

    /// Heartbeat on the granted cadence until `stop`, on a thread of its own. A failed heartbeat
    /// is passed to `on_error` and tried again at the next beat.
    pub fn run_heartbeat(
        self: &std::sync::Arc<Self>,
        stop: StopToken,
        mut on_error: impl FnMut(&PresenceError) + Send + 'static,
    ) -> io::Result<std::thread::JoinHandle<()>> {
        let client = self.clone();
        std::thread::Builder::new()
            .name("rk-presence".into())
            .spawn(move || loop {
                let every = client
                    .heartbeat_interval()
                    .unwrap_or(Duration::from_secs(10));
                if stop.wait(every).is_some() {
                    break;
                }
                if let Err(e) = client.heartbeat() {
                    on_error(&e);
                }
            })
    }

    /// Detach now (`DELETE /clients/{id}`), so the daemon can exit after the last client. Best
    /// effort: a daemon that is already gone is not an error.
    pub fn detach(&self) {
        let id = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .map(|a| a.client_id);
        if let Some(id) = id {
            let _ = request(
                self.record.port,
                "DELETE",
                &format!("/clients/{id}"),
                Some(&self.record.token),
                None,
                CALL_TIMEOUT,
            );
        }
    }
}

impl Drop for PresenceClient {
    fn drop(&mut self) {
        self.detach();
    }
}
