//! Shared remote-channel gateway policy/status shapes.
//!
//! This transport-neutral layer lets operators inspect channel readiness,
//! default toolset, trust boundary, and delivery policy without knowing each
//! adapter's private status payload.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeliveryTarget {
    pub channel: String,
    pub target: String,
    #[serde(default)]
    pub allow_private_network: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeliveryPayload {
    pub kind: String,
    pub source: String,
    pub delivery_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeliveryRecord {
    pub id: String,
    pub channel: String,
    pub target_redacted: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub payload: RemoteDeliveryPayload,
    pub created_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteDeliveryError {
    #[error("{0}")]
    InvalidTarget(String),
    #[error("{0}")]
    Http(String),
    #[error("{0}")]
    Store(String),
}

#[async_trait]
pub trait RemoteChannelAdapter: Send + Sync {
    async fn deliver(
        &self,
        target: RemoteDeliveryTarget,
        payload: RemoteDeliveryPayload,
    ) -> Result<RemoteDeliveryRecord, RemoteDeliveryError>;
}

#[derive(Debug, Clone)]
pub struct WebhookChannelAdapter {
    timeout: Duration,
}

impl WebhookChannelAdapter {
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn record_delivery(
        &self,
        target: RemoteDeliveryTarget,
        payload: RemoteDeliveryPayload,
        status: &str,
        error: Option<String>,
    ) -> Result<RemoteDeliveryRecord, RemoteDeliveryError> {
        Ok(RemoteDeliveryRecord {
            id: format!("remote-delivery-{}", current_epoch_nanos()),
            channel: target.channel,
            target_redacted: redact_delivery_target(&target.target),
            status: status.to_string(),
            error,
            payload,
            created_at: current_unix_timestamp(),
        })
    }
}

impl Default for WebhookChannelAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RemoteChannelAdapter for WebhookChannelAdapter {
    async fn deliver(
        &self,
        target: RemoteDeliveryTarget,
        payload: RemoteDeliveryPayload,
    ) -> Result<RemoteDeliveryRecord, RemoteDeliveryError> {
        let url = validated_delivery_url(&target.target, target.allow_private_network).await?;
        // Public addresses only (every request and redirect hop is vetted by
        // rightkit-http); an operator-approved private target is the one exception.
        let mut policy = rightkit_http::AddressPolicy::public_web();
        if target.allow_private_network {
            if let Some(host) = url.host_str() {
                policy = policy.allow_if(vec![host.to_string()], |_| true);
            }
        }
        let client = rightkit_http::AsyncClient::new(rightkit_http::ClientConfig {
            request_timeout: self.timeout,
            response_timeout: Some(self.timeout),
            retry: rightkit_http::RetryPolicy::none(),
            address_policy: policy,
            proxy: rightkit_http::ProxyMode::Environment,
            ..rightkit_http::ClientConfig::default()
        })
        .map_err(|e| RemoteDeliveryError::Http(format!("failed to build HTTP client: {e}")))?;
        let request = rightkit_http::Request::post(url.as_str())
            .json(&payload)
            .map_err(|e| RemoteDeliveryError::Http(format!("POST failed: {e}")))?;
        let response = client
            .send_any(&request)
            .await
            .map_err(|e| RemoteDeliveryError::Http(format!("POST failed: {e}")))?;
        if !(200..300).contains(&response.status) {
            return Err(RemoteDeliveryError::Http(format!(
                "POST returned HTTP {}",
                response.status
            )));
        }
        self.record_delivery(target, payload, "delivered", None)
    }
}

pub fn append_remote_delivery(
    path: &Path,
    record: RemoteDeliveryRecord,
) -> Result<Vec<RemoteDeliveryRecord>, RemoteDeliveryError> {
    let mut rows = load_remote_deliveries(path)?;
    rows.push(record);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            RemoteDeliveryError::Store(format!(
                "failed to create remote delivery dir '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let raw = serde_json::to_string_pretty(&rows)
        .map_err(|e| RemoteDeliveryError::Store(format!("failed to encode deliveries: {e}")))?;
    std::fs::write(path, raw).map_err(|e| {
        RemoteDeliveryError::Store(format!(
            "failed to write remote deliveries '{}': {e}",
            path.display()
        ))
    })?;
    Ok(rows)
}

pub fn load_remote_deliveries(
    path: &Path,
) -> Result<Vec<RemoteDeliveryRecord>, RemoteDeliveryError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| {
        RemoteDeliveryError::Store(format!(
            "failed to read remote deliveries '{}': {e}",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        RemoteDeliveryError::Store(format!(
            "failed to parse remote deliveries '{}': {e}",
            path.display()
        ))
    })
}

async fn validated_delivery_url(
    target: &str,
    allow_private_network: bool,
) -> Result<url::Url, RemoteDeliveryError> {
    let raw = target.trim();
    if raw.is_empty() {
        return Err(RemoteDeliveryError::InvalidTarget(
            "target URL is required".to_string(),
        ));
    }
    let url = url::Url::parse(raw)
        .map_err(|e| RemoteDeliveryError::InvalidTarget(format!("invalid target URL: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(RemoteDeliveryError::InvalidTarget(format!(
                "unsupported URL scheme '{other}'"
            )))
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(RemoteDeliveryError::InvalidTarget(
            "target URL must not contain credentials".to_string(),
        ));
    }
    let host = url.host_str().ok_or_else(|| {
        RemoteDeliveryError::InvalidTarget("target URL must include a host".to_string())
    })?;
    if !allow_private_network {
        reject_private_delivery_host(&url, host).await?;
    }
    Ok(url)
}

async fn reject_private_delivery_host(
    url: &url::Url,
    host: &str,
) -> Result<(), RemoteDeliveryError> {
    // Canonical SSRF check lives in rightkit-agent-policy::net_guard (resolve-then-reject,
    // localhost + literal-IP fast paths, IPv6). The gateway layer wraps the generic
    // rejection with the webhook-specific allowPrivateNetwork opt-in hint.
    let port = url.port_or_known_default().ok_or_else(|| {
        RemoteDeliveryError::InvalidTarget(
            "target URL must include a port or known scheme".to_string(),
        )
    })?;
    rightkit_agent_policy::resolve_then_reject_private(host, port)
        .await
        .map_err(|e| {
            RemoteDeliveryError::InvalidTarget(format!(
                "private webhook target requires allowPrivateNetwork opt-in: {e}"
            ))
        })
}

fn redact_delivery_target(target: &str) -> String {
    match url::Url::parse(target) {
        Ok(mut url) => {
            url.set_query(None);
            url.set_fragment(None);
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.to_string()
        }
        Err(_) => "<invalid-url>".to_string(),
    }
}

fn current_epoch_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn current_unix_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGatewayProfile {
    pub id: String,
    pub channels: Vec<String>,
    pub toolset: String,
    pub delivery_policy: String,
}

impl RemoteGatewayProfile {
    pub fn telegram_default() -> Self {
        Self {
            id: "default".to_string(),
            channels: vec!["telegram".to_string()],
            toolset: "telegram-remote".to_string(),
            delivery_policy: "approval-required".to_string(),
        }
    }

    pub fn outbound_webhook() -> Self {
        Self {
            id: "outbound-webhook".to_string(),
            channels: vec!["webhook".to_string(), "ntfy".to_string()],
            toolset: "outbound-notification".to_string(),
            delivery_policy: "target-validated".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteChannelProfile {
    pub profile_id: String,
    pub toolset: String,
    pub delivery_policy: String,
    pub trust: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteChannelStatus {
    pub state: String,
    pub configured: bool,
    pub binding_count: usize,
    pub active_target_count: usize,
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteChannelDeliverySummary {
    pub id: String,
    pub binding_id: Option<String>,
    pub target_redacted: String,
    pub status: String,
    pub delivery_kind: String,
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteChannelDescriptor {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    pub configured: bool,
    pub profile: RemoteChannelProfile,
    pub status: RemoteChannelStatus,
    pub recent_deliveries: Vec<RemoteChannelDeliverySummary>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteChannelsSnapshot {
    pub profiles: Vec<RemoteGatewayProfile>,
    pub channels: Vec<RemoteChannelDescriptor>,
}

pub struct TelegramChannelDescriptorInput {
    pub configured: bool,
    pub allowed_user_count: usize,
    pub long_poll_requested: bool,
    pub binding_count: usize,
    pub active_target_count: usize,
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
}

pub fn telegram_channel_descriptor(
    input: TelegramChannelDescriptorInput,
) -> RemoteChannelDescriptor {
    let profile = RemoteGatewayProfile::telegram_default();
    let state = if !input.configured {
        "notConfigured"
    } else if input.allowed_user_count == 0 {
        "blocked"
    } else if input.last_error.is_some() {
        "degraded"
    } else if input.last_success_at.is_some() {
        "ready"
    } else if input.long_poll_requested {
        "starting"
    } else {
        "configured"
    };

    RemoteChannelDescriptor {
        id: "telegram".to_string(),
        kind: "chat".to_string(),
        enabled: input.configured,
        configured: input.configured,
        profile: RemoteChannelProfile {
            profile_id: profile.id,
            toolset: profile.toolset,
            delivery_policy: profile.delivery_policy,
            trust: "remoteAgentUntrusted".to_string(),
        },
        status: RemoteChannelStatus {
            state: state.to_string(),
            configured: input.configured,
            binding_count: input.binding_count,
            active_target_count: input.active_target_count,
            last_success_at: input.last_success_at,
            last_error_at: input.last_error_at,
            last_error: input.last_error,
        },
        recent_deliveries: Vec::new(),
        notes: vec![
            "Secrets, raw chat ids, and message bodies are intentionally omitted.".to_string(),
            "Telegram messages enter the host as remote untrusted content.".to_string(),
        ],
    }
}

pub fn webhook_channel_descriptor(channel: &str) -> RemoteChannelDescriptor {
    let profile = RemoteGatewayProfile::outbound_webhook();
    RemoteChannelDescriptor {
        id: channel.to_string(),
        kind: "webhook".to_string(),
        enabled: true,
        configured: true,
        profile: RemoteChannelProfile {
            profile_id: profile.id,
            toolset: profile.toolset,
            delivery_policy: profile.delivery_policy,
            trust: "remoteAgentUntrusted".to_string(),
        },
        status: RemoteChannelStatus {
            state: "ready".to_string(),
            configured: true,
            binding_count: 0,
            active_target_count: 0,
            last_success_at: None,
            last_error_at: None,
            last_error: None,
        },
        recent_deliveries: Vec::new(),
        notes: vec![
            "Outbound targets are validated per delivery.".to_string(),
            "Loopback/internal/private targets require explicit allowPrivateNetwork opt-in."
                .to_string(),
            "Query strings, credentials, and fragments are redacted from delivery records."
                .to_string(),
        ],
    }
}
