//! studio.pipelines: named production-pipeline workflows (Vox-style explainer,
//! ShotCraft product video, OpenCreator dubbing, Lanshu narration-clock
//! presenter, brag launch, MoneyPrinterTurbo batch variations) as editable,
//! persistable step DAGs.
//!
//! This module is orchestration, not rendering: `plan` produces a validated,
//! scheduled workflow; `run_*`/`step_*` manage durable runs under
//! `<data_dir>/pipelines/runs/`. Steps that produce media return a `generate`
//! request contract — the caller (the Pipeline Workflows panel) submits it
//! through the existing `generate`/`job.*` engine commands and settles the
//! step with the job outcome. Steps that produce text artifacts (subtitle
//! exports, share copy, plans) execute natively and write real files into the
//! run folder. Routes with no native or configured provider are marked
//! `unsupported`/`route_unready` with an explicit reason rather than faked.
//!
//! Donor provenance (pattern sources only; no donor code is copied):
//! - Vox Director: github.com/Alisa0808/vox-director (beats.json, poster-first
//!   flow, narrative arcs; licence status per upstream).
//! - Video ShotCraft: github.com/Vincentwei1021/video-shotcraft (`references/shots/`
//!   recipe data, music-beat-sync, sound-design; pin a revision before counting
//!   manifest entries).
//! - OpenCreator/KrillinAI: github.com/krillinai/OpenCreator (subtitle-line
//!   handling, terminology translation, retiming, separate exports).
//! - Lanshu presenter skill: github.com/cclank/lanshu-create-ai-presenter-video
//!   (narration as master clock, pause-aligned segments, provider duration caps).
//! - brag: github.com/latent-spaces/brag (entry→action→result script shape,
//!   delivery checklist; voiceover optional upstream).
//! - MoneyPrinterTurbo: github.com/harry0703/MoneyPrinterTurbo (topic→script→
//!   assets→narration batch variation; stock-footage mapping stays a
//!   recommendation, not a source we run).
//! - OpenMontage routing/quality: capability-catalog + alternatives-with-reasons
//!   approach informs `route` fields; NullMotion/Remotion inform beat/timing
//!   vocabulary only — native Motion remains the render path.
//!
//! Commands (`studio.pipelines.<verb>`): templates, plan, validate, save,
//! list, get, delete, run_create, run_list, run_get, run_ready, step_begin,
//! step_attach, step_settle, step_retry, step_skip, run_cancel, run_recover, step_status, step_cancel.
//!
//! A step whose begin returns `requests[]` (the dub step: one voice job per segment) is a multi-job
//! step: see `fanout.rs`. Its jobs are attached per segment, its state is derived from them and kept
//! in the run record, and `step_status` reports and settles it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::production::slugify;
use crate::{Context, GenError, GenResult};

mod asr;
mod compose;
mod dubbing;
mod fanout;
pub mod finish;
mod media;
mod translate;

pub const COMMANDS: &[&str] = &[
    "studio.pipelines.templates",
    "studio.pipelines.plan",
    "studio.pipelines.validate",
    "studio.pipelines.save",
    "studio.pipelines.list",
    "studio.pipelines.get",
    "studio.pipelines.delete",
    "studio.pipelines.run_create",
    "studio.pipelines.run_list",
    "studio.pipelines.run_get",
    "studio.pipelines.run_ready",
    "studio.pipelines.step_begin",
    "studio.pipelines.step_attach",
    "studio.pipelines.step_settle",
    "studio.pipelines.step_retry",
    "studio.pipelines.step_skip",
    "studio.pipelines.run_cancel",
    "studio.pipelines.run_recover",
    "studio.pipelines.step_status",
    "studio.pipelines.step_cancel",
    "studio.pipelines.media_status",
    "studio.pipelines.probe",
    "studio.pipelines.compose",
    "studio.pipelines.transcribe",
    "studio.pipelines.asr_status",
    "studio.pipelines.asr_provision",
    "studio.pipelines.translate",
    "studio.pipelines.dubbing_plan",
    "studio.pipelines.cutright_finish",
];

const SCHEMA_VERSION: &str = "1.0.0";
const WORKFLOW_KIND: &str = "pipeline_workflow";
const RUN_KIND: &str = "pipeline_run";
/// Rough narration speech rate (chars per second) for clock estimates.
const SPEECH_CPS: f64 = 15.0;
const MEDIA_KINDS: &[&str] = &["image", "video", "voice", "avatar"];

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.pipelines.templates" => Ok(json!({ "templates": templates() })),
        "studio.pipelines.plan" => plan(ctx, input),
        "studio.pipelines.validate" => validate(input).map(|report| json!(report)),
        "studio.pipelines.save" => save(ctx, input),
        "studio.pipelines.list" => list(ctx),
        "studio.pipelines.get" => get_workflow(ctx, input),
        "studio.pipelines.delete" => delete_workflow(ctx, input),
        "studio.pipelines.run_create" => run_create(ctx, input),
        "studio.pipelines.run_list" => run_list(ctx),
        "studio.pipelines.run_get" => run_get(ctx, input),
        "studio.pipelines.run_ready" => run_ready(ctx, input),
        "studio.pipelines.step_begin" => step_begin(ctx, input),
        "studio.pipelines.step_attach" => step_attach(ctx, input),
        "studio.pipelines.step_settle" => step_settle(ctx, input),
        "studio.pipelines.step_retry" => step_retry(ctx, input),
        "studio.pipelines.step_skip" => step_skip(ctx, input),
        "studio.pipelines.run_cancel" => run_cancel(ctx, input),
        "studio.pipelines.run_recover" => run_recover(ctx, input),
        "studio.pipelines.step_status" => step_status(ctx, input),
        "studio.pipelines.step_cancel" => step_cancel(ctx, input),
        "studio.pipelines.media_status" => Ok(media::media_status()),
        "studio.pipelines.probe" => probe_cmd(input),
        "studio.pipelines.compose" => compose::compose(ctx, input),
        "studio.pipelines.transcribe" => asr::transcribe(ctx, input),
        "studio.pipelines.asr_status" => Ok(asr::asr_status(ctx)),
        "studio.pipelines.asr_provision" => asr::asr_provision(ctx, input),
        "studio.pipelines.translate" => translate::translate(ctx, input),
        "studio.pipelines.dubbing_plan" => dubbing::plan_command(ctx, input),
        "studio.pipelines.cutright_finish" => finish::finish(ctx, input),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

fn invalid(message: impl Into<String>) -> GenError {
    GenError::new("invalid_input", message)
}

fn clean(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn field(input: &Value, key: &str) -> String {
    clean(input.get(key))
}

fn tstr(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn num(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 1e12 {
        json!(v as i64)
    } else {
        json!((v * 1000.0).round() / 1000.0)
    }
}

/// Timestamp-y sortable stamp for artifact names (`20261006T061530-12345678`).
pub(crate) fn stamp() -> String {
    format!(
        "{}-{}",
        rightkit_media_gen::store::now()
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>(),
        short_id()
    )
}

fn probe_cmd(input: &Value) -> GenResult<Value> {
    let p = req_id(input, "path")?;
    media::probe(Path::new(&p))
}

fn short_id() -> String {
    rightkit_logs::identity::random_uuid()
        .chars()
        .filter(|c| *c != '-')
        .take(8)
        .collect()
}

fn req_id(input: &Value, key: &str) -> GenResult<String> {
    Some(field(input, key))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("missing field: {key}")))
}

// ---------------------------------------------------------------------------
// templates (donor workflows as step blueprints)
//
// Each step: id, label, kind (plan|voice|image|video|avatar|export|check),
// depends_on, produces, est_s, and either a `request` template (handoff to an
// existing engine route) or `native` execution (export/check/plan). `{key}`
// placeholders fill from workflow inputs. `needs` wires artifacts from earlier
// steps into request params at begin time.

fn step(id: &str, label: &str, kind: &str, deps: &[&str], produces: &str, est_s: f64) -> Value {
    json!({
        "id": id, "label": label, "kind": kind,
        "depends_on": deps, "produces": produces,
        "needs": [], "schedule": { "est_s": est_s },
        "retry": { "max_attempts": 2 }, "params": {},
    })
}

fn req(step: &mut Value, kind: &str, provider: &str, model: &str, prompt: &str, params: Value) {
    step["request"] = json!({
        "kind": kind, "provider": provider, "model": model,
        "prompt": prompt, "params": params,
    });
}

fn need(step: &mut Value, from: &str, kind: &str, param: &str) {
    step["needs"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "from": from, "kind": kind, "param": param }));
}

fn native(step: &mut Value, action: &str) {
    step["native"] = json!(action);
}

fn templates() -> Vec<Value> {
    let source =
        |donor: &str, url: &str, notes: &str| json!({ "donor": donor, "url": url, "notes": notes });
    let mut out = Vec::new();

    // -- Vox-style explainer (vox-director) ----------------------------------
    let mut keyframes = step(
        "keyframes",
        "Beat keyframes",
        "image",
        &["beats"],
        "image",
        60.0,
    );
    req(
        &mut keyframes,
        "image",
        "fal",
        "fal-ai/flux-2/klein/9b",
        "{hook_image_prompt}",
        json!({ "aspect": "{aspect}" }),
    );
    let mut scenes = step(
        "scenes",
        "Motion scenes",
        "video",
        &["keyframes", "narration"],
        "video",
        300.0,
    );
    need(&mut scenes, "keyframes", "image", "image_url");
    req(
        &mut scenes,
        "video",
        "fal",
        "fal-ai/veo3.1/fast/image-to-video",
        "{scene_video_prompt}",
        json!({ "aspect": "{aspect}", "duration": 8 }),
    );
    let mut narration = step(
        "narration",
        "Narration",
        "voice",
        &["script"],
        "audio",
        45.0,
    );
    req(
        &mut narration,
        "voice",
        "local-qts",
        rightkit_media_gen::providers::qts::MODEL,
        "{script}",
        json!({}),
    );
    let mut beats = step("beats", "Beat map", "plan", &[], "data", 1.0);
    native(&mut beats, "beats");
    let mut script = step("script", "Script", "plan", &[], "text", 1.0);
    native(&mut script, "script");
    let mut captions = step(
        "captions",
        "Compose + captions (native FFmpeg)",
        "video",
        &["scenes", "narration"],
        "video",
        30.0,
    );
    native(&mut captions, "compose_video");
    let mut finish = step(
        "finish",
        "Timeline finishing (optional)",
        "video",
        &["captions"],
        "video",
        60.0,
    );
    native(&mut finish, "cutright_finish");
    out.push(json!({
        "id": "vox_director", "label": "Vox-style explainer",
        "summary": "Topic → beats/script → keyframes → motion scenes → narration → captions/composition → playable video → optional timeline-app finishing (reports not_installed when absent).",
        "source": source("Vox Director", "https://github.com/Alisa0808/vox-director",
            "beats.json + poster-first flow; stage execution ported natively, models via existing adapters"),
        "inputs": ["topic", "script", "aspect", "hook_image_prompt", "scene_video_prompt", "asset_paths", "audio_path", "music_path", "subtitles_path", "ken_burns", "voice_id"],
        "steps": [script, beats, keyframes, narration, scenes, captions, finish],
    }));

    // -- ShotCraft product video ---------------------------------------------
    let mut shot1 = step(
        "shot_a",
        "Hero shot",
        "video",
        &["shot_plan"],
        "video",
        120.0,
    );
    req(
        &mut shot1,
        "video",
        "fal",
        "fal-ai/veo3.1/fast",
        "{shot_a_prompt}",
        json!({ "aspect": "{aspect}", "duration": 8 }),
    );
    let mut shot2 = step(
        "shot_b",
        "Detail shot",
        "video",
        &["shot_plan"],
        "video",
        120.0,
    );
    req(
        &mut shot2,
        "video",
        "fal",
        "fal-ai/veo3.1/fast",
        "{shot_b_prompt}",
        json!({ "aspect": "{aspect}", "duration": 8 }),
    );
    let mut shot_plan = step("shot_plan", "Shot plan", "plan", &[], "data", 1.0);
    native(&mut shot_plan, "shot_plan");
    let mut timing = step(
        "timing",
        "Music/SFX timing",
        "check",
        &["shot_a", "shot_b"],
        "data",
        2.0,
    );
    native(&mut timing, "check_delivery_intent");
    let mut sc_compose = step(
        "compose",
        "Compose final video (native FFmpeg)",
        "video",
        &["timing", "shot_a", "shot_b"],
        "video",
        30.0,
    );
    native(&mut sc_compose, "compose_video");
    out.push(json!({
        "id": "shotcraft", "label": "ShotCraft product video",
        "summary": "Product assets → shot plan → camera/motion recipe clips → music/SFX timing → composition. Local shots reuse the first asset_paths image (optional route params.image_from selects an image step).",
        "source": source("Video ShotCraft", "https://github.com/Vincentwei1021/video-shotcraft",
            "references/shots recipe data + music-beat-sync/sound-design; pin revision before counting entries"),
        "inputs": ["product_name", "aspect", "shot_a_prompt", "shot_b_prompt", "asset_paths", "audio_path", "music_path", "subtitles_path", "ken_burns"],
        "steps": [shot_plan, shot1, shot2, timing, sc_compose],
    }));

    // -- OpenCreator dubbing ---------------------------------------------------
    let mut transcribe = step(
        "transcribe",
        "Transcription (whisper.cpp)",
        "check",
        &[],
        "file",
        30.0,
    );
    native(&mut transcribe, "transcribe");
    transcribe["support"] = json!({ "status": Value::Null, "reason": "Local whisper.cpp + ggml model; probed at plan time." });
    let mut translate = step(
        "translate",
        "Terminology-consistent translation",
        "plan",
        &["transcribe"],
        "text",
        5.0,
    );
    native(&mut translate, "translate");
    translate["support"] = json!({ "status": "native", "reason": "Sentence translation through a text-LLM route (translate_provider, e.g. fal any-llm) with the glossary enforced; a dub refuses glossary-only translation and source_lang == target_lang unless passthrough is set. An unregistered route reports route_unready." });
    let mut dub = step(
        "dub",
        "Dub narration",
        "voice",
        &["translate"],
        "audio",
        60.0,
    );
    req(
        &mut dub,
        "voice",
        "local-qts",
        rightkit_media_gen::providers::qts::MODEL,
        "{dubbed_script}",
        json!({}),
    );
    let mut dub_plan = step(
        "dub_plan",
        "Retiming plan + dub track",
        "plan",
        &["dub"],
        "audio",
        5.0,
    );
    native(&mut dub_plan, "dub_plan");
    dub_plan["retry"] = json!({ "max_attempts": 6 });
    let mut srt = step(
        "srt_export",
        "Aligned subtitle export (SRT)",
        "export",
        &["dub_plan"],
        "file",
        2.0,
    );
    native(&mut srt, "export_srt");
    let mut oc_compose = step(
        "compose",
        "Dubbed MP4 (native FFmpeg)",
        "video",
        &["srt_export", "dub_plan"],
        "video",
        30.0,
    );
    native(&mut oc_compose, "compose_video");
    out.push(json!({
        "id": "opencreator_dubbing", "label": "OpenCreator translation/dubbing",
        "summary": "Transcription → per-segment LLM translation (explicit source/target language) → one voice job per segment (cloned or provider voice) → explicit retiming (fits / short / tempo / overflow, needs_review on overflow) → segment-aligned SRT + dub track → subtitled MP4 (dub replaces or mixes over the original audio).",
        "source": source("OpenCreator / KrillinAI", "https://github.com/krillinai/OpenCreator",
            "subtitle-line handling, terminology translation, retiming, separate exports; Auto Clips & Digital Avatar are upstream in-development, not counted"),
        "inputs": ["source_media", "dubbed_script", "voice_id", "voice_prompt", "target_lang", "glossary", "source_lang", "audio_path",
            "translate_provider", "translate_mode", "max_tempo", "allow_shift", "min_gap_ms", "original_audio", "passthrough"],
        "steps": [transcribe, translate, dub, dub_plan, srt, oc_compose],
    }));

    // -- Lanshu narration-clock presenter --------------------------------------
    let mut clock = step(
        "narration_clock",
        "Narration master clock",
        "voice",
        &[],
        "audio",
        60.0,
    );
    req(
        &mut clock,
        "voice",
        "local-qts",
        rightkit_media_gen::providers::qts::MODEL,
        "{script}",
        json!({}),
    );
    let mut seg_plan = step(
        "segment_plan",
        "Pause-aligned segment plan",
        "plan",
        &["narration_clock"],
        "data",
        1.0,
    );
    native(&mut seg_plan, "segment_plan");
    let mut segment = step(
        "segment_1",
        "Presenter segment 1",
        "avatar",
        &["segment_plan"],
        "video",
        240.0,
    );
    need(&mut segment, "narration_clock", "audio", "audio_url");
    req(
        &mut segment,
        "avatar",
        "heygen",
        "avatar-iv",
        "{segment_1_text}",
        json!({ "avatar_id": "{avatar_id}" }),
    );
    let mut joins = step(
        "joins_checks",
        "Joins + lip-sync + loudness checks",
        "check",
        &["segment_1"],
        "data",
        3.0,
    );
    native(&mut joins, "check_presenter");
    let mut final_video = step(
        "compose",
        "Compose presenter video",
        "video",
        &["joins_checks", "segment_1", "narration_clock"],
        "video",
        30.0,
    );
    native(&mut final_video, "compose_video");
    out.push(json!({
        "id": "lanshu_presenter", "label": "Lanshu presenter (narration clock)",
        "summary": "Script → full narration as master clock → pause-aligned segments → presenter clips → joins, lip-sync & loudness checks.",
        "source": source("Lanshu presenter skill", "https://github.com/cclank/lanshu-create-ai-presenter-video",
            "plan_segments/check_state/finalize_delivery shape; cost estimate & retry ceiling surface before paid steps"),
        "inputs": ["script", "avatar_id", "presenter_image", "voice_id", "segment_cap_s", "segment_1_text", "subtitles_path"],
        "steps": [clock, seg_plan, segment, joins, final_video],
    }));

    // -- brag launch video ------------------------------------------------------
    let mut story = step("story", "Launch story", "plan", &["inspect"], "text", 1.0);
    native(&mut story, "script");
    let mut inspect = step(
        "inspect",
        "Inspect product assets",
        "check",
        &[],
        "data",
        2.0,
    );
    native(&mut inspect, "check_inputs");
    let mut compose = step(
        "compose",
        "Compose launch video (native FFmpeg)",
        "video",
        &["story", "inspect", "voiceover"],
        "video",
        60.0,
    );
    native(&mut compose, "compose_video");
    let mut share = step(
        "share_copy",
        "Share copy",
        "export",
        &["story"],
        "file",
        1.0,
    );
    native(&mut share, "export_text");
    let mut vo = step(
        "voiceover",
        "Optional narration",
        "voice",
        &["story"],
        "audio",
        45.0,
    );
    req(
        &mut vo,
        "voice",
        "local-qts",
        rightkit_media_gen::providers::qts::MODEL,
        "{script}",
        json!({}),
    );
    vo["optional"] = json!(true);
    out.push(json!({
        "id": "brag_launch", "label": "brag launch video",
        "summary": "Inspect real product assets → concise entry→action→result story → motion composition → optional narration → video + share copy.",
        "source": source("brag", "https://github.com/latent-spaces/brag",
            "inspection rubric + step-1..4 delivery checklist; voiceover is optional upstream; git-log ingestion is our extension, not donor behaviour"),
        "inputs": ["product_name", "script", "asset_paths", "audio_path", "music_path", "subtitles_path", "voice_id"],
        "steps": [inspect, story, compose, vo, share],
    }));

    // -- MoneyPrinterTurbo batch ------------------------------------------------
    let mut mpt_script = step("mpt_script", "Script", "plan", &[], "text", 1.0);
    native(&mut mpt_script, "script");
    let mut mpt_voice = step(
        "mpt_voice",
        "Narration",
        "voice",
        &["mpt_script"],
        "audio",
        45.0,
    );
    req(
        &mut mpt_voice,
        "voice",
        "local-qts",
        rightkit_media_gen::providers::qts::MODEL,
        "{script}",
        json!({}),
    );
    let mut mpt_v1 = step(
        "variant_1",
        "Variant 1 video",
        "video",
        &["mpt_voice"],
        "video",
        240.0,
    );
    need(&mut mpt_v1, "mpt_voice", "audio", "audio_url");
    req(
        &mut mpt_v1,
        "video",
        "fal",
        "fal-ai/veo3.1/fast",
        "{variant_prompt}",
        json!({ "aspect": "{aspect}", "duration": 8 }),
    );
    let mut mpt_v2 = step(
        "variant_2",
        "Variant 2 video",
        "video",
        &["mpt_voice"],
        "video",
        240.0,
    );
    need(&mut mpt_v2, "mpt_voice", "audio", "audio_url");
    req(
        &mut mpt_v2,
        "video",
        "fal",
        "fal-ai/veo3.1/fast",
        "{variant_prompt_2}",
        json!({ "aspect": "{aspect}", "duration": 8 }),
    );
    out.push(json!({
        "id": "mpt_batch", "label": "MoneyPrinterTurbo batch variations",
        "summary": "Topic → script → narration → N video variants. Stock-footage/source mapping remains a recommendation, not an executed source.",
        "source": source("MoneyPrinterTurbo", "https://github.com/harry0703/MoneyPrinterTurbo",
            "keeps the batch-variation contract; upstream general pipeline overlaps the Production stack and is not duplicated"),
        "inputs": ["script", "aspect", "variant_prompt", "variant_prompt_2", "voice_id"],
        "steps": [mpt_script, mpt_voice, mpt_v1, mpt_v2],
    }));

    let voice =
        json!({ "provider": "local-qts", "model": rightkit_media_gen::providers::qts::MODEL });
    for template in &mut out {
        template["local_routes"] = match tstr(template, "id").as_str() {
            "vox_director" => json!({
                "keyframes": { "provider": "local-image", "model": "auto" },
                "scenes": { "native": "still_clip" }, "narration": voice,
            }),
            "shotcraft" => {
                json!({ "shot_a": { "native": "still_clip" }, "shot_b": { "native": "still_clip" } })
            }
            "lanshu_presenter" => json!({
                "narration_clock": voice,
                "segment_1": { "provider": rightkit_media_gen::providers::local_lipsync::ID,
                    "model": rightkit_media_gen::providers::local_lipsync::MODEL,
                    "params": { "image_path": "{presenter_image}" } },
            }),
            "brag_launch" => json!({ "voiceover": voice }),
            _ => continue,
        };
    }

    out
}

// ---------------------------------------------------------------------------
// planning
//
// Deterministic: inputs fill `{placeholder}`s, the DAG is topologically
// scheduled with each step's `est_s`, provider route readiness is checked
// against credential status + the model catalog, and unsupported routes stay
// flagged. Nothing here submits or spends.

fn fill(template: &str, vars: &HashMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

fn fill_value(v: &Value, vars: &HashMap<String, String>) -> Value {
    match v {
        Value::String(s) => Value::String(fill(s, vars)),
        Value::Array(a) => Value::Array(a.iter().map(|x| fill_value(x, vars)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), fill_value(x, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Split `script` into pause-aligned segments of at most `cap_s` estimated
/// seconds (Lanshu: narration is the master clock; segments break on sentence
/// boundaries, never mid-sentence when avoidable).
pub fn plan_segments(script: &str, cap_s: f64) -> Vec<Value> {
    let cap = cap_s.max(1.0);
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in script.chars() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?' | '；' | '。' | '！' | '？' | '\n') {
            let t = cur.trim();
            if !t.is_empty() {
                sentences.push(t.to_string());
            }
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        sentences.push(cur.trim().to_string());
    }
    let est = |s: &str| (s.chars().count() as f64 / SPEECH_CPS).max(1.0);
    let mut segments: Vec<Value> = Vec::new();
    let mut buf = String::new();
    let mut start = 0.0_f64;
    for s in sentences {
        let d = est(&s);
        if !buf.is_empty() && (est(&buf) + d) > cap {
            let end = start + est(&buf);
            segments.push(json!({ "index": segments.len(), "text": buf.trim(), "start_s": num(start), "end_s": num(end) }));
            start = end;
            buf.clear();
        }
        if d > cap && buf.is_empty() {
            // A single sentence longer than the cap still becomes its own segment.
            let end = start + d;
            segments.push(json!({ "index": segments.len(), "text": s, "start_s": num(start), "end_s": num(end) }));
            start = end;
            continue;
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(&s);
    }
    if !buf.trim().is_empty() {
        segments.push(json!({ "index": segments.len(), "text": buf.trim(), "start_s": num(start), "end_s": num(start + est(&buf)) }));
    }
    segments
}

/// Topological schedule: wave index per step plus start/end on the critical
/// path. Cycle detection is part of validation.
fn schedule(steps: &[Value]) -> GenResult<(HashMap<String, usize>, f64)> {
    let ids: Vec<String> = steps.iter().map(|s| tstr(s, "id")).collect();
    let mut depth: HashMap<String, usize> = HashMap::new();
    let mut end_at: HashMap<String, f64> = HashMap::new();
    let mut remaining: Vec<String> = ids.clone();
    let mut guard = 0usize;
    while !remaining.is_empty() {
        guard += 1;
        if guard > steps.len() + 2 {
            return Err(invalid("step dependencies contain a cycle"));
        }
        let mut progressed = false;
        let mut i = 0;
        while i < remaining.len() {
            let id = &remaining[i];
            let step = steps.iter().find(|s| tstr(s, "id") == *id).unwrap();
            let deps = strings(step, "depends_on");
            if deps.iter().all(|d| depth.contains_key(d)) {
                let wave = deps.iter().map(|d| depth[d] + 1).max().unwrap_or(0);
                let start = deps.iter().map(|d| end_at[d]).fold(0.0, f64::max);
                let est = step["schedule"]["est_s"].as_f64().unwrap_or(1.0);
                depth.insert(id.clone(), wave);
                end_at.insert(id.clone(), start + est);
                remaining.remove(i);
                progressed = true;
            } else {
                i += 1;
            }
        }
        if !progressed {
            return Err(invalid("step dependencies contain a cycle"));
        }
    }
    let total = end_at.values().cloned().fold(0.0, f64::max);
    Ok((depth, total))
}

/// Route readiness: is a provider configured/usable for a request? Falls back
/// to the credential status command; failures degrade to `unknown`.
fn provider_ready(ctx: &Context, provider: &str) -> (bool, String) {
    if provider.is_empty() {
        return (false, "no provider selected".into());
    }
    if let Some(adapter) = rightkit_media_gen::providers::registry()
        .iter()
        .find(|p| p.id() == provider && !p.needs_key())
    {
        return match adapter.runtime_ready() {
            Some(Err(e)) => (false, e.message),
            _ => (true, "local runtime ready".into()),
        };
    }
    match rightkit_media_gen::api::credentials_status(ctx, &json!({})) {
        Ok(v) => {
            let list = v["providers"].as_array().cloned().unwrap_or_default();
            match list.iter().find(|p| p["id"].as_str() == Some(provider)) {
                Some(p) if p["configured"].as_bool() == Some(true) => (true, "configured".into()),
                Some(_) => (
                    false,
                    format!("provider {provider} has no configured credential"),
                ),
                None => (false, format!("unknown provider: {provider}")),
            }
        }
        Err(e) => (false, format!("route readiness unknown: {e}")),
    }
}

/// Estimated cost for a request from the model catalog (0 when unknown/free).
fn est_usd(ctx: &Context, provider: &str, model: &str) -> Option<f64> {
    let models = rightkit_media_gen::api::models(ctx, &json!({})).ok()?;
    models
        .as_array()?
        .iter()
        .find(|m| m["provider"].as_str() == Some(provider) && m["model"].as_str() == Some(model))
        .and_then(|m| m["est_usd"].as_f64())
}

/// Build a workflow document from a template + inputs (pure; no writes).
fn build_workflow(ctx: &Context, template: &Value, input: &Value) -> GenResult<Value> {
    let tid = tstr(template, "id");
    let inputs = input
        .get("inputs")
        .cloned()
        .unwrap_or_else(|| input.clone());
    let mut vars: HashMap<String, String> = HashMap::new();
    if let Some(m) = inputs.as_object() {
        for (k, v) in m {
            if let Some(s) = v.as_str() {
                vars.insert(k.clone(), s.to_string());
            } else if !v.is_null() {
                vars.insert(k.clone(), v.to_string());
            }
        }
    }
    for k in strings(template, "inputs") {
        vars.entry(k).or_default();
    }

    let mut steps: Vec<Value> = template["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| fill_value(s, &vars))
        .collect();

    // Lanshu: expand the segment plan into real presenter segments.
    if tid == "lanshu_presenter" {
        let script = vars.get("script").cloned().unwrap_or_default();
        let cap = inputs
            .get("segment_cap_s")
            .and_then(Value::as_f64)
            .unwrap_or(25.0)
            .clamp(5.0, 120.0);
        let segments = plan_segments(&script, cap);
        if segments.len() > 1 {
            let base = steps
                .iter()
                .position(|s| tstr(s, "id") == "segment_1")
                .unwrap();
            let tpl = steps[base].clone();
            steps[base] = {
                let mut s = tpl.clone();
                s["id"] = json!("segment_1");
                s["label"] = json!(format!("Presenter segment 1 of {}", segments.len()));
                if let Some(r) = s.get_mut("request") {
                    r["prompt"] = json!(segments[0]["text"].as_str().unwrap_or_default());
                }
                s["segment"] = segments[0].clone();
                s
            };
            for (i, seg) in segments.iter().enumerate().skip(1) {
                let mut s = tpl.clone();
                s["id"] = json!(format!("segment_{}", i + 1));
                s["label"] = json!(format!("Presenter segment {} of {}", i + 1, segments.len()));
                if let Some(r) = s.get_mut("request") {
                    r["prompt"] = json!(seg["text"].as_str().unwrap_or_default());
                }
                s["segment"] = seg.clone();
                steps.insert(base + i, s);
            }
            // The checks step must wait on every segment.
            for s in steps.iter_mut() {
                if tstr(s, "id") == "joins_checks" || tstr(s, "id") == "compose" {
                    let mut deps: Vec<String> = (1..=segments.len())
                        .map(|i| format!("segment_{i}"))
                        .collect();
                    if tstr(s, "id") == "compose" {
                        deps.extend(["joins_checks".into(), "narration_clock".into()]);
                    }
                    s["depends_on"] = json!(deps);
                }
            }
        } else if let Some(s) = steps.iter_mut().find(|s| tstr(s, "id") == "segment_1") {
            s["segment"] = segments.first().cloned().unwrap_or(Value::Null);
            if let Some(r) = s.get_mut("request") {
                r["prompt"] = json!(segments
                    .first()
                    .and_then(|x| x["text"].as_str())
                    .unwrap_or_default());
            }
        }
    }

    apply_routes(
        &mut steps,
        input.get("routes").or_else(|| inputs.get("routes")),
        &vars,
    )?;
    refresh_support(ctx, &mut steps);

    let (waves, total_s) = schedule(&steps)?;
    for s in steps.iter_mut() {
        s["schedule"]["wave"] = json!(waves[&tstr(s, "id")]);
    }

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "kind": WORKFLOW_KIND,
        "id": clean(input.get("id")),
        "name": clean(input.get("name")),
        "template_id": tid,
        "template_label": template["label"],
        "source": template["source"],
        "inputs": inputs,
        "routes": input.get("routes").or_else(|| inputs.get("routes")).cloned().unwrap_or_else(|| json!({})),
        "steps": steps,
        "schedule": { "total_est_s": num(total_s), "waves": waves.len().saturating_sub(1) + 1 },
    }))
}

fn refresh_support(ctx: &Context, steps: &mut [Value]) {
    // Route readiness + per-step support status.
    for s in steps.iter_mut() {
        let mut support = s
            .get("support")
            .cloned()
            .unwrap_or_else(|| json!({ "status": Value::Null, "reason": Value::Null }));
        if let Some(r) = s.get("request").filter(|r| r.is_object()) {
            let provider = tstr(r, "provider");
            let model = tstr(r, "model");
            let (ok, why) = if provider == "local-image" {
                match rightkit_media_gen::providers::local_image::LocalImage::new()
                    .model_ready(&model)
                {
                    Ok(()) => (true, "installed".into()),
                    Err(e) => (false, e.message),
                }
            } else {
                provider_ready(ctx, &provider)
            };
            let est = est_usd(ctx, &provider, &model);
            s["budget"] = est
                .map(|e| json!({"est_usd": e}))
                .unwrap_or_else(|| json!({}));
            if support["status"].is_null() {
                support = if ok {
                    json!({ "status": "ready", "reason": Value::Null })
                } else {
                    json!({ "status": "route_unready", "reason": why })
                };
            } else if !ok && support["status"] == "handoff" {
                support["reason"] = json!(format!("{}; also: {}", tstr(&support, "reason"), why));
            }
        } else {
            // Native steps: probe tool availability so the plan is honest.
            let prior_reason = tstr(&support, "reason");
            let probed: Option<(bool, String)> = match s.get("native").and_then(Value::as_str) {
                Some("still_clip") => {
                    let ok = media::tools().ffmpeg.as_ref().is_some_and(|ffmpeg| {
                        let caps = media::ffmpeg_caps(ffmpeg);
                        caps.has_filter("zoompan")
                            && caps.pick_video_encoder().is_ok_and(|(_, h264)| h264)
                    });
                    Some((
                        ok,
                        if ok {
                            "ffmpeg H.264 + zoompan resolved".into()
                        } else {
                            "still_clip needs ffmpeg with H.264 + zoompan".into()
                        },
                    ))
                }
                Some("compose_video") => {
                    let ok = media::tools().ffmpeg.is_some();
                    Some((
                        ok,
                        if ok {
                            "ffmpeg resolved".into()
                        } else {
                            "no ffmpeg (bundled, RIGHTKIT_FFMPEG, or PATH)".into()
                        },
                    ))
                }
                Some("transcribe") => {
                    let ok = media::tools().whisper.is_some()
                        && media::whisper_model(&ctx.data_dir, None).is_some();
                    Some((
                        ok,
                        if ok {
                            "whisper.cpp binary + model resolved".into()
                        } else {
                            "whisper.cpp binary/model not provisioned".into()
                        },
                    ))
                }
                Some("translate") => Some((
                    true,
                    "glossary substitution; sentence MT needs a text-LLM route".into(),
                )),
                _ => None,
            };
            if let Some((ok, why)) = probed {
                support = json!({
                    "status": if ok { "native" } else { "route_unready" },
                    "reason": if ok || prior_reason.is_empty() { json!(why) } else { json!(format!("{prior_reason} {why}")) },
                });
            } else if support["status"].is_null() {
                support = json!({ "status": "native", "reason": Value::Null });
            }
        }
        s["support"] = support;
    }
}

/// Apply only registered, kind-compatible overrides before estimates/readiness/scheduling.
fn apply_routes(
    steps: &mut Vec<Value>,
    routes: Option<&Value>,
    vars: &HashMap<String, String>,
) -> GenResult<()> {
    let Some(routes) = routes else { return Ok(()) };
    let routes = routes
        .as_object()
        .ok_or_else(|| invalid("routes must be an object keyed by step id"))?;
    // Local Wav2Lip renders the full narration clock, without paid duration caps.
    if routes
        .get("segment_1")
        .is_some_and(|r| r["provider"] == rightkit_media_gen::providers::local_lipsync::ID)
    {
        let removed: HashSet<String> = steps
            .iter()
            .map(|s| tstr(s, "id"))
            .filter(|id| id.starts_with("segment_") && id != "segment_1" && id != "segment_plan")
            .collect();
        steps.retain(|s| !removed.contains(&tstr(s, "id")));
        for s in steps.iter_mut() {
            s["depends_on"] = json!(strings(s, "depends_on")
                .into_iter()
                .filter(|d| !removed.contains(d))
                .collect::<Vec<_>>());
        }
    }
    let registry = rightkit_media_gen::providers::registry();
    for (id, route) in routes {
        let pos = steps
            .iter()
            .position(|s| tstr(s, "id") == *id)
            .ok_or_else(|| invalid(format!("route {id}: unknown step")))?;
        let original = steps[pos]
            .get("route_request")
            .or_else(|| steps[pos].get("request"))
            .filter(|r| r.is_object())
            .cloned()
            .ok_or_else(|| invalid(format!("route {id}: step has no request")))?;
        if !route.is_object() {
            return Err(invalid(format!("route {id}: expected an object")));
        }
        let overrides = route
            .get("params")
            .map(|p| {
                p.as_object()
                    .ok_or_else(|| invalid(format!("route {id}: params must be an object")))
            })
            .transpose()?;
        let mut request = original.clone();
        let params = request["params"]
            .as_object_mut()
            .ok_or_else(|| invalid(format!("route {id}: request params must be an object")))?;
        if let Some(overrides) = overrides {
            params.extend(
                overrides
                    .iter()
                    .map(|(k, v)| (k.clone(), fill_value(v, vars))),
            );
        }
        if let Some(action) = route.get("native") {
            if action != "still_clip"
                || original["kind"] != "video"
                || route.get("provider").is_some()
                || route.get("model").is_some()
            {
                return Err(invalid(format!("route {id}: native still_clip requires a video request, without provider/model")));
            }
            if overrides.is_some_and(|p| {
                p.get("duration")
                    .is_some_and(|d| !d.as_f64().is_some_and(|d| d.is_finite() && d > 0.0))
            }) {
                return Err(invalid(format!(
                    "route {id}: duration must be positive seconds"
                )));
            }
            if request["params"]
                .get("image_from")
                .is_some_and(|v| !v.as_str().is_some_and(|s| !s.is_empty()))
            {
                return Err(invalid(format!("route {id}: image_from must be a step id")));
            }
            let image_from = tstr(&request["params"], "image_from");
            if !image_from.is_empty() {
                if !steps
                    .iter()
                    .any(|s| tstr(s, "id") == image_from && s["produces"] == "image")
                {
                    return Err(invalid(format!(
                        "route {id}: image_from must name an image-producing step"
                    )));
                }
                let mut deps = strings(&steps[pos], "depends_on");
                if !deps.contains(&image_from) {
                    deps.push(image_from);
                }
                steps[pos]["depends_on"] = json!(deps);
            }
            // A template's paid clip duration must not truncate the narration clock.
            if !overrides.is_some_and(|p| p.contains_key("duration")) {
                request["params"]["default_duration"] = original["params"]["duration"].clone();
                request["params"]
                    .as_object_mut()
                    .unwrap()
                    .remove("duration");
            }
            steps[pos]["route_request"] = original;
            steps[pos]["params"] = request["params"].clone();
            steps[pos].as_object_mut().unwrap().remove("request");
            steps[pos]["native"] = json!("still_clip");
            steps[pos]["budget"] = json!({"est_usd": 0});
        } else {
            let provider = field(route, "provider");
            let mut model = field(route, "model");
            if provider.is_empty() || model.is_empty() {
                return Err(invalid(format!(
                    "route {id}: provider & model are required"
                )));
            }
            let adapter = registry
                .iter()
                .find(|p| p.id() == provider)
                .ok_or_else(|| invalid(format!("route {id}: unknown provider {provider}")))?;
            if provider == "local-image" && model == "auto" {
                model = rightkit_media_gen::providers::local_image::LocalImage::new()
                    .first_installed_model()
                    .ok_or_else(|| {
                        invalid(format!(
                            "route {id}: no installed local-image profile & sd-server"
                        ))
                    })?;
            }
            let info = adapter
                .models()
                .iter()
                .find(|m| m.model == model)
                .ok_or_else(|| {
                    invalid(format!("route {id}: unknown model {model} for {provider}"))
                })?;
            let kind: rightkit_media_gen::providers::Kind =
                serde_json::from_value(original["kind"].clone())
                    .map_err(|_| invalid(format!("route {id}: invalid request kind")))?;
            if info.kind != kind {
                return Err(invalid(format!(
                    "route {id}: model {model} is incompatible with {}",
                    original["kind"]
                )));
            }
            request["provider"] = json!(provider);
            request["model"] = json!(model);
            if provider == rightkit_media_gen::providers::local_lipsync::ID {
                for n in steps[pos]["needs"].as_array_mut().into_iter().flatten() {
                    if n["kind"] == "audio" {
                        n["param"] = json!("audio_path");
                    }
                }
            }
            steps[pos]["request"] = request;
            steps[pos].as_object_mut().unwrap().remove("native");
        }
        steps[pos]["support"] = json!({"status": null, "reason": null});
    }
    schedule(steps)?;
    Ok(())
}

fn find_template(id: &str) -> GenResult<Value> {
    templates()
        .into_iter()
        .find(|t| tstr(t, "id") == id)
        .ok_or_else(|| invalid(format!("unknown template: {id}")))
}

fn plan(ctx: &Context, input: &Value) -> GenResult<Value> {
    if let Some(tpl) = input.get("template") {
        let mut wf = build_workflow(ctx, tpl, input)?;
        let report = validate(&json!({ "workflow": wf.clone() }))?;
        wf["validation"] = json!(report);
        return Ok(wf);
    }
    let tid = req_id(input, "template_id")?;
    let tpl = find_template(&tid)?;
    let mut wf = build_workflow(ctx, &tpl, input)?;
    let report = validate(&json!({ "workflow": wf.clone() }))?;
    wf["validation"] = json!(report);
    Ok(wf)
}

// ---------------------------------------------------------------------------
// validation

fn validate(input: &Value) -> GenResult<Map<String, Value>> {
    let wf = input.get("workflow").unwrap_or(input);
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let steps = wf
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if steps.is_empty() {
        errors.push("workflow has no steps".into());
    }
    let ids: HashSet<String> = steps.iter().map(|s| tstr(s, "id")).collect();
    if ids.len() != steps.len() {
        errors.push("duplicate step ids".into());
    }
    for s in &steps {
        let id = tstr(s, "id");
        if id.is_empty() {
            errors.push("a step is missing id".into());
        }
        for d in strings(s, "depends_on") {
            if !ids.contains(&d) {
                errors.push(format!("step {id} depends on unknown step {d}"));
            }
        }
        for n in s["needs"].as_array().cloned().unwrap_or_default() {
            let from = tstr(&n, "from");
            match ids.contains(&from) {
                true => {}
                false => errors.push(format!("step {id} needs artifact from unknown step {from}")),
            }
            let kind = tstr(&n, "kind");
            if !["image", "video", "audio", "file", "text", "data"].contains(&kind.as_str()) {
                errors.push(format!(
                    "step {id} has unsupported needed asset kind {kind}"
                ));
            }
        }
        if let Some(r) = s.get("request") {
            let kind = tstr(r, "kind");
            if !MEDIA_KINDS.contains(&kind.as_str()) {
                errors.push(format!("step {id} request kind {kind} is not a media kind"));
            }
            if tstr(r, "provider").is_empty() {
                warnings.push(format!("step {id} request has no provider"));
            }
        }
        match tstr(s, "kind").as_str() {
            "plan" | "voice" | "image" | "video" | "avatar" | "motion" | "export" | "check" => {}
            k => errors.push(format!("step {id} has unknown kind {k}")),
        }
        match s["support"]["status"].as_str() {
            Some("unsupported") => {
                warnings.push(format!("step {id}: {}", tstr(&s["support"], "reason")))
            }
            Some("route_unready") => {
                warnings.push(format!("step {id}: {}", tstr(&s["support"], "reason")))
            }
            _ => {}
        }
    }
    if let Err(e) = schedule(&steps) {
        errors.push(e.message);
    }
    Ok(Map::from_iter([
        ("ok".into(), json!(errors.is_empty())),
        ("errors".into(), json!(errors)),
        ("warnings".into(), json!(warnings)),
        ("steps".into(), json!(steps.len())),
    ]))
}

// ---------------------------------------------------------------------------
// workflow persistence: <data_dir>/pipelines/workflows/<id>.json

fn workflows_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("pipelines").join("workflows")
}

fn runs_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("pipelines").join("runs")
}

fn write_json(path: &Path, value: &Value) -> GenResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| GenError::new("io", format!("cannot create {}: {e}", parent.display())))?;
    }
    let text =
        serde_json::to_string_pretty(value).map_err(|e| GenError::new("io", e.to_string()))?;
    rightkit_fs::atomic_write(path, format!("{text}\n").as_bytes())
        .map_err(|e| GenError::new("io", format!("cannot write {}: {e}", path.display())))
}

fn read_json(path: &Path) -> GenResult<Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| GenError::new("not_found", format!("cannot read {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| GenError::new("io", format!("corrupt {}: {e}", path.display())))
}

fn workflow_path(ctx: &Context, id: &str) -> PathBuf {
    workflows_dir(ctx).join(format!("{}.json", slugify(id, "workflow")))
}

fn normalize_workflow(input: &Value) -> GenResult<Value> {
    let wf = input.get("workflow").unwrap_or(input);
    if wf.get("kind").and_then(Value::as_str) != Some(WORKFLOW_KIND) {
        return Err(invalid("payload must be a pipeline_workflow"));
    }
    if wf.get("schema_version").and_then(Value::as_str) != Some(SCHEMA_VERSION) {
        return Err(invalid("pipeline_workflow schema_version must be 1.0.0"));
    }
    let report = validate(&json!({ "workflow": wf.clone() }))?;
    if report["ok"] != true {
        return Err(invalid(format!(
            "workflow invalid: {}",
            report["errors"][0].as_str().unwrap_or_default()
        )));
    }
    let mut out = wf.clone();
    if tstr(&out, "id").is_empty() {
        out["id"] = json!(format!("wf-{}", short_id()));
    }
    if tstr(&out, "name").is_empty() {
        out["name"] = json!(Some(tstr(&out, "template_label"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| tstr(&out, "id")));
    }
    Ok(out)
}

fn save(ctx: &Context, input: &Value) -> GenResult<Value> {
    let mut wf = normalize_workflow(input)?;
    let id = tstr(&wf, "id");
    let path = workflow_path(ctx, &id);
    let created = if path.exists() {
        read_json(&path)
            .ok()
            .map(|old| tstr(&old, "created_at"))
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let now = rightkit_media_gen::store::now();
    wf["created_at"] = json!(created.unwrap_or_else(|| now.clone()));
    wf["updated_at"] = json!(now);
    write_json(&path, &wf)?;
    Ok(json!({ "id": id, "path": path.to_string_lossy(), "workflow": wf }))
}

fn list(ctx: &Context) -> GenResult<Value> {
    let mut items = Vec::new();
    if let Ok(read) = std::fs::read_dir(workflows_dir(ctx)) {
        for entry in read.flatten() {
            let Ok(v) = read_json(&entry.path()) else {
                continue;
            };
            if v["kind"] != WORKFLOW_KIND {
                continue;
            }
            items.push(json!({
                "id": v["id"], "name": v["name"], "template_id": v["template_id"],
                "steps": v["steps"].as_array().map_or(0, Vec::len),
                "updated_at": v["updated_at"], "created_at": v["created_at"],
            }));
        }
    }
    items.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    Ok(json!({ "workflows": items }))
}

fn get_workflow(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = req_id(input, "id")?;
    read_json(&workflow_path(ctx, &id))
}

fn delete_workflow(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = req_id(input, "id")?;
    let path = workflow_path(ctx, &id);
    if !path.exists() {
        return Err(GenError::new("not_found", format!("no workflow: {id}")));
    }
    std::fs::remove_file(&path)
        .map_err(|e| GenError::new("io", format!("cannot delete {}: {e}", path.display())))?;
    Ok(json!({ "deleted": id }))
}

// ---------------------------------------------------------------------------
// runs: durable state machine over a workflow's step DAG

const STEP_OPEN: &[&str] = &["pending", "ready", "awaiting_submit", "running"];

fn run_path(ctx: &Context, run_id: &str) -> PathBuf {
    runs_dir(ctx).join(run_id).join("run.json")
}

fn load_run(ctx: &Context, input: &Value) -> GenResult<Value> {
    let run_id = req_id(input, "run_id")?;
    read_json(&run_path(ctx, &run_id))
}

/// Atomic (`write_json` -> `rightkit_fs::atomic_write`: temp file, then replace), so a crash
/// mid-write leaves the previous record intact. Call it only while holding the run's [`RunLock`].
fn save_run(ctx: &Context, run: &Value) -> GenResult<()> {
    let run_id = tstr(run, "run_id");
    write_json(&run_path(ctx, &run_id), run)
}

/// Seconds a command waits for another process that is changing the same run record.
const RUN_LOCK_WAIT: Duration = Duration::from_secs(5);

/// Exclusive advisory lock over ONE run record (`<data>/locks/pipeline-run-<run_id>.lock`), the
/// same primitive and pattern as the rentals lock (`model_management/rental.rs`
/// `RentalLock`): rightkit-fs `FileLock` (flock kind), released on drop or when the process dies, and it
/// excludes other threads of this process as well as other processes. Every read-modify-write of
/// `run.json` runs under it, and it is held only around load -> modify -> save: never across a job
/// cancel, a media step, or any wait.
struct RunLock(rightkit_fs::FileLock);

impl RunLock {
    fn acquire(ctx: &Context, run_id: &str) -> GenResult<Self> {
        let dir = ctx.locks_dir();
        rightkit_fs::ensure_private_dir(&dir)
            .map_err(|e| GenError::new("io", format!("create {}: {e}", dir.display())))?;
        let safe: String = run_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("pipeline-run-{safe}.lock"));
        // `Flock` (not the default record lock) so threads of this process exclude each other too.
        match rightkit_fs::FileLock::try_lock_with(&path, rightkit_fs::LockKind::Flock, RUN_LOCK_WAIT) {
            Ok(lock) => Ok(Self(lock)),
            Err(rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. }) => Err(GenError::new(
                "run_busy",
                format!("another process is changing pipeline run {run_id}; nothing was changed, try again"),
            )),
            Err(e) => Err(GenError::new("io", format!("lock {}: {e}", path.display()))),
        }
    }
}

/// Passes of [`mutate_run`] before it gives up (each extra pass is a record that changed while the
/// job cancels of the previous one ran).
const MUTATE_PASSES: usize = 4;

/// Load the run, let `f` change it, save it, all under the run's lock, and return what `f`
/// returns. `f` may be called more than once and must start from the record it is given. When `f`
/// finds job cancels to make (see `fanout::Cancels`), its pass is discarded without saving, the
/// lock is released, the jobs are cancelled (provider calls happen with no lock held), and `f` runs
/// again on the freshly loaded record with the cancel results. An `Err` from `f` saves nothing.
fn mutate_run<T>(
    ctx: &Context,
    input: &Value,
    mut f: impl FnMut(&mut Value, &mut fanout::Cancels) -> GenResult<T>,
) -> GenResult<T> {
    let run_id = req_id(input, "run_id")?;
    let mut cancels = fanout::Cancels::default();
    for _ in 0..MUTATE_PASSES {
        {
            if !run_path(ctx, &run_id).is_file() {
                return Err(read_json(&run_path(ctx, &run_id))
                    .err()
                    .unwrap_or_else(|| GenError::new("not_found", format!("no run: {run_id}"))));
            }
            let _lock = RunLock::acquire(ctx, &run_id)?;
            let mut run = read_json(&run_path(ctx, &run_id))?;
            cancels.wanted.clear();
            let out = f(&mut run, &mut cancels);
            if cancels.wanted.is_empty() {
                let value = out?;
                save_run(ctx, &run)?;
                return Ok(value);
            }
        }
        for job_id in std::mem::take(&mut cancels.wanted) {
            let note = fanout::cancel_job_note(ctx, &job_id);
            cancels.done.insert(job_id, note);
        }
    }
    Err(GenError::new(
        "run_busy",
        format!(
            "pipeline run {run_id} kept changing while its jobs were being cancelled; try again"
        ),
    ))
}

fn run_step<'a>(run: &'a Value, step_id: &str) -> Option<&'a Value> {
    run["steps"]
        .as_array()?
        .iter()
        .find(|s| s["id"].as_str() == Some(step_id))
}

fn run_step_mut<'a>(run: &'a mut Value, step_id: &str) -> Option<&'a mut Value> {
    run["steps"]
        .as_array_mut()?
        .iter_mut()
        .find(|s| s["id"].as_str() == Some(step_id))
}

/// Recompute each step's `ready` flag: deps all succeeded, self still open,
/// and the route is supported. Returns the run with fresh statuses.
fn refresh(run: &mut Value) {
    let steps = run["steps"].as_array().cloned().unwrap_or_default();
    let done: HashSet<String> = steps
        .iter()
        .filter(|s| s["status"] == "succeeded" || s["status"] == "skipped")
        .filter_map(|s| s["id"].as_str().map(String::from))
        .collect();
    let cancelled = run["status"] == "cancelled";
    let mut any_open = false;
    let mut any_failed = false;
    let mut any_running = false;
    for s in run["steps"].as_array_mut().unwrap().iter_mut() {
        let status = s["status"].as_str().unwrap_or("pending").to_string();
        if status == "failed" {
            any_failed = true;
        }
        if status == "running" || status == "awaiting_submit" {
            any_running = true;
        }
        if !STEP_OPEN.contains(&status.as_str()) {
            continue;
        }
        any_open = true;
        if cancelled {
            s["status"] = json!("cancelled");
            continue;
        }
        let deps_ok = strings(s, "depends_on").iter().all(|d| done.contains(d));
        let supported = !matches!(
            s["support"]["status"].as_str(),
            Some("unsupported") | Some("route_unready")
        );
        if status == "pending" && deps_ok && supported {
            s["status"] = json!("ready");
        } else if status == "pending" && !supported {
            // Terminal non-ready state keeps the support label (`unsupported`
            // vs `route_unready`) so the UI can offer provisioning guidance.
            s["status"] = json!(s["support"]["status"].as_str().unwrap_or("unsupported"));
        }
    }
    let total = run["steps"].as_array().map_or(0, Vec::len);
    let done_count = run["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| {
            matches!(
                s["status"].as_str(),
                Some("succeeded" | "skipped" | "unsupported" | "route_unready")
            )
        })
        .count();
    let needs_review = run["steps"].as_array().unwrap().iter().any(|s| {
        s["status"] == "failed"
            && s["error"]
                .as_str()
                .is_some_and(|e| e.starts_with("needs_review"))
    });
    run["status"] = json!(if cancelled {
        "cancelled"
    } else if any_running {
        "running"
    } else if needs_review {
        "needs_review"
    } else if done_count == total {
        if any_failed {
            "partial"
        } else {
            "succeeded"
        }
    } else if any_open || any_failed {
        "running"
    } else {
        "queued"
    });
    run["updated_at"] = json!(rightkit_media_gen::store::now());
}

/// Build run steps from a workflow.
fn run_steps(wf: &Value) -> Vec<Value> {
    wf["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| {
            let status = match s["support"]["status"].as_str() {
                Some("unsupported") => "unsupported",
                Some("route_unready") => "route_unready",
                _ => "pending",
            };
            json!({
                "id": s["id"], "label": s["label"], "kind": s["kind"],
                "depends_on": s["depends_on"], "needs": s["needs"], "produces": s["produces"],
                "request": s.get("request").cloned().unwrap_or(Value::Null),
                "native": s.get("native").cloned().unwrap_or(Value::Null),
                "support": s["support"], "optional": s.get("optional").cloned().unwrap_or(json!(false)),
                "schedule": s["schedule"], "budget": s.get("budget").cloned().unwrap_or(json!({})),
                "retry": s["retry"], "segment": s.get("segment").cloned().unwrap_or(Value::Null),
                "status": status, "attempt": 0, "job_id": Value::Null, "assets": [],
                "error": Value::Null, "started_at": Value::Null, "finished_at": Value::Null,
                "params": s.get("params").cloned().unwrap_or(json!({})),
            })
        })
        .collect()
}

fn run_create(ctx: &Context, input: &Value) -> GenResult<Value> {
    let mut wf = if let Some(id) = input.get("workflow_id").and_then(Value::as_str) {
        read_json(&workflow_path(ctx, id))?
    } else if input.get("workflow").is_some() {
        normalize_workflow(input)?
    } else {
        return Err(invalid("run_create needs workflow_id or workflow"));
    };
    if let Some(inputs) = input.get("inputs") {
        let inputs = inputs
            .as_object()
            .ok_or_else(|| invalid("run inputs must be an object"))?;
        let values = wf["inputs"]
            .as_object_mut()
            .ok_or_else(|| invalid("workflow inputs must be an object"))?;
        values.extend(inputs.clone());
    }
    let vars = wf["inputs"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut steps = wf["steps"].as_array().cloned().unwrap_or_default();
    apply_routes(
        &mut steps,
        input
            .get("routes")
            .or_else(|| wf["inputs"].get("routes"))
            .or_else(|| wf.get("routes")),
        &vars,
    )?;
    for s in &mut steps {
        if s["support"]["status"] != "unsupported" {
            s["support"] = json!({"status": null, "reason": null});
        }
    }
    let (waves, _) = schedule(&steps)?;
    for s in &mut steps {
        s["schedule"]["wave"] = json!(waves[&tstr(s, "id")]);
    }
    refresh_support(ctx, &mut steps);
    wf["steps"] = json!(steps);
    let mut run = json!({
        "schema_version": SCHEMA_VERSION,
        "kind": RUN_KIND,
        "run_id": format!("run-{}", short_id()),
        "workflow_id": wf["id"], "workflow_name": wf["name"], "template_id": wf["template_id"],
        "status": "queued",
        "budget_usd": input.get("budget_usd").cloned().unwrap_or(Value::Null),
        "spent_usd": 0,
        "inputs": wf["inputs"],
        "steps": run_steps(&wf),
        "created_at": rightkit_media_gen::store::now(),
        "updated_at": rightkit_media_gen::store::now(),
    });
    refresh(&mut run);
    save_run(ctx, &run)?;
    Ok(run)
}

fn run_list(ctx: &Context) -> GenResult<Value> {
    let mut items = Vec::new();
    if let Ok(read) = std::fs::read_dir(runs_dir(ctx)) {
        for entry in read.flatten() {
            let Ok(v) = read_json(&entry.path().join("run.json")) else {
                continue;
            };
            if v["kind"] != RUN_KIND {
                continue;
            }
            items.push(json!({
                "run_id": v["run_id"], "workflow_name": v["workflow_name"], "template_id": v["template_id"],
                "status": v["status"], "spent_usd": v["spent_usd"], "budget_usd": v["budget_usd"],
                "created_at": v["created_at"], "updated_at": v["updated_at"],
                "run_dir": entry.path().to_string_lossy(),
            }));
        }
    }
    items.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    Ok(json!({ "runs": items }))
}

fn run_get(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, _| {
        refresh(run);
        Ok(run.clone())
    })
}

fn run_ready(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, _| {
        refresh(run);
        let ready: Vec<Value> = run["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["status"] == "ready")
            .cloned()
            .collect();
        Ok(json!({ "run_id": run["run_id"], "status": run["status"], "ready": ready }))
    })
}

/// Resolve `needs`: pull produced assets of the right kind from dependency
/// steps and inject them into the request params.
fn resolve_needs(run: &Value, step: &Value, request: &mut Value) -> GenResult<()> {
    let steps = run["steps"].as_array().cloned().unwrap_or_default();
    for n in step["needs"].as_array().cloned().unwrap_or_default() {
        let from = tstr(&n, "from");
        let kind = tstr(&n, "kind");
        let param = tstr(&n, "param");
        let dep = steps
            .iter()
            .find(|s| s["id"].as_str() == Some(from.as_str()))
            .ok_or_else(|| invalid(format!("dependency {from} is missing from this run")))?;
        if dep["status"] != "succeeded" {
            return Err(invalid(format!(
                "dependency {from} has not succeeded (status {})",
                tstr(dep, "status")
            )));
        }
        let asset = dep["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["kind"].as_str() == Some(kind.as_str())))
            .cloned()
            .ok_or_else(|| invalid(format!("dependency {from} produced no {kind} asset")))?;
        let path = tstr(&asset, "path");
        if path.is_empty() {
            return Err(invalid(format!(
                "dependency {from} produced a {kind} asset with no path"
            )));
        }
        request["params"][param] = json!(path);
    }
    Ok(())
}

/// Collect a produced asset of `kind` from step `dep_id` (must exist on disk).
fn dep_asset(run: &Value, dep_id: &str, kind: &str) -> Option<Value> {
    run_step(run, dep_id)?["assets"]
        .as_array()?
        .iter()
        .find(|a| a["kind"].as_str() == Some(kind) && Path::new(tstr(a, "path").as_str()).is_file())
        .cloned()
}

/// First on-disk asset of `kind` produced anywhere in the run.
fn run_asset(run: &Value, kind: &str) -> Option<Value> {
    run["steps"]
        .as_array()?
        .iter()
        .find_map(|s| dep_asset(run, &tstr(s, "id"), kind))
}

fn input_paths(run: &Value, key: &str) -> Vec<String> {
    if run["inputs"][key].is_array() {
        strings(&run["inputs"], key)
    } else {
        clean(run["inputs"].get(key))
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    }
}

fn upstream_audio(run: &Value, step: &Value) -> Option<Value> {
    let mut pending = strings(step, "depends_on");
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(a) = dep_asset(run, &id, "audio") {
            return Some(a);
        }
        if let Some(s) = run_step(run, &id) {
            pending.extend(strings(s, "depends_on"));
        }
    }
    None
}

fn audio_duration(path: &str) -> Option<f64> {
    media::probe(Path::new(path))
        .ok()?
        .get("duration_s")?
        .as_f64()
        .filter(|d| d.is_finite() && *d > 0.0)
}

/// Media-producing native steps (`transcribe`, `translate`, `compose_video`)
/// return a ready-made asset rather than text-to-write.
fn media_step_asset(ctx: &Context, run: &Value, step: &Value, dir: &Path) -> GenResult<Value> {
    let action = tstr(step, "native");
    let step_id = tstr(step, "id");
    match action.as_str() {
        "transcribe" => {
            // Audio source: this run's `source_media` input or an upstream audio asset.
            let audio = dep_asset(
                run,
                &strings(step, "depends_on")
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                "audio",
            )
            .map(|a| tstr(&a, "path"))
            .filter(|p| !p.is_empty())
            .or_else(|| Some(clean(run["inputs"].get("source_media"))).filter(|p| !p.is_empty()))
            .unwrap_or_default();
            if audio.is_empty() {
                return Err(invalid(format!("step {step_id}: transcribe needs inputs.source_media or an upstream audio asset")));
            }
            let out = asr::transcribe(
                ctx,
                &json!({
                    "audio_path": audio,
                    "lang": clean(run["inputs"].get("source_lang")),
                    "model": clean(run["inputs"].get("whisper_model")),
                    "out_dir": dir.to_string_lossy(),
                }),
            )?;
            let srt_path = PathBuf::from(tstr(&out, "srt_path"));
            let bytes = std::fs::read(&srt_path).map_err(|e| GenError::new("io", e.to_string()))?;
            Ok(json!({
                "asset_id": format!("{}-{}", tstr(run, "run_id"), step_id),
                "path": srt_path.to_string_lossy(), "sha256": rightkit_media_gen::library::sha256_hex(&bytes),
                "bytes": bytes.len(), "mime": "application/x-subrip", "kind": "file",
                "engine": out["engine"], "model": out["model"], "segment_count": out["segment_count"],
            }))
        }
        "dub_plan" => dubbing::plan_step(ctx, run, step, dir),
        "translate" => {
            let dubbing_run = tstr(run, "template_id") == "opencreator_dubbing";
            if dubbing_run {
                // A dub names both languages, and they differ, and the text is really translated.
                dubbing::check_languages(&run["inputs"])?;
                if !dubbing::is_passthrough(&run["inputs"])
                    && clean(run["inputs"].get("translate_provider")).is_empty()
                {
                    return Err(GenError::new(
                        "dubbing_needs_llm_translation",
                        format!("step {step_id}: a dub needs translate_provider (a text-LLM route); glossary substitution is not a translation"),
                    ));
                }
            }
            // Source text: translated input, or the transcribed SRT cues joined.
            let mut text = clean(run["inputs"].get("dubbed_script"));
            if text.is_empty() {
                text = clean(run["inputs"].get("script"));
            }
            let mut input = json!({
                "text": text, "target_lang": clean(run["inputs"].get("target_lang")),
                "provider": clean(run["inputs"].get("translate_provider")),
                "mode": clean(run["inputs"].get("translate_mode")),
            });
            if let Some(g) = run["inputs"].get("glossary") {
                input["glossary"] = g.clone();
            }
            // Translate the dependency's transcript segments when present.
            for dep_id in strings(step, "depends_on") {
                if let Some(a) = dep_asset(run, &dep_id, "file") {
                    if let Ok(srt) = std::fs::read_to_string(tstr(&a, "path")) {
                        input["segments"] = json!(asr::parse_srt(&srt));
                        input.as_object_mut().unwrap().remove("text");
                    }
                }
            }
            if tstr(&input, "target_lang").is_empty() {
                input["target_lang"] = json!("en");
            }
            // The run id keys this run's spend ledger entries. A partial result
            // saved by an earlier attempt in the same run folder is resumed, so
            // finished segments are not paid for twice.
            input["run_id"] = json!(tstr(run, "run_id"));
            let partial_path = dir.join(format!("{step_id}.partial.json"));
            if let Some(prev) = std::fs::read_to_string(&partial_path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                input["previous"] = prev;
            }
            let out = translate::translate(ctx, &input)?;
            let path = dir.join(format!("{step_id}.txt"));
            let segments = out["segments"].as_array().cloned().unwrap_or_default();
            let body = if out["segments"].is_array() {
                segments
                    .iter()
                    .map(|s| tstr(s, "text"))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                tstr(&out, "text")
            };
            // An LLM result that is not complete never becomes an asset: the
            // translated segments are kept in the partial payload and the
            // text file is marked, then the step fails with the count.
            if out["method"] == "llm" && out["complete"] != json!(true) {
                let done = out["translated_count"].as_u64().unwrap_or(0);
                let total = segments.len();
                let marked = format!(
                    "[PARTIAL translation: {done} of {total} segments translated; untranslated segments keep their source text]\n{body}"
                );
                rightkit_fs::atomic_write(&path, marked.as_bytes())
                    .map_err(|e| GenError::new("io", e.to_string()))?;
                rightkit_fs::atomic_write(
                    &partial_path,
                    &serde_json::to_vec_pretty(&out).unwrap_or_default(),
                )
                .map_err(|e| GenError::new("io", e.to_string()))?;
                let manual = out["manual_text_segments"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let reason = match out["error"]["message"].as_str() {
                    Some(message) => message.to_string(),
                    None if !manual.is_empty() => format!(
                        "segments {} need manual text (abandoned after an unknown outcome; not re-sent)",
                        manual.iter().filter_map(Value::as_u64).map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
                    ),
                    None => "translation stopped".to_string(),
                };
                return Err(GenError::new(
                    "translate_incomplete",
                    format!(
                        "{reason}; {done} of {total} segments translated and kept in {}; re-run this run to resume without paying for them again",
                        partial_path.display()
                    ),
                ));
            }
            rightkit_fs::atomic_write(&path, body.as_bytes())
                .map_err(|e| GenError::new("io", e.to_string()))?;
            let _ = std::fs::remove_file(&partial_path);
            // The full result (segment ids and timings with source and target text) is what the
            // dub plan and the per-segment voice jobs read.
            let segments_path = dir.join(format!("{step_id}.segments.json"));
            rightkit_fs::atomic_write(
                &segments_path,
                &serde_json::to_vec_pretty(&out).unwrap_or_default(),
            )
            .map_err(|e| GenError::new("io", e.to_string()))?;
            let bytes = std::fs::read(&path).map_err(|e| GenError::new("io", e.to_string()))?;
            Ok(json!({
                "asset_id": format!("{}-{}", tstr(run, "run_id"), step_id),
                "path": path.to_string_lossy(), "sha256": rightkit_media_gen::library::sha256_hex(&bytes),
                "segments_path": segments_path.to_string_lossy(),
                "bytes": bytes.len(), "mime": "text/plain", "kind": "text",
                "method": out["method"], "machine_translation": out["machine_translation"],
                "terms_applied": out["terms_applied"],
            }))
        }
        "still_clip" => {
            let params = &step["params"];
            let from = tstr(params, "image_from");
            let image = if !from.is_empty() {
                dep_asset(run, &from, "image").map(|a| tstr(&a, "path"))
            } else {
                step["needs"].as_array().into_iter().flatten().filter(|n| n["kind"] == "image")
                    .find_map(|n| dep_asset(run, &tstr(n, "from"), "image").map(|a| tstr(&a, "path")))
                    .or_else(|| input_paths(run, "asset_paths").into_iter().find(|p| compose::is_image(Path::new(p))))
            }.ok_or_else(|| invalid(format!("step {step_id}: still_clip needs an image need, params.image_from or asset_paths image")))?;
            let clock = upstream_audio(run, step).and_then(|a| audio_duration(&tstr(&a, "path")));
            let duration = params
                .get("duration")
                .and_then(Value::as_f64)
                .or(clock)
                .or_else(|| params.get("default_duration").and_then(Value::as_f64))
                .unwrap_or(4.0);
            if !duration.is_finite() || duration <= 0.0 {
                return Err(invalid(format!(
                    "step {step_id}: duration must be positive"
                )));
            }
            let out = compose::compose(
                ctx,
                &json!({
                    "inputs": [{"path": image, "duration_s": duration}], "ken_burns": true,
                    "require_h264": true, "require_ken_burns": true,
                    "width": params.get("width").and_then(Value::as_u64).unwrap_or(1280),
                    "height": params.get("height").and_then(Value::as_u64).unwrap_or(720),
                    "out_dir": dir.to_string_lossy(),
                }),
            )?;
            Ok(json!({
                "asset_id": format!("{}-{}", tstr(run, "run_id"), step_id),
                "path": out["out_path"], "sha256": out["sha256"], "bytes": out["bytes"],
                "mime": "video/mp4", "kind": "video", "duration_s": out["probe"]["duration_s"],
                "decode_ok": out["decode_ok"], "warnings": out["warnings"],
            }))
        }
        "compose_video" => {
            // Visual inputs in dependency order (video/image assets), then any
            // free input paths from `asset_paths`/`image_paths`.
            let mut inputs: Vec<Value> = Vec::new();
            // Dubbing-style runs keep the source video as the visual base —
            // but only when it actually has a video stream (an audio-only
            // source feeds narration instead).
            let sm = clean(run["inputs"].get("source_media"));
            let sm_has_video = !sm.is_empty()
                && media::probe(Path::new(&sm))
                    .ok()
                    .and_then(|p| p["video"].as_object().map(|_| true))
                    .unwrap_or(false);
            if sm_has_video {
                inputs.push(json!({ "path": sm }));
            }
            for dep_id in strings(step, "depends_on") {
                for kind in ["video", "image"] {
                    if let Some(a) = dep_asset(run, &dep_id, kind) {
                        inputs.push(json!({ "path": a["path"] }));
                    }
                }
            }
            for key in ["asset_paths", "image_paths"] {
                // Free visuals are fallback inputs, not extra clips after rendered scenes.
                if inputs.is_empty() {
                    for p in input_paths(run, key) {
                        inputs.push(json!({ "path": p }));
                    }
                }
            }
            if inputs.is_empty() {
                return Err(invalid(format!("step {step_id}: compose needs produced video/image assets or inputs.asset_paths")));
            }
            let audio = strings(step, "depends_on")
                .iter()
                .find_map(|d| dep_asset(run, d, "audio"))
                .or_else(|| run_asset(run, "audio"))
                .map(|a| tstr(&a, "path"))
                .or_else(|| Some(clean(run["inputs"].get("audio_path"))).filter(|p| !p.is_empty()))
                .or_else(|| (!sm.is_empty() && !sm_has_video).then(|| sm.clone()));
            let subs = strings(step, "depends_on")
                .iter()
                .find_map(|d| dep_asset(run, d, "file"))
                .or_else(|| run_asset(run, "file"))
                .map(|a| tstr(&a, "path"))
                .filter(|p| p.ends_with(".srt") || p.ends_with(".vtt"))
                .or_else(|| {
                    Some(clean(run["inputs"].get("subtitles_path"))).filter(|p| !p.is_empty())
                });
            // Image-only compositions span the real narration, divided across cards.
            if let Some(duration) = audio.as_ref().and_then(|p| audio_duration(p)) {
                if inputs
                    .iter()
                    .all(|i| compose::is_image(Path::new(&tstr(i, "path"))))
                {
                    let each = duration / inputs.len() as f64;
                    for i in &mut inputs {
                        i["duration_s"] = json!(each);
                    }
                }
            }
            // Dubbing: the dub replaces the original audio unless `original_audio` is "mix", which
            // feeds the source file through compose's existing music leg (its level and the
            // loudness normalisation are compose's, unchanged).
            let mut music = clean(run["inputs"].get("music_path"));
            if music.is_empty()
                && tstr(run, "template_id") == "opencreator_dubbing"
                && clean(run["inputs"].get("original_audio")) == "mix"
                && !sm.is_empty()
            {
                let has_audio = media::probe(Path::new(&sm))
                    .ok()
                    .is_some_and(|p| p["audio"].is_object());
                if !has_audio {
                    return Err(invalid(format!("step {step_id}: original_audio \"mix\" but source_media has no audio stream")));
                }
                music = sm.clone();
            }
            let spec = json!({
                "inputs": inputs,
                "require_h264": (["vox_director", "shotcraft", "lanshu_presenter", "brag_launch"].contains(&tstr(run, "template_id").as_str())),
                "audio_path": audio.unwrap_or_default(),
                "music_path": music,
                "subtitles_path": subs.unwrap_or_default(),
                "burn_subtitles": run["inputs"].get("burn_subtitles").and_then(|v| v.as_bool()).unwrap_or(true),
                "normalize_loudness": run["inputs"].get("normalize_loudness").and_then(|v| v.as_bool()).unwrap_or(true),
                "ken_burns": run["inputs"].get("ken_burns").and_then(|v| v.as_bool()).unwrap_or(tstr(run, "template_id") == "brag_launch"),
                "out_dir": dir.to_string_lossy(),
            });
            let out = compose::compose(ctx, &spec)?;
            if out["decode_ok"] != true {
                return Err(GenError::new(
                    "compose_failed",
                    format!("step {step_id}: output failed full decode"),
                ));
            }
            Ok(json!({
                "asset_id": format!("{}-{}", tstr(run, "run_id"), step_id),
                "path": out["out_path"], "sha256": out["sha256"], "bytes": out["bytes"],
                "mime": "video/mp4", "kind": "video",
                "duration_s": out["probe"]["duration_s"].clone(),
                "loudness": out["loudness"], "decode_ok": out["decode_ok"], "warnings": out["warnings"], "subtitle_mode": out["subtitle_mode"],
            }))
        }
        other => Err(invalid(format!("unknown native action: {other}"))),
    }
}

/// SubRip text for `(start_s, end_s, text)` cues, one text line per cue.
fn srt_text(timed: &[(f64, f64, String)]) -> String {
    let cues: Vec<rightkit_transcript::Cue> = timed
        .iter()
        .map(|(start, end, text)| rightkit_transcript::Cue {
            start_ms: (start * 1000.0).round() as u64,
            end_ms: (end * 1000.0).round() as u64,
            lines: vec![text.clone()],
            first_word: 0,
            last_word: 0,
        })
        .collect();
    rightkit_transcript::to_srt(&cues, rightkit_transcript::Newline::Lf)
}

/// Record the three asset facts on a native step asset derived from media (transcript, translation,
/// still clip, composed video). The parents are every asset of the step's dependencies plus the
/// run's input media paths, so the derived rights are the most restrictive of them; a library
/// file contributes its recorded facts, any other file is `unknown`, and an unused input can only
/// make the result more restrictive, never less. Plan, check and export steps are working data
/// derived from typed text and stay unstamped. The run record is the only place these facts live
/// (the file is in the run folder, not the library, so the gallery reads it as `unknown`).
fn stamp_pipeline_facts(ctx: &Context, run: &Value, step: &Value, mut asset: Value) -> Value {
    use rightkit_media_gen::provenance as prov;
    let action = tstr(step, "native");
    if !matches!(
        action.as_str(),
        "transcribe"
            | "translate"
            | "still_clip"
            | "compose_video"
            | "dub_plan"
            | "cutright_finish"
    ) {
        return asset;
    }
    let mut parents: Vec<prov::Parent> = Vec::new();
    for dep_id in strings(step, "depends_on") {
        if let Some(dep) = run_step(run, &dep_id) {
            for a in dep["assets"].as_array().into_iter().flatten() {
                parents.push(prov::parent_of_recorded(ctx, a));
            }
        }
    }
    if action != "translate" {
        for key in ["source_media", "audio_path", "music_path", "subtitles_path"] {
            let path = clean(run["inputs"].get(key));
            if !path.is_empty() {
                parents.push(prov::parent_of_path(ctx, Path::new(&path)));
            }
        }
        for key in ["asset_paths", "image_paths"] {
            for path in input_paths(run, key) {
                parents.push(prov::parent_of_path(ctx, Path::new(&path)));
            }
        }
    }
    let facts = prov::derived_facts(
        "rightkit-media-pipeline",
        &format!("native:{action}"),
        &tstr(run, "run_id"),
        &parents,
    );
    asset["facts"] = facts.to_value();
    asset
}

/// Native step actions that produce artifacts inside the run folder.
fn run_native(ctx: &Context, run: &Value, step: &Value) -> GenResult<Value> {
    let action = tstr(step, "native");
    let run_id = tstr(run, "run_id");
    let dir = runs_dir(ctx).join(&run_id).join("artifacts");
    rightkit_fs::ensure_private_dir(&dir)
        .map_err(|e| GenError::new("io", format!("cannot create {}: {e}", dir.display())))?;
    let step_id = tstr(step, "id");

    if action == "cutright_finish" {
        return finish::step_asset(ctx, run, step, &dir);
    }
    if matches!(
        action.as_str(),
        "transcribe" | "translate" | "compose_video" | "still_clip" | "dub_plan"
    ) {
        return media_step_asset(ctx, run, step, &dir);
    }

    let (name, text, kind, mime) = match action.as_str() {
        "export_srt" => {
            if let Some(aligned) = dubbing::aligned_srt(run) {
                // Dubbing: the plan's final placed timings (segment-level alignment), never an estimate.
                (
                    format!("{step_id}.srt"),
                    aligned,
                    "file",
                    "application/x-subrip",
                )
            } else {
                // OpenCreator: one cue per subtitle line; timing comes from the
                // narration clock estimate when real durations are unknown.
                let script = {
                    let dubbed = clean(run["inputs"].get("dubbed_script"));
                    if dubbed.is_empty() {
                        clean(run["inputs"].get("script"))
                    } else {
                        dubbed
                    }
                };
                let cues: Vec<String> = script
                    .split(|c| c == '.' || c == '!' || c == '?' || c == '\n')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut timed = Vec::with_capacity(cues.len());
                let mut clock = 0.0_f64;
                for cue in cues.iter() {
                    let dur = (cue.chars().count() as f64 / SPEECH_CPS).max(1.0);
                    timed.push((clock, clock + dur, cue.clone()));
                    clock += dur;
                }
                (
                    format!("{step_id}.srt"),
                    srt_text(&timed),
                    "file",
                    "application/x-subrip",
                )
            }
        }
        "export_text" | "script" => {
            let script = clean(run["inputs"].get("script"));
            let text = if script.is_empty() {
                clean(run["inputs"].get("story"))
            } else {
                script
            };
            (format!("{step_id}.txt"), text, "text", "text/plain")
        }
        "beats" | "shot_plan" | "segment_plan" => {
            let data = json!({
                "step": step_id,
                "segments": step.get("segment").cloned().unwrap_or(Value::Null),
                "depends_on": step["depends_on"],
                "est_s": step["schedule"]["est_s"],
            });
            (
                format!("{step_id}.json"),
                serde_json::to_string_pretty(&data).unwrap_or_default(),
                "data",
                "application/json",
            )
        }
        "check_inputs" | "check_delivery_intent" | "check_presenter" => {
            // Honest check step: verify upstream artifacts exist on disk and
            // record per-dependency findings. Real quality gates (lip-sync,
            // loudness) are reported as unverified rather than claimed.
            let mut findings = Vec::new();
            for dep_id in strings(step, "depends_on") {
                if let Some(dep) = run_step(run, &dep_id) {
                    for a in dep["assets"].as_array().cloned().unwrap_or_default() {
                        let p = tstr(&a, "path");
                        findings.push(json!({
                            "from": dep_id, "asset": a["asset_id"], "path": p,
                            "exists": !p.is_empty() && Path::new(&p).is_file(),
                        }));
                    }
                }
            }
            let data = json!({
                "step": step_id, "action": action,
                "checks": { "artifacts_exist": findings.iter().all(|f| f["exists"] == true), "findings": findings },
                "unverified": ["lip_sync", "loudness", "motion_quality"],
                "note": "Artifact existence is checked on disk; perceptual quality gates remain manual/provider-reported.",
            });
            (
                format!("{step_id}.json"),
                serde_json::to_string_pretty(&data).unwrap_or_default(),
                "data",
                "application/json",
            )
        }
        other => return Err(invalid(format!("unknown native action: {other}"))),
    };

    let path = dir.join(&name);
    rightkit_fs::atomic_write(&path, text.as_bytes())
        .map_err(|e| GenError::new("io", format!("cannot write {}: {e}", path.display())))?;
    let bytes = std::fs::read(&path).map_err(|e| GenError::new("io", e.to_string()))?;
    Ok(json!({
        "asset_id": format!("{}-{}", run_id, step_id),
        "path": path.to_string_lossy(),
        "sha256": rightkit_media_gen::library::sha256_hex(&bytes),
        "bytes": bytes.len(),
        "mime": mime,
        "kind": kind,
    }))
}

/// The step's request with its stored route dropped and `needs` resolved, plus (for the dub step) the
/// per-segment requests, one per translated segment.
fn request_and_segments(
    run: &Value,
    step: &Value,
    step_id: &str,
) -> GenResult<(Value, Option<Vec<Value>>)> {
    let mut request = step["request"].clone();
    // A stored engine route is stale data from an earlier run, not a caller's claim; drop it so the resubmission is not refused.
    rightkit_media_gen::routing::strip_saved_route(&mut request);
    if request.is_null() {
        return Err(invalid(format!(
            "step {step_id} has no request and no native action"
        )));
    }
    resolve_needs(run, step, &mut request)?;
    // Dubbing: one voice job per translated segment (the base request is the template's route).
    let per_segment = if step["id"] == "dub" && tstr(run, "template_id") == "opencreator_dubbing" {
        Some(dubbing::voice_requests(run, &request)?)
    } else {
        None
    };
    Ok((request, per_segment))
}

/// A multi-job step that was begun and whose caller lost its place (window reload, crash): the
/// requests of the segments with no attached job, at the SAME attempt and request ids. Nothing is
/// started or counted again; submitting them finds any job already created under the same id.
fn step_resume(run: Value, step_id: &str, step: &Value) -> GenResult<Value> {
    let (request, per_segment) = request_and_segments(&run, step, step_id)?;
    let all =
        per_segment.ok_or_else(|| invalid(format!("step {step_id} is not a per-segment step")))?;
    let requests = fanout::resume_requests(step, all);
    Ok(json!({
        "step_id": step_id, "run_id": run["run_id"], "executed": "resume",
        "requests": requests, "request": request,
        "settle_note": "submit each requests[].request as given, attach each job with step_attach {job_id, segment_id}, then poll step_status; no attempt was started",
        "run": run,
    }))
}

/// What the first, locked part of `step_begin` decided.
enum Begin {
    /// Everything was done under the lock (a hand-off, a resume, a fully reused step).
    Done(Value),
    /// A native step was claimed (`awaiting_submit`, attempt counted, saved); its work runs with NO lock
    /// held. `before` is the step as it was, to put back if the work fails.
    Native {
        run: Value,
        step: Value,
        before: Value,
    },
}

fn step_begin(ctx: &Context, input: &Value) -> GenResult<Value> {
    let run_id = req_id(input, "run_id")?;
    let step_id_in = field(input, "step_id");
    let begin = {
        if !run_path(ctx, &run_id).is_file() {
            return Err(load_run(ctx, input)
                .err()
                .unwrap_or_else(|| GenError::new("not_found", format!("no run: {run_id}"))));
        }
        let _lock = RunLock::acquire(ctx, &run_id)?;
        begin_claim(ctx, input)?
    };
    let (run0, step, before) = match begin {
        Begin::Done(v) => return Ok(v),
        Begin::Native { run, step, before } => (run, step, before),
    };
    let step_id = step_id_in;
    // Native artifact steps run synchronously, outside the run lock (transcribe, translate, ffmpeg
    // composition and the provenance stamp can take long). The claim above keeps a second begin out.
    let work = run_native(ctx, &run0, &step).map(|a| stamp_pipeline_facts(ctx, &run0, &step, a));
    let _lock = RunLock::acquire(ctx, &run_id)?;
    let mut run = load_run(ctx, input)?;
    let now = rightkit_media_gen::store::now();
    match work {
        Err(e) => {
            // Give the claim back: the step is as it was before this begin.
            if let Some(s) = run_step_mut(&mut run, &step_id) {
                if s["status"] == "awaiting_submit" {
                    for k in ["status", "attempt", "started_at", "error"] {
                        s[k] = before[k].clone();
                    }
                    refresh(&mut run);
                    save_run(ctx, &run)?;
                }
            }
            Err(e)
        }
        Ok(asset) => {
            // A dub plan that needs a decision (overflow, track past the media) holds the run: the step
            // fails with the reasons and the plan is kept in its assets; retry with new `inputs`.
            let review = asset["review_status"] == "needs_review";
            {
                let s = run_step_mut(&mut run, &step_id)
                    .ok_or_else(|| invalid(format!("no step: {step_id}")))?;
                if !matches!(
                    s["status"].as_str(),
                    Some("awaiting_submit" | "ready" | "pending")
                ) {
                    return Err(GenError::new(
                        "step_changed",
                        format!(
                            "step {step_id} became {} while it ran; its output {} was not recorded",
                            tstr(s, "status"),
                            tstr(&asset, "path")
                        ),
                    ));
                }
                s["status"] = json!(if review { "failed" } else { "succeeded" });
                s["error"] = if review {
                    json!(format!(
                        "needs_review: {} | options: {}",
                        tstr(&asset, "review_summary"),
                        strings(&asset, "options").join(" | ")
                    ))
                } else {
                    Value::Null
                };
                s["attempt"] = json!(before["attempt"].as_i64().unwrap_or(0) + 1);
                s["assets"] = json!([asset.clone()]);
                s["started_at"] = json!(now);
                s["finished_at"] = json!(now);
            }
            refresh(&mut run);
            save_run(ctx, &run)?;
            Ok(
                json!({ "step_id": step_id, "run_id": run["run_id"], "executed": "native", "asset": asset, "run": run }),
            )
        }
    }
}

/// `step_begin` under the run lock: checks, then either finishes (hand-off steps, resume, reuse) or
/// claims a native step and returns it to be run outside the lock.
fn begin_claim(ctx: &Context, input: &Value) -> GenResult<Begin> {
    let mut run = load_run(ctx, input)?;
    if run["status"] == "cancelled" {
        return Err(invalid("run is cancelled"));
    }
    let step_id = req_id(input, "step_id")?;
    refresh(&mut run);
    let step = run_step(&run, &step_id)
        .cloned()
        .ok_or_else(|| invalid(format!("no step: {step_id}")))?;
    if fanout::is_fanout(&step)
        && matches!(
            step["status"].as_str(),
            Some("awaiting_submit") | Some("running")
        )
    {
        return step_resume(run, &step_id, &step).map(Begin::Done);
    }
    match step["status"].as_str() {
        Some("ready") | Some("pending") => {}
        Some(other) => return Err(invalid(format!("step {step_id} is {other}, not ready"))),
        None => return Err(invalid(format!("step {step_id} has no status"))),
    }
    // Spend is refused before dependency order is checked: no effect starts either way.
    // Budget gate: refuse to begin a step whose estimate breaks the cap.
    if let (Some(cap), Some(est)) = (
        run["budget_usd"].as_f64(),
        step["budget"]["est_usd"].as_f64(),
    ) {
        let spent = run["spent_usd"].as_f64().unwrap_or(0.0);
        if spent + est > cap {
            return Err(GenError::new(
                "budget_exceeded",
                format!("step {step_id} estimates {est:.4} USD; {spent:.4} of {cap:.4} USD already spent"),
            ));
        }
    }

    if strings(&step, "depends_on").iter().any(|d| {
        !run_step(&run, d).is_some_and(|s| s["status"] == "succeeded" || s["status"] == "skipped")
    }) {
        return Err(invalid(format!(
            "step {step_id}: dependencies have not completed"
        )));
    }
    let mut out = json!({ "step_id": step_id, "run_id": run["run_id"] });
    if step["native"].is_string() {
        // Claim: the work runs after this lock is released, so mark the step taken (a second begin is
        // refused as "not ready") and count the attempt now; `before` restores it if the work fails.
        let run0 = run.clone();
        let before = {
            let s = run_step_mut(&mut run, &step_id).unwrap();
            let before = json!({ "status": s["status"], "attempt": s["attempt"], "started_at": s["started_at"], "error": s["error"] });
            s["status"] = json!("awaiting_submit");
            s["attempt"] = json!(s["attempt"].as_i64().unwrap_or(0) + 1);
            s["started_at"] = json!(rightkit_media_gen::store::now());
            s["error"] = Value::Null;
            before
        };
        refresh(&mut run);
        save_run(ctx, &run)?;
        return Ok(Begin::Native {
            run: run0,
            step,
            before,
        });
    }
    let (request, per_segment) = request_and_segments(&run, &step, &step_id)?;
    let run_id = tstr(&run, "run_id");
    let mut begun: Option<fanout::Begun> = None;
    {
        let now = rightkit_media_gen::store::now();
        let s = run_step_mut(&mut run, &step_id).unwrap();
        s["status"] = json!("awaiting_submit");
        s["attempt"] = json!(s["attempt"].as_i64().unwrap_or(0) + 1);
        s["started_at"] = json!(now);
        s["error"] = Value::Null;
        if let Some(segs) = per_segment {
            let attempt = s["attempt"].as_i64().unwrap_or(1);
            let b = fanout::begin(s, &run_id, attempt, segs)?;
            if let Some(clips) = &b.all_reused {
                // Every segment already has its clip from an earlier attempt: nothing to submit.
                s["assets"] = json!(clips);
                s["status"] = json!("succeeded");
                s["finished_at"] = json!(now);
            }
            begun = Some(b);
        }
    }
    refresh(&mut run);
    save_run(ctx, &run)?;
    out["executed"] = json!("handoff");
    if let Some(b) = begun {
        if b.all_reused.is_some() {
            out["executed"] = json!("reused");
        }
        if !b.reused.is_empty() {
            out["reused_segments"] = json!(b.reused);
        }
        out["requests"] = json!(b.requests);
        out["settle_note"] = json!("submit every requests[].request as given (its request_id is deterministic: a resubmission finds the same job), attach each job with step_attach {job_id, segment_id}, then poll step_status: the engine settles the step from its jobs");
    }
    out["request"] = request;
    out["run"] = run;
    Ok(Begin::Done(out))
}

/// Attach jobs to a multi-job step: `{job_id, segment_id}` or `jobs: [{job_id, segment_id}]`, any number
/// of times, idempotently. The whole call is refused if any pair is (see `fanout::attach`). Returns the
/// run (changed in place; the caller holds the run lock and saves) with an `attach` summary of what was attached, what already was, and which segments still have no job.
fn attach_fanout(ctx: &Context, run: &mut Value, step_id: &str, input: &Value) -> GenResult<Value> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let pair = |v: &Value| -> GenResult<(String, String)> {
        let job_id = req_id(v, "job_id")?;
        let segment_id = Some(field(v, "segment_id"))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid(format!("missing field: segment_id (step {step_id} takes one job per segment: {{job_id, segment_id}} or jobs: [...])")))?;
        Ok((job_id, segment_id))
    };
    match input.get("jobs").and_then(Value::as_array) {
        Some(list) => {
            for v in list {
                entries.push(pair(v)?);
            }
        }
        None => entries.push(pair(input)?),
    }
    let summary = {
        let s = run_step_mut(run, step_id).ok_or_else(|| invalid(format!("no step: {step_id}")))?;
        fanout::attach(ctx, s, &entries)?
    };
    refresh(run);
    let mut out = run.clone();
    out["attach"] = summary;
    Ok(out)
}

fn step_attach(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, _| {
        let step_id = req_id(input, "step_id")?;
        if run_step(run, &step_id).is_some_and(fanout::is_fanout) {
            return attach_fanout(ctx, run, &step_id, input);
        }
        let job_id = req_id(input, "job_id")?;
        let s =
            run_step_mut(run, &step_id).ok_or_else(|| invalid(format!("no step: {step_id}")))?;
        if s["status"] != "awaiting_submit" {
            return Err(invalid(format!(
                "step {step_id} is {}, expected awaiting_submit",
                tstr(s, "status")
            )));
        }
        s["job_id"] = json!(job_id);
        s["status"] = json!("running");
        refresh(run);
        Ok(run.clone())
    })
}

/// Settle a step from its job outcome. `status` is the engine job status
/// (complete|failed|cancelled); assets come from `job.result`.
fn step_settle(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, cancels| {
        let step_id = req_id(input, "step_id")?;
        let status = field(input, "status");
        let now = rightkit_media_gen::store::now();
        {
            let s = run_step_mut(run, &step_id)
                .ok_or_else(|| invalid(format!("no step: {step_id}")))?;
            if !STEP_OPEN.contains(&s["status"].as_str().unwrap_or("")) {
                return Err(invalid(format!(
                    "step {step_id} is {}, cannot settle",
                    tstr(s, "status")
                )));
            }
            if fanout::is_fanout(s) {
                // A multi-job step is settled by the engine from its jobs (step_status). A caller may only
                // end it early as failed/cancelled, which first cancels its live jobs and keeps finished clips.
                if !matches!(status.as_str(), "failed" | "cancelled" | "canceled") {
                    return Err(GenError::new(
                        "fanout_engine_settles",
                        format!("step {step_id} has one job per segment and settles itself from them; call step_status (a caller can only settle it failed or cancelled)"),
                    ));
                }
                let c = fanout::cancel(ctx, s, cancels)?;
                if !c.held.is_empty() || !c.still_live.is_empty() {
                    return Err(GenError::new(
                        "awaits_resolution",
                        format!("step {step_id}: job(s) {} cannot be cancelled now (held as outcome_unknown, or still live); resolve them before ending the step", c.held.iter().chain(c.still_live.iter()).cloned().collect::<Vec<_>>().join(", ")),
                    ));
                }
            }
            match status.as_str() {
                "complete" | "succeeded" => {
                    let assets = input
                        .get("assets")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if assets.is_empty()
                        && !s["produces"]
                            .as_str()
                            .map_or(true, |p| p == "data" || p == "text")
                    {
                        return Err(invalid(format!(
                            "step {step_id} produced no assets to settle as succeeded"
                        )));
                    }
                    // Kind check: produced assets must match the step's declared kind.
                    let want = match s["kind"].as_str() {
                        Some("image") => "image",
                        Some("video") | Some("motion") | Some("avatar") => "video",
                        Some("voice") => "audio",
                        _ => "",
                    };
                    let mut clean_assets = Vec::new();
                    for a in assets {
                        let kind = clean(a.get("kind").or_else(|| a.get("media")).into());
                        if !want.is_empty() && !kind.is_empty() && kind != want {
                            return Err(invalid(format!(
                                "step {step_id} expects {want} output, got {kind}"
                            )));
                        }
                        let mut settled = json!({
                            "asset_id": a["asset_id"], "path": a["path"], "sha256": a["sha256"],
                            "bytes": a["bytes"], "mime": a["mime"], "kind": if kind.is_empty() { want.into() } else { kind },
                            "duration_s": a.get("duration_s").cloned().or_else(|| a.get("durationS").cloned()).unwrap_or(Value::Null),
                        });
                        // A per-segment dub clip names the translated segment it voices.
                        if let Some(id) = a.get("segment_id").filter(|v| !v.is_null()) {
                            settled["segment_id"] = id.clone();
                        }
                        clean_assets.push(settled);
                    }
                    s["assets"] = json!(clean_assets);
                    s["status"] = json!("succeeded");
                    s["error"] = Value::Null;
                }
                "failed" => {
                    s["status"] = json!("failed");
                    let msg = clean(input.get("error"));
                    s["error"] = json!(if msg.is_empty() {
                        Value::Null
                    } else {
                        json!(msg)
                    });
                }
                "cancelled" | "canceled" => {
                    s["status"] = json!("cancelled");
                    s["error"] = json!("cancelled");
                }
                other => return Err(invalid(format!("unknown settle status: {other}"))),
            }
            s["finished_at"] = json!(now);
            if let Some(actual) = input.get("actual_usd").and_then(Value::as_f64) {
                s["actual_usd"] = json!(actual);
            }
        }
        accrue_spend(run);
        refresh(run);
        Ok(run.clone())
    })
}

/// Accumulate spend from settled steps (actual beats estimate).
fn accrue_spend(run: &mut Value) {
    let spent: f64 = run["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| {
            matches!(
                s["status"].as_str(),
                Some("succeeded" | "failed" | "running")
            )
        })
        .map(|s| {
            s["actual_usd"]
                .as_f64()
                .or_else(|| s["budget"]["est_usd"].as_f64())
                .unwrap_or(0.0)
        })
        .sum();
    run["spent_usd"] = num(spent);
}

fn step_retry(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, cancels| {
        let step_id = req_id(input, "step_id")?;
        {
            let s = run_step_mut(run, &step_id)
                .ok_or_else(|| invalid(format!("no step: {step_id}")))?;
            if !matches!(s["status"].as_str(), Some("failed" | "cancelled")) {
                return Err(invalid(format!(
                    "step {step_id} is {}, only failed/cancelled steps retry",
                    tstr(s, "status")
                )));
            }
            let max = s["retry"]["max_attempts"].as_i64().unwrap_or(2);
            let attempt = s["attempt"].as_i64().unwrap_or(0);
            if attempt >= max {
                return Err(GenError::new(
                    "retry_exhausted",
                    format!("step {step_id} reached the retry ceiling ({attempt}/{max})"),
                ));
            }
            if fanout::is_fanout(s) {
                // Banks finished clips, stops anything live, refuses while a job is held; the next begin
                // re-issues only the segments without a clip, under a new attempt number.
                fanout::prepare_retry(ctx, s, cancels)?;
            }
            s["status"] = json!("pending");
            s["job_id"] = Value::Null;
            s["error"] = Value::Null;
            s["finished_at"] = Value::Null;
        }
        // A step held for review (dub_plan) is retried with the caller's decision as run inputs
        // (max_tempo, allow_shift, min_gap_ms).
        if let Some(extra) = input.get("inputs").and_then(Value::as_object) {
            if let Some(inputs) = run["inputs"].as_object_mut() {
                inputs.extend(extra.clone());
            }
        }
        refresh(run);
        Ok(run.clone())
    })
}

fn step_skip(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, _| {
        let step_id = req_id(input, "step_id")?;
        {
            let s = run_step_mut(run, &step_id)
                .ok_or_else(|| invalid(format!("no step: {step_id}")))?;
            if !matches!(
                s["status"].as_str(),
                Some("pending" | "ready" | "failed" | "unsupported" | "route_unready")
            ) {
                return Err(invalid(format!(
                    "step {step_id} is {}, cannot skip",
                    tstr(s, "status")
                )));
            }
            s["status"] = json!("skipped");
        }
        refresh(run);
        Ok(run.clone())
    })
}

fn run_cancel(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, cancels| {
        let mut job_ids: Vec<String> = run["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["status"] == "running" && s["job_id"].is_string())
            .filter_map(|s| s["job_id"].as_str().map(String::from))
            .collect();
        // Multi-job steps: the engine cancels every live job itself (completed clips are kept) and names
        // what it did, so a caller that lost its window still reaches all of them.
        let mut multi: Vec<Value> = Vec::new();
        let fan_ids: Vec<String> = run["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| {
                fanout::is_fanout(s)
                    && matches!(s["status"].as_str(), Some("running" | "awaiting_submit"))
            })
            .map(|s| tstr(s, "id"))
            .collect();
        for id in fan_ids {
            let st = run_step_mut(run, &id).unwrap();
            let c = fanout::cancel(ctx, st, cancels)?;
            job_ids.extend(c.cancelled.iter().cloned());
            multi.push(json!({
                "step_id": id, "cancelled_job_ids": c.cancelled, "held_job_ids": c.held,
                "still_live_job_ids": c.still_live, "kept_segments": c.kept_segments,
            }));
        }
        run["status"] = json!("cancelled");
        refresh(run);
        let mut out = json!({ "run": run, "cancel_job_ids": job_ids });
        if !multi.is_empty() {
            out["multi_job"] = json!(multi);
        }
        Ok(out)
    })
}

/// `step_status` `{run_id, step_id?}`: read the job store, derive each multi-job step's state from its
/// jobs (see `fanout::sync`) and persist it. Without `step_id`, every multi-job step that is awaiting
/// submit or running. Returns per-segment request ids, job ids and states, so a reloaded window or a
/// CLI caller can find missing segments, see what runs and let the engine settle the step.
fn step_status(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, cancels| {
        let wanted = field(input, "step_id");
        let ids: Vec<String> = if wanted.is_empty() {
            run["steps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| {
                    fanout::is_fanout(s)
                        && matches!(s["status"].as_str(), Some("running" | "awaiting_submit"))
                })
                .map(|s| tstr(s, "id"))
                .collect()
        } else {
            vec![wanted]
        };
        let mut reports = Vec::new();
        for id in &ids {
            let s = run_step_mut(run, id).ok_or_else(|| invalid(format!("no step: {id}")))?;
            if fanout::is_fanout(s) {
                fanout::sync(ctx, s, cancels)?;
                reports.push(fanout::report(s));
            } else {
                reports.push(json!({ "step_id": id, "fanout": false, "status": s["status"], "attempt": s["attempt"], "job_id": s["job_id"], "error": s["error"] }));
            }
        }
        accrue_spend(run);
        refresh(run);
        // The report is read again after refresh so a settled step shows its final status.
        for r in reports.iter_mut() {
            if let Some(s) = run_step(&run, r["step_id"].as_str().unwrap_or("")) {
                r["status"] = s["status"].clone();
            }
        }
        Ok(
            json!({ "run_id": run["run_id"], "status": run["status"], "steps": reports, "run": run }),
        )
    })
}

/// `step_cancel` `{run_id, step_id}`: cancel every live job of a multi-job step and end the step as
/// `cancelled`; finished clips stay recorded for a retry. A held (`outcome_unknown`) job cannot be
/// cancelled: the step stays running with `awaits_resolution` until the job is resolved.
fn step_cancel(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, cancels| {
        let step_id = req_id(input, "step_id")?;
        let c = {
            let s = run_step_mut(run, &step_id)
                .ok_or_else(|| invalid(format!("no step: {step_id}")))?;
            if !fanout::is_fanout(s) {
                return Err(invalid(format!("step {step_id} is not a multi-job step; cancel its job and settle the step cancelled")));
            }
            if !matches!(s["status"].as_str(), Some("running" | "awaiting_submit")) {
                return Err(invalid(format!(
                    "step {step_id} is {}, nothing to cancel",
                    tstr(s, "status")
                )));
            }
            let c = fanout::cancel(ctx, s, cancels)?;
            if c.held.is_empty() && c.still_live.is_empty() {
                s["status"] = json!("cancelled");
                s["error"] = json!("cancelled");
                s["finished_at"] = json!(rightkit_media_gen::store::now());
            }
            c
        };
        accrue_spend(run);
        refresh(run);
        Ok(json!({
            "step_id": step_id, "status": run_step(&run, &step_id).map(|s| s["status"].clone()).unwrap_or(Value::Null),
            "cancelled_job_ids": c.cancelled, "held_job_ids": c.held, "still_live_job_ids": c.still_live,
            "kept_segments": c.kept_segments, "run": run,
        }))
    })
}

/// Recovery: reconcile steps still `running`/`awaiting_submit` with the job
/// store. Terminal job states settle the step (assets pulled from
/// `job.result`); orphaned `awaiting_submit` steps return to `ready`.
fn run_recover(ctx: &Context, input: &Value) -> GenResult<Value> {
    mutate_run(ctx, input, |run, cancels| {
        let mut recovered: Vec<String> = Vec::new();
        let mut orphaned: Vec<String> = Vec::new();
        let steps = run["steps"].as_array().cloned().unwrap_or_default();
        for s in steps {
            let id = tstr(&s, "id");
            if fanout::is_fanout(&s)
                && matches!(s["status"].as_str(), Some("running" | "awaiting_submit"))
            {
                // Multi-job step: adopt jobs submitted under its request ids, then derive the state from them.
                let st = run_step_mut(run, &id).unwrap();
                let before = tstr(st, "status");
                fanout::sync(ctx, st, cancels)?;
                let after = tstr(st, "status");
                if after == "awaiting_submit" {
                    // Began but no job exists under any of its request ids: safe to begin again. The attempt
                    // number is given back so the new begin issues the same request ids.
                    st["status"] = json!("ready");
                    st["error"] = json!("recovered: never submitted");
                    st["attempt"] = json!((st["attempt"].as_i64().unwrap_or(1) - 1).max(0));
                    orphaned.push(id);
                } else if after != before {
                    recovered.push(id);
                }
                continue;
            }
            match s["status"].as_str() {
                Some("running") => {
                    let Some(job_id) = s["job_id"].as_str() else {
                        orphaned.push(id.clone());
                        continue;
                    };
                    let job =
                        rightkit_media_gen::api::job_status(ctx, &json!({ "job_id": job_id })).ok();
                    let status = job
                        .as_ref()
                        .and_then(|j| j["status"].as_str().map(String::from));
                    // Carry the job's own error code and message into the step, not a generic note.
                    let job_error = job.as_ref().and_then(|j| {
                        let (code, msg) = (
                            j["error"]["code"].as_str()?,
                            j["error"]["message"].as_str().unwrap_or(""),
                        );
                        Some(format!("{code}: {msg}"))
                    });
                    match status.as_deref() {
                        Some("complete") => {
                            let assets = rightkit_media_gen::api::job_result(
                                ctx,
                                &json!({ "job_id": job_id }),
                            )
                            .ok()
                            .and_then(|r| r["assets"].as_array().cloned())
                            .unwrap_or_default();
                            let st = run_step_mut(run, &id).unwrap();
                            st["status"] = json!("succeeded");
                            st["assets"] = json!(assets.iter().map(|a| json!({
                                "asset_id": a["asset_id"], "path": a["path"], "sha256": a["sha256"],
                                "bytes": a["bytes"], "mime": a["mime"],
                                "kind": a["media"].as_str().map(String::from)
                                    .or_else(|| a["media"]["kind"].as_str().map(String::from))
                                    .unwrap_or_else(|| tstr(&s, "produces")),
                                "duration_s": a["media"]["duration_s"].clone(),
                            })).collect::<Vec<_>>());
                            st["finished_at"] = json!(rightkit_media_gen::store::now());
                            recovered.push(id);
                        }
                        Some("failed") | Some("cancelled") => {
                            let st = run_step_mut(run, &id).unwrap();
                            st["status"] = json!(status.clone().unwrap_or_else(|| "failed".into()));
                            st["error"] = json!(job_error
                                .clone()
                                .unwrap_or_else(|| "recovered terminal job state".into()));
                            st["finished_at"] = json!(rightkit_media_gen::store::now());
                            recovered.push(id);
                        }
                        Some(_) => {}
                        None => {
                            // Job vanished from the store: the step is recoverable.
                            let st = run_step_mut(run, &id).unwrap();
                            st["status"] = json!("failed");
                            st["error"] = json!("job missing from store during recovery");
                            recovered.push(id);
                        }
                    }
                }
                Some("awaiting_submit") => {
                    // Began but a job was never attached (e.g. app restart): safe to retry.
                    let st = run_step_mut(run, &id).unwrap();
                    st["status"] = json!("ready");
                    st["error"] = json!("recovered: never submitted");
                    orphaned.push(id);
                }
                _ => {}
            }
        }
        refresh(run);
        Ok(json!({ "run": run, "recovered": recovered, "orphaned": orphaned }))
    })
}
