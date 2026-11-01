//! Payload shapes for the analytics store (host-kept store). `analytics.pull` reads a
//! platform's post insights and runs only in the daemon-dev role. `analytics.list`, `analytics.latest`
//! and `analytics.summary` run in any role. A stored value is an exact decimal string or the explicit
//! state `unavailable`: never a float, and never zero for a metric the platform did not provide. The
//! summary carries each post's age at observation and computes no ranking and no recommendation.
use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;

/// `analytics.pull` input. The token is for this command only: it is never stored, logged or returned.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticsPullInput {
    pub scope: Scope,
    /// Platform base URL. Falls back to the `platform_endpoints` setting; loopback only unless `allow_remote`.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Bearer token for this pull only.
    pub token: String,
    /// The observation instant (UTC `YYYY-MM-DDTHH:MM:SSZ`). The pull is idempotent per scope and instant.
    #[serde(default)]
    pub now: Option<String>,
    /// Only `lifetime` in this slice (the default).
    #[serde(default)]
    pub window: Option<String>,
}

/// What a pull did with one published post. `observed` stored its metrics; `skipped` stored nothing
/// and carries `code` and `message`; `not_read` means the pull stopped before reaching the post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsPostOutcome {
    pub job_id: String,
    pub remote_post_id: String,
    pub outcome: String,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// One pull. `state` is `complete`, or `stopped` with `stop_code` `token_expired` when the platform
/// rejected the account's token. Counts: observed + skipped + not_read = posts_total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsPull {
    pub pull_id: String,
    pub scope: Scope,
    pub window: String,
    pub observed_at: String,
    pub started_at: String,
    pub finished_at: String,
    pub state: String,
    pub stop_code: Option<String>,
    pub posts_total: i64,
    pub posts_observed: i64,
    pub posts_skipped: i64,
    pub posts_not_read: i64,
    pub observations_written: i64,
    pub outcomes: Vec<AnalyticsPostOutcome>,
}

/// `analytics.pull` output. `created` is false when this scope already has a pull at this instant: that
/// pull is returned, and nothing was read or written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsPullOutput {
    pub pull: AnalyticsPull,
    pub created: bool,
}

/// `analytics.list` and `analytics.latest` input. Both filters are optional.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticsFilterInput {
    pub scope: Scope,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub metric: Option<String>,
}

/// One stored observation of one metric of one published post. An api row (`source` `api`) comes from an
/// analytics pull. A manual row (`source` `manual_export`) comes from a committed metric import; it has no
/// pull and names no window, so `pull_id` and `window` are null and `import_id` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsObservation {
    pub job_id: String,
    pub platform: String,
    pub remote_post_id: String,
    /// The instant the job reached `published` (the publish job's last write).
    pub published_at: String,
    pub metric: String,
    /// `value` (then `value` is an exact decimal string) or `unavailable` (then `value` is null).
    pub state: String,
    pub value: Option<String>,
    pub observed_at: String,
    /// `lifetime` for an api row. Null for a manual row.
    pub window: Option<String>,
    pub source: String,
    /// The pull that stored an api row. Null for a manual row.
    pub pull_id: Option<String>,
    /// The manual import that stored a manual row. Absent on an api row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
    /// The lineage of the publish job, when `analytics.lineage_put` recorded one. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_map: Option<String>,
    /// The registry's access for this platform and metric under own-app access: `available`, `unavailable` or
    /// `unverified`. Absent when the registry does not list the metric. An `unavailable` metric is always
    /// `state: unavailable` here, whatever a row stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// The named reason, set when `access` is `unavailable` or `unverified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// `analytics.lineage_put` input: the exact variant, render hash and time map that produced one publish job.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticsLineagePutInput {
    pub scope: Scope,
    pub job_id: String,
    pub variant_id: String,
    pub render_hash: String,
    pub time_map: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// One stored lineage row. A job has at most one; the same triple again is idempotent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsLineage {
    pub scope: Scope,
    pub job_id: String,
    pub variant_id: String,
    pub render_hash: String,
    pub time_map: String,
    pub created_at: String,
}

/// `analytics.lineage_put` output. `created` is false when the same lineage was already stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsLineagePutOutput {
    pub lineage: AnalyticsLineage,
    pub created: bool,
}

/// `analytics.list` and `analytics.latest` output, ordered by observed_at, job, metric (latest: job, metric).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsObservationsOutput {
    pub scope: Scope,
    pub observations: Vec<AnalyticsObservation>,
}

/// `analytics.summary` input.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticsSummaryInput {
    pub scope: Scope,
    #[serde(default)]
    pub job_id: Option<String>,
}

/// The latest observation of one metric, with the post's age at that observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsSummaryMetric {
    pub metric: String,
    pub state: String,
    pub value: Option<String>,
    pub observed_at: String,
    /// `lifetime` for an api row. Null for a manual row.
    pub window: Option<String>,
    /// The pull that stored an api row. Null for a manual row.
    pub pull_id: Option<String>,
    /// The manual import that stored a manual row. Absent on an api row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
    /// observed_at minus published_at, in seconds. Use it to refuse comparing posts of different ages.
    pub post_age_secs: i64,
    /// Registry access under own-app access (`available`, `unavailable`, `unverified`). Absent when unlisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// The named reason, set when `access` is `unavailable` or `unverified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsSummaryJob {
    pub job_id: String,
    pub platform: String,
    pub remote_post_id: String,
    pub published_at: String,
    pub metrics: Vec<AnalyticsSummaryMetric>,
}

/// `analytics.summary` output: jobs ordered by published_at, job; metrics ordered by name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsSummaryOutput {
    pub scope: Scope,
    pub jobs: Vec<AnalyticsSummaryJob>,
}

/// One row of the per-platform metric registry. It states what the
/// number means, what it is divided by (if anything), the window it covers, how fresh the platform keeps it, and
/// the access under own-app access. `access` is `available`, `unavailable` (closed or not exposed: never zero) or
/// `unverified` (not yet observed against the real platform). `reason` names why when it is not `available`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSemantics {
    pub platform: String,
    pub metric: String,
    pub meaning: String,
    /// What the metric is divided by, or `none` for a raw count.
    pub denominator: String,
    /// The windows the platform can report: `lifetime`, `24h`.
    pub windows: Vec<String>,
    /// How long after the event the platform's own counter may lag, as stated by its documentation.
    pub freshness: String,
    pub endpoint: String,
    pub account_type: String,
    pub scope: String,
    pub access: String,
    pub reason: Option<String>,
}

/// `analytics.registry` input. Both filters are optional; no filter lists the whole registry.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticsRegistryInput {
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub metric: Option<String>,
}

/// `analytics.registry` output, ordered by platform then metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsRegistryOutput {
    pub metrics: Vec<MetricSemantics>,
}

/// `analytics.gate` input: may this optimisation rule run on this metric of this platform? `rule` is the
/// optimisation rule's name (for example `remake`, `retention_edit`, `time_local`). `needs_retention` is true when
/// the rule edits within a post's timeline; a counts-only platform is refused for it.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticsGateInput {
    pub rule: String,
    pub platform: String,
    pub metric: String,
    #[serde(default)]
    pub needs_retention: bool,
}

/// `analytics.gate` output when the rule is allowed: the gate the rule names. A refusal is an error whose code
/// is `metric_unavailable`, `metric_unverified`, `counts_only` or `metric_unknown`, with the reason in its message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsGateOutput {
    pub rule: String,
    pub platform: String,
    pub metric: String,
    pub endpoint: String,
    pub account_type: String,
    pub scope: String,
    pub access: String,
    pub allowed: bool,
}
