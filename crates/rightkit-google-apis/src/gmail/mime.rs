use super::*;

/// Gmail base64url (URL-safe, often unpadded). Normalize to standard + pad.
pub fn decode_b64url(data: &str) -> Vec<u8> {
    let mut s = data.replace('-', "+").replace('_', "/");
    while !s.len().is_multiple_of(4) {
        s.push('=');
    }
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s.as_bytes())
        .unwrap_or_default()
}

/// The `raw` field Gmail's draft and send calls take: URL-safe base64 of the MIME text, padded
/// (the owner's `base64url_encode` is unpadded; Gmail's documented examples pad).
pub fn encode_raw(mime: &str) -> String {
    let mut raw = rightkit_crypto::base64url_encode(mime.as_bytes());
    while !raw.len().is_multiple_of(4) {
        raw.push('=');
    }
    raw
}

/// Header node from Gmail's MIME payload tree.
#[derive(Deserialize)]
pub struct Header {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// Body node from Gmail's MIME payload tree.
#[derive(Deserialize)]
pub struct PartBody {
    #[serde(rename = "attachmentId", default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub data: String,
}

/// Recursive Gmail MIME payload part.
#[derive(Deserialize)]
pub struct Part {
    #[serde(rename = "mimeType", default)]
    pub mime_type: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    pub body: Option<PartBody>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

/// RFC 2047-encode a header value if it isn't plain ASCII (so non-ASCII subjects
/// / display names survive). ASCII passes through unchanged.
fn enc_header(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            rightkit_crypto::base64_encode(s.as_bytes())
        )
    }
}

/// One outgoing attachment from the compose UI: the bytes already base64-encoded
/// in the webview (FileReader). camelCase to match the JS `SendReq.attachments`.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime_type: String,
    pub data_b64: String,
}

/// A random MIME multipart boundary unlikely to occur in any part.
fn mime_boundary() -> String {
    let mut b = [0u8; 12];
    let _ = getrandom::fill(&mut b);
    format!("=_rightkit_{}", rightkit_fs::digest::hex(&b))
}

/// Re-wrap base64 to 76-char CRLF lines (RFC 2045), stripping any existing
/// whitespace the webview may have inserted.
fn wrap_b64(data: &str) -> String {
    let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let mut out = String::with_capacity(clean.len() + clean.len() / 76 + 2);
    let mut i = 0;
    let n = clean.len();
    while i < n {
        let end = (i + 76).min(n);
        out.push_str(&clean[i..end]);
        out.push_str("\r\n");
        i = end;
    }
    out
}

/// Strip control characters, quote and backslash from a filename so it can't
/// inject MIME headers or break out of the quoted string.
fn header_safe_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '"' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Normalize a recipient-list header value: split on commas, trim each entry,
/// drop empties. The compose autocomplete appends a trailing ", " after each
/// pick, which otherwise leaves a dangling address Gmail rejects as an
/// "Invalid Bcc/Cc header" (HTTP 400). Returns "" when nothing valid remains.
fn clean_addrs(s: &str) -> String {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build an RFC 822 message. With no attachments it's a single `text/html`
/// (when `is_html`) or `text/plain` part; with attachments it's `multipart/mixed`
/// (body part + base64 attachment parts). The body is sent verbatim (HTML bodies
/// are DOMPurify-sanitized in the webview first). Bcc is included so Gmail
/// delivers to those recipients; Gmail strips the Bcc header from the copy.
/// `in_reply_to` (the original Message-ID) threads replies for other clients.
#[allow(clippy::too_many_arguments)] // one argument per API field, as the host's call sites pass them
pub fn build_raw_mime(
    from: &str,
    to: &str,
    cc: &str,
    bcc: &str,
    subject: &str,
    body: &str,
    is_html: bool,
    in_reply_to: Option<&str>,
    attachments: &[OutgoingAttachment],
) -> String {
    let to_c = clean_addrs(to);
    let cc_c = clean_addrs(cc);
    let bcc_c = clean_addrs(bcc);
    let mut h = String::new();
    h.push_str(&format!("From: {from}\r\n"));
    if !to_c.is_empty() {
        h.push_str(&format!("To: {to_c}\r\n"));
    }
    if !cc_c.is_empty() {
        h.push_str(&format!("Cc: {cc_c}\r\n"));
    }
    if !bcc_c.is_empty() {
        h.push_str(&format!("Bcc: {bcc_c}\r\n"));
    }
    h.push_str(&format!("Subject: {}\r\n", enc_header(subject)));
    if let Some(mid) = in_reply_to {
        if !mid.trim().is_empty() {
            h.push_str(&format!("In-Reply-To: {mid}\r\n"));
            h.push_str(&format!("References: {mid}\r\n"));
        }
    }
    h.push_str("MIME-Version: 1.0\r\n");
    let ctype = if is_html { "text/html" } else { "text/plain" };

    if attachments.is_empty() {
        h.push_str(&format!("Content-Type: {ctype}; charset=\"UTF-8\"\r\n"));
        h.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        h.push_str(body);
        return h;
    }

    let boundary = mime_boundary();
    h.push_str(&format!(
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
    ));
    // Body part.
    h.push_str(&format!("--{boundary}\r\n"));
    h.push_str(&format!("Content-Type: {ctype}; charset=\"UTF-8\"\r\n"));
    h.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    h.push_str(body);
    h.push_str("\r\n");
    // Attachment parts.
    for a in attachments {
        let mime = if a.mime_type.trim().is_empty() {
            "application/octet-stream"
        } else {
            a.mime_type.trim()
        };
        let fname = header_safe_filename(&a.filename);
        h.push_str(&format!("--{boundary}\r\n"));
        h.push_str(&format!("Content-Type: {mime}; name=\"{fname}\"\r\n"));
        h.push_str("Content-Transfer-Encoding: base64\r\n");
        h.push_str(&format!(
            "Content-Disposition: attachment; filename=\"{fname}\"\r\n\r\n"
        ));
        h.push_str(&wrap_b64(&a.data_b64));
    }
    h.push_str(&format!("--{boundary}--\r\n"));
    h
}
