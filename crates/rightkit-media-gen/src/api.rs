//! Command surface shared by the engine CLI and the app sidecar. Every
//! function takes JSON input and returns JSON so both transports stay
//! identical. Contract: see `docs/ENGINE-CLI.md`.

use serde_json::{json, Value};
use std::io::Write as _;

use crate::providers::{self, GenRequest, Provider};
use crate::routing::{self, EngineRoute};
use crate::{credentials, fal_storage, jobs, ledger, library, store, Context, GenError, GenResult};
use std::time::Duration;

type Providers<'a> = &'a [Box<dyn Provider>];

fn req_str<'a>(input: &'a Value, field: &str) -> GenResult<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GenError::new("invalid_input", format!("missing field: {field}")))
}

fn find_provider<'a>(providers: Providers<'a>, id: &str) -> GenResult<&'a dyn Provider> {
    providers
        .iter()
        .find(|p| p.id() == id)
        .map(|b| b.as_ref())
        .ok_or_else(|| GenError::new("unknown_provider", format!("unknown provider: {id}")))
}

/// `credentials.status` → `{providers:[{id, configured}]}` (live keychain check).
pub fn credentials_status(ctx: &Context, input: &Value) -> GenResult<Value> {
    credentials_status_with(ctx, &providers::registry(), input)
}

pub fn credentials_status_with(
    ctx: &Context,
    providers: Providers,
    _input: &Value,
) -> GenResult<Value> {
    let list: Vec<Value> = providers
        .iter()
        .map(|p| {
            let needs_key = p.needs_key();
            let source = if needs_key {
                credentials::status_source(ctx, p.id())
            } else {
                None
            };
            let runtime = p.runtime_ready().map(|r| match r {
                Ok(()) => json!({"ready": true, "external": p.runtime_is_external()}),
                Err(e) => json!({
                    "ready": false,
                    "external": p.runtime_is_external(),
                    "code": e.code,
                    "message": e.message,
                }),
            });
            json!({
                "id": p.id(),
                "needs_key": needs_key,
                "configured": source.is_some() || !needs_key,
                "source": source.or(if needs_key { None } else { Some("local") }),
                "runtime": runtime,
            })
        })
        .collect();
    Ok(json!({"providers": list}))
}

/// `credentials.import_env` `{overwrite?}` → `{imported, skipped_present, missing, source}` (names only).
pub fn credentials_import_env(ctx: &Context, input: &Value) -> GenResult<Value> {
    let overwrite = input
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    credentials::import_env(ctx, overwrite)
}

/// `credentials.set` `{provider, api_key}` → `{ok:true}`; empty key deletes.
pub fn credentials_set(ctx: &Context, input: &Value) -> GenResult<Value> {
    credentials_set_with(ctx, &providers::registry(), input)
}

pub fn credentials_set_with(
    ctx: &Context,
    providers: Providers,
    input: &Value,
) -> GenResult<Value> {
    let provider = req_str(input, "provider")?;
    let p = find_provider(providers, provider)?;
    if !p.needs_key() {
        return Err(GenError::new(
            "no_key_needed",
            format!("{provider} runs locally and takes no API key"),
        ));
    }
    let key = input.get("api_key").and_then(Value::as_str).unwrap_or("");
    credentials::set(ctx, provider, key)?;
    Ok(json!({"ok": true}))
}

/// `credentials.test` `{provider}` → `{ok, message}` via the provider's free test call.
pub fn credentials_test(ctx: &Context, input: &Value) -> GenResult<Value> {
    credentials_test_with(ctx, &providers::registry(), input)
}

pub fn credentials_test_with(
    ctx: &Context,
    providers: Providers,
    input: &Value,
) -> GenResult<Value> {
    let provider = req_str(input, "provider")?;
    let p = find_provider(providers, provider)?;
    let key = if p.needs_key() {
        let Some(key) = credentials::get(ctx, provider)? else {
            return Ok(json!({"ok": false, "message": "no API key stored"}));
        };
        key
    } else {
        String::new()
    };
    Ok(match p.test_key(&key) {
        Ok(()) => json!({"ok": true, "message": "key accepted"}),
        Err(e) => json!({"ok": false, "message": e.message}),
    })
}

/// `models` → list of [`crate::providers::ModelInfo`].
pub fn models(ctx: &Context, input: &Value) -> GenResult<Value> {
    models_with(ctx, &providers::registry(), input)
}

pub fn models_with(ctx: &Context, providers: Providers, _input: &Value) -> GenResult<Value> {
    Ok(Value::Array(crate::capabilities::model_rows(
        ctx, providers,
    )))
}

/// `generate` `{kind, provider, model, prompt, params?, request_id?}` →
/// `{job_id, status, est_usd, request_id, deduplicated}`. `dry_run` validates
/// and estimates only. The same `request_id` always maps to the same job.
///
/// NanoBanana reads only https URLs. A local file in `params.image_urls` is
/// uploaded to the user's own fal storage (`fal_storage`) before the job is
/// created, and only when this call carries `confirm_fal_upload: true` (the JSON
/// boolean, per run, never stored). Without it the call fails with
/// `upload_confirmation_required` before any network call or spend reservation;
/// without a fal key it fails with `no_key`. A dry run neither uploads nor needs
/// the confirmation: it reports `requires_fal_upload_confirmation` and the paths.
pub fn generate(ctx: &Context, input: &Value, dry_run: bool) -> GenResult<Value> {
    generate_with(ctx, &providers::registry(), input, dry_run)
}

/// Top-level keys `generate` accepts: the request fields, plus the engine controls
/// the CLI and sidecar add or strip (`request_id`, `dry_run`, `wait`, `wait_timeout_s`),
/// the per-run upload consent `confirm_fal_upload` (never stored in the job), and the optional
/// scope ids `venture`, `channel`, `account` (all three together; stored on the job as `scope`).
/// `route` is not here: only the engine writes it (see `routing::EngineRoute`), and a caller's
/// `route` is refused before this check.
pub const GENERATE_KEYS: &[&str] = &[
    "kind",
    "provider",
    "model",
    "prompt",
    "params",
    "request_id",
    "dry_run",
    "wait",
    "wait_timeout_s",
    "confirm_fal_upload",
    "venture",
    "channel",
    "account",
    "caller_budget",
];

/// Refuse a top-level key outside [`GENERATE_KEYS`] by name instead of dropping it.
fn reject_unknown_keys(input: &Value) -> GenResult<()> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !GENERATE_KEYS.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(GenError::new(
        "invalid_request",
        format!(
            "unknown generate key(s): {}; accepted keys: {}",
            unknown.join(", "),
            GENERATE_KEYS.join(", ")
        ),
    ))
}

/// Every generate entry (CLI, sidecar, studio callers, pipeline steps, brief runs) reaches this
/// function through [`generate_with`] or [`generate_routed_with`], so the caller-key refusal below
/// is the one place a caller's `route` is turned away.
pub fn generate_with(
    ctx: &Context,
    providers: Providers,
    input: &Value,
    dry_run: bool,
) -> GenResult<Value> {
    generate_routed_with(ctx, providers, input, dry_run, None)
}

/// [`generate_with`] carrying the engine's own routing record, which only `routing::route_generate`
/// creates. The record is never read from `input`: a `route` key in `input` is refused.
pub fn generate_routed_with(
    ctx: &Context,
    providers: Providers,
    input: &Value,
    dry_run: bool,
    route: Option<&EngineRoute>,
) -> GenResult<Value> {
    routing::refuse_caller_route(input)?;
    reject_unknown_keys(input)?;
    let scope = crate::host::asset_facts::Scope::from_request(input)
        .map_err(|m| GenError::new("invalid_scope", m))?;
    // A budget Pulse issued to the caller: `{project, remaining_usd}`. Validated before anything is
    // created (a non-finite or negative remaining is refused), stored with the job so every runner
    // enforces it, and never a way to spend more than the caps allow.
    let budget = ledger::CallerBudget::from_request(input)?;
    let request_id = input
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut body = input.clone();
    if let Some(o) = body.as_object_mut() {
        o.remove("caller_budget");
        o.remove("request_id");
        o.remove("dry_run");
        o.remove("wait");
        o.remove("wait_timeout_s");
        o.remove(fal_storage::CONFIRM_FIELD);
        for k in ["venture", "channel", "account"] {
            o.remove(k);
        }
    }
    let route = route.map(|r| r.value().clone());
    let upload_confirmed = fal_storage::confirmed(input);
    let mut req: GenRequest = serde_json::from_value(body)
        .map_err(|e| GenError::new("invalid_input", format!("bad generate request: {e}")))?;
    // A prompt is required unless the request names audio instead (HeyGen and WaveSpeed avatar
    // families take `params.audio_url` in place of a script). Those providers then judge the
    // request themselves, so an audio-only avatar gets its own refusal, never this generic one.
    let names_audio = req
        .params
        .get("audio_url")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.trim().is_empty());
    if req.prompt.trim().is_empty() && !names_audio {
        return Err(GenError::new("invalid_input", "prompt must not be empty"));
    }
    let provider = find_provider(providers, &req.provider)?;
    if !provider
        .models()
        .iter()
        .any(|m| m.model == req.model && m.kind == req.kind)
    {
        return Err(GenError::new(
            "unknown_model",
            format!("{} has no {:?} model {}", req.provider, req.kind, req.model),
        ));
    }
    // Local NanoBanana references and local files in catalogue-declared url params are priced as if
    // uploaded; the upload itself happens below, after consent, the fal key and the spend check.
    let local_refs = if req.provider == "nanobanana" {
        fal_storage::local_image_refs(&req.params)
    } else {
        Vec::new()
    };
    let param_refs =
        if req.provider != "nanobanana" && !providers::is_builtin(&req.provider, &req.model) {
            crate::catalogue::Catalogue::get()
                .find(&req.provider, &req.model)
                .map(|e| fal_storage::local_param_refs(&req.params, &e.url_params()))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
    let priced = if !local_refs.is_empty() {
        let mut copy = req.clone();
        copy.params = fal_storage::priced_params(&req.params, &local_refs);
        Some(copy)
    } else if !param_refs.is_empty() {
        let mut copy = req.clone();
        copy.params = fal_storage::priced_param_refs(&req.params, &param_refs);
        Some(copy)
    } else {
        None
    };
    let est = provider
        .estimate_usd(priced.as_ref().unwrap_or(&req))
        .map_err(|e| GenError::new(e.code, e.message))?;
    // The ledger counts micro-USD: the estimate is rounded up to one here, so the figure a dry run
    // reports, the job stores and the reservation holds are the same number.
    let est = ledger::quantize_usd(est)?;
    // Before a dry run reports and before any reservation: the estimate must fit the caller's budget.
    if let Some(b) = &budget {
        ledger::check_budget(b, est)?;
    }

    if dry_run {
        let mut out = json!({
            "dry_run": true,
            "est_usd": est,
            "would_submit": {
                "kind": req.kind, "provider": req.provider, "model": req.model,
                "prompt": req.prompt, "params": req.params, "request_id": request_id,
                "scope": scope.as_ref().map(|s| s.to_value()),
            },
        });
        if let Some(r) = &route {
            out["route"] = r.clone();
        }
        if !local_refs.is_empty() || !param_refs.is_empty() {
            out["requires_fal_upload_confirmation"] = json!(true);
            out["local_references"] = json!(local_refs
                .iter()
                .map(|r| r.path.to_string_lossy().into_owned())
                .chain(
                    param_refs
                        .iter()
                        .map(|r| r.path.to_string_lossy().into_owned())
                )
                .collect::<Vec<_>>());
        }
        return Ok(out);
    }

    // Consent and the user's own fal key come before any network call, key lookup
    // for the provider, or ledger access.
    fal_storage::require_confirmation(&local_refs, upload_confirmed)?;
    fal_storage::require_param_confirmation(&param_refs, upload_confirmed, &req.provider)?;
    if !local_refs.is_empty() || !param_refs.is_empty() {
        fal_storage::fal_key(ctx)?;
    }

    if provider.needs_key() {
        crate::entitlement::require(crate::entitlement::CLOUD_PROVIDERS)?;
    }
    if provider.needs_key() && credentials::get(ctx, &req.provider)?.is_none() {
        return Err(GenError::new(
            "no_credentials",
            format!("no API key stored for {}", req.provider),
        ));
    }

    let db = store::open(ctx)?;
    // Advisory early refusal; the authoritative reservation happens per attempt.
    if let Some(rid) = request_id {
        if store::get_job_by_request(&db, rid).is_err() {
            ledger::check(&db, est)?;
        }
    } else {
        ledger::check(&db, est)?;
    }
    // Confirmed, keyed and within the cap: only now does the image leave this machine.
    if !local_refs.is_empty() {
        fal_storage::upload_refs(ctx, &mut req.params, &local_refs)?;
    }
    if !param_refs.is_empty() {
        fal_storage::upload_param_refs(ctx, &mut req.params, &param_refs)?;
    }
    // The scope rides in the stored request beside the GenRequest fields (providers ignore it);
    // the job record reads it back as `scope`.
    let mut request_value =
        serde_json::to_value(&req).map_err(|e| GenError::new("invalid_input", e.to_string()))?;
    if let (Some(o), Some(s)) = (request_value.as_object_mut(), scope.as_ref()) {
        o.insert("scope".into(), s.to_value());
    }
    if let (Some(o), Some(r)) = (request_value.as_object_mut(), route.as_ref()) {
        o.insert("route".into(), r.clone());
    }
    if let (Some(o), Some(b)) = (request_value.as_object_mut(), budget.as_ref()) {
        o.insert("caller_budget".into(), b.to_value());
    }
    let request_json = request_value.to_string();
    let kind = serde_json::to_value(req.kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let (job, dedup) = store::insert_or_existing(
        &db,
        &store::NewJob {
            request_id,
            kind: &kind,
            provider: &req.provider,
            model: &req.model,
            request_json: &request_json,
            est_usd: est,
        },
    )?;
    // A request id never hands one scope's job to another scope.
    if dedup
        && store::job_scope(&job) != scope.as_ref().map(|s| s.to_value()).unwrap_or(Value::Null)
    {
        return Err(GenError::new(
            "scope_mismatch",
            format!(
                "request_id {} already belongs to a job with a different scope",
                request_id.unwrap_or_default()
            ),
        ));
    }
    Ok(json!({
        "job_id": job.job_id,
        "status": job.status,
        "est_usd": job.est_usd,
        "request_id": job.request_id,
        "deduplicated": dedup,
        "scope": scope.as_ref().map(|s| s.to_value()),
        "route": route,
    }))
}

/// `job.run` `{job_id}`: drive one job to a terminal state while holding its lock.
pub fn job_run(ctx: &Context, input: &Value) -> GenResult<Value> {
    jobs::run_job(ctx, &providers::registry(), req_str(input, "job_id")?)
}

pub fn job_run_with(ctx: &Context, providers: Providers, input: &Value) -> GenResult<Value> {
    jobs::run_job(ctx, providers, req_str(input, "job_id")?)
}

/// `generate … --wait`: the input asked to run the job in this process.
pub fn wants_wait(input: &Value) -> bool {
    input.get("wait").and_then(Value::as_bool).unwrap_or(false)
}

/// `generate` with `wait:true`: create (or find) the job, drive it in this
/// process while streaming NDJSON progress to stderr, and return the final
/// job record (plus `result` when complete). No detached runner is involved.
pub fn generate_wait_with(
    ctx: &Context,
    providers: Providers,
    input: &Value,
    opts: &jobs::RunOptions,
) -> GenResult<Value> {
    generate_wait_routed_with(ctx, providers, input, opts, None)
}

/// [`generate_wait_with`] carrying the engine's routing record (see [`generate_routed_with`]).
pub fn generate_wait_routed_with(
    ctx: &Context,
    providers: Providers,
    input: &Value,
    opts: &jobs::RunOptions,
    route: Option<&EngineRoute>,
) -> GenResult<Value> {
    // Validate before creating anything.
    let timeout = timeout_of(input, &["wait_timeout_s"])?;
    let made = generate_routed_with(ctx, providers, input, false, route)?;
    let job_id = made["job_id"].as_str().unwrap_or_default().to_string();
    let started = std::time::Instant::now();
    let mut run_opts = opts.clone();
    if let Some(t) = timeout {
        run_opts.total = run_opts.total.min(t);
    }
    let record = jobs::run_job_with(ctx, providers, &job_id, &run_opts)?;
    if record["status"] == "locked_elsewhere" {
        // Another process (a detached runner) owns it: follow its events. A runner that pauses
        // exits, and nothing drives the job after that, so the wait ends on the pause exactly as
        // an in-process run would return its recoverable record (it used to wait out the limit).
        follow_job(
            ctx,
            &job_id,
            Some(timeout.unwrap_or(opts.total)),
            opts,
            true,
        )?;
    }
    let mut v = final_record(ctx, &job_id)?;
    if let Some(t) = timeout {
        v["timed_out"] =
            json!(!is_terminal(&v) && !awaits_resolution(&v) && started.elapsed() >= t);
    }
    Ok(v)
}

fn is_terminal(record: &Value) -> bool {
    matches!(
        record["status"].as_str(),
        Some("complete" | "failed" | "cancelled")
    )
}

/// An `outcome_unknown` job changes only when the user resolves it, so waiting
/// for it ends at once; it is not terminal and never reported as timed out.
fn awaits_resolution(record: &Value) -> bool {
    record["status"] == store::OUTCOME_UNKNOWN
}

/// Optional non-negative seconds (fractions allowed) from the first present key.
fn timeout_of(input: &Value, keys: &[&str]) -> GenResult<Option<Duration>> {
    for key in keys {
        let Some(v) = input.get(*key).filter(|v| !v.is_null()) else {
            continue;
        };
        let secs = v.as_f64().ok_or_else(|| {
            GenError::new(
                "invalid_input",
                format!("{key} must be a number of seconds"),
            )
        })?;
        return Duration::try_from_secs_f64(secs).map(Some).map_err(|_| {
            GenError::new(
                "invalid_input",
                format!("{key} must be a non-negative number of seconds"),
            )
        });
    }
    Ok(None)
}

/// `job.wait` `{job_id, timeout_s?}`: block until the job is terminal or the
/// timeout passes, then return the job record (plus `result` when complete)
/// with `timed_out`. Never takes the job lock and never stops the job: it
/// follows whichever process drives the job through the store, echoing new
/// progress events to stderr. Without a timeout it waits for a terminal state.
/// A job in `outcome_unknown` ends the wait at once (`timed_out:false`, status
/// `outcome_unknown`): only `job.resolve` moves it, so waiting would never end.
pub fn job_wait(ctx: &Context, input: &Value) -> GenResult<Value> {
    job_wait_with(ctx, input, &jobs::RunOptions::default())
}

pub fn job_wait_with(ctx: &Context, input: &Value, opts: &jobs::RunOptions) -> GenResult<Value> {
    wait_for_job(ctx, input, opts, false)
}

/// Generation callers may return a recoverable record when the detached runner pauses.
/// Ordinary `job.wait` keeps following until terminal or timeout.
pub fn job_wait_for_runner(ctx: &Context, input: &Value) -> GenResult<Value> {
    wait_for_job(ctx, input, &jobs::RunOptions::default(), true)
}

fn wait_for_job(
    ctx: &Context,
    input: &Value,
    opts: &jobs::RunOptions,
    stop_on_pause: bool,
) -> GenResult<Value> {
    let job_id = req_str(input, "job_id")?;
    let timeout = timeout_of(input, &["timeout_s", "wait_timeout_s"])?;
    store::get_job(&store::open(ctx)?, job_id)?; // job_not_found
    let paused = follow_job(ctx, job_id, timeout, opts, stop_on_pause)?;
    let mut v = final_record(ctx, job_id)?;
    v["timed_out"] = json!(!is_terminal(&v) && !awaits_resolution(&v) && !paused);
    if paused {
        v["recoverable"] = json!(true);
    }
    Ok(v)
}

/// Poll the store until the job is terminal, awaits resolution or `limit`
/// passes (`None` waits without bound), echoing new events to stderr.
/// Read-only.
fn follow_job(
    ctx: &Context,
    job_id: &str,
    limit: Option<Duration>,
    opts: &jobs::RunOptions,
    stop_on_pause: bool,
) -> GenResult<bool> {
    let db = store::open(ctx)?;
    let started = std::time::Instant::now();
    let mut seen = 0i64;
    let mut paused = false;
    loop {
        for (seq, ev) in store::events_after(&db, job_id, seen)? {
            seen = seq;
            paused = ev["message"]
                .as_str()
                .is_some_and(|s| s.starts_with("paused, will resume:"));
            if opts.stderr_events {
                let _ = writeln!(std::io::stderr(), "{ev}");
            }
        }
        let job = store::get_job(&db, job_id)?;
        if job.is_terminal() || job.awaits_resolution() {
            return Ok(false);
        }
        if stop_on_pause && paused {
            return Ok(true);
        }
        let step = opts.poll_initial.min(Duration::from_millis(500));
        match limit {
            Some(l) => {
                let left = l.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Ok(false);
                }
                std::thread::sleep(step.min(left));
            }
            None => std::thread::sleep(step),
        }
    }
}

fn final_record(ctx: &Context, job_id: &str) -> GenResult<Value> {
    let db = store::open(ctx)?;
    let job = store::get_job(&db, job_id)?;
    let mut v = with_progress(&db, &job, false)?;
    if let (true, Some(r)) = (job.status == "complete", &job.result_json) {
        v["result"] = result_with_ack(&job, r)?;
    }
    Ok(v)
}

fn with_progress(db: &store::Db, job: &store::Job, events: bool) -> GenResult<Value> {
    let mut v = job.to_value();
    if let Some(spend) = spend_for(db, job)? {
        v["spend"] = spend;
    }
    v["progress"] = store::latest_event(db, &job.job_id)?;
    if events {
        v["events"] = Value::Array(store::events(db, &job.job_id)?);
    }
    Ok(v)
}

/// `{project, reserved_usd, actual_usd, run_reserved_usd, run_actual_usd}` when the job carries a
/// caller budget (stored with it) or this process runs under one; `None` otherwise.
fn spend_for(db: &store::Db, job: &store::Job) -> GenResult<Option<Value>> {
    let budget = ledger::CallerBudget::from_request_json(&job.request_json)?;
    ledger::spend_report(db, &job.job_id, budget.as_ref())
}

/// The caller-budget `spend` report of a job (`job run` adds it to the run's result).
pub fn job_spend(ctx: &Context, job_id: &str) -> GenResult<Option<Value>> {
    let db = store::open(ctx)?;
    let job = store::get_job(&db, job_id)?;
    spend_for(&db, &job)
}

fn result_with_ack(job: &store::Job, raw: &str) -> GenResult<Value> {
    let mut d: Value = serde_json::from_str(raw)
        .map_err(|e| GenError::new("db", format!("stored result unreadable: {e}")))?;
    d["acked_by"] = json!(job.acked_by);
    d["acked_at"] = json!(job.acked_at);
    // Results written before asset facts existed read as explicit `unknown`, never a guess.
    if let Some(assets) = d.get_mut("assets").and_then(Value::as_array_mut) {
        for a in assets {
            if a.get("facts").is_none_or(Value::is_null) {
                a["facts"] = crate::host::asset_facts::AssetFacts::unknown().to_value();
            }
            if a.get("scope").is_none() {
                a["scope"] = Value::Null;
            }
        }
    }
    Ok(d)
}

/// `job.status` `{job_id | request_id, events?}` → job record (live) with
/// `progress` (latest event) and, when `events:true`, the stored `events`.
pub fn job_status(ctx: &Context, input: &Value) -> GenResult<Value> {
    let db = store::open(ctx)?;
    let job = match (opt_str(input, "job_id"), opt_str(input, "request_id")) {
        (Some(id), _) => store::get_job(&db, id)?,
        (None, Some(rid)) => store::get_job_by_request(&db, rid)?,
        (None, None) => return Err(GenError::new("invalid_input", "missing field: job_id")),
    };
    let events = input
        .get("events")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut value = with_progress(&db, &job, events)?;
    if job.provider == providers::local_image::ID {
        value["backend"] =
            providers::local_image::job_backend_status(&job.job_id, job.provider_job_id.as_deref())
                .unwrap_or(Value::Null);
    }
    Ok(value)
}

fn opt_str<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// `job.result` `{job_id}` → result descriptor (§4.4) or `not_ready`.
pub fn job_result(ctx: &Context, input: &Value) -> GenResult<Value> {
    let db = store::open(ctx)?;
    let job = store::get_job(&db, req_str(input, "job_id")?)?;
    match (job.status.as_str(), &job.result_json) {
        ("complete", Some(r)) => result_with_ack(&job, r).map(|mut v| {
            relocate_asset_paths(ctx, &mut v);
            v
        }),
        ("complete", None) => Err(GenError::new(
            "result_cleaned",
            "result was cleaned up after acknowledgement",
        )),
        ("failed", _) | ("cancelled", _) => Err(GenError::new(
            "job_not_successful",
            format!("job is {}", job.status),
        )),
        _ => Err(GenError::new("not_ready", format!("job is {}", job.status))),
    }
}

/// Results keep the absolute path recorded when they were written. If the
/// library has since moved (e.g. the 2026-10-06 identifier rename), point each
/// missing path at the same file under the current library folder.
fn relocate_asset_paths(ctx: &Context, result: &mut Value) {
    let Some(assets) = result.get_mut("assets").and_then(Value::as_array_mut) else {
        return;
    };
    for asset in assets {
        let (Some(id), Some(path)) = (
            asset
                .get("asset_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            asset
                .get("path")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from),
        ) else {
            continue;
        };
        if path.exists() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let moved = ctx.library_dir().join(&id).join(name);
        if moved.exists() {
            asset["path"] = Value::String(moved.to_string_lossy().into_owned());
        }
    }
}

/// `asset.resolve` `{asset_id}` → `{asset_id, job_id, path, sha256, bytes,
/// mime, exists, facts, scope}` (`facts` is origin, evidence and rights; `unknown` for old rows). Resolves through the current library folder first, so a
/// moved library still resolves; a file found away from its recorded path is
/// verified against the recorded sha256.
pub fn asset_resolve(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = req_str(input, "asset_id")?;
    let db = store::open(ctx)?;
    let Some(a) = store::get_asset(&db, id)? else {
        return Err(GenError::new(
            "asset_not_found",
            format!("no such asset: {id}"),
        ));
    };
    let stored = std::path::PathBuf::from(&a.path);
    let name = stored.file_name().map(|n| n.to_owned()).unwrap_or_default();
    let current = ctx.library_dir().join(&a.asset_id).join(&name);
    let (path, moved) = if current.is_file() {
        (current.clone(), current != stored)
    } else if stored.is_file() {
        (stored.clone(), false)
    } else {
        (current.clone(), false)
    };
    let exists = path.is_file();
    if exists && moved {
        let data = std::fs::read(&path).map_err(store::io_err)?;
        if library::sha256_hex(&data) != a.sha256 {
            return Err(GenError::new(
                "asset_corrupt",
                format!("{} does not match its recorded sha256", path.display()),
            ));
        }
    }
    let (facts, scope) = store::asset_facts(&db, &a.asset_id)?;
    Ok(json!({
        "asset_id": a.asset_id,
        "job_id": a.job_id,
        "path": path.to_string_lossy(),
        "sha256": a.sha256,
        "bytes": a.bytes,
        "mime": a.mime,
        "exists": exists,
        "facts": facts.to_value(),
        "scope": scope,
    }))
}

/// `job.list` `{request_id?, status?, limit?}` → `{jobs, truncated}`; default
/// limit 50, maximum 500.
pub fn job_list(ctx: &Context, input: &Value) -> GenResult<Value> {
    let limit = input
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(50)
        .clamp(1, 500);
    let db = store::open(ctx)?;
    let (rows, truncated) = store::list_jobs(
        &db,
        limit,
        opt_str(input, "request_id"),
        opt_str(input, "status"),
    )?;
    let jobs: Vec<Value> = rows.iter().map(|j| j.to_value()).collect();
    Ok(json!({"jobs": jobs, "truncated": truncated}))
}

/// `job.ack` `{job_id, consumer}`: the caller durably imported the result; it
/// may now be cleaned up (results are retained until acknowledged).
pub fn job_ack(ctx: &Context, input: &Value) -> GenResult<Value> {
    let consumer = req_str(input, "consumer")?;
    let db = store::open(ctx)?;
    Ok(store::ack(&db, req_str(input, "job_id")?, consumer)?.to_value())
}

/// `job.cancel` `{job_id}`. A cancelled job's open reservations are released, with one
/// exception: a PAID (keyed) provider's job that the provider already ACCEPTED (a provider job
/// id exists) is released only when the adapter really cancels (`Provider::can_cancel`) and the
/// provider confirms it. Otherwise nothing is released: the job is held as `outcome_unknown`
/// ("cancel requested; the provider may still complete and charge") with its reservation kept,
/// and `job resolve --abandon` settles it. Local engines and jobs not yet accepted keep the plain
/// cancel. An `outcome_unknown` job is refused: cancelling
/// would drop its held reservation unrecorded; resolve it instead.
pub fn job_cancel(ctx: &Context, input: &Value) -> GenResult<Value> {
    job_cancel_with(ctx, &providers::registry(), input)
}

pub fn job_cancel_with(ctx: &Context, providers: Providers, input: &Value) -> GenResult<Value> {
    let mut db = store::open(ctx)?;
    let id = req_str(input, "job_id")?;
    let held = store::get_job(&db, id)?;
    if held.awaits_adoption() {
        return Err(GenError::new(
            jobs::RESULT_RECOVERED,
            format!("job {id} already has its paid result saved for recovery; resolve it with `job resolve {id} --adopt` or `--abandon`"),
        ));
    }
    if held.awaits_resolution() {
        return Err(GenError::new(
            "outcome_unknown",
            format!("job {id} may be running at the provider; resolve it with `job resolve {id} --resubmit` or `--abandon`"),
        ));
    }
    if !held.is_terminal() && held.provider_job_id.is_some() {
        if let Ok(p) = find_provider(providers, &held.provider) {
            if p.needs_key() {
                return cancel_accepted_paid(&mut db, p, &held);
            }
        }
    }
    let job = store::cancel(&db, id)?;
    if job.status == "cancelled" {
        ledger::release_unreconciled(&mut db, id)?;
        let req: GenRequest = serde_json::from_str(&job.request_json)
            .map_err(|e| GenError::new("bad_request", e.to_string()))?;
        find_provider(providers, &job.provider)?
            .cancel_job(&req, job.provider_job_id.as_deref(), id)
            .map_err(|e| GenError::new(e.code, e.message))?;
    }
    Ok(job.to_value())
}

/// Cancel a paid job the provider already accepted. Release only on a provider-confirmed
/// cancel; otherwise hold the job with its reservation and say so.
fn cancel_accepted_paid(
    db: &mut store::Db,
    p: &dyn Provider,
    job: &store::Job,
) -> GenResult<Value> {
    let id = job.job_id.as_str();
    let req: GenRequest = serde_json::from_str(&job.request_json)
        .map_err(|e| GenError::new("bad_request", e.to_string()))?;
    let why = if p.can_cancel() {
        match p.cancel_job(&req, job.provider_job_id.as_deref(), id) {
            Ok(()) => {
                let cancelled = store::cancel(db, id)?;
                if cancelled.status == "cancelled" {
                    ledger::release_unreconciled(db, id)?;
                }
                return Ok(cancelled.to_value());
            }
            Err(e) => format!(
                "{} did not confirm the cancel ({}: {})",
                job.provider, e.code, e.message
            ),
        }
    } else {
        format!("{} has no cancel endpoint the host can call", job.provider)
    };
    let note = format!(
        "cancel requested; {why}. The provider may still complete and charge, so attempt {} stays reserved. Check the provider, then `job resolve {id} --abandon` (counts it as spent) or `--resubmit` (nothing ran)",
        job.attempts
    );
    store::mark_outcome_unknown(
        db,
        id,
        &rightkit_job_queue::JobError::new("outcome_unknown", note.clone()),
    )?;
    let mut v = store::get_job(db, id)?.to_value();
    v["cancel"] = json!({"applied": false, "provider_cancelled": false, "note": note});
    Ok(v)
}

/// `job.resolve` `{job_id, action: "resubmit" | "abandon" | "adopt"}`: the user's decision
/// for an `outcome_unknown` job (see [`jobs::resolve_job`]).
pub fn job_resolve(ctx: &Context, input: &Value) -> GenResult<Value> {
    jobs::resolve_job(ctx, req_str(input, "job_id")?, req_str(input, "action")?)
}

/// `spend.status` → `{day, cap_usd, reserved_usd, actual_usd, month, monthly_cap_usd | null,
/// month_reserved_usd, month_actual_usd, month_counted_usd, monthly_remaining_usd | null,
/// amounts: {day, month}}`; each period's `amounts` keeps estimated, reserved, provider-reported and
/// invoice-reconciled USD separate (see [`ledger::status`]); the caps count only the ledger's
/// reserved (while open) and actual amounts.
pub fn spend_status(ctx: &Context, _input: &Value) -> GenResult<Value> {
    ledger::status(&store::open(ctx)?)
}

/// `spend.set_cap` `{cap_usd?, monthly_cap_usd?}`: at least one. `cap_usd` is the daily cap;
/// `monthly_cap_usd` is a number (UTC calendar month cap) or `null` (clear it). An omitted field
/// is left as it is. Both are validated before either is saved.
pub fn spend_set_cap(ctx: &Context, input: &Value) -> GenResult<Value> {
    let daily = match input.get("cap_usd") {
        None => None,
        Some(v) => Some(v.as_f64().ok_or_else(|| {
            GenError::new("invalid_input", "cap_usd must be a non-negative number")
        })?),
    };
    let monthly = match input.get("monthly_cap_usd") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => Some(Some(v.as_f64().ok_or_else(|| {
            GenError::new(
                "invalid_input",
                "monthly_cap_usd must be a non-negative number or null",
            )
        })?)),
    };
    if daily.is_none() && monthly.is_none() {
        return Err(GenError::new(
            "invalid_input",
            "missing field: cap_usd or monthly_cap_usd",
        ));
    }
    let db = store::open(ctx)?;
    ledger::set_caps(&db, daily, monthly)?;
    ledger::status(&db)
}

/// `spend.allowance` -> this device's offline allowance: `{device_id, device_allowance_usd | null,
/// counted_usd, remaining_usd, set_at, shared_allowance: {available: false, reason}, audit}`.
/// Read-only apart from generating the device id on first use.
pub fn spend_allowance(ctx: &Context, _input: &Value) -> GenResult<Value> {
    ledger::allowance_status(&store::open(ctx)?)
}

/// `spend.set_allowance` `{device_allowance_usd: number | null, note?}`: set or clear this device's
/// allowance and restart its counted baseline. The field must be present (`null` clears it).
/// Records an audit row (who is unknown: "local user"). Returns the new `spend.allowance` view.
pub fn spend_set_allowance(ctx: &Context, input: &Value) -> GenResult<Value> {
    let allowance = match input.get("device_allowance_usd") {
        None => {
            return Err(GenError::new(
                "invalid_input",
                "missing field: device_allowance_usd (a number, or null to clear)",
            ))
        }
        Some(Value::Null) => None,
        Some(v) => Some(v.as_f64().ok_or_else(|| {
            GenError::new(
                "invalid_input",
                "device_allowance_usd must be a non-negative number or null",
            )
        })?),
    };
    let note = match input.get("note") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .ok_or_else(|| GenError::new("invalid_input", "note must be a string"))?,
        ),
    };
    ledger::set_allowance(&store::open(ctx)?, allowance, note)
}

/// `spend.reconcile` `{period: "YYYY-MM" | {from, to}, provider, invoice_total_usd, invoice_ref?,
/// lines?: [{job_id | provider_job_id, attempt?, usd}]}`: record the user's invoice beside the
/// engine's amounts (see [`ledger::reconcile_invoice`]). Never changes a cap or a reservation.
pub fn spend_reconcile(ctx: &Context, input: &Value) -> GenResult<Value> {
    ledger::reconcile_invoice(&store::open(ctx)?, input)
}

/// `spend.acknowledge_overrun` `{note?}`: record that a person saw every settle that came in above
/// its reservation (a rental that ran past its budget). RightKit's ledger holds further
/// reservations on an allowance while one is unacknowledged. Returns `{acknowledged}`.
pub fn spend_acknowledge_overrun(ctx: &Context, input: &Value) -> GenResult<Value> {
    let note = match input.get("note") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .ok_or_else(|| GenError::new("invalid_input", "note must be a string"))?,
        ),
    };
    ledger::acknowledge_overruns(&store::open(ctx)?, note)
}

/// Jobs that are not terminal, are not held as `outcome_unknown` and whose
/// lock is free; the sidecar and any engine process start call this, then
/// spawn `job.run` for each.
pub fn recoverable_jobs(ctx: &Context) -> GenResult<Vec<String>> {
    let db = store::open(ctx)?;
    let mut out = Vec::new();
    for id in store::runnable_ids(&db)? {
        // Holding the lock briefly proves nobody else is running it.
        if let Some(_l) = jobs::try_lock(ctx, &id)? {
            out.push(id);
        }
    }
    Ok(out)
}

/// Live readiness for `doctor`: database openable, credentials per provider.
pub fn readiness(ctx: &Context) -> Value {
    readiness_with(ctx, &providers::registry())
}

pub fn readiness_with(ctx: &Context, providers: Providers) -> Value {
    let list: Vec<Value> = providers
        .iter()
        .map(|p| {
            let runtime = p.runtime_ready().map(|r| match r {
                Ok(()) => json!({"ready": true, "external": p.runtime_is_external()}),
                Err(e) => {
                    json!({"ready": false, "external": p.runtime_is_external(), "code": e.code})
                }
            });
            json!({
                "id": p.id(),
                "needs_key": p.needs_key(),
                "configured": !p.needs_key() || credentials::status(ctx, p.id()),
                "runtime": runtime,
            })
        })
        .collect();
    json!({"db_ok": store::open(ctx).is_ok(), "providers": list})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::MemoryCredentials;
    use crate::jobs::{run_job_with, RunOptions};
    use crate::providers::{Kind, ModelInfo, Output, PollOutcome, ProviderError, SubmitOutcome};
    use crate::store::Job;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    static MODELS: [ModelInfo; 1] = [ModelInfo {
        provider: "fake",
        model: "fake-img",
        kind: Kind::Image,
        label: "Fake",
        est_usd: 1.0,
        durations: crate::providers::Durations::NONE,
    }];

    #[derive(Default)]
    struct State {
        submits: usize,
        polls: usize,
        poll_script: VecDeque<PollOutcome>,
        sync: bool,
        fail_submit: bool,
        /// Idempotency keys received by `submit_job`, in order.
        keys: Vec<String>,
        /// The next N submits fail possibly-delivered (a 503 after the send).
        ambiguous: usize,
        /// The next N submits fail before anything is sent (connect refused).
        unsent: usize,
        /// `submit_is_idempotent`.
        idempotent: bool,
        /// Poll fails with this error.
        poll_error: Option<ProviderError>,
    }

    struct Fake(Arc<Mutex<State>>);

    fn png() -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&7u32.to_be_bytes());
        b.extend_from_slice(&5u32.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    fn done() -> PollOutcome {
        PollOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes: png(),
                mime: "image/png".into(),
            }],
        }
    }

    impl Provider for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn models(&self) -> &'static [ModelInfo] {
            &MODELS
        }
        fn estimate_usd(&self, _r: &GenRequest) -> Result<f64, ProviderError> {
            Ok(1.0)
        }
        fn submit(&self, _r: &GenRequest, key: &str) -> Result<SubmitOutcome, ProviderError> {
            assert_eq!(key, "k");
            let mut s = self.0.lock().unwrap();
            s.submits += 1;
            if s.unsent > 0 {
                s.unsent -= 1;
                return Err(ProviderError::new("transport", "connection refused", true));
            }
            if s.ambiguous > 0 {
                s.ambiguous -= 1;
                return Err(ProviderError::new("unavailable", "fake 503", true).ambiguous());
            }
            if s.fail_submit {
                return Err(ProviderError::new("rejected", "bad", false));
            }
            if s.sync {
                return Ok(SubmitOutcome::Completed {
                    outputs: vec![Output::Bytes {
                        bytes: png(),
                        mime: "image/png".into(),
                    }],
                });
            }
            Ok(SubmitOutcome::Accepted {
                provider_job_id: format!("p{}", s.submits),
            })
        }
        fn submit_job(
            &self,
            r: &GenRequest,
            key: &str,
            _job: &str,
            _attempt: i64,
            idempotency_key: &str,
        ) -> Result<SubmitOutcome, ProviderError> {
            self.0
                .lock()
                .unwrap()
                .keys
                .push(idempotency_key.to_string());
            self.submit(r, key)
        }
        fn submit_is_idempotent(&self) -> bool {
            self.0.lock().unwrap().idempotent
        }
        fn poll(&self, _r: &GenRequest, _id: &str, _k: &str) -> Result<PollOutcome, ProviderError> {
            let mut s = self.0.lock().unwrap();
            s.polls += 1;
            if let Some(e) = s.poll_error.clone() {
                return Err(e);
            }
            Ok(s.poll_script.pop_front().unwrap_or_else(done))
        }
        fn test_key(&self, _k: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn fetch(&self, _o: &Output) -> Result<(Vec<u8>, String), ProviderError> {
            Err(ProviderError::new("nope", "no network", false))
        }
    }

    static KEYLESS_MODELS: [ModelInfo; 1] = [ModelInfo {
        provider: "fake-keyless",
        model: "fake-voice",
        kind: Kind::Voice,
        label: "Fake local voice",
        est_usd: 0.0,
        durations: crate::providers::Durations::NONE,
    }];

    /// Keyless local engine: submit receives an empty key and still works.
    struct FakeKeyless;

    impl Provider for FakeKeyless {
        fn id(&self) -> &'static str {
            "fake-keyless"
        }
        fn models(&self) -> &'static [ModelInfo] {
            &KEYLESS_MODELS
        }
        fn needs_key(&self) -> bool {
            false
        }
        fn estimate_usd(&self, _r: &GenRequest) -> Result<f64, ProviderError> {
            Ok(0.0)
        }
        fn submit(&self, _r: &GenRequest, key: &str) -> Result<SubmitOutcome, ProviderError> {
            assert_eq!(key, "");
            Ok(SubmitOutcome::Completed {
                outputs: vec![Output::Bytes {
                    bytes: b"RIFF".to_vec(),
                    mime: "audio/wav".into(),
                }],
            })
        }
        fn poll(&self, _r: &GenRequest, _id: &str, _k: &str) -> Result<PollOutcome, ProviderError> {
            Err(ProviderError::new("invalid_request", "synchronous", false))
        }
        fn test_key(&self, _k: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        fn fetch(&self, _o: &Output) -> Result<(Vec<u8>, String), ProviderError> {
            Err(ProviderError::new("nope", "no network", false))
        }
    }

    struct Env {
        ctx: Context,
        st: Arc<Mutex<State>>,
        providers: Vec<Box<dyn Provider>>,
        _dir: tempfile_dir::Dir,
    }

    // Minimal temp dir without extra deps.
    mod tempfile_dir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = crate::test_temp_dir()
                    .join(format!("rightkit-gen-test-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn env() -> Env {
        let dir = tempfile_dir::Dir::new();
        let creds = Arc::new(MemoryCredentials::new());
        crate::credentials::CredentialSource::set(&*creds, "fake", "k").unwrap();
        let ctx = Context::new(dir.0.clone()).with_credentials(creds);
        let st = Arc::new(Mutex::new(State::default()));
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(Fake(st.clone()))];
        Env {
            ctx,
            st,
            providers,
            _dir: dir,
        }
    }

    fn fast() -> RunOptions {
        RunOptions {
            poll_initial: Duration::from_millis(5),
            poll_max: Duration::from_millis(10),
            total: Duration::from_secs(10),
            stderr_events: false,
        }
    }

    fn gen_input(rid: Option<&str>) -> Value {
        let mut v = json!({"kind":"image","provider":"fake","model":"fake-img","prompt":"a cat"});
        if let Some(r) = rid {
            v["request_id"] = json!(r);
        }
        v
    }

    fn new_job(e: &Env, rid: Option<&str>) -> String {
        generate_with(&e.ctx, &e.providers, &gen_input(rid), false).unwrap()["job_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn attempts(e: &Env, id: &str) -> i64 {
        let c = store::open(&e.ctx).unwrap();
        ledger::attempt_count(&c, id).unwrap()
    }

    /// `(reserved, actual)` of one attempt's ledger row.
    fn ledger_row(e: &Env, id: &str, attempt: i64) -> (f64, Option<f64>) {
        ledger::attempt_row(&store::open(&e.ctx).unwrap(), id, attempt)
            .unwrap()
            .expect("the attempt has a ledger row")
    }

    /// A process that counted attempt 1 (reservation and pending submission
    /// committed by `begin_submission`) and died before recording the provider's
    /// reply. Returns the attempt's idempotency key.
    fn crash_after_reserve(e: &Env, id: &str) -> String {
        let db = store::open(&e.ctx).unwrap();
        match db.begin_submission(id, jobs::MAX_ATTEMPTS).unwrap() {
            Some(rightkit_job_queue::Submission::Submit {
                attempt: 1,
                idempotency_key,
                replay: false,
            }) => idempotency_key,
            other => panic!("unexpected first step: {other:?}"),
        }
    }

    #[test]
    fn async_happy_path_writes_descriptor() {
        let e = env();
        e.st.lock().unwrap().poll_script = VecDeque::from([PollOutcome::Running, done()]);
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        let d = job_result(&e.ctx, &json!({"job_id": id})).unwrap();
        assert!(d.get("path").is_none(), "no single-asset flattening");
        assert_eq!(d["kind"], "image");
        assert_eq!(d["provider"], "fake");
        assert_eq!(d["acked_by"], Value::Null);
        let asset = d["assets"][0].clone();
        assert_eq!(asset["provider"], "fake");
        assert_eq!(asset["model"], "fake-img");
        let path = asset["path"].as_str().unwrap();
        let data = std::fs::read(path).unwrap();
        assert_eq!(asset["sha256"], crate::library::sha256_hex(&data));
        assert_eq!(asset["bytes"], data.len());
        assert_eq!(asset["media"]["kind"], "image");
        assert_eq!(asset["media"]["width"], 7);
        assert_eq!(asset["media"]["height"], 5);
        assert_eq!(asset["media"]["duration_s"], Value::Null);
        assert_eq!(d["assets"].as_array().unwrap().len(), 1);
        assert!(path.contains(asset["asset_id"].as_str().unwrap()));
        assert!(!std::path::Path::new(path).with_extension("tmp").exists());
        let dir = std::path::Path::new(path).parent().unwrap();
        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names.iter().filter(|n| n.as_str() != "facts.json").count(),
            1,
            "no temp file left: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "facts.json"),
            "the asset's facts sit beside it: {names:?}"
        );
        let (_, actual) =
            ledger::day_totals(&store::open(&e.ctx).unwrap(), &store::today()).unwrap();
        assert_eq!(actual, 1.0);
    }

    #[test]
    fn sync_provider_completes_immediately() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        assert_eq!(e.st.lock().unwrap().polls, 0);
    }

    #[test]
    fn failing_submit_marks_failed_and_releases_spend() {
        let e = env();
        e.st.lock().unwrap().fail_submit = true;
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "failed");
        assert_eq!(r["error"]["code"], "rejected");
        let s = spend_status(&e.ctx, &json!({})).unwrap();
        assert_eq!(s["reserved_usd"], 0.0);
        assert_eq!(s["actual_usd"], 0.0);
    }

    #[test]
    fn recovery_after_provider_id_saved_polls_without_resubmit() {
        let e = env();
        let id = new_job(&e, None);
        {
            let mut c = store::open(&e.ctx).unwrap();
            ledger::reserve(&mut c, &id, 1, 1.0).unwrap();
            store::set_provider_id(&c, &id, "p-existing").unwrap();
        }
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        assert_eq!(e.st.lock().unwrap().submits, 0);
        assert_eq!(attempts(&e, &id), 1);
    }

    #[test]
    fn replay_of_an_interrupted_paid_submit_is_held_not_resent() {
        let e = env();
        let id = new_job(&e, None);
        // Crashed after reserving and submitting, before saving an id.
        let key = crash_after_reserve(&e, &id);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "outcome_unknown");
        assert_eq!(r["error"]["code"], "outcome_unknown");
        assert_eq!(r["attempts"], 1, "no new attempt was counted");
        assert_eq!(
            r["outcome_unknown"],
            json!({"attempt": 1, "reserved_usd": 1.0, "idempotency_key": key, "recovered": false, "resolve": ["resubmit", "abandon"]})
        );
        assert_eq!(
            e.st.lock().unwrap().submits,
            0,
            "the provider was never called"
        );
        assert_eq!(ledger_row(&e, &id, 1), (1.0, None), "reservation kept");
        assert_eq!(attempts(&e, &id), 1);
        assert_eq!(
            spend_status(&e.ctx, &json!({})).unwrap()["reserved_usd"],
            1.0
        );
        // The held job is reported by status and list with the same details.
        let st = job_status(&e.ctx, &json!({"job_id": id})).unwrap();
        assert_eq!(
            (
                st["status"].as_str(),
                st["outcome_unknown"]["idempotency_key"].as_str()
            ),
            (Some("outcome_unknown"), Some(key.as_str()))
        );
        let held = job_list(&e.ctx, &json!({"status": "outcome_unknown"})).unwrap();
        assert_eq!(held["jobs"][0]["job_id"], json!(id));
    }

    #[test]
    fn replay_of_an_interrupted_idempotent_submit_reuses_the_key_and_reservation() {
        let e = env();
        e.st.lock().unwrap().idempotent = true;
        let id = new_job(&e, None);
        let key = crash_after_reserve(&e, &id);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        let s = e.st.lock().unwrap();
        assert_eq!((s.submits, s.keys.clone()), (1, vec![key]));
        drop(s);
        assert_eq!(attempts(&e, &id), 1, "no second reservation");
        assert_eq!(r["attempts"], 1);
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(1.0)));
    }

    #[test]
    fn ambiguous_submit_error_holds_the_job_and_refuses_a_second_run() {
        let e = env();
        e.st.lock().unwrap().ambiguous = 1;
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "outcome_unknown");
        assert_eq!(
            (r["error"]["code"].as_str(), r["error"]["message"].as_str()),
            (Some("unavailable"), Some("fake 503"))
        );
        assert_eq!(
            r["outcome_unknown"]["idempotency_key"],
            json!(format!("{id}:1"))
        );
        assert_eq!(e.st.lock().unwrap().submits, 1);
        assert_eq!(ledger_row(&e, &id, 1), (1.0, None), "reservation kept");
        // A second run refuses and sends nothing.
        let err = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap_err();
        assert_eq!(err.code, "outcome_unknown");
        assert!(err.message.contains("job resolve"), "{}", err.message);
        assert_eq!(e.st.lock().unwrap().submits, 1);
        // Waiting returns at once rather than blocking on a state only the user can change.
        let t = Instant::now();
        let w = job_wait_with(&e.ctx, &json!({"job_id": id}), &fast()).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(
            (w["status"].as_str(), w["timed_out"].clone()),
            (Some("outcome_unknown"), json!(false))
        );
        // Cancelling would drop the held reservation: refused too.
        assert_eq!(
            job_cancel_with(&e.ctx, &e.providers, &json!({"job_id": id}))
                .unwrap_err()
                .code,
            "outcome_unknown"
        );
        assert_eq!(
            job_status(&e.ctx, &json!({"job_id": id})).unwrap()["status"],
            "outcome_unknown"
        );
    }

    #[test]
    fn ambiguous_error_from_an_idempotent_provider_still_replays() {
        let e = env();
        {
            let mut s = e.st.lock().unwrap();
            s.idempotent = true;
            s.ambiguous = 1;
        }
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(
            (r["status"].as_str(), r["recoverable"].clone()),
            (Some("submitted"), json!(true))
        );
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        let s = e.st.lock().unwrap();
        assert_eq!((s.submits, s.keys.len()), (2, 2));
        assert_eq!(s.keys[0], s.keys[1], "the same key is sent on the replay");
        drop(s);
        assert_eq!(attempts(&e, &id), 1);
    }

    #[test]
    fn connect_failure_does_not_consume_an_attempt_or_double_reserve() {
        let e = env();
        e.st.lock().unwrap().unsent = 2;
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        assert_eq!(
            r["attempts"], 1,
            "a send that never left is not a paid attempt"
        );
        let s = e.st.lock().unwrap();
        assert_eq!(s.submits, 3, "two refused connects, then the send");
        assert_eq!(
            s.keys,
            vec![format!("{id}:1"); 3],
            "the same attempt and key every time"
        );
        drop(s);
        assert_eq!(
            attempts(&e, &id),
            1,
            "one ledger reservation, never a second"
        );
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(1.0)));
        let (reserved, actual) =
            ledger::day_totals(&store::open(&e.ctx).unwrap(), &store::today()).unwrap();
        assert_eq!((reserved, actual), (0.0, 1.0));
    }

    #[test]
    fn bounded_connect_retries_fail_the_job_with_the_reservation_released() {
        let e = env();
        e.st.lock().unwrap().unsent = 100;
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(
            r["status"], "failed",
            "a transport failure, never outcome_unknown"
        );
        assert_eq!(r["error"]["code"], "transport");
        assert_eq!(r["attempts"], 1, "no extra paid attempt was counted");
        assert_eq!(
            e.st.lock().unwrap().submits as u32,
            1 + jobs::MAX_CONNECT_RETRIES
        );
        assert_eq!(attempts(&e, &id), 1);
        assert_eq!(
            ledger_row(&e, &id, 1),
            (1.0, Some(0.0)),
            "nothing left this machine, nothing is owed"
        );
        let (reserved, actual) =
            ledger::day_totals(&store::open(&e.ctx).unwrap(), &store::today()).unwrap();
        assert_eq!((reserved, actual), (0.0, 0.0));
        assert!(recoverable_jobs(&e.ctx).unwrap().is_empty());
        // A later run sends nothing.
        let again = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(again["status"], "failed");
        assert_eq!(
            e.st.lock().unwrap().submits as u32,
            1 + jobs::MAX_CONNECT_RETRIES
        );
    }

    #[test]
    fn possibly_delivered_failure_after_a_connect_failure_is_still_held() {
        let e = env();
        {
            let mut s = e.st.lock().unwrap();
            s.unsent = 1;
            s.ambiguous = 1;
        }
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "outcome_unknown");
        assert_eq!(
            r["outcome_unknown"]["idempotency_key"],
            json!(format!("{id}:1"))
        );
        assert_eq!(
            e.st.lock().unwrap().submits,
            2,
            "no blind replay after the possibly-delivered send"
        );
        assert_eq!(attempts(&e, &id), 1);
        assert_eq!(ledger_row(&e, &id, 1), (1.0, None), "reservation kept");
    }

    #[test]
    fn resolve_resubmit_allows_exactly_one_new_attempt() {
        let e = env();
        e.st.lock().unwrap().ambiguous = 1;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let v = job_resolve(&e.ctx, &json!({"job_id": id, "action": "resubmit"})).unwrap();
        assert_eq!(v["status"], "submitted");
        assert!(v.get("outcome_unknown").is_none());
        assert_eq!(
            ledger_row(&e, &id, 1),
            (1.0, Some(0.0)),
            "old reservation reconciled to zero"
        );
        assert_eq!(e.st.lock().unwrap().submits, 1, "resolving sends nothing");
        // Not held any more: a second resolve is refused.
        assert_eq!(
            job_resolve(&e.ctx, &json!({"job_id": id, "action": "resubmit"}))
                .unwrap_err()
                .code,
            "not_outcome_unknown"
        );
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(
            (r["status"].as_str(), r["attempts"].clone()),
            (Some("complete"), json!(2))
        );
        let s = e.st.lock().unwrap();
        assert_eq!(s.submits, 2, "exactly one new submit");
        assert_eq!(
            s.keys,
            vec![format!("{id}:1"), format!("{id}:2")],
            "a new attempt number and key"
        );
        drop(s);
        assert_eq!(ledger_row(&e, &id, 2), (1.0, Some(1.0)));
        assert_eq!(spend_status(&e.ctx, &json!({})).unwrap()["actual_usd"], 1.0);
        // Bad actions are refused.
        assert_eq!(
            job_resolve(&e.ctx, &json!({"job_id": id, "action": "retry"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
    }

    #[test]
    fn resolve_abandon_fails_the_job_and_counts_the_reservation_as_spent() {
        let e = env();
        e.st.lock().unwrap().ambiguous = 1;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let v = job_resolve(&e.ctx, &json!({"job_id": id, "action": "abandon"})).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(v["error"]["code"], "outcome_unknown_abandoned");
        assert_eq!(v["actual_usd"], 1.0);
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(1.0)));
        let s = spend_status(&e.ctx, &json!({})).unwrap();
        assert_eq!(
            (s["reserved_usd"].clone(), s["actual_usd"].clone()),
            (json!(0.0), json!(1.0))
        );
        assert_eq!(e.st.lock().unwrap().submits, 1);
        // A terminal job runs nothing further.
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "failed");
        assert_eq!(e.st.lock().unwrap().submits, 1);
    }

    #[test]
    fn resolve_resubmit_is_refused_once_every_attempt_is_used() {
        let e = env();
        let id = new_job(&e, None);
        {
            let mut c = store::open(&e.ctx).unwrap();
            c.write(|c| {
                Ok(c.execute(
                    "UPDATE jobs SET attempts=?2, status='outcome_unknown' WHERE job_id=?1",
                    rightkit_db::rusqlite::params![id, jobs::MAX_ATTEMPTS],
                )?)
            })
            .unwrap();
            ledger::reserve(&mut c, &id, jobs::MAX_ATTEMPTS, 1.0).unwrap();
        }
        assert_eq!(
            job_resolve(&e.ctx, &json!({"job_id": id, "action": "resubmit"}))
                .unwrap_err()
                .code,
            "max_attempts"
        );
        assert_eq!(
            job_resolve(&e.ctx, &json!({"job_id": id, "action": "abandon"})).unwrap()["status"],
            "failed"
        );
    }

    /// A database written before the hold moved to RightKit: the job row says `outcome_unknown`
    /// (set by a plain status update) and the submission row is still `pending`.
    fn legacy_hold(e: &Env, id: &str, code: &str) {
        let c = store::open(&e.ctx).unwrap();
        let error = json!({"code": code, "message": "held before the move"}).to_string();
        c.write(|c| {
            Ok(c.execute(
                "UPDATE jobs SET status='outcome_unknown', error_json=?2 WHERE job_id=?1",
                rightkit_db::rusqlite::params![id, error],
            )?)
        })
        .unwrap();
    }

    #[test]
    fn a_job_held_by_the_old_local_code_resolves_through_rightkit() {
        let e = env();
        let id = new_job(&e, None);
        let key = crash_after_reserve(&e, &id);
        legacy_hold(&e, &id, "outcome_unknown");
        let st = job_status(&e.ctx, &json!({"job_id": id})).unwrap();
        assert_eq!(st["outcome_unknown"]["idempotency_key"], json!(key));
        assert_eq!(st["outcome_unknown"]["reserved_usd"], 1.0);
        assert!(
            recoverable_jobs(&e.ctx).unwrap().is_empty(),
            "a held job is never resumed"
        );
        let v = job_resolve(&e.ctx, &json!({"job_id": id, "action": "resubmit"})).unwrap();
        assert_eq!(v["status"], "submitted");
        assert_eq!(v["error"]["code"], "outcome_unknown_resubmitted");
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(0.0)));
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(
            (r["status"].as_str(), r["attempts"].clone()),
            (Some("complete"), json!(2))
        );
        assert_eq!(
            e.st.lock().unwrap().keys,
            vec![format!("{id}:2")],
            "one new attempt with a new key"
        );
        assert_eq!(ledger_row(&e, &id, 2), (1.0, Some(1.0)));

        let held = new_job(&e, None);
        crash_after_reserve(&e, &held);
        legacy_hold(&e, &held, "outcome_unknown");
        let v = job_resolve(&e.ctx, &json!({"job_id": held, "action": "abandon"})).unwrap();
        assert_eq!(
            (v["status"].as_str(), v["error"]["code"].as_str()),
            (Some("failed"), Some("outcome_unknown_abandoned"))
        );
        assert_eq!(
            ledger_row(&e, &held, 1),
            (1.0, Some(1.0)),
            "the abandoned reservation is counted as spent"
        );
    }

    #[test]
    fn the_ledger_refuses_to_release_a_settled_attempt() {
        let e = env();
        let id = new_job(&e, None);
        crash_after_reserve(&e, &id);
        // A charged attempt (settled) must never be released by a resubmit, whatever the error says.
        let mut c = store::open(&e.ctx).unwrap();
        ledger::reconcile(&mut c, &id, 1, 1.0).unwrap();
        legacy_hold(&e, &id, "outcome_unknown");
        let err = c
            .resolve(
                &id,
                &rightkit_job_queue::Resolution::Resubmit,
                Some(store::SUBMITTED),
            )
            .unwrap_err();
        assert_eq!(err.code, jobs::RESULT_RECOVERED);
        assert_eq!(
            store::get_job(&c, &id).unwrap().status,
            "outcome_unknown",
            "still held"
        );
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(1.0)));
    }

    #[test]
    fn recoverable_jobs_skips_a_held_job() {
        let e = env();
        e.st.lock().unwrap().ambiguous = 1;
        let held = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &held, &fast()).unwrap();
        let other = new_job(&e, None);
        assert_eq!(recoverable_jobs(&e.ctx).unwrap(), vec![other]);
    }

    #[test]
    fn confirmed_failure_resubmits_as_new_ledger_attempt() {
        // old: any `Failed` resubmitted. new: only for an idempotent adapter (a replay is free).
        let e = env();
        e.st.lock().unwrap().idempotent = true;
        e.st.lock().unwrap().poll_script = VecDeque::from([
            PollOutcome::Failed {
                message: "boom".into(),
            },
            done(),
        ]);
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        assert_eq!(e.st.lock().unwrap().submits, 2);
        assert_eq!(attempts(&e, &id), 2);
        assert_eq!(r["attempts"], 2);
        // The failed attempt owes nothing; only the attempt that produced output counts.
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(0.0)));
        assert_eq!(ledger_row(&e, &id, 2), (1.0, Some(1.0)));
        let (res, act) =
            ledger::day_totals(&store::open(&e.ctx).unwrap(), &store::today()).unwrap();
        assert_eq!((res, act), (0.0, 1.0));
    }

    #[test]
    fn confirmed_failure_of_a_non_idempotent_job_fails_it_released_and_is_not_resubmitted() {
        let e = env();
        e.st.lock().unwrap().poll_script = VecDeque::from([
            PollOutcome::Failed {
                message: "boom".into(),
            },
            done(),
        ]);
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(
            (r["status"].as_str(), r["error"]["code"].as_str()),
            (Some("failed"), Some("provider_failed"))
        );
        assert_eq!(
            (e.st.lock().unwrap().submits, attempts(&e, &id)),
            (1, 1),
            "no second paid attempt"
        );
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(0.0)));
    }

    #[test]
    fn unknown_poll_outcome_holds_the_job_and_keeps_the_reservation() {
        let e = env();
        e.st.lock().unwrap().poll_script = VecDeque::from([
            PollOutcome::Unknown {
                status: "WEIRD".into(),
                detail: "{\"x\":1}".into(),
                completed: false,
            },
            done(),
        ]);
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "outcome_unknown");
        assert!(r["error"]["message"].as_str().unwrap().contains("WEIRD"));
        let (submits, polls) = {
            let st = e.st.lock().unwrap();
            (st.submits, st.polls)
        };
        assert_eq!((submits, polls, attempts(&e, &id)), (1, 1, 1));
        assert_eq!(ledger_row(&e, &id, 1), (1.0, None), "still reserved");
        // A held job is never replayed by a later run.
        assert_eq!(
            run_job_with(&e.ctx, &e.providers, &id, &fast())
                .unwrap_err()
                .code,
            "outcome_unknown"
        );
        assert_eq!(e.st.lock().unwrap().submits, 1);
    }

    #[test]
    fn completed_without_usable_output_holds_settled_and_resubmit_is_refused() {
        let e = env();
        e.st.lock().unwrap().poll_script = VecDeque::from([PollOutcome::Unknown {
            status: "completed, no usable media".into(),
            detail: String::new(),
            completed: true,
        }]);
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(
            (r["status"].as_str(), r["error"]["code"].as_str()),
            (Some("outcome_unknown"), Some("result_unusable"))
        );
        assert_eq!(
            ledger_row(&e, &id, 1),
            (1.0, Some(1.0)),
            "settled at the estimate"
        );
        assert_eq!(
            jobs::resolve_job(&e.ctx, &id, "resubmit").unwrap_err().code,
            "result_unusable"
        );
        // The listing must not offer the refused action.
        let held = r["outcome_unknown"]["resolve"].clone();
        assert_eq!(
            held,
            json!(["abandon"]),
            "result_unusable offers abandon only"
        );
        let v = jobs::resolve_job(&e.ctx, &id, "abandon").unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(1.0)));
        let audit = ledger::setting_audit(&store::open(&e.ctx).unwrap(), 10).unwrap();
        assert!(
            audit
                .iter()
                .any(|r| r["command"] == "job.resolve" && r["detail"]["action"] == "abandon"),
            "{audit:?}"
        );
    }

    #[test]
    fn poll_error_on_an_accepted_paid_job_holds_instead_of_releasing() {
        // old: a non-retryable poll error failed the job and released the reservation to 0.
        // new: the job was accepted and may still bill (a 404 may only mean it expired): held.
        let e = env();
        e.st.lock().unwrap().poll_error = Some(ProviderError::new("not_found", "gone", false));
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "outcome_unknown");
        assert_eq!(ledger_row(&e, &id, 1), (1.0, None));
        let s = spend_status(&e.ctx, &json!({})).unwrap();
        assert_eq!(s["reserved_usd"], json!(1.0));
        let v = jobs::resolve_job(&e.ctx, &id, "abandon").unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(
            ledger_row(&e, &id, 1),
            (1.0, Some(1.0)),
            "abandon counts it as spent"
        );
    }

    #[test]
    fn descriptor_failure_of_a_paid_result_holds_it_settled() {
        // old: released to 0 and failed `no_output`. new: the provider completed and charged.
        let e = env();
        e.st.lock().unwrap().poll_script =
            VecDeque::from([PollOutcome::Completed { outputs: vec![] }]);
        let id = new_job(&e, None);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(
            (r["status"].as_str(), r["error"]["code"].as_str()),
            (Some("outcome_unknown"), Some("result_unusable"))
        );
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(1.0)));
        let s = spend_status(&e.ctx, &json!({})).unwrap();
        assert_eq!(
            (s["reserved_usd"].clone(), s["actual_usd"].clone()),
            (json!(0.0), json!(1.0))
        );
    }

    #[test]
    fn cancelling_an_accepted_paid_job_without_a_provider_cancel_holds_and_keeps_the_reservation() {
        // old: released the reservation, then called the provider's no-op cancel.
        // new: the Fake has no cancel endpoint (`can_cancel` false): held, reservation kept.
        let e = env();
        let id = new_job(&e, None);
        crash_after_reserve(&e, &id);
        store::set_provider_id(&store::open(&e.ctx).unwrap(), &id, "p1").unwrap();
        let v = job_cancel_with(&e.ctx, &e.providers, &json!({"job_id": id})).unwrap();
        assert_eq!(v["status"], "outcome_unknown");
        assert_eq!(v["cancel"]["applied"], false);
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cancel requested"));
        assert_eq!(ledger_row(&e, &id, 1), (1.0, None));
        let v = jobs::resolve_job(&e.ctx, &id, "abandon").unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(1.0)));
    }

    #[test]
    fn cancelling_releases_the_open_reservation() {
        let e = env();
        let id = new_job(&e, None);
        crash_after_reserve(&e, &id);
        assert_eq!(
            spend_status(&e.ctx, &json!({})).unwrap()["reserved_usd"],
            1.0
        );
        let v = job_cancel_with(&e.ctx, &e.providers, &json!({"job_id": id})).unwrap();
        assert_eq!(v["status"], "cancelled");
        assert_eq!(ledger_row(&e, &id, 1), (1.0, Some(0.0)));
        let s = spend_status(&e.ctx, &json!({})).unwrap();
        assert_eq!(
            (s["reserved_usd"].clone(), s["actual_usd"].clone()),
            (json!(0.0), json!(0.0))
        );
    }

    #[test]
    fn spend_cap_blocks() {
        let e = env();
        spend_set_cap(&e.ctx, &json!({"cap_usd": 1.5})).unwrap();
        let a = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &a, &fast()).unwrap();
        // Second job no longer fits: refused up front.
        let err = generate_with(&e.ctx, &e.providers, &gen_input(None), false).unwrap_err();
        assert_eq!(err.code, "spend_cap");
        // And the authoritative reservation refuses too.
        let c = {
            let mut conn = store::open(&e.ctx).unwrap();
            let r = ledger::reserve(&mut conn, "other", 1, 1.0);
            r.unwrap_err()
        };
        assert_eq!(c.code, "spend_cap");
        assert_eq!(spend_status(&e.ctx, &json!({})).unwrap()["cap_usd"], 1.5);
    }

    fn budgeted(remaining: Value) -> Value {
        let mut v = gen_input(None);
        v["caller_budget"] = json!({"project": "proj-1", "remaining_usd": remaining});
        v
    }

    #[test]
    fn caller_budget_refuses_over_budget_before_any_job_or_reservation() {
        let e = env();
        // The fake provider estimates 1.0 USD; the budget has 0.5 left.
        for dry_run in [false, true] {
            let err =
                generate_with(&e.ctx, &e.providers, &budgeted(json!(0.5)), dry_run).unwrap_err();
            assert_eq!(err.code, "caller_budget_exceeded", "dry_run={dry_run}");
            let d = err.details.expect("typed details");
            assert_eq!(
                (
                    d["project"].as_str(),
                    d["needed"].as_f64(),
                    d["remaining"].as_f64()
                ),
                (Some("proj-1"), Some(1.0), Some(0.5))
            );
        }
        assert!(job_list(&e.ctx, &json!({})).unwrap()["jobs"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(e.st.lock().unwrap().submits, 0);
        // Unusable budgets are refused by name, never read as "no budget".
        for bad in [json!(-1.0), json!("5"), Value::Null] {
            let err =
                generate_with(&e.ctx, &e.providers, &budgeted(bad.clone()), false).unwrap_err();
            assert_eq!(err.code, "invalid_caller_budget", "{bad}");
        }
        let half = {
            let mut v = gen_input(None);
            v["caller_budget"] = json!({"project": "proj-1"});
            v
        };
        assert_eq!(
            generate_with(&e.ctx, &e.providers, &half, false)
                .unwrap_err()
                .code,
            "invalid_caller_budget"
        );
    }

    #[test]
    fn caller_budget_accepts_under_budget_and_reports_the_spend() {
        let e = env();
        let made = generate_with(&e.ctx, &e.providers, &budgeted(json!(1.5)), false).unwrap();
        let id = made["job_id"].as_str().unwrap().to_string();
        let done = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(done["status"], "complete");
        let status = job_status(&e.ctx, &json!({"job_id": id})).unwrap();
        let spend = &status["spend"];
        assert_eq!(spend["project"], "proj-1");
        assert_eq!(spend["reserved_usd"], 1.0, "{spend}");
        assert!(spend["actual_usd"].as_f64().is_some(), "{spend}");
        assert_eq!(spend["run_reserved_usd"], spend["reserved_usd"]);
        assert_eq!(
            job_spend(&e.ctx, &id).unwrap().unwrap()["project"],
            "proj-1"
        );
        // No budget: no `spend` object, behaviour unchanged.
        let plain = new_job(&e, Some("plain"));
        run_job_with(&e.ctx, &e.providers, &plain, &fast()).unwrap();
        assert!(job_status(&e.ctx, &json!({"job_id": plain}))
            .unwrap()
            .get("spend")
            .is_none());
    }

    #[test]
    fn runner_refuses_when_the_caller_budget_is_spent_and_fails_the_job_unreserved() {
        let e = env();
        let id = generate_with(&e.ctx, &e.providers, &budgeted(json!(1.5)), false).unwrap()
            ["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        // 0.8 of the budget was already reserved against this job; this attempt's 1.0 no longer fits.
        // (`ledger::reserve_budgeted` would also move the job's attempt count, so reserve through
        // the ledger's own attempt function.)
        {
            let conn = store::open(&e.ctx).unwrap();
            let budget = ledger::CallerBudget::new("proj-1", 1.5).unwrap();
            let h = conn.spend().clone();
            let a = ledger::Attempt {
                job_id: &id,
                attempt: 9,
                est_usd: 0.8,
                route: "test",
                scope: None,
                budget: Some(&budget),
                process: None,
                day: store::today(),
            };
            conn.write(|c| ledger::reserve_attempt_on(&ledger::lock(&h), c, &a))
                .unwrap();
        }
        let err = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap_err();
        assert_eq!(err.code, "caller_budget_exceeded");
        let d = err.details.expect("typed details");
        assert_eq!(
            (d["project"].as_str(), d["needed"].as_f64()),
            (Some("proj-1"), Some(1.0))
        );
        assert!((d["remaining"].as_f64().unwrap() - 0.7).abs() < 1e-9, "{d}");
        assert_eq!(
            e.st.lock().unwrap().submits,
            0,
            "nothing was sent to the provider"
        );
        assert_eq!(
            attempts(&e, &id),
            1,
            "only the earlier reservation exists; this attempt reserved nothing"
        );
        let status = job_status(&e.ctx, &json!({"job_id": id})).unwrap();
        assert_eq!(status["status"], "failed");
        assert_eq!(status["error"]["code"], "caller_budget_exceeded");
    }

    #[test]
    fn runner_refuses_when_cap_exhausted() {
        let e = env();
        let id = new_job(&e, None);
        spend_set_cap(&e.ctx, &json!({"cap_usd": 0.5})).unwrap();
        let err = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap_err();
        assert_eq!(err.code, "spend_cap");
        assert_eq!(e.st.lock().unwrap().submits, 0);
        assert_eq!(
            job_status(&e.ctx, &json!({"job_id": id})).unwrap()["status"],
            "failed"
        );
    }

    #[test]
    fn request_id_dedup_returns_same_job() {
        let e = env();
        let a = generate_with(&e.ctx, &e.providers, &gen_input(Some("req-1")), false).unwrap();
        let b = generate_with(&e.ctx, &e.providers, &gen_input(Some("req-1")), false).unwrap();
        assert_eq!(a["job_id"], b["job_id"]);
        assert_eq!(a["deduplicated"], false);
        assert_eq!(b["deduplicated"], true);
        assert_eq!(b["request_id"], "req-1");
        assert_eq!(
            job_list(&e.ctx, &json!({})).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn lock_prevents_two_runners() {
        let e = env();
        let id = new_job(&e, None);
        let held = jobs::try_lock(&e.ctx, &id).unwrap().expect("first lock");
        assert!(jobs::try_lock(&e.ctx, &id).unwrap().is_none());
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "locked_elsewhere");
        assert!(recoverable_jobs(&e.ctx).unwrap().is_empty());
        // From another thread too.
        let ctx = e.ctx.clone();
        let id2 = id.clone();
        assert!(
            std::thread::spawn(move || jobs::try_lock(&ctx, &id2).unwrap().is_none())
                .join()
                .unwrap()
        );
        drop(held);
        assert_eq!(recoverable_jobs(&e.ctx).unwrap(), vec![id.clone()]);
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        assert!(recoverable_jobs(&e.ctx).unwrap().is_empty());
    }

    #[test]
    fn unacked_results_survive_cleanup() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let a = new_job(&e, None);
        let b = new_job(&e, None);
        for id in [&a, &b] {
            run_job_with(&e.ctx, &e.providers, id, &fast()).unwrap();
        }
        let pa = job_result(&e.ctx, &json!({"job_id": a})).unwrap()["assets"][0]["path"]
            .as_str()
            .unwrap()
            .to_string();
        let pb = job_result(&e.ctx, &json!({"job_id": b})).unwrap()["assets"][0]["path"]
            .as_str()
            .unwrap()
            .to_string();
        job_ack(&e.ctx, &json!({"job_id": b, "consumer": "cutright"})).unwrap();
        // Not old enough yet: nothing deleted.
        assert_eq!(crate::descriptor::cleanup_acked(&e.ctx, 1).unwrap(), 0);
        assert!(std::path::Path::new(&pb).exists());
        // Old enough: only the acked one goes.
        assert_eq!(crate::descriptor::cleanup_acked(&e.ctx, 0).unwrap(), 1);
        assert!(
            std::path::Path::new(&pa).exists(),
            "un-acked result survives"
        );
        assert!(!std::path::Path::new(&pb).exists());
        assert!(job_result(&e.ctx, &json!({"job_id": a})).is_ok());
        assert_eq!(
            job_result(&e.ctx, &json!({"job_id": b})).unwrap_err().code,
            "result_cleaned"
        );
    }

    #[test]
    fn caller_route_key_is_refused_before_anything_is_created() {
        let e = env();
        let forged = json!({"kind":"image","provider":"fake","model":"fake-img","prompt":"a cat",
            "route":{"basis":"free_route","reason":"forged"}});
        for dry in [true, false] {
            let err = generate_with(&e.ctx, &e.providers, &forged, dry).unwrap_err();
            assert_eq!(err.code, "invalid_input");
            assert!(
                err.message.contains("route is set by the engine"),
                "{}",
                err.message
            );
        }
        let err = generate_wait_with(&e.ctx, &e.providers, &forged, &fast()).unwrap_err();
        assert_eq!(err.code, "invalid_input");
        // A null route is still a caller key.
        let mut null_route = gen_input(Some("null-route"));
        null_route["route"] = Value::Null;
        assert_eq!(
            generate_with(&e.ctx, &e.providers, &null_route, false)
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert!(
            !e.ctx.db_path().exists(),
            "a refused request creates no store and no job"
        );
        assert!(
            !GENERATE_KEYS.contains(&"route"),
            "route is not an accepted caller key"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let e = env();
        let r = generate_with(&e.ctx, &e.providers, &gen_input(Some("x")), true).unwrap();
        assert_eq!(r["dry_run"], true);
        assert_eq!(r["est_usd"], 1.0);
        assert!(r["would_submit"].is_object());
        assert!(!e.ctx.db_path().exists());
        assert!(!e.ctx.library_dir().exists());
        let bad = json!({"kind":"image","provider":"fake","model":"nope","prompt":"x"});
        assert_eq!(
            generate_with(&e.ctx, &e.providers, &bad, true)
                .unwrap_err()
                .code,
            "unknown_model"
        );
        let empty = json!({"kind":"image","provider":"fake","model":"fake-img","prompt":"  "});
        assert_eq!(
            generate_with(&e.ctx, &e.providers, &empty, true)
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert!(!e.ctx.db_path().exists());
    }

    #[test]
    fn cancel_stops_runner_and_readiness_is_live() {
        let e = env();
        let id = new_job(&e, None);
        job_cancel_with(&e.ctx, &e.providers, &json!({"job_id": id})).unwrap();
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "cancelled");
        assert_eq!(e.st.lock().unwrap().submits, 0);
        let rd = readiness_with(&e.ctx, &e.providers);
        assert_eq!(rd["db_ok"], true);
        assert_eq!(rd["providers"][0]["configured"], true);
        credentials_set_with(
            &e.ctx,
            &e.providers,
            &json!({"provider":"fake","api_key":""}),
        )
        .unwrap();
        assert_eq!(
            readiness_with(&e.ctx, &e.providers)["providers"][0]["configured"],
            false
        );
    }

    #[test]
    fn generate_without_key_creates_no_job() {
        let e = env();
        credentials_set_with(
            &e.ctx,
            &e.providers,
            &json!({"provider":"fake","api_key":""}),
        )
        .unwrap();
        let err =
            generate_with(&e.ctx, &e.providers, &gen_input(Some("nokey")), false).unwrap_err();
        assert_eq!(err.code, "no_credentials");
        let jobs = job_list(&e.ctx, &json!({})).unwrap();
        assert_eq!(jobs["jobs"].as_array().map(Vec::len).unwrap_or(0), 0);
    }

    #[test]
    fn keyless_provider_generates_and_reports_ready_without_a_key() {
        let dir = tempfile_dir::Dir::new();
        let creds = Arc::new(MemoryCredentials::new());
        let ctx = Context::new(dir.0.clone()).with_credentials(creds);
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(FakeKeyless)];
        let st = credentials_status_with(&ctx, &providers, &json!({})).unwrap();
        assert_eq!(st["providers"][0]["configured"], true);
        assert_eq!(st["providers"][0]["needs_key"], false);
        assert_eq!(st["providers"][0]["source"], "local");
        assert_eq!(
            credentials_set_with(
                &ctx,
                &providers,
                &json!({"provider":"fake-keyless","api_key":"x"})
            )
            .unwrap_err()
            .code,
            "no_key_needed"
        );
        let r = generate_with(
            &ctx,
            &providers,
            &json!({"kind":"voice","provider":"fake-keyless","model":"fake-voice","prompt":"hi"}),
            false,
        )
        .unwrap();
        let done = run_job_with(&ctx, &providers, r["job_id"].as_str().unwrap(), &fast()).unwrap();
        assert_eq!(done["status"], "complete");
    }

    #[test]
    fn keyless_status_readiness_and_generation_never_touch_credentials() {
        #[derive(Debug)]
        struct NoCredentialAccess;
        impl crate::credentials::CredentialSource for NoCredentialAccess {
            fn store(&self) -> &rightkit_credentials::Store {
                panic!("unexpected credential store access")
            }
            fn get(&self, _: &str) -> GenResult<Option<String>> {
                panic!("unexpected secret read")
            }
            fn contains(&self, _: &str) -> GenResult<bool> {
                panic!("unexpected credential probe")
            }
            fn set(&self, _: &str, _: &str) -> GenResult<()> {
                panic!("unexpected credential write")
            }
        }
        let dir = tempfile_dir::Dir::new();
        let ctx = Context::new(dir.0.clone()).with_credentials(Arc::new(NoCredentialAccess));
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(FakeKeyless)];
        assert_eq!(
            credentials_status_with(&ctx, &providers, &json!({})).unwrap()["providers"][0]
                ["configured"],
            true
        );
        assert_eq!(
            readiness_with(&ctx, &providers)["providers"][0]["configured"],
            true
        );
        let input = json!({"kind":"voice", "provider":"fake-keyless", "model":"fake-voice", "prompt":"test"});
        let generated = generate_with(&ctx, &providers, &input, false).unwrap();
        let result = run_job_with(
            &ctx,
            &providers,
            generated["job_id"].as_str().unwrap(),
            &fast(),
        )
        .unwrap();
        assert_eq!(result["status"], "complete");
    }

    /// Keyless local engine whose model files are absent: `configured` is true
    /// (no key needed) but `runtime.ready` must be false — never conflated.
    struct FakeKeylessUninstalled;

    impl Provider for FakeKeylessUninstalled {
        fn id(&self) -> &'static str {
            "fake-keyless-missing"
        }
        fn models(&self) -> &'static [ModelInfo] {
            &KEYLESS_MODELS
        }
        fn needs_key(&self) -> bool {
            false
        }
        fn estimate_usd(&self, _r: &GenRequest) -> Result<f64, ProviderError> {
            Ok(0.0)
        }
        fn submit(&self, _r: &GenRequest, _k: &str) -> Result<SubmitOutcome, ProviderError> {
            Err(ProviderError::new(
                "not_installed",
                "model files missing",
                false,
            ))
        }
        fn poll(&self, _r: &GenRequest, _id: &str, _k: &str) -> Result<PollOutcome, ProviderError> {
            Err(ProviderError::new("invalid_request", "synchronous", false))
        }
        fn test_key(&self, _k: &str) -> Result<(), ProviderError> {
            Err(ProviderError::new(
                "not_installed",
                "model files missing",
                false,
            ))
        }
        fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
            Some(self.test_key(""))
        }
        fn fetch(&self, _o: &Output) -> Result<(Vec<u8>, String), ProviderError> {
            Err(ProviderError::new("nope", "no network", false))
        }
    }

    #[test]
    fn keyless_uninstalled_provider_reports_runtime_not_ready() {
        let dir = tempfile_dir::Dir::new();
        let creds = Arc::new(MemoryCredentials::new());
        let ctx = Context::new(dir.0.clone()).with_credentials(creds);
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(FakeKeylessUninstalled)];
        let st = credentials_status_with(&ctx, &providers, &json!({})).unwrap();
        let p = &st["providers"][0];
        assert_eq!(p["configured"], true);
        assert_eq!(p["runtime"]["ready"], false);
        assert_eq!(p["runtime"]["code"], "not_installed");
        let rd = readiness_with(&ctx, &providers);
        assert_eq!(rd["providers"][0]["runtime"]["ready"], false);
    }

    fn first_asset(e: &Env, job_id: &str) -> Value {
        job_result(&e.ctx, &json!({"job_id": job_id})).unwrap()["assets"][0].clone()
    }

    #[test]
    fn progress_events_are_stored_bounded_and_reported() {
        let e = env();
        e.st.lock().unwrap().poll_script =
            VecDeque::from([PollOutcome::Running, PollOutcome::Running, done()]);
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let s = job_status(&e.ctx, &json!({"job_id": id, "events": true})).unwrap();
        let stages: Vec<&str> = s["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["stage"].as_str().unwrap())
            .collect();
        assert_eq!(stages.first(), Some(&"submitted"));
        assert_eq!(stages.iter().filter(|x| **x == "polling").count(), 3);
        assert!(stages.contains(&"downloading"));
        let dl = s["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["stage"] == "downloading")
            .unwrap();
        assert_eq!(
            (dl["status"].as_str(), dl["attempt"].as_i64()),
            (Some("running"), Some(1))
        );
        assert_eq!(stages.last(), Some(&"complete"));
        let last = &s["progress"];
        assert_eq!(last["event"], "job.progress");
        assert_eq!(last["job_id"], id.as_str());
        assert_eq!(last["status"], "complete");
        assert_eq!(last["stage"], "complete");
        assert_eq!(last["attempt"], 1);
        assert!(last["at"].is_string());
        // Without events:true only `progress` is present.
        let plain = job_status(&e.ctx, &json!({"job_id": id})).unwrap();
        assert!(plain.get("events").is_none());
        assert_eq!(plain["progress"]["stage"], "complete");
        // Bounded to the last MAX_EVENTS per job.
        let c = store::open(&e.ctx).unwrap();
        for i in 0..250 {
            store::add_event(&c, &id, "polling", Some(&format!("n{i}"))).unwrap();
        }
        let all = store::events(&c, &id).unwrap();
        assert_eq!(all.len() as i64, store::MAX_EVENTS);
        assert_eq!(all.last().unwrap()["message"], "n249");
    }

    #[test]
    fn failed_and_cancelled_runs_emit_failed_stage() {
        let e = env();
        e.st.lock().unwrap().fail_submit = true;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let p = job_status(&e.ctx, &json!({"job_id": id})).unwrap()["progress"].clone();
        assert_eq!(
            (p["status"].as_str(), p["stage"].as_str()),
            (Some("failed"), Some("failed"))
        );
        assert_eq!(p["message"], "bad");
    }

    #[test]
    fn wait_runs_in_process_and_returns_final_record() {
        let e = env();
        e.st.lock().unwrap().poll_script = VecDeque::from([PollOutcome::Running, done()]);
        let mut input = gen_input(Some("w1"));
        input["wait"] = json!(true);
        assert!(wants_wait(&input));
        let r = generate_wait_with(&e.ctx, &e.providers, &input, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        assert_eq!(r["request_id"], "w1");
        assert_eq!(r["progress"]["stage"], "complete");
        assert_eq!(r["result"]["assets"][0]["media"]["width"], 7);
        // `wait` never reaches the stored request.
        assert!(r["request"].get("wait").is_none());
        // Repeat with the same request id returns the same finished job.
        let again = generate_wait_with(&e.ctx, &e.providers, &input, &fast()).unwrap();
        assert_eq!(again["job_id"], r["job_id"]);
        assert_eq!(e.st.lock().unwrap().submits, 1);
    }

    #[test]
    fn asset_resolve_by_id_and_unknown() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let a = first_asset(&e, &id);
        let r = asset_resolve(&e.ctx, &json!({"asset_id": a["asset_id"]})).unwrap();
        assert_eq!(r["job_id"], id.as_str());
        assert_eq!(r["path"], a["path"]);
        assert_eq!(r["sha256"], a["sha256"]);
        assert_eq!(r["bytes"], a["bytes"]);
        assert_eq!(r["mime"], "image/png");
        assert_eq!(r["exists"], true);
        assert_eq!(
            asset_resolve(&e.ctx, &json!({"asset_id": "nope"}))
                .unwrap_err()
                .code,
            "asset_not_found"
        );
        assert_eq!(
            asset_resolve(&e.ctx, &json!({})).unwrap_err().code,
            "invalid_input"
        );
    }

    #[test]
    fn asset_resolve_falls_back_to_descriptor_for_legacy_jobs() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let a = first_asset(&e, &id);
        store::open(&e.ctx)
            .unwrap()
            .write(|c| Ok(c.execute("DELETE FROM assets", [])?))
            .unwrap();
        let r = asset_resolve(&e.ctx, &json!({"asset_id": a["asset_id"]})).unwrap();
        assert_eq!(r["exists"], true);
        assert_eq!(r["job_id"], id.as_str());
    }

    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for ent in std::fs::read_dir(from).unwrap() {
            let ent = ent.unwrap();
            let dest = to.join(ent.file_name());
            if ent.path().is_dir() {
                copy_dir(&ent.path(), &dest);
            } else {
                std::fs::copy(ent.path(), dest).unwrap();
            }
        }
    }

    #[test]
    fn asset_resolve_survives_a_moved_library_and_checks_sha() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let id = new_job(&e, None); // never acked
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let a = first_asset(&e, &id);
        // Same database; library copied elsewhere and the old one gone.
        let moved = tempfile_dir::Dir::new();
        copy_dir(&e.ctx.library_dir(), &moved.0.join("library"));
        std::fs::remove_dir_all(e.ctx.library_dir()).unwrap();
        let ctx2 = e.ctx.clone().with_library_dir(moved.0.join("library"));
        let r = asset_resolve(&ctx2, &json!({"asset_id": a["asset_id"]})).unwrap();
        assert_eq!(r["exists"], true);
        let p = r["path"].as_str().unwrap();
        assert!(p.starts_with(moved.0.to_str().unwrap()), "{p}");
        assert_eq!(
            std::fs::read(p).unwrap().len() as i64,
            a["bytes"].as_i64().unwrap()
        );
        // The un-acked job result is still retrievable too.
        assert!(job_result(&ctx2, &json!({"job_id": id})).is_ok());
        // Tampered file in the new location is rejected.
        std::fs::write(p, b"tampered").unwrap();
        assert_eq!(
            asset_resolve(&ctx2, &json!({"asset_id": a["asset_id"]}))
                .unwrap_err()
                .code,
            "asset_corrupt"
        );
        // Missing everywhere: exists:false rather than an error.
        std::fs::remove_file(p).unwrap();
        assert_eq!(
            asset_resolve(&ctx2, &json!({"asset_id": a["asset_id"]})).unwrap()["exists"],
            false
        );
    }

    #[test]
    fn ack_state_in_status_and_result() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        job_ack(&e.ctx, &json!({"job_id": id, "consumer": "cutright"})).unwrap();
        let s = job_status(&e.ctx, &json!({"job_id": id})).unwrap();
        assert_eq!(s["acked_by"], "cutright");
        assert!(s["acked_at"].is_string());
        let d = job_result(&e.ctx, &json!({"job_id": id})).unwrap();
        assert_eq!(d["acked_by"], "cutright");
        assert_eq!(d["acked_at"], s["acked_at"]);
    }

    #[test]
    fn status_by_request_id_and_list_filters() {
        let e = env();
        let a = new_job(&e, Some("r-a"));
        let b = new_job(&e, Some("r-b"));
        let s = job_status(&e.ctx, &json!({"request_id": "r-b"})).unwrap();
        assert_eq!(s["job_id"], b.as_str());
        assert_eq!(
            job_status(&e.ctx, &json!({"request_id": "nope"}))
                .unwrap_err()
                .code,
            "job_not_found"
        );
        assert_eq!(
            job_status(&e.ctx, &json!({"job_id": "nope"}))
                .unwrap_err()
                .code,
            "job_not_found"
        );
        assert_eq!(
            job_status(&e.ctx, &json!({})).unwrap_err().code,
            "invalid_input"
        );
        run_job_with(&e.ctx, &e.providers, &a, &fast()).unwrap();
        let l = job_list(&e.ctx, &json!({"request_id": "r-a"})).unwrap();
        assert_eq!(l["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(l["truncated"], false);
        let done = job_list(&e.ctx, &json!({"status": "complete"})).unwrap();
        assert_eq!(done["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(done["jobs"][0]["job_id"], a.as_str());
        let sub = job_list(&e.ctx, &json!({"status": "submitted"})).unwrap();
        assert_eq!(sub["jobs"][0]["job_id"], b.as_str());
        let one = job_list(&e.ctx, &json!({"limit": 1})).unwrap();
        assert_eq!(one["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(one["truncated"], true);
        let all = job_list(&e.ctx, &json!({"limit": 5000})).unwrap();
        assert_eq!(all["truncated"], false);
    }

    #[test]
    fn media_probe_fills_descriptor_for_audio_and_video() {
        use crate::providers::Output;
        let e = env();
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&[1, 0, 1, 0]);
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&[2, 0, 16, 0]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&vec![0u8; 32000]);
        let job = {
            let c = store::open(&e.ctx).unwrap();
            let id = new_job(&e, None);
            store::get_job(&c, &id).unwrap()
        };
        let d = crate::descriptor::build(
            &e.ctx,
            e.providers[0].as_ref(),
            &Job {
                kind: "voice".into(),
                ..job
            },
            &[Output::Bytes {
                bytes: wav,
                mime: "audio/wav".into(),
            }],
        )
        .unwrap();
        let m = &d["assets"][0]["media"];
        assert_eq!(m["kind"], "voice");
        assert_eq!(m["duration_s"], 2.0);
        assert_eq!(m["width"], Value::Null);
        let unk = crate::descriptor::build(
            &e.ctx,
            e.providers[0].as_ref(),
            &store::get_job(&store::open(&e.ctx).unwrap(), &new_job(&e, None)).unwrap(),
            &[Output::Bytes {
                bytes: b"???".to_vec(),
                mime: "application/octet-stream".into(),
            }],
        )
        .unwrap();
        assert_eq!(unk["assets"][0]["media"]["width"], Value::Null);
    }

    fn file_count(dir: &std::path::Path) -> usize {
        walk(dir)
    }
    fn walk(dir: &std::path::Path) -> usize {
        match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .flatten()
                // `facts.json` sits beside each asset and is removed with it; count media files.
                .map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        usize::from(e.file_name() != "facts.json")
                    }
                })
                .sum(),
            Err(_) => 0,
        }
    }

    #[test]
    fn cleanup_never_touches_unacked_complete_jobs() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        // Make it arbitrarily old.
        store::open(&e.ctx)
            .unwrap()
            .write(|c| Ok(c.execute("UPDATE jobs SET updated_at='2000-01-01T00:00:00Z'", [])?))
            .unwrap();
        let before = file_count(&e.ctx.library_dir());
        assert_eq!(before, 1);
        for days in [0, 1, 365, 100_000] {
            assert_eq!(crate::descriptor::cleanup_acked(&e.ctx, days).unwrap(), 0);
        }
        assert_eq!(file_count(&e.ctx.library_dir()), before);
        assert!(job_result(&e.ctx, &json!({"job_id": id})).is_ok());
        assert_eq!(
            asset_resolve(
                &e.ctx,
                &json!({"asset_id": first_asset(&e, &id)["asset_id"]})
            )
            .unwrap()["exists"],
            true
        );
        // Once acked and old, cleanup removes the files; the asset record remains.
        job_ack(&e.ctx, &json!({"job_id": id, "consumer": "cutright"})).unwrap();
        store::open(&e.ctx)
            .unwrap()
            .write(|c| Ok(c.execute("UPDATE jobs SET updated_at='2000-01-01T00:00:00Z'", [])?))
            .unwrap();
        let aid = first_asset(&e, &id)["asset_id"].clone();
        assert_eq!(crate::descriptor::cleanup_acked(&e.ctx, 1).unwrap(), 1);
        assert_eq!(file_count(&e.ctx.library_dir()), 0);
        assert_eq!(
            asset_resolve(&e.ctx, &json!({"asset_id": aid})).unwrap()["exists"],
            false
        );
    }

    #[test]
    fn no_other_api_deletes_assets() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let id = new_job(&e, None);
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let before = file_count(&e.ctx.library_dir());
        job_cancel_with(&e.ctx, &e.providers, &json!({"job_id": id})).unwrap();
        assert_eq!(
            job_status(&e.ctx, &json!({"job_id": id})).unwrap()["status"],
            "complete"
        );
        job_ack(&e.ctx, &json!({"job_id": id, "consumer": "c"})).unwrap();
        let _ = job_list(&e.ctx, &json!({}));
        let _ = job_result(&e.ctx, &json!({"job_id": id}));
        let _ = asset_resolve(
            &e.ctx,
            &json!({"asset_id": first_asset(&e, &id)["asset_id"]}),
        );
        // Re-running a finished job, recovery scans and credential changes keep files too.
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let _ = recoverable_jobs(&e.ctx);
        credentials_set_with(
            &e.ctx,
            &e.providers,
            &json!({"provider":"fake","api_key":""}),
        )
        .unwrap();
        assert_eq!(file_count(&e.ctx.library_dir()), before);
        assert!(job_result(&e.ctx, &json!({"job_id": id})).is_ok());
        // And a failed job (store_asset is only reached on success) never deletes.
        let e2 = env();
        e2.st.lock().unwrap().fail_submit = true;
        let bad = new_job(&e2, None);
        run_job_with(&e2.ctx, &e2.providers, &bad, &fast()).unwrap();
        assert!(!e2.ctx.library_dir().exists());
    }

    // ---- Concurrency, wait timeouts, idempotent re-generate ----

    use std::sync::Barrier;
    use std::time::Instant;

    fn ledger_rows(e: &Env) -> i64 {
        store::open(&e.ctx)
            .unwrap()
            .read(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM genright_spend_attempts", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap()
    }

    fn job_rows(e: &Env) -> i64 {
        store::open(&e.ctx)
            .unwrap()
            .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))?))
            .unwrap()
    }

    #[test]
    fn parallel_generates_create_distinct_jobs_without_busy_errors() {
        let e = env();
        const N: usize = 16;
        let barrier = Barrier::new(N);
        let ids: Vec<String> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..N)
                .map(|i| {
                    let (e, b) = (&e, &barrier);
                    s.spawn(move || {
                        b.wait(); // everyone hits a brand-new database at once
                        let r = generate_with(
                            &e.ctx,
                            &e.providers,
                            &gen_input(Some(&format!("par-{i}"))),
                            false,
                        )
                        .expect("no busy/db error under contention");
                        assert_eq!(r["deduplicated"], false);
                        r["job_id"].as_str().unwrap().to_string()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), N);
        assert_eq!(job_rows(&e), N as i64);
    }

    #[test]
    fn spend_reservation_under_the_cap_is_exact_under_contention() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        spend_set_cap(&e.ctx, &json!({"cap_usd": 5.0})).unwrap();
        const N: usize = 16;
        let ids: Vec<String> = (0..N)
            .map(|i| new_job(&e, Some(&format!("cap-{i}"))))
            .collect();
        let barrier = Barrier::new(N);
        let outcomes: Vec<GenResult<Value>> = std::thread::scope(|s| {
            let hs: Vec<_> = ids
                .iter()
                .map(|id| {
                    let (e, b) = (&e, &barrier);
                    s.spawn(move || {
                        b.wait();
                        run_job_with(&e.ctx, &e.providers, id, &fast())
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let done = outcomes
            .iter()
            .filter(|r| matches!(r, Ok(v) if v["status"] == "complete"))
            .count();
        let capped = outcomes
            .iter()
            .filter(|r| matches!(r, Err(x) if x.code == "spend_cap"))
            .count();
        assert_eq!(
            (done, capped),
            (5, N - 5),
            "only a busy/db error would break this: {outcomes:?}"
        );
        assert_eq!(
            ledger_rows(&e),
            5,
            "exactly the cap's worth of reservations"
        );
        assert_eq!(
            e.st.lock().unwrap().submits,
            5,
            "nothing past the cap reached the provider"
        );
        let st = spend_status(&e.ctx, &json!({})).unwrap();
        assert!(
            st["reserved_usd"].as_f64().unwrap() + st["actual_usd"].as_f64().unwrap() <= 5.0 + 1e-9
        );
    }

    #[test]
    fn same_request_id_in_parallel_yields_one_job_and_one_runner() {
        let e = env();
        e.st.lock().unwrap().poll_script = VecDeque::from([
            PollOutcome::Running,
            PollOutcome::Running,
            PollOutcome::Running,
            done(),
        ]);
        const K: usize = 16;
        let barrier = Barrier::new(K);
        let made: Vec<Value> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..K)
                .map(|_| {
                    let (e, b) = (&e, &barrier);
                    s.spawn(move || {
                        b.wait();
                        generate_with(&e.ctx, &e.providers, &gen_input(Some("same")), false)
                            .unwrap()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(job_rows(&e), 1);
        assert!(made.iter().all(|m| m["job_id"] == made[0]["job_id"]));
        assert_eq!(
            made.iter().filter(|m| m["deduplicated"] == false).count(),
            1
        );
        assert_eq!(
            made.iter().filter(|m| m["deduplicated"] == true).count(),
            K - 1
        );

        // Every caller then runs the job at once (generate --wait style).
        let id = made[0]["job_id"].as_str().unwrap().to_string();
        let barrier = Barrier::new(K);
        // The only other legitimate answer to a run is the wall-clock pause:
        // once `RunOptions::total` elapses the runner returns the job still
        // `submitted`/`running` with `recoverable: true`. Every DB write
        // fsyncs, so on a loaded machine the 10s of `fast()` can run out
        // (item 197); this test is about exclusivity, not run time, so give
        // the winning runner a limit that load cannot reach.
        let patient = RunOptions {
            total: Duration::from_secs(300),
            ..fast()
        };
        let outcomes: Vec<GenResult<Value>> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..K)
                .map(|_| {
                    let (e, b, id, o) = (&e, &barrier, &id, &patient);
                    s.spawn(move || {
                        b.wait();
                        run_job_with(&e.ctx, &e.providers, id, o)
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let runs: Vec<Value> = outcomes
            .iter()
            .map(|r| {
                r.as_ref()
                    .unwrap_or_else(|x| {
                        panic!("a duplicate runner got an error: {x:?}; all: {outcomes:?}")
                    })
                    .clone()
            })
            .collect();
        assert!(
            runs.iter()
                .all(|r| r["status"] == "complete" || r["status"] == "locked_elsewhere"),
            "unexpected run status: {runs:?}"
        );
        assert!(runs.iter().any(|r| r["status"] == "complete"));
        assert_eq!(e.st.lock().unwrap().submits, 1, "one runner drove the job");
        assert_eq!(attempts(&e, &id), 1, "one ledger reservation per attempt");
        assert_eq!(
            job_status(&e.ctx, &json!({"job_id": id})).unwrap()["status"],
            "complete"
        );
    }

    #[test]
    fn parallel_generate_wait_with_same_request_id_all_get_the_result() {
        let e = env();
        e.st.lock().unwrap().poll_script =
            VecDeque::from([PollOutcome::Running, PollOutcome::Running, done()]);
        const K: usize = 8;
        let barrier = Barrier::new(K);
        let recs: Vec<Value> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..K)
                .map(|_| {
                    let (e, b) = (&e, &barrier);
                    s.spawn(move || {
                        b.wait();
                        generate_wait_with(&e.ctx, &e.providers, &gen_input(Some("w")), &fast())
                            .unwrap()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(recs
            .iter()
            .all(|r| r["status"] == "complete" && r["result"]["assets"][0]["sha256"].is_string()));
        assert!(recs.iter().all(|r| r["job_id"] == recs[0]["job_id"]));
        assert_eq!(
            (job_rows(&e), ledger_rows(&e), e.st.lock().unwrap().submits),
            (1, 1, 1)
        );
    }

    #[test]
    fn job_wait_follows_a_job_driven_elsewhere_and_times_out_without_killing_it() {
        let e = env();
        let id = new_job(&e, None);
        // Another "process" holds the lock and is not progressing: wait times out.
        let held = jobs::try_lock(&e.ctx, &id).unwrap().unwrap();
        let t = Instant::now();
        let v = job_wait_with(&e.ctx, &json!({"job_id": id, "timeout_s": 0.2}), &fast()).unwrap();
        assert!(t.elapsed() >= Duration::from_millis(200));
        assert_eq!(v["timed_out"], true);
        assert_eq!(v["status"], "submitted");
        assert!(v.get("result").is_none());
        // job.wait neither took the lock nor altered the job.
        assert!(jobs::try_lock(&e.ctx, &id).unwrap().is_none());
        assert_eq!(attempts(&e, &id), 0);
        drop(held);

        // Now a runner in another thread drives it while job.wait only watches.
        e.st.lock().unwrap().poll_script =
            VecDeque::from([PollOutcome::Running, PollOutcome::Running, done()]);
        let v = std::thread::scope(|s| {
            let runner = s.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap()
            });
            let v =
                job_wait_with(&e.ctx, &json!({"job_id": id, "timeout_s": 30}), &fast()).unwrap();
            runner.join().unwrap();
            v
        });
        assert_eq!(v["timed_out"], false);
        assert_eq!(v["status"], "complete");
        assert!(v["result"]["assets"][0]["asset_id"].is_string());
        // Already terminal: returns at once even with a zero timeout.
        let v = job_wait_with(&e.ctx, &json!({"job_id": id, "timeout_s": 0}), &fast()).unwrap();
        assert_eq!(
            (v["status"].as_str(), v["timed_out"].clone()),
            (Some("complete"), json!(false))
        );
        assert_eq!(
            job_wait(&e.ctx, &json!({"job_id": "missing"}))
                .unwrap_err()
                .code,
            "job_not_found"
        );
        assert_eq!(
            job_wait(&e.ctx, &json!({"job_id": id, "timeout_s": -1}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            job_wait(&e.ctx, &json!({"job_id": id, "timeout_s": "soon"}))
                .unwrap_err()
                .code,
            "invalid_input"
        );
    }

    #[test]
    fn generate_wait_timeout_returns_the_live_job_and_the_job_still_finishes() {
        let e = env();
        e.st.lock().unwrap().poll_script = (0..10_000).map(|_| PollOutcome::Running).collect();
        let mut input = gen_input(Some("slow"));
        input["wait_timeout_s"] = json!(0.3);
        let t = Instant::now();
        let v = generate_wait_with(&e.ctx, &e.providers, &input, &fast()).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(v["timed_out"], true);
        assert_eq!(v["status"], "running", "the job is not cancelled or failed");
        assert!(v.get("result").is_none());
        let id = v["job_id"].as_str().unwrap().to_string();
        assert!(
            jobs::try_lock(&e.ctx, &id).unwrap().is_some(),
            "this run released the lock"
        );
        // A later runner (the detached one the CLI spawns) picks it up: no resubmit.
        e.st.lock().unwrap().poll_script.clear();
        let r = run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        assert_eq!(r["status"], "complete");
        assert_eq!((e.st.lock().unwrap().submits, attempts(&e, &id)), (1, 1));
        // Same request_id with a timeout after completion: complete, not timed out.
        let v = generate_wait_with(&e.ctx, &e.providers, &input, &fast()).unwrap();
        assert_eq!(
            (v["status"].as_str(), v["timed_out"].clone()),
            (Some("complete"), json!(false))
        );
        assert!(v["result"].is_object());
        // Bad timeout is refused before any job exists.
        let mut bad = gen_input(Some("bad"));
        bad["wait_timeout_s"] = json!(-3);
        assert_eq!(
            generate_wait_with(&e.ctx, &e.providers, &bad, &fast())
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(job_rows(&e), 1);
    }

    #[test]
    fn generate_wait_timeout_follows_a_runner_elsewhere() {
        let e = env();
        let id = new_job(&e, Some("held"));
        let _held = jobs::try_lock(&e.ctx, &id).unwrap().unwrap();
        let mut input = gen_input(Some("held"));
        input["wait_timeout_s"] = json!(0.2);
        let v = generate_wait_with(&e.ctx, &e.providers, &input, &fast()).unwrap();
        assert_eq!(
            (v["job_id"].as_str(), v["timed_out"].clone()),
            (Some(id.as_str()), json!(true))
        );
        assert_eq!(
            e.st.lock().unwrap().submits,
            0,
            "this process never drove it"
        );
    }

    #[test]
    fn generate_wait_without_timeout_keeps_current_shape() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        let v = generate_wait_with(&e.ctx, &e.providers, &gen_input(None), &fast()).unwrap();
        assert_eq!(v["status"], "complete");
        assert!(v.get("timed_out").is_none());
    }

    #[test]
    fn repeating_generate_after_completion_and_ack_returns_the_finished_job() {
        let e = env();
        e.st.lock().unwrap().sync = true;
        // Cap is exactly used up by the one job: a repeat must not be refused.
        spend_set_cap(&e.ctx, &json!({"cap_usd": 1.0})).unwrap();
        let first = generate_with(&e.ctx, &e.providers, &gen_input(Some("crash")), false).unwrap();
        let id = first["job_id"].as_str().unwrap().to_string();
        run_job_with(&e.ctx, &e.providers, &id, &fast()).unwrap();
        let expected = job_result(&e.ctx, &json!({"job_id": id})).unwrap();

        let again = generate_with(&e.ctx, &e.providers, &gen_input(Some("crash")), false).unwrap();
        assert_eq!(
            (
                again["job_id"].as_str(),
                again["status"].as_str(),
                again["deduplicated"].clone()
            ),
            (Some(id.as_str()), Some("complete"), json!(true))
        );
        assert_eq!(
            job_result(&e.ctx, &json!({"job_id": id})).unwrap(),
            expected
        );
        // generate --wait on the same id returns the result without new work.
        let w =
            generate_wait_with(&e.ctx, &e.providers, &gen_input(Some("crash")), &fast()).unwrap();
        assert_eq!(w["status"], "complete");
        assert_eq!(w["result"], expected);
        assert_eq!(
            (e.st.lock().unwrap().submits, job_rows(&e), ledger_rows(&e)),
            (1, 1, 1)
        );

        // After ack the same holds, and assets stay until cleanup_acked.
        job_ack(&e.ctx, &json!({"job_id": id, "consumer": "cutright"})).unwrap();
        let again = generate_with(&e.ctx, &e.providers, &gen_input(Some("crash")), false).unwrap();
        assert_eq!(
            (again["job_id"].as_str(), again["status"].as_str()),
            (Some(id.as_str()), Some("complete"))
        );
        let res = job_result(&e.ctx, &json!({"job_id": id})).unwrap();
        assert_eq!(res["acked_by"], "cutright");
        let path = res["assets"][0]["path"].as_str().unwrap().to_string();
        assert!(std::path::Path::new(&path).exists());
        assert_eq!(crate::descriptor::cleanup_acked(&e.ctx, 1).unwrap(), 0);
        assert!(std::path::Path::new(&path).exists());
        assert_eq!(crate::descriptor::cleanup_acked(&e.ctx, 0).unwrap(), 1);
        assert!(!std::path::Path::new(&path).exists());
        let again = generate_with(&e.ctx, &e.providers, &gen_input(Some("crash")), false).unwrap();
        assert_eq!(again["job_id"], json!(id));
        assert_eq!(
            job_result(&e.ctx, &json!({"job_id": id})).unwrap_err().code,
            "result_cleaned"
        );
        assert_eq!(e.st.lock().unwrap().submits, 1);
    }

    #[test]
    fn job_result_follows_a_moved_library() {
        let mut v =
            json!({"assets": [{"asset_id": "a1", "path": "/nonexistent/old/library/a1/a1.png"}]});
        let dir = crate::test_temp_dir().join(format!("gr-relocate-{}", std::process::id()));
        let ctx = Context::new(dir.clone());
        std::fs::create_dir_all(ctx.library_dir().join("a1")).unwrap();
        std::fs::write(ctx.library_dir().join("a1").join("a1.png"), b"x").unwrap();
        relocate_asset_paths(&ctx, &mut v);
        assert_eq!(
            v["assets"][0]["path"].as_str().unwrap(),
            ctx.library_dir()
                .join("a1")
                .join("a1.png")
                .to_string_lossy()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
