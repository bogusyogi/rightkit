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

use std::collections::HashMap;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn presence() -> Presence {
        Presence::new(PresenceConfig::default())
    }

    #[test]
    fn dates_are_formatted_as_utc() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601_utc(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(iso8601_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn attach_heartbeat_and_detach_follow_the_contract() {
        let p = presence();
        let t0 = Instant::now();
        assert_eq!(
            p.attach("window", 1, None, t0).unwrap_err(),
            AttachError::BadKind
        );
        let app = p
            .attach(" APP ", 10, Some("tauri-shell".into()), t0)
            .unwrap();
        assert!(app.client_id.starts_with("c-"));
        assert_eq!((app.heartbeat_seconds, app.expiry_seconds), (10, 30));
        // A second live app is a conflict; a cli is not.
        assert_eq!(
            p.attach("app", 11, None, t0).unwrap_err(),
            AttachError::Conflict
        );
        let cli = p.attach("cli", 12, None, t0).unwrap();
        assert_eq!(p.live_count(t0), 2);
        // Heartbeats keep a lease; an unknown id is not found.
        assert!(p.heartbeat(&app.client_id, t0 + Duration::from_secs(25)));
        assert!(!p.heartbeat("c-unknown", t0));
        // Past the expiry without a heartbeat the cli is gone, the app (heartbeated) lives.
        let later = t0 + Duration::from_secs(40);
        assert_eq!(p.live_count(later), 1);
        assert!(!p.heartbeat(&cli.client_id, later));
        // An expired app no longer blocks a new one.
        let much_later = t0 + Duration::from_secs(120);
        assert!(p.attach("app", 13, None, much_later).is_ok());
        // Explicit detach.
        let listed = p.list(much_later);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pid, 13);
        assert!(p.detach(&listed[0].client_id, much_later));
        assert!(!p.detach(&listed[0].client_id, much_later));
        assert_eq!(p.live_count(much_later), 0);
    }

    #[test]
    fn the_exit_rule_matches_coderight() {
        let p = presence();
        let t0 = Instant::now();
        // A host that never saw a client never exits.
        assert!(!p.should_shut_down(t0 + Duration::from_secs(3600), false));
        let a = p.attach("app", 1, None, t0).unwrap();
        // A live client keeps it up.
        assert!(!p.should_shut_down(t0, false));
        assert!(p.detach(&a.client_id, t0 + Duration::from_secs(1)));
        let zero = t0 + Duration::from_secs(1);
        // Zero, but the linger (5 s) has not passed.
        assert!(!p.should_shut_down(zero + Duration::from_secs(4), false));
        // Linger passed but something is in flight.
        assert!(!p.should_shut_down(zero + Duration::from_secs(6), true));
        // Linger passed and idle: exit.
        assert!(p.should_shut_down(zero + Duration::from_secs(6), false));
        // A new client resets the count.
        assert!(p
            .attach("cli", 2, None, zero + Duration::from_secs(7))
            .is_ok());
        assert!(!p.should_shut_down(zero + Duration::from_secs(8), false));
    }

    #[test]
    fn the_reaper_starts_the_linger_when_leases_expire() {
        let p = presence();
        let t0 = Instant::now();
        p.attach("app", 1, None, t0).unwrap();
        let expired_at = t0 + Duration::from_secs(31);
        assert_eq!(p.reap(expired_at), 1);
        assert!(!p.should_shut_down(expired_at + Duration::from_secs(1), false));
        assert!(p.should_shut_down(expired_at + Duration::from_secs(5), false));
    }

    #[test]
    fn records_serialise_as_coderight_does() {
        let p = presence();
        let t0 = Instant::now();
        let grant = p.attach("app", 7, Some("tauri-shell".into()), t0).unwrap();
        let json = grant.to_json();
        assert_eq!(json["heartbeatSeconds"], 10);
        assert_eq!(json["expirySeconds"], 30);
        let listed = p.list(t0);
        let record = listed[0].to_json();
        assert_eq!(record["kind"], "app");
        assert_eq!(record["pid"], 7);
        assert_eq!(record["label"], "tauri-shell");
        assert!(record["attachedAt"].as_str().unwrap().ends_with('Z'));
        let unlabelled = p.attach("cli", 8, None, t0).unwrap();
        let again = p.list(t0);
        let cli = again
            .iter()
            .find(|c| c.client_id == unlabelled.client_id)
            .unwrap();
        assert!(cli.to_json().get("label").is_none());
    }
}
