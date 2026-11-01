//! Payload shapes for briefs and calendar entries (desktop cache and proposal store). Every row is
//! keyed by venture, channel and account, like channels and proofs. Times are RFC 3339 UTC strings
//! ending in `Z`; a local wall time is entered with its IANA zone name.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use rightkit_social_publish::protocol::Scope;

/// States an entry can be stored in. `late` is never stored: `calendar.list` and `calendar.get`
/// report it as `effective_state` when a draft or media_pending entry passes its preparation deadline.
pub const ENTRY_STATES: [&str; 8] = [
    "draft",
    "media_pending",
    "ready",
    "scheduled",
    "late",
    "paused",
    "published",
    "failed",
];
/// States `calendar.put` accepts. `paused` is set only by `calendar.pause`; `late` is derived.
pub const PUT_STATES: [&str; 6] = [
    "draft",
    "media_pending",
    "ready",
    "scheduled",
    "published",
    "failed",
];
/// Approval states the store holds. `pending` is set by `calendar.request_approval`.
pub const APPROVAL_STATES: [&str; 5] = ["none", "pending", "approved", "rejected", "invalidated"];

/// One media item attached to an entry. `hash` is the content hash of the file (blake3 hex when
/// the media pipeline writes it; the store does not check the format).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaItem {
    pub hash: String,
    pub kind: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// `brief.put` input. Creates the next revision; an identical latest revision is a no-op.
#[derive(Debug, Clone, Deserialize)]
pub struct BriefPutInput {
    pub scope: Scope,
    pub brief_id: String,
    pub brief_type: String,
    pub title: String,
    pub payload: Value,
    #[serde(default)]
    pub now: Option<String>,
}

/// `brief.get` input. Without `revision`, the latest revision is returned.
#[derive(Debug, Clone, Deserialize)]
pub struct BriefGetInput {
    pub scope: Scope,
    pub brief_id: String,
    #[serde(default)]
    pub revision: Option<i64>,
}

/// `brief.put` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefPutOutput {
    pub scope: Scope,
    pub brief_id: String,
    pub revision: i64,
    /// True when the payload matched the latest revision and nothing was written.
    pub unchanged: bool,
}

/// One stored brief revision. Revisions are immutable once written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefRecord {
    pub scope: Scope,
    pub brief_id: String,
    pub revision: i64,
    pub brief_type: String,
    pub title: String,
    pub payload: Value,
    pub created_at: String,
}

/// `calendar.put` input: create (`expected_revision` absent or 0) or update (`expected_revision`
/// must equal the stored revision). `local_time` is the wall time as entered, `YYYY-MM-DDTHH:MM`
/// or with `:SS`, in `timezone`. `fold` (`earlier` or `later`) is required only for an ambiguous time.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarPutInput {
    pub scope: Scope,
    pub entry_id: String,
    pub brief_id: String,
    pub brief_revision: i64,
    pub platform: String,
    pub format: String,
    pub caption: String,
    #[serde(default)]
    pub media: Vec<MediaItem>,
    pub timezone: String,
    pub local_time: String,
    #[serde(default)]
    pub fold: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
    /// The first comment posted under the entry, when the platform supports one. Absent is none. It is
    /// approval-bound, like the caption.
    #[serde(default)]
    pub first_comment: Option<String>,
    /// Platform setting labels the operator chose (for example `comments_off`). Absent is the empty list. They are
    /// approval-bound, like the caption.
    #[serde(default)]
    pub settings: Vec<String>,
}

/// `calendar.get` input. `now` and `prep_minutes` only drive `effective_state`; `min_spacing_minutes` only drives
/// `conflict` (default 15; 0 turns the spacing check off).
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarGetInput {
    pub scope: Scope,
    pub entry_id: String,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(default)]
    pub prep_minutes: Option<i64>,
    #[serde(default)]
    pub min_spacing_minutes: Option<i64>,
}

/// `calendar.list` input. `from` is inclusive and `to` exclusive, both UTC instants. Ordered by
/// `scheduled_at`, then entry id.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarListInput {
    pub scope: Scope,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(default)]
    pub prep_minutes: Option<i64>,
    #[serde(default)]
    pub min_spacing_minutes: Option<i64>,
}

/// `calendar.approve` input. Approves exactly `expected_revision`; a different stored revision is
/// `revision_conflict`.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarApproveInput {
    pub scope: Scope,
    pub entry_id: String,
    pub expected_revision: i64,
    pub approved_by: String,
    pub policy_revision: i64,
    #[serde(default)]
    pub now: Option<String>,
}

/// `calendar.reject` input.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarRejectInput {
    pub scope: Scope,
    pub entry_id: String,
    pub expected_revision: i64,
    pub reason: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `calendar.request_approval` input. Marks exactly `expected_revision` as waiting for a decision
/// (`approval_state` `pending`). A different stored revision is `revision_conflict`.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarRequestApprovalInput {
    pub scope: Scope,
    pub entry_id: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub now: Option<String>,
}

/// `calendar.pause` and `calendar.resume` input. `expected_revision` is optional; when present it
/// must match the stored revision.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarStateInput {
    pub scope: Scope,
    pub entry_id: String,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
}

/// DST kinds an entry reports. `nonexistent_moved` is part of the desktop's vocabulary; the engine refuses a
/// wall time that does not exist (`time_nonexistent`), so it never stores or reports that kind.
pub const DST_KINDS: [&str; 3] = ["none", "ambiguous_fires_once", "nonexistent_moved"];

/// The daylight-saving note of an entry, derived from its zone and wall time. Never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DstNote {
    pub kind: String,
    /// Plain text for the operator; absent when `kind` is `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Default for DstNote {
    fn default() -> Self {
        DstNote {
            kind: "none".to_string(),
            note: None,
        }
    }
}

/// Another entry on the same account and platform scheduled closer than the minimum spacing. Derived on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpacingConflict {
    pub with_entry_id: String,
    pub minimum_spacing_minutes: i64,
}

/// `engine.clock` output: the engine's own clock, for a desktop that should not trust the browser's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineClock {
    /// UTC instant, `YYYY-MM-DDTHH:MM:SSZ`.
    pub now_utc: String,
    pub unix_secs: i64,
    /// Always `system`: the engine process's system clock.
    pub source: String,
}

/// One stored calendar entry, as every calendar command returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEntry {
    pub scope: Scope,
    pub entry_id: String,
    pub brief_id: String,
    pub brief_revision: i64,
    pub platform: String,
    pub format: String,
    pub caption: String,
    pub media: Vec<MediaItem>,
    /// UTC instant, `YYYY-MM-DDTHH:MM:SSZ`.
    pub scheduled_at: String,
    pub timezone: String,
    pub local_time: String,
    /// The stored state.
    pub state: String,
    /// `late` when a draft or media_pending entry is past its preparation deadline; otherwise `state`.
    pub effective_state: String,
    /// The state `calendar.resume` returns to, set while paused.
    pub paused_from: Option<String>,
    pub revision: i64,
    pub approval_state: String,
    pub approved_entry_revision: Option<i64>,
    pub approved_by: Option<String>,
    pub policy_revision: Option<i64>,
    pub rejection_reason: Option<String>,
    pub updated_at: String,
    /// The first comment, or null.
    #[serde(default)]
    pub first_comment: Option<String>,
    /// Platform setting labels; empty when none.
    #[serde(default)]
    pub settings: Vec<String>,
    /// Daylight-saving note derived from `timezone` and `local_time` (`kind` `none` when there is nothing to say).
    #[serde(default)]
    pub dst: DstNote,
    /// The nearest entry on the same account and platform closer than the minimum spacing, or null.
    #[serde(default)]
    pub conflict: Option<SpacingConflict>,
}

/// `calendar.list` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarListOutput {
    pub scope: Scope,
    pub entries: Vec<CalendarEntry>,
}
