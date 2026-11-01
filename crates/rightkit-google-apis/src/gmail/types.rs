#[derive(Debug, Clone)]
pub struct FetchedAttachment {
    pub attachment_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
}

#[derive(Debug, Clone)]
pub struct FetchedMessage {
    pub id: String,
    pub thread_id: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: String,
    pub cc_addrs: String,
    pub reply_to: Option<String>,
    pub list_unsubscribe: Option<String>,
    /// Raw `Authentication-Results` header (SPF/DKIM/DMARC), if present.
    pub auth_results: Option<String>,
    pub subject: String,
    pub snippet: String,
    pub internal_date: i64,
    pub unread: bool,
    pub label_ids: Vec<String>,
    pub body_plain: Option<String>,
    pub body_html: Option<String>,
    pub has_attachments: bool,
    pub attachments: Vec<FetchedAttachment>,
    /// Gmail's `sizeEstimate` (approximate RFC 2822 byte size), top-level on
    /// the message resource for both `format=metadata` and `format=full`.
    /// None only when Gmail omits the field (never fabricated).
    pub size_estimate: Option<i64>,
    /// The RFC 5322 `Message-Id` header, angle brackets stripped. None when
    /// the header is absent (rare but not impossible) — never fabricated.
    pub rfc822_message_id: Option<String>,
}

/// One message that failed to fetch during a sync. `reason` is a short error
/// class only — never a body or token (safety rail #10).
#[derive(Debug, Clone)]
pub struct SkipInfo {
    pub id: String,
    pub reason: String,
}

/// Result of a recent-inbox sync: the messages fetched plus the ones skipped, so
/// the caller can surface partial failure instead of swallowing it.
#[derive(Clone)]
pub struct SyncOutcome {
    pub messages: Vec<FetchedMessage>,
    pub skipped: Vec<SkipInfo>,
}

impl SyncOutcome {
    /// Compact, body-free summary of skipped messages for the audit log: the
    /// count plus up to 10 `id:reason_class` pairs. Gmail message ids are opaque
    /// identifiers, not content, so they are safe to record (safety rail #10).
    pub fn skip_summary(&self) -> String {
        if self.skipped.is_empty() {
            return String::new();
        }
        let sample: Vec<String> = self
            .skipped
            .iter()
            .take(10)
            .map(|s| format!("{}:{}", s.id, s.reason))
            .collect();
        format!(", skipped {} [{}]", self.skipped.len(), sample.join(", "))
    }
}

/// A Gmail label — system (INBOX/SENT/…) or user/custom (e.g. "Orders"). The
/// full catalog is stored so folder names are available; custom-label UI is
/// deferred but the data is kept rather than thrown away.
#[derive(Debug, Clone)]
pub struct LabelInfo {
    pub id: String,
    pub name: String,
    pub label_type: String,    // "system" | "user"
    pub color: Option<String>, // user label background color hex from Gmail, if set
}

/// Result of a full multi-folder sync: the fetched messages (+ skips), the
/// account's complete label catalog, and the current history cursor (the
/// baseline for the next delta sync).
#[derive(Clone)]
pub struct SyncResult {
    pub outcome: SyncOutcome,
    pub labels: Vec<LabelInfo>,
    pub history_id: Option<String>,
}

/// What a sync produced — a full snapshot or an incremental delta. Handed to
/// db::apply_sync_work.
#[derive(Clone)]
pub enum SyncWork {
    Full(SyncResult),
    Delta(DeltaOutcome),
}

/// Outcome of a History API delta: messages to (re)fetch in full, ids that were
/// deleted, and the new cursor. `stale` = the startHistoryId was too old (Gmail
/// 404) and the caller must fall back to a full sync.
#[derive(Clone)]
pub struct DeltaOutcome {
    pub changed: Vec<FetchedMessage>,
    pub deleted: Vec<String>,
    pub new_history_id: Option<String>,
    /// The latest cursor returned by Gmail, even when a fetch failure prevents
    /// normal advancement. The DB layer may use this after repeated retries so
    /// one permanently bad message cannot pin the whole account forever.
    pub latest_history_id: Option<String>,
    pub stale: bool,
    /// Count of changed ids whose full fetch failed (non-404). When > 0,
    /// `new_history_id` is forced to None so the cursor is NOT advanced and the
    /// next delta re-attempts them — otherwise an inbound message could be lost.
    pub failed: usize,
    pub failed_ids: Vec<String>,
}
