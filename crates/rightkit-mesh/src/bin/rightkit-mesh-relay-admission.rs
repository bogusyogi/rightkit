//! `rightkit-mesh-relay-admission`: the customer-grade admission service for OUR `iroh-relay`
//! (#952). It extends the circle-only callback `rightkit-mesh-admission` (Pulse `relay-tools`)
//! with device registration, pairing attestation, eviction without a relay restart, and quotas
//! per circle and per network prefix. The rules are in `rightkit_mesh::mailbox::registry`.
//!
//! Three endpoints, all loopback only (a TLS reverse proxy fronts `/register` and `/pow` for
//! customers and sets `X-Forwarded-For`; the service uses the LAST entry, which only the proxy can
//! have written):
//!
//! * `POST /admit` (the relay's callback, `Authorization: Bearer <relay token>`, header
//!   `X-Iroh-NodeId` or `X-Iroh-Endpoint-Id`): `200 true` only for an id in the registry or in the
//!   operator allowlist (`--allow-file`, the floor for the owner's own computers and the box), or
//!   holding an unexpired grant (`--grant-file`); anything else is `403 false`.
//! * `POST /register` (no token; JSON `RegistrationRequest`): proof of work or an attestation from
//!   a member. `200 {"ok":true}` or `4xx {"error":"<code>"}`.
//! * `POST /evict` (`Authorization: Bearer <admin token>`; JSON `{"endpoint_id":".."}` or
//!   `{"circle_id":".."}`): the device stops being admitted at its next connection, no restart.
//!   A connection that is already up is not cut (iroh-relay has no per-connection revocation).
//! * `GET /pow`: `{"bits":N}`, the difficulty a client must meet.
//!
//! Arguments: `--state-file PATH` (required), `--allow-file PATH` (required), `--grant-file PATH`,
//! `--bind ADDR` (loopback, default 127.0.0.1:8082), `--pow-bits N`, `--max-per-circle N`,
//! `--max-per-prefix N`. Environment: `IROH_RELAY_HTTP_BEARER_TOKEN` (the relay's token) and
//! `RIGHTKIT_RELAY_ADMIN_TOKEN`, each at least 32 characters and different from each other.
//! Events are JSON lines on stderr, counts only.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rightkit_mesh::mailbox::admission::{Allowlist, GrantList, is_endpoint_id};
use rightkit_mesh::mailbox::registry::{
    AdmissionRegistry, Limits, RegisterError, RegistrationRequest, network_prefix,
};
use rightkit_mesh::mailbox::server::event;
use serde_json::{Value, json};

const RELAY_TOKEN_ENV: &str = "IROH_RELAY_HTTP_BEARER_TOKEN";
const ADMIN_TOKEN_ENV: &str = "RIGHTKIT_RELAY_ADMIN_TOKEN";
const DEFAULT_BIND: &str = "127.0.0.1:8082";
const MAX_HEAD_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 4 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 64;

struct Args {
    state_file: PathBuf,
    allow_file: PathBuf,
    grant_file: Option<PathBuf>,
    bind: SocketAddr,
    limits: Limits,
}

fn parse_args() -> Result<Args, String> {
    let mut state_file = None;
    let mut allow_file = None;
    let mut grant_file = None;
    let mut bind = DEFAULT_BIND.to_string();
    let mut limits = Limits::default();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        let number = |text: String| {
            text.parse::<usize>()
                .map_err(|_| format!("{flag}: not a number"))
        };
        match flag.as_str() {
            "--state-file" => state_file = Some(PathBuf::from(value()?)),
            "--allow-file" => allow_file = Some(PathBuf::from(value()?)),
            "--grant-file" => grant_file = Some(PathBuf::from(value()?)),
            "--bind" => bind = value()?,
            "--pow-bits" => {
                limits.pow_bits =
                    u32::try_from(number(value()?)?).map_err(|_| "--pow-bits: too large")?
            }
            "--max-per-circle" => limits.max_devices_per_circle = number(value()?)?,
            "--max-per-prefix" => limits.max_devices_per_prefix = number(value()?)?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let bind: SocketAddr = bind.parse().map_err(|_| "--bind: not an address:port")?;
    if !bind.ip().is_loopback() {
        return Err("--bind must be a loopback address".to_string());
    }
    if limits.pow_bits > 40 {
        return Err("--pow-bits above 40 cannot be met by a phone".to_string());
    }
    Ok(Args {
        state_file: state_file.ok_or("--state-file is required")?,
        allow_file: allow_file.ok_or("--allow-file is required")?,
        grant_file,
        bind,
        limits,
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Compares two byte strings without stopping at the first difference.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn bearer_is(&self, token: &str) -> bool {
        self.header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| same_secret(t.trim().as_bytes(), token.as_bytes()))
    }

    /// The client address the reverse proxy saw: the last `X-Forwarded-For` entry, else loopback.
    fn client_ip(&self) -> IpAddr {
        self.header("x-forwarded-for")
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return None;
        }
    };
    let head = String::from_utf8(buffer[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.split('?').next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY_BYTES {
        return None;
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > MAX_BODY_BYTES {
            return None;
        }
    }
    body.truncate(length);
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn respond_json(stream: &mut TcpStream, status: &str, value: &Value) {
    respond(stream, status, "application/json", &value.to_string());
}

struct Service {
    registry: AdmissionRegistry,
    allow: Allowlist,
    grants: Option<GrantList>,
    relay_token: String,
    admin_token: String,
}

impl Service {
    fn handle(&self, stream: &mut TcpStream, request: &Request) {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/admit") => {
                let allowed = self.admit(request);
                event(
                    if allowed {
                        "admission.allow"
                    } else {
                        "admission.deny"
                    },
                    1,
                );
                if allowed {
                    respond(stream, "200 OK", "text/plain", "true");
                } else {
                    respond(stream, "403 Forbidden", "text/plain", "false");
                }
            }
            ("GET", "/pow") => respond_json(
                stream,
                "200 OK",
                &json!({ "bits": self.registry.limits().pow_bits }),
            ),
            ("POST", "/register") => self.register(stream, request),
            ("POST", "/evict") => self.evict(stream, request),
            _ => respond_json(stream, "404 Not Found", &json!({ "error": "not_found" })),
        }
    }

    fn admit(&self, request: &Request) -> bool {
        if !request.bearer_is(&self.relay_token) {
            return false;
        }
        let Some(id) = request
            .header("x-iroh-endpoint-id")
            .or_else(|| request.header("x-iroh-nodeid"))
        else {
            return false;
        };
        is_endpoint_id(id)
            && (self.registry.admits(id)
                || self.allow.contains(id)
                || self.grants.as_ref().is_some_and(|g| g.admits(id, now_ms())))
    }

    fn register(&self, stream: &mut TcpStream, request: &Request) {
        let Ok(registration) = serde_json::from_slice::<RegistrationRequest>(&request.body) else {
            respond_json(
                stream,
                "400 Bad Request",
                &json!({ "error": "bad_request" }),
            );
            return;
        };
        let prefix = network_prefix(&request.client_ip());
        match self.registry.register(&registration, &prefix, now_ms()) {
            Ok(()) => respond_json(stream, "200 OK", &json!({ "ok": true })),
            Err(error) => {
                event("registry.refused", 1);
                let status = match error {
                    RegisterError::Persist(_) => "500 Internal Server Error",
                    RegisterError::CircleFull
                    | RegisterError::PrefixFull
                    | RegisterError::CapacityFull => "429 Too Many Requests",
                    _ => "403 Forbidden",
                };
                respond_json(stream, status, &json!({ "error": error.code() }));
            }
        }
    }

    fn evict(&self, stream: &mut TcpStream, request: &Request) {
        if !request.bearer_is(&self.admin_token) {
            respond_json(stream, "403 Forbidden", &json!({ "error": "forbidden" }));
            return;
        }
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            respond_json(
                stream,
                "400 Bad Request",
                &json!({ "error": "bad_request" }),
            );
            return;
        };
        let result = if let Some(id) = body.get("endpoint_id").and_then(Value::as_str) {
            self.registry.evict(id).map(|_| 1)
        } else if let Some(circle) = body.get("circle_id").and_then(Value::as_str) {
            self.registry.evict_circle(circle)
        } else {
            respond_json(
                stream,
                "400 Bad Request",
                &json!({ "error": "bad_request" }),
            );
            return;
        };
        match result {
            Ok(count) => respond_json(stream, "200 OK", &json!({ "evicted": count })),
            Err(_) => respond_json(
                stream,
                "500 Internal Server Error",
                &json!({ "error": "persist_failed" }),
            ),
        }
    }
}

fn token_from_env(name: &str) -> Result<String, String> {
    let token = std::env::var(name).unwrap_or_default();
    if token.len() < 32 {
        return Err(format!(
            "{name} must be set to a secret of at least 32 characters"
        ));
    }
    Ok(token)
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let relay_token = token_from_env(RELAY_TOKEN_ENV)?;
    let admin_token = token_from_env(ADMIN_TOKEN_ENV)?;
    if relay_token == admin_token {
        return Err(format!(
            "{RELAY_TOKEN_ENV} and {ADMIN_TOKEN_ENV} must differ"
        ));
    }
    let service = Arc::new(Service {
        registry: AdmissionRegistry::open(&args.state_file, args.limits)?,
        allow: Allowlist::open(&args.allow_file)?,
        grants: args.grant_file.as_deref().map(GrantList::new),
        relay_token,
        admin_token,
    });
    let listener = TcpListener::bind(args.bind).map_err(|e| format!("{}: {e}", args.bind))?;
    event("admission.start", service.registry.device_count() as u64);
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            continue; // dropped: the relay treats a failed check as Deny
        }
        let (service, active) = (service.clone(), active.clone());
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            match read_request(&mut stream) {
                Some(request) => service.handle(&mut stream, &request),
                None => respond_json(
                    &mut stream,
                    "400 Bad Request",
                    &json!({ "error": "bad_request" }),
                ),
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rightkit-mesh-relay-admission: {message}");
            ExitCode::FAILURE
        }
    }
}
