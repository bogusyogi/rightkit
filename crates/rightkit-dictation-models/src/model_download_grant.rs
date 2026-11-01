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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    const SHA256: &str = "5b2f98080f7855750398433a13d57feed51719cd8e8baefac30fc4ece77d7f14";

    fn one_response_server(body: String) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&bytes);
                let header_end = text.find("\r\n\r\n");
                let content_length = text
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_string)
                    })
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if let Some(header_end) = header_end {
                    if bytes.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
            let _ = sender.send(String::from_utf8(bytes).unwrap());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}"), receiver)
    }

    fn request<'a>(worker_base: &'a str) -> ModelGrantRequest<'a> {
        ModelGrantRequest {
            worker_base,
            route: "/parakeet/windows/unified.tar.gz",
            model: "parakeet-unified",
            platform: "windows",
            sha256: SHA256,
            device_id: "device-1234567890",
        }
    }

    #[test]
    fn acquire_posts_the_exact_model_scope_and_returns_the_bearer() {
        let response = serde_json::json!({
            "token": "scoped-token",
            "expiresAt": 2_000_000_600u64,
            "route": "/parakeet/windows/unified.tar.gz",
            "sha256": SHA256,
        })
        .to_string();
        let (worker_base, captured) = one_response_server(response);
        let token = acquire_model_grant(request(&worker_base), Duration::from_secs(10)).unwrap();

        assert_eq!(token, "scoped-token");
        let request = captured.recv().unwrap();
        assert!(request.starts_with("POST /v1/model-grants HTTP/1.1"));
        assert!(request.contains("\"route\":\"/parakeet/windows/unified.tar.gz\""));
        assert!(request.contains("\"model\":\"parakeet-unified\""));
        assert!(request.contains("\"platform\":\"windows\""));
        assert!(request.contains(&format!("\"sha256\":\"{SHA256}\"")));
        assert!(request.contains("\"deviceId\":\"device-1234567890\""));
    }

    #[test]
    fn acquire_rejects_a_response_scoped_to_a_different_route() {
        let response = serde_json::json!({
            "token": "wrong-token",
            "expiresAt": 2_000_000_600u64,
            "route": "/parakeet/windows/tdt.tar.gz",
            "sha256": SHA256,
        })
        .to_string();
        let (worker_base, _) = one_response_server(response);
        let error =
            acquire_model_grant(request(&worker_base), Duration::from_secs(10)).unwrap_err();

        assert!(error.contains("scope"), "{error}");
    }
}
