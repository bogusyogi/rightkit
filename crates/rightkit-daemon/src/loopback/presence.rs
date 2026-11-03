//! CodeRight's client-presence contract (v1), kept as it is on the wire:
//!
//! ```text
//! POST   /clients            {"kind":"app"|"cli","pid":<int>,"label":"..."}
//!                            -> 200 {"clientId","heartbeatSeconds","expirySeconds"} | 409 | 400
//! POST   /clients/{id}/heartbeat -> 204 | 404
//! DELETE /clients/{id}           -> 204 | 404
//! GET    /clients                -> 200 {"clients":[{"clientId","kind","pid","label",
//!                                                    "attachedAt","lastSeenAt"}]}
//! ```
//!
//! One live `app` client at a time (a second attach is 409), any number of `cli` clients. A client
//! that stops heartbeating for `expirySeconds` is gone. The exit rule is CodeRight's
//! `should_shut_down`: once any client has ever attached, the host may exit when the live count has
//! been zero for `lingerSeconds` and nothing is in flight; a host that never saw a client never
//! self-exits. The clock is passed in, never read for decisions, so tests inject time.
//!
//! Mirrors `engine/bins/coderight/src/api_clients.rs` (HEARTBEAT_SECONDS 10, EXPIRY_SECONDS 30,
//! LINGER_SECONDS 5, the 409 on a second live app, `should_shut_down`).

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// Most clients held at once (a bound the CodeRight contract does not state).
pub const MAX_CLIENTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenceConfig {
    pub heartbeat_seconds: u64,
    pub expiry_seconds: u64,
    pub linger_seconds: u64,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            heartbeat_seconds: 10,
            expiry_seconds: 30,
            linger_seconds: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRecord {
    pub client_id: String,
    pub kind: String,
    pub pid: i64,
    pub label: Option<String>,
    pub attached_at_ms: u64,
    pub last_seen_ms: u64,
}

/// What an attach answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachGrant {
    pub client_id: String,
    pub heartbeat_seconds: u64,
    pub expiry_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// The kind is neither `app` nor `cli` (400).
    BadKind,
    /// An `app` client is already live (409).
    Conflict,
    /// [`MAX_CLIENTS`] are held (503).
    Full,
}

struct Entry {
    record: ClientRecord,
    last_seen: Instant,
}

#[derive(Default)]
struct Inner {
    clients: HashMap<String, Entry>,
    ever_attached: bool,
    zero_since: Option<Instant>,
    counter: u64,
}

pub struct Presence {
    cfg: PresenceConfig,
    inner: Mutex<Inner>,
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Presence {
    pub fn new(cfg: PresenceConfig) -> Self {
        Self {
            cfg,
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn config(&self) -> PresenceConfig {
        self.cfg
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn expired(&self, entry: &Entry, now: Instant) -> bool {
        now.saturating_duration_since(entry.last_seen)
            > Duration::from_secs(self.cfg.expiry_seconds)
    }

    fn live_in(&self, inner: &Inner, now: Instant) -> usize {
        inner
            .clients
            .values()
            .filter(|entry| !self.expired(entry, now))
            .count()
    }

    /// Note a zero (or non-zero) live count at `now`, for the linger rule.
    fn refresh_zero(&self, inner: &mut Inner, now: Instant) {
        if self.live_in(inner, now) == 0 {
            if inner.zero_since.is_none() {
                inner.zero_since = Some(now);
            }
        } else {
            inner.zero_since = None;
        }
    }

    fn new_id(counter: &mut u64) -> String {
        let mut raw = [0u8; 16];
        if getrandom::fill(&mut raw).is_ok() {
            let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
            format!("c-{hex}")
        } else {
            // An opaque handle, not a secret: a counter and the time are enough.
            *counter += 1;
            format!("c-fallback-{}-{}", *counter, wall_ms())
        }
    }

    pub fn attach(
        &self,
        kind: &str,
        pid: i64,
        label: Option<String>,
        now: Instant,
    ) -> Result<AttachGrant, AttachError> {
        let kind = kind.trim().to_ascii_lowercase();
        if kind != "app" && kind != "cli" {
            return Err(AttachError::BadKind);
        }
        let mut inner = self.lock();
        // Expired leases are gone for every purpose; forget them here too.
        let expiry = Duration::from_secs(self.cfg.expiry_seconds);
        inner
            .clients
            .retain(|_, entry| now.saturating_duration_since(entry.last_seen) <= expiry);
        if kind == "app" && inner.clients.values().any(|e| e.record.kind == "app") {
            return Err(AttachError::Conflict);
        }
        if inner.clients.len() >= MAX_CLIENTS {
            return Err(AttachError::Full);
        }
        let client_id = Self::new_id(&mut inner.counter);
        let wall = wall_ms();
        inner.clients.insert(
            client_id.clone(),
            Entry {
                record: ClientRecord {
                    client_id: client_id.clone(),
                    kind,
                    pid,
                    label,
                    attached_at_ms: wall,
                    last_seen_ms: wall,
                },
                last_seen: now,
            },
        );
        inner.ever_attached = true;
        inner.zero_since = None;
        Ok(AttachGrant {
            client_id,
            heartbeat_seconds: self.cfg.heartbeat_seconds,
            expiry_seconds: self.cfg.expiry_seconds,
        })
    }

    /// Keep a client's lease alive. False when it is unknown or already expired.
    pub fn heartbeat(&self, client_id: &str, now: Instant) -> bool {
        let mut inner = self.lock();
        let expiry = Duration::from_secs(self.cfg.expiry_seconds);
        match inner.clients.get_mut(client_id) {
            Some(entry) if now.saturating_duration_since(entry.last_seen) <= expiry => {
                entry.last_seen = now;
                entry.record.last_seen_ms = wall_ms();
                true
            }
            _ => false,
        }
    }

    /// Explicit detach. False when the client is unknown.
    pub fn detach(&self, client_id: &str, now: Instant) -> bool {
        let mut inner = self.lock();
        let removed = inner.clients.remove(client_id).is_some();
        if removed {
            self.refresh_zero(&mut inner, now);
        }
        removed
    }

    pub fn live_count(&self, now: Instant) -> usize {
        let inner = self.lock();
        self.live_in(&inner, now)
    }

    /// The live clients, oldest attachment first.
    pub fn list(&self, now: Instant) -> Vec<ClientRecord> {
        let inner = self.lock();
        let mut live: Vec<ClientRecord> = inner
            .clients
            .values()
            .filter(|entry| !self.expired(entry, now))
            .map(|entry| entry.record.clone())
            .collect();
        live.sort_by(|a, b| {
            a.attached_at_ms
                .cmp(&b.attached_at_ms)
                .then_with(|| a.client_id.cmp(&b.client_id))
        });
        live
    }

    /// Remove expired leases. Returns how many.
    pub fn reap(&self, now: Instant) -> usize {
        let mut inner = self.lock();
        let before = inner.clients.len();
        let expiry = Duration::from_secs(self.cfg.expiry_seconds);
        inner
            .clients
            .retain(|_, entry| now.saturating_duration_since(entry.last_seen) <= expiry);
        let reaped = before - inner.clients.len();
        self.refresh_zero(&mut inner, now);
        reaped
    }

    pub fn ever_attached(&self) -> bool {
        self.lock().ever_attached
    }

    /// CodeRight's exit rule. `busy` is "something is in flight" (a running session).
    pub fn should_shut_down(&self, now: Instant, busy: bool) -> bool {
        let mut inner = self.lock();
        if !inner.ever_attached {
            return false;
        }
        if self.live_in(&inner, now) != 0 {
            inner.zero_since = None;
            return false;
        }
        // The first observation of zero starts the linger.
        if inner.zero_since.is_none() {
            inner.zero_since = Some(now);
        }
        if busy {
            return false;
        }
        inner.zero_since.is_some_and(|since| {
            now.saturating_duration_since(since) >= Duration::from_secs(self.cfg.linger_seconds)
        })
    }
}

impl AttachGrant {
    pub fn to_json(&self) -> Value {
        json!({
            "clientId": self.client_id,
            "heartbeatSeconds": self.heartbeat_seconds,
            "expirySeconds": self.expiry_seconds,
        })
    }
}

impl ClientRecord {
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "clientId": self.client_id,
            "kind": self.kind,
            "pid": self.pid,
            "attachedAt": iso8601_utc(self.attached_at_ms),
            "lastSeenAt": iso8601_utc(self.last_seen_ms),
        });
        if let (Some(label), Some(object)) = (&self.label, value.as_object_mut()) {
            object.insert("label".to_string(), Value::String(label.clone()));
        }
        value
    }
}

/// Milliseconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn iso8601_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
