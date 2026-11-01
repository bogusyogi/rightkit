// Google Meet REST client. Builds on the shared `google` client for auth,
// retry, and backoff — this module only knows the Meet Spaces endpoint and how
// to map the response to a `MeetSpace`.

use crate::google::Google;
use serde::Deserialize;

const SPACES_ENDPOINT: &str = "https://meet.googleapis.com/v2/spaces";

/// A newly created Google Meet space.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MeetSpace {
    /// Resource name, e.g. `spaces/abc-defg-hij`.
    pub name: String,
    /// Full join URL, e.g. `https://meet.google.com/abc-defg-hij`.
    pub meeting_uri: String,
    /// Short code portion only, e.g. `abc-defg-hij`.
    pub meeting_code: String,
}

/// POST to `https://meet.googleapis.com/v2/spaces` with an empty body.
/// Returns the join link and resource metadata on success.
pub async fn create_space(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<MeetSpace, String> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        name: String,
        #[serde(rename = "meetingUri", default)]
        meeting_uri: String,
        #[serde(rename = "meetingCode", default)]
        meeting_code: String,
    }

    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let resp: Resp = g
        .post_json::<serde_json::Value, Resp>(SPACES_ENDPOINT, &serde_json::json!({}))
        .await
        .map_err(String::from)?;

    Ok(MeetSpace {
        name: resp.name,
        meeting_uri: resp.meeting_uri,
        meeting_code: resp.meeting_code,
    })
}
