//! Optimise: calendar change proposals made from stored analytics, kept by the host behind
//! [`OptimiseStore`]. `optimise.evaluate` reads the
//! observations that `analytics.pull` stored and applies one rule, `remake`. This module makes NO network
//! call and reads no clock (`now` is an input). Its one table is `optimise_proposals`. Accepting a remake
//! creates one draft calendar entry through the host's calendar commands ([`OptimiseStore::calendar_get`],
//! [`OptimiseStore::brief_put`], [`OptimiseStore::calendar_put`]); it never writes a calendar table itself.
//!
//! The remake rule:
//! - Population: every published job in scope with a remote post id. Each one needs a stored value of the
//!   metric at one snapshot instant (the newest `observed_at` of that metric in scope), and all of them must
//!   be the same age at that instant. A post with no value (or `unavailable`) makes the evaluation
//!   insufficient, never zero.
//! - Sample: at least `min_sample` posts (default 10). Fewer is `too_few_posts`.
//! - Top decile: the decile holds `ceil(n / 10)` posts. A post wins when its value is at least the decile's
//!   smallest value and the population is not all equal. Ties at the cut all win.
//! - Each winner gets one proposal, due `now` + 14 days. A remake is proposed at most once per source job,
//!   so a replay or a later evaluation of the same job creates nothing.
//!
//! Insufficient or incomparable evidence yields no proposal and a `reason`: `too_few_posts`,
//! `no_observations`, `missing_metric`, `incomparable_post_ages` or `value_not_integer`.
//!
//! Every row is keyed by venture, channel and account, and every command checks its scope against a stored
//! channel (`foreign_scope` otherwise). Decisions are compare-and-swap on `expected_revision`.
use rightkit_social_publish::protocol::PublishRemote;
use std::collections::{BTreeMap, BTreeSet};

use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::util::{
    bad, db_err, in_txn, internal, parse_input, require_channel, WriteTxn,
};
use serde_json::{json, Value};

use crate::protocol::{
    OptimiseDecideInput, OptimiseEvaluateInput, OptimiseEvaluateOutput, OptimiseGetInput,
    OptimiseListInput, OptimiseListOutput, OptimiseProposal, Scope, PROPOSAL_STATES,
};
use crate::Fail;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: &[&str] = &["optimise.evaluate", "optimise.accept", "optimise.dismiss"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "optimise.evaluate",
    "optimise.list",
    "optimise.get",
    "optimise.accept",
    "optimise.dismiss",
];

/// The one rule in this slice.
const RULE_REMAKE: &str = "remake";
/// The only window stored (the insights endpoints report lifetime totals).
const WINDOW: &str = "lifetime";
/// The stored state of a metric the platform reported.
const STATE_VALUE: &str = "value";
const STATE_PROPOSED: &str = "proposed";
const STATE_ACCEPTED: &str = "accepted";
const STATE_DISMISSED: &str = "dismissed";
/// The default sample a rule needs before it compares posts.
const DEFAULT_MIN_SAMPLE: i64 = 10;
/// The top decile is one tenth of the population, rounded up.
const DECILE_DIVISOR: usize = 10;
/// A remake is due this many seconds after the evaluation (14 days).
const REMAKE_DUE_SECS: i64 = 14 * 86_400;

const STATUS_EVALUATED: &str = "evaluated";
const STATUS_INSUFFICIENT: &str = "insufficient_evidence";
const REASON_TOO_FEW: &str = "too_few_posts";
const REASON_NO_OBSERVATIONS: &str = "no_observations";
const REASON_MISSING: &str = "missing_metric";
const REASON_AGES: &str = "incomparable_post_ages";
const REASON_NOT_INTEGER: &str = "value_not_integer";

fn encoded(value: serde_json::Result<Value>) -> Result<Value, Fail> {
    value.map_err(internal)
}

fn not_found(proposal_id: &str) -> Fail {
    (
        "not_found",
        format!("no optimise proposal {proposal_id} for this scope"),
    )
}

/// A published job as the store lists it for the remake rule.
#[derive(Debug, Clone)]
pub struct PublishedJob {
    pub job_id: String,
    pub platform: String,
    /// The stored `remote` JSON text.
    pub remote_json: String,
    /// The job's last update, taken as its publish instant.
    pub updated_at: String,
}

/// One stored observation of a metric.
#[derive(Debug, Clone)]
pub struct Obs {
    pub job_id: String,
    pub observed_at: String,
    pub state: String,
    pub value: Option<String>,
    pub pull_id: String,
}

/// A proposal to store (state `proposed`, revision 1, `created_at` and `updated_at` both `stamp`).
#[derive(Debug, Clone)]
pub struct NewProposal<'a> {
    pub scope: &'a Scope,
    pub proposal_id: &'a str,
    pub kind: &'a str,
    pub source_job_id: &'a str,
    pub platform: &'a str,
    pub metric: &'a str,
    pub window: &'a str,
    pub observed_at: &'a str,
    pub value: &'a str,
    pub threshold: &'a str,
    pub sample_size: i64,
    pub decile_size: i64,
    pub due_at: &'a str,
    /// JSON text of the lineage job ids.
    pub lineage_jobs_json: &'a str,
    /// JSON text of the lineage pull ids.
    pub lineage_pulls_json: &'a str,
    pub stamp: &'a str,
}

/// The decision to store on a `proposed` proposal at `expected` revision.
#[derive(Debug, Clone)]
pub struct Decision<'a> {
    pub scope: &'a Scope,
    pub proposal_id: &'a str,
    pub state: &'a str,
    pub revision: i64,
    pub note: Option<&'a str>,
    pub stamp: &'a str,
    pub expected_revision: i64,
    /// The calendar entry an accepted remake created (None for a dismissal).
    pub calendar_entry_id: Option<&'a str>,
}

/// What the host app provides: the `optimise_proposals` table, the published jobs and stored observations
/// the rule reads, and the host's calendar commands an accepted remake creates its draft entry through.
pub trait OptimiseStore: WriteTxn {
    /// Published jobs in scope (complete), ordered by job id.
    fn published_jobs(&self, scope: &Scope) -> Result<Vec<PublishedJob>, Fail>;
    /// Every stored observation of one metric in one window in scope, ordered by job and instant.
    fn observations(&self, scope: &Scope, metric: &str, window: &str) -> Result<Vec<Obs>, Fail>;
    /// One proposal by id, if stored.
    fn load_proposal(
        &self,
        scope: &Scope,
        proposal_id: &str,
    ) -> Result<Option<OptimiseProposal>, Fail>;
    /// Proposals in scope (optionally in one state), ordered by creation then id.
    fn list_proposals(
        &self,
        scope: &Scope,
        state: Option<&str>,
    ) -> Result<Vec<OptimiseProposal>, Fail>;
    /// Stores a new proposal.
    fn insert_proposal(&self, proposal: &NewProposal<'_>) -> Result<(), Fail>;
    /// Stores a decision, compare-and-swap on the revision and the `proposed` state; the rows changed.
    fn update_decision(&self, decision: &Decision<'_>) -> Result<u64, Fail>;
    /// The host's `calendar.get` command for `input` (`scope`, `entry_id`, `now`).
    fn calendar_get(&self, input: Value) -> Result<Value, Fail>;
    /// The host's `brief.put` command for `input`.
    fn brief_put(&self, input: Value) -> Result<Value, Fail>;
    /// The host's `calendar.put` command for `input`.
    fn calendar_put(&self, input: Value) -> Result<Value, Fail>;
    /// The caption prefix the host's calendar uses to mark a placeholder caption.
    fn placeholder_caption_prefix(&self) -> &str;
}

fn metric_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

/// The window the caller asked for. Only `lifetime` is stored.
fn window_of(raw: Option<&str>) -> Result<&'static str, Fail> {
    match raw {
        None => Ok(WINDOW),
        Some(w) if w == WINDOW => Ok(WINDOW),
        Some(other) => Err(bad(format!(
            "window must be {WINDOW} (the insights endpoints report lifetime totals only), got {other}"
        ))),
    }
}

fn check_state(state: &str) -> Result<(), Fail> {
    if PROPOSAL_STATES.contains(&state) {
        Ok(())
    } else {
        Err(bad(format!(
            "state must be one of {}, got {state}",
            PROPOSAL_STATES.join(", ")
        )))
    }
}

/// A published job with a remote post id: the only jobs the rule compares.
struct Post {
    job_id: String,
    platform: String,
    published_at: String,
}

struct Compared<'a> {
    post: &'a Post,
    obs: &'a Obs,
    value: Option<i128>,
    /// Seconds from publish to the snapshot.
    age: i64,
}

struct Shortfall {
    reason: &'static str,
    message: String,
    observed_at: Option<String>,
    sample_size: i64,
}

struct Winner {
    job_id: String,
    platform: String,
    value: String,
}

struct Ranking {
    observed_at: String,
    sample_size: i64,
    decile_size: i64,
    threshold: i128,
    winners: Vec<Winner>,
    lineage_job_ids: Vec<String>,
    lineage_pull_ids: Vec<String>,
}

enum Assessment {
    Insufficient(Shortfall),
    Ranked(Ranking),
}

/// The pure rule: no store access, no clock. Reads only the posts and observations it is given.
fn assess(
    posts: &[Post],
    obs: &[Obs],
    metric: &str,
    window: &str,
    min_sample: i64,
) -> Result<Assessment, Fail> {
    let n = posts.len();
    let sample_size = n as i64;
    if sample_size < min_sample {
        return Ok(Assessment::Insufficient(Shortfall {
            reason: REASON_TOO_FEW,
            message: format!("{n} published posts have a remote id; min_sample is {min_sample}"),
            observed_at: None,
            sample_size,
        }));
    }
    let Some(snapshot) = obs
        .iter()
        .map(|o| o.observed_at.as_str())
        .max()
        .map(str::to_string)
    else {
        return Ok(Assessment::Insufficient(Shortfall {
            reason: REASON_NO_OBSERVATIONS,
            message: format!("no observation of {metric} over {window} exists in this scope"),
            observed_at: None,
            sample_size,
        }));
    };
    let snap_secs = time::parse_instant(&snapshot, "observed_at")?;
    let at: BTreeMap<&str, &Obs> = obs
        .iter()
        .filter(|o| o.observed_at == snapshot)
        .map(|o| (o.job_id.as_str(), o))
        .collect();

    let mut compared: Vec<Compared> = Vec::with_capacity(n);
    let mut missing = 0usize;
    for post in posts {
        match at.get(post.job_id.as_str()).copied() {
            Some(o) if o.state == STATE_VALUE => {
                let published = time::parse_instant(&post.published_at, "published_at")?;
                compared.push(Compared {
                    post,
                    obs: o,
                    value: o.value.as_deref().and_then(|v| v.parse::<i128>().ok()),
                    age: snap_secs - published,
                });
            }
            _ => missing += 1,
        }
    }
    if missing > 0 {
        return Ok(Assessment::Insufficient(Shortfall {
            reason: REASON_MISSING,
            message: format!(
                "{missing} of {n} published posts have no stored {metric} at {snapshot}; a missing value is never zero"
            ),
            observed_at: Some(snapshot),
            sample_size,
        }));
    }
    if let Some(c) = compared.iter().find(|c| c.value.is_none()) {
        return Ok(Assessment::Insufficient(Shortfall {
            reason: REASON_NOT_INTEGER,
            message: format!(
                "{metric} of job {} is {:?}; the ranking compares whole numbers only",
                c.post.job_id,
                c.obs.value.as_deref().unwrap_or("")
            ),
            observed_at: Some(snapshot),
            sample_size,
        }));
    }
    let ages: BTreeSet<i64> = compared.iter().map(|c| c.age).collect();
    if ages.len() > 1 {
        let lo = ages.first().copied().unwrap_or_default();
        let hi = ages.last().copied().unwrap_or_default();
        return Ok(Assessment::Insufficient(Shortfall {
            reason: REASON_AGES,
            message: format!(
                "published posts were between {lo} and {hi} seconds old at {snapshot}; only posts of one age are comparable"
            ),
            observed_at: Some(snapshot),
            sample_size,
        }));
    }

    let mut values: Vec<i128> = compared.iter().filter_map(|c| c.value).collect();
    values.sort_unstable_by_key(|v| std::cmp::Reverse(*v));
    let decile_size = n.div_ceil(DECILE_DIVISOR);
    let (Some(threshold), Some(lowest)) =
        (values.get(decile_size - 1).copied(), values.last().copied())
    else {
        return Err(internal("the top-decile cut is outside the population"));
    };
    let winners: Vec<Winner> = compared
        .iter()
        .filter(|c| c.value.is_some_and(|v| v >= threshold && v > lowest))
        .map(|c| Winner {
            job_id: c.post.job_id.clone(),
            platform: c.post.platform.clone(),
            value: c.obs.value.clone().unwrap_or_default(),
        })
        .collect();
    let lineage_job_ids: Vec<String> = compared.iter().map(|c| c.post.job_id.clone()).collect();
    let lineage_pull_ids: Vec<String> = compared
        .iter()
        .map(|c| c.obs.pull_id.clone())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    Ok(Assessment::Ranked(Ranking {
        observed_at: snapshot,
        sample_size,
        decile_size: decile_size as i64,
        threshold,
        winners,
        lineage_job_ids,
        lineage_pull_ids,
    }))
}

/// Published jobs in scope with a remote post id, ordered by job id.
fn published_posts(store: &dyn OptimiseStore, scope: &Scope) -> Result<Vec<Post>, Fail> {
    let mut posts = Vec::new();
    for PublishedJob {
        job_id,
        platform,
        remote_json,
        updated_at: published_at,
    } in store.published_jobs(scope)?
    {
        let remote: PublishRemote = serde_json::from_str(&remote_json).map_err(db_err)?;
        if remote.post_id.is_some_and(|id| !id.trim().is_empty()) {
            posts.push(Post {
                job_id,
                platform,
                published_at,
            });
        }
    }
    Ok(posts)
}

/// The deterministic proposal id for one remake of one source job in one scope.
fn proposal_id_for(scope: &Scope, job_id: &str) -> String {
    let key = format!(
        "{RULE_REMAKE}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{job_id}",
        scope.venture, scope.channel, scope.account
    );
    format!("opt_{:016x}", rightkit_social_publish::fnv1a(&key))
}

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(store: &dyn OptimiseStore, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "optimise.evaluate" => evaluate(store, input),
        "optimise.list" => list(store, input),
        "optimise.get" => get(store, input),
        "optimise.accept" => decide(store, "optimise.accept", input, STATE_ACCEPTED),
        "optimise.dismiss" => decide(store, "optimise.dismiss", input, STATE_DISMISSED),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `optimise.evaluate`: applies the remake rule to the stored analytics of one metric and window, and
/// stores one proposal per winner that has none yet. Insufficient or incomparable evidence stores nothing.
fn evaluate(store: &dyn OptimiseStore, input: Value) -> Result<Value, Fail> {
    let req: OptimiseEvaluateInput = parse_input(input, "optimise.evaluate")?;
    if !metric_name_ok(&req.metric) {
        return Err(bad(format!(
            "metric must be lowercase letters, digits, underscores or dots, got {:?}",
            req.metric
        )));
    }
    let window = window_of(req.window.as_deref())?;
    let min_sample = req.min_sample.unwrap_or(DEFAULT_MIN_SAMPLE);
    if min_sample < 1 {
        return Err(bad(format!(
            "min_sample must be at least 1, got {min_sample}"
        )));
    }
    let now = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now);
    let due_at = time::format_instant(now + REMAKE_DUE_SECS);

    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let posts = published_posts(store, &req.scope)?;
        let obs = store.observations(&req.scope, &req.metric, window)?;
        let assessment = assess(&posts, &obs, &req.metric, window, min_sample)?;

        let mut out = OptimiseEvaluateOutput {
            scope: req.scope.clone(),
            rule: RULE_REMAKE.to_string(),
            metric: req.metric.clone(),
            window: window.to_string(),
            min_sample,
            status: STATUS_EVALUATED.to_string(),
            reason: None,
            message: None,
            observed_at: None,
            sample_size: 0,
            decile_size: 0,
            threshold: None,
            created: 0,
            proposals: Vec::new(),
        };
        match assessment {
            Assessment::Insufficient(short) => {
                out.status = STATUS_INSUFFICIENT.to_string();
                out.reason = Some(short.reason.to_string());
                out.message = Some(short.message);
                out.observed_at = short.observed_at;
                out.sample_size = short.sample_size;
            }
            Assessment::Ranked(rank) => {
                out.observed_at = Some(rank.observed_at.clone());
                out.sample_size = rank.sample_size;
                out.decile_size = rank.decile_size;
                out.threshold = Some(rank.threshold.to_string());
                let threshold_text = rank.threshold.to_string();
                let lineage_jobs =
                    serde_json::to_string(&rank.lineage_job_ids).map_err(internal)?;
                let lineage_pulls =
                    serde_json::to_string(&rank.lineage_pull_ids).map_err(internal)?;
                for w in &rank.winners {
                    let id = proposal_id_for(&req.scope, &w.job_id);
                    if let Some(existing) = store.load_proposal(&req.scope, &id)? {
                        out.proposals.push(existing);
                        continue;
                    }
                    store.insert_proposal(&NewProposal {
                        scope: &req.scope,
                        proposal_id: &id,
                        kind: RULE_REMAKE,
                        source_job_id: &w.job_id,
                        platform: &w.platform,
                        metric: &req.metric,
                        window,
                        observed_at: &rank.observed_at,
                        value: &w.value,
                        threshold: &threshold_text,
                        sample_size: rank.sample_size,
                        decile_size: rank.decile_size,
                        due_at: &due_at,
                        lineage_jobs_json: &lineage_jobs,
                        lineage_pulls_json: &lineage_pulls,
                        stamp: &stamp,
                    })?;
                    out.created += 1;
                    let saved = store
                        .load_proposal(&req.scope, &id)?
                        .ok_or_else(|| not_found(&id))?;
                    out.proposals.push(saved);
                }
            }
        }
        Ok(out)
    })?;
    encoded(serde_json::to_value(out))
}

/// `optimise.list`: every proposal in one scope, optionally by state, oldest first.
fn list(store: &dyn OptimiseStore, input: Value) -> Result<Value, Fail> {
    let req: OptimiseListInput = parse_input(input, "optimise.list")?;
    if let Some(state) = req.state.as_deref() {
        check_state(state)?;
    }
    require_channel(store, &req.scope)?;
    let proposals = store.list_proposals(&req.scope, req.state.as_deref())?;
    encoded(serde_json::to_value(OptimiseListOutput {
        scope: req.scope,
        proposals,
    }))
}

/// `optimise.get`: one proposal in scope.
fn get(store: &dyn OptimiseStore, input: Value) -> Result<Value, Fail> {
    let req: OptimiseGetInput = parse_input(input, "optimise.get")?;
    require_channel(store, &req.scope)?;
    let proposal = store
        .load_proposal(&req.scope, &req.proposal_id)?
        .ok_or_else(|| not_found(&req.proposal_id))?;
    encoded(serde_json::to_value(proposal))
}

/// The revision and state checks of a decision, in the order the store has always reported them.
fn check_decidable(current: &OptimiseProposal, expected_revision: i64) -> Result<(), Fail> {
    if current.revision != expected_revision {
        return Err((
            "revision_conflict",
            format!(
                "proposal {} is at revision {}; expected_revision {} is stale",
                current.proposal_id, current.revision, expected_revision
            ),
        ));
    }
    if current.state != STATE_PROPOSED {
        return Err((
            "invalid_state",
            format!(
                "proposal {} is {}; only a proposed proposal can be accepted or dismissed",
                current.proposal_id, current.state
            ),
        ));
    }
    Ok(())
}

/// Creates the one draft calendar entry an accepted remake needs. Its brief carries the lineage (metric,
/// window, source job, the posts and pulls compared). The entry is due on the proposal's due date in UTC, in
/// the state a new entry normally has (draft, approval none), so a person still approves it. Both ids derive
/// from the proposal id: an entry that already exists (a retry after a failed decision write) is reused and
/// nothing is written again.
fn create_remake_entry(
    store: &dyn OptimiseStore,
    scope: &Scope,
    p: &OptimiseProposal,
    now: &str,
) -> Result<String, Fail> {
    let entry_id = format!("remake_entry_{}", p.proposal_id);
    match store.calendar_get(json!({ "scope": scope, "entry_id": entry_id, "now": now })) {
        Ok(_) => return Ok(entry_id),
        Err(e) if e.0 != "not_found" => return Err(e),
        Err(_) => {}
    }

    let brief_id = format!("remake_brief_{}", p.proposal_id);
    let brief = store.brief_put(json!({
        "scope": scope,
        "brief_id": brief_id,
        "brief_type": RULE_REMAKE,
        "title": format!("Remake of {} ({} {})", p.source_job_id, p.metric, p.window),
        "payload": {
            "proposal_id": p.proposal_id,
            "rule": RULE_REMAKE,
            "source_job_id": p.source_job_id,
            "platform": p.platform,
            "metric": p.metric,
            "window": p.window,
            "observed_at": p.observed_at,
            "value": p.value,
            "threshold": p.threshold,
            "sample_size": p.sample_size,
            "decile_size": p.decile_size,
            "lineage_job_ids": p.lineage_job_ids,
            "lineage_pull_ids": p.lineage_pull_ids,
        },
        "now": now,
    }))?;
    let brief_revision = brief
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or_else(|| internal("brief.put returned no revision"))?;

    // The due instant without its `Z` is a naive wall time, which the UTC zone takes as it is.
    let local_time = p.due_at.trim_end_matches('Z');
    store.calendar_put(json!({
        "scope": scope,
        "entry_id": entry_id,
        "brief_id": brief_id,
        "brief_revision": brief_revision,
        "platform": p.platform,
        "format": RULE_REMAKE,
        "caption": format!(
            "{} Remake of {} on {}: {} over {}, observed {}; proposal {}",
            store.placeholder_caption_prefix(),
            p.source_job_id, p.platform, p.metric, p.window, p.observed_at, p.proposal_id
        ),
        "media": [],
        "timezone": "UTC",
        "local_time": local_time,
        "now": now,
    }))?;
    Ok(entry_id)
}

/// `optimise.accept` and `optimise.dismiss`: move one `proposed` proposal to `target`. `expected_revision`
/// must equal the stored revision (`revision_conflict` otherwise), and a proposal already decided is
/// `invalid_state`. Accepting a remake first creates its calendar entry, then records the decision with that
/// entry's id. The calendar write runs before this proposal's transaction, because calendar.rs opens its own
/// connection. If it fails, nothing on the proposal changes and the error is returned. Dismissing writes only
/// the proposal row.
fn decide(
    store: &dyn OptimiseStore,
    method: &str,
    input: Value,
    target: &str,
) -> Result<Value, Fail> {
    let req: OptimiseDecideInput = parse_input(input, method)?;
    let now = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now);
    let note = req
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let current = {
        require_channel(store, &req.scope)?;
        store
            .load_proposal(&req.scope, &req.proposal_id)?
            .ok_or_else(|| not_found(&req.proposal_id))?
    };
    check_decidable(&current, req.expected_revision)?;

    let calendar_entry_id = if target == STATE_ACCEPTED {
        Some(create_remake_entry(store, &req.scope, &current, &req.now)?)
    } else {
        None
    };

    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let n = store.update_decision(&Decision {
            scope: &req.scope,
            proposal_id: &req.proposal_id,
            state: target,
            revision: current.revision + 1,
            note: note.as_deref(),
            stamp: &stamp,
            expected_revision: req.expected_revision,
            calendar_entry_id: calendar_entry_id.as_deref(),
        })?;
        if n != 1 {
            return Err((
                "revision_conflict",
                format!("proposal {} changed while it was written", req.proposal_id),
            ));
        }
        store
            .load_proposal(&req.scope, &req.proposal_id)?
            .ok_or_else(|| not_found(&req.proposal_id))
    })?;
    encoded(serde_json::to_value(saved))
}
