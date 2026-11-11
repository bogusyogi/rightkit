//! The `studio.models.*` commands for rented workers: the worker records (`workers`), the
//! Vast.ai offer search (`rental.search`), the worker lifecycle (`worker.action`: register,
//! stage, reconnect, begin, retrieve, idle, unload, teardown, provision) and the rental bracket
//! (`rentals`, `rentals_enforce`). The command names keep the `studio.models.` prefix of the
//! engine wire; the local model commands are `rightkit-media-gen-models`.
//!
//! - `workers`   → persisted rented-worker records incl. budget state.
//! - `rental.search` `{query?, api_key?}` → Vast offer search. The key is resolved like every Vast
//!   call: the stored credential, then `VAST_API_KEY`, then the per-call `api_key` (never persisted).
//!   `no_credentials` without one.
//! - `worker.action` `{worker_id?|spec, action}` → register / stage /
//!   reconnect / begin / retrieve (hash-checked) / idle / unload / teardown /
//!   provision (Vast create — requires `authorized:true` + offer_id +
//!   max_usd_hr + budget_usd + max_duration_min). Drivers: `spec.driver`
//!   `ssh` | `vast` | `local-fs` | none→NullDriver (`driver_not_configured`).
//!   No action launches a billed instance without the explicit provision call.
//!   `provision` reserves its `budget_usd` in the spend ledger first and `teardown` settles it
//!   (see `rental.rs`); `resolve_rental` resolves a create whose outcome is unknown;
//!   `resolve_destroy` re-checks a `destroy_unconfirmed` rental (confirmed-gone settles with
//!   `estimated_end`). `set_vast_key` `{api_key}` saves the Vast key in the OS credential store
//!   (empty deletes it) and `vast_key_status` reports whether one is stored; neither returns the key.
//!   `provision` is refused (`no_stored_key`) unless a key is stored, so a rental can never exist
//!   that the app cannot destroy. A destroy only goes to an instance carrying this worker's label
//!   (`instance_not_owned` otherwise), and `register` takes no `spec.vast.instance_id`.
//!   `teardown` of a worker that is `running` is refused unless `force:true` (it interrupts the
//!   work), then follows the same confirmed-destroy and settlement path.
//! - `rentals` → every Vast rental with elapsed, accrued and time left on its duration, budget and
//!   idle limits, plus warnings. `rentals_enforce` → destroys and settles the rentals past a limit
//!   and reports what it did (`rental.rs`).
//!   Enforcement also resolves `renting`/`outcome_unknown` creates by label (adopt one, release after
//!   none twice a minute apart, report several, never destroy automatically on several).
//!   HONEST LIMIT: enforcement runs only while the host app runs (sidecar start, every 60 s while the app
//!   is open, the start of each CLI command touching workers or generation, and `rentals_enforce`).
//!   With neither the app nor a CLI command running, nothing enforces anything and a rented instance
//!   bills until the next engine start or until it is destroyed at the provider. An instance-side
//!   maximum lifetime in the create request would be the real fix; it needs the provider's documented
//!   field name from a fetched source and none is sent today.

use serde_json::{json, Value};

use rightkit_media_gen::{store, Context, GenError, GenResult};

use crate::{
    load_workers, new_worker_id, provision_rental, save_workers, teardown_legacy_vast,
    teardown_rental, worker_event,
};
use crate::{remote, rental};

/// Every command name this module owns.
pub const COMMANDS: &[&str] = &[
    "studio.models.workers",
    "studio.models.rental.search",
    "studio.models.worker.action",
    "studio.models.rentals",
    "studio.models.rentals_enforce",
];

/// Run one of [`COMMANDS`].
pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.models.workers" => Ok(json!({"workers": load_workers(ctx)?,
                                            "driver": remote::default_driver_name()})),
        "studio.models.rental.search" => remote::VastDriver::new(ctx).search(input),
        "studio.models.worker.action" => worker_action(ctx, input),
        "studio.models.rentals" => rental::list(ctx),
        "studio.models.rentals_enforce" => rental::enforce_rentals(ctx),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

/// Every worker-record read-modify-write runs under the rentals lock, so the CLI, the sidecar and
/// the enforcement pass cannot overwrite each other's changes to the one records array.
fn worker_action(ctx: &Context, input: &Value) -> GenResult<Value> {
    let _lock = rental::RentalLock::acquire(ctx, std::time::Duration::from_secs(90))?;
    worker_action_locked(ctx, input)
}

fn worker_action_locked(ctx: &Context, input: &Value) -> GenResult<Value> {
    let action = input["action"].as_str().unwrap_or_default();
    let mut workers = load_workers(ctx)?;

    // The Vast key lives in the OS credential store, never in a worker record. Neither action
    // returns, logs or echoes the key.
    if action == "set_vast_key" {
        let key = input["api_key"].as_str().unwrap_or("");
        remote::store_key(ctx, key)?;
        return Ok(json!({"ok": true, "vast_key_stored": remote::key_is_stored(ctx)}));
    }
    if action == "vast_key_status" {
        return Ok(json!({"vast_key_stored": remote::key_is_stored(ctx)}));
    }

    if action == "register" {
        let spec = input.get("spec").cloned().unwrap_or_else(|| json!({}));
        // A Vast worker is minted with a fresh id, so no existing instance can carry its label: an
        // instance id supplied here could only point at someone else's instance (and a later teardown
        // would destroy it). Adoption goes through provision then resolve_rental, which match by label.
        if spec["driver"].as_str() == Some("vast") && !spec["vast"]["instance_id"].is_null() {
            return Err(GenError::new(
                "instance_not_owned",
                "a Vast worker cannot be registered with an instance_id: no existing instance carries the label of a worker that does not exist yet. Provision it (or resolve_rental) so the instance is matched by this worker's label",
            ));
        }
        let id = new_worker_id();
        let budget = input["budget_usd"].as_f64().unwrap_or(0.0);
        let w = json!({
            "worker_id": id,
            "label": input["label"].as_str().unwrap_or("rented worker"),
            "spec": spec,
            "driver": remote::driver_for(ctx, &json!({"spec": spec})).name(),
            "state": "configured",
            "budget_usd": budget,
            "accrued_usd": 0.0,
            "outputs": [],
            "events": [{"at": store::now(), "action": "register", "from": null, "to": "configured", "detail": spec.clone()}],
            "created_at": store::now(),
            "updated_at": store::now(),
        });
        workers.push(w.clone());
        save_workers(ctx, &workers)?;
        return Ok(w);
    }

    let id = input["worker_id"].as_str().unwrap_or_default();
    let idx = workers
        .iter()
        .position(|w| w["worker_id"].as_str() == Some(id))
        .ok_or_else(|| GenError::new("worker_not_found", format!("no such worker: {id}")))?;
    let mut w = workers[idx].clone();
    let state = w["state"].as_str().unwrap_or_default().to_string();
    let drv = remote::driver_for(ctx, &w);

    let accrue = |w: &mut Value, input: &Value| {
        if let Some(c) = input["cost_usd"].as_f64() {
            let cur = w["accrued_usd"].as_f64().unwrap_or(0.0);
            w["accrued_usd"] = json!(cur + c);
        }
    };

    // Budget gate: refuse billable transitions once the recorded spend reaches
    // the worker's cap (0 = uncapped record-keeping only).
    let over_budget = |w: &Value| {
        let b = w["budget_usd"].as_f64().unwrap_or(0.0);
        b > 0.0 && w["accrued_usd"].as_f64().unwrap_or(0.0) >= b
    };

    match action {
        "stage" => {
            if !matches!(state.as_str(), "configured" | "error") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot stage from {state}"),
                ));
            }
            if over_budget(&w) {
                return Err(GenError::new("budget_exceeded", "worker budget reached"));
            }
            worker_event(&mut w, "stage", &state, "staging", Value::Null);
            match drv.stage(&w) {
                Ok(receipt) => {
                    accrue(&mut w, input);
                    worker_event(&mut w, "stage", "staging", "staged", receipt);
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "stage",
                        "staging",
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "reconnect" => {
            if matches!(state.as_str(), "terminated") {
                return Err(GenError::new("bad_state", "worker is terminated"));
            }
            match drv.reconnect(&w) {
                Ok(r) => worker_event(&mut w, "reconnect", &state, &state, r),
                Err(e) => {
                    worker_event(
                        &mut w,
                        "reconnect",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        // `begin` marks compute starting on an already-staged worker. It never
        // provisions a billed instance — that is the separately authorized
        // `provision` action below.
        "begin" => {
            if state != "staged" && state != "idle" {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot begin from {state}"),
                ));
            }
            if over_budget(&w) {
                return Err(GenError::new("budget_exceeded", "worker budget reached"));
            }
            match drv.begin(&w) {
                Ok(r) => {
                    accrue(&mut w, input);
                    if let Some(o) = w.as_object_mut() {
                        o.remove("idle_since");
                        o.remove("idle_since_epoch");
                    }
                    worker_event(&mut w, "begin", &state, "running", r);
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "begin",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "retrieve" => {
            if !matches!(state.as_str(), "running" | "idle" | "staged") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot retrieve from {state}"),
                ));
            }
            worker_event(&mut w, "retrieve", &state, "retrieving", Value::Null);
            let dest = ctx.library_dir().join("rented").join(id);
            match drv.retrieve(&w, &dest) {
                Ok(files) => {
                    let count = files.len();
                    w["outputs"] = json!(files);
                    worker_event(
                        &mut w,
                        "retrieve",
                        "retrieving",
                        "retrieved",
                        json!({"dest": dest, "files": count}),
                    );
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "retrieve",
                        "retrieving",
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "idle" => {
            // A provisioned rental that is staged and waiting bills exactly like an idle running one,
            // so it may be marked idle too (and then falls under `idle_limit_min`).
            let rental_waiting =
                state == "staged" && w["rental"]["status"].as_str() == Some("active");
            if state != "running" && !rental_waiting {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot idle from {state}"),
                ));
            }
            let limit = input["idle_limit_min"].as_i64().unwrap_or(30);
            w["idle_since"] = json!(store::now());
            w["idle_since_epoch"] = json!(rental::now_epoch());
            w["idle_limit_min"] = json!(limit);
            worker_event(
                &mut w,
                "idle",
                &state,
                "idle",
                json!({"idle_limit_min": limit}),
            );
        }
        "unload" => {
            // Unload model weights but keep the worker reachable (staged).
            if !matches!(state.as_str(), "running" | "idle" | "retrieved") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot unload from {state}"),
                ));
            }
            if let Some(o) = w.as_object_mut() {
                o.remove("idle_since");
                o.remove("idle_since_epoch");
            }
            worker_event(&mut w, "unload", &state, "staged", Value::Null);
        }
        // Vast instance create: billed — requires explicit authorized target + budget in the
        // input. The budget is RESERVED in the spend ledger first (daily cap, monthly cap, device
        // allowance); see `rental.rs` for the whole reserve / hold / settle lifecycle.
        "provision" => {
            if !matches!(state.as_str(), "configured" | "staged" | "error") {
                return Err(GenError::new(
                    "bad_state",
                    format!("cannot provision from {state}"),
                ));
            }
            if w["spec"]["driver"].as_str() != Some("vast") {
                return Err(GenError::new(
                    "invalid_input",
                    "provision applies to vast workers only",
                ));
            }
            // A worker that holds, or may hold, an instance is refused without touching its state.
            if let Some(e) = rental::blocks_new_rental(&w) {
                return Err(e);
            }
            match provision_rental(ctx, &mut workers, idx, &mut w, input) {
                Ok(v) => {
                    let inst = v["instance_id"].clone();
                    w["spec"]["vast"]["instance_id"] = inst;
                    worker_event(&mut w, "provision", &state, "staged", v);
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "provision",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        // Resolve a rental whose create may have been delivered: adopt the one instance carrying
        // this worker's label, or release the reservation once the user confirms none exists.
        "resolve_rental" => {
            if !matches!(
                w["rental"]["status"].as_str(),
                Some("renting" | "outcome_unknown")
            ) {
                return Err(GenError::new("bad_state", "no held rental to resolve"));
            }
            let found = remote::VastDriver::new(ctx).find_by_label(&w)?;
            match found.len() {
                1 => {
                    rental::adopt(&mut w, &found[0]);
                    worker_event(
                        &mut w,
                        "resolve_rental",
                        &state,
                        "staged",
                        json!({"adopted_instance": found[0]["id"]}),
                    );
                }
                0 if input["confirm_none"].as_bool() == Some(true) => {
                    if let Some(msg) = rental::release(
                        ctx,
                        &mut w,
                        "no instance carries the label; confirmed by the user",
                    ) {
                        return Err(GenError::new(
                            "spend_ledger",
                            format!("could not release the reservation: {msg}"),
                        ));
                    }
                    worker_event(
                        &mut w,
                        "resolve_rental",
                        &state,
                        "configured",
                        json!({"released": true}),
                    );
                }
                0 => {
                    return Err(GenError::new(
                        "rental_unresolved",
                        "no instance carries this worker's label; pass confirm_none:true only when sure the create never reached Vast (the reservation stays open until then)",
                    ));
                }
                n => {
                    let ids: Vec<Value> = found.iter().map(|i| i["id"].clone()).collect();
                    return Err(GenError::new(
                        "rental_unresolved",
                        format!("{n} instances carry this worker's label ({}); destroy the extras on Vast, then resolve again", Value::Array(ids)),
                    ));
                }
            }
        }
        // Re-check a destroy the provider did not confirm: if it says the instance does not exist
        // (404 on the instance read, or absent from the label list on two reads a minute apart) the
        // destroy counts as confirmed-gone and settles with `estimated_end`. Never sends a DELETE.
        "resolve_destroy" => {
            if w["rental"]["status"].as_str() != Some("destroy_unconfirmed") {
                return Err(GenError::new(
                    "bad_state",
                    "no unconfirmed destroy to resolve",
                ));
            }
            let drv = remote::VastDriver::new(ctx);
            match rental::check_gone(&drv, &mut w, None) {
                rental::GoneVerdict::Gone(how) => {
                    let iid = w["rental"]["instance_id"]
                        .as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| {
                            w["spec"]["vast"]["instance_id"]
                                .as_i64()
                                .map(|n| n.to_string())
                        })
                        .unwrap_or_default();
                    rental::destroyed_gone(&mut w, &iid, how);
                    let settlement = rental::settle(ctx, &mut w)?;
                    worker_event(
                        &mut w,
                        "resolve_destroy",
                        &state,
                        "terminated",
                        json!({"confirmed_gone": how, "settlement": settlement}),
                    );
                }
                other => {
                    let (result, why) = match &other {
                        rental::GoneVerdict::StillPresent => ("still_exists", "the provider still lists the instance: it may still be billing; teardown retries the destroy".to_string()),
                        rental::GoneVerdict::PendingSecondRead => ("pending_second_read", "absent from the label list once; read again at least a minute later".to_string()),
                        rental::GoneVerdict::Unknown(m) => ("unknown", m.clone()),
                        rental::GoneVerdict::Gone(_) => ("gone", String::new()),
                    };
                    w["rental"]["resolve_destroy"] =
                        json!({"result": result, "detail": why, "at": store::now()});
                    worker_event(
                        &mut w,
                        "resolve_destroy",
                        &state,
                        &state,
                        json!({"result": result, "detail": why}),
                    );
                }
            }
        }
        "teardown" => {
            let vast_rental_live = w["spec"]["driver"].as_str() == Some("vast")
                && matches!(
                    w["rental"]["status"].as_str(),
                    Some("active" | "destroy_unconfirmed" | "destroyed_unsettled" | "destroying")
                );
            let force = input["force"].as_bool() == Some(true);
            let forced = state == "running" && vast_rental_live && force;
            if matches!(state.as_str(), "running" | "staging" | "retrieving") && !forced {
                let message = if vast_rental_live && state == "running" {
                    "the rental is running work and is billing; teardown would interrupt it. Pass \"force\": true to destroy it anyway (the destroy is confirmed by the provider and the spend is settled as usual)".to_string()
                } else {
                    format!("finish or cancel in-flight work before teardown (state {state})")
                };
                return Err(GenError::new("bad_state", message));
            }
            if matches!(
                w["rental"]["status"].as_str(),
                Some("renting" | "outcome_unknown")
            ) {
                return Err(GenError::new(
                    "rental_unresolved",
                    "a Vast create may have been delivered and may be billing; run resolve_rental first",
                ));
            }
            if rental::destroy_in_progress(&w, rental::now_epoch()) {
                return Err(GenError::new(
                    "destroy_in_progress",
                    "an automatic destroy of this rental is in flight; read the worker again in a moment",
                ));
            }
            // A rental the provider already confirmed destroyed and the ledger already settled has
            // nothing left to destroy: never send a second DELETE for it (an unrelated instance
            // could carry a reused id), and never touch its settlement.
            let vast_rental_settled = w["spec"]["driver"].as_str() == Some("vast")
                && w["rental"]["status"].as_str() == Some("settled");
            let result = if vast_rental_live {
                teardown_rental(ctx, &mut w)
            } else if vast_rental_settled {
                Ok(
                    json!({"released": true, "driver": "vast", "instance_id": w["rental"]["instance_id"], "already_settled": true}),
                )
            } else if drv.name() == "vast" && !w["rental"].is_object() {
                teardown_legacy_vast(ctx, &mut w)
            } else {
                drv.teardown(&w)
            };
            match result {
                Ok(mut r) => {
                    if forced {
                        r["forced"] = json!(true);
                        r["interrupted"] = json!(rental::FORCE_NOTE);
                        w["rental"]["forced_teardown"] = json!({"at": store::now(), "from_state": state, "note": rental::FORCE_NOTE});
                    }
                    worker_event(&mut w, "teardown", &state, "terminated", r)
                }
                Err(e) => {
                    worker_event(
                        &mut w,
                        "teardown",
                        &state,
                        "error",
                        json!({"code": e.code, "message": e.message}),
                    );
                    workers[idx] = w;
                    save_workers(ctx, &workers)?;
                    return Err(e);
                }
            }
        }
        "status" => {}
        other => {
            return Err(GenError::new(
                "invalid_input",
                format!("unknown worker action: {other}"),
            ))
        }
    }
    workers[idx] = w.clone();
    save_workers(ctx, &workers)?;
    Ok(w)
}
