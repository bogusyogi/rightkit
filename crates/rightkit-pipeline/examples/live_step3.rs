//! Live step-3 run against an INSTALLED GenRight (spends real money, kept to
//! the cheapest image + voice models, optionally one cheapest video).
//!
//! ```text
//! cargo run -p cutright-genright --example live_step3 -- [<engine path>] [--video] [--out <file>]
//! ```
//! The engine path is the first non-flag argument, else the first line of the
//! marker file `crates/cutright-genright/tests/.genright-engine` (rightkit
//! scrubs environment variables, so there is no env fallback). Windows:
//! `%LOCALAPPDATA%\GenRight\genright-engine.exe`; macOS:
//! `/Applications/GenRight.app/Contents/MacOS/genright-engine`.
//!
//! In a temp CutRight project directory it: checks the engine is contract v2;
//! submits one image and one voice (and with `--video` one video); waits by
//! polling `job status` with progress events; then proves crash safety
//! (the image copy/journal crash case left with the failpoint seam, 8811af2d2):
//!   * voice: forgets the job mapping (crash after `generate`, before the job
//!     id was journaled) and `recover()` re-finds the same job by request id.
//!
//! It asserts one media file per asset, matching sha256 (re-hashed here) and
//! BLAKE3, the same job ids throughout, exactly one GenRight job per request
//! id, and that no job was acked before its durable import. Prints a JSON
//! evidence report; exits non-zero on any failed assertion.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rightkit_fs::digest::sha256_bytes;
use rightkit_pipeline::journal::import_key;
use rightkit_pipeline::{
    Error, GenerateSpec, JobListQuery, JobRecord, MediaKind, Orchestrator, RequestState,
    WaitOptions,
};
use serde_json::{json, Value};

type R = Result<(), Box<dyn std::error::Error>>;

fn check(cond: bool, what: &str) -> R {
    if cond {
        eprintln!("  ok: {what}");
        Ok(())
    } else {
        Err(format!("ASSERTION FAILED: {what}").into())
    }
}

fn engine_path(arg: Option<String>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(a) = arg {
        return Ok(PathBuf::from(a));
    }
    let marker = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/.genright-engine");
    let text = std::fs::read_to_string(&marker)
        .map_err(|_| format!("pass the engine path, or put it in {}", marker.display()))?;
    Ok(PathBuf::from(text.lines().next().unwrap_or("").trim()))
}

fn media_files(project: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(project.join("media"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn spend(rec: &JobRecord) -> Value {
    json!({"job_id": rec.job_id, "model": rec.model, "status": rec.status,
        "est_usd": rec.est_usd, "actual_usd": rec.actual_usd})
}

fn main() -> R {
    let mut engine_arg = None;
    let mut video = false;
    let mut out_file = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--video" => video = true,
            "--out" => out_file = it.next(),
            _ => engine_arg = Some(a),
        }
    }
    let engine = engine_path(engine_arg)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let project = std::env::temp_dir().join(format!("rightkit-pipeline-live-step3-{stamp}"));
    std::fs::create_dir_all(&project)?;
    eprintln!("engine {}\nproject {}", engine.display(), project.display());

    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let make = || {
        let o = Orchestrator::for_engine(&project, &engine);
        let sink = events.clone();
        o.set_event_sink(move |e| sink.lock().unwrap().push(e));
        o
    };
    let o = make();
    let info = o.ensure_ready()?.clone();
    check(
        info.contract_version == 2,
        "engine reports contract_version 2",
    )?;

    // Cheapest listed models, cheapest first; skip providers without keys.
    let catalog = o.models()?.clone();
    let pick = |kind: &str| -> Vec<(String, String, f64, Value)> {
        let mut v: Vec<_> = catalog
            .models
            .iter()
            .filter(|m| m.kind == kind && m.est_usd.is_some())
            .map(|m| {
                (
                    m.provider.clone(),
                    m.model.clone(),
                    m.est_usd.unwrap(),
                    serde_json::to_value(&m.durations).unwrap(),
                )
            })
            .collect();
        v.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
        v
    };
    let stamp_id = format!("live-step3-{stamp}");
    let mut specs: Vec<(String, GenerateSpec, f64)> = Vec::new();
    for (tag, kind, prompt, params) in [
        (
            "img",
            MediaKind::Image,
            "a single red apple on a white table, studio light",
            json!({"aspect": "16:9"}),
        ),
        (
            "voice",
            MediaKind::Voice,
            "Pipeline live step three check.",
            json!({}),
        ),
        (
            "vid",
            MediaKind::Video,
            "a slow pan across a calm lake at dawn",
            json!({"duration": 5}),
        ),
    ] {
        if tag == "vid" && !video {
            continue;
        }
        let (p, m, usd, _) = pick(kind.as_str())
            .into_iter()
            .next()
            .ok_or("no model of that kind")?;
        eprintln!("{tag}: cheapest {kind:?} model {p}/{m} est ${usd}");
        specs.push((
            format!("{stamp_id}-{tag}"),
            GenerateSpec {
                kind,
                provider: p,
                model: m,
                prompt: prompt.into(),
                params,
            },
            usd,
        ));
    }
    let planned: f64 = specs.iter().map(|s| s.2).sum();
    check(
        planned <= 0.25,
        &format!("planned spend ${planned:.4} is within the live budget"),
    )?;

    // ---- submit + wait --------------------------------------------------
    let mut subs = Vec::new();
    for (rid, spec, _) in &specs {
        let s = o.submit(spec, Some(rid))?;
        eprintln!("submitted {} -> job {}", rid, s.job_id);
        subs.push((rid.clone(), s.job_id));
    }
    let opts = WaitOptions {
        poll_interval: Duration::from_secs(2),
        timeout: Some(Duration::from_secs(600)),
        cancel: None,
    };
    let mut done = Vec::new();
    for (rid, job) in &subs {
        let rec = o.wait(rid, &opts)?;
        check(
            rec.status == "complete",
            &format!(
                "job {job} complete (est {:?} actual {:?})",
                rec.est_usd, rec.actual_usd
            ),
        )?;
        check(
            rec.acked_by.is_none(),
            &format!("job {job} not acked before import"),
        )?;
        done.push(rec);
    }
    let progress_events = events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e["event"] == "genright.job_progress")
        .count();
    check(
        progress_events > 0,
        &format!("{progress_events} progress events were observed while waiting"),
    )?;

    drop(o);

    // ---- crash 2: voice, mapping lost after generate (before job id journaled)
    let (voice_rid, voice_job) = subs[1].clone();
    let o = make();
    o.journal().update(|d| {
        let e = d
            .requests
            .get_mut(&voice_rid)
            .ok_or(Error::UnknownRequest {
                request_id: voice_rid.clone(),
            })?;
        e.job_id = None;
        e.state = RequestState::Pending;
        Ok(())
    })?;

    // ---- restart: recover + import everything ----------------------------
    let o2 = make();
    let recovered = o2.recover()?;
    check(
        recovered.len() == 1,
        "recover touched exactly the request with no job id",
    )?;
    let (rid, sub) = &recovered[0];
    let sub = sub.as_ref().map_err(|e| e.to_string())?;
    check(
        rid == &voice_rid && sub.job_id == voice_job && sub.deduplicated,
        "recover re-found the SAME voice job (no resubmission)",
    )?;
    let mut imported = Vec::new();
    for (rid, job) in &subs {
        let recs = o2.import(rid)?;
        check(recs.len() == 1, &format!("job {job}: one asset imported"))?;
        imported.extend(recs);
    }
    check(
        o2.import(&subs[0].0)?.len() == 1,
        "re-running import is a no-op",
    )?;

    // ---- verification ----------------------------------------------------
    let files = media_files(&project);
    check(
        files.len() == imported.len(),
        &format!("exactly one media file per asset ({files:?})"),
    )?;
    check(
        files
            .iter()
            .all(|f| !f.ends_with(".part") && !f.starts_with('.')),
        "no temp residue in media/",
    )?;
    let mut report_assets = Vec::new();
    for r in &imported {
        let bytes = std::fs::read(&r.media_path)?;
        check(
            sha256_bytes(&bytes) == r.sha256,
            &format!("{} sha256 re-hashed", r.asset_id),
        )?;
        check(
            rightkit_fs::digest::blake3_bytes(&bytes) == r.blake3,
            &format!("{} blake3 re-hashed", r.asset_id),
        )?;
        check(
            files.contains(&r.media_rel.rsplit('/').next().unwrap_or("").to_string()),
            "journal path matches the file on disk",
        )?;
        check(
            o2.journal()
                .read()?
                .imports
                .contains_key(&import_key(&r.job_id, &r.asset_id)),
            "durable journal record exists",
        )?;
        report_assets.push(
            json!({"job_id": r.job_id, "asset_id": r.asset_id, "kind": r.kind,
            "media_rel": r.media_rel, "bytes": r.bytes, "mime": r.mime, "sha256": r.sha256,
            "blake3": r.blake3, "media": r.media, "needs_probe": r.needs_probe,
            "provider": r.provider, "model": r.model}),
        );
    }
    let mut report_jobs = Vec::new();
    for ((rid, job), (_, spec, _)) in subs.iter().zip(&specs) {
        let page = o2.client().job_page(&JobListQuery {
            request_id: Some(rid.clone()),
            limit: Some(10),
            ..Default::default()
        })?;
        let mine: Vec<_> = page
            .jobs
            .iter()
            .filter(|j| j.request_id.as_deref() == Some(rid))
            .collect();
        check(
            mine.len() == 1 && mine[0].job_id == *job,
            &format!("GenRight holds exactly one job ({job}) for {rid}"),
        )?;
        let rec = o2.client().job_status(job)?;
        check(
            rec.acked_by.as_deref() == Some("rightkit"),
            &format!("job {job} acked after durable import"),
        )?;
        let mut j = spend(&rec);
        j["request_id"] = json!(rid);
        j["provider"] = json!(spec.provider);
        report_jobs.push(j);
    }
    let est: f64 = report_jobs
        .iter()
        .filter_map(|j| j["est_usd"].as_f64())
        .sum();
    let actual: f64 = report_jobs
        .iter()
        .filter_map(|j| j["actual_usd"].as_f64())
        .sum();
    let report = json!({
        "engine": engine.display().to_string(), "app_version": info.app_version,
        "contract_version": info.contract_version, "project": project.display().to_string(),
        "jobs": report_jobs, "assets": report_assets, "media_files": files,
        "progress_events": progress_events, "est_usd_total": est, "actual_usd_total": actual,
        "result": "PASS",
    });
    let text = serde_json::to_string_pretty(&report)?;
    if let Some(f) = out_file {
        std::fs::write(f, &text)?;
    }
    println!("{text}");
    Ok(())
}
