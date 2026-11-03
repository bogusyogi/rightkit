//! Job runner (§3, §4.3): one process at a time drives a job, holding an
//! exclusive per-job lock file.

use std::io::Write as _;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use rightkit_job_queue::{JobError, JobLocks, Resolution, Submission};

use crate::providers::{GenRequest, Output, PollOutcome, Provider, ProviderError, SubmitOutcome};
use crate::store::{self, je, rk_err, Db, Job, SUBMITTED};
use crate::{credentials, descriptor, ledger, Context, GenError, GenResult};

/// Resubmissions after confirmed provider failures are bounded. Only an idempotent adapter is
/// resubmitted automatically; a paid, non-idempotent job that the provider CONFIRMS failed ends
/// `failed` with its reservation released and the user retries explicitly.
pub const MAX_ATTEMPTS: i64 = 3;

/// Extra sends of the SAME attempt after a failure where the request provably never left (name
/// lookup, TCP connect, TLS handshake). Such a failure consumes no paid attempt and opens no
/// second reservation; this separate small bound, with doubling backoff from `poll_initial`
/// (capped at `poll_max`), ends in a failed `transport` job with the reservation released.
pub const MAX_CONNECT_RETRIES: u32 = 3;

/// A submit failure that provably sent nothing: retryable, not ambiguous, and a transport error.
/// (Providers mark `ambiguous` from `request_may_have_reached`; see providers/mod.rs.)
fn request_never_left(e: &ProviderError) -> bool {
    e.retryable && !e.ambiguous && e.code == "transport"
}

/// A paid submit that may already be running remotely is never sent twice
/// without the user's say-so: the job is held as `outcome_unknown`, its
/// reservation kept, until `job resolve`.
///
/// A synchronous paid result that arrived but could not be stored in the
/// library is held under the same status with the distinct error code
/// [`RESULT_RECOVERED`]: the bytes are saved under `<data_dir>/recovered`, the
/// attempt is settled as spent, and only `job resolve --adopt` (store the
/// recovered file, no provider call) or `--abandon` leaves it. `--resubmit` is
/// refused for it.
pub const RESULT_RECOVERED: &str = "result_recovered";

/// A paid provider finished the work (or a download of its finished result failed for good) but
/// nothing usable is in hand: no bytes were received, so nothing can be adopted. The job is held
/// under `outcome_unknown` with this error code and the reservation SETTLED at the estimate (the
/// provider did the work). `--resubmit` is refused (the attempt is settled); `--abandon` closes
/// it, and the user requests the media again explicitly.
pub const RESULT_UNUSABLE: &str = "result_unusable";

/// The error code a held job was held with.
fn held_code(job: &Job) -> Option<String> {
    job.error_json
        .as_deref()
        .and_then(|x| serde_json::from_str::<Value>(x).ok())
        .and_then(|e| e["code"].as_str().map(String::from))
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub poll_initial: Duration,
    pub poll_max: Duration,
    /// Bounded total run time; the job stays recoverable afterwards.
    pub total: Duration,
    /// Print each progress event as one JSON line on stderr (always stored
    /// in `job_events` regardless).
    pub stderr_events: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            poll_initial: Duration::from_secs(2),
            poll_max: Duration::from_secs(15),
            total: Duration::from_secs(2 * 60 * 60),
            stderr_events: true,
        }
    }
}

pub use rightkit_job_queue::JobLock;

/// `Ok(None)` when another holder has the lock.
pub fn try_lock(ctx: &Context, job_id: &str) -> GenResult<Option<JobLock>> {
    JobLocks::new(ctx.locks_dir()).try_lock(job_id).map_err(je)
}

pub fn run_job(ctx: &Context, providers: &[Box<dyn Provider>], job_id: &str) -> GenResult<Value> {
    run_job_with(ctx, providers, job_id, &RunOptions::default())
}

fn find<'a>(providers: &'a [Box<dyn Provider>], id: &str) -> GenResult<&'a dyn Provider> {
    providers
        .iter()
        .find(|p| p.id() == id)
        .map(|b| b.as_ref())
        .ok_or_else(|| GenError::new("unknown_provider", format!("unknown provider: {id}")))
}

/// Record one progress event and, when enabled, write it as an NDJSON line
/// to stderr. Progress is best-effort: it never fails the job. The event's
/// status and attempt are the job's current ones.
pub(crate) fn emit(db: &Db, opts: &RunOptions, job_id: &str, stage: &str, message: Option<&str>) {
    if let Ok(ev) = store::add_event(db, job_id, stage, message) {
        if opts.stderr_events {
            let _ = writeln!(std::io::stderr(), "{ev}");
        }
    }
}

/// Like [`emit`] with an explicit event status and attempt.
fn emit_with(
    db: &Db,
    opts: &RunOptions,
    job_id: &str,
    status: &str,
    attempt: i64,
    stage: &str,
    message: Option<&str>,
) {
    if let Ok(ev) = store::add_event_with(db, job_id, status, attempt, stage, message) {
        if opts.stderr_events {
            let _ = writeln!(std::io::stderr(), "{ev}");
        }
    }
}

fn fail_job(db: &Db, opts: &RunOptions, job_id: &str, error: &JobError) -> GenResult<()> {
    store::mark_failed(db, job_id, error)?;
    emit(db, opts, job_id, "failed", Some(&error.message));
    Ok(())
}

fn err_json(code: &str, message: &str) -> JobError {
    JobError::new(code, message)
}

pub fn run_job_with(
    ctx: &Context,
    providers: &[Box<dyn Provider>],
    job_id: &str,
    opts: &RunOptions,
) -> GenResult<Value> {
    let Some(_lock) = try_lock(ctx, job_id)? else {
        return Ok(json!({"job_id": job_id, "status": "locked_elsewhere"}));
    };
    let mut db = store::open(ctx)?;
    let _ = take_provider_cost(); // never carry another job's figure onto this one
    let started = Instant::now();
    let mut delay = opts.poll_initial;
    let mut first = true;
    'run: loop {
        let job = store::get_job(&db, job_id)?;
        if job.is_terminal() {
            if job.status == "cancelled" {
                let req: GenRequest = serde_json::from_str(&job.request_json)
                    .map_err(|e| GenError::new("bad_request", e.to_string()))?;
                find(providers, &job.provider)?
                    .cancel_job(&req, job.provider_job_id.as_deref(), job_id)
                    .map_err(|e| GenError::new(e.code, e.message))?;
            }
            if !first && job.status == "cancelled" {
                emit(&db, opts, job_id, "failed", Some("cancelled"));
            }
            return Ok(job.to_value());
        }
        if job.awaits_adoption() {
            return Err(GenError::new(
                RESULT_RECOVERED,
                format!(
                    "job {job_id} already has its paid result, saved under {}; it is never requested again. Store it with `job resolve {job_id} --adopt` (or `--abandon`)",
                    recovered_dir(ctx).display()
                ),
            ));
        }
        if job.awaits_resolution() && held_code(&job).as_deref() == Some(RESULT_UNUSABLE) {
            return Err(GenError::new(
                RESULT_UNUSABLE,
                format!(
                    "job {job_id}: {} finished the work and is counted as spent, but no usable result is held. It is never requested again automatically; `job resolve {job_id} --abandon` closes it, then request the media again",
                    job.provider
                ),
            ));
        }
        if job.awaits_resolution() {
            return Err(GenError::new(
                "outcome_unknown",
                format!(
                    "job {job_id} may already be running at {}; its attempt {} stays reserved. Check the provider, then `job resolve {job_id} --resubmit` (nothing ran) or `--abandon` (it ran, or may have)",
                    job.provider, job.attempts
                ),
            ));
        }
        if !first && started.elapsed() >= opts.total {
            return recoverable(&db, opts, job_id, "run time limit reached");
        }
        first = false;
        let provider = find(providers, &job.provider)?;
        let req: GenRequest = serde_json::from_str(&job.request_json)
            .map_err(|e| GenError::new("bad_request", format!("stored request unreadable: {e}")))?;
        if provider.needs_key() {
            crate::entitlement::require(crate::entitlement::CLOUD_PROVIDERS)?;
        }
        let key = if provider.needs_key() {
            let Some(key) = credentials::get(ctx, &job.provider)? else {
                // Stays recoverable: it resumes once a key is stored again.
                let message = format!("no API key stored for {}", job.provider);
                store::set_error(&db, job_id, &err_json("no_credentials", &message))?;
                return Err(GenError::new("no_credentials", message));
            };
            key
        } else {
            String::new()
        };

        // One transaction decides: resume by the stored provider id, replay a
        // counted-but-unrecorded attempt, or count a new attempt (the spend
        // reservation commits with it).
        let step = match db.begin_submission(job_id, MAX_ATTEMPTS) {
            Ok(Some(step)) => step,
            Ok(None) => continue, // went terminal meanwhile
            // A cap or the caller's budget refused the reservation: the job ends failed with
            // nothing reserved, and the caller gets the typed error (with `details` for a budget).
            Err(e)
                if e.code == "spend_cap"
                    || e.code == ledger::BUDGET_EXCEEDED
                    || e.code == ledger::BUDGET_INVALID =>
            {
                let (clean, refusal) = ledger::split_refusal(e);
                fail_job(&db, opts, job_id, &clean)?;
                return Err(refusal);
            }
            Err(e) => return Err(je(e)),
        };
        match step {
            Submission::Exhausted { .. } => fail_job(
                &db,
                opts,
                job_id,
                &err_json("max_attempts", "provider failed on every allowed attempt"),
            )?,
            Submission::Submit {
                attempt,
                idempotency_key,
                replay,
            } => {
                let idempotent = provider.submit_is_idempotent();
                if replay && !idempotent {
                    // A previous process counted this attempt and died before the
                    // provider's reply was recorded: the request may have been
                    // delivered, and sending it again could run (and charge) twice.
                    let held = err_json(
                        "outcome_unknown",
                        &format!(
                            "attempt {attempt} was interrupted before {} replied; the provider may hold a paid job for it",
                            job.provider
                        ),
                    );
                    return outcome_unknown(&db, opts, job_id, attempt, &idempotency_key, &held);
                }
                let message = if replay {
                    "resubmitting interrupted attempt"
                } else {
                    "submitting to provider"
                };
                emit(&db, opts, job_id, SUBMITTED, Some(message));
                // A failure where the request provably never left re-sends this same attempt
                // (same reservation, same key) a bounded number of times, then fails the job.
                let mut connect_failures = 0u32;
                let sent = loop {
                    let r = provider.submit_job(&req, &key, job_id, attempt, &idempotency_key);
                    let Err(e) = &r else { break r };
                    if idempotent || !request_never_left(e) {
                        break r;
                    }
                    connect_failures += 1;
                    if connect_failures > MAX_CONNECT_RETRIES {
                        let j = err_json(
                            &e.code,
                            &format!(
                                "{} (the request never left this machine; retried {MAX_CONNECT_RETRIES} times; nothing was charged)",
                                e.message
                            ),
                        );
                        ledger::reconcile(&mut db, job_id, attempt, 0.0)?;
                        fail_job(&db, opts, job_id, &j)?;
                        continue 'run;
                    }
                    emit(
                        &db,
                        opts,
                        job_id,
                        SUBMITTED,
                        Some(&format!("could not reach {} ({}); retrying the same attempt ({connect_failures}/{MAX_CONNECT_RETRIES})", job.provider, e.message)),
                    );
                    let wait = crate::host::http::RetryPolicy {
                        initial_backoff: opts.poll_initial,
                        max_backoff: opts.poll_max,
                        multiplier: 2.0,
                        jitter: 0.0,
                        ..Default::default()
                    }
                    .backoff(connect_failures);
                    let until = Instant::now() + wait;
                    while Instant::now() < until {
                        if store::get_job(&db, job_id)?.is_terminal() {
                            continue 'run;
                        }
                        std::thread::sleep(
                            Duration::from_millis(250)
                                .min(until.saturating_duration_since(Instant::now())),
                        );
                    }
                };
                match sent {
                    Ok(SubmitOutcome::Accepted { provider_job_id }) => {
                        store::set_provider_id(&db, job_id, &provider_job_id)?;
                        emit(&db, opts, job_id, "submitted", Some("accepted by provider"));
                        delay = opts.poll_initial;
                    }
                    Ok(SubmitOutcome::Completed { outputs }) => {
                        if let Some(v) =
                            finalize(ctx, &mut db, opts, provider, &job, attempt, &outputs, true)?
                        {
                            return Ok(v);
                        }
                    }
                    Err(e) => {
                        let j = err_json(&e.code, &e.message);
                        if e.ambiguous && !idempotent {
                            // The request may have been delivered: hold the job and
                            // its reservation; a second send could charge twice.
                            return outcome_unknown(
                                &db,
                                opts,
                                job_id,
                                attempt,
                                &idempotency_key,
                                &j,
                            );
                        }
                        if e.retryable && idempotent {
                            // Replaying is free and safe: keep the reservation and
                            // stay recoverable; the next pass replays this same attempt.
                            store::set_error(&db, job_id, &j)?;
                            return recoverable(&db, opts, job_id, &e.message);
                        }
                        if e.retryable {
                            // Nothing reached the provider (connect-phase failure):
                            // release this attempt; the next pass counts a new one.
                            ledger::reconcile(&mut db, job_id, attempt, 0.0)?;
                            db.fail_submission(job_id, &j, None).map_err(je)?;
                            return recoverable(&db, opts, job_id, &e.message);
                        }
                        // Provider rejected the request: nothing was charged.
                        ledger::reconcile(&mut db, job_id, attempt, 0.0)?;
                        fail_job(&db, opts, job_id, &j)?;
                    }
                }
            }
            Submission::Resume {
                external_id: pid,
                attempt,
            } => {
                if started.elapsed() >= opts.total {
                    return recoverable(&db, opts, job_id, "run time limit reached");
                }
                let idempotent = provider.submit_is_idempotent();
                let message = provider.progress_message(&req, &pid);
                emit(&db, opts, job_id, "polling", message.as_deref());
                match provider.poll(&req, &pid, &key) {
                    Ok(PollOutcome::Running) => {
                        let left = opts.total.saturating_sub(started.elapsed());
                        // Wake promptly for persisted cancellation even when
                        // provider polling has backed off to 15 seconds.
                        let until = Instant::now() + delay.min(left);
                        while Instant::now() < until {
                            if store::get_job(&db, job_id)?.is_terminal() {
                                break;
                            }
                            std::thread::sleep(
                                Duration::from_millis(250)
                                    .min(until.saturating_duration_since(Instant::now())),
                            );
                        }
                        delay = (delay * 2).min(opts.poll_max);
                        // Cancellation is checked at the top of the loop.
                    }
                    Ok(PollOutcome::Completed { outputs }) => {
                        if let Some(v) =
                            finalize(ctx, &mut db, opts, provider, &job, attempt, &outputs, false)?
                        {
                            return Ok(v);
                        }
                    }
                    Ok(PollOutcome::Failed { message }) => {
                        // The provider CONFIRMED the failure in its own vocabulary: nothing is
                        // owed for this attempt.
                        ledger::reconcile(&mut db, job_id, attempt, 0.0)?;
                        if idempotent {
                            // Replaying is free and safe: the next pass resubmits as a new
                            // attempt with its own reservation.
                            db.fail_submission(
                                job_id,
                                &err_json("provider_failed", &message),
                                Some(SUBMITTED),
                            )
                            .map_err(je)?;
                            emit(
                                &db,
                                opts,
                                job_id,
                                "polling",
                                Some(&format!(
                                    "provider reported failure, resubmitting: {message}"
                                )),
                            );
                        } else {
                            // A new paid attempt is a new charge: never started silently.
                            fail_job(
                                &db,
                                opts,
                                job_id,
                                &err_json(
                                    "provider_failed",
                                    &format!("{message} (the provider confirmed the failure; nothing was charged; not retried automatically, run it again to retry)"),
                                ),
                            )?;
                        }
                    }
                    Ok(PollOutcome::Unknown {
                        status,
                        detail,
                        completed,
                    }) => {
                        return hold_poll_unknown(
                            &mut db, opts, &job, attempt, &pid, &status, &detail, completed,
                        );
                    }
                    Err(e) => {
                        let j = err_json(&e.code, &e.message);
                        if e.retryable {
                            store::set_error(&db, job_id, &j)?;
                            return recoverable(&db, opts, job_id, &e.message);
                        }
                        if provider.needs_key() {
                            // The provider accepted this job and may still run and bill it. A
                            // 401/402/403/404/other 4xx on the poll proves nothing about that
                            // (404 may mean the job expired): hold, keep the reservation.
                            let held = err_json(
                                "outcome_unknown",
                                &format!(
                                    "polling {} job {pid} failed with {} ({}); the job was accepted and may still run or bill, so attempt {attempt} stays reserved",
                                    job.provider, e.code, e.message
                                ),
                            );
                            return outcome_unknown(
                                &db,
                                opts,
                                job_id,
                                attempt,
                                &format!("{job_id}:{attempt}"),
                                &held,
                            );
                        }
                        ledger::reconcile(&mut db, job_id, attempt, 0.0)?;
                        fail_job(&db, opts, job_id, &j)?;
                    }
                }
            }
        }
    }
}

/// A poll reply the adapter could not classify ([`PollOutcome::Unknown`]) on an accepted job.
/// `completed == false`: held as `outcome_unknown`, reservation untouched (it may still run or
/// bill; `--resubmit` is the user's call after checking the provider). `completed == true`: the
/// provider did the work but gave nothing usable, so the reservation is settled at the estimate
/// and the job is held as [`RESULT_UNUSABLE`]. Never resubmits.
#[allow(clippy::too_many_arguments)]
fn hold_poll_unknown(
    db: &mut Db,
    opts: &RunOptions,
    job: &Job,
    attempt: i64,
    pid: &str,
    status: &str,
    detail: &str,
    completed: bool,
) -> GenResult<Value> {
    let job_id = &job.job_id;
    let reply = if detail.is_empty() {
        String::new()
    } else {
        format!(" Provider reply: {detail}")
    };
    if !completed {
        let held = err_json(
            "outcome_unknown",
            &format!(
                "{} answered job {pid} with status {status:?}, which is not a recognised status; it may still run or bill, so attempt {attempt} stays reserved and nothing is resubmitted.{reply}",
                job.provider
            ),
        );
        return outcome_unknown(
            db,
            opts,
            job_id,
            attempt,
            &format!("{job_id}:{attempt}"),
            &held,
        );
    }
    ledger::reconcile(db, job_id, attempt, job.est_usd)?;
    let held = err_json(
        RESULT_UNUSABLE,
        &format!(
            "{} reports job {pid} finished ({status}) but returned nothing usable; ${:.2} is counted as spent and nothing is requested again. `job resolve {job_id} --abandon` closes it, then request the media again.{reply}",
            job.provider, job.est_usd
        ),
    );
    if store::mark_outcome_unknown(db, job_id, &held)? {
        emit(db, opts, job_id, RESULT_UNUSABLE, Some(&held.message));
    }
    Ok(store::get_job(db, job_id)?.to_value())
}

/// A finished paid result that cannot be stored and left no received bytes to recover (a
/// download or descriptor failure on provider-hosted output). The provider charged: settle the
/// estimate and hold under [`RESULT_UNUSABLE`], naming the output URLs so the user can fetch
/// them by hand. Never releases, never resubmits.
fn hold_unusable(
    db: &mut Db,
    opts: &RunOptions,
    job: &Job,
    attempt: i64,
    outputs: &[Output],
    cause: &GenError,
) -> GenResult<Value> {
    let job_id = &job.job_id;
    ledger::reconcile(db, job_id, attempt, job.est_usd)?;
    let urls: String = outputs
        .iter()
        .filter_map(|o| {
            if let Output::Url { url, .. } = o {
                Some(url.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
        .chars()
        .take(400)
        .collect();
    let where_ = if urls.is_empty() {
        String::new()
    } else {
        format!(" Output at the provider: {urls}.")
    };
    let held = err_json(
        RESULT_UNUSABLE,
        &format!(
            "{} finished and charged for this job but its result could not be stored ({}); ${:.2} is counted as spent and nothing is requested again.{where_} `job resolve {job_id} --abandon` closes it, then request the media again.",
            job.provider, cause.message, job.est_usd
        ),
    );
    if store::mark_outcome_unknown(db, job_id, &held)? {
        emit(db, opts, job_id, RESULT_UNUSABLE, Some(&held.message));
    }
    Ok(store::get_job(db, job_id)?.to_value())
}

/// Hold the job as `outcome_unknown` (reservation untouched) and return its record.
fn outcome_unknown(
    db: &Db,
    opts: &RunOptions,
    job_id: &str,
    attempt: i64,
    idempotency_key: &str,
    error: &JobError,
) -> GenResult<Value> {
    if store::mark_outcome_unknown(db, job_id, error)? {
        emit(
            db,
            opts,
            job_id,
            "outcome_unknown",
            Some(&format!(
                "held: {} (attempt {attempt}, key {idempotency_key}); resolve with `job resolve {job_id} --resubmit` or `--abandon`",
                error.message
            )),
        );
    }
    Ok(store::get_job(db, job_id)?.to_value())
}

/// The user's decision for an `outcome_unknown` job, under the job lock. Every decision is
/// one `JobStore::resolve` (rightkit-job-queue 0.2.9), so the status move and the spend ledger
/// (`ledger::SpendHook::on_resolve`) change in one transaction.
///
/// - `resubmit`: they confirmed with the provider that nothing ran. The held
///   attempt's reservation is released (reconciled to zero) and the job returns
///   to `submitted`; the next run counts one new attempt with a new key and
///   reservation. Refused once every allowed attempt is used.
/// - `abandon`: the job ends `failed` with `outcome_unknown_abandoned`. The
///   reservation is kept as spent, because the provider may have charged.
/// - `adopt`: a [`RESULT_RECOVERED`] job only; stores the recovered file and
///   completes the job without contacting the provider.
pub fn resolve_job(ctx: &Context, job_id: &str, action: &str) -> GenResult<Value> {
    if !matches!(action, "resubmit" | "abandon" | "adopt") {
        return Err(GenError::new(
            "invalid_input",
            "action must be resubmit, abandon or adopt",
        ));
    }
    let Some(_lock) = try_lock(ctx, job_id)? else {
        return Err(GenError::new(
            "job_busy",
            format!("job {job_id} is being run by another process"),
        ));
    };
    let db = store::open(ctx)?;
    let job = store::get_job(&db, job_id)?;
    if !job.awaits_resolution() {
        return Err(GenError::new(
            "not_outcome_unknown",
            format!(
                "job {job_id} is {}; only an outcome_unknown job can be resolved",
                job.status
            ),
        ));
    }
    let attempt = job.attempts;
    let opts = RunOptions {
        stderr_events: false,
        ..RunOptions::default()
    };
    let recovered = job.awaits_adoption();
    // Resolving moves money (a resubmit releases a reservation and spends again): leave an audit row.
    ledger::audit_setting(
        &db,
        "job.resolve",
        &json!({"job_id": job_id, "action": action, "attempt": attempt}),
    )?;
    if action == "adopt" {
        if !recovered {
            return Err(GenError::new(
                "nothing_to_adopt",
                format!("job {job_id} holds no recovered result; only a {RESULT_RECOVERED} job can be adopted"),
            ));
        }
        return adopt_recovered(ctx, &db, &opts, &job);
    }
    if action == "resubmit" {
        if recovered {
            return Err(GenError::new(
                RESULT_RECOVERED,
                format!("job {job_id} already has its paid result; it is never sent to the provider again. Use `job resolve {job_id} --adopt`"),
            ));
        }
        if held_code(&job).as_deref() == Some(RESULT_UNUSABLE) {
            return Err(GenError::new(
                RESULT_UNUSABLE,
                format!("job {job_id}: the provider finished and the attempt is settled as spent; it is not resubmitted. Use `job resolve {job_id} --abandon`, then request the media again"),
            ));
        }
        if attempt >= MAX_ATTEMPTS {
            return Err(GenError::new(
                "max_attempts",
                format!("job {job_id} has used all {MAX_ATTEMPTS} attempts; abandon it instead"),
            ));
        }
        if let store::Resolved::AlreadySettled(settled) =
            store::resolve(&db, job_id, &Resolution::Resubmit)?
        {
            return Ok(already_settled_value(&settled));
        }
        emit(
            &db,
            &opts,
            job_id,
            "resubmitting",
            Some(&format!(
                "resubmit confirmed by the user; attempt {attempt} released"
            )),
        );
    } else {
        let spent = ledger::attempt_reserved(&db, job_id, attempt)?.unwrap_or(job.est_usd);
        let message = if recovered {
            format!(
                "abandoned by the user; ${spent:.2} stays counted as spent and the recovered result stays in {}",
                recovered_dir(ctx).display()
            )
        } else {
            format!("abandoned by the user; attempt {attempt} may have run, so ${spent:.2} stays counted as spent")
        };
        if let store::Resolved::AlreadySettled(settled) = store::resolve(
            &db,
            job_id,
            &Resolution::Abandon(err_json("outcome_unknown_abandoned", &message)),
        )? {
            return Ok(already_settled_value(&settled));
        }
        emit(&db, &opts, job_id, "failed", Some(&message));
    }
    Ok(store::get_job(&db, job_id)?.to_value())
}

/// The answer to an owner decision that arrived after the runner's outcome was stored: the
/// job as it actually is, with a note. RightKit refused the move (`illegal_transition`), so the
/// status and the spend reservation are exactly as the runner left them.
fn already_settled_value(job: &Job) -> Value {
    let mut v = job.to_value();
    v["resolution"] = json!({"applied": false, "note": store::already_settled_note(job)});
    v
}

fn recoverable(db: &Db, opts: &RunOptions, job_id: &str, why: &str) -> GenResult<Value> {
    emit(
        db,
        opts,
        job_id,
        "polling",
        Some(&format!("paused, will resume: {why}")),
    );
    let mut v = store::get_job(db, job_id)?.to_value();
    v["recoverable"] = json!(true);
    Ok(v)
}

// Store outputs, write the descriptor, reconcile spend, complete the job.
// `Ok(None)` means the job was cancelled meanwhile; `Ok(Some)` is the final
// record or a recoverable one when downloading failed transiently.
thread_local! {
    static PROVIDER_COST: std::cell::RefCell<Option<(f64, String)>> = const { std::cell::RefCell::new(None) };
}

/// For an adapter, from inside `submit` or `poll` (the runner's thread): state the actual cost
/// the provider's response reported for the request in flight, and where it was read (for
/// example "fal response header x-fal-billable"). The runner records it on the ledger row as
/// `provider_reported_usd` when the job completes. The estimate still settles the attempt.
pub fn report_provider_cost(usd: f64, source: &str) {
    PROVIDER_COST.with(|c| *c.borrow_mut() = Some((usd, source.to_string())));
}

fn take_provider_cost() -> Option<(f64, String)> {
    PROVIDER_COST.with(|c| c.borrow_mut().take())
}

fn finalize(
    ctx: &Context,
    db: &mut Db,
    opts: &RunOptions,
    provider: &dyn Provider,
    job: &Job,
    attempt: i64,
    outputs: &[Output],
    // The outputs came straight from the submit reply: they cannot be fetched
    // again, and the provider has already charged for them.
    synchronous: bool,
) -> GenResult<Option<Value>> {
    emit_with(
        db,
        opts,
        &job.job_id,
        store::RUNNING,
        attempt,
        "downloading",
        None,
    );
    match descriptor::build(ctx, provider, job, outputs) {
        Ok(desc) => {
            // The estimate settles the attempt; a cost the adapter read from the provider's
            // response is recorded beside it and never moves what the caps count.
            ledger::reconcile(db, &job.job_id, attempt, job.est_usd)?;
            if let Some((usd, source)) = take_provider_cost() {
                if let Err(e) =
                    ledger::record_provider_reported(db, &job.job_id, attempt, usd, &source)
                {
                    crate::host::diag::warn(
                        "provider_cost_record_failed",
                        &[("job_id", json!(job.job_id)), ("message", json!(e.message))],
                    );
                }
            }
            // Index assets first: a complete job always has resolvable assets.
            store::record_assets(db, &job.job_id, &desc)?;
            if store::mark_complete(db, &job.job_id, &desc)? {
                emit(db, opts, &job.job_id, "complete", None);
            }
            Ok(Some(store::get_job(db, &job.job_id)?.to_value()))
        }
        Err((e, retryable)) => {
            // A paid provider has finished and charged by the time its result is fetched: a
            // permanent store/fetch failure holds the job with the reservation settled, for an
            // async result exactly as for a synchronous one. Only a keyless local engine's
            // permanent failure still releases (nothing was ever billed).
            let paid = provider.needs_key();
            if synchronous || (paid && !retryable) {
                if let Some(v) = hold_received(ctx, db, opts, job, attempt, outputs, &e)? {
                    return Ok(Some(v));
                }
            }
            let j = rk_err(&e);
            if retryable {
                store::set_error(db, &job.job_id, &j)?;
                Ok(Some(recoverable(db, opts, &job.job_id, &e.message)?))
            } else if paid {
                Ok(Some(hold_unusable(db, opts, job, attempt, outputs, &e)?))
            } else {
                ledger::reconcile(db, &job.job_id, attempt, 0.0)?;
                fail_job(db, opts, &job.job_id, &j)?;
                Ok(Some(store::get_job(db, &job.job_id)?.to_value()))
            }
        }
    }
}

// ---- received-but-unstored results -----------------------------------------------------------------

/// Where received-but-unstored paid results are kept.
pub fn recovered_dir(ctx: &Context) -> std::path::PathBuf {
    ctx.data_dir.join("recovered")
}

fn recovery_file_name(job_id: &str, attempt: i64, index: usize, mime: &str) -> String {
    let ext = crate::library::ext_for_mime(mime);
    if index == 0 {
        format!("{job_id}-{attempt}.{ext}")
    } else {
        format!("{job_id}-{attempt}-{index}.{ext}")
    }
}

fn mime_for_recovered_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "json" => "application/json",
        "bin" => "application/octet-stream",
        _ => return None,
    })
}

/// Save every received byte output as `<data_dir>/recovered/<job_id>-<attempt>[-<n>].<ext>`
/// (temp file then rename). Returns the paths in output order.
fn write_recovery(
    ctx: &Context,
    job_id: &str,
    attempt: i64,
    outputs: &[Output],
) -> Result<Vec<std::path::PathBuf>, String> {
    let dir = recovered_dir(ctx);
    rightkit_fs::ensure_private_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut paths = Vec::new();
    for (index, output) in outputs.iter().enumerate() {
        let Output::Bytes { bytes, mime } = output else {
            continue;
        };
        let path = dir.join(recovery_file_name(job_id, attempt, index, mime));
        rightkit_fs::atomic_write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

/// The recovered files of one attempt with their media types, in output order.
pub fn recovered_files(
    ctx: &Context,
    job_id: &str,
    attempt: i64,
) -> Vec<(std::path::PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(recovered_dir(ctx)) else {
        return Vec::new();
    };
    let first = format!("{job_id}-{attempt}.");
    let more = format!("{job_id}-{attempt}-");
    let mut found: Vec<(usize, std::path::PathBuf, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let (index, ext) = if let Some(ext) = name.strip_prefix(&first) {
            (0, ext.to_string())
        } else if let Some((i, ext)) = name.strip_prefix(&more).and_then(|r| r.split_once('.')) {
            let Ok(i) = i.parse::<usize>() else { continue };
            (i, ext.to_string())
        } else {
            continue;
        };
        let Some(mime) = mime_for_recovered_ext(&ext) else {
            continue;
        };
        found.push((index, entry.path(), mime.to_string()));
    }
    found.sort_by_key(|f| f.0);
    found.into_iter().map(|(_, p, m)| (p, m)).collect()
}

/// A synchronous paid reply arrived and storing it failed. The provider has
/// charged and will not repeat the answer, so the bytes are written to the
/// recovery directory, the attempt is settled as spent and the job is held
/// under [`RESULT_RECOVERED`]. Never re-submits. If the recovery write fails
/// too, the job is held as plain `outcome_unknown` and says the bytes are lost.
/// `Ok(None)` when the outputs are not all received bytes (nothing to save).
fn hold_received(
    ctx: &Context,
    db: &mut Db,
    opts: &RunOptions,
    job: &Job,
    attempt: i64,
    outputs: &[Output],
    cause: &GenError,
) -> GenResult<Option<Value>> {
    if outputs.is_empty() || !outputs.iter().all(|o| matches!(o, Output::Bytes { .. })) {
        return Ok(None);
    }
    let job_id = &job.job_id;
    match write_recovery(ctx, job_id, attempt, outputs) {
        Ok(paths) => {
            ledger::reconcile(db, job_id, attempt, job.est_usd)?;
            let shown = paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let held = err_json(
                RESULT_RECOVERED,
                &format!(
                    "{} returned and charged for this result, but it could not be stored in the library ({}). It is saved at {shown}; `job resolve {job_id} --adopt` stores it. It will not be requested again",
                    job.provider, cause.message
                ),
            );
            if store::mark_outcome_unknown(db, job_id, &held)? {
                emit(db, opts, job_id, RESULT_RECOVERED, Some(&held.message));
            }
        }
        Err(write_error) => {
            let held = err_json(
                "outcome_unknown",
                &format!(
                    "{} returned a result but it could not be stored ({}) and could not be saved for recovery ({write_error}); the received bytes are lost and attempt {attempt} stays reserved",
                    job.provider, cause.message
                ),
            );
            return outcome_unknown(
                db,
                opts,
                job_id,
                attempt,
                &format!("{job_id}:{attempt}"),
                &held,
            )
            .map(Some);
        }
    }
    Ok(Some(store::get_job(db, job_id)?.to_value()))
}

/// `job resolve --adopt`: store the recovered file(s) of a [`RESULT_RECOVERED`]
/// job in the library and complete it. No provider call; the spend was already
/// settled. A failure leaves the job held and the files in place.
fn adopt_recovered(ctx: &Context, db: &Db, opts: &RunOptions, job: &Job) -> GenResult<Value> {
    let job_id = &job.job_id;
    let files = recovered_files(ctx, job_id, job.attempts);
    if files.is_empty() {
        return Err(GenError::new(
            "recovery_missing",
            format!("no recovered file for job {job_id} under {}; `job resolve {job_id} --abandon` closes it", recovered_dir(ctx).display()),
        ));
    }
    let mut outputs = Vec::new();
    for (path, mime) in &files {
        let bytes = std::fs::read(path).map_err(store::io_err)?;
        outputs.push(Output::Bytes {
            bytes,
            mime: mime.clone(),
        });
    }
    // Bytes outputs never touch the provider; the adapter is only needed by the signature.
    let providers = crate::providers::registry();
    let provider = find(&providers, &job.provider)?;
    let desc = descriptor::build(ctx, provider, job, &outputs).map_err(|(e, _)| e)?;
    store::record_assets(db, job_id, &desc)?;
    // RightKit's adopt: the job completes with this result and nothing is sent to the provider.
    // The attempt was settled as spent when the result was held; the hook settles any open row.
    // The recovered files stay in place when the job had already been settled.
    if let store::Resolved::AlreadySettled(settled) =
        store::resolve(db, job_id, &Resolution::Adopt(desc))?
    {
        return Ok(already_settled_value(&settled));
    }
    emit(
        db,
        opts,
        job_id,
        "complete",
        Some("recovered result stored in the library"),
    );
    for (path, _) in &files {
        let _ = std::fs::remove_file(path);
    }
    Ok(store::get_job(db, job_id)?.to_value())
}
