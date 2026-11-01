//! Payload shapes for content records (desktop store): reusable media with stable occurrence ids,
//! per-account cooldowns, expiry, withdrawal, and caption and CTA variants for experiments. A record is
//! keyed by venture, channel and account, like a draft. Nothing here posts. The engine stores records,
//! plans occurrences and refuses cooldown, expiry and withdrawal conflicts; the publisher marks an
//! occurrence published once the platform accepts it.
//!
//! `expired` is a read-time state. It is derived from the stored record and the clock (`now`), so no
//! sweep writes it. Stored records are only `active` or `withdrawn`.
use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;

/// Media kinds a record may list.
pub const CONTENT_MEDIA_KINDS: [&str; 2] = ["image", "video"];

/// Record states as returned. `expired` is derived at read time (`now` is past `expires_at`).
pub const CONTENT_STATES: [&str; 3] = ["active", "withdrawn", "expired"];

/// Variant kinds. A caption variant changes the text; a CTA variant changes the call to action.
pub const CONTENT_VARIANT_KINDS: [&str; 2] = ["caption", "cta"];

/// Occurrence states. `cancelled` is set by `content.cancel_occurrence` or by withdrawal.
pub const OCCURRENCE_STATES: [&str; 3] = ["planned", "published", "cancelled"];

/// One media file a record reuses. `hash` identifies the bytes; the record never stores the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentMediaItem {
    pub hash: String,
    pub kind: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// One stored record, as every record command returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRecord {
    pub scope: Scope,
    pub content_id: String,
    pub title: String,
    pub media: Vec<ContentMediaItem>,
    pub base_caption: String,
    /// Recorded for the operator. This slice gates nothing on it.
    pub evergreen: bool,
    /// Days between two non-cancelled occurrences of this record on the same account and platform.
    pub cooldown_days: i64,
    /// Last UTC instant the record may run, `YYYY-MM-DDTHH:MM:SSZ`. Null when it never expires.
    pub expires_at: Option<String>,
    /// `active`, `withdrawn` or `expired` (derived from the clock at read time).
    pub state: String,
    /// Required reason, set only when withdrawn.
    pub withdrawal_reason: Option<String>,
    /// Compare-and-swap revision. Put, withdraw and add_variant bump it.
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One caption or CTA variant of a record. Variants never change once written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentVariant {
    pub scope: Scope,
    pub variant_id: String,
    pub content_id: String,
    pub kind: String,
    pub text: String,
    pub created_at: String,
}

/// One planned, published or cancelled use of a record on a platform. `occurrence_id` is stable for
/// (content id, platform, scheduled_at), so planning the same key twice returns the same occurrence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentOccurrence {
    pub scope: Scope,
    pub occurrence_id: String,
    pub content_id: String,
    pub platform: String,
    pub calendar_entry_id: Option<String>,
    pub caption_variant_id: Option<String>,
    pub cta_variant_id: Option<String>,
    /// UTC instant the occurrence is due, `YYYY-MM-DDTHH:MM:SSZ`.
    pub scheduled_at: String,
    pub state: String,
    /// Set only in `published`.
    pub published_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// `content.put` input. Creates the record when it does not exist (`expected_revision` absent or 0);
/// otherwise `expected_revision` must equal the stored revision. A withdrawn record cannot be edited.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentPutInput {
    pub scope: Scope,
    pub content_id: String,
    pub title: String,
    pub media: Vec<ContentMediaItem>,
    #[serde(default)]
    pub base_caption: String,
    #[serde(default)]
    pub evergreen: bool,
    /// Zero or more. Negative is `bad_input`.
    #[serde(default)]
    pub cooldown_days: i64,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.get` input. `now` fixes the clock for the derived `expired` state.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentGetInput {
    pub scope: Scope,
    pub content_id: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.get` output: the record and its variants, caption variants first, each in creation order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentGetOutput {
    pub record: ContentRecord,
    pub variants: Vec<ContentVariant>,
}

/// `content.list` input. `state` is an optional filter over the derived state.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentListInput {
    pub scope: Scope,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.list` output, ordered by content id ascending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentListOutput {
    pub scope: Scope,
    pub records: Vec<ContentRecord>,
}

/// `content.withdraw` input. `reason` is required. Planned occurrences become cancelled in the same
/// transaction.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentWithdrawInput {
    pub scope: Scope,
    pub content_id: String,
    pub expected_revision: i64,
    pub reason: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.withdraw` output: the withdrawn record and the occurrence ids cancelled with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentWithdrawOutput {
    pub record: ContentRecord,
    pub cancelled_occurrences: Vec<String>,
}

/// `content.add_variant` input. Bumps the record revision, so `expected_revision` is required.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentAddVariantInput {
    pub scope: Scope,
    pub content_id: String,
    pub expected_revision: i64,
    pub variant_id: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.add_variant` output: the record at its new revision and the stored variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentAddVariantOutput {
    pub record: ContentRecord,
    pub variant: ContentVariant,
}

/// `content.plan_occurrence` input. `scheduled_at` is a UTC instant. Variant ids, when given, must be
/// variants of this record with the matching kind.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentPlanOccurrenceInput {
    pub scope: Scope,
    pub content_id: String,
    pub platform: String,
    pub scheduled_at: String,
    #[serde(default)]
    pub caption_variant_id: Option<String>,
    #[serde(default)]
    pub cta_variant_id: Option<String>,
    #[serde(default)]
    pub calendar_entry_id: Option<String>,
    /// On a cooldown conflict, plan at the earliest allowed time instead of refusing (`cooldown_active`).
    /// Null or false keeps the refusal. A repeat with the same `calendar_entry_id` returns the occurrence.
    #[serde(default)]
    pub reschedule_on_cooldown: Option<bool>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.plan_occurrence` output. `created` is false when the same key was already planned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPlanOccurrenceOutput {
    pub occurrence: ContentOccurrence,
    pub created: bool,
}

/// `content.cancel_occurrence` input. Only a planned occurrence can be cancelled.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentCancelOccurrenceInput {
    pub scope: Scope,
    pub occurrence_id: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.mark_published` input. Only a planned occurrence can be marked published. `published_at`
/// defaults to `now`.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentMarkPublishedInput {
    pub scope: Scope,
    pub occurrence_id: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.next_allowed` input.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentNextAllowedInput {
    pub scope: Scope,
    pub content_id: String,
    pub platform: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.next_allowed` output. `earliest_allowed_at` is null with a `reason` when the record is
/// withdrawn (`content_withdrawn`) or the earliest time is past expiry (`content_expired`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentNextAllowedOutput {
    pub scope: Scope,
    pub content_id: String,
    pub platform: String,
    pub now: String,
    pub state: String,
    pub earliest_allowed_at: Option<String>,
    pub reason: Option<String>,
}

/// `content.experiment_summary` input. `min_occurrences` defaults to 20 and must be at least 1.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentExperimentSummaryInput {
    pub scope: Scope,
    pub content_id: String,
    #[serde(default)]
    pub min_occurrences: Option<i64>,
    /// The clock for the 24-hour rule of a declared primary metric. Defaults to the current time.
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.declare_metric` input: names the one primary metric that may decide a record's experiment.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentDeclareMetricInput {
    pub scope: Scope,
    pub content_id: String,
    pub metric: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `content.declare_metric` output. `created` is false when the same metric was already declared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentDeclareMetricOutput {
    pub scope: Scope,
    pub content_id: String,
    pub metric: String,
    pub window: String,
    pub declared_at: String,
    pub created: bool,
}

/// Plain counts for one stored variant. Only `published` counts toward the minimum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantCount {
    pub variant_id: String,
    pub text: String,
    pub planned: i64,
    pub published: i64,
    pub cancelled: i64,
    /// Published occurrences whose primary metric value was compared (a declared metric only; else 0).
    pub compared: i64,
    /// The exact sum of the compared values, as decimal text (a declared metric only; else "0").
    pub total: String,
    pub meets_minimum: bool,
}

/// `content.experiment_summary` output. `verdict` is `insufficient_evidence` whenever a compared
/// variant is below the minimum, or fewer than two variants of a kind exist. It is `evidence_sufficient`
/// only when every compared variant meets the minimum, and even then no winner is named in this slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentExperimentSummaryOutput {
    pub scope: Scope,
    pub content_id: String,
    pub min_occurrences: i64,
    /// `winner` when a declared primary metric names one strict maximum in every compared kind; `evidence_sufficient`
    /// when no metric is declared and counts suffice (no winner); else `insufficient_evidence`.
    pub verdict: String,
    pub reason: String,
    /// The primary metric declared for the record, or null.
    pub primary_metric: Option<String>,
    pub caption_winner: Option<String>,
    pub cta_winner: Option<String>,
    pub caption: Vec<VariantCount>,
    pub cta: Vec<VariantCount>,
}
