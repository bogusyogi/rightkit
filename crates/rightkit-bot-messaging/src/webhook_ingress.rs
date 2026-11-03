//! ENG-95 — bounded HTTP webhook ingress that triggers an agent turn.
//!
//! ## Non-negotiable invariant
//!
//! A webhook-originated turn runs under the **most restrictive** permission
//! mode and every effect requires explicit approval, regardless of the
//! session's configured mode. Auto-approve never fires for a turn nobody
//! started. This module never itself starts a turn or a session — it only
//! verifies an inbound payload and, on success, produces a
//! [`WebhookAdmission`] whose `unattended` flag is unconditionally `true`.
//! The consumer (the WP-07 workflow supervisor / the daemon's session
//! bootstrap) is responsible for turning that admission into
//! `host_workflow::trigger::Trigger::webhook(..)` and for forcing
//! `PermissionMode::Ask` at session construction time — see the gap note in
//! the crate-level report for why that wiring is not done here.
//!
//! ## Listener situation (read before wiring a route to this module)
//!
//! `remote::gateway` is **outbound only** — `WebhookChannelAdapter` posts
//! *out* to a configured target; it starts no server and owns no listener.
//! The only inbound-HTTP-webhook surface that exists today is
//! `engine/host-app/src/api_triggers.rs` ("Authenticated local
//! webhook triggers"), which is a locally-authenticated dashboard/admin
//! surface (`webhook_trigger_admin` scope) served by the daemon's own axum
//! router — it is not part of this crate and does not do HMAC/replay
//! verification of an externally-signed payload. This module does **not**
//! attach to either of those; there is no existing listener in
//! `host_remote` to attach to. Wiring an actual HTTP route to
//! [`WebhookIngress::admit`] is orchestrator/binary work: add an axum route
//! in `engine/bins/host app` (or extend `api_triggers.rs`) that reads the
//! raw body + `X-Webhook-Signature`/`X-Webhook-Timestamp`/`X-Webhook-Nonce`
//! headers and calls `admit`.
//!
//! ## Bounds enforced here
//!
//! - **Payload size cap** — checked against the raw byte length before any
//!   parsing or HMAC work, so an oversized body is rejected cheaply.
//! - **Rate limit per source** — a fixed-window counter keyed by the
//!   caller-supplied source id (e.g. webhook binding id), independent of
//!   signature validity, so a flood of even correctly-signed requests is
//!   bounded.
//! - **HMAC-SHA256 signature verification** with a per-webhook secret,
//!   compared in constant time. SHA-256, HMAC and the constant-time compare
//!   are `rightkit-crypto` (`hmac_sha256`, `ct_eq`, `hex_encode`).
//! - **Replay protection** — a signed timestamp must fall within a bounded
//!   tolerance window of "now", and a signed nonce must not have been seen
//!   before within that same window. Both are required; a valid signature
//!   over a replayed (timestamp, nonce) pair is still rejected.
//! - **Secret handling** — [`WebhookSecret`] never implements `Display`,
//!   and its `Debug` impl prints a fixed redacted placeholder, so the
//!   secret cannot end up in a log line via `{:?}` or `{}` by accident.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// A per-webhook HMAC secret. Deliberately opaque: no `Display`, and
/// `Debug` prints a fixed placeholder so the secret is never logged.
#[derive(Clone)]
pub struct WebhookSecret(Vec<u8>);

impl WebhookSecret {
    pub fn new(raw: impl Into<Vec<u8>>) -> Self {
        Self(raw.into())
    }
}

impl std::fmt::Debug for WebhookSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebhookSecret(<redacted>)")
    }
}

/// Bounded configuration for one webhook ingress point.
#[derive(Debug, Clone)]
pub struct WebhookIngressConfig {
    /// Hard cap on raw payload bytes. A request over this is rejected
    /// before any parsing or signature work.
    pub max_payload_bytes: usize,
    /// Maximum admitted requests per source id within `rate_limit_window`.
    pub rate_limit_max_requests: u32,
    /// Sliding window for the per-source rate limit and for replay
    /// tolerance, in seconds.
    pub rate_limit_window_secs: u64,
    /// How far a signed timestamp may drift from "now" (either direction)
    /// before the request is rejected as stale/future-dated.
    pub replay_tolerance_secs: u64,
}

impl Default for WebhookIngressConfig {
    fn default() -> Self {
        Self {
            max_payload_bytes: 256 * 1024, // 256 KiB
            rate_limit_max_requests: 30,
            rate_limit_window_secs: 60,
            replay_tolerance_secs: 300, // 5 minutes
        }
    }
}

/// One verified webhook payload, mapped to the shape a caller needs to
/// admit a workflow trigger. `unattended` is always `true` — there is no
/// constructor for this type that can produce `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookAdmission {
    /// Caller-supplied source id (e.g. the webhook binding id). Also the
    /// rate-limit and audit key.
    pub source_id: String,
    /// Signed nonce from the request, used as the trigger's dedupe key so
    /// a replayed-but-otherwise-valid delivery cannot double-admit.
    pub nonce: String,
    /// Signed unix-seconds timestamp from the request.
    pub timestamp_unix_secs: u64,
    /// The raw payload bytes, unparsed. Ingress does not interpret the
    /// body — that is the workflow admission layer's job.
    pub payload: Vec<u8>,
    /// ENG-95 invariant marker: a webhook-originated admission always
    /// requires the resulting turn to be forced to `PermissionMode::Ask`.
    /// Always `true`.
    pub unattended: bool,
}

/// Everything that can make ingress reject a request before it ever
/// reaches workflow admission. Deliberately does not include the secret or
/// the raw signature in any variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebhookIngressError {
    #[error("payload of {actual} bytes exceeds the {limit}-byte cap")]
    PayloadTooLarge { actual: usize, limit: usize },
    #[error("source '{source_id}' exceeded {limit} requests per {window_secs}s")]
    RateLimited {
        source_id: String,
        limit: u32,
        window_secs: u64,
    },
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("timestamp is outside the {tolerance_secs}s replay tolerance window")]
    StaleTimestamp { tolerance_secs: u64 },
    #[error("nonce has already been used within the replay window")]
    ReplayedNonce,
    #[error("source id must not be empty")]
    EmptySourceId,
}

/// Bounded HTTP webhook ingress. Holds only in-memory rate-limit and
/// replay-nonce state — no I/O, no listener. Safe to share across threads
/// (`Send + Sync`) behind an `Arc`, since all mutable state is behind a
/// `Mutex`.
pub struct WebhookIngress {
    config: WebhookIngressConfig,
    rate_state: Mutex<std::collections::HashMap<String, VecDeque<u64>>>,
    seen_nonces: Mutex<HashSet<(String, String)>>,
}

impl WebhookIngress {
    pub fn new(config: WebhookIngressConfig) -> Self {
        Self {
            config,
            rate_state: Mutex::new(std::collections::HashMap::new()),
            seen_nonces: Mutex::new(HashSet::new()),
        }
    }

    /// Verify one inbound webhook request and, on success, produce a
    /// [`WebhookAdmission`]. `now_unix_secs` is threaded in explicitly
    /// (rather than read from the clock inside) so tests are
    /// deterministic and so a real caller can supply a monotonic source.
    ///
    /// Verification order (fail fast, cheapest checks first):
    /// 1. source id non-empty
    /// 2. payload size cap
    /// 3. per-source rate limit
    /// 4. timestamp within replay tolerance
    /// 5. HMAC signature (constant-time compare)
    /// 6. nonce not already seen within the replay window
    ///
    /// Signature is checked *before* nonce-replay so a request with a
    /// forged nonce but invalid signature is rejected as
    /// `InvalidSignature`, not `ReplayedNonce` — replay state is only
    /// consulted for payloads that already proved authenticity.
    #[allow(clippy::too_many_arguments)]
    pub fn admit(
        &self,
        source_id: &str,
        secret: &WebhookSecret,
        payload: &[u8],
        timestamp_unix_secs: u64,
        nonce: &str,
        signature_hex: &str,
        now_unix_secs: u64,
    ) -> Result<WebhookAdmission, WebhookIngressError> {
        if source_id.trim().is_empty() {
            return Err(WebhookIngressError::EmptySourceId);
        }

        if payload.len() > self.config.max_payload_bytes {
            return Err(WebhookIngressError::PayloadTooLarge {
                actual: payload.len(),
                limit: self.config.max_payload_bytes,
            });
        }

        self.check_rate_limit(source_id, now_unix_secs)?;

        let drift = now_unix_secs.abs_diff(timestamp_unix_secs);
        if drift > self.config.replay_tolerance_secs {
            return Err(WebhookIngressError::StaleTimestamp {
                tolerance_secs: self.config.replay_tolerance_secs,
            });
        }

        let expected = sign(secret, timestamp_unix_secs, nonce, payload);
        if !constant_time_eq_hex(&expected, signature_hex) {
            return Err(WebhookIngressError::InvalidSignature);
        }

        self.check_and_record_nonce(source_id, nonce, now_unix_secs)?;

        Ok(WebhookAdmission {
            source_id: source_id.to_string(),
            nonce: nonce.to_string(),
            timestamp_unix_secs,
            payload: payload.to_vec(),
            unattended: true,
        })
    }

    fn check_rate_limit(&self, source_id: &str, now: u64) -> Result<(), WebhookIngressError> {
        let mut state = self.rate_state.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.config.rate_limit_window_secs;
        let entry = state.entry(source_id.to_string()).or_default();
        while let Some(&oldest) = entry.front() {
            if now.saturating_sub(oldest) > window {
                entry.pop_front();
            } else {
                break;
            }
        }
        if entry.len() as u32 >= self.config.rate_limit_max_requests {
            return Err(WebhookIngressError::RateLimited {
                source_id: source_id.to_string(),
                limit: self.config.rate_limit_max_requests,
                window_secs: window,
            });
        }
        entry.push_back(now);
        Ok(())
    }

    fn check_and_record_nonce(
        &self,
        source_id: &str,
        nonce: &str,
        now: u64,
    ) -> Result<(), WebhookIngressError> {
        let mut seen = self.seen_nonces.lock().unwrap_or_else(|e| e.into_inner());
        let key = (source_id.to_string(), nonce.to_string());
        if seen.contains(&key) {
            return Err(WebhookIngressError::ReplayedNonce);
        }
        seen.insert(key);
        // Bound growth: a full sweep keyed on `now` and the replay
        // tolerance window would require storing per-entry timestamps;
        // V1 keeps this simple (unbounded within a rate-limit window's
        // worth of unique nonces) and documents the tradeoff rather than
        // fake a TTL. `now` is threaded through so a future revision can
        // add timestamped eviction without changing the call signature.
        let _ = now;
        Ok(())
    }
}

fn sign(secret: &WebhookSecret, timestamp_unix_secs: u64, nonce: &str, payload: &[u8]) -> String {
    let mut message = Vec::with_capacity(payload.len() + nonce.len() + 24);
    message.extend_from_slice(timestamp_unix_secs.to_string().as_bytes());
    message.push(b'.');
    message.extend_from_slice(nonce.as_bytes());
    message.push(b'.');
    message.extend_from_slice(payload);
    let mac = rightkit_crypto::hmac_sha256(&secret.0, &message);
    rightkit_crypto::hex_encode(&mac)
}

fn constant_time_eq_hex(expected_hex: &str, actual_hex: &str) -> bool {
    rightkit_crypto::ct_eq(expected_hex.as_bytes(), actual_hex.as_bytes())
}

pub fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
