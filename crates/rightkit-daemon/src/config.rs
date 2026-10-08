use std::collections::BTreeMap;
use std::path::PathBuf;

use rightkit_contracts::types::daemon_api::DaemonApi;

/// Upload bounds. Defaults mirror the daemon-api v1 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadLimits {
    pub chunk_min: u64,
    pub chunk_max: u64,
    pub max_object_bytes: u64,
    pub session_ttl_secs: u64,
    /// Concurrent open upload sessions per device.
    pub max_open_per_device: usize,
}

impl Default for UploadLimits {
    fn default() -> Self {
        Self {
            chunk_min: 1_048_576,
            chunk_max: 16_777_216,
            max_object_bytes: 8_589_934_592,
            session_ttl_secs: 86_400,
            max_open_per_device: 16,
        }
    }
}

impl UploadLimits {
    pub fn from_contract(api: &DaemonApi) -> Self {
        Self {
            chunk_min: api.upload.chunk_bytes.min,
            chunk_max: api.upload.chunk_bytes.max,
            max_object_bytes: api.upload.max_object_bytes,
            session_ttl_secs: api.upload.session_ttl_seconds,
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub struct DaemonConfig {
    /// Root of all durable state: `state.json`, `uploads/`, `objects/`, `events.jsonl`.
    pub state_dir: PathBuf,
    pub limits: UploadLimits,
    /// Accepted clock skew for signed device requests, either direction.
    pub replay_window_secs: u64,
    /// Accepted clock skew for webhook timestamps, either direction.
    pub webhook_tolerance_secs: u64,
    pub idempotency_ttl_secs: u64,
    /// Per-provider HMAC secrets. A provider without a secret rejects every delivery.
    pub webhook_secrets: BTreeMap<String, Vec<u8>>,
    /// Where a successful OAuth callback redirects the browser.
    pub oauth_success_redirect: String,
    pub max_json_body: usize,
    pub max_webhook_body: usize,
    /// Bound on retained, not-yet-drained webhook deliveries.
    pub max_webhook_inbox: usize,
}

impl DaemonConfig {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            limits: UploadLimits::default(),
            replay_window_secs: 300,
            webhook_tolerance_secs: 300,
            idempotency_ttl_secs: 86_400,
            webhook_secrets: BTreeMap::new(),
            oauth_success_redirect: "/".into(),
            max_json_body: 64 * 1024,
            max_webhook_body: 1024 * 1024,
            max_webhook_inbox: 10_000,
        }
    }

    pub fn from_contract(api: &DaemonApi, state_dir: impl Into<PathBuf>) -> Self {
        let mut c = Self::new(state_dir);
        c.limits = UploadLimits::from_contract(api);
        c.idempotency_ttl_secs = api.mutations.idempotency.ttl_seconds;
        c
    }
}
