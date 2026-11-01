//! Payload shapes for manual drafts: text the operator posts by hand. A draft is keyed
//! by venture, channel, account and draft id, and carries its platform as a plain string, because the
//! draft-only platform `reddit` is not in the account vocabulary (`PLATFORMS`). Nothing here posts;
//! the engine stores drafts and checks them, and the operator records the link once posted.
use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;

/// Draft kinds. `comment` and `reply` need a target URL to approve.
pub const DRAFT_KINDS: [&str; 3] = ["post", "comment", "reply"];

/// Draft states. `posted_by_operator` is set only by `draft.mark_posted` and always carries a posted URL.
pub const DRAFT_STATES: [&str; 5] = [
    "drafted",
    "approved",
    "posted_by_operator",
    "skipped",
    "stale",
];

/// Platforms that exist only as drafts: the host app never posts to them. Accepted in addition to the
/// account vocabulary (`PLATFORMS`), which account validation keeps unchanged.
pub const DRAFT_ONLY_PLATFORMS: [&str; 1] = ["reddit"];

/// One stored draft, as every draft command returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualDraft {
    pub scope: Scope,
    pub draft_id: String,
    pub platform: String,
    pub kind: String,
    /// Community or thread URL. Null for a new post with no target.
    pub target: Option<String>,
    pub title: Option<String>,
    pub body: String,
    /// Unicode scalar values in `body`.
    pub body_chars: u64,
    pub disclosure_required: bool,
    pub state: String,
    /// Compare-and-swap revision. Every write bumps it.
    pub revision: i64,
    /// The link the operator recorded. Set only in `posted_by_operator`.
    pub posted_url: Option<String>,
    /// UTC instant the operator recorded the post, `YYYY-MM-DDTHH:MM:SSZ`. Set only when posted.
    pub posted_at: Option<String>,
    /// Required reason, set only in `skipped`.
    pub skip_reason: Option<String>,
    /// Free text: why this draft exists.
    pub source_note: String,
    pub created_at: String,
    pub updated_at: String,
}

/// `draft.put` input. Creates the draft when it does not exist (`expected_revision` absent or 0);
/// otherwise `expected_revision` must equal the stored revision. Editing an approved draft returns it
/// to `drafted`. Skipped, stale and posted drafts cannot be edited.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftPutInput {
    pub scope: Scope,
    pub draft_id: String,
    pub platform: String,
    pub kind: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub disclosure_required: bool,
    #[serde(default)]
    pub source_note: String,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `draft.get` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftGetInput {
    pub scope: Scope,
    pub draft_id: String,
}

/// `draft.list` input. `state` and `platform` are optional filters.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftListInput {
    pub scope: Scope,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
}

/// `draft.approve` input. The gate checks the body, the target, the optional length limit and the
/// disclosure. `disclosure_phrases` is supplied by the caller: the engine has no built-in list.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftApproveInput {
    pub scope: Scope,
    pub draft_id: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub disclosure_phrases: Vec<String>,
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `draft.mark_posted` input. `posted_url` must be an https URL on an allowed host for the platform.
/// `posted_at` defaults to `now`.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftMarkPostedInput {
    pub scope: Scope,
    pub draft_id: String,
    pub expected_revision: i64,
    pub posted_url: String,
    #[serde(default)]
    pub posted_at: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `draft.skip` input. `reason` is required.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftSkipInput {
    pub scope: Scope,
    pub draft_id: String,
    pub expected_revision: i64,
    pub reason: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `draft.mark_stale` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftMarkStaleInput {
    pub scope: Scope,
    pub draft_id: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub now: Option<String>,
}

/// `draft.list` output, ordered by updated_at descending, then draft id ascending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftListOutput {
    pub scope: Scope,
    pub drafts: Vec<ManualDraft>,
}

/// `draft.mark_posted` output. `host_check` is `table` when the host matched the platform's allowed
/// hosts, or `any_https` when the platform has no host table; `host_note` explains the second case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftMarkPostedOutput {
    pub draft: ManualDraft,
    pub host_check: String,
    pub host_note: Option<String>,
}
