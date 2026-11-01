//! Fake `genright-engine` for tests. Emulates GenRight's contract-v2 CLI
//! (docs/ENGINE-CLI.md): doctor with per-command capabilities, generate with
//! request_id dedup, job status (live record with `progress`, `events` on
//! request, `--request-id` with `job_not_found`), job result (assets[] only,
//! per-asset provider/model, probed media), job list
//! `{request_id,status,limit}` with `truncated`, job ack, asset resolve, and
//! the persisted `job.progress` log. Never contacts a provider. State lives
//! in a JSON file under `$FAKE_GENRIGHT_DIR`; a moved library is found under
//! `$FAKE_GENRIGHT_DIR/moved/<asset_id>/`.
//!
//! Knobs (environment): FAKE_DOCTOR=ok|v1|v3 (contract version 1/3 report),
//! FAKE_HIDE=`<capability names to drop from doctor>`, FAKE_LIST_CAP=`<max rows
//! job list returns, truncated is reported>`, FAKE_NULL_MEDIA=1 (the probe
//! returns nulls), FAKE_CRASH_AFTER_GENERATE=1 (record the job, then exit
//! without printing), FAKE_STEPS=`<polls to complete>`, FAKE_BAD_SHA=1,
//! FAKE_DURATIONS=1 (advertise `models.durations` v1 and emit structured
//! duration fields from `models`; without it `models` has labels only, like
//! an engine before c1441e7), FAKE_STDERR=1 (a `job.progress` line on stderr of `job status`),
//! FAKE_ACK_FAIL=1 (ack errors), FAKE_CLEAN_ON_ACK=1 (delete library files
//! on ack, as cleanup would), FAKE_PROJECT=`<dir>` (on ack, record whether that
//! project's import journal already lists the job), FAKE_LEGACY_STDIN=1
//! (the pre-58860e4 behaviour: read stdin whenever it is not a terminal and no
//! `--input` was given, so an open unwritten pipe hangs the call).
//!
//! Stdin: like the real engine, stdin is read ONLY with `--input -`.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use rightkit_fs::digest::sha256_bytes;
use serde_json::{json, Value};

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("FAKE_GENRIGHT_DIR").expect("FAKE_GENRIGHT_DIR"))
}

/// Missing state means a fresh engine. A state file that exists but does not
/// parse is a hard error: silently resetting it would drop jobs and acks.
fn load() -> Value {
    let path = dir().join("state.json");
    match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b)
            .unwrap_or_else(|e| panic!("corrupt fake state {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({"jobs": [], "generate_calls": 0, "ack_log": []})
        }
        Err(e) => panic!("unreadable fake state {}: {e}", path.display()),
    }
}

/// Write to a temp file and rename, so a reader never sees a partial state.
fn save(state: &Value) {
    std::fs::create_dir_all(dir()).unwrap();
    let tmp = dir().join(format!("state.json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(state).unwrap()).unwrap();
    std::fs::rename(&tmp, dir().join("state.json")).unwrap();
}

fn out(v: Value) -> ! {
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
    std::process::exit(0);
}

fn fail(code: &str, message: &str) -> ! {
    println!("{}", json!({"error": {"code": code, "message": message}}));
    std::process::exit(1);
}

fn env_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

fn job_view(j: &Value) -> Value {
    json!({
        "job_id": j["job_id"], "request_id": j["request_id"], "kind": j["kind"],
        "provider": j["provider"], "model": j["model"],
        "request": {"kind": j["kind"], "provider": j["provider"], "model": j["model"],
            "prompt": j["prompt"], "params": null},
        "status": j["status"],
        "provider_job_id": null, "attempts": 1,
        "est_usd": j.get("est_usd").cloned().unwrap_or(json!(0.0)),
        "actual_usd": j.get("actual_usd").cloned().unwrap_or(Value::Null),
        "error": j["error"], "acked_by": j["acked_by"], "acked_at": j["acked_at"],
        "created_at": "2026-10-05T00:00:00Z", "updated_at": "2026-10-05T00:00:00Z",
    })
}

fn main() {
    let mut words = Vec::new();
    let mut input_arg = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dry-run" => {}
            "--input" => input_arg = args.next(),
            _ => words.push(a),
        }
    }
    let mut raw = input_arg.unwrap_or_default();
    if raw == "-" {
        raw.clear();
        let _ = std::io::stdin().read_to_string(&mut raw);
    } else if raw.is_empty() && env_on("FAKE_LEGACY_STDIN") && !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_string(&mut raw);
    }
    let input: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw).unwrap_or_else(|_| fail("bad_input", "input is not JSON"))
    };
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["doctor"] => doctor(),
        ["models"] => out(models()),
        ["generate", kind] => generate(kind, &input),
        ["job", "status", "--request-id", rid] => status_by_request(
            input["request_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(rid),
        ),
        ["job", "status", id] => job_status(id, &input),
        ["job", "result", id] => job_result(id),
        ["asset", "resolve", id] => asset_resolve(id),
        ["job", "list"] => job_list(&input),
        ["job", "ack", id] => job_ack(id, &input),
        ["job", "cancel", id] => job_cancel(id),
        _ => fail(
            "unknown_command",
            &format!("unknown command: {}", w.join(" ")),
        ),
    }
}

fn doctor() -> ! {
    let mode = std::env::var("FAKE_DOCTOR").unwrap_or_else(|_| "ok".into());
    let mut caps: Vec<&str> = vec![
        "doctor",
        "sidecar.stdio",
        "artifacts.index",
        "progress.ndjson",
    ];
    let commands = [
        "apps",
        "models",
        "credentials.status",
        "credentials.set",
        "credentials.test",
        "generate",
        "job.run",
        "job.status",
        "job.result",
        "job.list",
        "job.ack",
        "job.cancel",
        "asset.resolve",
        "spend.status",
        "spend.set_cap",
    ];
    caps.extend(commands);
    if env_on("FAKE_DURATIONS") {
        caps.push("models.durations");
    }
    let hidden = std::env::var("FAKE_HIDE").unwrap_or_default();
    caps.retain(|n| !hidden.split(',').any(|h| h == *n));
    out(json!({
        "app": "GenRight", "engine": "genright-engine", "app_version": "0.0.0-fake",
        "contract_version": match mode.as_str() { "v1" => 1, "v3" => 3, _ => 2 },
        "capabilities": caps.iter().map(|n| json!({"name": n, "version": 1})).collect::<Vec<_>>(),
        "commands": commands,
        "readiness": {},
    }))
}

fn generate(kind: &str, input: &Value) -> ! {
    if !["image", "video", "voice"].contains(&kind) {
        fail("invalid_input", "bad kind");
    }
    let prompt = input["prompt"].as_str().unwrap_or("");
    if prompt.trim().is_empty() {
        fail("invalid_input", "prompt must not be empty");
    }
    if input["model"].as_str() == Some("nope") {
        fail("unknown_model", "no such model");
    }
    let mut state = load();
    state["generate_calls"] = json!(state["generate_calls"].as_i64().unwrap_or(0) + 1);
    let rid = input["request_id"].as_str().filter(|s| !s.is_empty());
    if let Some(r) = rid {
        if let Some(j) = state["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["request_id"] == r)
        {
            let reply = json!({"job_id": j["job_id"], "status": j["status"], "est_usd": 0.0,
                "request_id": r, "deduplicated": true});
            save(&state);
            out(reply);
        }
    }
    let n = state["jobs"].as_array().unwrap().len() + 1;
    let job = json!({
        "job_id": format!("job-{n}"), "request_id": rid, "kind": kind,
        "provider": input["provider"], "model": input["model"], "prompt": prompt,
        "status": "submitted", "polls": 0, "acked_by": null, "error": null, "result": null,
        "events": [],
    });
    state["jobs"].as_array_mut().unwrap().push(job.clone());
    save(&state);
    if env_on("FAKE_CRASH_AFTER_GENERATE") {
        std::process::exit(9);
    }
    out(
        json!({"job_id": job["job_id"], "status": "submitted", "est_usd": 0.0,
        "request_id": rid, "deduplicated": false}),
    )
}

fn find<'a>(state: &'a mut Value, id: &str) -> &'a mut Value {
    match state["jobs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|j| j["job_id"] == id)
    {
        Some(j) => j,
        None => fail("not_found", &format!("no such job: {id}")),
    }
}

fn complete(job: &mut Value) {
    let kind = job["kind"].as_str().unwrap().to_string();
    let ext = match kind.as_str() {
        "image" => "png",
        "video" => "mp4",
        _ => "wav",
    };
    let asset_id = format!("asset-{}", job["job_id"].as_str().unwrap());
    let data = format!("fake-{kind}-{}", job["prompt"].as_str().unwrap())
        .repeat(50)
        .into_bytes();
    let d = dir().join("library").join(&asset_id);
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join(format!("{asset_id}.{ext}"));
    std::fs::write(&path, &data).unwrap();
    let sha = if env_on("FAKE_BAD_SHA") {
        "0".repeat(64)
    } else {
        sha256_bytes(&data)
    };
    let mime = format!(
        "{}/x-fake",
        if kind == "voice" {
            "audio"
        } else {
            kind.as_str()
        }
    );
    let media = if env_on("FAKE_NULL_MEDIA") {
        json!({"kind": kind, "width": null, "height": null, "duration_s": null})
    } else {
        match kind.as_str() {
            "image" => json!({"kind": kind, "width": 1280, "height": 720, "duration_s": null}),
            "video" => json!({"kind": kind, "width": 1920, "height": 1080, "duration_s": 5.5}),
            _ => json!({"kind": kind, "width": null, "height": null, "duration_s": 2.25}),
        }
    };
    let asset = json!({
        "asset_id": asset_id, "path": path.to_string_lossy(), "sha256": sha,
        "bytes": data.len(), "mime": mime, "media": media,
        "provider": format!("asset-{}", job["provider"].as_str().unwrap_or("")),
        "model": format!("asset-{}", job["model"].as_str().unwrap_or("")),
    });
    // Contract v2: everything is under assets[], no top-level flattening.
    let desc = json!({"job_id": job["job_id"], "request_id": job["request_id"],
        "provider": job["provider"], "model": job["model"], "kind": kind,
        "created_at": "2026-10-05T00:00:00Z", "acked_by": null, "acked_at": null,
        "assets": [asset]});
    job["result"] = desc;
    job["status"] = json!("complete");
    job["est_usd"] = json!(0.02);
    job["actual_usd"] = json!(0.021);
}

fn status_by_request(rid: &str) -> ! {
    let state = load();
    match state["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["request_id"] == rid)
    {
        Some(j) => out(job_view(j)),
        None => fail("job_not_found", &format!("no job for request {rid}")),
    }
}

fn progress(job: &Value, stage: &str, polls: i64) -> Value {
    json!({"event": "job.progress", "job_id": job["job_id"], "status": job["status"],
        "stage": stage, "attempt": 1, "at": format!("2026-10-05T00:00:{polls:02}Z")})
}

fn job_status(id: &str, input: &Value) -> ! {
    let mut state = load();
    let steps = std::env::var("FAKE_STEPS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(2);
    let job = find(&mut state, id);
    if !["complete", "cancelled", "failed"].contains(&job["status"].as_str().unwrap_or("")) {
        let polls = job["polls"].as_i64().unwrap() + 1;
        job["polls"] = json!(polls);
        job["status"] = json!("running");
        let mut stage = "polling";
        if polls >= steps {
            complete(job);
            stage = "complete";
        }
        let ev = progress(job, stage, polls);
        job["events"].as_array_mut().unwrap().push(ev);
    }
    let mut view = job_view(job);
    view["progress"] = job["events"]
        .as_array()
        .and_then(|e| e.last().cloned())
        .unwrap_or(Value::Null);
    if input["events"] == true {
        view["events"] = job["events"].clone();
    }
    save(&state);
    if env_on("FAKE_STDERR") {
        eprintln!("{}", progress(&view, "polling", 0));
    }
    out(view)
}

fn job_list(input: &Value) -> ! {
    let state = load();
    let mut jobs: Vec<&Value> = state["jobs"].as_array().unwrap().iter().rev().collect();
    let cap = std::env::var("FAKE_LIST_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(500);
    if let Some(r) = input["request_id"].as_str() {
        jobs.retain(|j| j["request_id"] == r);
    }
    if let Some(s) = input["status"].as_str() {
        jobs.retain(|j| j["status"] == s);
    }
    let limit = (input["limit"].as_u64().unwrap_or(50) as usize).clamp(1, 500.min(cap));
    let truncated = jobs.len() > limit;
    jobs.truncate(limit);
    out(json!({"jobs": jobs.into_iter().map(job_view).collect::<Vec<_>>(), "truncated": truncated}))
}

fn asset_resolve(id: &str) -> ! {
    let state = load();
    for j in state["jobs"].as_array().unwrap() {
        let Some(a) = j["result"]["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["asset_id"] == id))
        else {
            continue;
        };
        let original = PathBuf::from(a["path"].as_str().unwrap());
        let moved = dir()
            .join("moved")
            .join(id)
            .join(original.file_name().unwrap());
        let (path, exists) = if original.exists() {
            (original, true)
        } else if moved.exists() {
            (moved, true)
        } else {
            (original, false)
        };
        out(
            json!({"asset_id": id, "job_id": j["job_id"], "path": path.to_string_lossy(),
            "sha256": a["sha256"], "bytes": a["bytes"], "mime": a["mime"], "exists": exists}),
        );
    }
    fail("asset_not_found", &format!("no such asset: {id}"))
}

fn job_result(id: &str) -> ! {
    let mut state = load();
    let job = find(&mut state, id);
    match job["status"].as_str().unwrap() {
        "complete" if job["result"].is_null() => fail(
            "result_cleaned",
            "result was cleaned up after acknowledgement",
        ),
        "complete" => out(job["result"].clone()),
        s => fail("not_ready", &format!("job is {s}")),
    }
}

/// `job cancel`: a running job goes cancelled; a complete job stays complete
/// and keeps its assets (contract: cancel never removes finished results).
fn job_cancel(id: &str) -> ! {
    let mut state = load();
    let job = find(&mut state, id);
    match job["status"].as_str().unwrap_or("") {
        "complete" | "cancelled" => {}
        _ => {
            job["status"] = json!("cancelled");
            job["error"] = json!({"code": "cancelled", "message": "cancelled by caller"});
            let ev = progress(job, "failed", job["polls"].as_i64().unwrap_or(0));
            job["events"].as_array_mut().unwrap().push(ev);
        }
    }
    let view = job_view(job);
    save(&state);
    out(view)
}

fn job_ack(id: &str, input: &Value) -> ! {
    let consumer = input["consumer"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fail("invalid_input", "missing field: consumer"));
    if env_on("FAKE_ACK_FAIL") {
        fail("db", "database is locked");
    }
    let mut state = load();
    let saw_journal = std::env::var("FAKE_PROJECT").ok().is_some_and(|p| {
        std::fs::read_to_string(PathBuf::from(p).join("imports").join("journal.json"))
            .is_ok_and(|t| t.contains(&format!("\"{id}:")))
    });
    let job = find(&mut state, id);
    if job["status"] != "complete" {
        let s = job["status"].as_str().unwrap().to_string();
        fail(
            "not_complete",
            &format!("job {id} is {s}, only complete jobs can be acknowledged"),
        );
    }
    job["acked_by"] = json!(consumer);
    job["acked_at"] = json!("2026-10-05T00:01:00Z");
    if env_on("FAKE_CLEAN_ON_ACK") {
        if let Some(assets) = job["result"]["assets"].as_array() {
            for a in assets {
                let _ = std::fs::remove_dir_all(
                    dir().join("library").join(a["asset_id"].as_str().unwrap()),
                );
            }
        }
        job["result"] = Value::Null;
    }
    let view = job_view(job);
    state["ack_log"]
        .as_array_mut()
        .unwrap()
        .push(json!({"job_id": id, "journal_had_import": saw_journal}));
    save(&state);
    out(view)
}

/// The real catalog shape (a bare array; durations only in labels) plus the
/// fake models the tests use.
fn models() -> Value {
    let mut rows = legacy_models();
    if !env_on("FAKE_DURATIONS") {
        return rows;
    }
    // Structured fields as GenRight c1441e7 emits them. Labels are left as
    // they were so a client that still parsed them would be caught.
    let set = |rows: &mut Value, model: &str, extra: Value| {
        let r = rows
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["model"] == model)
            .unwrap();
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
    };
    set(
        &mut rows,
        "fake-vid",
        json!({"durations_s": [5, 8], "default_duration_s": 5}),
    );
    set(
        &mut rows,
        "fal-ai/veo3.1/fast",
        json!({"durations_s": [4, 6, 8], "default_duration_s": 4}),
    );
    set(
        &mut rows,
        "kling-3-pro",
        json!({"min_duration_s": 3, "max_duration_s": 15, "default_duration_s": 5}),
    );
    set(
        &mut rows,
        "wan-ultra",
        json!({"durations_s": [5, 8], "default_duration_s": 5}),
    );
    let extra = json!([
        {"provider": "fake", "model": "fake-one", "kind": "video", "label": "Fake one-value (4 s)", "est_usd": 0.02,
         "durations_s": [4], "default_duration_s": 4},
        {"provider": "wavespeed", "model": "infinitetalk", "kind": "video", "label": "InfiniteTalk (length follows the audio)",
         "est_usd": 0.3, "min_duration_s": 3, "max_duration_s": 600},
    ]);
    rows.as_array_mut()
        .unwrap()
        .extend(extra.as_array().unwrap().iter().cloned());
    rows
}

fn legacy_models() -> Value {
    let row = |p: &str, m: &str, k: &str, l: &str, usd: f64| json!({"provider": p, "model": m, "kind": k, "label": l, "est_usd": usd});
    json!([
        row("fake", "fake-vid", "video", "Fake video (5 or 8 s)", 0.01),
        row("fake", "fake-voice", "voice", "Fake voice", 0.001),
        row(
            "fal",
            "fal-ai/flux-2/klein/9b",
            "image",
            "FLUX.2 klein 9B (image, ~$0.006/MP)",
            0.0065
        ),
        row(
            "fal",
            "fal-ai/veo3.1/fast",
            "video",
            "Veo 3.1 Fast text-to-video (4 s, no audio, $0.10/s)",
            0.4
        ),
        row(
            "elevenlabs",
            "eleven_flash_v2_5",
            "voice",
            "Eleven Flash v2.5 (lowest latency, $0.04/1K chars)",
            0.02
        ),
        row(
            "wavespeed",
            "kling-3-pro",
            "video",
            "Kling 3.0 Pro (3-15 s, first+last frame, $0.112/s)",
            0.56
        ),
        row(
            "wavespeed",
            "wan-ultra",
            "video",
            "Wan 2.2 480p ultra-fast (5 or 8 s, $0.01/s)",
            0.05
        ),
        row(
            "wavespeed",
            "elevenlabs-turbo-v2.5",
            "voice",
            "ElevenLabs Turbo v2.5 TTS ($0.10/1000 chars)",
            0.01
        ),
        row(
            "heygen",
            "avatar-video",
            "avatar",
            "HeyGen avatar video (Avatar IV, ~$4/min)",
            2.0
        ),
    ])
}
