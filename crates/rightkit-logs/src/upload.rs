//! Consent-gated batch uploader for the outbox.
//!
//! Ported from HeardRight `telemetry_upload` (batch caps, exponential backoff,
//! `Retry-After`, permanent-reject handling) with every HeardRight-specific
//! event shape and the hard-coded ingest URL removed: the app supplies the
//! endpoint, and RightKit refuses non-HTTPS endpoints except loopback.

use crate::{Consent, ConsentStore, Outbox};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub const PAYLOAD_SCHEMA_VERSION: u32 = 1;
const BASE_BACKOFF_SECS: u64 = 60;
const MAX_BACKOFF_SECS: u64 = 60 * 60;
const MAX_BACKOFF_EXPONENT: u32 = 6;

#[derive(Debug, Clone)]
pub struct UploadConfig {
    /// Where batches are POSTed. Must be `https://`, or `http://` to a
    /// loopback host (`localhost`, `127.0.0.1`, `[::1]`) for local test servers.
    pub endpoint: String,
    pub app_name: String,
    pub app_version: String,
    /// Required consent wording version; an older grant does not authorize upload.
    pub policy_version: Option<String>,
    pub max_batch_events: usize,
    pub max_batch_bytes: usize,
    /// Send the pseudonymous install id. Off by default.
    pub install_id: Option<String>,
    pub base_backoff_secs: u64,
}

impl UploadConfig {
    pub fn new(
        endpoint: impl Into<String>,
        app_name: impl Into<String>,
        app_version: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            app_name: app_name.into(),
            app_version: app_version.into(),
            policy_version: None,
            max_batch_events: 20,
            max_batch_bytes: 60 * 1024,
            install_id: None,
            base_backoff_secs: BASE_BACKOFF_SECS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpOutcome {
    pub status: u16,
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

/// Pluggable wire. The default is `HttpTransport`; apps that already own an
/// HTTP stack (or want OTLP) implement this and keep consent/redaction/outbox.
pub trait Transport: Send + Sync {
    fn post_json(&self, url: &str, body: &[u8]) -> Result<HttpOutcome, TransportError>;
}

#[cfg(feature = "http")]
pub struct HttpTransport {
    agent: ureq::Agent,
    user_agent: String,
}

#[cfg(feature = "http")]
impl HttpTransport {
    pub fn new(user_agent: impl Into<String>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into();
        Self {
            agent,
            user_agent: user_agent.into(),
        }
    }
}

#[cfg(feature = "http")]
impl Transport for HttpTransport {
    fn post_json(&self, url: &str, body: &[u8]) -> Result<HttpOutcome, TransportError> {
        let response = self
            .agent
            .post(url)
            .header("content-type", "application/json")
            .header("user-agent", &self.user_agent)
            .send(body)
            .map_err(|error| TransportError(error.to_string()))?;
        let retry_after_secs = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(|secs| secs.min(MAX_BACKOFF_SECS));
        Ok(HttpOutcome {
            status: response.status().as_u16(),
            retry_after_secs,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushState {
    /// Consent absent or revoked. Nothing was read from the queue or sent.
    NoConsent,
    /// Queue empty.
    Idle,
    /// Everything ready was sent and acknowledged.
    Drained,
    /// Events exist but are waiting out a backoff.
    Backoff { wait_ms: i64 },
    /// A send failed; events kept and rescheduled.
    Retrying { status: Option<u16>, wait_secs: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushReport {
    pub state: FlushState,
    pub sent: usize,
    /// Events the server permanently rejected (4xx other than 401/403/404/408/429); dropped.
    pub rejected: usize,
    pub remaining: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EndpointError {
    Empty,
    NotHttps(String),
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointError::Empty => write!(f, "upload endpoint is empty"),
            EndpointError::NotHttps(url) => {
                write!(f, "upload endpoint must be https (or loopback http): {url}")
            }
        }
    }
}

impl std::error::Error for EndpointError {}

pub fn validate_endpoint(endpoint: &str) -> Result<(), EndpointError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(EndpointError::Empty);
    }
    if endpoint.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = endpoint.strip_prefix("http://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = if let Some(stripped) = authority.strip_prefix('[') {
            stripped.split(']').next().unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        if matches!(host, "localhost" | "127.0.0.1" | "::1") {
            return Ok(());
        }
    }
    Err(EndpointError::NotHttps(endpoint.to_string()))
}

pub struct Uploader {
    outbox: Arc<Outbox>,
    transport: Box<dyn Transport>,
    config: UploadConfig,
    session_id: String,
    consecutive_failures: AtomicU64,
}

impl Uploader {
    pub fn new(
        outbox: Arc<Outbox>,
        transport: Box<dyn Transport>,
        config: UploadConfig,
    ) -> Result<Self, EndpointError> {
        validate_endpoint(&config.endpoint)?;
        Ok(Self {
            outbox,
            transport,
            config,
            session_id: crate::identity::random_uuid(),
            consecutive_failures: AtomicU64::new(0),
        })
    }

    fn consented(&self) -> bool {
        match self.outbox.consent().status() {
            Consent::Granted { policy_version, .. } => self
                .config
                .policy_version
                .as_deref()
                .is_none_or(|required| required == policy_version),
            _ => false,
        }
    }

    /// Send everything that is ready, respecting persisted backoff.
    pub fn flush(&self) -> FlushReport {
        self.flush_inner(false)
    }

    /// Like `flush`, ignoring per-event backoff (user pressed "send now").
    pub fn flush_now(&self) -> FlushReport {
        self.flush_inner(true)
    }

    fn flush_inner(&self, ignore_backoff: bool) -> FlushReport {
        let mut sent = 0;
        let mut rejected = 0;
        loop {
            if !self.consented() {
                return self.report(FlushState::NoConsent, sent, rejected);
            }
            let now = crate::now_ms();
            let horizon = if ignore_backoff { i64::MAX } else { now };
            let batch = self.next_batch(horizon);
            if batch.is_empty() {
                let state = match self.outbox.next_ready_at_ms() {
                    None => {
                        if sent + rejected == 0 {
                            FlushState::Idle
                        } else {
                            FlushState::Drained
                        }
                    }
                    Some(at) => FlushState::Backoff {
                        wait_ms: (at - now).max(0),
                    },
                };
                return self.report(state, sent, rejected);
            }
            let ids: Vec<u64> = batch.iter().map(|e| e.id).collect();
            let body = self.build_body(&batch);
            match self.transport.post_json(&self.config.endpoint, &body) {
                Ok(outcome) if (200..300).contains(&outcome.status) => {
                    self.consecutive_failures.store(0, Ordering::Release);
                    if self.outbox.acknowledge(&ids).is_err() {
                        return self.report(
                            FlushState::Retrying {
                                status: Some(outcome.status),
                                wait_secs: 0,
                            },
                            sent,
                            rejected,
                        );
                    }
                    sent += ids.len();
                }
                Ok(outcome) if is_permanent_rejection(outcome.status) => {
                    // The server will never accept these bytes; do not retry forever.
                    let _ = self.outbox.acknowledge(&ids);
                    rejected += ids.len();
                }
                Ok(outcome) => {
                    return self.fail(
                        &ids,
                        Some(outcome.status),
                        outcome.retry_after_secs,
                        sent,
                        rejected,
                    );
                }
                Err(_) => return self.fail(&ids, None, None, sent, rejected),
            }
        }
    }

    fn next_batch(&self, horizon_ms: i64) -> Vec<crate::OutboxEvent> {
        let candidates = self
            .outbox
            .pending(self.config.max_batch_events, horizon_ms);
        let mut batch = Vec::new();
        let mut bytes = 0;
        for event in candidates {
            let size = serde_json::to_vec(&event.payload)
                .map(|b| b.len())
                .unwrap_or(0)
                + 128;
            if !batch.is_empty() && bytes + size > self.config.max_batch_bytes {
                break;
            }
            bytes += size;
            batch.push(event);
        }
        batch
    }

    fn build_body(&self, batch: &[crate::OutboxEvent]) -> Vec<u8> {
        let mut body = json!({
            "schema_version": PAYLOAD_SCHEMA_VERSION,
            "app": { "name": self.config.app_name, "version": self.config.app_version },
            "session_id": self.session_id,
            "sent_at_ms": crate::now_ms(),
            "events": batch.iter().map(|event| json!({
                "type": event.event_type,
                "created_at_ms": event.created_at_ms,
                "payload": event.payload,
            })).collect::<Vec<Value>>(),
        });
        if let Some(install_id) = &self.config.install_id {
            body["install_id"] = json!(install_id);
        }
        serde_json::to_vec(&body).unwrap_or_default()
    }

    fn fail(
        &self,
        ids: &[u64],
        status: Option<u16>,
        retry_after: Option<u64>,
        sent: usize,
        rejected: usize,
    ) -> FlushReport {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
        let wait = backoff_secs(self.config.base_backoff_secs, failures, retry_after);
        let _ = self
            .outbox
            .record_failure(ids, crate::now_ms() + (wait as i64) * 1000);
        self.report(
            FlushState::Retrying {
                status,
                wait_secs: wait,
            },
            sent,
            rejected,
        )
    }

    fn report(&self, state: FlushState, sent: usize, rejected: usize) -> FlushReport {
        FlushReport {
            state,
            sent,
            rejected,
            remaining: self.outbox.len(),
        }
    }

    /// Delay until the scheduler should look again.
    pub fn next_wake(&self, idle: Duration) -> Duration {
        match self.outbox.next_ready_at_ms() {
            Some(at) => {
                let wait = (at - crate::now_ms()).max(0) as u64;
                Duration::from_millis(wait).max(Duration::from_millis(50))
            }
            None => idle,
        }
    }
}

fn is_permanent_rejection(status: u16) -> bool {
    (400..500).contains(&status) && !matches!(status, 401 | 403 | 404 | 408 | 429)
}

/// `base * 2^(failures-1)`, capped, never below the server's `Retry-After`.
pub fn backoff_secs(base: u64, failures: u64, retry_after: Option<u64>) -> u64 {
    let exponent = (failures.saturating_sub(1) as u32).min(MAX_BACKOFF_EXPONENT);
    let exponential = base.saturating_mul(1u64 << exponent).min(MAX_BACKOFF_SECS);
    exponential.max(retry_after.unwrap_or(0).min(MAX_BACKOFF_SECS))
}

/// Background flush loop. Consent is re-checked on every pass, so a revoked
/// user stops uploading without restarting the app.
pub struct Scheduler {
    stop: Sender<()>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Scheduler {
    pub fn spawn(uploader: Arc<Uploader>, idle_interval: Duration) -> std::io::Result<Self> {
        let (stop, signal) = mpsc::channel::<()>();
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let handle = std::thread::Builder::new()
            .name("rightkit-telemetry-upload".into())
            .spawn(move || {
                let mut wait = Duration::from_millis(0);
                loop {
                    match signal.recv_timeout(wait) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                    let report = uploader.flush();
                    wait = match report.state {
                        FlushState::Retrying { wait_secs, .. } => {
                            Duration::from_secs(wait_secs.max(1))
                        }
                        _ => uploader.next_wake(idle_interval),
                    };
                }
                flag.store(false, Ordering::Release);
            })?;
        Ok(Self {
            stop,
            running,
            handle: Some(handle),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Convenience bundle most apps want: consent + outbox + uploader.
pub struct Telemetry {
    pub consent: ConsentStore,
    pub outbox: Arc<Outbox>,
    pub uploader: Arc<Uploader>,
}

impl Telemetry {
    pub fn open(
        dir: &std::path::Path,
        config: UploadConfig,
        transport: Box<dyn Transport>,
        outbox_config: crate::OutboxConfig,
    ) -> Result<Self, EndpointError> {
        let consent = ConsentStore::new(dir.join("telemetry-consent.json"));
        let outbox = Arc::new(Outbox::open(
            dir.join("telemetry-outbox.json"),
            consent.clone(),
            outbox_config,
        ));
        let uploader = Arc::new(Uploader::new(outbox.clone(), transport, config)?);
        Ok(Self {
            consent,
            outbox,
            uploader,
        })
    }

    /// Record the user's decision to stop: consent is revoked first, then every
    /// queued event is deleted. If purge fails the revoke still holds.
    pub fn revoke(&self) -> std::io::Result<()> {
        self.consent.revoke()?;
        self.outbox.purge()
    }
}
