//! Durable, consent-gated, redacting event outbox.
//!
//! Merged from HeardRight `telemetry_outbox.rs` (atomic ledger, corrupt-file
//! quarantine, id allocation, capped queue, ack / failure bookkeeping) and made
//! instance-based and product-neutral. Privacy rules enforced here, not by
//! callers:
//!
//! * `enqueue` refuses unless consent is granted at call time.
//! * every payload is key-redacted and string-scrubbed *before* it is stored,
//!   so PII never reaches disk, let alone the network.
//! * revocation purges the queue, including quarantined files.

use crate::{atomic, scrub, ConsentStore, RedactionPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

const QUARANTINE_MARKER: &str = ".corrupt-";
static QUARANTINE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct OutboxConfig {
    /// Oldest events are dropped beyond this many.
    pub max_events: usize,
    /// Events whose serialized payload exceeds this are refused, not truncated.
    pub max_event_bytes: usize,
    pub redaction: RedactionPolicy,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            max_events: 10_000,
            max_event_bytes: 64 * 1024,
            redaction: RedactionPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxEvent {
    pub id: u64,
    pub event_type: String,
    pub created_at_ms: i64,
    pub payload: Value,
    #[serde(default)]
    pub upload_attempts: u8,
    /// Do not retry before this wall-clock time; persisted so backoff survives restart.
    #[serde(default)]
    pub not_before_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ledger {
    events: Vec<OutboxEvent>,
    next_id: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EnqueueError {
    /// Consent is not granted; nothing was stored.
    NoConsent,
    TooLarge {
        bytes: usize,
        max: usize,
    },
    Storage(String),
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnqueueError::NoConsent => write!(f, "telemetry consent not granted"),
            EnqueueError::TooLarge { bytes, max } => {
                write!(f, "event is {bytes} bytes, limit is {max}")
            }
            EnqueueError::Storage(message) => write!(f, "outbox storage: {message}"),
        }
    }
}

impl std::error::Error for EnqueueError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutboxStatus {
    pub total: usize,
    pub error: Option<String>,
}

/// Runtime figures of the outbox, computed on call by [`Outbox::health`].
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxHealth {
    /// Events queued now (the backlog).
    pub events: usize,
    /// The configured event cap.
    pub max_events: usize,
    /// Why the ledger could not load, if it could not.
    pub error: Option<String>,
    /// Resident memory, CPU time and I/O counters of the whole process, or `None` when the host
    /// cannot report them. See `rightkit_process::ProcessUsage`.
    pub process: Option<rightkit_process::ProcessUsage>,
}

#[derive(Debug)]
struct State {
    ledger: Ledger,
    load_error: Option<String>,
}

#[derive(Debug)]
pub struct Outbox {
    path: PathBuf,
    config: OutboxConfig,
    consent: ConsentStore,
    state: Mutex<State>,
}

impl Outbox {
    /// Open (or create lazily) the outbox at `path`. A corrupt ledger is moved
    /// aside to `<name>.corrupt-*` and replaced with an empty one.
    pub fn open(path: impl Into<PathBuf>, consent: ConsentStore, config: OutboxConfig) -> Self {
        let path = path.into();
        let state = load(&path);
        Self {
            path,
            config,
            consent,
            state: Mutex::new(state),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn consent(&self) -> &ConsentStore {
        &self.consent
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Redact, scrub and persist one event. Returns its id.
    pub fn enqueue(&self, event_type: &str, payload: Value) -> Result<u64, EnqueueError> {
        if !self.consent.is_granted() {
            return Err(EnqueueError::NoConsent);
        }
        let mut payload = payload;
        self.config.redaction.redact_value(&mut payload);
        scrub::scrub_value(&mut payload);
        let bytes = serde_json::to_vec(&payload)
            .map(|b| b.len())
            .map_err(|e| EnqueueError::Storage(e.to_string()))?;
        if bytes > self.config.max_event_bytes {
            return Err(EnqueueError::TooLarge {
                bytes,
                max: self.config.max_event_bytes,
            });
        }
        let mut state = self.lock();
        self.reload_if_unavailable(&mut state);
        if let Some(error) = &state.load_error {
            return Err(EnqueueError::Storage(error.clone()));
        }
        let mut candidate = state.ledger.clone();
        let id = candidate.next_id.max(1);
        candidate.next_id = id + 1;
        candidate.events.push(OutboxEvent {
            id,
            event_type: scrub::scrub_text(event_type),
            created_at_ms: crate::now_ms(),
            payload,
            upload_attempts: 0,
            not_before_ms: 0,
        });
        if candidate.events.len() > self.config.max_events {
            let excess = candidate.events.len() - self.config.max_events;
            candidate.events.drain(0..excess);
        }
        self.persist(&mut state, candidate)
            .map(|_| id)
            .map_err(EnqueueError::Storage)
    }

    /// Events ready to send now (not in backoff), oldest first.
    pub fn pending(&self, limit: usize, now_ms: i64) -> Vec<OutboxEvent> {
        self.lock()
            .ledger
            .events
            .iter()
            .filter(|event| event.not_before_ms <= now_ms)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Earliest `not_before_ms` among queued events, if any are waiting.
    pub fn next_ready_at_ms(&self) -> Option<i64> {
        self.lock()
            .ledger
            .events
            .iter()
            .map(|e| e.not_before_ms)
            .min()
    }

    /// Remove delivered (or permanently rejected) events.
    pub fn acknowledge(&self, ids: &[u64]) -> io::Result<()> {
        let mut state = self.lock();
        let mut candidate = state.ledger.clone();
        candidate.events.retain(|event| !ids.contains(&event.id));
        self.persist(&mut state, candidate)
            .map_err(io::Error::other)
    }

    /// Count a failed attempt and hold the events until `not_before_ms`.
    pub fn record_failure(&self, ids: &[u64], not_before_ms: i64) -> io::Result<()> {
        let mut state = self.lock();
        let mut candidate = state.ledger.clone();
        for event in candidate.events.iter_mut().filter(|e| ids.contains(&e.id)) {
            event.upload_attempts = event.upload_attempts.saturating_add(1).min(8);
            event.not_before_ms = not_before_ms;
        }
        self.persist(&mut state, candidate)
            .map_err(io::Error::other)
    }

    pub fn status(&self) -> OutboxStatus {
        let state = self.lock();
        OutboxStatus {
            total: state.ledger.events.len(),
            error: state.load_error.clone(),
        }
    }

    /// Backlog and process resource usage, read now. No polling thread runs for this.
    pub fn health(&self) -> OutboxHealth {
        let state = self.lock();
        OutboxHealth {
            events: state.ledger.events.len(),
            max_events: self.config.max_events,
            error: state.load_error.clone(),
            process: rightkit_process::process_usage().ok(),
        }
    }

    pub fn len(&self) -> usize {
        self.lock().ledger.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Delete every queued event and any quarantined ledger bytes.
    pub fn purge(&self) -> io::Result<()> {
        let mut state = self.lock();
        state.ledger = Ledger::default();
        state.load_error = None;
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        remove_quarantined(&self.path)
    }

    fn reload_if_unavailable(&self, state: &mut State) {
        if state.load_error.is_some() {
            let refreshed = load(&self.path);
            if refreshed.load_error.is_none() {
                *state = refreshed;
            }
        }
    }

    fn persist(&self, state: &mut State, candidate: Ledger) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&candidate).map_err(|e| e.to_string())?;
        atomic::write_atomic(&self.path, &bytes).map_err(|e| e.to_string())?;
        state.ledger = candidate;
        Ok(())
    }
}

fn load(path: &Path) -> State {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Ledger>(&bytes) {
            Ok(ledger) => State {
                ledger,
                load_error: None,
            },
            Err(parse_error) => match quarantine(path) {
                Ok(_) => State {
                    ledger: Ledger::default(),
                    load_error: None,
                },
                Err(error) => State {
                    ledger: Ledger::default(),
                    load_error: Some(format!("parse {}: {parse_error}; {error}", path.display())),
                },
            },
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => State {
            ledger: Ledger::default(),
            load_error: None,
        },
        Err(error) => State {
            ledger: Ledger::default(),
            load_error: Some(format!("read {}: {error}", path.display())),
        },
    }
}

fn quarantine(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("outbox.json");
    let counter = QUARANTINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let target = path.with_file_name(format!(
        "{name}{QUARANTINE_MARKER}{}-{counter}",
        std::process::id()
    ));
    fs::rename(path, &target)?;
    Ok(target)
}

fn remove_quarantined(path: &Path) -> io::Result<()> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return Ok(());
    };
    let prefix = format!("{name}{QUARANTINE_MARKER}");
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}
