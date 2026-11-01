// Gmail Settings client — vacation responder + forwarding addresses.
// Builds on the shared `google` client for auth, retry, and backoff — this
// module only knows the /settings/ sub-tree of the Gmail REST API.
//
// Scope required: gmail.settings.basic (already authorised).
// Endpoints consumed:
//   GET/PUT  https://gmail.googleapis.com/gmail/v1/users/me/settings/vacation
//   GET      https://gmail.googleapis.com/gmail/v1/users/me/settings/forwardingAddresses

use crate::google::Google;
use serde::Deserialize;

const VACATION_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/settings/vacation";
const FORWARDING_ENDPOINT: &str =
    "https://gmail.googleapis.com/gmail/v1/users/me/settings/forwardingAddresses";

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Gmail vacation-responder settings, round-tripping Gmail's camelCase JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vacation {
    pub enable_auto_reply: bool,
    pub response_subject: String,
    /// Plain-text body. Gmail's field is `responseBodyPlainText`; the camelCase
    /// rule would map this to `responseBodyPlain`, so rename it explicitly.
    #[serde(rename = "responseBodyPlainText")]
    pub response_body_plain: String,
    pub response_body_html: String,
    pub restrict_to_contacts: bool,
    pub restrict_to_domain: bool,
    /// Epoch-millisecond window bounds. Gmail's API types these as int64-format
    /// STRINGS; keep them as `Option<String>` so a round-trip (get → edit → put)
    /// sends back exactly what Gmail expects. Absent when no window is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Return the current vacation-responder settings for one Gmail account.
pub async fn get_vacation(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vacation, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    g.get_json(VACATION_ENDPOINT, &[])
        .await
        .map_err(String::from)
}

/// Write updated vacation-responder settings back to Gmail.
///
/// Gmail's PUT response mirrors the resource; we deserialise into `Vacation`
/// as a throwaway to satisfy `put_json`'s type parameter and discard it.
pub async fn update_vacation(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    v: &Vacation,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let _: Vacation = g
        .put_json(VACATION_ENDPOINT, v)
        .await
        .map_err(String::from)?;
    Ok(())
}

/// Return the list of verified forwarding addresses for one Gmail account.
/// Read-only — the API does not support creating forwarding addresses via
/// `settings.basic` scope.
pub async fn list_forwarding_addresses(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ForwardingAddress {
        forwarding_email: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ForwardingResp {
        #[serde(default)]
        forwarding_addresses: Vec<ForwardingAddress>,
    }

    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let resp: ForwardingResp = g
        .get_json(FORWARDING_ENDPOINT, &[])
        .await
        .map_err(String::from)?;

    Ok(resp
        .forwarding_addresses
        .into_iter()
        .map(|a| a.forwarding_email)
        .collect())
}

// ---------------------------------------------------------------------------
// Tests — offline shape checks only, no network calls
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vacation_roundtrips_gmail_json() {
        let raw = r#"{
            "enableAutoReply": true,
            "responseSubject": "Out of office",
            "responseBodyPlainText": "Back Monday.",
            "responseBodyHtml": "<p>Back Monday.</p>",
            "restrictToContacts": false,
            "restrictToDomain": true,
            "startTime": "1716412800000",
            "endTime": "1716844800000"
        }"#;

        let v: Vacation = serde_json::from_str(raw).unwrap();
        assert!(v.enable_auto_reply);
        assert_eq!(v.response_body_plain, "Back Monday.");
        assert_eq!(v.start_time.as_deref(), Some("1716412800000"));
        assert_eq!(v.end_time.as_deref(), Some("1716844800000"));

        // Serialise back → deserialise again: idempotent.
        let re: Vacation = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(re.response_subject, "Out of office");
    }

    #[test]
    fn vacation_accepts_absent_times() {
        let raw = r#"{
            "enableAutoReply": false,
            "responseSubject": "",
            "responseBodyPlainText": "",
            "responseBodyHtml": "",
            "restrictToContacts": false,
            "restrictToDomain": false
        }"#;
        let v: Vacation = serde_json::from_str(raw).unwrap();
        assert_eq!(v.start_time, None);
        assert_eq!(v.end_time, None);
    }

    #[test]
    fn vacation_accepts_null_times() {
        let raw = r#"{
            "enableAutoReply": false,
            "responseSubject": "",
            "responseBodyPlainText": "",
            "responseBodyHtml": "",
            "restrictToContacts": false,
            "restrictToDomain": false,
            "startTime": null,
            "endTime": null
        }"#;
        let v: Vacation = serde_json::from_str(raw).unwrap();
        assert_eq!(v.start_time, None);
        assert_eq!(v.end_time, None);
    }
}
