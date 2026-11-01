//! Hook test over a host-implemented [`HookTestStore`]: one brief body with N openings (2 to 5).
//! `hook_test.bind` records which published publish job carries which opening. `hook_test.read` compares the
//! openings from the observations a pull stored and decides a winner. This module makes NO network call and
//! reads no clock (`now` is an input); it never writes a publish or analytics table. Publishing the trial
//! reels is not here.
//!
//! The rule, for each opening in index order:
//! - Its job must be in scope and `published` (`not_published` otherwise).
//! - Its stored observation of the set's metric is the one taken closest to 24 hours after the job's published
//!   instant: at least 24 hours after, and at or before `now`. No such observation is `no_observation_after_24h`.
//! - A missing or `unavailable` value is `missing_metric`, never zero. A value that is not a whole number is
//!   `value_not_integer`.
//!
//! When every opening compares, the top value must be held by exactly one opening (`tie` otherwise). A strict
//! single maximum decides the set: the winner is `promoted`, the others `archived`, and each keeps its number,
//! observation instant, pull id and publish instant. A decided set never changes. Any other outcome stores
//! nothing and returns `insufficient_evidence` with its reason.
//!
//! Every row is keyed by venture, channel and account, and every command checks its scope against a stored
//! channel (`foreign_scope` otherwise). Writes are compare-and-swap on `revision`.
use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::util::{
    bad, in_txn, internal, non_empty, parse_input, require_channel, to_value, WriteTxn,
};
use serde_json::Value;

use crate::protocol::{
    HookTestBindInput, HookTestCreateInput, HookTestGetInput, HookTestListInput,
    HookTestListOutput, HookTestOpening, HookTestReadInput, HookTestReadOutput, HookTestSet, Scope,
    HOOK_TEST_STATES,
};
use crate::Fail;

/// Methods that write. A dry run reports these and writes nothing. `hook_test.read` writes only when it decides.
pub const WRITE_METHODS: &[&str] = &["hook_test.create", "hook_test.bind", "hook_test.read"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "hook_test.create",
    "hook_test.bind",
    "hook_test.read",
    "hook_test.get",
    "hook_test.list",
];

/// The only window stored (the insights endpoints report lifetime totals).
const WINDOW: &str = "lifetime";
const STATE_OPEN: &str = "open";
const STATE_DECIDED: &str = "decided";
const ROLE_PROMOTED: &str = "promoted";
const ROLE_ARCHIVED: &str = "archived";
/// The stored observation state of a value the platform reported.
const OBS_VALUE: &str = "value";
/// The publish job state that makes an opening bindable and comparable.
const PUBLISHED: &str = "published";
/// The 24-hour window an observation must fall after, in seconds.
const DAY_SECS: i64 = 86_400;
const MIN_OPENINGS: usize = 2;
const MAX_OPENINGS: usize = 5;

const STATUS_DECIDED: &str = "decided";
const STATUS_INSUFFICIENT: &str = "insufficient_evidence";
const REASON_UNBOUND: &str = "opening_unbound";
const REASON_NOT_PUBLISHED: &str = "not_published";
const REASON_NO_OBS: &str = "no_observation_after_24h";
const REASON_MISSING: &str = "missing_metric";
const REASON_NOT_INTEGER: &str = "value_not_integer";
const REASON_TIE: &str = "tie";

fn not_found(set_id: &str) -> Fail {
    (
        "not_found",
        format!("no hook test set {set_id} for this scope"),
    )
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
        None | Some(WINDOW) => Ok(WINDOW),
        Some(other) => Err(bad(format!(
            "window must be {WINDOW} (the insights endpoints report lifetime totals only), got {other}"
        ))),
    }
}

fn check_state(state: &str) -> Result<(), Fail> {
    if HOOK_TEST_STATES.contains(&state) {
        Ok(())
    } else {
        Err(bad(format!(
            "state must be one of {}, got {state}",
            HOOK_TEST_STATES.join(", ")
        )))
    }
}

/// Exactly one row must have changed. Zero means a racing write moved the row under its revision guard.
/// Exactly one row must have changed. Zero means a racing write moved the row under its revision guard.
fn one(changed: bool, what: &str) -> Result<(), Fail> {
    if changed {
        Ok(())
    } else {
        Err((
            "revision_conflict",
            format!("{what} changed while it was written"),
        ))
    }
}

/// One stored observation of the metric for one job in one window.
#[derive(Debug, Clone)]
pub struct HookObs {
    pub observed_at: String,
    /// `value` or `unavailable`.
    pub state: String,
    pub value: Option<String>,
    pub pull_id: String,
}

/// What a decision stores on one opening.
#[derive(Debug, Clone)]
pub struct OpeningDecision<'a> {
    pub scope: &'a Scope,
    pub set_id: &'a str,
    pub opening_index: i64,
    /// `promoted` or `archived`.
    pub role: &'a str,
    pub value: &'a str,
    pub observed_at: &'a str,
    pub pull_id: &'a str,
    pub published_at: &'a str,
}

/// What the host app provides: the hook-test set and opening tables, and read access to publish jobs and
/// stored observations. Every method is one read or one write on the host's own database, on the caller's
/// transaction. A write that returns `bool` reports whether exactly one row changed.
pub trait HookTestStore: WriteTxn {
    /// One set with its openings in index order.
    fn load_set(&self, scope: &Scope, set_id: &str) -> Result<Option<HookTestSet>, Fail>;
    /// The state (in the publish vocabulary: `published`, ...) and `updated_at` of a publish job in scope.
    /// For a published job, `updated_at` is the instant the publish run stored it as published.
    fn job_row(&self, scope: &Scope, job_id: &str) -> Result<Option<(String, String)>, Fail>;
    /// The stored observations of the metric for one job in one window, oldest first.
    fn observations(
        &self,
        scope: &Scope,
        job_id: &str,
        metric: &str,
        window: &str,
    ) -> Result<Vec<HookObs>, Fail>;
    /// Stores a new open set at revision 1 with its unbound openings (`set.openings` carries index and text).
    fn insert_set(&self, set: &HookTestSet) -> Result<(), Fail>;
    /// Whether any opening in scope is bound to this job.
    fn job_bound(&self, scope: &Scope, job_id: &str) -> Result<bool, Fail>;
    /// Binds an opening that is still unbound to the job.
    fn bind_opening(
        &self,
        scope: &Scope,
        set_id: &str,
        opening_index: i64,
        job_id: &str,
        bound_at: &str,
    ) -> Result<bool, Fail>;
    /// Sets the revision and `updated_at` of an open set where the stored revision equals `expected_revision`.
    fn bump_set(
        &self,
        scope: &Scope,
        set_id: &str,
        new_revision: i64,
        updated_at: &str,
        expected_revision: i64,
    ) -> Result<bool, Fail>;
    /// Stores a decision on an opening whose role is still empty.
    fn decide_opening(&self, decision: &OpeningDecision<'_>) -> Result<bool, Fail>;
    /// Marks an open set decided (winner, `decided_at` and `updated_at` = `stamp`, the new revision) where
    /// the stored revision equals `expected_revision`.
    fn decide_set(
        &self,
        scope: &Scope,
        set_id: &str,
        winner: i64,
        stamp: &str,
        new_revision: i64,
        expected_revision: i64,
    ) -> Result<bool, Fail>;
    /// Set ids in scope (of `state` when given), oldest first then by id.
    fn set_ids(&self, scope: &Scope, state: Option<&str>) -> Result<Vec<String>, Fail>;
}

/// The observation one opening is compared at, with the lineage a decision stores.
struct Pick {
    opening_index: i64,
    value: i128,
    value_text: String,
    observed_at: String,
    pull_id: String,
    published_at: String,
}

struct Shortfall {
    reason: &'static str,
    message: String,
}

enum Verdict {
    Open(Shortfall),
    Decide { winner: i64, picks: Vec<Pick> },
}

fn short(reason: &'static str, message: String) -> Verdict {
    Verdict::Open(Shortfall { reason, message })
}

/// The rule over stored rows, with no writes: one pick per opening, then a strict single maximum.

fn assess(
    store: &dyn HookTestStore,
    scope: &Scope,
    set: &HookTestSet,
    now: i64,
) -> Result<Verdict, Fail> {
    let mut picks: Vec<Pick> = Vec::with_capacity(set.openings.len());
    for opening in &set.openings {
        let Some(job_id) = opening.job_id.as_deref() else {
            return Ok(short(
                REASON_UNBOUND,
                format!("opening {} has no publish job bound", opening.opening_index),
            ));
        };
        let Some((job_state, published_at)) = store.job_row(scope, job_id)? else {
            return Ok(short(
                REASON_NOT_PUBLISHED,
                format!("publish job {job_id} is not in this scope"),
            ));
        };
        if job_state != PUBLISHED {
            return Ok(short(
                REASON_NOT_PUBLISHED,
                format!("publish job {job_id} is {job_state}, not published"),
            ));
        }
        let published = time::parse_instant(&published_at, "published_at")?;

        // The observation closest to 24 hours after publish: never before the window, never after `now`.
        let mut best: Option<(i64, HookObs)> = None;
        for obs in store.observations(scope, job_id, &set.metric, &set.window)? {
            let at = time::parse_instant(&obs.observed_at, "observed_at")?;
            let age = at - published;
            if age < DAY_SECS || at > now {
                continue;
            }
            if best.as_ref().is_none_or(|(b, _)| age < *b) {
                best = Some((age, obs));
            }
        }
        let Some((_, obs)) = best else {
            return Ok(short(
                REASON_NO_OBS,
                format!(
                    "opening {}, job {job_id}, has no {} observation at least 24 hours after publish up to this read",
                    opening.opening_index, set.metric
                ),
            ));
        };

        let value = match (obs.state.as_str(), obs.value.as_deref()) {
            (OBS_VALUE, Some(text)) => text,
            _ => {
                return Ok(short(
                    REASON_MISSING,
                    format!(
                        "opening {}, job {job_id}, has no stored {} value at {}; a missing value is never zero",
                        opening.opening_index, set.metric, obs.observed_at
                    ),
                ))
            }
        };
        let Ok(number) = value.parse::<i128>() else {
            return Ok(short(
                REASON_NOT_INTEGER,
                format!(
                    "{} of job {job_id} is {value:?}; the decision compares whole numbers only",
                    set.metric
                ),
            ));
        };
        picks.push(Pick {
            opening_index: opening.opening_index,
            value: number,
            value_text: value.to_string(),
            observed_at: obs.observed_at.clone(),
            pull_id: obs.pull_id.clone(),
            published_at: published_at.clone(),
        });
    }

    let Some(top) = picks.iter().map(|p| p.value).max() else {
        return Err(internal("a hook test set has no openings"));
    };
    let winners: Vec<i64> = picks
        .iter()
        .filter(|p| p.value == top)
        .map(|p| p.opening_index)
        .collect();
    match winners.as_slice() {
        [winner] => Ok(Verdict::Decide {
            winner: *winner,
            picks,
        }),
        _ => Ok(short(
            REASON_TIE,
            format!(
                "{} openings share the top {} value {top}",
                winners.len(),
                set.metric
            ),
        )),
    }
}

/// Writes a decision: each opening's role and lineage, then the set as decided. Both writes are guarded by the
/// revision the read loaded and by the set still being open, so a racing decision changes nothing twice.

/// Writes a decision: each opening's role and lineage, then the set as decided. Both writes are guarded by the
/// revision the read loaded and by the set still being open, so a racing decision changes nothing twice.
fn record_decision(
    store: &dyn HookTestStore,
    scope: &Scope,
    set: &HookTestSet,
    winner: i64,
    picks: &[Pick],
    stamp: &str,
) -> Result<(), Fail> {
    for pick in picks {
        let role = if pick.opening_index == winner {
            ROLE_PROMOTED
        } else {
            ROLE_ARCHIVED
        };
        let changed = store.decide_opening(&OpeningDecision {
            scope,
            set_id: &set.set_id,
            opening_index: pick.opening_index,
            role,
            value: &pick.value_text,
            observed_at: &pick.observed_at,
            pull_id: &pick.pull_id,
            published_at: &pick.published_at,
        })?;
        one(changed, "hook test opening")?;
    }
    let changed = store.decide_set(
        scope,
        &set.set_id,
        winner,
        stamp,
        set.revision + 1,
        set.revision,
    )?;
    one(changed, "hook test set")
}

fn read_output(
    scope: &Scope,
    set: HookTestSet,
    status: &str,
    reason: Option<&str>,
    message: Option<String>,
) -> Result<Value, Fail> {
    to_value(&HookTestReadOutput {
        scope: scope.clone(),
        set_id: set.set_id.clone(),
        status: status.to_string(),
        reason: reason.map(str::to_string),
        message,
        set,
    })
}

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(store: &dyn HookTestStore, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "hook_test.create" => create(store, input),
        "hook_test.bind" => bind(store, input),
        "hook_test.read" => read(store, input),
        "hook_test.get" => get(store, input),
        "hook_test.list" => list(store, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `hook_test.create`: one body with 2 to 5 openings, all unbound, at revision 1.
fn create(store: &dyn HookTestStore, input: Value) -> Result<Value, Fail> {
    let req: HookTestCreateInput = parse_input(input, "hook_test.create")?;
    non_empty(&req.set_id, "set_id")?;
    non_empty(&req.brief_ref, "brief_ref")?;
    non_empty(&req.body, "body")?;
    let count = req.openings.len();
    if !(MIN_OPENINGS..=MAX_OPENINGS).contains(&count) {
        return Err(bad(format!(
            "openings must hold {MIN_OPENINGS} to {MAX_OPENINGS} texts, got {count}"
        )));
    }
    for (i, text) in req.openings.iter().enumerate() {
        non_empty(text, &format!("opening {}", i + 1))?;
    }
    if !metric_name_ok(&req.metric) {
        return Err(bad(format!(
            "metric must be lowercase letters, digits, underscores or dots, got {:?}",
            req.metric
        )));
    }
    let window = window_of(req.window.as_deref())?;
    let now = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now);

    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        if store.load_set(&req.scope, &req.set_id)?.is_some() {
            return Err((
                "already_exists",
                format!("hook test set {} already exists in this scope", req.set_id),
            ));
        }
        store.insert_set(&HookTestSet {
            scope: req.scope.clone(),
            set_id: req.set_id.clone(),
            brief_ref: req.brief_ref.clone(),
            body: req.body.clone(),
            metric: req.metric.clone(),
            window: window.to_string(),
            state: STATE_OPEN.to_string(),
            winner_opening: None,
            revision: 1,
            decided_at: None,
            created_at: stamp.clone(),
            updated_at: stamp.clone(),
            openings: req
                .openings
                .iter()
                .enumerate()
                .map(|(i, text)| HookTestOpening {
                    opening_index: (i + 1) as i64,
                    text: text.trim().to_string(),
                    job_id: None,
                    bound_at: None,
                    role: None,
                    value: None,
                    observed_at: None,
                    pull_id: None,
                    published_at: None,
                })
                .collect(),
        })?;
        store
            .load_set(&req.scope, &req.set_id)?
            .ok_or_else(|| not_found(&req.set_id))
    })?;
    to_value(&saved)
}

/// `hook_test.bind`: records that one opening is carried by one published job. `expected_revision` must equal
/// the set's revision. A decided set is `invalid_state`. A job that another opening already carries, or an
/// opening already bound to a different job, is `already_bound`. Binding the same job again changes nothing.
fn bind(store: &dyn HookTestStore, input: Value) -> Result<Value, Fail> {
    let req: HookTestBindInput = parse_input(input, "hook_test.bind")?;
    non_empty(&req.job_id, "job_id")?;
    let now = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now);

    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let set = store
            .load_set(&req.scope, &req.set_id)?
            .ok_or_else(|| not_found(&req.set_id))?;
        if set.revision != req.expected_revision {
            return Err((
                "revision_conflict",
                format!(
                    "set {} is at revision {}; expected_revision {} is stale",
                    set.set_id, set.revision, req.expected_revision
                ),
            ));
        }
        if set.state != STATE_OPEN {
            return Err((
                "invalid_state",
                format!(
                    "set {} is {}; a decided set never changes",
                    set.set_id, set.state
                ),
            ));
        }
        let Some(opening) = set
            .openings
            .iter()
            .find(|o| o.opening_index == req.opening_index)
        else {
            return Err(bad(format!(
                "opening_index must be between 1 and {}, got {}",
                set.openings.len(),
                req.opening_index
            )));
        };
        if let Some(bound) = opening.job_id.clone() {
            if bound == req.job_id {
                return Ok(set);
            }
            return Err((
                "already_bound",
                format!(
                    "opening {} is bound to job {bound}; a different job cannot take its place",
                    req.opening_index
                ),
            ));
        }
        let (job_state, _) = store.job_row(&req.scope, &req.job_id)?.ok_or_else(|| {
            (
                "not_found",
                format!("no publish job {} for this scope", req.job_id),
            )
        })?;
        if job_state != PUBLISHED {
            return Err((
                "not_published",
                format!(
                    "publish job {} is {job_state}; only a published job can be bound",
                    req.job_id
                ),
            ));
        }
        if store.job_bound(&req.scope, &req.job_id)? {
            return Err((
                "already_bound",
                format!("job {} is already bound to another opening", req.job_id),
            ));
        }

        let changed = store.bind_opening(
            &req.scope,
            &req.set_id,
            req.opening_index,
            &req.job_id,
            &stamp,
        )?;
        one(changed, "hook test opening")?;
        let changed = store.bump_set(
            &req.scope,
            &req.set_id,
            set.revision + 1,
            &stamp,
            set.revision,
        )?;
        one(changed, "hook test set")?;
        store
            .load_set(&req.scope, &req.set_id)?
            .ok_or_else(|| not_found(&req.set_id))
    })?;
    to_value(&saved)
}

/// `hook_test.read`: evaluates an open set at `now`. A decided set returns its stored decision and writes
/// nothing. An open set that cannot be decided stores nothing and returns `insufficient_evidence` with its
/// reason. A set that can be decided is decided in this call. `expected_revision`, when given, must match the
/// set's revision for an open set.
fn read(store: &dyn HookTestStore, input: Value) -> Result<Value, Fail> {
    let req: HookTestReadInput = parse_input(input, "hook_test.read")?;
    let now = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now);

    in_txn(store, || {
        require_channel(store, &req.scope)?;
        let set = store
            .load_set(&req.scope, &req.set_id)?
            .ok_or_else(|| not_found(&req.set_id))?;
        if set.state == STATE_DECIDED {
            return read_output(&req.scope, set, STATUS_DECIDED, None, None);
        }
        if let Some(expected) = req.expected_revision.filter(|e| *e != set.revision) {
            return Err((
                "revision_conflict",
                format!(
                    "set {} is at revision {}; expected_revision {expected} is stale",
                    set.set_id, set.revision
                ),
            ));
        }

        match assess(store, &req.scope, &set, now)? {
            Verdict::Open(shortfall) => read_output(
                &req.scope,
                set,
                STATUS_INSUFFICIENT,
                Some(shortfall.reason),
                Some(shortfall.message),
            ),
            Verdict::Decide { winner, picks } => {
                record_decision(store, &req.scope, &set, winner, &picks, &stamp)?;
                let saved = store
                    .load_set(&req.scope, &req.set_id)?
                    .ok_or_else(|| not_found(&req.set_id))?;
                read_output(&req.scope, saved, STATUS_DECIDED, None, None)
            }
        }
    })
}

fn get(store: &dyn HookTestStore, input: Value) -> Result<Value, Fail> {
    let req: HookTestGetInput = parse_input(input, "hook_test.get")?;
    require_channel(store, &req.scope)?;
    let set = store
        .load_set(&req.scope, &req.set_id)?
        .ok_or_else(|| not_found(&req.set_id))?;
    to_value(&set)
}

/// `hook_test.list`: every set in one scope, optionally by state, oldest first.
fn list(store: &dyn HookTestStore, input: Value) -> Result<Value, Fail> {
    let req: HookTestListInput = parse_input(input, "hook_test.list")?;
    if let Some(state) = req.state.as_deref() {
        check_state(state)?;
    }
    require_channel(store, &req.scope)?;
    let ids = store.set_ids(&req.scope, req.state.as_deref())?;
    let mut sets = Vec::with_capacity(ids.len());
    for id in &ids {
        sets.push(
            store
                .load_set(&req.scope, id)?
                .ok_or_else(|| not_found(id))?,
        );
    }
    to_value(&HookTestListOutput {
        scope: req.scope,
        sets,
    })
}
