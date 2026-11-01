use super::*;

pub fn walk_parts(
    p: &Part,
    plain: &mut Option<String>,
    html: &mut Option<String>,
    attachments: &mut Vec<FetchedAttachment>,
) {
    let mt = p.mime_type.to_ascii_lowercase();
    if !p.filename.is_empty() {
        if let Some(body) = &p.body {
            if let Some(attachment_id) = &body.attachment_id {
                if !attachment_id.is_empty() {
                    attachments.push(FetchedAttachment {
                        attachment_id: attachment_id.clone(),
                        filename: p.filename.clone(),
                        mime_type: p.mime_type.clone(),
                        size: body.size.unwrap_or(0),
                    });
                }
            }
        }
    }
    if mt == "text/plain" && plain.is_none() {
        if let Some(b) = &p.body {
            if !b.data.is_empty() {
                *plain = Some(String::from_utf8_lossy(&decode_b64url(&b.data)).into_owned());
            }
        }
    } else if mt == "text/html" && html.is_none() {
        if let Some(b) = &p.body {
            if !b.data.is_empty() {
                *html = Some(String::from_utf8_lossy(&decode_b64url(&b.data)).into_owned());
            }
        }
    }
    for c in &p.parts {
        walk_parts(c, plain, html, attachments);
    }
}

async fn get_metadata_inner(g: &Google, id: &str) -> Result<FetchedMessage, GoogleError> {
    #[derive(Deserialize)]
    struct Hdr {
        #[serde(default)]
        name: String,
        #[serde(default)]
        value: String,
    }
    #[derive(Deserialize, Default)]
    struct Payload {
        #[serde(default)]
        headers: Vec<Hdr>,
    }
    #[derive(Deserialize)]
    struct Msg {
        id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(default)]
        snippet: String,
        #[serde(rename = "internalDate", default)]
        internal_date: String,
        #[serde(rename = "labelIds", default)]
        label_ids: Vec<String>,
        #[serde(rename = "sizeEstimate", default)]
        size_estimate: Option<i64>,
        #[serde(default)]
        payload: Payload,
    }
    let m: Msg = g
        .get_json(
            &format!("{MSG_ENDPOINT}/{id}"),
            &[
                ("format", "metadata".to_string()),
                ("metadataHeaders", "From".to_string()),
                ("metadataHeaders", "To".to_string()),
                ("metadataHeaders", "Cc".to_string()),
                ("metadataHeaders", "Subject".to_string()),
                ("metadataHeaders", "Reply-To".to_string()),
                ("metadataHeaders", "List-Unsubscribe".to_string()),
                ("metadataHeaders", "Authentication-Results".to_string()),
                ("metadataHeaders", "Message-Id".to_string()),
            ],
        )
        .await?;
    let mut from_raw = String::new();
    let mut subject = String::new();
    let mut to_addrs = String::new();
    let mut cc_addrs = String::new();
    let mut reply_to: Option<String> = None;
    let mut list_unsubscribe: Option<String> = None;
    let mut auth_results: Option<String> = None;
    let mut rfc822_message_id: Option<String> = None;
    for h in &m.payload.headers {
        match h.name.to_ascii_lowercase().as_str() {
            "from" => from_raw = h.value.clone(),
            "subject" => subject = h.value.clone(),
            "to" => to_addrs = h.value.clone(),
            "cc" => cc_addrs = h.value.clone(),
            "reply-to" => reply_to = Some(h.value.clone()),
            "list-unsubscribe" => list_unsubscribe = Some(h.value.clone()),
            "authentication-results" if auth_results.is_none() => {
                auth_results = Some(h.value.clone())
            }
            "message-id" if rfc822_message_id.is_none() => {
                rfc822_message_id = Some(normalize_message_id(&h.value))
            }
            _ => {}
        }
    }
    let (from_name, from_addr) = parse_from(&from_raw);
    Ok(FetchedMessage {
        id: m.id,
        thread_id: m.thread_id,
        from_name,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        list_unsubscribe,
        auth_results,
        subject,
        snippet: m.snippet,
        internal_date: m.internal_date.parse::<i64>().unwrap_or(0),
        unread: m.label_ids.iter().any(|l| l == "UNREAD"),
        label_ids: m.label_ids,
        body_plain: None,
        body_html: None,
        has_attachments: false,
        attachments: Vec::new(),
        size_estimate: m.size_estimate,
        rfc822_message_id,
    })
}

/// One page of message ids over a scope, for enumeration/backfill. Returns the
/// page's ids and the next page token (None when exhausted). messages.list = 5
async fn get_full_inner(g: &Google, id: &str) -> Result<FetchedMessage, GoogleError> {
    #[derive(Deserialize)]
    struct Msg {
        id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(default)]
        snippet: String,
        #[serde(rename = "internalDate", default)]
        internal_date: String,
        #[serde(rename = "labelIds", default)]
        label_ids: Vec<String>,
        #[serde(rename = "sizeEstimate", default)]
        size_estimate: Option<i64>,
        payload: Part,
    }

    let m: Msg = g
        .get_json(
            &format!("{MSG_ENDPOINT}/{id}"),
            &[("format", "full".to_string())],
        )
        .await?;

    let mut from_raw = String::new();
    let mut subject = String::new();
    let mut to_addrs = String::new();
    let mut cc_addrs = String::new();
    let mut reply_to: Option<String> = None;
    let mut list_unsubscribe: Option<String> = None;
    let mut auth_results: Option<String> = None;
    let mut rfc822_message_id: Option<String> = None;
    for h in &m.payload.headers {
        // Header names are case-insensitive (RFC 5322); match lowercased.
        match h.name.to_ascii_lowercase().as_str() {
            "from" => from_raw = h.value.clone(),
            "subject" => subject = h.value.clone(),
            "to" => to_addrs = h.value.clone(),
            "cc" => cc_addrs = h.value.clone(),
            "reply-to" => reply_to = Some(h.value.clone()),
            "list-unsubscribe" => list_unsubscribe = Some(h.value.clone()),
            // Keep the first (top-most) Authentication-Results — that's the one
            // added by the receiving server (Gmail), the trustworthy one.
            "authentication-results" if auth_results.is_none() => {
                auth_results = Some(h.value.clone())
            }
            "message-id" if rfc822_message_id.is_none() => {
                rfc822_message_id = Some(normalize_message_id(&h.value))
            }
            _ => {}
        }
    }
    let (from_name, from_addr) = parse_from(&from_raw);

    let mut plain = None;
    let mut html = None;
    let mut attachments = Vec::new();
    walk_parts(&m.payload, &mut plain, &mut html, &mut attachments);
    let has_attachments = !attachments.is_empty();

    Ok(FetchedMessage {
        id: m.id,
        thread_id: m.thread_id,
        from_name,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        list_unsubscribe,
        auth_results,
        subject,
        snippet: m.snippet,
        internal_date: m.internal_date.parse::<i64>().unwrap_or(0),
        unread: m.label_ids.iter().any(|l| l == "UNREAD"),
        label_ids: m.label_ids,
        body_plain: plain,
        body_html: html,
        has_attachments,
        attachments,
        size_estimate: m.size_estimate,
        rfc822_message_id,
    })
}

/// Normalize an RFC 5322 `Message-Id` header value to the bare form stored in
/// `messages.rfc822_message_id`: trims whitespace and strips a single pair of
/// surrounding angle brackets if present (the header is conventionally
/// `<id@host>`; the search-side `rfc822msgid:` operator already strips `<>`
/// from the query value, so storing the bare form keeps both sides in the
/// same shape — see db/search/query.rs and predicates.rs TRIM(...,'<>')).
fn normalize_message_id(raw: &str) -> String {
    raw.trim().trim_matches(['<', '>']).to_string()
}

pub fn parse_from(raw: &str) -> (String, String) {
    let raw = raw.trim();
    if let (Some(lt), Some(gt)) = (raw.find('<'), raw.rfind('>')) {
        if gt > lt {
            let addr = raw[lt + 1..gt].trim().to_string();
            let name = raw[..lt].trim().trim_matches('"').trim().to_string();
            let name = if name.is_empty() { addr.clone() } else { name };
            return (name, addr);
        }
    }
    (raw.to_string(), raw.to_string())
}

/// Fetch one full message, bounded by `REQUEST_TIMEOUT` (connect + response +
/// body). A stalled request surfaces as a retryable `Transient` error.
pub async fn get_full(g: &Google, id: &str) -> Result<FetchedMessage, GoogleError> {
    super::deadline::bounded(
        super::deadline::REQUEST_TIMEOUT,
        "messages.get",
        get_full_inner(g, id),
    )
    .await
}

/// Fetch one message's metadata, bounded by `REQUEST_TIMEOUT`.
pub async fn get_metadata(g: &Google, id: &str) -> Result<FetchedMessage, GoogleError> {
    super::deadline::bounded(
        super::deadline::REQUEST_TIMEOUT,
        "messages.get(metadata)",
        get_metadata_inner(g, id),
    )
    .await
}
