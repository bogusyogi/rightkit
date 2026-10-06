use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ModelGrantRequest<'a> {
    pub worker_base: &'a str,
    pub route: &'a str,
    pub model: &'a str,
    pub platform: &'a str,
    pub sha256: &'a str,
    pub device_id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantRequestBody<'a> {
    route: &'a str,
    model: &'a str,
    platform: &'a str,
    sha256: &'a str,
    device_id: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantResponse {
    token: String,
    expires_at: u64,
    route: String,
    sha256: String,
}

/// A worker-issued, route-and-hash-scoped download token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelGrant {
    pub token: String,
    /// Unix seconds.
    pub expires_at: u64,
}

impl ModelGrant {
    /// `Authorization` header value.
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// True when the token is expired or expires within `skew` seconds.
    pub fn expires_within(&self, skew: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);
        self.expires_at <= now.saturating_add(skew)
    }
}

pub fn acquire_model_grant(
    agent: &ureq::Agent,
    request: ModelGrantRequest<'_>,
) -> Result<String, String> {
    acquire_grant(agent, request).map(|grant| grant.token)
}

/// As [`acquire_model_grant`], keeping the expiry so callers can refresh.
pub fn acquire_grant(
    agent: &ureq::Agent,
    request: ModelGrantRequest<'_>,
) -> Result<ModelGrant, String> {
    let endpoint = format!("{}/v1/model-grants", request.worker_base.trim_end_matches('/'));
    let body = serde_json::to_string(&GrantRequestBody {
        route: request.route,
        model: request.model,
        platform: request.platform,
        sha256: request.sha256,
        device_id: request.device_id,
    })
    .map_err(|error| format!("could not encode model grant request: {error}"))?;
    let mut response = agent
        .post(&endpoint)
        .header("content-type", "application/json")
        .send(body)
        .map_err(|error| format!("model grant request failed: {error}"))?;
    let response_body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("model grant response is malformed: {error}"))?;
    let response: GrantResponse = serde_json::from_str(&response_body)
        .map_err(|error| format!("model grant response is malformed: {error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is invalid: {error}"))?
        .as_secs();
    if response.token.trim().is_empty() || response.expires_at <= now {
        return Err("model grant response is incomplete".to_string());
    }
    if response.route != request.route || !response.sha256.eq_ignore_ascii_case(request.sha256) {
        return Err("model grant response scope does not match the requested model".to_string());
    }
    Ok(ModelGrant {
        token: response.token,
        expires_at: response.expires_at,
    })
}
