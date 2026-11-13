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
        format!(
            "{} has no cancel endpoint {} can call",
            job.provider,
            crate::host::app_or("the host")
        )
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
