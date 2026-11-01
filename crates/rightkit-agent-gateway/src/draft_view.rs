//! A Gmail draft as the approval card shows it, the hash that binds a click to
//! that content, and the parser for Gmail's `drafts.get?format=full` JSON.
//! Pure: no I/O. Text written by anyone is plain text here and stays plain text
//! all the way to the card.

use super::draft::DraftRequest;
use super::mime::{decode_b64url, Part};
use rightkit_fs::digest::Sha256Hasher;
use serde::{Deserialize, Serialize};

/// A part of the draft that is not its plain or HTML text: an attachment, or
/// any other part that carries body data. `name` may be empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentInfo {
    pub name: String,
    pub mime_type: String,
    pub size: i64,
}

/// What Gmail holds for a draft right now.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchedDraft {
    pub from: String,
    pub reply_to: String,
    /// Recipient entries exactly as the draft's headers carry them.
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// True when the draft has no plain-text part and `body` is its HTML source.
    pub html_only: bool,
    /// The draft's text/html part (raw source), when it has one.
    pub html: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

/// Content must never reach a log through `{:?}`.
impl std::fmt::Debug for FetchedDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchedDraft")
            .field("counts", &self.counts())
            .finish_non_exhaustive()
    }
}

impl FetchedDraft {
    /// Hash of everything that determines what is sent: the approval binds to it.
    pub fn hash(&self) -> String {
        let mut h = Sha256Hasher::new();
        feed(&mut h, "from", &self.from);
        feed(&mut h, "reply_to", &self.reply_to);
        for (tag, list) in [("to", &self.to), ("cc", &self.cc), ("bcc", &self.bcc)] {
            feed_list(&mut h, tag, list);
        }
        feed(&mut h, "subject", &self.subject);
        feed(&mut h, "plain", &norm_body(&self.plain_text()));
        match &self.html {
            Some(x) => feed(&mut h, "html", &format!("+{}", norm_body(x))),
            None => feed(&mut h, "html", "-"),
        }
        feed(&mut h, "parts", &self.attachments.len().to_string());
        for a in &self.attachments {
            feed(&mut h, "part_name", &a.name);
            feed(&mut h, "part_type", &a.mime_type);
            feed(&mut h, "part_size", &a.size.to_string());
        }
        h.finalize()
    }
    /// Hash over the fields an agent's `draft` request sets, comparable with
    /// [`request_hash`]: tells whether the draft was edited after creation.
    pub fn creation_hash(&self) -> String {
        content_hash(
            &self.to,
            &self.cc,
            &self.bcc,
            &self.subject,
            &self.body,
            &self.attachments,
        )
    }
    /// The plain-text part ("" when the draft only has HTML).
    fn plain_text(&self) -> String {
        if self.html_only {
            String::new()
        } else {
            self.body.clone()
        }
    }
    /// The HTML part as text, when it says something other than the plain part.
    pub fn html_text_if_different(&self) -> Option<String> {
        let t = crate::host::host().html_to_text(self.html.as_deref()?);
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        (squash(&t) != squash(&self.plain_text())).then_some(t)
    }
    /// Counts only, e.g. `to=2 cc=0 bcc=0 body_chars=153`. Safe for the audit log.
    pub fn counts(&self) -> String {
        format!(
            "to={} cc={} bcc={} body_chars={} attachments={}",
            self.to.len(),
            self.cc.len(),
            self.bcc.len(),
            self.body.chars().count(),
            self.attachments.len()
        )
    }
}

fn norm_body(s: &str) -> String {
    s.replace("\r\n", "\n").trim_end().to_string()
}

fn feed(h: &mut Sha256Hasher, tag: &str, s: &str) {
    h.update(tag.as_bytes());
    h.update(&(s.len() as u64).to_be_bytes());
    h.update(s.as_bytes());
}

fn feed_list(h: &mut Sha256Hasher, tag: &str, list: &[String]) {
    feed(h, tag, &list.len().to_string());
    for a in list {
        feed(h, tag, a);
    }
}

/// SHA-256 (hex) over recipients, subject, body and each attachment's name,
/// type and size, each field length-prefixed so no two different drafts share
/// an input. A draft hashes the same whether built from a validated request
/// or read back from Gmail. The bytes themselves are not in this hash. A digest
/// of them (`draft_files::digest`) is stored beside it for the record, but
/// nothing compares it at approve, so the approval does not bind the bytes.
pub fn content_hash(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    attachments: &[AttachmentInfo],
) -> String {
    let mut h = Sha256Hasher::new();
    for (tag, list) in [("to", to), ("cc", cc), ("bcc", bcc)] {
        feed_list(&mut h, tag, list);
    }
    feed(&mut h, "subject", subject);
    feed(&mut h, "body", &norm_body(body));
    if !attachments.is_empty() {
        feed(&mut h, "attachments", &attachments.len().to_string());
        for a in attachments {
            feed(&mut h, "attachment", &a.name);
            feed(&mut h, "attachment_type", &a.mime_type);
            feed(&mut h, "attachment_size", &a.size.to_string());
        }
    }
    h.finalize()
}

/// The hash recorded when the agent's `draft` request was validated (the
/// attachments, if any, already read into the request).
pub fn request_hash(r: &DraftRequest) -> String {
    let files = super::draft_files::infos(&r.attachments);
    content_hash(&r.to, &r.cc, &r.bcc, &r.subject, &r.body, &files)
}

#[derive(Deserialize)]
struct DraftMsg {
    #[serde(default)]
    id: String,
    payload: Option<Part>,
}

#[derive(Deserialize)]
struct DraftResp {
    message: Option<DraftMsg>,
}

fn header(p: &Part, name: &str) -> Option<String> {
    p.headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.clone())
}

/// Split a recipient header on commas that are outside quotes and angle brackets.
fn split_list(v: &str) -> Vec<String> {
    let (mut out, mut cur) = (Vec::new(), String::new());
    let (mut quoted, mut angle) = (false, false);
    for c in v.chars() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' if !quoted && !angle => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Decode RFC 2047 UTF-8 encoded words (`=?UTF-8?B?..?=`, `=?UTF-8?Q?..?=`).
/// Anything else is left as written.
fn decode_words(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("=?") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 2..];
        let decoded = tail.find("?=").and_then(|end| {
            let mut it = tail[..end].splitn(3, '?');
            let (cs, enc, text) = (it.next()?, it.next()?, it.next()?);
            if !cs.eq_ignore_ascii_case("utf-8") {
                return None;
            }
            let bytes = if enc.eq_ignore_ascii_case("b") {
                rightkit_crypto::base64_decode(text).ok()?
            } else if enc.eq_ignore_ascii_case("q") {
                q_decode(text)?
            } else {
                return None;
            };
            Some((String::from_utf8(bytes).ok()?, end + 2))
        });
        match decoded {
            Some((text, used)) => {
                out.push_str(&text);
                rest = &tail[used..];
            }
            None => {
                out.push_str("=?");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn q_decode(t: &str) -> Option<Vec<u8>> {
    let b = t.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'_' => out.push(b' '),
            b'=' => {
                let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    Some(out)
}

fn walk(
    p: &Part,
    plain: &mut Option<String>,
    html: &mut Option<String>,
    files: &mut Vec<AttachmentInfo>,
) {
    let mt = p.mime_type.to_ascii_lowercase();
    let data = p.body.as_ref().filter(|b| !b.data.is_empty());
    let has_id = p.body.as_ref().is_some_and(|b| b.attachment_id.is_some());
    let text = || data.map(|b| String::from_utf8_lossy(&decode_b64url(&b.data)).into_owned());
    if !p.filename.is_empty() || has_id {
        files.push(part_info(p, decode_words(&p.filename), &mt));
    } else if let Some(t) = text() {
        if mt == "text/plain" && plain.is_none() {
            *plain = Some(t);
        } else if mt == "text/html" && html.is_none() {
            *html = Some(t);
        } else {
            files.push(part_info(p, String::new(), &mt));
        }
    }
    for c in &p.parts {
        walk(c, plain, html, files);
    }
}

fn part_info(p: &Part, name: String, mime: &str) -> AttachmentInfo {
    let size = p.body.as_ref().map_or(0, |b| {
        b.size
            .unwrap_or_else(|| decode_b64url(&b.data).len() as i64)
    });
    AttachmentInfo {
        name,
        mime_type: mime.to_string(),
        size,
    }
}

/// Parse a `drafts.get?format=full` response. `None` when it has no message.
pub fn parse_gmail_draft(json: &serde_json::Value) -> Option<(String, FetchedDraft)> {
    let resp: DraftResp = serde_json::from_value(json.clone()).ok()?;
    let msg = resp.message?;
    let payload = msg.payload?;
    let list = |name: &str| -> Vec<String> {
        header(&payload, name)
            .map(|v| split_list(&v).iter().map(|s| decode_words(s)).collect())
            .unwrap_or_default()
    };
    let (mut plain, mut html, mut attachments) = (None, None, Vec::new());
    walk(&payload, &mut plain, &mut html, &mut attachments);
    let html_only = plain.is_none() && html.is_some();
    let draft = FetchedDraft {
        from: header(&payload, "From")
            .map(|v| decode_words(&v))
            .unwrap_or_default(),
        reply_to: header(&payload, "Reply-To")
            .map(|v| decode_words(&v))
            .unwrap_or_default(),
        to: list("To"),
        cc: list("Cc"),
        bcc: list("Bcc"),
        subject: header(&payload, "Subject")
            .map(|s| decode_words(&s))
            .unwrap_or_default(),
        body: plain.or_else(|| html.clone()).unwrap_or_default(),
        html_only,
        html,
        attachments,
    };
    Some((msg.id, draft))
}
