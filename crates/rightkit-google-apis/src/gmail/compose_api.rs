use super::*;

/// A send-as identity (the account address + any verified aliases), for the
/// compose alias picker.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct SendAsInfo {
    pub email: String,
    pub name: String,
    pub is_default: bool,
}

/// List the account's send-as addresses (primary + verified aliases). Uses the
/// gmail.settings.basic scope.
pub async fn list_send_as(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<SendAsInfo>, String> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "sendAsEmail")]
        email: String,
        #[serde(rename = "displayName", default)]
        display_name: String,
        #[serde(rename = "isDefault", default)]
        is_default: bool,
        #[serde(rename = "isPrimary", default)]
        is_primary: bool,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "sendAs", default)]
        send_as: Vec<Raw>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let r: Resp = g
        .get_json(SENDAS_ENDPOINT, &[])
        .await
        .map_err(String::from)?;
    Ok(r.send_as
        .into_iter()
        .map(|s| SendAsInfo {
            email: s.email,
            name: s.display_name,
            is_default: s.is_default || s.is_primary,
        })
        .collect())
}

/// Reference to a just-sent message, returned by Gmail's send endpoint.
/// Used to immediately fetch + upsert the sent message so it appears in the
/// Sent folder without waiting for the next sync poll.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SentRef {
    #[serde(default)]
    pub id: String,
    // Parsed from Gmail's send response; kept for callers that need to thread
    // the sent message locally even though the current Sent-mirror path keys off
    // `id` only.
    #[allow(dead_code)]
    #[serde(rename = "threadId", default)]
    pub thread_id: String,
}

/// Send a prebuilt RFC 822 message. `thread_id` threads a reply within Gmail.
/// Returns the sent message ref (id + threadId) so callers can immediately
/// fetch it into the local mirror.
pub async fn send_message(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    raw_mime: &str,
    thread_id: Option<&str>,
) -> Result<SentRef, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let raw = super::encode_raw(raw_mime);
    let body = match thread_id {
        Some(t) if !t.is_empty() => serde_json::json!({ "raw": raw, "threadId": t }),
        _ => serde_json::json!({ "raw": raw }),
    };
    g.post_json::<_, SentRef>(SEND_ENDPOINT, &body)
        .await
        .map_err(send_error_text)
}

/// Prefix of a send error whose delivery is unknown: the message may have been sent. Such an outcome
/// is never retried automatically (outbox drain, retry sweep, quit drain); the user decides.
pub const UNCERTAIN_SEND_PREFIX: &str =
    "send outcome unknown (the message may have been sent; check Sent before sending again): ";

/// True when `error` came from [`send_error_text`] with an uncertain delivery.
pub fn is_uncertain_send(error: &str) -> bool {
    error.starts_with(UNCERTAIN_SEND_PREFIX)
}

/// The error text of a failed send, keeping the delivery classification of the typed failure.
pub fn send_error_text(error: crate::google::GoogleError) -> String {
    if error.delivery_uncertain() {
        format!("{UNCERTAIN_SEND_PREFIX}{error}")
    } else {
        error.to_string()
    }
}

/// List `(draft_id, message_id)` pairs so the UI can map a synced DRAFT message
/// back to its Gmail draft id (for edit + PUT-by-id). Read-only.
pub async fn list_drafts(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<(String, String)>, String> {
    #[derive(Deserialize)]
    struct MsgRef {
        #[serde(default)]
        id: String,
    }
    #[derive(Deserialize)]
    struct Draft {
        #[serde(default)]
        id: String,
        #[serde(default)]
        message: Option<MsgRef>,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        drafts: Vec<Draft>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let r: Resp = g
        .get_json(
            DRAFTS_ENDPOINT,
            &[("fields", "drafts(id,message/id)".to_string())],
        )
        .await
        .map_err(String::from)?;
    Ok(r.drafts
        .into_iter()
        .filter_map(|d| d.message.map(|m| (d.id, m.id)))
        .collect())
}

/// Fetch a message's full RFC 822 source (`format=raw`), decoded to bytes. Backs
/// "Show original" (rendered as text) and "Download .eml" (written verbatim).
/// Read-only.
pub async fn fetch_raw_message(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    message_id: &str,
) -> Result<Vec<u8>, String> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        raw: String,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let r: Raw = g
        .get_json(
            &format!("{MSG_ENDPOINT}/{message_id}"),
            &[("format", "raw".to_string())],
        )
        .await
        .map_err(String::from)?;
    Ok(decode_b64url(&r.raw))
}
