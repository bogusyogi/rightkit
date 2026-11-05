//! Spend-ledger bookkeeping for a rented, time-billed Vast instance (PENDING 247).
//!
//! A rental is spend like any other: before the billed create it reserves its declared
//! `budget_usd` in the spend ledger under `rental:<worker_id>` (attempt N), so the daily cap, the
//! monthly cap and the device allowance all apply (`ledger::reserve` -> `reserve_attempt_on`). The
//! reservation then follows the instance:
//!
//! - create refused or provably not sent (connect failure, definite 4xx): released (settled 0).
//! - create possibly delivered (timeout after send, 5xx/408/429, unreadable reply): HELD open, the
//!   rental record is `outcome_unknown`, nothing is retried. `resolve_rental` adopts the instance
//!   found by its label, or releases once the user confirms none exists.
//! - active: the reservation stays open at the budget.
//! - destroy confirmed by Vast (2xx to the DELETE; `destroyed_unsettled` until the ledger write lands): settled to the actual cost, elapsed x hourly
//!   rate, at the true amount even above the reservation (the overrun is recorded).
//! - destroy not confirmed: the reservation stays open and the record says the instance may still
//!   be billing. If the provider then reports the instance does not exist (404 on the instance
//!   read, or absent from the label list on two reads at least a minute apart), the destroy counts
//!   as confirmed-gone and settles with `estimated_end: true` (elapsed to the FIRST destroy attempt).
//!
//! Open reservations count at `reserved_usd` and settled rows at `actual_usd` in every cap, so
//! `spend status` shows rentals with no change of shape. The ledger row has no `jobs` row; the
//! standalone `reserve`/`reconcile` entry points already tolerate that (as character training does).
//!
//! Record (`worker["rental"]`): `ledger_id`, `attempt`, `status` (renting | outcome_unknown |
//! active | destroy_unconfirmed | destroyed_unsettled | settled | released), `reserved_usd`, `offer_id`, `dph_total`,
//! `instance_id`, `rented_at_epoch`, and after settlement `elapsed_sec`, `rate_usd_hr`,
//! `actual_usd`, `overrun_usd`. `billing_warning` is present whenever the instance may still bill.
//! The record also carries `max_duration_min` (the declared duration cap) and, while a destroy is
//! being sent, status `destroying` (see below).
//!
//! # Enforcement (PENDING 248)
//!
//! A rental's limits are `max_duration_min`, the declared `budget_usd` (accrued = elapsed x the
//! higher known hourly rate) and, for an idle worker, `idle_limit_min`. [`rental_due`] is the pure
//! decision; [`enforce_rentals`] acts on it: for each due `active` rental it persists status
//! `destroying` (under the rentals file lock) BEFORE the one DELETE, destroys through the same
//! confirmed-destroy and settlement path as `teardown`, and records `auto_destroyed {reason, at}`.
//! An unconfirmed destroy leaves `destroy_unconfirmed` with the billing warning and is retried at
//! most every 5 minutes. A `destroying` claim older than 3 minutes is taken over (the claimant died).
//! `renting` rentals older than a grace period and `outcome_unknown` rentals are looked up by this
//! worker's label: exactly one instance is adopted (then the limits apply), none twice at least a
//! minute apart releases the reservation (`auto_resolved: none_found`), more than one is reported
//! and nothing is destroyed. Nothing is ever destroyed on an instance whose label is not this
//! worker's (`instance_not_owned`).
//!
//! Enforcement runs at: engine sidecar start, every 60 s while the sidecar lives, the start of every
//! CLI command that touches workers or generation, and on demand (`studio.models.rentals_enforce`).
//!
//! HONEST LIMITS: the host has no always-on service. When neither the app nor a CLI command is
//! running, NOTHING enforces any limit: the instance bills until the next engine start or until the
//! user destroys it at the provider. Destroying needs the Vast key the enforcing process can read:
//! the stored credential (a rental can only be created while one is stored), else `VAST_API_KEY`;
//! without either the due rental is reported as blocked, not destroyed. The real fix is an
//! instance-side maximum lifetime or auto-stop in the create request; it needs the provider's
//! documented field name (a fetched source), which has NOT been verified, so none is sent.
//!

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use rightkit_media_gen::{ledger, store, Context, GenError, GenResult};

use super::remote::{DestroyFacts, RentParams};

pub const BILLING_WARNING: &str =
    "the Vast instance may still be billing: destroy was not confirmed by the provider; the spend reservation stays open";

fn ledger_id(w: &Value) -> String {
    format!("rental:{}", w["worker_id"].as_str().unwrap_or("worker"))
}

fn status(w: &Value) -> &str {
    w["rental"]["status"].as_str().unwrap_or("")
}

fn epoch_now() -> f64 {
    now_epoch()
}

/// Unix seconds now.
pub fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// A rental that exists, may exist, or may still bill blocks a second create on the same worker.
pub fn blocks_new_rental(w: &Value) -> Option<GenError> {
    match status(w) {
        "renting" | "outcome_unknown" => Some(GenError::new(
            "rental_unresolved",
            "a previous create may have been delivered; run resolve_rental (adopt by label, or confirm none exists) before any new create",
        )),
        "active" | "destroy_unconfirmed" | "destroying" => Some(GenError::new(
            "already_provisioned",
            "this worker already holds a rented instance; teardown it first",
        )),
        _ => None,
    }
}

/// Reserve the declared budget BEFORE anything is sent to Vast. Refuses with `spend_cap` (naming
/// the daily cap, monthly cap or device allowance); on refusal nothing was written. On success the
/// record is `renting` and the caller must persist the worker before the billed call.
pub fn open(ctx: &Context, w: &mut Value, p: &RentParams) -> GenResult<()> {
    if let Some(e) = blocks_new_rental(w) {
        return Err(e);
    }
    let attempt = w["rental"]["attempt"].as_i64().unwrap_or(0) + 1;
    let id = ledger_id(w);
    let mut db = store::open(ctx)?;
    ledger::reserve(&mut db, &id, attempt, p.budget)?;
    w["rental"] = json!({
        "ledger_id": id, "attempt": attempt, "status": "renting",
        "reserved_usd": p.budget, "max_duration_min": p.max_min, "offer_id": p.offer_id, "reserved_at": store::now(),
        "reserved_at_epoch": epoch_now(),
        "label": super::remote::rental_label(w),
    });
    Ok(())
}

/// Release the reservation (settle it at zero): nothing was, or provably can have been, rented.
/// Best effort in the safe direction: if the ledger write fails the reservation stays open (it keeps
/// counting) and the error text is returned for the event log.
pub fn release(ctx: &Context, w: &mut Value, why: &str) -> Option<String> {
    let id = w["rental"]["ledger_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let attempt = w["rental"]["attempt"].as_i64().unwrap_or(0);
    let res = store::open(ctx).and_then(|mut db| ledger::reconcile(&mut db, &id, attempt, 0.0));
    match res {
        Ok(()) => {
            w["rental"]["status"] = json!("released");
            w["rental"]["released_at"] = json!(store::now());
            w["rental"]["released_because"] = json!(why);
            if let Some(o) = w["rental"].as_object_mut() {
                o.remove("billing_warning");
            }
            None
        }
        Err(e) => {
            w["rental"]["release_error"] = json!(e.message);
            Some(e.message)
        }
    }
}

/// The create may have been delivered: keep the reservation open, mark the record for resolution.
pub fn hold(w: &mut Value, why: &GenError) {
    w["rental"]["status"] = json!("outcome_unknown");
    w["rental"]["held_at"] = json!(store::now());
    w["rental"]["held_because"] = json!({"code": why.code, "message": why.message});
    w["rental"]["billing_warning"] = json!(
        "the Vast create may have been delivered: an instance labelled for this worker may exist and bill; the spend reservation stays open until resolve_rental"
    );
}

/// Vast accepted the create and returned an instance id.
pub fn activate(w: &mut Value, instance_id: &Value, dph: f64) {
    w["rental"]["status"] = json!("active");
    w["rental"]["instance_id"] = instance_id.clone();
    w["rental"]["dph_total"] = json!(dph);
    w["rental"]["rented_at"] = json!(store::now());
    w["rental"]["rented_at_epoch"] = json!(epoch_now());
    if let Some(o) = w["rental"].as_object_mut() {
        o.remove("billing_warning");
    }
}

/// Adopt an instance found by label while resolving a held rental. The reservation stays open at
/// the budget; the rate comes from the provider's listing when present, else the quoted one is
/// unknown and the settle rate falls back to the instance's own at destroy time.
pub fn adopt(w: &mut Value, instance: &Value) {
    let id = instance["id"].clone();
    w["spec"]["vast"]["instance_id"] = id.clone();
    w["rental"]["status"] = json!("active");
    w["rental"]["instance_id"] = id;
    w["rental"]["adopted_at"] = json!(store::now());
    // Settlement later takes the earliest known start, so the longer reading wins.
    if let Some(start) = instance["start_date"].as_f64().filter(|t| *t > 0.0) {
        w["rental"]["provider_start_epoch"] = json!(start);
    }
    if let Some(d) = instance["dph_total"]
        .as_f64()
        .filter(|d| d.is_finite() && *d >= 0.0)
    {
        w["rental"]["dph_total"] = json!(d);
    }
    // The instance cannot have started before the reservation, which preceded the PUT.
    w["rental"]["rented_at_epoch"] = w["rental"]["reserved_at_epoch"].clone();
    if let Some(o) = w["rental"].as_object_mut() {
        o.remove("billing_warning");
    }
}

/// Destroy was NOT confirmed by Vast: the reservation stays open, the instance may still bill.
pub fn destroy_unconfirmed(w: &mut Value, why: &GenError) {
    if matches!(status(w), "active" | "destroy_unconfirmed" | "destroying") {
        w["rental"]["status"] = json!("destroy_unconfirmed");
        w["rental"]["billing_warning"] = json!(BILLING_WARNING);
        w["rental"]["destroy_error"] = json!({"code": why.code, "message": why.message, "at": store::now(), "at_epoch": now_epoch()});
    }
}

/// Vast confirmed the destroy (2xx to the DELETE). Recorded BEFORE the ledger write so a failed
/// settle can be retried from the record without a second DELETE.
pub fn destroyed(w: &mut Value, f: &DestroyFacts) {
    w["rental"]["status"] = json!("destroyed_unsettled");
    w["rental"]["destroyed_at"] = json!(store::now());
    w["rental"]["destroyed_at_epoch"] = json!(epoch_now());
    w["rental"]["instance_id"] = json!(f.instance_id);
    if let Some(t) = f.start_epoch {
        w["rental"]["provider_start_epoch"] = json!(t);
    }
    if let Some(d) = f.dph_total {
        w["rental"]["provider_dph_total"] = json!(d);
    }
    if let Some(o) = w["rental"].as_object_mut() {
        o.remove("billing_warning");
        o.remove("destroy_error");
    }
}

/// Record the moment of the FIRST destroy attempt (kept across retries): a destroy later found to
/// have already happened settles to this time, since the true end cannot be known.
pub fn note_destroy_attempt(w: &mut Value) {
    if !w["rental"]["first_destroy_attempt_epoch"].is_number() {
        w["rental"]["first_destroy_attempt_epoch"] = json!(epoch_now());
    }
}

/// The provider reports the instance does not exist, so the rental is treated as destroyed. The end
/// is ESTIMATED as the first destroy attempt (never later than now): settlement says `estimated_end`.
pub fn destroyed_gone(w: &mut Value, instance_id: &str, how: &str) {
    let end = w["rental"]["first_destroy_attempt_epoch"]
        .as_f64()
        .filter(|t| *t > 0.0)
        .unwrap_or_else(epoch_now)
        .min(epoch_now());
    w["rental"]["status"] = json!("destroyed_unsettled");
    w["rental"]["destroyed_at"] = json!(store::now());
    w["rental"]["destroyed_at_epoch"] = json!(end);
    w["rental"]["estimated_end"] = json!(true);
    w["rental"]["confirmed_gone"] =
        json!({"how": how, "at": store::now(), "at_epoch": epoch_now()});
    w["rental"]["instance_id"] = json!(instance_id);
    if let Some(o) = w["rental"].as_object_mut() {
        o.remove("billing_warning");
        o.remove("destroy_error");
        o.remove("gone_absent_first_epoch");
    }
}

/// What a check after an unconfirmed destroy found.
#[derive(Debug, Clone, PartialEq)]
pub enum GoneVerdict {
    /// The provider says the instance does not exist.
    Gone(&'static str),
    /// The instance is still listed or readable: it may still be billing.
    StillPresent,
    /// Absent from the label list once; needs a second read at least [`GONE_REREAD_SEC`] later.
    PendingSecondRead,
    /// The check itself failed or was not applicable; nothing is concluded.
    Unknown(String),
}

/// The second absence read must be at least this long after the first.
pub const GONE_REREAD_SEC: f64 = 60.0;

/// After a destroy that was not a clean 2xx (`err`), or on demand (`err` None): is the instance
/// already gone? A 404 from the instance read says so at once. Otherwise the labelled list must omit
/// the instance on two reads at least a minute apart (the first is recorded on the record). Never
/// sends a DELETE.
pub fn check_gone(
    drv: &super::remote::VastDriver,
    w: &mut Value,
    err: Option<&GenError>,
) -> GoneVerdict {
    if let Some(e) = err {
        match e.code.as_str() {
            "instance_gone" => {
                return GoneVerdict::Gone("the provider reports the instance does not exist (404)")
            }
            "instance_not_owned" | "no_credentials" | "not_provisioned" | "keychain" => {
                return GoneVerdict::Unknown(format!("{}: not checked", e.code));
            }
            _ => {}
        }
    }
    let id = match w["rental"]["instance_id"]
        .as_i64()
        .map(|n| n.to_string())
        .or_else(|| {
            w["spec"]["vast"]["instance_id"]
                .as_i64()
                .map(|n| n.to_string())
        }) {
        Some(id) => id,
        None => return GoneVerdict::Unknown("no instance id is recorded".into()),
    };
    match drv.read_instance(&id) {
        Ok(super::remote::InstanceRead::Gone) => {
            return GoneVerdict::Gone(
                "the provider reports the instance does not exist (404 on the instance read)",
            )
        }
        Ok(super::remote::InstanceRead::Present(_)) => {
            if let Some(o) = w["rental"].as_object_mut() {
                o.remove("gone_absent_first_epoch");
            }
            return GoneVerdict::StillPresent;
        }
        Err(_) => {}
    }
    let now = epoch_now();
    match drv.find_by_label(w) {
        Ok(list) => {
            let present = list
                .iter()
                .any(|i| i["id"].as_i64().map(|n| n.to_string()).as_deref() == Some(id.as_str()));
            if present {
                if let Some(o) = w["rental"].as_object_mut() {
                    o.remove("gone_absent_first_epoch");
                }
                return GoneVerdict::StillPresent;
            }
            match w["rental"]["gone_absent_first_epoch"].as_f64() {
                Some(first) if now - first >= GONE_REREAD_SEC => GoneVerdict::Gone(
                    "absent from the provider's labelled list on two reads at least a minute apart",
                ),
                Some(_) => GoneVerdict::PendingSecondRead,
                None => {
                    w["rental"]["gone_absent_first_epoch"] = json!(now);
                    GoneVerdict::PendingSecondRead
                }
            }
        }
        Err(e) => GoneVerdict::Unknown(format!("{}: {}", e.code, e.message)),
    }
}

/// Settle the reservation of a destroyed rental to the actual cost.
///
/// elapsed = destroy confirmation - the EARLIEST known start (provider `start_date`, the time our
/// own create was accepted or reserved): the longer reading wins, because this number caps real money.
/// rate = the HIGHER of the quoted offer rate and the instance's reported `dph_total`. Storage or
/// bandwidth beyond `dph_total` are not known to the code and are not added.
/// The settled amount is the true amount; above the reservation it is recorded as `overrun_usd`
/// and the caps then count the true amount. With no usable start or rate the whole declared
/// budget is settled (never zero).
pub fn settle(ctx: &Context, w: &mut Value) -> GenResult<Value> {
    if status(w) != "destroyed_unsettled" {
        return Err(GenError::new(
            "bad_state",
            "rental is not awaiting settlement",
        ));
    }
    let end = w["rental"]["destroyed_at_epoch"]
        .as_f64()
        .unwrap_or_else(epoch_now);
    let starts: Vec<f64> = [
        w["rental"]["provider_start_epoch"].as_f64(),
        w["rental"]["rented_at_epoch"].as_f64(),
    ]
    .into_iter()
    .flatten()
    .filter(|t| *t > 0.0 && *t <= end)
    .collect();
    let start = starts.iter().cloned().fold(f64::INFINITY, f64::min);
    let elapsed = if start.is_finite() {
        (end - start).max(0.0)
    } else {
        0.0
    };
    let rate = [
        w["rental"]["dph_total"].as_f64(),
        w["rental"]["provider_dph_total"].as_f64(),
    ]
    .into_iter()
    .flatten()
    .fold(0.0_f64, f64::max);
    let unknown_basis = !start.is_finite() || rate <= 0.0;
    let reserved = w["rental"]["reserved_usd"].as_f64().unwrap_or(0.0);
    let actual = if unknown_basis {
        reserved
    } else {
        round6(elapsed / 3600.0 * rate)
    };
    let id = w["rental"]["ledger_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let attempt = w["rental"]["attempt"].as_i64().unwrap_or(0);
    let mut db = store::open(ctx)?;
    ledger::reconcile(&mut db, &id, attempt, actual)?;
    let overrun = round6((actual - reserved).max(0.0));
    let instance_id = w["rental"]["instance_id"].clone();
    let rec = &mut w["rental"];
    rec["status"] = json!("settled");
    rec["settled_at"] = json!(store::now());
    rec["elapsed_sec"] = json!(elapsed);
    rec["rate_usd_hr"] = json!(rate);
    rec["actual_usd"] = json!(actual);
    rec["overrun_usd"] = json!(overrun);
    rec["settled_on_declared_budget"] = json!(unknown_basis);
    let estimated_end = rec["estimated_end"].as_bool() == Some(true);
    Ok(json!({
        "instance_id": instance_id, "elapsed_sec": elapsed, "rate_usd_hr": rate,
        "actual_usd": actual, "reserved_usd": reserved, "overrun_usd": overrun,
        "settled_on_declared_budget": unknown_basis,
        "estimated_end": estimated_end,
        "note": "cost is elapsed x dph_total; storage/bandwidth beyond dph_total are not known here and are not added",
    }))
}

// ---------- limits and enforcement (PENDING 248) ----------------------------------

/// The sidecar re-checks rentals this often while it lives.
pub const SIDECAR_INTERVAL: Duration = Duration::from_secs(60);
/// A `destroying` claim older than this belongs to a dead claimant and may be taken over.
const CLAIM_STALE_SEC: f64 = 180.0;
/// An unconfirmed destroy is retried by enforcement no more often than this.
const RETRY_AFTER_SEC: f64 = 300.0;
/// A `renting` record is left alone this long: the create call's own limit is 30 s (see
/// `VastDriver::http_for`), plus margin. Older than this, the create's outcome is looked up by label.
pub const RENTING_GRACE_SEC: f64 = 120.0;

pub const FORCE_NOTE: &str =
    "force:true destroyed a rental whose worker was running: any work in flight on the instance was interrupted";

/// Stated verbatim in the `rentals` and `rentals_enforce` output.
pub const HONEST_LIMITS: &str = "enforcement runs only while the host app runs: at engine sidecar start, every 60 s while the app is open, at the start of each CLI command that touches workers or generation, and on studio.models.rentals_enforce. When neither the app nor a CLI command is running nothing enforces any limit, and the instance keeps billing until the next engine start or until you destroy it at the provider. Destroying needs the Vast API key saved in the app (a rental is only created while one is saved) or VAST_API_KEY in the enforcing process. The real fix is an instance-side maximum lifetime or auto-stop in the create request; it needs the provider's documented field name from a fetched source, which has not been verified, so none is sent";

/// Why a rental is due for destruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Duration,
    Budget,
    Idle,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Duration => "duration",
            Reason::Budget => "budget",
            Reason::Idle => "idle",
        }
    }
}

/// Every number a limit check uses, derived from the worker record and `now`. `None` means unknown.
#[derive(Debug, Clone, Default)]
pub struct Limits {
    pub elapsed_sec: Option<f64>,
    pub rate_usd_hr: Option<f64>,
    pub accrued_usd: Option<f64>,
    pub budget_usd: Option<f64>,
    pub max_duration_min: Option<f64>,
    pub idle_limit_min: Option<f64>,
    pub idle_for_sec: Option<f64>,
    /// Limits that cannot be enforced because the record lacks the input.
    pub warnings: Vec<String>,
}

impl Limits {
    pub fn duration_due_in_sec(&self) -> Option<f64> {
        Some(self.max_duration_min? * 60.0 - self.elapsed_sec?)
    }
    pub fn budget_due_in_sec(&self) -> Option<f64> {
        let rate = self.rate_usd_hr.filter(|r| *r > 0.0)?;
        Some((self.budget_usd? - self.accrued_usd?) / rate * 3600.0)
    }
    pub fn idle_due_in_sec(&self) -> Option<f64> {
        Some(self.idle_limit_min? * 60.0 - self.idle_for_sec?)
    }
}

/// Compute the limit inputs of a worker record at `now`. Elapsed uses the EARLIEST known start
/// (provider start, our accepted create, the reservation) not after `now`, as settlement does; rate
/// is the HIGHER of the quoted and the provider-reported hourly rate.
pub fn limits(w: &Value, now: f64) -> Limits {
    let r = &w["rental"];
    let mut l = Limits::default();
    let start = [
        r["provider_start_epoch"].as_f64(),
        r["rented_at_epoch"].as_f64(),
        r["reserved_at_epoch"].as_f64(),
    ]
    .into_iter()
    .flatten()
    .filter(|t| *t > 0.0 && *t <= now)
    .fold(f64::INFINITY, f64::min);
    l.elapsed_sec = start.is_finite().then(|| (now - start).max(0.0));
    let rate = [r["dph_total"].as_f64(), r["provider_dph_total"].as_f64()]
        .into_iter()
        .flatten()
        .fold(0.0_f64, f64::max);
    l.rate_usd_hr = (rate > 0.0).then_some(rate);
    l.budget_usd = r["reserved_usd"]
        .as_f64()
        .filter(|b| b.is_finite() && *b > 0.0);
    l.max_duration_min = r["max_duration_min"]
        .as_f64()
        .filter(|m| m.is_finite() && *m > 0.0);
    if let (Some(e), Some(rate)) = (l.elapsed_sec, l.rate_usd_hr) {
        l.accrued_usd = Some(round6(e / 3600.0 * rate));
    }
    if w["state"].as_str() == Some("idle") {
        l.idle_limit_min = w["idle_limit_min"]
            .as_f64()
            .filter(|m| m.is_finite() && *m > 0.0);
        l.idle_for_sec = w["idle_since_epoch"]
            .as_f64()
            .filter(|t| *t > 0.0 && *t <= now)
            .map(|t| now - t);
    }
    if l.elapsed_sec.is_none() {
        l.warnings.push(
            "no start time is recorded: the duration and budget limits cannot be enforced".into(),
        );
    }
    if l.max_duration_min.is_none() {
        l.warnings
            .push("no max_duration_min is recorded: the duration limit cannot be enforced".into());
    }
    if l.rate_usd_hr.is_none() {
        l.warnings
            .push("no hourly rate is known: the budget limit cannot be enforced".into());
    }
    l
}

/// Pure decision: is this rental past a limit at `now`? Due when elapsed >= `max_duration_min`, or
/// accrued (elapsed x the higher known rate) >= `budget_usd`, or the idle worker has been idle for
/// >= `idle_limit_min`. Checked in that order. Says nothing about status; the caller decides.
pub fn rental_due(w: &Value, now: f64) -> Option<Reason> {
    let l = limits(w, now);
    if l.duration_due_in_sec().is_some_and(|d| d <= 0.0) {
        Some(Reason::Duration)
    } else if l.budget_due_in_sec().is_some_and(|d| d <= 0.0) {
        Some(Reason::Budget)
    } else if l.idle_due_in_sec().is_some_and(|d| d <= 0.0) {
        Some(Reason::Idle)
    } else {
        None
    }
}

fn describe(reason: Reason, l: &Limits) -> String {
    let n = |x: Option<f64>| x.map(|v| format!("{v:.2}")).unwrap_or_else(|| "?".into());
    match reason {
        Reason::Duration => format!(
            "elapsed {} s >= max_duration_min {} ({} s)",
            n(l.elapsed_sec),
            n(l.max_duration_min),
            n(l.max_duration_min.map(|m| m * 60.0))
        ),
        Reason::Budget => format!(
            "accrued ${} (elapsed {} s x ${}/hr) >= budget ${}",
            n(l.accrued_usd),
            n(l.elapsed_sec),
            n(l.rate_usd_hr),
            n(l.budget_usd)
        ),
        Reason::Idle => format!(
            "idle {} s >= idle_limit_min {}",
            n(l.idle_for_sec),
            n(l.idle_limit_min)
        ),
    }
}

/// A fresh `destroying` claim: a DELETE may be in flight, so nobody else may send one.
pub fn destroy_in_progress(w: &Value, now: f64) -> bool {
    status(w) == "destroying"
        && now - w["rental"]["destroying_at_epoch"].as_f64().unwrap_or(0.0) < CLAIM_STALE_SEC
}

/// Exclusive advisory lock over the rented-worker records (`<data>/locks/rentals.lock`). The
/// records are one JSON array in the settings table, so every read-modify-write of it, in this
/// process or another, runs under this lock. Released on drop (or process death).
pub struct RentalLock {
    _lock: rightkit_fs::FileLock,
}

impl RentalLock {
    pub fn acquire(ctx: &Context, wait: Duration) -> GenResult<Self> {
        let dir = ctx.locks_dir();
        rightkit_fs::ensure_private_dir(&dir)
            .map_err(|e| GenError::new("io", format!("create {}: {e}", dir.display())))?;
        let path = dir.join("rentals.lock");
        // `Flock` (not the default record lock) so threads of this process exclude each other too.
        match rightkit_fs::FileLock::try_lock_with(&path, rightkit_fs::LockKind::Flock, wait) {
            Ok(lock) => Ok(Self { _lock: lock }),
            Err(rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. }) => {
                Err(GenError::new(
                    "rentals_busy",
                    "another process is changing the rented-worker records; try again",
                ))
            }
            Err(e) => Err(GenError::new("io", format!("lock {}: {e}", path.display()))),
        }
    }
}

fn is_vast_rental(w: &Value) -> bool {
    w["spec"]["driver"].as_str() == Some("vast") && w["rental"].is_object()
}

/// Whether any rental could need attention; a plain read, so a no-rental install pays nothing.
fn any_live_rental(workers: &[Value]) -> bool {
    workers.iter().any(|w| {
        is_vast_rental(w)
            && matches!(
                status(w),
                "renting"
                    | "outcome_unknown"
                    | "active"
                    | "destroying"
                    | "destroy_unconfirmed"
                    | "destroyed_unsettled"
            )
    })
}

/// Phase 0 of enforcement: for each `outcome_unknown` rental, and each `renting` rental older than
/// [`RENTING_GRACE_SEC`], look the instance up by this worker's label (read-only).
///
/// - exactly one instance: adopted (status `active`, the reservation kept at the budget), recorded as
///   `auto_resolved: "adopted"`; the limits then apply and it is destroyed when due;
/// - none: the first sighting is recorded; a second at least [`GONE_REREAD_SEC`] later releases the
///   reservation, recorded as `auto_resolved: "none_found"`. An instance that appears in between
///   resets the count;
/// - more than one: NOTHING is destroyed; the record keeps `multiple_instances` and the report says so.
///
/// The lookup runs outside the rentals lock; each result is applied under it to the record as it then
/// is (skipped if the user resolved it meanwhile).
fn resolve_unknown(ctx: &Context, now: f64, have_key: bool, report: &mut Value) -> GenResult<()> {
    let push = |report: &mut Value, key: &str, v: Value| {
        if let Some(a) = report[key].as_array_mut() {
            a.push(v);
        }
    };
    let candidates: Vec<Value> = super::load_workers(ctx)?
        .into_iter()
        .filter(is_vast_rental)
        .filter(|w| match status(w) {
            "outcome_unknown" => true,
            "renting" => {
                now - w["rental"]["reserved_at_epoch"].as_f64().unwrap_or(0.0) >= RENTING_GRACE_SEC
            }
            _ => false,
        })
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    if !have_key {
        for w in &candidates {
            push(
                report,
                "blocked",
                json!({
                    "worker_id": w["worker_id"], "reason": "unknown_outcome", "detail": "the create's outcome is unknown and must be looked up by label",
                    "error": "no Vast API key is stored for this app and VAST_API_KEY is not set in this process, so the instance cannot be looked up from here",
                    "billing_warning": "an instance may exist and be billing",
                }),
            );
        }
        return Ok(());
    }
    let drv = super::remote::VastDriver::new(ctx);
    let lookups: Vec<(Value, GenResult<Vec<Value>>)> = candidates
        .into_iter()
        .map(|w| {
            let r = drv.find_by_label(&w);
            (w, r)
        })
        .collect();

    let _lock = match RentalLock::acquire(ctx, Duration::from_secs(10)) {
        Ok(l) => l,
        Err(e) if e.code == "rentals_busy" => {
            report["busy"] = json!(true);
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let mut workers = super::load_workers(ctx)?;
    let mut changed = false;
    for (seen, found) in lookups {
        let wid = seen["worker_id"].clone();
        let Some(w) = workers.iter_mut().find(|x| {
            x["worker_id"] == wid
                && x["rental"]["attempt"] == seen["rental"]["attempt"]
                && matches!(status(x), "renting" | "outcome_unknown")
        }) else {
            continue; // resolved by someone else since the lookup
        };
        let state = w["state"].as_str().unwrap_or_default().to_string();
        let found = match found {
            Ok(f) => f,
            Err(e) => {
                push(
                    report,
                    "failed",
                    json!({"worker_id": wid, "status": status(w), "reason": "unknown_outcome", "error": {"code": e.code, "message": e.message}, "billing_warning": w["rental"]["billing_warning"]}),
                );
                continue;
            }
        };
        match found.len() {
            1 => {
                adopt(w, &found[0]);
                w["rental"]["auto_resolved"] = json!("adopted");
                w["rental"]["auto_resolved_detail"] = json!({"instance_id": found[0]["id"], "at": store::now(), "at_epoch": now});
                if let Some(o) = w["rental"].as_object_mut() {
                    o.remove("none_found_first_epoch");
                    o.remove("multiple_instances");
                }
                super::worker_event(w, "auto_resolve", &state, "staged", json!({"adopted_instance": found[0]["id"]}));
                changed = true;
                push(report, "auto_resolved", json!({"worker_id": wid, "auto_resolved": "adopted", "instance_id": found[0]["id"]}));
            }
            0 => match w["rental"]["none_found_first_epoch"].as_f64() {
                Some(first) if now - first >= GONE_REREAD_SEC => {
                    match release(ctx, w, "no instance carries the label on two reads at least a minute apart (automatic)") {
                        None => {
                            w["rental"]["auto_resolved"] = json!("none_found");
                            w["rental"]["auto_resolved_detail"] = json!({"first_read_epoch": first, "second_read_epoch": now, "at": store::now()});
                            super::worker_event(w, "auto_resolve", &state, "configured", json!({"released": true, "auto_resolved": "none_found"}));
                            changed = true;
                            push(report, "auto_resolved", json!({"worker_id": wid, "auto_resolved": "none_found"}));
                        }
                        Some(msg) => {
                            changed = true;
                            push(report, "failed", json!({"worker_id": wid, "reason": "unknown_outcome", "error": {"code": "spend_ledger", "message": format!("could not release the reservation: {msg}")}}));
                        }
                    }
                }
                Some(_) => push(report, "unresolved", json!({"worker_id": wid, "status": status(w), "action": "no instance carries the label yet; the next read at least a minute after the first decides"})),
                None => {
                    w["rental"]["none_found_first_epoch"] = json!(now);
                    changed = true;
                    push(report, "unresolved", json!({"worker_id": wid, "status": status(w), "action": "no instance carries the label on the first read; a second at least a minute later releases the reservation"}));
                }
            },
            n => {
                let ids: Vec<Value> = found.iter().map(|i| i["id"].clone()).collect();
                w["rental"]["multiple_instances"] = json!(ids);
                if let Some(o) = w["rental"].as_object_mut() {
                    o.remove("none_found_first_epoch");
                }
                changed = true;
                push(report, "unresolved", json!({
                    "worker_id": wid, "status": status(w), "multiple_instances": ids,
                    "action": format!("{n} instances carry this worker's label: NOTHING was destroyed automatically; destroy the extras at the provider, then run resolve_rental"),
                    "billing_warning": "several instances may be billing",
                }));
            }
        }
    }
    if changed {
        super::save_workers(ctx, &workers)?;
    }
    Ok(())
}

/// `studio.models.rentals_enforce`: destroy and settle every rental past a limit. Idempotent and
/// safe to run concurrently with itself, the CLI and the sidecar:
///
/// 1. under the rentals lock, pick the due rentals and persist `destroying` for each (and settle any
///    `destroyed_unsettled`, a local ledger write with no DELETE);
/// 2. outside the lock, send ONE confirmed destroy per claimed rental (the `teardown` path, including
///    settlement), then record `auto_destroyed {reason, at}` or leave `destroy_unconfirmed`;
/// 3. under the lock again, persist each result onto the record it claimed.
///
/// A second pass sees `destroying`/`settled` and sends nothing. Phase 0 first resolves `renting`
/// (older than [`RENTING_GRACE_SEC`]) and `outcome_unknown` rentals by label (`resolve_unknown`):
/// that never destroys anything itself; an adopted rental is then subject to the limits below.
pub fn enforce_rentals(ctx: &Context) -> GenResult<Value> {
    let now = now_epoch();
    let mut report = json!({
        "at_epoch": now, "destroyed": [], "failed": [], "blocked": [], "unresolved": [],
        "in_progress": [], "settled_only": [], "auto_resolved": [], "persist_errors": [], "busy": false,
        "limits": HONEST_LIMITS,
    });
    let push = |report: &mut Value, key: &str, v: Value| {
        if let Some(a) = report[key].as_array_mut() {
            a.push(v);
        }
    };
    // Cheap exit: only reads.
    if !any_live_rental(&super::load_workers(ctx)?) {
        return Ok(report);
    }
    // The same resolution every Vast call uses: stored credential, then VAST_API_KEY.
    let have_key = super::remote::resolve_key(ctx, None).is_ok();

    // Phase 0: look up the instance of a create whose outcome is unknown.
    resolve_unknown(ctx, now, have_key, &mut report)?;

    // Phase 1: claim, under the lock.
    let mut claimed: Vec<(Value, &'static str, String, String)> = Vec::new(); // worker, reason, detail, token
    {
        let _lock = match RentalLock::acquire(ctx, Duration::from_secs(10)) {
            Ok(l) => l,
            Err(e) if e.code == "rentals_busy" => {
                report["busy"] = json!(true);
                return Ok(report);
            }
            Err(e) => return Err(e),
        };
        let mut workers = super::load_workers(ctx)?;
        let mut changed = false;
        for w in workers.iter_mut() {
            if !is_vast_rental(w) {
                continue;
            }
            let wid = w["worker_id"].clone();
            let st = status(w).to_string();
            let (reason, detail): (&'static str, String) = match st.as_str() {
                "renting" | "outcome_unknown" => {
                    // Phase 0 already looked this one up; what is left is still unresolved (inside the
                    // grace, awaiting the second read, several labelled instances, or the lookup failed).
                    push(
                        &mut report,
                        "unresolved",
                        json!({
                            "worker_id": wid, "status": st, "billing_warning": w["rental"]["billing_warning"],
                            "multiple_instances": w["rental"]["multiple_instances"],
                            "action": "enforcement looks this rental up by label and never destroys an instance whose create outcome is unknown; run resolve_rental to decide sooner",
                        }),
                    );
                    continue;
                }
                "destroyed_unsettled" => {
                    // The DELETE was confirmed earlier; only the ledger write is owed.
                    let state = w["state"].as_str().unwrap_or_default().to_string();
                    match settle(ctx, w) {
                        Ok(s) => {
                            super::worker_event(w, "auto_settle", &state, "terminated", s.clone());
                            changed = true;
                            push(
                                &mut report,
                                "settled_only",
                                json!({"worker_id": wid, "settlement": s}),
                            );
                        }
                        Err(e) => push(
                            &mut report,
                            "failed",
                            json!({"worker_id": wid, "status": st, "error": {"code": e.code, "message": e.message}}),
                        ),
                    }
                    continue;
                }
                "active" => match rental_due(w, now) {
                    Some(r) => (r.as_str(), describe(r, &limits(w, now))),
                    None => continue,
                },
                "destroy_unconfirmed" => {
                    if now
                        - w["rental"]["destroy_error"]["at_epoch"]
                            .as_f64()
                            .unwrap_or(0.0)
                        < RETRY_AFTER_SEC
                    {
                        continue;
                    }
                    (
                        "retry",
                        "the previous destroy was not confirmed by the provider".to_string(),
                    )
                }
                "destroying" => {
                    if destroy_in_progress(w, now) {
                        push(&mut report, "in_progress", json!({"worker_id": wid}));
                        continue;
                    }
                    (
                        "retry",
                        "a previous destroy claim never finished".to_string(),
                    )
                }
                _ => continue,
            };
            if !have_key {
                push(
                    &mut report,
                    "blocked",
                    json!({
                        "worker_id": wid, "reason": reason, "detail": detail,
                        "error": "no Vast API key is stored for this app and VAST_API_KEY is not set in this process, so the instance cannot be destroyed from here",
                        "billing_warning": "the instance is past a limit and may still be billing",
                    }),
                );
                continue;
            }
            let token = super::new_worker_id();
            w["rental"]["status"] = json!("destroying");
            w["rental"]["destroying_at_epoch"] = json!(now);
            w["rental"]["destroying_reason"] = json!(reason);
            w["rental"]["destroy_claim"] = json!(token);
            note_destroy_attempt(w);
            w["rental"]["billing_warning"] = json!(BILLING_WARNING);
            changed = true;
            claimed.push((w.clone(), reason, detail, token));
        }
        if changed {
            super::save_workers(ctx, &workers)?;
        }
    }

    // Phase 2: one confirmed destroy per claim, outside the lock.
    let mut finished: Vec<(Value, String)> = Vec::new();
    for (mut w, reason, detail, token) in claimed {
        let wid = w["worker_id"].clone();
        let state = w["state"].as_str().unwrap_or_default().to_string();
        match super::teardown_rental(ctx, &mut w) {
            Ok(r) => {
                w["rental"]["auto_destroyed"] = json!({"reason": reason, "detail": detail, "at": store::now(), "at_epoch": now_epoch()});
                super::worker_event(
                    &mut w,
                    "auto_destroy",
                    &state,
                    "terminated",
                    json!({"reason": reason, "detail": detail, "result": r}),
                );
                push(
                    &mut report,
                    "destroyed",
                    json!({
                        "worker_id": wid, "reason": reason, "detail": detail, "instance_id": r["instance_id"],
                        "status": w["rental"]["status"], "settlement": r["settlement"],
                    }),
                );
            }
            Err(e) => {
                w["rental"]["auto_destroy_attempt"] = json!({"reason": reason, "detail": detail, "at": store::now(), "error": {"code": e.code, "message": e.message}});
                super::worker_event(
                    &mut w,
                    "auto_destroy",
                    &state,
                    "error",
                    json!({"reason": reason, "code": e.code, "message": e.message}),
                );
                push(
                    &mut report,
                    "failed",
                    json!({
                        "worker_id": wid, "reason": reason, "detail": detail, "status": w["rental"]["status"],
                        "error": {"code": e.code, "message": e.message}, "billing_warning": w["rental"]["billing_warning"],
                    }),
                );
            }
        }
        finished.push((w, token));
    }

    // Phase 3: persist each result onto the record it claimed.
    if !finished.is_empty() {
        match RentalLock::acquire(ctx, Duration::from_secs(60)) {
            Ok(_lock) => {
                let mut workers = super::load_workers(ctx)?;
                for (w, token) in finished {
                    match workers.iter_mut().find(|x| {
                        x["worker_id"] == w["worker_id"]
                            && x["rental"]["destroy_claim"].as_str() == Some(token.as_str())
                    }) {
                        Some(slot) => *slot = w,
                        None => push(
                            &mut report,
                            "persist_errors",
                            json!({"worker_id": w["worker_id"], "error": "the record changed under the claim; its current state was kept"}),
                        ),
                    }
                }
                super::save_workers(ctx, &workers)?;
            }
            Err(e) => {
                for (w, _) in finished {
                    push(
                        &mut report,
                        "persist_errors",
                        json!({
                            "worker_id": w["worker_id"],
                            "error": format!("{}: the result could not be saved; the record stays `destroying` and is taken over after {} s", e.message, CLAIM_STALE_SEC),
                        }),
                    );
                }
            }
        }
    }
    Ok(report)
}

fn acted(report: &Value) -> bool {
    [
        "destroyed",
        "failed",
        "blocked",
        "unresolved",
        "settled_only",
        "auto_resolved",
        "persist_errors",
    ]
    .iter()
    .any(|k| report[*k].as_array().is_some_and(|a| !a.is_empty()))
}

/// Run enforcement and record one diagnostic event when it did or found anything (stdout belongs
/// to the command's own result or the sidecar protocol; stderr is not a diagnostics channel). Never fails the caller.
pub fn enforce_and_report(ctx: &Context) {
    match enforce_rentals(ctx) {
        Ok(r) if acted(&r) => rightkit_media_gen::host::diag::event(
            rightkit_logs::Level::Info,
            "rentals_enforced",
            &[("report", r)],
        ),
        Ok(_) => {}
        Err(e) => rightkit_media_gen::host::diag::error(
            "rentals_enforce_error",
            &[("code", json!(e.code)), ("message", json!(e.message))],
        ),
    }
}

/// Stops the sidecar's enforcement thread when dropped, waiting for an in-flight destroy so its
/// result is saved before the process exits.
pub struct EnforcerGuard {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for EnforcerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Sidecar enforcement: once immediately, then every [`SIDECAR_INTERVAL`] until the guard drops.
pub fn spawn_enforcer() -> EnforcerGuard {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::Builder::new()
        .name("rental-enforcer".into())
        .spawn(move || {
            let ctx = Context::production();
            while !flag.load(Ordering::SeqCst) {
                enforce_and_report(&ctx);
                let until = Instant::now() + SIDECAR_INTERVAL;
                while Instant::now() < until {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        })
        .ok();
    EnforcerGuard { stop, handle }
}

/// `studio.models.rentals`: every worker that holds or held a Vast rental, with elapsed, accrued,
/// time left on each limit and warnings. Read only.
pub fn list(ctx: &Context) -> GenResult<Value> {
    let now = now_epoch();
    let mut rentals = Vec::new();
    for w in super::load_workers(ctx)?
        .iter()
        .filter(|w| is_vast_rental(w))
    {
        let l = limits(w, now);
        let st = status(w);
        let live = matches!(st, "active" | "destroying" | "destroy_unconfirmed");
        let mut warnings = l.warnings.clone();
        if let Some(b) = w["rental"]["billing_warning"].as_str() {
            warnings.push(b.to_string());
        }
        if live && !matches!(st, "active") {
            warnings.push(format!("rental status is {st}"));
        }
        rentals.push(json!({
            "worker_id": w["worker_id"], "label": w["label"], "worker_state": w["state"],
            "rental_status": st, "instance_id": w["rental"]["instance_id"],
            "elapsed_sec": l.elapsed_sec, "rate_usd_hr": l.rate_usd_hr, "accrued_usd": l.accrued_usd,
            "budget_usd": l.budget_usd, "max_duration_min": l.max_duration_min,
            "idle_limit_min": l.idle_limit_min, "idle_for_sec": l.idle_for_sec,
            "due_in_sec": {"duration": l.duration_due_in_sec(), "budget": l.budget_due_in_sec(), "idle": l.idle_due_in_sec()},
            "due": if st == "active" { rental_due(w, now).map(|r| r.as_str()) } else { None },
            "auto_destroyed": w["rental"]["auto_destroyed"],
            "auto_resolved": w["rental"]["auto_resolved"],
            "estimated_end": w["rental"]["estimated_end"],
            "warnings": warnings,
        }));
    }
    Ok(json!({
        "now_epoch": now, "clock_overridden": false,
        "rentals": rentals, "enforcement": HONEST_LIMITS,
    }))
}
