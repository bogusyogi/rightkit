//! Analytics over a host-implemented [`AnalyticsStore`]: `analytics.pull` reads each published job's post
//! insights from the platform and stores one observation per named metric; `analytics.list`, `latest` and
//! `summary` read them back (with the committed manual imports of [`crate::metric_import`], source
//! `manual_export`, no pull, no window); `analytics.lineage_put` records a job's variant, render hash and
//! time map; `analytics.registry` and `analytics.gate` state what each metric means and whether it may be
//! read. The tables and the HTTP GET belong to the host ([`AnalyticsStore::http_get`]).
//!
//! Rules this module keeps:
//! - A value is an exact decimal string taken from the platform's integer or decimal string. Floats are
//!   refused, so a metric is never rounded.
//! - `unavailable` is an explicit state. A missing or null metric is stored as `unavailable`, never 0.
//! - A pull is idempotent per scope and observed instant: the same `now` returns the stored pull and
//!   sends no request. A later `now` appends a new observation for each metric.
//! - A 429, a 5xx or a transport failure skips that one post (no retry in the pull) and the others go on.
//!   A 401 stops the pull for the account with `token_expired`.
//! - A per-platform registry states each metric's meaning, denominator, windows, freshness and own-app access.
//!   A metric the registry marks `unavailable` reads as `unavailable` on every api row, whatever was stored.
//! - Nothing here ranks posts or recommends anything.
use std::collections::BTreeMap;

use rightkit_http::percent_encode_component as percent_encode;
use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::http::Reply;
use rightkit_social_publish::protocol::PublishRemote;
use rightkit_social_publish::util::{
    bad, db_err, in_txn, internal, parse_input, require_channel, to_value, WriteTxn,
};
use serde_json::Value;

use crate::protocol::{
    AnalyticsFilterInput, AnalyticsGateInput, AnalyticsGateOutput, AnalyticsLineage,
    AnalyticsLineagePutInput, AnalyticsLineagePutOutput, AnalyticsObservation,
    AnalyticsObservationsOutput, AnalyticsPostOutcome, AnalyticsPull, AnalyticsPullInput,
    AnalyticsPullOutput, AnalyticsRegistryInput, AnalyticsRegistryOutput, AnalyticsSummaryInput,
    AnalyticsSummaryJob, AnalyticsSummaryMetric, AnalyticsSummaryOutput, MetricSemantics, Scope,
};
use crate::Fail;

/// Methods that write. A dry run reports them and sends nothing.
pub const WRITE_METHODS: [&str; 2] = ["analytics.pull", "analytics.lineage_put"];

/// The default window: the platform insights endpoints report lifetime totals.
const WINDOW: &str = "lifetime";
/// The rolling 24-hour window. The platform must report it (`?window=24h` on the insights endpoint).
const WINDOW_24H: &str = "24h";
/// The only source stored by a pull.
const SOURCE: &str = "api";
/// The platform's explicit marker for a metric it does not provide.
const UNAVAILABLE: &str = "unavailable";

/// A published job with a remote post id as the host stores it: the only jobs a pull reads.
#[derive(Debug, Clone)]
pub struct PublishedRow {
    pub job_id: String,
    pub platform: String,
    pub remote_account: String,
    /// The stored `remote` JSON ([`PublishRemote`]).
    pub remote_json: String,
    /// The job's last write, which for a published job is the write that set `published`.
    pub updated_at: String,
}

/// One stored reading: an exact decimal string, or the explicit unavailable state.
#[derive(Debug, Clone)]
pub enum Reading {
    Value(String),
    Unavailable,
}

/// One observation to write; it carries the pull's scope, instant, window and id from the pull row.
#[derive(Debug, Clone)]
pub struct NewObservation {
    pub job_id: String,
    pub platform: String,
    pub remote_post_id: String,
    pub published_at: String,
    pub metric: String,
    pub reading: Reading,
}

/// What the host app provides: the pull, observation and lineage tables, the published jobs, and the one
/// HTTP GET of a pull. Every method is one read or one write on the host's own database, on the caller's
/// transaction.
pub trait AnalyticsStore: WriteTxn {
    /// The validated base URL for a platform: `given` (the command's `base_url`), else the host's setting.
    fn base_url(&self, platform: &str, given: Option<&str>) -> Result<String, Fail>;
    /// GETs `url` with `token` as the bearer credential and classifies the answer. The token is scrubbed
    /// from every message in the reply.
    fn http_get(&self, url: &str, token: &str) -> Reply;
    /// Published (complete) jobs in scope, oldest first (created, then job id): the order of reads.
    fn published_rows(&self, scope: &Scope) -> Result<Vec<PublishedRow>, Fail>;
    /// The stored pull with this id, outcomes included.
    fn load_pull(&self, pull_id: &str) -> Result<Option<AnalyticsPull>, Fail>;
    /// Stores the pull row (a concurrent identical pull fails on the key and writes nothing).
    fn insert_pull(&self, pull: &AnalyticsPull) -> Result<(), Fail>;
    /// Stores one observation of a stored pull (source `api`).
    fn insert_observation(
        &self,
        scope: &Scope,
        pull: &AnalyticsPull,
        row: &NewObservation,
    ) -> Result<(), Fail>;
    /// Every observation in scope as one set: the api rows and the stored values of committed manual
    /// imports (`window` and `pull_id` absent, `source` `manual_export`, `import_id` set, `published_at` the
    /// job's last write), filtered by job and metric when given. With `latest` false: ordered by
    /// `observed_at`, job, metric, then an api row before a manual row at the same instant, then the
    /// import's creation instant, import id and data row. With `latest` true: per job, metric and window the
    /// row with the greatest `observed_at` (two manual rows at that instant are ordered by the import's
    /// creation instant, import id and data row, and the greatest wins), ordered by job, metric, window and
    /// import id. The variant, render hash, time map, access and reason fields are left empty.
    fn observations(
        &self,
        scope: &Scope,
        job_id: Option<&str>,
        metric: Option<&str>,
        latest: bool,
    ) -> Result<Vec<AnalyticsObservation>, Fail>;
    /// The stored lineage of one job in scope, if any.
    fn load_lineage(&self, scope: &Scope, job_id: &str) -> Result<Option<AnalyticsLineage>, Fail>;
    /// Whether the job exists in the scope.
    fn job_in_scope(&self, scope: &Scope, job_id: &str) -> Result<bool, Fail>;
    /// Stores one lineage row.
    fn insert_lineage(&self, lineage: &AnalyticsLineage) -> Result<(), Fail>;
}

/// The window the caller asked for: `lifetime` (the default) or `24h`.
fn window_of(raw: Option<&str>) -> Result<&'static str, Fail> {
    match raw {
        None | Some(WINDOW) => Ok(WINDOW),
        Some(WINDOW_24H) => Ok(WINDOW_24H),
        Some(other) => Err(bad(format!(
            "window must be {WINDOW} or {WINDOW_24H}, got {other}"
        ))),
    }
}

fn check_token(token: &str) -> Result<(), Fail> {
    if token.trim().is_empty() {
        return Err(bad("token must not be empty"));
    }
    if token.chars().any(char::is_control) {
        return Err(bad("token must not contain control characters"));
    }
    Ok(())
}

/// Metric names are lowercase ASCII letters, digits, underscores and dots (for example `reach`,
/// `average_view_duration_secs`), at most 64 bytes.
fn metric_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

/// An exact decimal literal: optional minus, digits, optional fraction. No exponent, no float.
fn is_decimal(text: &str) -> bool {
    if text.len() > 40 {
        return false;
    }
    let body = text.strip_prefix('-').unwrap_or(text);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = body.splitn(2, '.');
    let whole = parts.next().unwrap_or("");
    match parts.next() {
        None => digits(whole),
        Some(fraction) => digits(whole) && digits(fraction),
    }
}

/// The derived pull id: the same scope, window and instant is the same pull on every run.
fn pull_id_for(scope: &Scope, window: &str, observed_at: &str) -> String {
    let key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        scope.venture, scope.channel, scope.account, window, observed_at
    );
    format!("ap-{:016x}", rightkit_social_publish::fnv1a(&key))
}

/// A published job with a remote post id: the only jobs a pull reads.
struct PublishedPost {
    job_id: String,
    platform: String,
    remote_account: String,
    post_id: String,
    published_at: String,
}

/// Published jobs in scope with a remote post id, oldest first. This order is the order of reads.
fn published_posts(store: &dyn AnalyticsStore, scope: &Scope) -> Result<Vec<PublishedPost>, Fail> {
    let mut posts = Vec::new();
    for row in store.published_rows(scope)? {
        let remote: PublishRemote = serde_json::from_str(&row.remote_json).map_err(db_err)?;
        if let Some(post_id) = remote.post_id.filter(|id| !id.trim().is_empty()) {
            posts.push(PublishedPost {
                job_id: row.job_id,
                platform: row.platform,
                remote_account: row.remote_account,
                post_id,
                published_at: row.updated_at,
            });
        }
    }
    Ok(posts)
}

/// The insights endpoint of each supported platform, as a path under the platform's base URL. Each path
/// names the account and the post id, both percent-encoded.
fn insights_path(platform: &str, account: &str, post_id: &str) -> Option<String> {
    let template = match platform {
        "instagram" => "/instagram/{a}/media/{id}/insights",
        "threads" => "/threads/{a}/posts/{id}/insights",
        "x" => "/x/{a}/tweets/{id}/analytics",
        "linkedin" => "/linkedin/{a}/posts/{id}/analytics",
        "youtube" => "/youtube/{a}/videos/{id}/analytics",
        "bluesky" => "/bluesky/{a}/posts/{id}/analytics",
        "mastodon" => "/mastodon/{a}/statuses/{id}/analytics",
        _ => return None,
    };
    Some(
        template
            .replace("{a}", &percent_encode(account))
            .replace("{id}", &percent_encode(post_id)),
    )
}

/// The named metrics of one insights answer, sorted by name. The whole answer is refused (and the post
/// skipped) when any one metric is not an integer, a decimal string or `unavailable`: a partial post
/// would mislead a later comparison.
fn readings(body: &Value) -> Result<Vec<(String, Reading)>, Fail> {
    let metrics = body
        .get("metrics")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            (
                "bad_response",
                "the insights answer has no metrics object".to_string(),
            )
        })?;
    let mut out = Vec::new();
    for (name, raw) in metrics {
        if !metric_name_ok(name) {
            return Err((
                "bad_metric",
                format!(
                    "metric name {name:?} is not lowercase letters, digits, underscores or dots"
                ),
            ));
        }
        let reading = match raw {
            Value::Null => Reading::Unavailable,
            Value::String(s) if s == UNAVAILABLE => Reading::Unavailable,
            Value::Number(n) if n.is_i64() || n.is_u64() => Reading::Value(n.to_string()),
            Value::String(s) if is_decimal(s) => Reading::Value(s.clone()),
            _ => {
                return Err((
                    "bad_metric",
                    format!(
                        "metric {name} is neither an integer, a decimal string nor {UNAVAILABLE}"
                    ),
                ))
            }
        };
        out.push((name.clone(), reading));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn post_outcome(
    post: &PublishedPost,
    outcome: &str,
    code: Option<&str>,
    message: Option<String>,
) -> AnalyticsPostOutcome {
    AnalyticsPostOutcome {
        job_id: post.job_id.clone(),
        remote_post_id: post.post_id.clone(),
        outcome: outcome.to_string(),
        code: code.map(str::to_string),
        message,
    }
}

// ---- Metric registry and access gates ----

const ACCESS_AVAILABLE: &str = "available";
const ACCESS_UNAVAILABLE: &str = "unavailable";
const ACCESS_UNVERIFIED: &str = "unverified";
/// Platforms that expose counts only: a rule that edits within a post's timeline is refused for them.
const COUNTS_ONLY: [&str; 1] = ["tiktok"];

/// One row's fixed text: (platform, metric, meaning, denominator, endpoint, account type, scope, freshness, access, reason).
type RegRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

const ROWS: &[RegRow] = &[
    (
        "bluesky",
        "likes",
        "Likes on the post",
        "none",
        "app.bsky.feed.getPosts",
        "own account",
        "read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "bluesky",
        "replies",
        "Replies to the post",
        "none",
        "app.bsky.feed.getPosts",
        "own account",
        "read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "bluesky",
        "reposts",
        "Reposts of the post",
        "none",
        "app.bsky.feed.getPosts",
        "own account",
        "read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "instagram",
        "likes",
        "Likes on the media",
        "none",
        "/{media-id}/insights",
        "professional account",
        "instagram_business_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "instagram",
        "reach",
        "Distinct accounts that saw the media",
        "none",
        "/{media-id}/insights",
        "professional account",
        "instagram_business_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "instagram",
        "saves",
        "Accounts that saved the media",
        "none",
        "/{media-id}/insights",
        "professional account",
        "instagram_business_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "instagram",
        "shares",
        "Shares of the media",
        "none",
        "/{media-id}/insights",
        "professional account",
        "instagram_business_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "linkedin",
        "comments",
        "Comments on the member post",
        "none",
        "member social actions (r_member_social)",
        "member",
        "r_member_social",
        "n/a",
        ACCESS_UNAVAILABLE,
        Some("linkedin_member_analytics_closed"),
    ),
    (
        "linkedin",
        "impressions",
        "Impressions of the member post",
        "none",
        "member social actions (r_member_social)",
        "member",
        "r_member_social",
        "n/a",
        ACCESS_UNAVAILABLE,
        Some("linkedin_member_analytics_closed"),
    ),
    (
        "linkedin",
        "reactions",
        "Reactions on the member post",
        "none",
        "member social actions (r_member_social)",
        "member",
        "r_member_social",
        "n/a",
        ACCESS_UNAVAILABLE,
        Some("linkedin_member_analytics_closed"),
    ),
    (
        "mastodon",
        "favourites",
        "Favourites of the status",
        "none",
        "/api/v1/statuses/{id}",
        "own account",
        "read:statuses",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "mastodon",
        "reblogs",
        "Reblogs of the status",
        "none",
        "/api/v1/statuses/{id}",
        "own account",
        "read:statuses",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "mastodon",
        "replies",
        "Replies to the status",
        "none",
        "/api/v1/statuses/{id}",
        "own account",
        "read:statuses",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "pinterest",
        "impressions",
        "Pin impressions",
        "none",
        "/v5/pins/{id}/analytics",
        "own account",
        "pins:read",
        "unverified",
        ACCESS_UNVERIFIED,
        Some("pinterest_analytics_unverified"),
    ),
    (
        "pinterest",
        "saves",
        "Pin saves",
        "none",
        "/v5/pins/{id}/analytics",
        "own account",
        "pins:read",
        "unverified",
        ACCESS_UNVERIFIED,
        Some("pinterest_analytics_unverified"),
    ),
    (
        "threads",
        "likes",
        "Likes on the post",
        "none",
        "/{media-id}/insights",
        "own account",
        "threads_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "threads",
        "replies",
        "Replies to the post",
        "none",
        "/{media-id}/insights",
        "own account",
        "threads_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "threads",
        "reposts",
        "Reposts of the post",
        "none",
        "/{media-id}/insights",
        "own account",
        "threads_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "threads",
        "views",
        "Views of the post",
        "none",
        "/{media-id}/insights",
        "own account",
        "threads_manage_insights",
        "up to 24 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "tiktok",
        "average_watch_time_secs",
        "Mean seconds watched per view",
        "views",
        "none (not exposed)",
        "own account",
        "video.list",
        "n/a",
        ACCESS_UNAVAILABLE,
        Some("tiktok_counts_only"),
    ),
    (
        "tiktok",
        "comments",
        "Comments on the video",
        "none",
        "video.list",
        "own account",
        "video.list",
        "unverified",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "tiktok",
        "likes",
        "Likes on the video",
        "none",
        "video.list",
        "own account",
        "video.list",
        "unverified",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "tiktok",
        "retention_curve",
        "Share of viewers still watching at each second",
        "views",
        "none (desktop export only)",
        "own account",
        "video.list",
        "n/a",
        ACCESS_UNAVAILABLE,
        Some("tiktok_counts_only"),
    ),
    (
        "tiktok",
        "shares",
        "Shares of the video",
        "none",
        "video.list",
        "own account",
        "video.list",
        "unverified",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "tiktok",
        "views",
        "Views of the video",
        "none",
        "video.list",
        "own account",
        "video.list",
        "unverified",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "x",
        "bookmarks",
        "Bookmarks of the post",
        "none",
        "/2/tweets/{id} non_public_metrics",
        "own account",
        "tweet.read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "x",
        "impressions",
        "Impressions of the post",
        "none",
        "/2/tweets/{id} non_public_metrics",
        "own account",
        "tweet.read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "x",
        "likes",
        "Likes on the post",
        "none",
        "/2/tweets/{id} public_metrics",
        "own account",
        "tweet.read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "x",
        "replies",
        "Replies to the post",
        "none",
        "/2/tweets/{id} public_metrics",
        "own account",
        "tweet.read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "x",
        "reposts",
        "Reposts of the post",
        "none",
        "/2/tweets/{id} public_metrics",
        "own account",
        "tweet.read",
        "near real time",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "youtube",
        "average_view_duration_secs",
        "Mean seconds watched per view",
        "views",
        "YouTube Analytics reports.query",
        "own channel",
        "yt-analytics.readonly",
        "up to 48 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "youtube",
        "comments",
        "Comments on the video",
        "none",
        "YouTube Analytics reports.query",
        "own channel",
        "yt-analytics.readonly",
        "up to 48 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "youtube",
        "likes",
        "Likes on the video",
        "none",
        "YouTube Analytics reports.query",
        "own channel",
        "yt-analytics.readonly",
        "up to 48 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
    (
        "youtube",
        "views",
        "Views of the video",
        "none",
        "YouTube Analytics reports.query",
        "own channel",
        "yt-analytics.readonly",
        "up to 48 h lag",
        ACCESS_AVAILABLE,
        None,
    ),
];

fn to_semantics(r: &RegRow) -> MetricSemantics {
    let windows = if r.8 == ACCESS_UNAVAILABLE {
        Vec::new()
    } else {
        vec![WINDOW.to_string(), WINDOW_24H.to_string()]
    };
    MetricSemantics {
        platform: r.0.to_string(),
        metric: r.1.to_string(),
        meaning: r.2.to_string(),
        denominator: r.3.to_string(),
        windows,
        freshness: r.7.to_string(),
        endpoint: r.4.to_string(),
        account_type: r.5.to_string(),
        scope: r.6.to_string(),
        access: r.8.to_string(),
        reason: r.9.map(str::to_string),
    }
}

/// The registry row for one platform and metric, if listed.
pub fn semantics(platform: &str, metric: &str) -> Option<MetricSemantics> {
    ROWS.iter()
        .find(|r| r.0 == platform && r.1 == metric)
        .map(to_semantics)
}

/// Marks an observation with the registry's access. A metric the registry says is `unavailable` reads as
/// `unavailable` with no value on an api row, whatever was stored; a manual export is its own access path
/// and keeps its value.
fn apply_registry(obs: &mut AnalyticsObservation) {
    let Some(sem) = semantics(&obs.platform, &obs.metric) else {
        return;
    };
    if sem.access == ACCESS_UNAVAILABLE && obs.source == SOURCE {
        obs.state = UNAVAILABLE.to_string();
        obs.value = None;
    }
    obs.access = Some(sem.access);
    obs.unavailable_reason = sem.reason;
}

/// `analytics.registry`: the per-platform metric semantics, optionally for one platform or metric.
pub fn registry(input: Value) -> Result<Value, Fail> {
    let req: AnalyticsRegistryInput = parse_input(input, "analytics.registry")?;
    check_metric_filter(req.metric.as_deref())?;
    let metrics = ROWS
        .iter()
        .filter(|r| req.platform.as_deref().is_none_or(|p| r.0 == p))
        .filter(|r| req.metric.as_deref().is_none_or(|m| r.1 == m))
        .map(to_semantics)
        .collect();
    to_value(&AnalyticsRegistryOutput { metrics })
}

/// The gate every optimisation rule passes before it reads a metric. It names the metric, endpoint, account
/// type and scope, and refuses with a named code when the metric is not available under own-app access:
/// `counts_only` (a timeline edit on a counts-only platform), `metric_unavailable`, `metric_unverified`,
/// `metric_unknown`. Callers (the optimisation rules) must call it before reading observations.
pub fn check_gate(
    rule: &str,
    platform: &str,
    metric: &str,
    needs_retention: bool,
) -> Result<AnalyticsGateOutput, Fail> {
    if rule.trim().is_empty() {
        return Err(bad("rule must not be empty"));
    }
    if needs_retention && COUNTS_ONLY.contains(&platform) {
        return Err((
            "counts_only",
            format!("rule {rule} edits within a post's timeline and {platform} exposes counts only (tiktok_counts_only)"),
        ));
    }
    let Some(sem) = semantics(platform, metric) else {
        return Err((
            "metric_unknown",
            format!("rule {rule}: the registry has no metric {metric} for {platform}"),
        ));
    };
    let reason = sem.reason.clone().unwrap_or_default();
    match sem.access.as_str() {
        ACCESS_UNAVAILABLE => Err((
            "metric_unavailable",
            format!(
                "rule {rule}: {platform} {metric} is unavailable under own-app access ({reason})"
            ),
        )),
        ACCESS_UNVERIFIED => Err((
            "metric_unverified",
            format!(
                "rule {rule}: {platform} {metric} is not verified under own-app access ({reason})"
            ),
        )),
        _ => Ok(AnalyticsGateOutput {
            rule: rule.to_string(),
            platform: platform.to_string(),
            metric: metric.to_string(),
            endpoint: sem.endpoint,
            account_type: sem.account_type,
            scope: sem.scope,
            access: sem.access,
            allowed: true,
        }),
    }
}

/// `analytics.gate`: runs [`check_gate`] for the named rule, platform and metric.
pub fn gate(input: Value) -> Result<Value, Fail> {
    let req: AnalyticsGateInput = parse_input(input, "analytics.gate")?;
    to_value(&check_gate(
        &req.rule,
        &req.platform,
        &req.metric,
        req.needs_retention,
    )?)
}

/// How a scheduled pull narrows `analytics.pull`. The command itself uses the default: every published post in
/// scope, every metric stored as read.
#[derive(Default)]
pub struct PullPolicy {
    /// Read only these jobs (the posts the host found due). `None` reads every published post in scope.
    pub only_jobs: Option<std::collections::HashSet<String>>,
    /// Never store a metric the registry marks `unavailable`.
    pub skip_unavailable: bool,
}

/// `analytics.pull`: read each published post's insights once and store one observation per metric.
///
/// Returns `{ pull, created }`. When the scope already has a pull at this `now`, that pull is returned
/// with `created: false` and no request is sent. A 401 stops the pull for the account (`state: stopped`,
/// `stop_code: token_expired`); the posts after it are `not_read`. Observations stored before the stop
/// stay.
pub fn pull(store: &dyn AnalyticsStore, input: Value) -> Result<Value, Fail> {
    pull_with(store, input, &PullPolicy::default())
}

/// `analytics.pull` with a policy; see [`pull`].
pub fn pull_with(
    store: &dyn AnalyticsStore,
    input: Value,
    policy: &PullPolicy,
) -> Result<Value, Fail> {
    let req: AnalyticsPullInput = parse_input(input, "analytics.pull")?;
    let window = window_of(req.window.as_deref())?;
    check_token(&req.token)?;
    let now = time::now_secs(req.now.as_deref())?;
    let observed_at = time::format_instant(now);
    let pull_id = pull_id_for(&req.scope, window, &observed_at);

    require_channel(store, &req.scope)?;
    if let Some(existing) = store.load_pull(&pull_id)? {
        return to_value(&AnalyticsPullOutput {
            pull: existing,
            created: false,
        });
    }

    let mut posts = published_posts(store, &req.scope)?;
    if let Some(only) = &policy.only_jobs {
        posts.retain(|p| only.contains(&p.job_id));
    }
    // Validate every platform's base URL before the first request, so a refused URL sends nothing.
    let mut bases: BTreeMap<String, String> = BTreeMap::new();
    for post in &posts {
        if !bases.contains_key(&post.platform) {
            let base = store.base_url(&post.platform, req.base_url.as_deref())?;
            bases.insert(post.platform.clone(), base);
        }
    }

    let mut outcomes: Vec<AnalyticsPostOutcome> = Vec::new();
    let mut rows: Vec<NewObservation> = Vec::new();
    let mut stopped = false;
    for post in &posts {
        if stopped {
            outcomes.push(post_outcome(
                post,
                "not_read",
                Some("pull_stopped"),
                Some("the pull stopped for this account before this post".to_string()),
            ));
            continue;
        }
        let Some(path) = insights_path(&post.platform, &post.remote_account, &post.post_id) else {
            outcomes.push(post_outcome(
                post,
                "skipped",
                Some("platform_unsupported"),
                Some(format!("no insights endpoint for {}", post.platform)),
            ));
            continue;
        };
        let Some(base) = bases.get(&post.platform) else {
            outcomes.push(post_outcome(
                post,
                "skipped",
                Some("platform_unsupported"),
                Some(format!("no base URL for {}", post.platform)),
            ));
            continue;
        };
        // The lifetime window is the endpoint's default; any other window is named on the query string.
        let query = if window == WINDOW {
            String::new()
        } else {
            format!("?window={window}")
        };
        let url = format!("{base}{path}{query}");
        match store.http_get(&url, &req.token) {
            Reply::Ok(body) => match readings(&body) {
                Ok(found) => {
                    for (metric, reading) in found {
                        if policy.skip_unavailable
                            && semantics(&post.platform, &metric)
                                .is_some_and(|s| s.access == ACCESS_UNAVAILABLE)
                        {
                            continue;
                        }
                        rows.push(NewObservation {
                            job_id: post.job_id.clone(),
                            platform: post.platform.clone(),
                            remote_post_id: post.post_id.clone(),
                            published_at: post.published_at.clone(),
                            metric,
                            reading,
                        });
                    }
                    outcomes.push(post_outcome(post, "observed", None, None));
                }
                Err((code, message)) => {
                    outcomes.push(post_outcome(post, "skipped", Some(code), Some(message)))
                }
            },
            Reply::Unauthorized => {
                stopped = true;
                outcomes.push(post_outcome(
                    post,
                    "skipped",
                    Some("token_expired"),
                    Some(
                        "the platform rejected the token; the pull stops for this account"
                            .to_string(),
                    ),
                ));
            }
            Reply::RateLimited(_) => outcomes.push(post_outcome(
                post,
                "skipped",
                Some("rate_limited"),
                Some(
                    "the platform answered 429; this post is skipped and not retried in this pull"
                        .to_string(),
                ),
            )),
            Reply::ServerError(status) => outcomes.push(post_outcome(
                post,
                "skipped",
                Some("server_error"),
                Some(format!(
                    "HTTP {status}; this post is skipped and not retried in this pull"
                )),
            )),
            Reply::Rejected { status, message } => outcomes.push(post_outcome(
                post,
                "skipped",
                Some("platform_rejected"),
                Some(format!("HTTP {status}: {message}")),
            )),
            Reply::Transport { message, .. } => outcomes.push(post_outcome(
                post,
                "skipped",
                Some("connection_failed"),
                Some(message),
            )),
            Reply::Timeout => outcomes.push(post_outcome(
                post,
                "skipped",
                Some("timeout"),
                Some("the request timed out; this post is skipped".to_string()),
            )),
            Reply::Invalid(message) => outcomes.push(post_outcome(
                post,
                "skipped",
                Some("invalid_request"),
                Some(message),
            )),
        }
    }

    let count = |kind: &str| outcomes.iter().filter(|o| o.outcome == kind).count() as i64;
    // One clock per pull: the instant the caller gave. A replay in logical time has no wall-clock span.
    let pull = AnalyticsPull {
        pull_id,
        scope: req.scope.clone(),
        window: window.to_string(),
        observed_at: observed_at.clone(),
        started_at: observed_at.clone(),
        finished_at: observed_at,
        state: String::from(if stopped { "stopped" } else { "complete" }),
        stop_code: stopped.then(|| "token_expired".to_string()),
        posts_total: posts.len() as i64,
        posts_observed: count("observed"),
        posts_skipped: count("skipped"),
        posts_not_read: count("not_read"),
        observations_written: rows.len() as i64,
        outcomes,
    };
    // The pull row goes first because each observation references it.
    in_txn(store, || {
        store.insert_pull(&pull)?;
        for row in &rows {
            store.insert_observation(&req.scope, &pull, row)?;
        }
        Ok(())
    })?;
    to_value(&AnalyticsPullOutput {
        pull,
        created: true,
    })
}

fn check_metric_filter(metric: Option<&str>) -> Result<(), Fail> {
    match metric {
        Some(name) if !metric_name_ok(name) => Err(bad(format!(
            "metric must be lowercase letters, digits, underscores or dots, got {name:?}"
        ))),
        _ => Ok(()),
    }
}

fn query_observations(
    store: &dyn AnalyticsStore,
    latest: bool,
    scope: &Scope,
    job_id: Option<&str>,
    metric: Option<&str>,
) -> Result<Vec<AnalyticsObservation>, Fail> {
    let mut out = store.observations(scope, job_id, metric, latest)?;
    // The registry decides what an api row may say: a metric closed under own-app access is `unavailable`.
    for obs in out.iter_mut() {
        apply_registry(obs);
    }
    // Each row joins to its job's lineage, when one is stored (one row per job).
    for obs in out.iter_mut() {
        if let Some(lineage) = store.load_lineage(scope, &obs.job_id)? {
            obs.variant_id = Some(lineage.variant_id);
            obs.render_hash = Some(lineage.render_hash);
            obs.time_map = Some(lineage.time_map);
        }
    }
    Ok(out)
}

/// `analytics.lineage_put`: records the variant, render hash and time map of a publish job in scope. The same
/// triple again returns the stored row (`created` false); a different triple is `lineage_conflict`.
pub fn lineage_put(store: &dyn AnalyticsStore, input: Value) -> Result<Value, Fail> {
    let req: AnalyticsLineagePutInput = parse_input(input, "analytics.lineage_put")?;
    for (value, name) in [
        (&req.variant_id, "variant_id"),
        (&req.render_hash, "render_hash"),
        (&req.time_map, "time_map"),
    ] {
        if value.trim().is_empty() {
            return Err(bad(format!("{name} must not be empty")));
        }
    }
    let stamp = time::format_instant(time::now_secs(req.now.as_deref())?);
    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        if !store.job_in_scope(&req.scope, &req.job_id)? {
            return Err((
                "not_found",
                format!("no publish job {} in this scope", req.job_id),
            ));
        }
        if let Some(stored) = store.load_lineage(&req.scope, &req.job_id)? {
            if stored.variant_id != req.variant_id
                || stored.render_hash != req.render_hash
                || stored.time_map != req.time_map
            {
                return Err((
                    "lineage_conflict",
                    format!("job {} already has a different lineage", req.job_id),
                ));
            }
            return Ok(AnalyticsLineagePutOutput {
                lineage: stored,
                created: false,
            });
        }
        store.insert_lineage(&AnalyticsLineage {
            scope: req.scope.clone(),
            job_id: req.job_id.clone(),
            variant_id: req.variant_id.clone(),
            render_hash: req.render_hash.clone(),
            time_map: req.time_map.clone(),
            created_at: stamp.clone(),
        })?;
        let saved = store
            .load_lineage(&req.scope, &req.job_id)?
            .ok_or_else(|| internal("lineage vanished after insert"))?;
        Ok(AnalyticsLineagePutOutput {
            lineage: saved,
            created: true,
        })
    })?;
    to_value(&out)
}

/// `analytics.list`: every observation in scope, optionally by job and metric, oldest first.
pub fn list(store: &dyn AnalyticsStore, input: Value) -> Result<Value, Fail> {
    let req: AnalyticsFilterInput = parse_input(input, "analytics.list")?;
    check_metric_filter(req.metric.as_deref())?;
    require_channel(store, &req.scope)?;
    let observations = query_observations(
        store,
        false,
        &req.scope,
        req.job_id.as_deref(),
        req.metric.as_deref(),
    )?;
    to_value(&AnalyticsObservationsOutput {
        scope: req.scope,
        observations,
    })
}

/// `analytics.latest`: the newest observation per job and metric, `unavailable` kept as stated.
pub fn latest(store: &dyn AnalyticsStore, input: Value) -> Result<Value, Fail> {
    let req: AnalyticsFilterInput = parse_input(input, "analytics.latest")?;
    check_metric_filter(req.metric.as_deref())?;
    require_channel(store, &req.scope)?;
    let observations = query_observations(
        store,
        true,
        &req.scope,
        req.job_id.as_deref(),
        req.metric.as_deref(),
    )?;
    to_value(&AnalyticsObservationsOutput {
        scope: req.scope,
        observations,
    })
}

/// `analytics.summary`: per job, the latest value of each metric and the post's age at that
/// observation. No ranking and no recommendation: a later rule decides what to compare.
pub fn summary(store: &dyn AnalyticsStore, input: Value) -> Result<Value, Fail> {
    let req: AnalyticsSummaryInput = parse_input(input, "analytics.summary")?;
    require_channel(store, &req.scope)?;
    let latest = query_observations(store, true, &req.scope, req.job_id.as_deref(), None)?;

    let mut by_job: BTreeMap<String, AnalyticsSummaryJob> = BTreeMap::new();
    for obs in latest {
        let observed = time::parse_instant(&obs.observed_at, "observed_at")?;
        let published = time::parse_instant(&obs.published_at, "published_at")?;
        let job_id = obs.job_id.clone();
        let entry = by_job
            .entry(job_id.clone())
            .or_insert_with(|| AnalyticsSummaryJob {
                job_id,
                platform: obs.platform.clone(),
                remote_post_id: obs.remote_post_id.clone(),
                published_at: obs.published_at.clone(),
                metrics: Vec::new(),
            });
        entry.metrics.push(AnalyticsSummaryMetric {
            metric: obs.metric,
            state: obs.state,
            value: obs.value,
            observed_at: obs.observed_at,
            window: obs.window,
            pull_id: obs.pull_id,
            import_id: obs.import_id,
            post_age_secs: observed - published,
            access: obs.access,
            unavailable_reason: obs.unavailable_reason,
        });
    }
    let mut jobs: Vec<AnalyticsSummaryJob> = by_job.into_values().collect();
    jobs.sort_by(|a, b| {
        (a.published_at.as_str(), a.job_id.as_str())
            .cmp(&(b.published_at.as_str(), b.job_id.as_str()))
    });
    to_value(&AnalyticsSummaryOutput {
        scope: req.scope,
        jobs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_are_exact_literals() {
        for ok in ["0", "12", "-3", "1.50", "-0.25"] {
            assert!(is_decimal(ok), "{ok}");
        }
        for bad_text in ["", "1e3", "1.", ".5", "NaN", "1,5", "--1"] {
            assert!(!is_decimal(bad_text), "{bad_text}");
        }
    }

    #[test]
    fn readings_refuse_a_partial_answer() {
        let ok = readings(&serde_json::json!({"metrics": {"b": 2, "a": "3.5", "c": null}}))
            .expect("readings");
        let names: Vec<&str> = ok.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert!(readings(&serde_json::json!({"metrics": {"a": 1.5}})).is_err());
        assert!(readings(&serde_json::json!({"metrics": {"A": 1}})).is_err());
        assert!(readings(&serde_json::json!({})).is_err());
    }

    #[test]
    fn the_gate_names_a_refusal_code() {
        assert!(matches!(
            check_gate("", "x", "likes", false),
            Err((c, _)) if c == "bad_input"
        ));
        assert!(matches!(
            check_gate("r", "tiktok", "likes", true),
            Err((c, _)) if c == "counts_only"
        ));
        assert!(matches!(
            check_gate("r", "x", "no_such_metric", false),
            Err((c, _)) if c == "metric_unknown"
        ));
        assert!(semantics("bluesky", "likes").is_some());
    }

    #[test]
    fn insight_paths_encode_ids() {
        assert_eq!(
            insights_path("bluesky", "a b", "p/1").as_deref(),
            Some("/bluesky/a%20b/posts/p%2F1/analytics")
        );
        assert!(insights_path("nowhere", "a", "p").is_none());
    }
}
