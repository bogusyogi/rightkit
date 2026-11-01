//! Payload shapes for the publish core: durable publish jobs and the commands that drive them. These
//! commands run only in the host app's development daemon role (the desktop role is
//! refused with `role_forbidden`). Tokens arrive in the command input, are used for the request and
//! are never stored, logged or returned.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Scope;

/// Platforms the publish core can drive. The mock platform implements exactly these.
pub const PUBLISH_PLATFORMS: [&str; 10] = [
    "instagram",
    "facebook",
    "threads",
    "x",
    "linkedin",
    "youtube",
    "bluesky",
    "mastodon",
    "tiktok",
    "pinterest",
];

/// Job states. `published`, `failed` and `cancelled` are terminal. `outcome_unknown` is terminal for
/// `publish.step` and `publish.run` and is left only by `publish.reconcile` or a person's `publish.resolve_unknown`.
pub const PUBLISH_STATES: [&str; 7] = [
    "queued",
    "running",
    "waiting",
    "published",
    "failed",
    "outcome_unknown",
    "cancelled",
];

/// The two decisions a person can record for an `outcome_unknown` job (`publish.resolve_unknown`).
pub const PUBLISH_RESOLUTIONS: [&str; 2] = ["posted", "not_posted"];

/// What a person recorded for a job that was `outcome_unknown`: they looked at the platform and said
/// whether the post is there. Set only by `publish.resolve_unknown`; absent on every other job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishResolution {
    /// `posted` or `not_posted`.
    pub decision: String,
    /// The human author who recorded it (`adrian`).
    pub resolved_by: String,
    /// The instant the resolution was recorded (UTC).
    pub resolved_at: String,
    /// What the person checked.
    pub note: String,
    /// True when `remote.post_id` was typed by the person, not read from a platform answer.
    pub remote_id_entered_by_hand: bool,
}

/// One media item in a job payload. `url` is where the platform fetches the file; the object-storage
/// upload that produces it is not built, so it comes from the enqueue input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishMedia {
    pub hash: String,
    pub kind: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

/// What the job posts, frozen at enqueue from the approved calendar entry revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishPayload {
    pub caption: String,
    pub format: String,
    #[serde(default)]
    pub media: Vec<PublishMedia>,
    #[serde(default)]
    pub settings: Value,
}

/// Remote ids recorded after each successful phase.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PublishRemote {
    pub container_id: Option<String>,
    pub post_id: Option<String>,
    pub permalink: Option<String>,
}

/// One durable publish job, as every publish command returns it. Holds no token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishJob {
    pub job_id: String,
    pub scope: Scope,
    /// The platform's own account id (the path segment on the platform API).
    pub remote_account: String,
    pub platform: String,
    pub entry_id: String,
    /// The calendar entry revision this job was approved under.
    pub entry_revision: i64,
    pub payload: PublishPayload,
    pub idempotency_key: String,
    pub phase: String,
    pub remote: PublishRemote,
    /// Consecutive transient failures (429, 5xx on a read, connection refused) in the current phase.
    pub attempts: i64,
    /// Container status polls made.
    pub polls: i64,
    /// Times the job was sent back to a fresh start: by `publish.reconcile` returning it to `queued`, or (Instagram)
    /// by the machine replacing a container that expired while it was polled.
    pub reconciles: i64,
    /// Set while a write request is in flight, cleared when its outcome is stored.
    pub inflight: Option<String>,
    pub next_attempt_at: Option<String>,
    pub state: String,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
    /// Set when a person closed an `outcome_unknown` job with `publish.resolve_unknown`.
    #[serde(default)]
    pub resolution: Option<PublishResolution>,
}

/// `publish.enqueue` input. The entry must be `approved` at exactly its current revision.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishEnqueueInput {
    pub scope: Scope,
    pub entry_id: String,
    /// The platform's account id for this channel account.
    pub remote_account: String,
    /// Media content hash to the URL the platform fetches it from.
    #[serde(default)]
    pub media_urls: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub now: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishEnqueueOutput {
    pub job: PublishJob,
    /// False when the same entry revision already had a job (that job is returned).
    pub created: bool,
}

/// Input of `publish.step`, `publish.run` and `publish.reconcile`.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishDriveInput {
    pub scope: Scope,
    pub job_id: String,
    /// Platform base URL, for example `http://127.0.0.1:4010`. Falls back to the `platform_endpoints`
    /// setting for the job's platform. A non-loopback host is refused.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Bearer token for this request only. Never stored.
    pub token: String,
    #[serde(default)]
    pub now: Option<String>,
    /// `publish.run` only: the step cap (default 20, at most 100).
    #[serde(default)]
    pub max_steps: Option<i64>,
    /// Seconds to wait between container status polls (default 0).
    #[serde(default)]
    pub poll_interval_secs: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishStepOutput {
    pub job: PublishJob,
    /// What this step did: `advanced`, `polling`, `published`, `waiting`, `not_due`, `failed`,
    /// `outcome_unknown`, `cancelled`, `terminal`, `reconciled_adopted`, `reconciled_requeued`,
    /// `reconcile_exhausted`, `reconcile_ambiguous`, `reconcile_failed`.
    pub outcome: String,
    pub detail: Option<String>,
    /// Platform requests this command sent.
    pub calls: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRunOutput {
    pub job: PublishJob,
    pub steps: i64,
    pub outcomes: Vec<String>,
    /// Why the run ended: a state name (`published`, `failed`, `cancelled`, `waiting`,
    /// `outcome_unknown`) or `max_steps`.
    pub stopped: String,
    pub calls: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublishGetInput {
    pub scope: Scope,
    pub job_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublishListInput {
    pub scope: Scope,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub entry_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishListOutput {
    pub scope: Scope,
    pub jobs: Vec<PublishJob>,
}

/// `publish.resolve_unknown` input: a person records what they saw on the platform for a job that is
/// `outcome_unknown`. No token and no base URL: the command makes no network call.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishResolveUnknownInput {
    pub scope: Scope,
    pub job_id: String,
    /// `posted` or `not_posted`.
    pub decision: String,
    /// The human author (`adrian`). An agent author is refused.
    pub resolved_by: String,
    /// What the person checked, 1 to 500 characters.
    pub note: String,
    /// `posted` only, required: the platform's id of the post, as the provider stores ids.
    #[serde(default)]
    pub remote_id: Option<String>,
    /// `posted` only, optional: a plain `https://` URL of the post.
    #[serde(default)]
    pub permalink: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResolveUnknownOutput {
    pub job: PublishJob,
    /// False when the identical resolution was already stored (nothing was written).
    pub applied: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublishCancelInput {
    pub scope: Scope,
    pub job_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// The reasons `publish.next` gives for a job that may not run now, in the order they are reported when
/// several apply (the first is `reason`, all are in `reasons`). `publish.step` and `publish.run` refuse
/// an opted-in account's create with the same code as the error code.
pub const PUBLISH_NEXT_REASONS: [&str; 7] = [
    "not_due",
    "outcome_unknown_on_account",
    "account_not_ready",
    "account_paused",
    "rate_limited_until",
    "daily_cap",
    "account_gap",
];

/// `publish.next` scope: a venture and a channel. Every channel account in it is considered.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishNextScope {
    pub venture: String,
    pub channel: String,
}

/// `publish.next` input. Read only: it changes nothing and never reads the clock.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishNextInput {
    pub scope: PublishNextScope,
    /// The instant to decide at (UTC, `YYYY-MM-DDTHH:MM:SSZ`). Required.
    pub now: String,
    /// At most this many jobs in `ready` (1 to 100). `ready_total` still counts all of them.
    #[serde(default)]
    pub limit: Option<i64>,
}

/// One job named by `publish.next`. `scheduled_at` is `next_attempt_at` for a waiting job, else the
/// instant the job was enqueued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishNextJob {
    pub job_id: String,
    pub scope: Scope,
    pub platform: String,
    pub remote_account: String,
    pub entry_id: String,
    pub state: String,
    pub phase: String,
    pub scheduled_at: String,
}

/// A job that may not run now: the exact reason, every reason that applies, and the earliest instant
/// it could (absent when a person or an unknown must act first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishNextBlocked {
    pub job_id: String,
    pub scope: Scope,
    pub platform: String,
    pub remote_account: String,
    pub entry_id: String,
    pub state: String,
    pub phase: String,
    pub scheduled_at: String,
    /// One of [`PUBLISH_NEXT_REASONS`].
    pub reason: String,
    pub reasons: Vec<String>,
    /// The earliest instant every reason has passed. Absent when a reason has no instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    pub detail: String,
    /// Set when the job could run but another job of the same account is selected first: that job's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<String>,
}

/// `publish.next` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishNextOutput {
    pub now: String,
    /// Jobs that may run now: at most one per account, oldest scheduled time first, then job id.
    pub ready: Vec<PublishNextJob>,
    /// How many jobs could run now before `limit` cut the list.
    pub ready_total: i64,
    /// Jobs that may not run now, in the same order.
    pub blocked: Vec<PublishNextBlocked>,
    /// Jobs already past their create (or running): pacing does not hold these back.
    pub in_progress: Vec<PublishNextJob>,
}

/// `publish.quota_get` input: one account's pacing, read at `now`.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishQuotaGetInput {
    pub scope: Scope,
    pub platform: String,
    pub remote_account: String,
    /// Required: the UTC day count is read at this instant.
    pub now: String,
}

/// One account's pacing: the limits in force (stored, or the host's own defaults), the pause, the last
/// create, the count for the UTC day of `now`, and the last rate-limit signal seen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishQuotaRecord {
    pub scope: Scope,
    pub platform: String,
    pub remote_account: String,
    /// True once `publish.quota_put` wrote this account's limits. Only then do `publish.step` and
    /// `publish.run` enforce pacing on a direct run; `publish.next` applies the limits in force always.
    pub opted_in: bool,
    pub min_gap_secs: i64,
    pub max_per_day: i64,
    /// the host's own default gap (its caution, not a platform limit).
    pub default_min_gap_secs: i64,
    /// the host's own default per-day count (its caution, not a platform limit).
    pub default_max_per_day: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_create_at: Option<String>,
    /// Creates counted in the UTC day of `now`.
    pub creates_today: i64,
    /// The next UTC midnight after the day the count applies to.
    pub day_resets_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limited_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limited_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// `publish.quota_put` input. A person sets all three limits; an agent author is refused. A setting
/// looser than the host's defaults needs `confirm_looser: true`.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishQuotaPutInput {
    pub scope: Scope,
    pub platform: String,
    pub remote_account: String,
    /// Seconds between two creates on the account, 60 to 86400.
    pub min_gap_secs: i64,
    /// Creates per UTC day, 1 to 100.
    pub max_per_day: i64,
    /// No create before this instant. Absent clears the pause.
    #[serde(default)]
    pub paused_until: Option<String>,
    #[serde(default)]
    pub confirm_looser: bool,
    /// The human author (`adrian`). An agent author is refused.
    pub set_by: String,
    /// Required: the instant the change is recorded at.
    pub now: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishQuotaPutOutput {
    pub record: PublishQuotaRecord,
    /// True when the stored limits are looser than the defaults (and were confirmed).
    pub looser_than_defaults: bool,
    pub set_by: String,
}
