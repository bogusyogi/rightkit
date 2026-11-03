use rightkit_dictation_telemetry::http_client::{
    parity_client, parity_client_allowing_host, send_text,
};
use rightkit_transcript_library::net::host_of_url;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Host of the compiled-in model worker. Any other worker base is a development
/// override, the only case allowed to resolve to a local or private address.
const DEFAULT_WORKER_HOST: &str = "rightkit-models.adrdsouza.workers.dev";

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

pub fn acquire_model_grant(
    request: ModelGrantRequest<'_>,
    timeout: Duration,
) -> Result<String, String> {
    let endpoint = format!(
        "{}/v1/model-grants",
        request.worker_base.trim_end_matches('/')
    );
    let body = GrantRequestBody {
        route: request.route,
        model: request.model,
        platform: request.platform,
        sha256: request.sha256,
        device_id: request.device_id,
    };
    let body = serde_json::to_string(&body)
        .map_err(|error| format!("could not encode model grant request: {error}"))?;
    let override_host =
        host_of_url(&endpoint).filter(|host| !host.eq_ignore_ascii_case(DEFAULT_WORKER_HOST));
    let client = match override_host.as_deref() {
        Some(host) => parity_client_allowing_host(timeout, host)?,
        None => parity_client(timeout)?,
    };
    let http_request = rightkit_http::Request::post(&endpoint)
        .header("content-type", "application/json")
        .body(body);
    let (_, response_body) = send_text(
        &client,
        &http_request,
        true,
        "model grant request failed",
        "model grant response is malformed",
    )?;
    let response: GrantResponse = serde_json::from_str(&response_body)
        .map_err(|error| format!("model grant response is malformed: {error}"))?;
    if response.token.trim().is_empty() || response.expires_at == 0 {
        return Err("model grant response is incomplete".to_string());
    }
    if response.route != request.route || !response.sha256.eq_ignore_ascii_case(request.sha256) {
        return Err("model grant response scope does not match the requested model".to_string());
    }
    Ok(response.token)
}
