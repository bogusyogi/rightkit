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

/// Send one feedback note and then its diagnostics, paired by one fresh submission id.
///
/// Blocking, like [`submit_feedback`]; the host runs it off the UI thread. The feedback note
/// is sent first and its failure is returned as is. `send_diagnostics(report_id,
/// submission_id)` is the host's diagnostics upload (it builds the archive and calls
/// [`upload_diagnostics_archive`]); it runs only after the note was accepted. A diagnostics
/// failure never fails the feedback: it goes to `on_diagnostics_failure(report_id, error)`
/// and the feedback's report id is still returned.
pub fn submit_feedback_with_diagnostics(
    message: &str,
    contact: Option<&str>,
    send_diagnostics: impl FnOnce(&str, &str) -> Result<String, String>,
    on_diagnostics_failure: impl FnOnce(&str, &str),
) -> Result<String, String> {
    // One random id per submission, shared by the note and its diagnostics, never stored.
    let submission_id = new_submission_id();
    attach_diagnostics(
        &submission_id,
        |submission_id| submit_feedback(message, contact, submission_id),
        send_diagnostics,
        on_diagnostics_failure,
    )
}

fn attach_diagnostics(
    submission_id: &str,
    submit: impl FnOnce(&str) -> Result<String, String>,
    send_diagnostics: impl FnOnce(&str, &str) -> Result<String, String>,
    on_diagnostics_failure: impl FnOnce(&str, &str),
) -> Result<String, String> {
    let report_id = submit(submission_id)?;
    if let Err(error) = send_diagnostics(report_id.as_str(), submission_id) {
        on_diagnostics_failure(report_id.as_str(), error.as_str());
    }
    Ok(report_id)
}

/// Upload one host-created diagnostics archive after explicit user consent and delete it.
/// The archive is read once, never more than `max_bytes` + 1 bytes: the checked bytes are sent.
/// It is removed whether or not reading succeeds, since it exists only for this upload.
/// Removal is best effort.
/// Returns the server's report id
/// (header `<prefix>report-id`, the prefix set by [`configure_feedback_endpoint`]).
/// `upload_url` and `app_version` are host values.
pub fn upload_diagnostics_archive(
    archive: &std::path::Path,
    upload_url: &str,
    max_bytes: u64,
    app_version: &str,
    feedback_id: Option<&str>,
    submission_id: Option<&str>,
) -> Result<String, String> {
    // One read: the size that is checked and the body that is sent are the same immutable bytes.
    let bytes =
        rightkit_crash::export::read_archive_bounded(archive, max_bytes).map_err(
            |error| match error {
                rightkit_crash::export::BoundedReadError::TooLarge => {
                    format!("diagnostics ZIP exceeds {} MB", max_bytes / 1024 / 1024)
                }
                rightkit_crash::export::BoundedReadError::Unreadable => {
                    "read diagnostics ZIP failed".to_string()
                }
            },
        );
    // The archive existed only for this upload; the bytes (or the refusal) are all that is kept.
    let _ = std::fs::remove_file(archive);
    let bytes = bytes?;
    let client = crate::http_client::parity_client(Duration::from_secs(30))?;
    let mut request = rightkit_http::Request::post(upload_url);
    for (name, value) in diagnostics_upload_headers(app_version, feedback_id, submission_id) {
        request = request.header(name, value);
    }
    let request = request.body(bytes);
    let response =
        crate::http_client::send_head(&client, &request, true, "diagnostics send failed")?;
    if response.status != 202 {
        return Err(format!(
            "diagnostics endpoint returned {}",
            crate::http_client::status_display(response.status)
        ));
    }
    response
        .header(&header_name("report-id"))
        .map(str::to_owned)
        .ok_or_else(|| "diagnostics endpoint omitted report ID".to_string())
}
