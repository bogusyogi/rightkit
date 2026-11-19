use rightkit_dictation_telemetry::http_client::{
    parity_client, parity_client_allowing_host, send_text,
};
use rightkit_transcript_library::net::host_of_url;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// True for a plain `https://` origin: a host, no credentials, no query or fragment, no
/// whitespace or control character. The grant request carries the device identifier and the
/// response carries a bearer token, so neither may travel over a cleartext connection or to
/// an address that names credentials.
fn is_plain_https_base(base: &str) -> bool {
    let base = base.trim();
    let Some(rest) = base
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .and_then(|_| base.get(8..))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && !base.contains(['?', '#'])
        && !base.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Longest grant token accepted.
const MAX_GRANT_TOKEN_BYTES: usize = 8 * 1024;

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

/// Ask the model worker at `request.worker_base` for a download grant and return its token.
///
/// Network policy: the worker base must be a plain `https://` origin and the connection uses the
/// public-web address policy, so a worker that resolves to a loopback, private or link-local
/// address is refused, whatever host the base names. The only exception is a build with the
/// non-default `dev-worker-override` feature, where the one host named by
/// `RIGHTKIT_MODEL_WORKER_BASE` may be reached at a local address (and over `http://`); no
/// release build has that feature.
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
    let host =
        host_of_url(&endpoint).ok_or_else(|| "model worker address is invalid".to_string())?;
    // The development host named for this build, if any; never derived from the request.
    let dev_host = crate::resumable_download::dev_worker_host().filter(|dev| *dev == host);
    if dev_host.is_none() && !is_plain_https_base(request.worker_base) {
        return Err("model worker address must be an https origin".to_string());
    }
    let client = match dev_host.as_deref() {
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
    // The token becomes an `Authorization` header value: bounded, and no control character.
    if response.token.len() > MAX_GRANT_TOKEN_BYTES || response.token.chars().any(char::is_control)
    {
        return Err("model grant response is malformed".to_string());
    }
    Ok(response.token)
}
