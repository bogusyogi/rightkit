//! Multi-job steps ("fan-out"): one step that owns several engine jobs, one per segment (today the
//! dub step, one voice job per translated segment).
//!
//! The step record carries a `fanout` object; a step without it is a single-job step and is never
//! touched here. State lives in the run record, not in a window:
//!
//! - `segment_ids`  every segment the step must end up with, in order;
//! - `expected`     the segments this attempt issues, each `{segment_id, request_id, prompt}`; the
//!   request id is `<run>:<step>:a<attempt>:<segment_id>`, so a resubmission after a
//!   crash lands on the same job (the job store dedups on request id);
//! - `jobs`         the attached jobs, `{segment_id, job_id, request_id, state, error, asset}`;
//! - `kept`         completed clips carried over from earlier attempts, `{segment_id, prompt,
//!                  attempt, job_id, asset}`, reused by a retry (the clips are paid for);
//! - `cancelled`    jobs the engine cancelled under the policy, with the engine's note;
//! - `failed`, `held`, `missing`, `awaits_resolution`, `policy`, `synced_at`.
//!
//! Job `state` is one of `attached` (named, not yet observed), `running`, `complete`, `failed`,
//! `cancelled`, `held` (the job store says `outcome_unknown`) or `missing` (the job left the
//! store). The step's own status is derived from them by [`sync`].
use std::collections::HashSet;
use std::path::Path;

use serde_json::{json, Value};

use super::{invalid, strings, tstr};
use crate::{Context, GenError, GenResult};

pub(super) const POLICY: &str = "cancel_rest_on_failure";

pub(super) fn is_fanout(step: &Value) -> bool {
    step.get("fanout").is_some_and(Value::is_object)
}

fn arr(v: &Value, key: &str) -> Vec<Value> {
    v.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn request_id(run_id: &str, step_id: &str, attempt: i64, segment_id: &str) -> String {
    format!("{run_id}:{step_id}:a{attempt}:{segment_id}")
}

fn is_active(state: &str) -> bool {
    matches!(state, "attached" | "running")
}

/// A job that ended without a clip for its segment.
fn is_bad(state: &str) -> bool {
    matches!(state, "failed" | "cancelled" | "missing")
}

fn is_final(state: &str) -> bool {
    matches!(state, "complete" | "failed" | "cancelled" | "missing")
}

// ---------------------------------------------------------------------------
// begin / resume

pub(super) struct Begun {
    /// The per-segment requests to submit (only segments without a kept clip), with their request ids.
    pub requests: Vec<Value>,
    /// Segments whose completed clip from an earlier attempt is reused, not re-issued.
    pub reused: Vec<String>,
    /// Every segment already has a kept clip: the assets the step settles with at once.
    pub all_reused: Option<Vec<Value>>,
}

fn clip_asset(kept: &Value) -> Value {
    let mut a = kept["asset"].clone();
    a["segment_id"] = kept["segment_id"].clone();
    a
}

/// Start (or restart after a retry) a multi-job step at `attempt`. `per_segment` is the dubbing
/// module's full list of `{segment_id, ..., request}`. Segments with a kept clip (same prompt, file
/// still on disk) are not re-issued; the rest get their deterministic request id.
pub(super) fn begin(
    step: &mut Value,
    run_id: &str,
    attempt: i64,
    per_segment: Vec<Value>,
) -> GenResult<Begun> {
    let step_id = tstr(step, "id");
    let mut seen = HashSet::new();
    let mut segment_ids = Vec::new();
    for e in &per_segment {
        let id = tstr(e, "segment_id");
        if id.is_empty() || !seen.insert(id.clone()) {
            return Err(GenError::new(
                "duplicate_segment",
                format!("step {step_id}: the translation names segment '{id}' twice (or blank); segment ids must be unique to track one job each"),
            ));
        }
        segment_ids.push(id);
    }
    let old_kept = arr(&step["fanout"], "kept");
    let mut kept = Vec::new();
    let mut reused = Vec::new();
    let mut requests = Vec::new();
    let mut expected = Vec::new();
    for mut e in per_segment {
        let id = tstr(&e, "segment_id");
        let prompt = tstr(&e["request"], "prompt");
        let hit = old_kept.iter().find(|k| {
            tstr(k, "segment_id") == id
                && tstr(k, "prompt") == prompt
                && Path::new(&tstr(&k["asset"], "path")).is_file()
        });
        if let Some(k) = hit {
            kept.push(k.clone());
            reused.push(id);
            continue;
        }
        let rid = request_id(run_id, &step_id, attempt, &id);
        e["request_id"] = json!(rid);
        e["request"]["request_id"] = json!(rid);
        e["attempt"] = json!(attempt);
        expected.push(json!({ "segment_id": id, "request_id": rid, "prompt": prompt }));
        requests.push(e);
    }
    let all_reused: Option<Vec<Value>> = if requests.is_empty() {
        Some(
            segment_ids
                .iter()
                .filter_map(|id| kept.iter().find(|k| tstr(k, "segment_id") == *id))
                .map(clip_asset)
                .collect(),
        )
    } else {
        None
    };
    step["fanout"] = json!({
        "policy": POLICY,
        "attempt": attempt,
        "segment_ids": segment_ids,
        "expected": expected,
        "jobs": [],
        "kept": kept,
        "cancelled": [],
        "failed": [],
        "held": [],
        "missing": expected.iter().map(|e| e["segment_id"].clone()).collect::<Vec<_>>(),
        "awaits_resolution": false,
    });
    Ok(Begun {
        requests,
        reused,
        all_reused,
    })
}

/// A begun step whose window is gone: the requests of the segments that still have no attached job,
/// at the SAME attempt and request ids (so a job that was submitted but never attached is found, not
/// duplicated). `per_segment` is the dubbing module's full list.
pub(super) fn resume_requests(step: &Value, per_segment: Vec<Value>) -> Vec<Value> {
    let fan = &step["fanout"];
    let attached: HashSet<String> = arr(fan, "jobs")
        .iter()
        .map(|j| tstr(j, "segment_id"))
        .collect();
    let expected = arr(fan, "expected");
    let attempt = fan["attempt"].as_i64().unwrap_or(0);
    per_segment
        .into_iter()
        .filter_map(|mut e| {
            let id = tstr(&e, "segment_id");
            let exp = expected.iter().find(|x| tstr(x, "segment_id") == id)?;
            if attached.contains(&id) {
                return None;
            }
            let rid = tstr(exp, "request_id");
            e["request_id"] = json!(rid);
            e["request"]["request_id"] = json!(rid);
            e["attempt"] = json!(attempt);
            Some(e)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// attach

/// Attach `(job_id, segment_id)` pairs. All-or-nothing: every pair is checked before any is recorded.
/// Re-attaching the same pair is a no-op. Refusals, by name: `unknown_segment`, `segment_job_conflict`,
/// `job_already_attached`, `job_request_mismatch`.
pub(super) fn attach(
    ctx: &Context,
    step: &mut Value,
    entries: &[(String, String)],
) -> GenResult<Value> {
    let step_id = tstr(step, "id");
    let status = tstr(step, "status");
    if status != "awaiting_submit" && status != "running" {
        return Err(invalid(format!(
            "step {step_id} is {status}, expected awaiting_submit or running"
        )));
    }
    if entries.is_empty() {
        return Err(invalid("missing field: job_id"));
    }
    let fan = step["fanout"].clone();
    let expected = arr(&fan, "expected");
    let mut jobs = arr(&fan, "jobs");
    let mut added: Vec<String> = Vec::new();
    let mut already: Vec<String> = Vec::new();
    for (job_id, seg) in entries {
        let Some(exp) = expected.iter().find(|e| tstr(e, "segment_id") == *seg) else {
            let kept = arr(&fan, "kept")
                .iter()
                .any(|k| tstr(k, "segment_id") == *seg);
            let names = expected
                .iter()
                .map(|e| tstr(e, "segment_id"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(GenError::new(
                "unknown_segment",
                if kept {
                    format!("step {step_id}: segment '{seg}' already has a completed clip kept from an earlier attempt; this attempt does not issue it (it issues: {names})")
                } else {
                    format!("step {step_id} has no segment '{seg}' in this attempt (it issues: {names})")
                },
            ));
        };
        if let Some(j) = jobs.iter().find(|j| tstr(j, "segment_id") == *seg) {
            if tstr(j, "job_id") == *job_id {
                already.push(seg.clone());
                continue;
            }
            return Err(GenError::new(
                "segment_job_conflict",
                format!("step {step_id}: segment '{seg}' already has job {}; refusing a second job {job_id} for it", tstr(j, "job_id")),
            ));
        }
        if let Some(j) = jobs.iter().find(|j| tstr(j, "job_id") == *job_id) {
            return Err(GenError::new(
                "job_already_attached",
                format!(
                    "step {step_id}: job {job_id} is already attached to segment '{}'",
                    tstr(j, "segment_id")
                ),
            ));
        }
        let want = tstr(exp, "request_id");
        match rightkit_media_gen::api::job_status(ctx, &json!({ "job_id": job_id })) {
            Ok(j) => {
                if let Some(rid) = j["request_id"].as_str().filter(|r| !r.is_empty()) {
                    if rid != want {
                        return Err(GenError::new(
                            "job_request_mismatch",
                            format!("step {step_id}: job {job_id} was submitted under request id '{rid}', not '{want}' for segment '{seg}'"),
                        ));
                    }
                }
            }
            // Attached before the engine knows it (or after it was pruned): kept, reported `missing` by sync.
            Err(e) if e.code == "job_not_found" => {}
            Err(e) => return Err(e),
        }
        jobs.push(json!({ "segment_id": seg, "job_id": job_id, "request_id": want, "state": "attached", "error": Value::Null, "asset": Value::Null }));
        added.push(seg.clone());
    }
    step["fanout"]["jobs"] = json!(jobs);
    let missing = missing_of(&step["fanout"]);
    step["fanout"]["missing"] = json!(missing);
    if status == "awaiting_submit" && !jobs.is_empty() {
        step["status"] = json!("running");
    }
    Ok(
        json!({ "step_id": step_id, "attached": added, "already_attached": already, "missing_segments": missing }),
    )
}

fn missing_of(fan: &Value) -> Vec<String> {
    let attached: HashSet<String> = arr(fan, "jobs")
        .iter()
        .map(|j| tstr(j, "segment_id"))
        .collect();
    arr(fan, "expected")
        .iter()
        .map(|e| tstr(e, "segment_id"))
        .filter(|id| !attached.contains(id))
        .collect()
}

// ---------------------------------------------------------------------------
// observe jobs

struct Obs {
    state: &'static str,
    error: Option<String>,
    asset: Option<Value>,
}

fn job_error(j: &Value) -> Option<String> {
    let code = j["error"]["code"].as_str()?;
    Some(format!(
        "{code}: {}",
        j["error"]["message"].as_str().unwrap_or("")
    ))
}

/// The job store's view of one job. A store error other than "no such job" is returned, never turned
/// into a step failure: a database hiccup must not lose track of a paid job.
fn observe(ctx: &Context, job_id: &str, produces: &str) -> GenResult<Obs> {
    let j = match rightkit_media_gen::api::job_status(ctx, &json!({ "job_id": job_id })) {
        Ok(j) => j,
        Err(e) if e.code == "job_not_found" => {
            return Ok(Obs {
                state: "missing",
                error: Some("job_not_found: the job is not in the job store".into()),
                asset: None,
            })
        }
        Err(e) => return Err(e),
    };
    Ok(match j["status"].as_str().unwrap_or("") {
        "complete" => {
            match rightkit_media_gen::api::job_result(ctx, &json!({ "job_id": job_id })) {
                Ok(r) => match r["assets"].as_array().and_then(|a| a.first()) {
                    Some(a) => Obs {
                        state: "complete",
                        error: None,
                        asset: Some(json!({
                            "asset_id": a["asset_id"], "path": a["path"], "sha256": a["sha256"],
                            "bytes": a["bytes"], "mime": a["mime"], "kind": produces,
                            "duration_s": a["media"]["duration_s"].clone(),
                        })),
                    },
                    None => Obs {
                        state: "failed",
                        error: Some("the job completed without an asset".into()),
                        asset: None,
                    },
                },
                Err(e) if e.code == "result_cleaned" => Obs {
                    state: "failed",
                    error: Some(
                        "result_cleaned: the job's result was cleaned up before the step read it"
                            .into(),
                    ),
                    asset: None,
                },
                Err(e) => return Err(e),
            }
        }
        "failed" => Obs {
            state: "failed",
            error: Some(job_error(&j).unwrap_or_else(|| "failed".into())),
            asset: None,
        },
        "cancelled" => Obs {
            state: "cancelled",
            error: Some("cancelled".into()),
            asset: None,
        },
        "outcome_unknown" => Obs {
            state: "held",
            error: Some(job_error(&j).unwrap_or_else(|| {
                "outcome_unknown: resolve the job (job resolve) before this step can finish".into()
            })),
            asset: None,
        },
        _ => Obs {
            state: "running",
            error: None,
            asset: None,
        },
    })
}

fn apply(j: &mut Value, o: Obs) {
    j["state"] = json!(o.state);
    j["error"] = o.error.map(Value::from).unwrap_or(Value::Null);
    if let Some(a) = o.asset {
        j["asset"] = a;
    }
}

/// Cancel one job at the job store and read back what it became (it may have finished first).
///
/// A cancel may reach the provider, so it never runs while the run's lock is held. Under the lock
/// the caller passes [`Cancels`]: a job whose cancel is already done (`done`) is read back with its
/// recorded note; any other is only collected into `wanted` and left as it was, and the caller
/// discards that pass, cancels the wanted jobs outside the lock and runs the operation again on a
/// freshly loaded record (see `mutate_run` in the parent module).
fn cancel_and_observe(
    ctx: &Context,
    cancels: &mut Cancels,
    job_id: &str,
    produces: &str,
) -> GenResult<(Obs, String)> {
    let Some(note) = cancels.done.get(job_id).cloned() else {
        if !cancels.wanted.iter().any(|w| w == job_id) {
            cancels.wanted.push(job_id.to_string());
        }
        return Ok((
            Obs {
                state: "running",
                error: None,
                asset: None,
            },
            "pending".to_string(),
        ));
    };
    Ok((observe(ctx, job_id, produces)?, note))
}

/// Job cancels for one operation on a run record: `wanted` is filled by a pass that found jobs to
/// cancel; `done` maps a job id to the note of its finished cancel.
#[derive(Default)]
pub(super) struct Cancels {
    pub wanted: Vec<String>,
    pub done: std::collections::HashMap<String, String>,
}

/// The one place a multi-job step's job is cancelled at the job store (and so possibly the provider).
/// Called with no run lock held. The note is `cancelled` or the engine's `code: message`.
pub(super) fn cancel_job_note(ctx: &Context, job_id: &str) -> String {
    match rightkit_media_gen::api::job_cancel(ctx, &json!({ "job_id": job_id })) {
        Ok(_) => "cancelled".to_string(),
        Err(e) => format!("{}: {}", e.code, e.message),
    }
}

fn produces_of(step: &Value) -> String {
    let p = tstr(step, "produces");
    if p.is_empty() {
        "audio".into()
    } else {
        p
    }
}

/// Merge the completed jobs' clips into `kept` (an earlier kept clip for a segment wins).
fn harvest(fan: &Value) -> Vec<Value> {
    let mut kept = arr(fan, "kept");
    let expected = arr(fan, "expected");
    let attempt = fan["attempt"].clone();
    for j in arr(fan, "jobs") {
        let seg = tstr(&j, "segment_id");
        if tstr(&j, "state") != "complete"
            || j["asset"].is_null()
            || kept.iter().any(|k| tstr(k, "segment_id") == seg)
        {
            continue;
        }
        let prompt = expected
            .iter()
            .find(|e| tstr(e, "segment_id") == seg)
            .map(|e| tstr(e, "prompt"))
            .unwrap_or_default();
        kept.push(json!({ "segment_id": seg, "prompt": prompt, "attempt": attempt, "job_id": j["job_id"], "asset": j["asset"] }));
    }
    kept
}

// ---------------------------------------------------------------------------
// sync: derive the step's state from its jobs

/// Find jobs that were submitted under an expected request id but never attached (a crash between
/// submit and attach) and attach them. Lookup is by the deterministic request id only.
fn adopt(ctx: &Context, fan: &mut Value, produces: &str) -> GenResult<()> {
    let mut jobs = arr(fan, "jobs");
    let attached: HashSet<String> = jobs.iter().map(|j| tstr(j, "segment_id")).collect();
    for e in arr(fan, "expected") {
        let seg = tstr(&e, "segment_id");
        if attached.contains(&seg) {
            continue;
        }
        let rid = tstr(&e, "request_id");
        match rightkit_media_gen::api::job_status(ctx, &json!({ "request_id": rid })) {
            Ok(j) => {
                let job_id = tstr(&j, "job_id");
                if job_id.is_empty() || jobs.iter().any(|x| tstr(x, "job_id") == job_id) {
                    continue;
                }
                let mut row = json!({ "segment_id": seg, "job_id": job_id, "request_id": rid, "state": "attached", "error": Value::Null, "asset": Value::Null, "adopted": true });
                apply(&mut row, observe(ctx, &job_id, produces)?);
                jobs.push(row);
            }
            Err(e) if e.code == "job_not_found" => {}
            Err(e) => return Err(e),
        }
    }
    fan["jobs"] = json!(jobs);
    Ok(())
}

/// Observe every non-final job, then derive the step. Rules, in order:
///
/// 1. a job that failed, was cancelled by someone else, or left the store is a failed segment;
/// 2. on any failed segment the engine cancels the other live jobs (policy `cancel_rest_on_failure`);
///    completed clips stay recorded in `kept`; a held (`outcome_unknown`) job cannot be cancelled;
/// 3. the step is `failed` once a segment failed and no job is live or held, naming each failed segment;
/// 4. held jobs keep the step `running` with `awaits_resolution: true` (even if another segment failed:
///    failing the step would let a retry resubmit the held segment blindly);
/// 5. the step is `succeeded` only when every expected segment has an attached, completed job with an
///    asset; its assets are one clip per segment (kept ones included) in segment order, each with `segment_id`;
/// 6. otherwise it stays `running` (jobs live, or segments not attached yet: see `missing`).
///
/// A store error aborts without changing the step. The caller recomputes spend, refreshes and saves.
pub(super) fn sync(ctx: &Context, step: &mut Value, cancels: &mut Cancels) -> GenResult<()> {
    let status = tstr(step, "status");
    if status != "running" && status != "awaiting_submit" {
        return Ok(());
    }
    let produces = produces_of(step);
    let mut fan = step["fanout"].clone();
    adopt(ctx, &mut fan, &produces)?;
    let mut jobs = arr(&fan, "jobs");
    for j in jobs.iter_mut() {
        if is_final(&tstr(j, "state")) {
            continue;
        }
        let o = observe(ctx, &tstr(j, "job_id"), &produces)?;
        apply(j, o);
    }
    let mut cancelled_log = arr(&fan, "cancelled");
    let was_cancelled = |log: &Vec<Value>, id: &str| log.iter().any(|c| tstr(c, "job_id") == id);
    let any_bad = jobs
        .iter()
        .any(|j| is_bad(&tstr(j, "state")) && !was_cancelled(&cancelled_log, &tstr(j, "job_id")));
    if any_bad {
        for j in jobs.iter_mut() {
            if !is_active(&tstr(j, "state")) {
                continue;
            }
            let job_id = tstr(j, "job_id");
            let (o, note) = cancel_and_observe(ctx, cancels, &job_id, &produces)?;
            if o.state == "cancelled" {
                cancelled_log
                    .push(json!({ "segment_id": j["segment_id"], "job_id": job_id, "note": note }));
            } else if o.state == "held" {
                cancelled_log.push(json!({ "segment_id": j["segment_id"], "job_id": job_id, "note": format!("not cancelled: {note}") }));
            }
            apply(j, o);
        }
    }
    fan["jobs"] = json!(jobs);
    fan["cancelled"] = json!(cancelled_log);
    let state_of = |j: &Value| tstr(j, "state");
    let failed: Vec<Value> = jobs
        .iter()
        .filter(|j| is_bad(&state_of(j)) && !was_cancelled(&cancelled_log, &tstr(j, "job_id")))
        .map(|j| json!({ "segment_id": j["segment_id"], "job_id": j["job_id"], "error": j["error"] }))
        .collect();
    let held: Vec<Value> = jobs
        .iter()
        .filter(|j| state_of(j) == "held")
        .map(|j| json!({ "segment_id": j["segment_id"], "job_id": j["job_id"] }))
        .collect();
    let live = jobs.iter().any(|j| is_active(&state_of(j)));
    let missing = missing_of(&fan);
    fan["failed"] = json!(failed);
    fan["held"] = json!(held);
    fan["missing"] = json!(missing);
    fan["awaits_resolution"] = json!(!held.is_empty());
    fan["synced_at"] = json!(rightkit_media_gen::store::now());
    let now = rightkit_media_gen::store::now();
    if !failed.is_empty() && !live && held.is_empty() {
        let kept = harvest(&fan);
        let mut msg = failed
            .iter()
            .map(|f| {
                format!(
                    "segment {}: {}",
                    tstr(f, "segment_id"),
                    f["error"].as_str().unwrap_or("failed")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let stopped: Vec<String> = cancelled_log
            .iter()
            .filter(|c| tstr(c, "note") == "cancelled")
            .map(|c| tstr(c, "segment_id"))
            .collect();
        if !stopped.is_empty() {
            msg.push_str(&format!(
                "; cancelled the other live job(s): segment {}",
                stopped.join(", ")
            ));
        }
        if !kept.is_empty() {
            msg.push_str(&format!(
                "; kept {} completed clip(s) (segment {}) for a retry of only the rest",
                kept.len(),
                kept.iter()
                    .map(|k| tstr(k, "segment_id"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        fan["kept"] = json!(kept);
        step["status"] = json!("failed");
        step["error"] = json!(msg);
        step["finished_at"] = json!(now);
    } else if failed.is_empty()
        && !live
        && held.is_empty()
        && missing.is_empty()
        && !jobs.is_empty()
    {
        // Every expected segment is attached and every job completed with an asset.
        let kept = harvest(&fan);
        let assets: Vec<Value> = arr(&fan, "segment_ids")
            .iter()
            .filter_map(|id| {
                kept.iter()
                    .find(|k| tstr(k, "segment_id") == id.as_str().unwrap_or(""))
            })
            .map(clip_asset)
            .collect();
        if assets.len() == arr(&fan, "segment_ids").len() {
            fan["kept"] = json!(kept);
            step["assets"] = json!(assets);
            step["status"] = json!("succeeded");
            step["error"] = Value::Null;
            step["finished_at"] = json!(now);
        }
    }
    if tstr(step, "status") == "awaiting_submit" && !jobs.is_empty() {
        step["status"] = json!("running");
    }
    step["fanout"] = fan;
    Ok(())
}

// ---------------------------------------------------------------------------
// cancel / retry

pub(super) struct Cancelled {
    pub cancelled: Vec<String>,
    pub held: Vec<String>,
    pub still_live: Vec<String>,
    pub kept_segments: Vec<String>,
}

/// Cancel every attached job that is still live (policy: all of them). Completed jobs are not touched;
/// their clips are kept. A held job cannot be cancelled and is returned in `held`.
pub(super) fn cancel(
    ctx: &Context,
    step: &mut Value,
    cancels: &mut Cancels,
) -> GenResult<Cancelled> {
    let produces = produces_of(step);
    let mut fan = step["fanout"].clone();
    adopt(ctx, &mut fan, &produces)?;
    let mut jobs = arr(&fan, "jobs");
    let mut log = arr(&fan, "cancelled");
    for j in jobs.iter_mut() {
        if is_final(&tstr(j, "state")) {
            continue;
        }
        let job_id = tstr(j, "job_id");
        let o = observe(ctx, &job_id, &produces)?;
        apply(j, o);
        if !is_active(&tstr(j, "state")) {
            continue;
        }
        let (o, note) = cancel_and_observe(ctx, cancels, &job_id, &produces)?;
        if matches!(o.state, "cancelled" | "held") {
            let note = if o.state == "held" {
                format!("not cancelled: {note}")
            } else {
                note
            };
            log.push(json!({ "segment_id": j["segment_id"], "job_id": job_id, "note": note }));
        }
        apply(j, o);
    }
    let by = |state: &str| -> Vec<String> {
        jobs.iter()
            .filter(|j| tstr(j, "state") == state)
            .map(|j| tstr(j, "job_id"))
            .collect()
    };
    let mut out = Cancelled {
        cancelled: log
            .iter()
            .filter(|c| tstr(c, "note") == "cancelled")
            .map(|c| tstr(c, "job_id"))
            .collect(),
        held: by("held"),
        still_live: jobs
            .iter()
            .filter(|j| is_active(&tstr(j, "state")))
            .map(|j| tstr(j, "job_id"))
            .collect(),
        kept_segments: Vec::new(),
    };
    fan["jobs"] = json!(jobs);
    fan["cancelled"] = json!(log);
    let kept = harvest(&fan);
    out.kept_segments = kept.iter().map(|k| tstr(k, "segment_id")).collect();
    fan["kept"] = json!(kept);
    fan["held"] = json!(jobs
        .iter()
        .filter(|j| tstr(j, "state") == "held")
        .map(|j| json!({ "segment_id": j["segment_id"], "job_id": j["job_id"] }))
        .collect::<Vec<_>>());
    fan["awaits_resolution"] = json!(!out.held.is_empty());
    fan["missing"] = json!(missing_of(&fan));
    fan["synced_at"] = json!(rightkit_media_gen::store::now());
    step["fanout"] = fan;
    Ok(out)
}

/// Before a retry: stop anything still live, bank completed clips, refuse while a job is held, then
/// clear the attempt's jobs so `begin` re-issues only the segments without a kept clip.
pub(super) fn prepare_retry(
    ctx: &Context,
    step: &mut Value,
    cancels: &mut Cancels,
) -> GenResult<()> {
    let step_id = tstr(step, "id");
    let c = cancel(ctx, step, cancels)?;
    if !c.held.is_empty() {
        return Err(GenError::new(
            "awaits_resolution",
            format!("step {step_id}: job(s) {} are held as outcome_unknown and may be running at the provider; resolve them (job resolve) before retrying", c.held.join(", ")),
        ));
    }
    if !c.still_live.is_empty() {
        return Err(GenError::new(
            "job_still_live",
            format!(
                "step {step_id}: job(s) {} could not be cancelled; retry after they end",
                c.still_live.join(", ")
            ),
        ));
    }
    let kept = arr(&step["fanout"], "kept");
    step["fanout"]["jobs"] = json!([]);
    step["fanout"]["expected"] = json!([]);
    step["fanout"]["cancelled"] = json!([]);
    step["fanout"]["failed"] = json!([]);
    step["fanout"]["held"] = json!([]);
    step["fanout"]["missing"] = json!([]);
    step["fanout"]["awaits_resolution"] = json!(false);
    step["fanout"]["kept"] = json!(kept);
    Ok(())
}

// ---------------------------------------------------------------------------
// report

/// Per-segment view of a multi-job step as last synced: one row per segment of the step, with its
/// request id, job id and state (`not_attached` when no job is attached yet, `kept` for a clip carried
/// over from an earlier attempt).
pub(super) fn report(step: &Value) -> Value {
    let fan = &step["fanout"];
    let jobs = arr(fan, "jobs");
    let expected = arr(fan, "expected");
    let kept = arr(fan, "kept");
    let mut rows = Vec::new();
    for id in strings(fan, "segment_ids") {
        if let Some(j) = jobs.iter().find(|j| tstr(j, "segment_id") == id) {
            rows.push(json!({
                "segment_id": id, "request_id": j["request_id"], "job_id": j["job_id"], "state": j["state"],
                "error": j["error"], "asset_id": j["asset"]["asset_id"],
            }));
        } else if let Some(e) = expected.iter().find(|e| tstr(e, "segment_id") == id) {
            rows.push(json!({ "segment_id": id, "request_id": e["request_id"], "job_id": Value::Null, "state": "not_attached", "error": Value::Null, "asset_id": Value::Null }));
        } else if let Some(k) = kept.iter().find(|k| tstr(k, "segment_id") == id) {
            rows.push(json!({ "segment_id": id, "request_id": Value::Null, "job_id": k["job_id"], "state": "kept", "error": Value::Null, "asset_id": k["asset"]["asset_id"], "attempt": k["attempt"] }));
        }
    }
    json!({
        "step_id": step["id"], "fanout": true, "status": step["status"], "attempt": step["attempt"], "error": step["error"],
        "policy": fan["policy"], "awaits_resolution": fan["awaits_resolution"],
        "segments": rows, "missing": fan["missing"], "failed": fan["failed"], "held": fan["held"],
        "cancelled": fan["cancelled"], "synced_at": fan["synced_at"],
    })
}
