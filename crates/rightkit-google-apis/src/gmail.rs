mod sync_full;
pub use sync_full::fetch_sync;
pub use sync_full::FullSyncApi;
#[cfg(test)]
pub use sync_full::{run_full_sync, run_sync_pass, with_pass_deadline};
mod message_fetch;
use message_fetch::{get_full, get_metadata};
#[cfg(test)]
use message_fetch::{parse_from, walk_parts};
mod types;
pub use types::*;
mod batch_fetch;
mod deadline;
// Gmail REST client. Builds on the shared `google` client for auth, retry, and
// backoff — this module only knows Gmail's endpoints, query shapes, and how to
// parse a message (a MIME walk for the text/plain + text/html bodies). All
// egress is in Rust; the webview never calls Google.

use crate::google::{Google, GoogleError};
mod mime;
#[cfg(test)]
pub use mime::PartBody;
pub use mime::{build_raw_mime, OutgoingAttachment};
pub use mime::{decode_b64url, encode_raw, Part};
mod compose_api;
pub use compose_api::*;
mod history;
pub use history::*;

use serde::Deserialize;
use std::collections::HashSet;

const LIST_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages";
const MSG_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages";
const BATCH_MODIFY_ENDPOINT: &str =
    "https://gmail.googleapis.com/gmail/v1/users/me/messages/batchModify";
#[allow(dead_code)]
const BATCH_DELETE_ENDPOINT: &str =
    "https://gmail.googleapis.com/gmail/v1/users/me/messages/batchDelete";
const LABELS_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/labels";
const HISTORY_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/history";
const PROFILE_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/profile";
const SEND_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";
const SENDAS_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/settings/sendAs";
const DRAFTS_ENDPOINT: &str = "https://gmail.googleapis.com/gmail/v1/users/me/drafts";

/// Per-system-label fetch caps for a full sync. Tuned to keep first-sync API
/// volume bounded; custom labels are catalogued but not listed here —
/// custom-folder sync is deferred.
///
/// UNREAD is synced explicitly, NOT just covered by the INBOX window: in a large
/// mailbox the 50 newest inbox messages can all be read, leaving older unread
/// mail unmirrored. That undercounts the unread badge (a local COUNT over the
/// mirror) and empties the Unread view. Listing UNREAD pulls unread mail of any
/// age; the id union dedupes, so an unread INBOX message still costs one fetch.
const SYNC_FOLDERS: &[(&str, u32)] = &[
    ("INBOX", 50),
    ("UNREAD", 200),
    ("SENT", 50),
    ("STARRED", 50),
    ("DRAFT", 25),
    ("SPAM", 25),
    ("TRASH", 25),
];

/// List up to `cap` message ids carrying `label`, paging via nextPageToken.
async fn list_label_ids(g: &Google, label: &str, cap: u32) -> Result<Vec<String>, GoogleError> {
    #[derive(Deserialize)]
    struct MsgRef {
        id: String,
    }
    #[derive(Deserialize)]
    struct ListResp {
        #[serde(default)]
        messages: Vec<MsgRef>,
        #[serde(rename = "nextPageToken", default)]
        next_page_token: Option<String>,
    }
    let mut out: Vec<String> = Vec::new();
    let mut page_token: Option<String> = None;
    while (out.len() as u32) < cap {
        let page_size = (cap - out.len() as u32).min(500);
        let mut query = vec![
            ("labelIds", label.to_string()),
            ("maxResults", page_size.to_string()),
        ];
        if let Some(t) = &page_token {
            query.push(("pageToken", t.clone()));
        }
        let resp: ListResp = deadline::get_json(g, "messages.list", LIST_ENDPOINT, &query).await?;
        let got = resp.messages.len();
        out.extend(resp.messages.into_iter().map(|m| m.id));
        match resp.next_page_token {
            Some(t) if got > 0 && (out.len() as u32) < cap => page_token = Some(t),
            _ => break,
        }
    }
    out.truncate(cap as usize);
    Ok(out)
}

/// Page up to `cap` message ids matching arbitrary messages.list parameters
/// (any mix of `labelIds`, `q`, `includeSpamTrash`). `base` is the param set
/// applied to every page; this adds `maxResults`/`pageToken`. Generalizes
/// `list_query_ids`/`list_label_ids` so a caller can scope by label id AND a
/// `before:` time bound at once.
async fn list_ids_paged(
    g: &Google,
    base: &[(&'static str, String)],
    cap: u32,
) -> Result<Vec<String>, GoogleError> {
    #[derive(Deserialize)]
    struct MsgRef {
        id: String,
    }
    #[derive(Deserialize)]
    struct ListResp {
        #[serde(default)]
        messages: Vec<MsgRef>,
        #[serde(rename = "nextPageToken", default)]
        next_page_token: Option<String>,
    }
    let mut out: Vec<String> = Vec::new();
    let mut page_token: Option<String> = None;
    while (out.len() as u32) < cap {
        let page_size = (cap - out.len() as u32).min(500);
        let mut query: Vec<(&str, String)> = base.iter().map(|(k, v)| (*k, v.clone())).collect();
        query.push(("maxResults", page_size.to_string()));
        if let Some(t) = &page_token {
            query.push(("pageToken", t.clone()));
        }
        let resp: ListResp = deadline::get_json(g, "messages.list", LIST_ENDPOINT, &query).await?;
        let got = resp.messages.len();
        out.extend(resp.messages.into_iter().map(|m| m.id));
        match resp.next_page_token {
            Some(t) if got > 0 && (out.len() as u32) < cap => page_token = Some(t),
            _ => break,
        }
    }
    out.truncate(cap as usize);
    Ok(out)
}

/// Fetch up to `limit` messages OLDER than `before_secs` (epoch seconds) within
/// a scope expressed as `label_ids` (empty = no label filter, e.g. All Mail), an
/// extra Gmail `q` (carries `before:<secs>` plus any `in:`/`-in:` operators), and
/// `include_spam_trash` (required when listing TRASH/SPAM). Backs "load older"
/// pagination for any folder or label. Per-message failures are collected
/// (class only), never swallowed.
pub async fn fetch_older(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    label_ids: &[String],
    q: &str,
    include_spam_trash: bool,
    limit: u32,
) -> Result<SyncOutcome, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let mut base: Vec<(&'static str, String)> = Vec::new();
    for l in label_ids {
        base.push(("labelIds", l.clone()));
    }
    let q = q.trim();
    if !q.is_empty() {
        base.push(("q", q.to_string()));
    }
    if include_spam_trash {
        base.push(("includeSpamTrash", "true".to_string()));
    }
    let ids = list_ids_paged(&g, &base, limit)
        .await
        .map_err(|e| e.to_string())?;
    // L12: fetch the listed ids with bounded concurrency instead of one-at-a-time.
    // The pagination page itself is still a single round-trip; only the per-id
    // fetches below run in parallel. Apply-side ordering preserved.
    let ordered =
        batch_fetch::fetch_with_concurrency(&ids, batch_fetch::INITIAL_CONCURRENCY, |id| {
            let g = g.clone();
            async move { get_full(&g, &id).await }
        })
        .await;
    let (messages, skipped) = batch_fetch::partition_outcome(ordered);
    Ok(SyncOutcome { messages, skipped })
}

/// Fetch the account's full label catalog (system + user/custom labels).
pub async fn list_labels(g: &Google) -> Result<Vec<LabelInfo>, GoogleError> {
    #[derive(Deserialize)]
    struct Color {
        #[serde(rename = "backgroundColor", default)]
        background_color: String,
    }
    #[derive(Deserialize)]
    struct L {
        id: String,
        #[serde(default)]
        name: String,
        #[serde(rename = "type", default)]
        label_type: String,
        #[serde(default)]
        color: Option<Color>,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        labels: Vec<L>,
    }
    let resp: Resp = deadline::get_json(g, "labels.list", LABELS_ENDPOINT, &[]).await?;
    Ok(resp
        .labels
        .into_iter()
        .map(|l| LabelInfo {
            id: l.id,
            name: l.name,
            label_type: l.label_type,
            color: l
                .color
                .map(|c| c.background_color)
                .filter(|c| !c.is_empty()),
        })
        .collect())
}

/// Metadata-only fetch (`format=metadata`): headers + labels + date, NO body.
/// Same 20 quota units as a full get, but no body bandwidth — used by the
/// background backfill to make the list complete without downloading bodies.
/// quota units per call.
pub async fn list_message_page(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    q: &str,
    page_token: Option<&str>,
    page_size: u32,
) -> Result<(Vec<String>, Option<String>), String> {
    #[derive(Deserialize)]
    struct MsgRef {
        id: String,
    }
    #[derive(Deserialize)]
    struct ListResp {
        #[serde(default)]
        messages: Vec<MsgRef>,
        #[serde(rename = "nextPageToken", default)]
        next_page_token: Option<String>,
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let mut params = vec![("maxResults", page_size.clamp(1, 500).to_string())];
    if !q.trim().is_empty() {
        params.push(("q", q.trim().to_string()));
    }
    if let Some(t) = page_token {
        params.push(("pageToken", t.to_string()));
    }
    let resp: ListResp = deadline::get_json(&g, "messages.list", LIST_ENDPOINT, &params)
        .await
        .map_err(|e| e.to_string())?;
    let ids = resp.messages.into_iter().map(|m| m.id).collect();
    Ok((ids, resp.next_page_token.filter(|t| !t.is_empty())))
}

/// Fetch FULL messages (headers + bodies) for a set of ids — the download-all
/// archive (T22). Per-message failures are collected, never swallowed.
///
/// L12: requests run with bounded concurrency (initial = 6) via
/// `batch_fetch::fetch_with_concurrency`; on a 429/403-rate response the helper
/// backs off (1s/2s/4s, max 3 attempts) and the next-batch caller would use
/// the reduced cap. Apply-side ordering is preserved (results sorted by input
/// index) so downstream `apply_search_messages` keeps seeing results in the
/// same order the prior sequential loop produced.
pub async fn fetch_full_batch(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    ids: &[String],
) -> Result<SyncOutcome, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let ordered =
        batch_fetch::fetch_with_concurrency(ids, batch_fetch::INITIAL_CONCURRENCY, |id| {
            let g = g.clone();
            async move { get_full(&g, &id).await }
        })
        .await;
    let (messages, skipped) = batch_fetch::partition_outcome(ordered);
    Ok(SyncOutcome { messages, skipped })
}

/// Fetch metadata for a set of message ids (for backfill). Per-message failures
/// are collected, never swallowed. See `fetch_full_batch` for the L12
/// concurrency/backoff contract.
pub async fn fetch_metadata_batch(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    ids: &[String],
) -> Result<SyncOutcome, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let ordered =
        batch_fetch::fetch_with_concurrency(ids, batch_fetch::INITIAL_CONCURRENCY, |id| {
            let g = g.clone();
            async move { get_metadata(&g, &id).await }
        })
        .await;
    let (messages, skipped) = batch_fetch::partition_outcome(ordered);
    Ok(SyncOutcome { messages, skipped })
}

/// "Display Name <addr@x.com>" -> ("Display Name", "addr@x.com").
/// Apply Gmail label changes to a set of messages via batchModify. Best-effort.
/// Used for mark read/unread, archive (remove INBOX), trash (add TRASH).
pub async fn modify_labels(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    ids: &[String],
    add: &[&str],
    remove: &[&str],
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let body = serde_json::json!({
        "ids": ids,
        "addLabelIds": add,
        "removeLabelIds": remove,
    });
    g.post_no_content(BATCH_MODIFY_ENDPOINT, &body)
        .await
        .map_err(String::from)
}

/// Permanently delete messages (Empty Trash). IRREVERSIBLE — there is no
/// "undo", which is why the only caller routes through the DestructiveMutation
/// guard (off by default). NOTE: batchDelete requires the full
/// `https://mail.google.com/` scope; the default scope set ([`crate::oauth::SCOPE`]) requests only
/// `gmail.modify`, so Google returns 403 here until the scope is broadened and
/// the account reconnected. The error is surfaced, not swallowed, and no local
/// rows are purged unless this succeeds.
#[allow(dead_code)]
pub async fn batch_delete(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    ids: &[String],
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let body = serde_json::json!({ "ids": ids });
    g.post_no_content(BATCH_DELETE_ENDPOINT, &body)
        .await
        .map_err(String::from)
}

#[cfg(test)]
mod tests;
