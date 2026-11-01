//! Local and rented model compute. [`models`] is the local model catalogue, hardware fit, the
//! memory-aware governor for heavy local models, installs and the `studio.models.*` commands.
//! Rented GPU workers: the register, stage, run and teardown lifecycle of remote workers, the
//! drivers behind it ([`remote`]: SSH, Vast.ai and, in tests, a local folder stand-in) and the
//! time-billed spend bracket around a rental ([`rental`]: reserve before the create, settle at
//! destroy, enforcement of duration, budget and idle limits).
//!
//! Worker records live as one JSON array in the job store's settings table under
//! [`WORKERS_KEY`]. Every read-modify-write of that array should run under
//! [`rental::RentalLock`].

use serde_json::{json, Value};

use rightkit_media_gen::{store, Context, GenResult};

pub mod models;
pub mod remote;
pub mod rental;

/// Settings key of the worker records array.
pub const WORKERS_KEY: &str = "models.rented_workers";

/// Read a JSON setting, or `default` when it is absent or unreadable.
pub fn get_json(ctx: &Context, key: &str, default: Value) -> GenResult<Value> {
    let conn = store::open(ctx)?;
    Ok(conn
        .read(|c| store::settings().get(c, key).map_err(store::settings_err))
        .map_err(store::je)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(default))
}

pub fn set_json(ctx: &Context, key: &str, value: &Value) -> GenResult<()> {
    let conn = store::open(ctx)?;
    conn.write(|c| {
        store::settings()
            .set(c, key, &value.to_string())
            .map_err(store::settings_err)
    })
    .map_err(store::je)
}

/// Worker ids from the owner's random identifier (`rightkit-logs` `identity::random_uuid`).
pub fn new_worker_id() -> String {
    format!("rw-{}", rightkit_logs::identity::random_uuid())
}

pub fn load_workers(ctx: &Context) -> GenResult<Vec<Value>> {
    Ok(get_json(ctx, WORKERS_KEY, json!([]))?
        .as_array()
        .cloned()
        .unwrap_or_default())
}

pub fn save_workers(ctx: &Context, workers: &[Value]) -> GenResult<()> {
    set_json(ctx, WORKERS_KEY, &Value::Array(workers.to_vec()))
}

pub fn worker_event(worker: &mut Value, action: &str, from: &str, to: &str, detail: Value) {
    let event =
        json!({"at": store::now(), "action": action, "from": from, "to": to, "detail": detail});
    worker["state"] = json!(to);
    worker["updated_at"] = json!(store::now());
    if let Some(events) = worker["events"].as_array_mut() {
        events.push(event);
    } else {
        worker["events"] = json!([event]);
    }
}

/// The billed create, bracketed by the spend ledger: validate, RESERVE the budget (all three limits;
/// a refusal sends nothing), persist the `renting` record, re-quote, create once. A failure before
/// the PUT or a provable non-delivery releases; a possibly delivered create is HELD, never retried.
pub fn provision_rental(
    ctx: &Context,
    workers: &mut [Value],
    idx: usize,
    w: &mut Value,
    input: &Value,
) -> GenResult<Value> {
    if let Some(e) = rental::blocks_new_rental(w) {
        return Err(e);
    }
    let p = remote::VastDriver::rent_params(ctx, input)?;
    rental::open(ctx, w, &p)?;
    workers[idx] = w.clone();
    if let Err(e) = save_workers(ctx, workers) {
        rental::release(ctx, w, "rental record could not be saved before the create");
        return Err(e);
    }
    let drv = remote::VastDriver::new(ctx);
    let dph = match drv.quote(&p) {
        Ok(d) => d,
        Err(e) => {
            rental::release(ctx, w, "refused before the create");
            return Err(e);
        }
    };
    match drv.create(w, &p, dph) {
        Ok(mut v) => {
            rental::activate(w, &v["instance_id"], dph);
            // The instance exists and bills: persist the active record NOW, retrying. If every
            // attempt fails the disk still holds the `renting` record with this worker's label, which
            // enforcement finds the instance by (rental.rs `resolve_unknown`) after the grace period.
            workers[idx] = w.clone();
            // Only the SAVE retries (bounded, owner backoff `RetryPolicy::backoff`); the billed `drv.create` above ran
            // once and is never repeated.
            let save_retry = rightkit_media_gen::host::http::RetryPolicy {
                max_attempts: 3,
                initial_backoff: std::time::Duration::from_millis(250),
                max_backoff: std::time::Duration::from_secs(1),
                multiplier: 2.0,
                jitter: 0.0,
                ..Default::default()
            };
            let mut saved = false;
            for attempt in 1..=save_retry.max_attempts {
                if save_workers(ctx, workers).is_ok() {
                    saved = true;
                    break;
                }
                std::thread::sleep(save_retry.backoff(attempt));
            }
            if !saved {
                v["record_save_warning"] = json!("the active rental record could not be saved; the disk still holds the `renting` record with this worker's label, so enforcement finds the instance by label");
                rightkit_media_gen::host::diag::error(
                    "rental_record_save_failed",
                    &[
                        ("worker_id", w["worker_id"].clone()),
                        ("label", w["rental"]["label"].clone()),
                        ("instance_id", v["instance_id"].clone()),
                    ],
                );
            }
            Ok(v)
        }
        Err(r) => {
            if r.delivered {
                rental::hold(w, &r.error);
            } else {
                rental::release(ctx, w, "create provably not delivered");
            }
            Err(r.error)
        }
    }
}

/// Destroy a rented instance and settle its reservation. Only a 2xx to the DELETE counts as
/// destroyed, with one exception: after a destroy that was not a clean 2xx, the provider is asked
/// whether the instance still exists (`rental::check_gone`); if it says it does not, the destroy is
/// treated as confirmed-gone and settles with `estimated_end: true` (elapsed to the FIRST destroy
/// attempt). Anything else leaves the reservation open with the billing warning on the record.
pub fn teardown_rental(ctx: &Context, w: &mut Value) -> GenResult<Value> {
    if w["rental"]["status"].as_str() != Some("destroyed_unsettled") {
        rental::note_destroy_attempt(w);
        let drv = remote::VastDriver::new(ctx);
        match drv.destroy(w) {
            Ok(f) => rental::destroyed(w, &f),
            Err(e) => match rental::check_gone(&drv, w, Some(&e)) {
                rental::GoneVerdict::Gone(how) => {
                    let id = w["rental"]["instance_id"]
                        .as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| {
                            w["spec"]["vast"]["instance_id"]
                                .as_i64()
                                .map(|n| n.to_string())
                        })
                        .unwrap_or_default();
                    rental::destroyed_gone(w, &id, how);
                }
                _ => {
                    rental::destroy_unconfirmed(w, &e);
                    return Err(e);
                }
            },
        }
    }
    let settlement = rental::settle(ctx, w)?;
    Ok(
        json!({"released": true, "driver": "vast", "instance_id": w["rental"]["instance_id"], "settlement": settlement}),
    )
}

/// Teardown of a Vast worker that has an instance id but NO rental record (created before the spend
/// ledger bracket). Never a blind delete: the destroy reads the instance and requires this worker's
/// label (`instance_not_owned` otherwise). On success the cost is recorded on the worker, not in the
/// spend ledger (no reservation exists): at the worker's declared `budget_usd` when one is known, else
/// `unsettled_legacy`.
pub fn teardown_legacy_vast(ctx: &Context, w: &mut Value) -> GenResult<Value> {
    let f = remote::VastDriver::new(ctx).destroy(w)?;
    let budget = w["budget_usd"]
        .as_f64()
        .filter(|b| b.is_finite() && *b > 0.0);
    let settlement = match budget {
        Some(b) => json!({
            "status": "legacy_declared_budget", "amount_usd": b, "ledger_written": false,
            "note": "this rental predates the spend ledger bracket; the worker's declared budget_usd is recorded as its cost on the worker record only, not in the spend ledger",
        }),
        None => json!({
            "status": "unsettled_legacy", "ledger_written": false,
            "note": "this rental predates the spend ledger bracket and no budget was declared: its cost is NOT recorded anywhere; check the provider's invoice",
        }),
    };
    w["legacy_settlement"] = settlement.clone();
    Ok(
        json!({"released": true, "driver": "vast", "instance_id": f.instance_id, "settlement": settlement}),
    )
}

// Test binaries must never reach the OS credential store or the user's real data folder.
#[cfg(test)]
mod no_keychain_in_tests {
    #[ctor::ctor(unsafe)]
    fn guard() {
        std::env::set_var("RIGHTKIT_NO_KEYCHAIN", "1");
        if std::env::var_os("RIGHTKIT_APP_DATA")
            .filter(|v| !v.is_empty())
            .is_none()
        {
            let dir = std::env::temp_dir().join(format!(
                "rightkit-remote-compute-unit-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("isolated test data folder");
            std::env::set_var("RIGHTKIT_APP_DATA", dir);
        }
    }
}
