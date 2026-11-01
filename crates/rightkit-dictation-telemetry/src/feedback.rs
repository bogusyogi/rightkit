//! User-initiated feedback from the About page. The explicit Send click is the
//! consent; nothing is queued and nothing is stored. The server requires an id on every
//! submission, so each send carries a fresh random one (`new_submission_id`): it is never
//! persisted, so no identifier survives the send or links two sends.

use chrono::{SecondsFormat, Utc};
use serde_json::json;
use std::time::Duration;

/// Where feedback and diagnostics uploads go, and the prefix of the metadata headers the
/// server reads (for example `x-myapp-`). Configured once by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedbackEndpoint {
    /// Absolute URL of the feedback endpoint.
    pub url: &'static str,
    /// Lower-case metadata header prefix, ending in `-`.
    pub header_prefix: &'static str,
}

static ENDPOINT: std::sync::OnceLock<FeedbackEndpoint> = std::sync::OnceLock::new();

/// Configure the feedback endpoint once at startup. Returns false when one was already set.
pub fn configure_feedback_endpoint(endpoint: FeedbackEndpoint) -> bool {
    ENDPOINT.set(endpoint).is_ok()
}

fn endpoint() -> Result<&'static FeedbackEndpoint, String> {
    ENDPOINT
        .get()
        .ok_or_else(|| "feedback endpoint not configured".to_string())
}

/// Header prefix of the configured endpoint (`x-app-` when none is configured).
fn header_prefix() -> &'static str {
    ENDPOINT
        .get()
        .map_or("x-app-", |endpoint| endpoint.header_prefix)
}

fn header_name(suffix: &str) -> String {
    format!("{}{suffix}", header_prefix())
}

pub(crate) const MAX_FEEDBACK_MESSAGE_CHARS: usize = 4000;
pub(crate) const MAX_FEEDBACK_CONTACT_CHARS: usize = 200;

/// A fresh random UUID v4 for one submission. The same id goes on the feedback and on the
/// diagnostics upload attached to it. Never stored.
pub fn new_submission_id() -> String {
    rightkit_logs::identity::random_uuid()
}

/// The metadata headers of one diagnostics upload. The server requires the `install-id` header
/// to be a UUID v4: the id of the feedback submission this upload is attached to, or a fresh
/// random one for a standalone upload. Never stored, so no identifier survives the send.
pub fn diagnostics_upload_headers(
    app_version: &str,
    feedback_id: Option<&str>,
    submission_id: Option<&str>,
) -> Vec<(String, String)> {
    let install_id = submission_id
        .map(str::to_owned)
        .unwrap_or_else(new_submission_id);
    let mut headers = vec![
        ("content-type".to_string(), "application/zip".to_string()),
        (
            header_name("occurred-at"),
            Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        ),
        (header_name("app-version"), app_version.to_string()),
        (header_name("platform"), std::env::consts::OS.to_string()),
        (header_name("install-id"), install_id),
    ];
    if let Some(feedback_id) = feedback_id {
        headers.push((header_name("feedback-id"), feedback_id.to_string()));
    }
    headers
}

/// The JSON body of one feedback submission.
pub(crate) fn feedback_payload(
    message: &str,
    contact: Option<&str>,
    submission_id: &str,
) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "install_id": submission_id,
        "occurred_at": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "app_version": crate::host_app_version(),
        "platform": std::env::consts::OS,
        "message": message,
        "contact": contact,
    })
}

/// One blocking POST; the Tauri command runs it via spawn_blocking. `submission_id` is this
/// send's own id from `new_submission_id`.
pub fn submit_feedback(
    message: &str,
    contact: Option<&str>,
    submission_id: &str,
) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("feedback message is empty".to_string());
    }
    if message.chars().count() > MAX_FEEDBACK_MESSAGE_CHARS {
        return Err(format!(
            "feedback message exceeds {MAX_FEEDBACK_MESSAGE_CHARS} chars"
        ));
    }
    let contact = contact.map(str::trim).filter(|value| !value.is_empty());
    if let Some(value) = contact {
        if value.chars().count() > MAX_FEEDBACK_CONTACT_CHARS {
            return Err(format!(
                "contact exceeds {MAX_FEEDBACK_CONTACT_CHARS} chars"
            ));
        }
    }
    let payload = feedback_payload(message, contact, submission_id);
    let body =
        serde_json::to_string(&payload).map_err(|error| format!("encode feedback: {error}"))?;
    let client = crate::http_client::parity_client(Duration::from_secs(10))?;
    let endpoint = endpoint()?;
    let request = rightkit_http::Request::post(endpoint.url)
        .header("content-type", "application/json")
        .body(body);
    let response = crate::http_client::send_head(&client, &request, true, "feedback send failed")?;
    if response.status != 202 {
        return Err(format!(
            "feedback endpoint returned {}",
            crate::http_client::status_display(response.status)
        ));
    }
    response
        .header(&header_name("report-id"))
        .map(str::to_owned)
        .ok_or_else(|| "feedback endpoint omitted report ID".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's `uuidV4` rule .
    fn is_server_uuid_v4(value: &str) -> bool {
        let parts: Vec<&str> = value.split('-').collect();
        let lengths = [8, 4, 4, 4, 12];
        parts.len() == 5
            && parts.iter().zip(lengths).all(|(part, len)| {
                part.len() == len && part.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
            })
            && parts[2].starts_with('4')
            && matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b'))
    }

    #[test]
    fn submission_ids_are_server_valid_uuid_v4_and_differ_per_submission() {
        let first = new_submission_id();
        let second = new_submission_id();
        assert!(is_server_uuid_v4(&first), "{first}");
        assert!(is_server_uuid_v4(&second), "{second}");
        assert_ne!(first, second);
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn upload_headers_carry_a_server_valid_install_id_and_platform() {
        let headers = diagnostics_upload_headers("0.1.230", None, None);
        let id = header(&headers, &header_name("install-id")).expect("install id header");
        assert!(is_server_uuid_v4(id), "{id}");
        assert!(["windows", "macos", "linux"]
            .contains(&header(&headers, &header_name("platform")).unwrap()));
        assert_eq!(header(&headers, "content-type"), Some("application/zip"));
        assert_eq!(
            header(&headers, &header_name("app-version")),
            Some("0.1.230")
        );
        assert!(header(&headers, &header_name("occurred-at")).is_some());
        assert!(header(&headers, &header_name("feedback-id")).is_none());
        // Two standalone uploads never share an id.
        let other = diagnostics_upload_headers("0.1.230", None, None);
        assert_ne!(header(&other, &header_name("install-id")), Some(id));
    }

    #[test]
    fn an_attached_upload_reuses_the_feedback_submissions_id() {
        let submission = new_submission_id();
        let report = "5b1d6c0e-3f2a-4c8e-9d7b-0a1b2c3d4e5f";
        let headers = diagnostics_upload_headers("0.1.230", Some(report), Some(&submission));
        assert_eq!(
            header(&headers, &header_name("install-id")),
            Some(submission.as_str())
        );
        assert_eq!(header(&headers, &header_name("feedback-id")), Some(report));
    }

    #[test]
    fn the_payload_has_exactly_the_servers_key_set() {
        let id = new_submission_id();
        let payload = feedback_payload("It crashed", Some("a@example.com"), &id);
        let mut keys: Vec<&str> = payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "app_version",
                "contact",
                "install_id",
                "message",
                "occurred_at",
                "platform",
                "schema_version"
            ]
        );
        assert_eq!(payload["install_id"], id);
        assert_eq!(payload["schema_version"], 1);
        assert!(["windows", "macos", "linux"].contains(&payload["platform"].as_str().unwrap()));
        assert!(payload["contact"].is_string());
        assert!(feedback_payload("x", None, &id)["contact"].is_null());
    }
}
