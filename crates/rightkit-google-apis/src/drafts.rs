// Gmail Drafts — create or update a draft via the shared Google client.
// Mirror of gmail.rs style: base64url-encode raw MIME, delegate auth/retry to
// Google::new, surface errors as String so Tauri commands can use `?`.

use crate::google::Google;
use serde::Deserialize;

const DRAFTS_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/drafts";

#[derive(Deserialize)]
struct Resp {
    #[serde(default)]
    id: String,
    #[serde(default)]
    message: Option<RespMessage>,
}

#[derive(Deserialize)]
struct RespMessage {
    #[serde(default)]
    id: String,
}

/// Save a Gmail draft.  When `gmail_draft_id` is Some the existing draft is
/// updated (PUT); otherwise a new draft is created (POST).  Returns
/// `(draft_id, created_message_id)` — the message id lets the caller pull the
/// new draft straight into the local DB so it shows in Drafts without a sync.
pub async fn save_draft(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    raw_mime: &str,
    gmail_draft_id: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let raw = crate::gmail::encode_raw(raw_mime);
    let body = serde_json::json!({ "message": { "raw": raw } });

    let resp: Resp = match gmail_draft_id {
        Some(id) if !id.is_empty() => {
            let url = format!("{DRAFTS_ENDPOINT}/{id}");
            g.put_json(&url, &body).await.map_err(String::from)?
        }
        _ => g
            .post_json(DRAFTS_ENDPOINT, &body)
            .await
            .map_err(String::from)?,
    };

    Ok((
        resp.id,
        resp.message.map(|m| m.id).filter(|s| !s.is_empty()),
    ))
}

/// Create a new Gmail draft, optionally inside an existing thread. Never
/// updates or sends. Returns `(draft_id, created_message_id)`.
pub async fn create_draft(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    raw_mime: &str,
    thread_id: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let raw = crate::gmail::encode_raw(raw_mime);
    let message = match thread_id {
        Some(t) if !t.is_empty() => serde_json::json!({ "raw": raw, "threadId": t }),
        _ => serde_json::json!({ "raw": raw }),
    };
    let resp: Resp = g
        .post_json(DRAFTS_ENDPOINT, &serde_json::json!({ "message": message }))
        .await
        .map_err(String::from)?;
    Ok((
        resp.id,
        resp.message.map(|m| m.id).filter(|s| !s.is_empty()),
    ))
}

/// Permanently delete a Gmail draft. Calls
/// `DELETE /gmail/v1/users/me/drafts/{id}` — once deleted, the draft is
/// gone from Gmail AND the next sync will not refetch it (the messages.list
/// DRAFT folder no longer contains it). Local DRAFT label is dropped by the
/// caller after a successful remote delete.
pub async fn delete_draft(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    draft_id: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{DRAFTS_ENDPOINT}/{draft_id}");
    g.delete_no_content(&url).await.map_err(String::from)
}

/// Read one draft as Gmail holds it now (`format=full`: headers and parts).
/// Read only. Backs the agent send-approval card.
pub async fn get_draft(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    draft_id: &str,
) -> Result<serde_json::Value, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    g.get_json(
        &format!("{DRAFTS_ENDPOINT}/{draft_id}"),
        &[("format", "full".to_string())],
    )
    .await
    .map_err(String::from)
}

/// Send an existing Gmail draft as it stands (`drafts.send`; Gmail removes the
/// draft). Returns the sent message id. Called only by the agent approval path,
/// after the user approved that exact draft.
pub async fn send_existing_draft(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    draft_id: &str,
) -> Result<String, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let sent: serde_json::Value = g
        .post_json(
            &format!("{DRAFTS_ENDPOINT}/send"),
            &serde_json::json!({ "id": draft_id }),
        )
        .await
        .map_err(crate::gmail::send_error_text)?;
    Ok(sent
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}
