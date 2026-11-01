// Gmail attachment listing and download. Builds on the shared `google` client
// for auth, retry, and backoff, and on the shared Gmail MIME payload structs.

use crate::gmail::{decode_b64url, Part};
use crate::google::Google;
use serde::Deserialize;

/// Metadata for a single attachment in a Gmail message.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AttachmentMeta {
    pub attachment_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
}

/// An inline image referenced by `cid:` in an HTML body. `cid` is the
/// Content-ID with surrounding `<>` stripped; the reader rewrites
/// `src="cid:{cid}"` to the embedded data URL. Bytes are base64 (standard).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InlineImage {
    pub cid: String,
    pub mime_type: String,
    pub data_b64: String,
}

/// A pending inline-image reference discovered in the MIME tree: its Content-ID,
/// mime type, and either inline base64 data or an attachmentId to fetch.
struct InlineRef {
    cid: String,
    mime_type: String,
    data: String,
    attachment_id: Option<String>,
}

/// Pull the `Content-ID` header off a part and strip the surrounding `<>`.
fn content_id(p: &Part) -> Option<String> {
    p.headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-id"))
        .map(|h| {
            h.value
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

/// Collect every `image/*` part that carries a Content-ID (i.e. is referenced
/// inline by the HTML body), recording where its bytes come from.
fn walk_inline(p: &Part, out: &mut Vec<InlineRef>) {
    if p.mime_type.to_ascii_lowercase().starts_with("image/") {
        if let Some(cid) = content_id(p) {
            if let Some(body) = &p.body {
                out.push(InlineRef {
                    cid,
                    mime_type: p.mime_type.clone(),
                    data: body.data.clone(),
                    attachment_id: body.attachment_id.clone(),
                });
            }
        }
    }
    for c in &p.parts {
        walk_inline(c, out);
    }
}

/// Recursively collect attachment metadata from a MIME part tree.
fn walk_parts(p: &Part, out: &mut Vec<AttachmentMeta>) {
    if !p.filename.is_empty() {
        if let Some(body) = &p.body {
            if let Some(aid) = &body.attachment_id {
                if !aid.is_empty() {
                    out.push(AttachmentMeta {
                        attachment_id: aid.clone(),
                        filename: p.filename.clone(),
                        mime_type: p.mime_type.clone(),
                        size: body.size.unwrap_or(0),
                    });
                }
            }
        }
    }
    for c in &p.parts {
        walk_parts(c, out);
    }
}

// --- public API ---

/// Return all attachments present in `message_id`, identified by their
/// `attachmentId` (needed to call [`download_attachment`]).
pub async fn list_attachments(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    message_id: &str,
) -> Result<Vec<AttachmentMeta>, String> {
    #[derive(Deserialize)]
    struct Msg {
        payload: Part,
    }

    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}");
    let msg: Msg = g
        .get_json(&url, &[("format", "full".to_string())])
        .await
        .map_err(String::from)?;

    let mut attachments = Vec::new();
    walk_parts(&msg.payload, &mut attachments);
    Ok(attachments)
}

/// Download the raw bytes of a single attachment identified by `attachment_id`
/// (obtained from [`list_attachments`]).
pub async fn download_attachment(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    message_id: &str,
    attachment_id: &str,
) -> Result<Vec<u8>, String> {
    #[derive(Deserialize)]
    struct AttachResp {
        #[serde(default)]
        data: String,
    }

    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/attachments/{attachment_id}"
    );
    let resp: AttachResp = g.get_json(&url, &[]).await.map_err(String::from)?;

    Ok(decode_b64url(&resp.data))
}

/// Return the inline (`cid:`) images embedded in `message_id` as
/// `cid → {mime_type, standard-base64 bytes}`. Parts with the bytes already
/// present in the message payload are used directly; the rest are fetched by
/// attachmentId. Non-image and non-Content-ID parts are ignored. Best-effort:
/// an image that fails to fetch is skipped, not fatal (the reader just shows a
/// broken `cid:` ref for that one).
pub async fn inline_images(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    message_id: &str,
) -> Result<Vec<InlineImage>, String> {
    #[derive(Deserialize)]
    struct Msg {
        payload: Part,
    }
    #[derive(Deserialize)]
    struct AttachResp {
        #[serde(default)]
        data: String,
    }

    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}");
    let msg: Msg = g
        .get_json(&url, &[("format", "full".to_string())])
        .await
        .map_err(String::from)?;

    let mut refs = Vec::new();
    walk_inline(&msg.payload, &mut refs);

    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        // Prefer inline data; otherwise fetch by attachmentId.
        let bytes = if !r.data.is_empty() {
            decode_b64url(&r.data)
        } else if let Some(aid) = &r.attachment_id {
            let aurl = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/attachments/{aid}"
            );
            match g.get_json::<AttachResp>(&aurl, &[]).await {
                Ok(resp) => decode_b64url(&resp.data),
                Err(_) => continue, // best-effort: skip the one that failed
            }
        } else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        out.push(InlineImage {
            cid: r.cid,
            mime_type: r.mime_type,
            data_b64: rightkit_crypto::base64_encode(&bytes),
        });
    }
    Ok(out)
}
