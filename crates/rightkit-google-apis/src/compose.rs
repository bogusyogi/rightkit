//! A message to send or draft, after cleaning: the MIME text, the digest an approval binds to, and
//! the sentence the owner is shown. Plus the plain checks the tools that take agent arguments share
//! (Gmail ids, bare addresses, all-day dates).
//!
//! * No `From`: Gmail sets the signed-in account.
//! * A reply takes its thread and Message-Id from Gmail, never from the caller
//!   ([`reply_header`]).
//! * The bind covers every field, so changing a word asks again.

use serde_json::json;

use crate::gmail;

/// A Gmail id or label id: letters, digits, `_` and `-`, at most 64.
pub fn plain_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A bare address: `name@host.tld` with plain characters. No display name, no angle brackets.
pub fn valid_address(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
    };
    text.len() <= 254
        && !local.is_empty()
        && plain(local)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && plain(domain)
}

/// `YYYY-MM-DD`, a real-looking date.
pub fn date_only(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && matches!(text[5..7].parse::<u8>(), Ok(1..=12))
        && matches!(text[8..10].parse::<u8>(), Ok(1..=31))
}

/// The `In-Reply-To` header value for a Gmail message's own Message-Id: angle-bracketed, and
/// `None` when it is empty or carries a control character or whitespace.
pub fn reply_header(rfc822_message_id: Option<String>) -> Option<String> {
    rfc822_message_id
        .filter(|m| !m.is_empty() && !m.chars().any(|c| c.is_control() || c.is_whitespace()))
        .map(|m| format!("<{m}>"))
}

/// A SHA-256 hex digest of `tool:payload`, the digest an approval binds.
pub fn bind_digest(tool: &str, payload: &serde_json::Value) -> String {
    let text = format!("{tool}:{payload}");
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(text.as_bytes()))
}

/// What would be sent or drafted, after cleaning.
#[derive(Clone, Debug)]
pub struct Composed {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// For a reply: Gmail's thread id and the original's Message-Id header value, both read from Gmail.
    pub thread_id: Option<String>,
    pub in_reply_to: Option<String>,
}

impl Composed {
    /// The raw MIME text, with no `From`.
    pub fn raw(&self) -> String {
        let raw = gmail::build_raw_mime(
            "",
            &self.to.join(", "),
            &self.cc.join(", "),
            &self.bcc.join(", "),
            &self.subject,
            &self.body.replace('\n', "\r\n"),
            false,
            self.in_reply_to.as_deref(),
            &[],
        );
        // No `From`: Gmail sets the signed-in account.
        raw.strip_prefix("From: \r\n").unwrap_or(&raw).to_string()
    }

    /// The digest an approval binds: the exact effect, field by field. `tool` is the name of the
    /// tool that sends it.
    pub fn bind(&self, tool: &str) -> String {
        let payload = json!({
            "to": self.to, "cc": self.cc, "bcc": self.bcc,
            "subject": self.subject, "body": self.body,
            "threadId": self.thread_id, "inReplyTo": self.in_reply_to,
        });
        bind_digest(tool, &payload)
    }

    /// What the owner is shown: who, what about, and the whole text.
    pub fn summary(&self, tool: &str) -> String {
        // The whole text as it is written (line breaks shown as " / "): the owner approves exactly what the recipient will read, on the
        // notch, the hub and the phone alike. A text too long to show whole is refused before an approval is asked.
        let lines: Vec<&str> = self
            .body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let shown = lines.join(" / ");
        let fingerprint: String = self.bind(tool).chars().take(8).collect();
        let also = |label: &str, list: &[String]| {
            if list.is_empty() {
                String::new()
            } else {
                format!(" {label} {}", list.join(", "))
            }
        };
        format!(
            "send this email to {}{}{} with the subject \"{}\". It says: {shown}. Message fingerprint {fingerprint}: the approval is for exactly this message, and any change asks again.",
            self.to.join(", "),
            also("and copy", &self.cc),
            also("and blind copy", &self.bcc),
            self.subject
        )
    }
}
