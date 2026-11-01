//! Vox finishing through an external timeline engine CLI (`<engine>` below).
//!
//! The pipeline DAG assembles shots, voice and SFX; this step hands them to the engine CLI to
//! build a real timeline and render it. Contract used (all JSON on stdout, errors as
//! `{"error":{"code","message"}}` with a non-zero exit):
//!
//! 1. `<engine> project init <dir> --fps <n|n/d> --name <name>`
//! 2. `<engine> source add <dir> <file> --reference` per shot, voice, music and SFX file
//!    -> `source.id` (64 hex) and `source.duration` (ticks, 705_600_000 per second)
//! 3. `<engine> edit <dir> --action <json>` with one `batch` action: shots on `V1`, voice
//!    on the default narration track `A1`, music on a new music track `A2`, SFX on a new music
//!    track `A3` (the engine's roles are narration and music only)
//! 4. `<engine> render <dir> --preset <youtube|reels|tiktok|square> --out <name>`
//!    -> `output`, `bytes`, `duration_ms`
//!
//! The finishing engine is an installed timeline app speaking this CLI contract (the step and
//! command ids keep their original `cutright_*` names for wire compatibility). The host registers
//! how to find it with [`set_engine_resolver`] (installed bundles/registry only, never PATH); an
//! explicit `engine_path` input always wins. Absent, the result is `status: "not_installed"`
//! rather than an error, so the pipeline can report it and carry on with the native render.
//! Processes run through `rightkit_child_process::run`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::{GenError, GenResult};

const TICKS_PER_SECOND: f64 = 705_600_000.0;
const PRESETS: &[&str] = &["youtube", "reels", "tiktok", "square"];

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .map(|x| x.trim().to_string())
        .unwrap_or_default()
}

fn bad(msg: impl Into<String>) -> GenError {
    GenError::new("invalid_input", msg)
}

fn ticks(seconds: f64) -> i64 {
    (seconds.max(0.0) * TICKS_PER_SECOND).round() as i64
}

/// Finds the installed finishing engine binary, or `None` when it is not installed.
pub type EngineResolver = fn() -> Option<PathBuf>;

static ENGINE_RESOLVER: std::sync::OnceLock<EngineResolver> = std::sync::OnceLock::new();

/// Register how the host discovers the finishing engine. Without one, only an explicit
/// `engine_path` input is used. Returns the rejected resolver when one is already set.
pub fn set_engine_resolver(resolver: EngineResolver) -> Result<(), EngineResolver> {
    ENGINE_RESOLVER.set(resolver)
}

/// The installed finishing engine, or why there is none.
fn resolve_engine(input: &Value) -> Result<PathBuf, String> {
    let given = s(input, "engine_path");
    if !given.is_empty() {
        let p = PathBuf::from(&given);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("engine_path {given} is not a file"))
        };
    }
    ENGINE_RESOLVER
        .get()
        .and_then(|resolve| resolve())
        .filter(|p| p.is_file())
        .ok_or_else(|| {
            "the finishing engine is not installed (no installed app found by discovery)"
                .to_string()
        })
}

fn not_installed(reason: String) -> Value {
    json!({
        "status": "not_installed",
        "app": "finishing-engine",
        "reason": reason,
        "hint": "Install the finishing app to finish through its engine; the native composition is unchanged.",
    })
}

/// Last JSON object on stdout that is not a progress event.
fn parse_result(stdout: &str) -> Option<Value> {
    stdout
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(|v| v.is_object() && v.get("event").is_none())
}

fn call(engine: &Path, args: &[String], timeout: Duration, what: &str) -> GenResult<Value> {
    let options = rightkit_child_process::RunOptions::new(timeout)
        .stdout_cap(256 * 1024 * 1024)
        .stderr_cap(256 * 1024 * 1024);
    let out = rightkit_child_process::run(engine, args, &options).map_err(|e| {
        GenError::new(
            "cutright_missing",
            format!("cannot run {} ({what}): {e}", engine.display()),
        )
    })?;
    if out.timed_out {
        return Err(GenError::new(
            "cutright_missing",
            format!(
                "cannot run {} ({what}): child exceeded its time limit and was terminated",
                engine.display()
            ),
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout.bytes).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr.bytes).into_owned();
    let parsed = parse_result(&stdout);
    if !out.status.success() || parsed.as_ref().is_some_and(|v| v.get("error").is_some()) {
        let detail = parsed
            .as_ref()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| stderr.trim().chars().take(600).collect());
        let code = parsed
            .as_ref()
            .and_then(|v| v["error"]["code"].as_str().map(String::from))
            .unwrap_or_default();
        return Err(GenError::new(
            "cutright_failed",
            format!("finishing engine {what} failed ({code}): {detail}"),
        ));
    }
    parsed.ok_or_else(|| {
        GenError::new(
            "cutright_bad_output",
            format!("finishing engine {what} printed no JSON result"),
        )
    })
}

struct Source {
    id: String,
    duration_ticks: i64,
}

fn add_source(engine: &Path, dir: &str, file: &str, timeout: Duration) -> GenResult<Source> {
    if !Path::new(file).is_file() {
        return Err(bad(format!("media file not found: {file}")));
    }
    let r = call(
        engine,
        &[
            "source".into(),
            "add".into(),
            dir.into(),
            file.into(),
            "--reference".into(),
        ],
        timeout,
        "source add",
    )?;
    let id = r["source"]["id"].as_str().unwrap_or_default().to_string();
    let dur = r["source"]["duration"].as_i64().unwrap_or(0);
    if id.is_empty() || dur <= 0 {
        return Err(GenError::new(
            "cutright_bad_output",
            format!("source add for {file} returned no id/duration: {r}"),
        ));
    }
    Ok(Source {
        id,
        duration_ticks: dur,
    })
}

fn audio_clip(id: &str, src: &Source, start_s: f64, gain_db: f64) -> Value {
    json!({
        "id": id, "source": src.id, "trim_in": 0, "trim_out": src.duration_ticks,
        "start": ticks(start_s), "gain": (gain_db * 1000.0).round() as i64,
        "link": null, "under": { "mode": "keep" },
    })
}

/// `studio.pipelines.cutright_finish`.
///
/// Input: `shots` (ordered video paths) or `video_path`; `voice_path`; optional `music_path`,
/// `music_gain_db`, `sfx` [{path, at_s, gain_db?}], `preset` (default youtube), `fps` (default
/// 30), `project_dir`, `out_name`, `timeout_s`, `engine_path` (explicit engine; a missing file
/// reports `not_installed`).
pub fn finish(_ctx: &crate::Context, input: &Value) -> GenResult<Value> {
    let mut shots: Vec<String> = input
        .get("shots")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if shots.is_empty() && !s(input, "video_path").is_empty() {
        shots.push(s(input, "video_path"));
    }
    if shots.is_empty() {
        return Err(bad("cutright_finish needs `shots` or `video_path`"));
    }
    let preset = if s(input, "preset").is_empty() {
        "youtube".to_string()
    } else {
        s(input, "preset")
    };
    if !PRESETS.contains(&preset.as_str()) {
        return Err(bad(format!(
            "unknown preset {preset}: expected {}",
            PRESETS.join(", ")
        )));
    }
    let fps = if s(input, "fps").is_empty() {
        input
            .get("fps")
            .and_then(Value::as_u64)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "30".into())
    } else {
        s(input, "fps")
    };
    let sfx: Vec<(String, f64, f64)> = input
        .get("sfx")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| {
                    (
                        s(x, "path"),
                        x.get("at_s").and_then(Value::as_f64).unwrap_or(0.0),
                        x.get("gain_db").and_then(Value::as_f64).unwrap_or(-6.0),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if sfx.iter().any(|(p, _, _)| p.is_empty()) {
        return Err(bad("every sfx entry needs a path"));
    }

    let engine = match resolve_engine(input) {
        Ok(e) => e,
        Err(reason) => return Ok(not_installed(reason)),
    };

    let dir = s(input, "project_dir");
    if dir.is_empty() {
        return Err(bad(
            "cutright_finish needs project_dir (a folder for the finishing project)",
        ));
    }
    let timeout = Duration::from_secs(
        input
            .get("timeout_s")
            .and_then(Value::as_u64)
            .unwrap_or(1800),
    );
    let out_name = if s(input, "out_name").is_empty() {
        "vox-final".to_string()
    } else {
        s(input, "out_name")
    };

    call(
        &engine,
        &[
            "project".into(),
            "init".into(),
            dir.clone(),
            "--fps".into(),
            fps.clone(),
            "--name".into(),
            out_name.clone(),
        ],
        timeout,
        "project init",
    )?;

    let mut actions: Vec<Value> = Vec::new();
    for (i, shot) in shots.iter().enumerate() {
        let src = add_source(&engine, &dir, shot, timeout)?;
        actions.push(json!({
            "type": "insert_clip", "track_id": "V1", "index": i,
            "clip": { "id": format!("vox-v{}", i + 1), "source": src.id, "source_in": 0, "source_out": src.duration_ticks },
        }));
    }
    let voice = s(input, "voice_path");
    if !voice.is_empty() {
        let src = add_source(&engine, &dir, &voice, timeout)?;
        actions.push(json!({ "type": "add_audio_clip", "track_id": "A1", "clip": audio_clip("vox-voice", &src, 0.0, 0.0) }));
    }
    let music = s(input, "music_path");
    if !music.is_empty() {
        let src = add_source(&engine, &dir, &music, timeout)?;
        let gain = input
            .get("music_gain_db")
            .and_then(Value::as_f64)
            .unwrap_or(-18.0);
        actions.push(json!({ "type": "add_audio_track", "track_id": "A2", "role": "music" }));
        actions.push(json!({ "type": "add_audio_clip", "track_id": "A2", "clip": audio_clip("vox-music", &src, 0.0, gain) }));
    }
    if !sfx.is_empty() {
        actions.push(json!({ "type": "add_audio_track", "track_id": "A3", "role": "music" }));
        for (i, (path, at_s, gain)) in sfx.iter().enumerate() {
            let src = add_source(&engine, &dir, path, timeout)?;
            actions.push(json!({ "type": "add_audio_clip", "track_id": "A3", "clip": audio_clip(&format!("vox-sfx{}", i + 1), &src, *at_s, *gain) }));
        }
    }
    let action = json!({ "type": "batch", "actions": actions }).to_string();
    let edit = call(
        &engine,
        &["edit".into(), dir.clone(), "--action".into(), action],
        timeout,
        "edit",
    )?;
    let render = call(
        &engine,
        &[
            "render".into(),
            dir.clone(),
            "--preset".into(),
            preset.clone(),
            "--out".into(),
            out_name.clone(),
            "--timeout-secs".into(),
            timeout.as_secs().to_string(),
        ],
        timeout + Duration::from_secs(30),
        "render",
    )?;
    Ok(json!({
        "status": "finished",
        "engine": engine.to_string_lossy(),
        "project_dir": dir,
        "preset": preset,
        "out_path": render["output"],
        "bytes": render["bytes"],
        "duration_ms": render["duration_ms"],
        "shots": shots.len(),
        "sfx": sfx.len(),
        "edit_status": edit["status"],
        "render": render,
    }))
}

/// The `cutright_finish` native step: reads shots/voice/SFX from the run, hands them over and
/// records the finished video as the step's asset. `not_installed` succeeds the step with a data
/// asset carrying `status: not_installed` (nothing is faked); retry after installing CutRight.
pub fn step_asset(ctx: &crate::Context, run: &Value, step: &Value, dir: &Path) -> GenResult<Value> {
    let run_id = s(run, "run_id");
    let inputs = &run["inputs"];
    let mut shots: Vec<String> = Vec::new();
    for dep in step["depends_on"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let step_of = run["steps"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["id"] == dep));
        for a in step_of
            .and_then(|x| x["assets"].as_array())
            .into_iter()
            .flatten()
        {
            if a["kind"] == "video" && Path::new(&s(a, "path")).is_file() {
                shots.push(s(a, "path"));
            }
        }
    }
    if shots.is_empty() {
        shots = match inputs.get("asset_paths") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(Value::String(t)) => t
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect(),
            _ => Vec::new(),
        };
    }
    let voice = run["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|x| x["assets"].as_array().into_iter().flatten())
        .find(|a| a["kind"] == "audio" && Path::new(&s(a, "path")).is_file())
        .map(|a| s(a, "path"))
        .unwrap_or_else(|| s(inputs, "audio_path"));
    let out_name = format!("{}-{}", run_id, s(step, "id"));
    let result = finish(
        ctx,
        &json!({
            "shots": shots, "voice_path": voice, "music_path": s(inputs, "music_path"),
            "sfx": inputs.get("sfx").cloned().unwrap_or(json!([])),
            "preset": s(inputs, "cutright_preset"), "fps": s(inputs, "fps"),
            "engine_path": s(inputs, "cutright_engine"),
            "project_dir": dir.join(format!("{out_name}-project")).to_string_lossy(),
            "out_name": out_name,
        }),
    )?;
    if result["status"] == "not_installed" {
        // Optional finishing: the earlier steps' outputs stand. The step records the reason as a
        // data asset (no video) instead of failing the run.
        let path = dir.join(format!("{}.json", s(step, "id")));
        let body = serde_json::to_vec_pretty(&result).unwrap_or_default();
        rightkit_fs::atomic_write(&path, &body)
            .map_err(|e| GenError::new("io", format!("cannot write {}: {e}", path.display())))?;
        return Ok(json!({
            "asset_id": out_name, "path": path.to_string_lossy(),
            "sha256": rightkit_media_gen::library::sha256_hex(&body), "bytes": body.len(),
            "mime": "application/json", "kind": "data", "status": "not_installed",
            "reason": result["reason"], "finished_by": null,
        }));
    }
    let path = PathBuf::from(result["out_path"].as_str().unwrap_or_default());
    let bytes = std::fs::read(&path)
        .map_err(|e| GenError::new("io", format!("cannot read {}: {e}", path.display())))?;
    Ok(json!({
        "asset_id": out_name, "path": path.to_string_lossy(),
        "sha256": rightkit_media_gen::library::sha256_hex(&bytes), "bytes": bytes.len(),
        "mime": "video/mp4", "kind": "video", "finished_by": "cutright-engine",
        "sfx": result["sfx"],
    }))
}
