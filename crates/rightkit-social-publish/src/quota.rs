//! Quota scheduling across accounts: which queued publish job may run now, and the per-account pacing
//! state that decides it. Nothing here sends a request, runs a timer or reads the clock: every instant is
//! an input (`now`), so the answer is a pure function of the store and `now`. The short version:
//!
//! - An account is `(venture, channel, channel account, platform, remote account)`. Its pacing row
//!   ([`QuotaRow`]) holds the limits, a pause, the last create, the create count for the
//!   current UTC day and the last rate-limit signal. Sends are also written to a ledger, one
//!   row per job and reconcile round, so a retry after a clean failure is not counted twice.
//! - The host app keeps the schema and implements [`PublishQuotaStore`] over its own database; every
//!   function here takes the store and does no I/O of its own.
//! - `publish.next` (read) names the jobs that may run now (at most one per account) and, for every other
//!   job, the exact reason and the earliest instant. `publish.quota_get` (read) and `publish.quota_put`
//!   (daemon-only write, human author) read and set an account's limits.
//! - The run path records every first write it is about to send (`before_write`, in the step's first
//!   transaction, so a crash after the send still counts it) and every 429 (`record_rate_limit`). It
//!   REFUSES a first write whose account `publish.next` would not allow, but only for an account that was
//!   opted in with `publish.quota_put`; any other account behaves exactly as before the pacing existed.
use std::collections::{HashMap, HashSet};
use std::io;
use std::time::{Duration, UNIX_EPOCH};

use rightkit_http::{defer_in, QuotaStore};
use serde_json::{json, Value};

use crate::calendar_time as time;
use crate::jobs::PublishJobStore;
use crate::protocol::{
    PublishJob, PublishNextBlocked, PublishNextInput, PublishNextJob, PublishNextOutput,
    PublishQuotaGetInput, PublishQuotaPutInput, PublishQuotaPutOutput, PublishQuotaRecord, Scope,
    PUBLISH_PLATFORMS,
};
use crate::util::{
    bad, db_err, internal, non_empty, parse_input, path_safe, require_channel, require_person,
    to_value,
};
use crate::Fail;

/// The host's OWN caution when an account has no limits set: at least this many seconds between two
/// creates on one account. This is NOT a platform limit and no platform quota is sourced here; it is a
/// deliberately slow starting point that a person may change with `publish.quota_put`. The single place
/// the number lives.
pub const DEFAULT_MIN_GAP_SECS: i64 = 900;
/// The host's OWN caution when an account has no limits set: at most this many creates per UTC day on one
/// account. NOT a platform limit and not sourced from any platform; the single place the number lives.
pub const DEFAULT_MAX_PER_DAY: i64 = 10;

/// Bounds `publish.quota_put` accepts (the store's CHECK constraints repeat them).
const MIN_GAP_LOWEST: i64 = 60;
const MIN_GAP_HIGHEST: i64 = 86_400;
const MAX_PER_DAY_LOWEST: i64 = 1;
const MAX_PER_DAY_HIGHEST: i64 = 100;
/// The most jobs `publish.next` returns in `ready`.
const LIMIT_HIGHEST: i64 = 100;
const DAY_SECS: i64 = 86_400;

/// The UTC day number of an instant.
fn day_of(secs: i64) -> i64 {
    secs.div_euclid(DAY_SECS)
}

/// The first instant after the UTC day `day`.
fn day_end(day: i64) -> i64 {
    (day + 1) * DAY_SECS
}

/// True for the phase that starts a new post: the first write of a job (`create` on a simple platform,
/// `create_container` on a container platform). Pacing gates and counts this write only.
fn is_first_write(phase: &str) -> bool {
    matches!(phase, "create_container" | "create")
}

/// True when the limits are looser than the defaults (a shorter gap or more creates per day).
fn looser_than_defaults(min_gap_secs: i64, max_per_day: i64) -> bool {
    min_gap_secs < DEFAULT_MIN_GAP_SECS || max_per_day > DEFAULT_MAX_PER_DAY
}

/// One account: where its pacing is kept.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct AccountKey {
    pub venture: String,
    pub channel: String,
    pub account: String,
    pub platform: String,
    pub remote_account: String,
}

impl AccountKey {
    /// The account a job belongs to.
    pub fn of(job: &PublishJob) -> Self {
        Self {
            venture: job.scope.venture.clone(),
            channel: job.scope.channel.clone(),
            account: job.scope.account.clone(),
            platform: job.platform.clone(),
            remote_account: job.remote_account.clone(),
        }
    }

    /// The channel scope of the account.
    pub fn scope(&self) -> Scope {
        Scope {
            venture: self.venture.clone(),
            channel: self.channel.clone(),
            account: self.account.clone(),
        }
    }
}

/// A stored pacing row. Instants are epoch seconds (UTC); `count_day` is the UTC day number (epoch
/// seconds divided by 86400) that `day_count` belongs to. A row with `opted_in = false` holds state only
/// (its limit columns are ignored: the defaults in this module apply).
#[derive(Clone, Debug)]
pub struct QuotaRow {
    pub opted_in: bool,
    pub min_gap_secs: i64,
    pub max_per_day: i64,
    pub paused_until: Option<i64>,
    pub last_create: Option<i64>,
    pub count_day: Option<i64>,
    pub day_count: i64,
    pub rate_limited_at: Option<i64>,
    pub rate_limited_until: Option<i64>,
    pub updated_at: String,
}

impl QuotaRow {
    /// A state-only row: not opted in, no history.
    pub fn fresh(stamp: &str) -> Self {
        Self {
            opted_in: false,
            min_gap_secs: DEFAULT_MIN_GAP_SECS,
            max_per_day: DEFAULT_MAX_PER_DAY,
            paused_until: None,
            last_create: None,
            count_day: None,
            day_count: 0,
            rate_limited_at: None,
            rate_limited_until: None,
            updated_at: stamp.to_string(),
        }
    }
}

/// A queued job as the store lists it for [`next`], before its instants are parsed.
#[derive(Clone, Debug)]
pub struct QueuedJob {
    pub job_id: String,
    pub account: String,
    pub platform: String,
    pub remote_account: String,
    pub entry_id: String,
    pub phase: String,
    pub state: String,
    pub next_attempt_at: Option<String>,
    pub created_at: String,
    pub reconciles: i64,
    pub inflight: Option<String>,
    /// The calendar entry's timezone (None: the job has no entry row).
    pub timezone: Option<String>,
    /// The calendar entry's local time.
    pub local_time: Option<String>,
    /// The calendar entry's stored UTC instant.
    pub stored_at: Option<String>,
}

/// What the host app provides on top of [`PublishJobStore`]: the pacing rows and the send ledger, the jobs
/// the selector reads and the accounts store's readiness verdict. The host keeps the
/// schema and the SQL; every method is one read or one write on its own database. Methods that write
/// run on the caller's connection and so inside the caller's open transaction.
pub trait PublishQuotaStore: PublishJobStore {
    /// The pacing row of an account, if stored.
    fn load_row(&self, key: &AccountKey) -> Result<Option<QuotaRow>, Fail>;
    /// Inserts or replaces the pacing row of an account.
    fn store_row(&self, key: &AccountKey, row: &QuotaRow) -> Result<(), Fail>;
    /// True when `job_id` already has a ledger row for reconcile round `round`.
    fn round_counted(&self, job_id: &str, round: i64) -> Result<bool, Fail>;
    /// True when a job other than `except_job` on the account is `outcome_unknown` or has a request in
    /// flight.
    fn unknown_on_account(&self, key: &AccountKey, except_job: &str) -> Result<bool, Fail>;
    /// Writes the ledger row `(job_id, round)` unless it exists; true when it was newly written.
    fn insert_send(
        &self,
        job_id: &str,
        round: i64,
        key: &AccountKey,
        utc_day: i64,
        sent_secs: i64,
    ) -> Result<bool, Fail>;
    /// The UTC day of the ledger row `(job_id, round)`, if any.
    fn send_day(&self, job_id: &str, round: i64) -> Result<Option<i64>, Fail>;
    /// Removes the ledger row `(job_id, round)`.
    fn delete_send(&self, job_id: &str, round: i64) -> Result<(), Fail>;
    /// The newest `sent_secs` in the ledger for the account.
    fn newest_send(&self, key: &AccountKey) -> Result<Option<i64>, Fail>;
    /// The account row's `rate_limited_until` (epoch seconds), if the row exists and has one.
    fn rate_limited_until(&self, key: &AccountKey) -> Result<Option<i64>, Fail>;
    /// Raises the account row's `rate_limited_until` to at least `secs` (the row must exist).
    fn merge_rate_limited_until(&self, key: &AccountKey, secs: i64) -> Result<(), Fail>;
    /// Every non-finished job (`queued`, `running`, `waiting`, `outcome_unknown`) of one venture and
    /// channel, oldest first (by `created_at`, then job id), with its calendar entry's timezone, local time
    /// and stored instant.
    fn queued_jobs(&self, venture: &str, channel: &str) -> Result<Vec<QueuedJob>, Fail>;
    /// The accounts store's `account.get` answer for `input` (`scope`, `platform`, `now`): the JSON the
    /// readiness rule is read from. `Err(("not_found", _))` for no stored account.
    fn account_get(&self, input: Value) -> Result<Value, Fail>;
}

/// The limits in force: the stored ones for an opted-in account, the defaults otherwise.
fn limits(row: Option<&QuotaRow>) -> (i64, i64) {
    match row {
        Some(r) if r.opted_in => (r.min_gap_secs, r.max_per_day),
        _ => (DEFAULT_MIN_GAP_SECS, DEFAULT_MAX_PER_DAY),
    }
}

/// Creates counted in the UTC day `now_day`. A count that belongs to an earlier day is zero; a count that
/// belongs to a LATER day (a `now` that went backwards) still stands, so a backwards clock never loosens.
fn count_at(row: Option<&QuotaRow>, now_day: i64) -> i64 {
    match row {
        Some(r) => match r.count_day {
            Some(day) if day >= now_day => r.day_count,
            _ => 0,
        },
        None => 0,
    }
}

/// The instant the day count in force stops applying: the end of the later of the count's day and today.
fn count_resets_at(row: Option<&QuotaRow>, now_day: i64) -> i64 {
    let day = row
        .and_then(|r| r.count_day)
        .map_or(now_day, |d| d.max(now_day));
    day_end(day)
}

/// Why a job may not run now. The order of the variants is the order they are reported in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reason {
    NotDue,
    OutcomeUnknown,
    NotReady,
    Paused,
    RateLimited,
    DailyCap,
    Gap,
}

impl Reason {
    fn code(self) -> &'static str {
        match self {
            Self::NotDue => "not_due",
            Self::OutcomeUnknown => "outcome_unknown_on_account",
            Self::NotReady => "account_not_ready",
            Self::Paused => "account_paused",
            Self::RateLimited => "rate_limited_until",
            Self::DailyCap => "daily_cap",
            Self::Gap => "account_gap",
        }
    }
}

/// One reason with the instant it ends (None: it ends when a person or another event acts).
struct Blocker {
    reason: Reason,
    until: Option<i64>,
    detail: String,
}

/// What the accounts store says about an account, through its own readiness rule (`account.get`).
struct Readiness {
    can_publish: bool,
    blockers: Vec<String>,
    cooldown_until: Option<i64>,
}

/// The accounts store's verdict for `(scope, platform)` at `now`. The rule is `accounts.rs`'s
/// (`readiness_of`, reached through `account.get`); nothing is re-implemented. No stored account is not
/// ready (`no_account_record`): readiness is never assumed.
fn read_readiness(
    store: &dyn PublishQuotaStore,
    scope: &Scope,
    platform: &str,
    now: i64,
) -> Result<Readiness, Fail> {
    let input = json!({
        "scope": {
            "venture": scope.venture,
            "channel": scope.channel,
            "account": scope.account,
        },
        "platform": platform,
        "now": time::format_instant(now),
    });
    match store.account_get(input) {
        Ok(value) => {
            let readiness = value.get("readiness");
            let can_publish = readiness
                .and_then(|r| r.get("can_publish"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut blockers: Vec<String> = readiness
                .and_then(|r| r.get("blockers"))
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if !can_publish && blockers.is_empty() {
                blockers.push("readiness_unreadable".to_string());
            }
            let cooldown_until = value
                .get("quota_cooldown_until")
                .and_then(Value::as_str)
                .map(|s| time::parse_instant(s, "quota_cooldown_until"))
                .transpose()?;
            Ok(Readiness {
                can_publish,
                blockers,
                cooldown_until,
            })
        }
        Err(("not_found", _)) => Ok(Readiness {
            can_publish: false,
            blockers: vec!["no_account_record".to_string()],
            cooldown_until: None,
        }),
        Err(fail) => Err(fail),
    }
}

/// A job as the selector sees it.
struct Cand {
    job_id: String,
    scope: Scope,
    platform: String,
    remote_account: String,
    entry_id: String,
    phase: String,
    state: String,
    next_attempt: Option<i64>,
    created: i64,
    /// The calendar entry's scheduled instant, derived from its local time and timezone (None: no entry row).
    entry_at: Option<i64>,
    reconciles: i64,
    inflight: bool,
}

impl Cand {
    fn from_job(job: &PublishJob) -> Result<Self, Fail> {
        Ok(Self {
            job_id: job.job_id.clone(),
            scope: job.scope.clone(),
            platform: job.platform.clone(),
            remote_account: job.remote_account.clone(),
            entry_id: job.entry_id.clone(),
            phase: job.phase.clone(),
            state: job.state.clone(),
            next_attempt: job
                .next_attempt_at
                .as_deref()
                .map(|s| time::parse_instant(s, "next_attempt_at"))
                .transpose()?,
            created: time::parse_instant(&job.created_at, "created_at")?,
            entry_at: None,
            reconciles: job.reconciles,
            inflight: job.inflight.is_some(),
        })
    }

    fn key(&self) -> AccountKey {
        AccountKey {
            venture: self.scope.venture.clone(),
            channel: self.scope.channel.clone(),
            account: self.scope.account.clone(),
            platform: self.platform.clone(),
            remote_account: self.remote_account.clone(),
        }
    }

    /// The job's scheduled time: when a waiting job is next due, else the calendar entry's local time in its
    /// timezone as a UTC instant; the enqueue instant only for a job whose entry has no schedule.
    fn scheduled(&self) -> i64 {
        match (self.state.as_str(), self.next_attempt) {
            ("waiting", Some(at)) => at,
            _ => self.entry_at.unwrap_or(self.created),
        }
    }

    fn next_job(&self) -> PublishNextJob {
        PublishNextJob {
            job_id: self.job_id.clone(),
            scope: self.scope.clone(),
            platform: self.platform.clone(),
            remote_account: self.remote_account.clone(),
            entry_id: self.entry_id.clone(),
            state: self.state.clone(),
            phase: self.phase.clone(),
            scheduled_at: time::format_instant(self.scheduled()),
        }
    }
}

/// Everything the decision needs besides the job.
struct Facts<'a> {
    now: i64,
    row: Option<&'a QuotaRow>,
    ready: &'a Readiness,
    /// An `outcome_unknown` job (or one with a request in flight) exists on the same account.
    unknown: bool,
    /// This job's create was already counted in its current reconcile round (a retry after a clean failure).
    counted: bool,
}

/// Every reason `cand` may not run now, in report order; empty means it may. The one rule behind both
/// `publish.next` and the refusal on the direct run path.
fn blockers_for(cand: &Cand, f: &Facts<'_>) -> Vec<Blocker> {
    let now = f.now;
    let (gap, cap) = limits(f.row);
    let mut out = Vec::new();
    if let Some(at) = cand.next_attempt.filter(|at| *at > now) {
        out.push(Blocker {
            reason: Reason::NotDue,
            until: Some(at),
            detail: format!("not due until {}", time::format_instant(at)),
        });
    }
    if f.unknown {
        out.push(Blocker {
            reason: Reason::OutcomeUnknown,
            until: None,
            detail: "an outcome_unknown job (or one with a request in flight) exists on this account: nothing else is sent on it until a person resolves it with publish.resolve_unknown".to_string(),
        });
    }
    if !f.ready.can_publish {
        let only_cooldown = f.ready.blockers.len() == 1 && f.ready.blockers[0] == "cooldown";
        out.push(Blocker {
            reason: Reason::NotReady,
            until: if only_cooldown {
                f.ready.cooldown_until
            } else {
                None
            },
            detail: format!(
                "the accounts store says this account cannot publish: {}",
                f.ready.blockers.join(", ")
            ),
        });
    }
    if let Some(at) = f.row.and_then(|r| r.paused_until).filter(|at| *at > now) {
        out.push(Blocker {
            reason: Reason::Paused,
            until: Some(at),
            detail: format!("paused until {}", time::format_instant(at)),
        });
    }
    if let Some(at) = f
        .row
        .and_then(|r| r.rate_limited_until)
        .filter(|at| *at > now)
    {
        out.push(Blocker {
            reason: Reason::RateLimited,
            until: Some(at),
            detail: format!(
                "the platform asked this account to wait until {}",
                time::format_instant(at)
            ),
        });
    }
    if !f.counted {
        let now_day = day_of(now);
        let today = count_at(f.row, now_day);
        if today >= cap {
            out.push(Blocker {
                reason: Reason::DailyCap,
                until: Some(count_resets_at(f.row, now_day)),
                detail: format!("{today} creates counted today, the limit is {cap} per UTC day"),
            });
        }
        if let Some(last) = f.row.and_then(|r| r.last_create) {
            if last + gap > now {
                out.push(Blocker {
                    reason: Reason::Gap,
                    until: Some(last + gap),
                    detail: format!(
                        "the last create on this account was at {}; the gap is {gap} s",
                        time::format_instant(last)
                    ),
                });
            }
        }
    }
    out
}

/// The reason a second job of an account must wait once `selected` is chosen to run now: the day cap if
/// that send would reach it, else the gap measured from now. A lower bound, never a promise.
fn after_send(now: i64, row: Option<&QuotaRow>, selected: &str) -> Blocker {
    let (gap, cap) = limits(row);
    let now_day = day_of(now);
    if count_at(row, now_day) + 1 >= cap {
        Blocker {
            reason: Reason::DailyCap,
            until: Some(count_resets_at(row, now_day)),
            detail: format!(
                "job {selected} is selected for this account now; its create reaches the limit of {cap} per UTC day"
            ),
        }
    } else {
        Blocker {
            reason: Reason::Gap,
            until: Some(now + gap),
            detail: format!(
                "job {selected} is selected for this account now; a second create waits the gap of {gap} s"
            ),
        }
    }
}

/// The earliest instant every blocker has passed; None when any of them has no instant.
fn earliest(blockers: &[Blocker]) -> Option<i64> {
    if blockers.iter().any(|b| b.until.is_none()) {
        None
    } else {
        blockers.iter().filter_map(|b| b.until).max()
    }
}

fn blocked_entry(
    cand: &Cand,
    blockers: &[Blocker],
    behind: Option<String>,
) -> Option<PublishNextBlocked> {
    let primary = blockers.first()?;
    Some(PublishNextBlocked {
        job_id: cand.job_id.clone(),
        scope: cand.scope.clone(),
        platform: cand.platform.clone(),
        remote_account: cand.remote_account.clone(),
        entry_id: cand.entry_id.clone(),
        state: cand.state.clone(),
        phase: cand.phase.clone(),
        scheduled_at: time::format_instant(cand.scheduled()),
        reason: primary.reason.code().to_string(),
        reasons: blockers
            .iter()
            .map(|b| b.reason.code().to_string())
            .collect(),
        until: earliest(blockers).map(time::format_instant),
        detail: primary.detail.clone(),
        behind,
    })
}

/// How the run path treats pacing for one call. Built once by [`pace_for`].
pub struct Pace {
    /// Some: the job's account was opted in, so its first write is checked (with this readiness).
    enforce: Option<Readiness>,
}

impl Pace {
    /// No enforcement: the send is still recorded, nothing is refused.
    pub fn off() -> Self {
        Self { enforce: None }
    }
}

/// Reads `ignore_pacing` from a `publish.step` / `publish.run` input. `true` is accepted only in the
/// daemon-dev role (`PublishJobStore::require_role`; the command table has already refused every other role, this repeats
/// the check where the flag is read). Anything but a boolean is `bad_input`.
pub fn ignore_pacing(store: &dyn PublishQuotaStore, input: &Value) -> Result<bool, Fail> {
    match input.get("ignore_pacing") {
        None | Some(Value::Null | Value::Bool(false)) => Ok(false),
        Some(Value::Bool(true)) => {
            store.require_role()?;
            Ok(true)
        }
        Some(_) => Err(bad("ignore_pacing must be true or false")),
    }
}

/// The pacing mode of one direct call. Pacing is enforced only when ALL hold: `ignore_pacing` is not set,
/// the job exists and is at its first write, and its account has a stored pacing record that a person
/// opted in with `publish.quota_put`. Otherwise [`Pace::off`] (exactly the behaviour before pacing). The
/// accounts store's readiness is read here, outside any transaction.
pub fn pace_for(
    store: &dyn PublishQuotaStore,
    scope: &Scope,
    job_id: &str,
    now: i64,
    ignore: bool,
) -> Result<Pace, Fail> {
    if ignore {
        return Ok(Pace::off());
    }
    let Some(job) = store.load_job(scope, job_id)? else {
        return Ok(Pace::off());
    };
    if !is_first_write(&job.phase) && job.phase != "publish" {
        return Ok(Pace::off());
    }
    if !store
        .load_row(&AccountKey::of(&job))?
        .is_some_and(|r| r.opted_in)
    {
        return Ok(Pace::off());
    }
    let ready = read_readiness(store, &job.scope, &job.platform, now)?;
    Ok(Pace {
        enforce: Some(ready),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The job's first write: counted once per reconcile round.
    Create,
    /// A later write of a container job (its `publish`): moves the last-create instant only.
    Publish,
}

/// Called by the step's first transaction for a write phase, after the approval and pause checks and
/// before the in-flight marker. For the first write of an opted-in account it refuses (`Err`, the
/// reason's code) a job `publish.next` would not allow now. Then it records the send. Counted here
/// (before the request, in the same commit as the marker) so a crash after the send cannot lose it; a
/// step that then refuses locally (an over-long text) has still counted, which only ever tightens pacing.
pub fn before_write(
    store: &dyn PublishQuotaStore,
    job: &PublishJob,
    now: i64,
    pace: &Pace,
) -> Result<(), Fail> {
    if is_first_write(&job.phase) {
        if let Some(ready) = &pace.enforce {
            enforce(store, job, now, ready, false)?;
        }
        record_send(store, job, now, Kind::Create)
    } else if job.phase == "publish" {
        // The container's second write is charged to the job's own reservation (its create was counted in
        // this round): the not-due, outcome, readiness, pause and rate-limit gates apply; the cap and gap do not.
        if let Some(ready) = &pace.enforce {
            enforce(store, job, now, ready, true)?;
        }
        record_send(store, job, now, Kind::Publish)
    } else {
        Ok(())
    }
}

/// The direct-path refusal: the same blockers as `publish.next`, the first one's code as the error code.
/// `reserved` says the write is charged to a create already counted for this job's round, so the cap and
/// the gap are not checked again.
fn enforce(
    store: &dyn PublishQuotaStore,
    job: &PublishJob,
    now: i64,
    ready: &Readiness,
    reserved: bool,
) -> Result<(), Fail> {
    let cand = Cand::from_job(job)?;
    let key = cand.key();
    let row = store.load_row(&key)?;
    let facts = Facts {
        now,
        row: row.as_ref(),
        ready,
        unknown: store.unknown_on_account(&key, &job.job_id)?,
        counted: reserved || store.round_counted(&job.job_id, job.reconciles)?,
    };
    let blockers = blockers_for(&cand, &facts);
    let Some(first) = blockers.first() else {
        return Ok(());
    };
    let when = earliest(&blockers).map_or_else(
        || "no earliest instant is known".to_string(),
        |at| format!("earliest {}", time::format_instant(at)),
    );
    Err((
        first.reason.code(),
        format!(
            "job {} may not send its create now: {} ({when}); no request was made. publish.next shows when it may run",
            job.job_id, first.detail
        ),
    ))
}

/// Records that a write of `job` is about to be sent on its account.
fn record_send(
    store: &dyn PublishQuotaStore,
    job: &PublishJob,
    now: i64,
    kind: Kind,
) -> Result<(), Fail> {
    let key = AccountKey::of(job);
    let stamp = time::format_instant(now);
    let mut row = store
        .load_row(&key)?
        .unwrap_or_else(|| QuotaRow::fresh(&stamp));
    let now_day = day_of(now);
    if kind == Kind::Create {
        let newly_counted = store.insert_send(&job.job_id, job.reconciles, &key, now_day, now)?;
        if newly_counted {
            let held = row.count_day.filter(|day| *day >= now_day);
            row.day_count = if held.is_some() { row.day_count + 1 } else { 1 };
            row.count_day = Some(held.unwrap_or(now_day));
        }
    }
    row.last_create = Some(row.last_create.map_or(now, |last| last.max(now)));
    row.updated_at = stamp;
    store.store_row(&key, &row)
}

/// Un-counts the first write of `job` after its step refused it locally: the step sent nothing, so the create
/// must not use the day's cap. Removes this round's counted row, takes its day off the day count when it is
/// the held day, and sets the last create to the newest send that remains. A job whose create was never
/// counted, or whose phase is not a first write, changes nothing.
pub fn uncount_refused(
    store: &dyn PublishQuotaStore,
    job: &PublishJob,
    now: i64,
) -> Result<(), Fail> {
    if !is_first_write(&job.phase) {
        return Ok(());
    }
    let key = AccountKey::of(job);
    let counted_day = store.send_day(&job.job_id, job.reconciles)?;
    let Some(day) = counted_day else {
        return Ok(());
    };
    store.delete_send(&job.job_id, job.reconciles)?;
    let stamp = time::format_instant(now);
    let mut row = store
        .load_row(&key)?
        .unwrap_or_else(|| QuotaRow::fresh(&stamp));
    if row.count_day == Some(day) && row.day_count > 0 {
        row.day_count -= 1;
    }
    row.last_create = store.newest_send(&key)?;
    row.updated_at = stamp;
    store.store_row(&key, &row)
}

/// Records a 429 (or the platform's equivalent) seen for `job`'s account at `now`: the account must wait
/// until `until`. A signal never shortens a wait already in force.
pub fn record_rate_limit(
    store: &dyn PublishQuotaStore,
    job: &PublishJob,
    now: i64,
    until: i64,
) -> Result<(), Fail> {
    let key = AccountKey::of(job);
    let stamp = time::format_instant(now);
    let mut row = store
        .load_row(&key)?
        .unwrap_or_else(|| QuotaRow::fresh(&stamp));
    row.rate_limited_at = Some(row.rate_limited_at.map_or(now, |at| at.max(now)));
    row.updated_at = stamp;
    store.store_row(&key, &row)?;
    // The wait is max-merged by rightkit-http's `defer_in` over this account's row, on the caller's connection
    // and so its transaction: a shorter signal never shortens a wait already in force.
    let mut row_store = RowQuotaStore { store, key: &key };
    let start = UNIX_EPOCH + Duration::from_secs(now.max(0) as u64);
    let wait = Duration::from_secs(until.saturating_sub(now).max(0) as u64);
    defer_in(&mut row_store, RATE_LIMIT_KEY, wait, start).map_err(db_err)?;
    Ok(())
}

/// The provider key `defer_in` is given; the row itself is the account, so the key only has to be non-empty.
const RATE_LIMIT_KEY: &str = "rate_limited_until";

/// `rightkit_http::QuotaStore` over one account's pacing row (its `rate_limited_until`). The row must
/// exist (`record_rate_limit` stores it first). Millisecond values round up to whole seconds.
struct RowQuotaStore<'a> {
    store: &'a dyn PublishQuotaStore,
    key: &'a AccountKey,
}

impl QuotaStore for RowQuotaStore<'_> {
    fn get_not_before_ms(&self, _provider: &str) -> io::Result<Option<u64>> {
        let secs = self
            .store
            .rate_limited_until(self.key)
            .map_err(|(_, message)| io::Error::other(message))?;
        Ok(secs
            .and_then(|s| u64::try_from(s).ok())
            .map(|s| s.saturating_mul(1000)))
    }

    fn merge_not_before_ms(&mut self, _provider: &str, not_before_ms: u64) -> io::Result<()> {
        let secs = i64::try_from(not_before_ms.div_ceil(1000)).unwrap_or(i64::MAX);
        self.store
            .merge_rate_limited_until(self.key, secs)
            .map_err(|(_, message)| io::Error::other(message))
    }
}

fn parse_now(now: &str) -> Result<i64, Fail> {
    time::parse_instant(now, "now")
}

/// The pacing of one account at `now`, as the commands return it.
fn record_view(key: &AccountKey, row: Option<&QuotaRow>, now: i64) -> PublishQuotaRecord {
    let (gap, cap) = limits(row);
    let now_day = day_of(now);
    PublishQuotaRecord {
        scope: key.scope(),
        platform: key.platform.clone(),
        remote_account: key.remote_account.clone(),
        opted_in: row.is_some_and(|r| r.opted_in),
        min_gap_secs: gap,
        max_per_day: cap,
        default_min_gap_secs: DEFAULT_MIN_GAP_SECS,
        default_max_per_day: DEFAULT_MAX_PER_DAY,
        paused_until: row.and_then(|r| r.paused_until).map(time::format_instant),
        last_create_at: row.and_then(|r| r.last_create).map(time::format_instant),
        creates_today: count_at(row, now_day),
        day_resets_at: time::format_instant(count_resets_at(row, now_day)),
        rate_limited_at: row
            .and_then(|r| r.rate_limited_at)
            .map(time::format_instant),
        rate_limited_until: row
            .and_then(|r| r.rate_limited_until)
            .map(time::format_instant),
        updated_at: row.map(|r| r.updated_at.clone()),
    }
}

/// `publish.quota_get`: one account's pacing at `now`. Read only; an account nobody set or sent on shows
/// the defaults with `opted_in: false`.
pub fn quota_get(store: &dyn PublishQuotaStore, input: Value) -> Result<Value, Fail> {
    let req: PublishQuotaGetInput = parse_input(input, "publish.quota_get")?;
    let now = parse_now(&req.now)?;
    path_safe(&req.remote_account, "remote_account")?;
    require_channel(store, &req.scope)?;
    let key = AccountKey {
        venture: req.scope.venture.clone(),
        channel: req.scope.channel.clone(),
        account: req.scope.account.clone(),
        platform: req.platform.clone(),
        remote_account: req.remote_account.clone(),
    };
    let row = store.load_row(&key)?;
    to_value(&record_view(&key, row.as_ref(), now))
}

/// `publish.quota_put`: a person sets an account's three limits, which also opts the account in to
/// enforcement on the direct run path. Daemon-only (the command table). Order of refusals: input shape,
/// author (`author_forbidden` for an agent), platform and account shape, bounds, `now` and the pause
/// instant, then looser-than-defaults without `confirm_looser` (`confirm_looser_required`). The create
/// history and the rate-limit signal of the account are kept.
pub fn quota_put(store: &dyn PublishQuotaStore, input: Value) -> Result<Value, Fail> {
    let req: PublishQuotaPutInput = parse_input(input, "publish.quota_put")?;
    let set_by = require_person(
        "set_by",
        &req.set_by,
        "changes how fast an account may publish",
    )?;
    if !PUBLISH_PLATFORMS.contains(&req.platform.as_str()) {
        return Err(bad(format!(
            "platform must be one of {}, got {}",
            PUBLISH_PLATFORMS.join(", "),
            req.platform
        )));
    }
    path_safe(&req.remote_account, "remote_account")?;
    non_empty(&req.scope.venture, "scope.venture")?;
    if !(MIN_GAP_LOWEST..=MIN_GAP_HIGHEST).contains(&req.min_gap_secs) {
        return Err(bad(format!(
            "min_gap_secs must be between {MIN_GAP_LOWEST} and {MIN_GAP_HIGHEST}, got {}",
            req.min_gap_secs
        )));
    }
    if !(MAX_PER_DAY_LOWEST..=MAX_PER_DAY_HIGHEST).contains(&req.max_per_day) {
        return Err(bad(format!(
            "max_per_day must be between {MAX_PER_DAY_LOWEST} and {MAX_PER_DAY_HIGHEST}, got {}",
            req.max_per_day
        )));
    }
    let now = parse_now(&req.now)?;
    let paused_until = req
        .paused_until
        .as_deref()
        .map(|s| time::parse_instant(s, "paused_until"))
        .transpose()?;
    let looser = looser_than_defaults(req.min_gap_secs, req.max_per_day);
    if looser && !req.confirm_looser {
        return Err((
            "confirm_looser_required",
            format!(
                "min_gap_secs {} / max_per_day {} is looser than the defaults ({} s / {} per day): send confirm_looser: true to set it",
                req.min_gap_secs, req.max_per_day, DEFAULT_MIN_GAP_SECS, DEFAULT_MAX_PER_DAY
            ),
        ));
    }
    let stamp = time::format_instant(now);
    let key = AccountKey {
        venture: req.scope.venture.clone(),
        channel: req.scope.channel.clone(),
        account: req.scope.account.clone(),
        platform: req.platform.clone(),
        remote_account: req.remote_account.clone(),
    };
    let mut stored: Option<QuotaRow> = None;
    store.write_txn(&mut || {
        require_channel(store, &req.scope)?;
        let mut row = store
            .load_row(&key)?
            .unwrap_or_else(|| QuotaRow::fresh(&stamp));
        row.opted_in = true;
        row.min_gap_secs = req.min_gap_secs;
        row.max_per_day = req.max_per_day;
        row.paused_until = paused_until;
        row.updated_at = stamp.clone();
        store.store_row(&key, &row)?;
        stored = Some(row);
        Ok(())
    })?;
    let row = stored.ok_or_else(|| internal("quota_put wrote no row"))?;
    to_value(&PublishQuotaPutOutput {
        record: record_view(&key, Some(&row), now),
        looser_than_defaults: looser,
        set_by,
    })
}

/// The entry's scheduled instant from its local time and timezone. A wall time that occurs twice (clocks go
/// back) takes the instant the entry was stored with; a zone the table cannot resolve falls back to the stored
/// UTC instant; no entry gives None.
fn entry_instant(tz: Option<&str>, local: Option<&str>, stored: Option<&str>) -> Option<i64> {
    let stored_at = stored.and_then(|s| time::parse_instant(s, "scheduled_at").ok());
    match (tz, local) {
        (Some(tz), Some(local)) => time::to_utc(tz, local, None).ok().or(stored_at),
        _ => stored_at,
    }
}

/// Every non-finished job of one venture and channel, oldest first.
fn load_jobs(
    store: &dyn PublishQuotaStore,
    venture: &str,
    channel: &str,
) -> Result<Vec<Cand>, Fail> {
    let rows = store.queued_jobs(venture, channel)?;
    let mut out = Vec::new();
    for raw in rows {
        out.push(Cand {
            job_id: raw.job_id,
            scope: Scope {
                venture: venture.to_string(),
                channel: channel.to_string(),
                account: raw.account,
            },
            platform: raw.platform,
            remote_account: raw.remote_account,
            entry_id: raw.entry_id,
            phase: raw.phase,
            state: raw.state,
            next_attempt: raw
                .next_attempt_at
                .as_deref()
                .map(|s| time::parse_instant(s, "next_attempt_at"))
                .transpose()?,
            created: time::parse_instant(&raw.created_at, "created_at")?,
            entry_at: entry_instant(
                raw.timezone.as_deref(),
                raw.local_time.as_deref(),
                raw.stored_at.as_deref(),
            ),
            reconciles: raw.reconciles,
            inflight: raw.inflight.is_some(),
        });
    }
    Ok(out)
}

/// `publish.next`: which queued or due-waiting jobs of a venture and channel may run now. Read only, no
/// side effect, a pure function of the store and `now`.
///
/// `ready` holds at most one job per account (the oldest scheduled one that nothing blocks; ties by job
/// id), in that order. `blocked` holds every other queued or waiting job at its first write, with the
/// exact reason, every reason that applies and the earliest instant. A job that nothing blocks but that
/// sits behind a selected job of the same account is `account_gap` (or `daily_cap` if the selected send
/// would reach the cap) with `behind` naming the selected job. `in_progress` lists the jobs past their
/// first write, which pacing does not hold back. An `outcome_unknown` job, or a job with a request in
/// flight, blocks every other job of its account.
pub fn next(store: &dyn PublishQuotaStore, input: Value) -> Result<Value, Fail> {
    let req: PublishNextInput = parse_input(input, "publish.next")?;
    let now = parse_now(&req.now)?;
    non_empty(&req.scope.venture, "scope.venture")?;
    non_empty(&req.scope.channel, "scope.channel")?;
    if let Some(limit) = req.limit {
        if !(1..=LIMIT_HIGHEST).contains(&limit) {
            return Err(bad(format!("limit must be between 1 and {LIMIT_HIGHEST}")));
        }
    }
    let known = store.channel_known(&req.scope.venture, &req.scope.channel, None)?;
    if !known {
        return Err((
            "foreign_scope",
            format!(
                "no stored channel for venture={} channel={}",
                req.scope.venture, req.scope.channel
            ),
        ));
    }
    let jobs = load_jobs(store, &req.scope.venture, &req.scope.channel)?;
    let unknown: HashSet<AccountKey> = jobs
        .iter()
        .filter(|j| j.state == "outcome_unknown" || j.inflight)
        .map(Cand::key)
        .collect();
    let mut candidates: Vec<&Cand> = jobs
        .iter()
        .filter(|j| {
            !j.inflight
                && matches!(j.state.as_str(), "queued" | "waiting")
                && is_first_write(&j.phase)
        })
        .collect();
    candidates.sort_by(|a, b| {
        a.scheduled()
            .cmp(&b.scheduled())
            .then_with(|| a.created.cmp(&b.created))
            .then_with(|| a.job_id.cmp(&b.job_id))
    });
    let mut in_progress: Vec<&Cand> = jobs
        .iter()
        .filter(|j| {
            !j.inflight
                && matches!(j.state.as_str(), "queued" | "running" | "waiting")
                && !is_first_write(&j.phase)
        })
        .collect();
    in_progress.sort_by(|a, b| {
        a.scheduled()
            .cmp(&b.scheduled())
            .then_with(|| a.created.cmp(&b.created))
            .then_with(|| a.job_id.cmp(&b.job_id))
    });

    let mut rows: HashMap<AccountKey, Option<QuotaRow>> = HashMap::new();
    let mut readiness: HashMap<(String, String), Readiness> = HashMap::new();
    let mut selected: HashMap<AccountKey, String> = HashMap::new();
    let mut ready: Vec<PublishNextJob> = Vec::new();
    let mut blocked: Vec<PublishNextBlocked> = Vec::new();
    for cand in candidates {
        let key = cand.key();
        if !rows.contains_key(&key) {
            let loaded = store.load_row(&key)?;
            rows.insert(key.clone(), loaded);
        }
        let rkey = (cand.scope.account.clone(), cand.platform.clone());
        if !readiness.contains_key(&rkey) {
            let verdict = read_readiness(store, &cand.scope, &cand.platform, now)?;
            readiness.insert(rkey.clone(), verdict);
        }
        let Some(verdict) = readiness.get(&rkey) else {
            return Err(internal("readiness cache lost an entry"));
        };
        let row = rows.get(&key).and_then(|r| r.as_ref());
        let facts = Facts {
            now,
            row,
            ready: verdict,
            unknown: unknown.contains(&key),
            counted: store.round_counted(&cand.job_id, cand.reconciles)?,
        };
        let mut blockers = blockers_for(cand, &facts);
        let mut behind = None;
        if blockers.is_empty() {
            match selected.get(&key).cloned() {
                Some(first) => {
                    blockers.push(after_send(now, row, &first));
                    behind = Some(first);
                }
                None => {
                    selected.insert(key, cand.job_id.clone());
                    ready.push(cand.next_job());
                    continue;
                }
            }
        }
        if let Some(entry) = blocked_entry(cand, &blockers, behind) {
            blocked.push(entry);
        }
    }
    let ready_total = i64::try_from(ready.len()).map_err(internal)?;
    if let Some(limit) = req.limit {
        ready.truncate(usize::try_from(limit).map_err(internal)?);
    }
    to_value(&PublishNextOutput {
        now: time::format_instant(now),
        ready,
        ready_total,
        blocked,
        in_progress: in_progress.iter().map(|c| c.next_job()).collect(),
    })
}
