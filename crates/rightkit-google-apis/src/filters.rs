// Gmail filter management. Builds on the shared `google` client for auth —
// this module only knows the filters endpoint and request shape.
//
// Usage notes:
//   "Block sender":  create_filter(from, &["TRASH"], &["INBOX"])
//   Plain label rule: create_filter(from, &[label_id], &[])

use crate::google::Google;
use serde::Deserialize;

const FILTERS_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/settings/filters";

/// A Gmail filter, flattened for display in Settings.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FilterInfo {
    pub id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
}

/// List the account's Gmail filters (read-only, for the Settings list).
pub async fn list_filters(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Vec<FilterInfo>, String> {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct Criteria {
        #[serde(default)]
        from: String,
        #[serde(default)]
        to: String,
        #[serde(default)]
        subject: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct Action {
        #[serde(default)]
        add_label_ids: Vec<String>,
        #[serde(default)]
        remove_label_ids: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Filter {
        id: String,
        #[serde(default)]
        criteria: Criteria,
        #[serde(default)]
        action: Action,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        filter: Vec<Filter>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let r: Resp = g
        .get_json(FILTERS_ENDPOINT, &[])
        .await
        .map_err(String::from)?;
    Ok(r.filter
        .into_iter()
        .map(|f| FilterInfo {
            id: f.id,
            from: f.criteria.from,
            to: f.criteria.to,
            subject: f.criteria.subject,
            add_labels: f.action.add_label_ids,
            remove_labels: f.action.remove_label_ids,
        })
        .collect())
}

/// Delete a Gmail filter by id.
pub async fn delete_filter(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    filter_id: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("{FILTERS_ENDPOINT}/{filter_id}");
    g.delete_no_content(&url).await.map_err(String::from)
}

/// Creates a Gmail filter that matches messages from `from_addr` and applies
/// the given label actions.
///
/// # Block-sender pattern
/// ```text
/// create_filter(id, secret, token, "spam@example.com", &["TRASH"], &["INBOX"]).await?;
/// ```
///
/// # Plain label rule
/// ```text
/// create_filter(id, secret, token, "news@example.com", &["Label_42"], &[]).await?;
/// ```
pub async fn create_filter(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    from_addr: &str,
    add_label_ids: &[&str],
    remove_label_ids: &[&str],
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let body = serde_json::json!({
        "criteria": {
            "from": from_addr
        },
        "action": {
            "addLabelIds": add_label_ids,
            "removeLabelIds": remove_label_ids
        }
    });

    g.post_no_content(FILTERS_ENDPOINT, &body)
        .await
        .map_err(String::from)
}

/// Create a Gmail filter from a full criteria set + action label arrays (the
/// Settings filter builder). Any empty criterion field is omitted so Gmail
/// doesn't reject it; `has_attachment` is only sent when true.
#[allow(clippy::too_many_arguments)]
pub async fn create_filter_full(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    from: &str,
    to: &str,
    subject: &str,
    query: &str,
    has_attachment: bool,
    add_label_ids: &[String],
    remove_label_ids: &[String],
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let mut criteria = serde_json::Map::new();
    for (k, v) in [
        ("from", from),
        ("to", to),
        ("subject", subject),
        ("query", query),
    ] {
        if !v.trim().is_empty() {
            criteria.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    if has_attachment {
        criteria.insert("hasAttachment".to_string(), serde_json::Value::Bool(true));
    }
    if criteria.is_empty() {
        return Err("a filter needs at least one criterion".into());
    }

    let body = serde_json::json!({
        "criteria": criteria,
        "action": {
            "addLabelIds": add_label_ids,
            "removeLabelIds": remove_label_ids
        }
    });

    g.post_no_content(FILTERS_ENDPOINT, &body)
        .await
        .map_err(String::from)
}
