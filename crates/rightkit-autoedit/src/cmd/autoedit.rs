//! Automatic editing methods: `autoedit.analyze` and `autoedit.apply`.
//!
//! One run may cover one source (`source`), several (`sources`) or every
//! project source (neither given). Sources with a transcript go through the
//! editorial engine (dead air, disfluency, retakes, beats); sources without one
//! are analysed visually — typed FFprobe duration/stream probe plus an FFmpeg
//! scene-score pass and a decoded RMS envelope — via
//! [`crate::editorial::visual`]. Repeated lines across different files become
//! cross-file retake proposals on the losing source's plan. Applying resolves
//! every plan and lands one merged, undoable action group on the timeline.

use std::collections::BTreeMap;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::cuts::ActionGroup;
use crate::editorial;
use rightkit_ffmpeg::FfmpegCommand;
use rightkit_timeline::{Action, Fps, SourceId, Ticks};
use rightkit_transcript::Transcript;
use rightkit_video_project::worker::{spawn_ffmpeg, StdioMode, StdioPolicy};
use rightkit_video_project::{Access, MediaTools, Project, ProjectError};
use serde::Deserialize;
use serde_json::{json, Value};

use super::rules as rules_cmd;
use rightkit_engine_host::env::Env;
use rightkit_engine_host::project::media_tools;
use rightkit_engine_host::project::{
    analysis_dir, bad, find_source, load_envelope, load_transcript, timemap_path,
    transcript_exists, write_json,
};

/// How one source was analysed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Speech,
    Visual,
}

struct Planned {
    record_id: SourceId,
    mode: Mode,
    plan: editorial::EditorialPlan,
    transcript: Option<Transcript>,
}

/// ffmpeg `metadata=print` output → per-frame scene scores (milli).
fn parse_scene_scores(
    text: &str,
    scene_cut_milli: i32,
) -> (Vec<editorial::visual::MotionSample>, Vec<i64>) {
    let mut samples = Vec::new();
    let mut cuts = Vec::new();
    let mut cur_ms: Option<i64> = None;
    for line in text.lines() {
        if let Some(t) = line.split("pts_time:").nth(1) {
            cur_ms = t
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .map(|s| (s * 1000.0).round() as i64);
        } else if let Some(v) = line.strip_prefix("lavfi.signalstats.YDIF=") {
            if let (Some(ms), Ok(score)) = (cur_ms, v.trim().parse::<f64>()) {
                let milli = (score * 100.0).round() as i32;
                samples.push(editorial::visual::MotionSample { ms, milli });
                if milli >= scene_cut_milli {
                    cuts.push(ms);
                }
            }
        }
    }
    (samples, cuts)
}

/// Typed FFmpeg pass: samples `lavfi.signalstats.YDIF` (frame-to-frame luma
/// delta, in 0..255 units scaled ×100 to milli) for the whole source,
/// subsampled to ~8 fps so cost stays proportional. Every frame is sampled —
/// stills report ~0, so dead regions are measurable, not merely the absence
/// of scene cuts.
fn motion_samples(
    tools: &MediaTools,
    media: &Path,
    duration_ms: i64,
    scene_cut_milli: i32,
) -> Result<(Vec<editorial::visual::MotionSample>, Vec<i64>), ProjectError> {
    // Long sources sample coarser to bound the pass cost (~4800 frames max).
    let fps = if duration_ms > 600_000 { 4 } else { 8 };
    let vf = format!("fps={fps},signalstats,metadata=print:key=lavfi.signalstats.YDIF:file=-");
    let mut cmd = FfmpegCommand::default();
    for a in ["-hide_banner", "-nostats", "-i"] {
        cmd.text(a);
    }
    cmd.path(media);
    for a in ["-vf", vf.as_str(), "-an", "-f", "null", "-"] {
        cmd.text(a);
    }
    let mut child = spawn_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        StdioPolicy {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Null,
        },
    )
    .map_err(|e| {
        bad(format!(
            "cannot run ffmpeg at {}: {e}",
            tools.ffmpeg.display()
        ))
    })?;
    let stdout = child.take_stdout().expect("piped");
    let mut text = String::new();
    BufReader::new(stdout)
        .read_to_string(&mut text)
        .map_err(|e| bad(format!("reading ffmpeg scene output: {e}")))?;
    let status = child
        .wait()
        .map_err(|e| bad(format!("waiting for ffmpeg: {e}")))?;
    if !status.success() {
        return Err(bad(format!("ffmpeg scene pass exited with {status}")));
    }
    Ok(parse_scene_scores(&text, scene_cut_milli))
}

/// Maps an edit preset name to a cut preset.
pub fn cut_preset(name: &str) -> Result<crate::cuts::Preset, ProjectError> {
    use crate::cuts::Preset;
    match name {
        "tight" => Ok(Preset::Tight),
        "natural" => Ok(Preset::Natural),
        "punchy" => Ok(Preset::Punchy),
        other => Err(bad(format!(
            "unknown edit preset {other:?}: expected tight, natural or punchy"
        ))),
    }
}

/// Decodes the first audio stream of `source` to 16 kHz mono f32 with the editing ffmpeg at
/// `ffmpeg` (no `PATH` lookup). Sample 0 is the first decoded sample of the source's audio.
fn decode_16k_mono(ffmpeg: &Path, source: &Path) -> Result<Vec<f32>, String> {
    use rightkit_ffmpeg::decode::{decode_pcm_f32, DecodeOptions};
    use rightkit_ffmpeg::runner::RunError;
    use rightkit_ffmpeg::verify_ffmpeg_handle;

    if !ffmpeg.is_file() {
        return Err(format!(
            "ffmpeg path {} is not a file (no PATH lookup is performed)",
            ffmpeg.display()
        ));
    }
    if !source.is_file() {
        return Err(format!("source {} does not exist", source.display()));
    }
    let timeout = std::time::Duration::from_secs(10);
    let binary = verify_ffmpeg_handle(ffmpeg, rightkit_ffmpeg::EXPORT_ASS_ZIMG_VARIANT, timeout)
        .or_else(|_| verify_ffmpeg_handle(ffmpeg, rightkit_ffmpeg::EXPORT_ASS_VARIANT, timeout))
        .map_err(|e| format!("ffmpeg verification failed: {e}"))?;
    let options = DecodeOptions {
        sample_rate: 16_000,
        channels: 1,
        ..DecodeOptions::default()
    }
    .with_audio_stream_index(Some(0));
    let samples = decode_pcm_f32(&binary, source, &options).map_err(|err| match err {
        RunError::Failed {
            status,
            stderr_tail,
        } => {
            let stderr = stderr_tail.trim().to_string();
            if stderr.contains("matches no streams") || stderr.contains("Stream map") {
                "source has no decodable audio".to_string()
            } else {
                format!("ffmpeg failed to decode audio ({status}): {stderr}")
            }
        }
        other => format!("decoding audio: {other}"),
    })?;
    if samples.is_empty() {
        return Err("source has no decodable audio".to_string());
    }
    Ok(samples)
}

/// Per-10 ms RMS envelope for a source (same layout as the transcribe path).
fn audio_envelope(tools: &MediaTools, media: &Path) -> Result<Vec<f32>, ProjectError> {
    decode_16k_mono(&tools.ffmpeg, media)
        .map_err(|e| bad(format!("decoding {} for analysis: {e}", media.display())))
        .map(|samples| {
            samples
                .chunks(160)
                .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
                .collect()
        })
}

fn visual_plan(
    tools: &MediaTools,
    media: &Path,
    record_id: &SourceId,
    cfg: &editorial::EditorialConfig,
    fps: Fps,
) -> Result<editorial::EditorialPlan, ProjectError> {
    let probe = rightkit_video_project::probe::probe_file(tools, media)?;
    // Floor, never round: a kept range ending at a rounded-up millisecond
    // would run past the source (e.g. 48.221667s must plan as 48221ms).
    let duration_ms =
        (probe.duration.0 as i128 * 1000 / rightkit_timeline::TICKS_PER_SECOND as i128) as i64;
    let (motion, scene_cuts) = if probe.video.is_some() {
        motion_samples(tools, media, duration_ms, cfg.visual.scene_cut_milli)?
    } else {
        (Vec::new(), Vec::new())
    };
    let envelope = if probe.has_audio {
        audio_envelope(tools, media)?
    } else {
        Vec::new()
    };
    let signals = editorial::visual::VisualSignals {
        duration_ms,
        has_video: probe.video.is_some(),
        has_audio: probe.has_audio,
        motion,
        scene_cuts,
        envelope,
    };
    editorial::visual::analyze_visual(record_id, &signals, cfg, fps)
        .map_err(|e| bad(format!("visual analysis failed: {e}")))
}

/// Which sources the run covers, in project order.
fn target_sources(
    p: &Project,
    q_source: Option<&str>,
    q_sources: Option<&[String]>,
) -> Result<Vec<SourceId>, ProjectError> {
    if let Some(list) = q_sources {
        if list.is_empty() {
            return Err(bad("sources must not be empty"));
        }
        return list
            .iter()
            .map(|s| find_source(p, s).map(|r| r.id.clone()))
            .collect();
    }
    if let Some(s) = q_source {
        return Ok(vec![find_source(p, s)?.id.clone()]);
    }
    if p.meta.sources.is_empty() {
        return Err(bad("no sources in this project"));
    }
    Ok(p.meta.sources.iter().map(|r| r.id.clone()).collect())
}

fn plan_source(
    tools: Option<&MediaTools>,
    p: &Project,
    source_id: &SourceId,
    cfg: &editorial::EditorialConfig,
) -> Result<Planned, ProjectError> {
    let record = p
        .meta
        .sources
        .iter()
        .find(|r| &r.id == source_id)
        .ok_or_else(|| bad("source vanished mid-analysis"))?
        .clone();
    if transcript_exists(p, source_id.as_str()) {
        let t = load_transcript(p, source_id.as_str())?;
        let envelope = load_envelope(p, source_id.as_str());
        let plan = editorial::analyze(&t, envelope.as_deref(), cfg, p.meta.fps)
            .map_err(|e| bad(format!("analysis failed: {e}")))?;
        Ok(Planned {
            record_id: record.id,
            mode: Mode::Speech,
            plan,
            transcript: Some(t),
        })
    } else {
        let tools =
            tools.ok_or_else(|| bad("ffmpeg/ffprobe are required for transcript-free sources"))?;
        let media = p.source_path(&record);
        let plan = visual_plan(tools, &media, source_id, cfg, p.meta.fps)?;
        Ok(Planned {
            record_id: record.id,
            mode: Mode::Visual,
            plan,
            transcript: None,
        })
    }
}

/// Injects cross-file retake items into the losing sources' plans.
fn mark_cross_file(planned: &mut [Planned]) -> usize {
    let transcripts: Vec<(String, Transcript)> = planned
        .iter()
        .filter_map(|p| {
            p.transcript
                .clone()
                .map(|t| (p.record_id.as_str().to_string(), t))
        })
        .collect();
    if transcripts.len() < 2 {
        return 0;
    }
    let hits = editorial::cross_file_retakes(&transcripts, 6);
    let n = hits.len();
    for h in hits {
        let Some(p) = planned
            .iter_mut()
            .find(|p| p.record_id.as_str() == h.loser_source)
        else {
            continue;
        };
        let mut signals = BTreeMap::new();
        signals.insert("words".to_string(), h.words as f64);
        signals.insert("take_complete".into(), u8::from(h.take_complete) as f64);
        signals.insert("kept_complete".into(), u8::from(h.kept_complete) as f64);
        signals.insert("take_fluency".into(), h.take_fluency);
        signals.insert("kept_fluency".into(), h.kept_fluency);
        signals.insert("kept_start_ms".into(), h.kept_start_ms as f64);
        signals.insert("kept_end_ms".into(), h.kept_end_ms as f64);
        p.plan.extend(vec![editorial::EditorialItem {
            id: String::new(),
            kind: editorial::ItemKind::CrossFileRetake,
            tier: editorial::Tier::Auto,
            confidence: 0.8,
            start_ms: h.start_ms,
            end_ms: h.end_ms,
            reason: format!(
                "repeated take ranked by completeness, fluency & length ({} losing words; later source breaks ties)",
                h.words
            ),
            evidence: editorial::Evidence {
                text: h.text,
                kept_text: Some(h.kept_text),
                kept_source: Some(h.kept_source),
                word_range: Some(h.word_range),
                signals,
                ..editorial::Evidence::default()
            },
            action: editorial::ItemAction::RemoveWords {
                first: h.word_range[0],
                end: h.word_range[1],
            },
            beat_id: None,
            covered_by: None,
            take: None,
        }]);
    }
    n
}

/// One source's plan serialised for review.
fn plan_json(p: &Planned) -> Result<Value, ProjectError> {
    Ok(json!({
        "source": p.record_id.as_str(),
        "mode": match p.mode { Mode::Speech => "speech", Mode::Visual => "visual" },
        "plan_id": p.plan.id,
        "duration_ms": p.plan.source_duration_ms,
        "items": p.plan.items,
        "take_groups": p.plan.take_groups,
        "beats": p.plan.beats,
    }))
}

/// Applies `review` to one plan; `review` may be a flat `{reject,accept,takes}`
/// object applied to every source, or `{<source id>: {...}}` per source.
fn review_for<'a>(review: Option<&'a Value>, source: &SourceId) -> Option<&'a Value> {
    let rv = review?;
    rv.get(source.as_str()).or(Some(rv)).filter(|v| {
        v.get("reject").is_some() || v.get("accept").is_some() || v.get("takes").is_some()
    })
}

fn build_review(
    plan: &editorial::EditorialPlan,
    rv: Option<&Value>,
) -> Result<editorial::Review, ProjectError> {
    // Unreviewed items follow their tier: `auto` applies, `suggest` waits for
    // an explicit accept. accept_all also applied the alternative-take
    // suggestion, cutting both takes of a retaken line.
    let mut review = editorial::Review::new();
    if let Some(rv) = rv {
        if let Some(ids) = rv.get("reject").and_then(Value::as_array) {
            for id in ids.iter().filter_map(Value::as_str) {
                review.reject(id);
            }
        }
        if let Some(ids) = rv.get("accept").and_then(Value::as_array) {
            for id in ids.iter().filter_map(Value::as_str) {
                review.accept(id);
            }
        }
        if let Some(takes) = rv.get("takes").and_then(Value::as_object) {
            for (group, idx) in takes {
                if let Some(i) = idx.as_u64() {
                    review
                        .pick_take(plan, group, i as usize)
                        .map_err(|e| bad(e.to_string()))?;
                }
            }
        }
    }
    Ok(review)
}

pub fn m_autoedit_analyze(
    dir: &Path,
    params: &Value,
    env: &Env,
    dry_run: bool,
    progress: &mut dyn FnMut(&Value),
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        source: Option<String>,
        sources: Option<Vec<String>>,
        preset: Option<String>,
        context: Option<Value>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let preset_name = q.preset.clone().unwrap_or_else(|| "natural".into());
    let preset = cut_preset(&preset_name)?;
    let tuned = rules_cmd::tuned_config(preset, dir, env, q.context.as_ref())?;
    let p = Project::open(dir, Access::Read)?;
    let ids = target_sources(&p, q.source.as_deref(), q.sources.as_deref())?;
    // Only visual sources need ffmpeg; resolve lazily so speech-only projects
    // never require the media tools.
    let needs_tools = ids.iter().any(|id| !transcript_exists(&p, id.as_str()));
    let tools = if needs_tools {
        Some(media_tools(env)?)
    } else {
        None
    };
    let mut planned = Vec::new();
    for id in &ids {
        progress(&json!({"event": "autoedit.analyze.source", "source": id.as_str()}));
        planned.push(plan_source(tools.as_ref(), &p, id, &tuned.cfg)?);
    }
    let cross = mark_cross_file(&mut planned);
    let mut plans = Vec::new();
    for pl in &planned {
        let plan_value = plan_json(pl)?;
        if !dry_run {
            write_json(
                &analysis_dir(dir).join(format!("editplan-{}.json", pl.plan.source_blake3)),
                &serde_json::from_slice::<Value>(&pl.plan.to_json())
                    .map_err(|e| bad(e.to_string()))?,
            )?;
        }
        plans.push(plan_value);
    }
    let mut out = json!({
        "status": "planned",
        "preset": preset_name,
        "plans": plans,
        "cross_file_retakes": cross,
        "dry_run": dry_run,
        "rules": tuned.report,
    });
    // Back-compat: a single-source run keeps the original flat shape.
    if planned.len() == 1 {
        let pl = &planned[0];
        out["plan_id"] = json!(pl.plan.id);
        out["source"] = json!(pl.plan.source_blake3);
        out["mode"] = json!(match pl.mode {
            Mode::Speech => "speech",
            Mode::Visual => "visual",
        });
        out["items"] = json!(pl.plan.items);
        out["take_groups"] = json!(pl.plan.take_groups);
        out["beats"] = json!(pl.plan.beats);
    }
    Ok(out)
}

/// Merges per-source action groups into one group: video inserts are offset by
/// the clips already placed, linked audio starts shift by the output frames
/// already produced. One group = one undo step, all-or-nothing.
fn merge_groups(groups: Vec<(ActionGroup, i64)>, fps: Fps) -> ActionGroup {
    let mut hasher = rightkit_fs::digest::Blake3Hasher::new();
    let mut actions = Vec::new();
    let (mut vids, mut aids) = (Vec::new(), Vec::new());
    let mut video_offset = 0usize;
    let mut audio_offset = Ticks::ZERO;
    for (g, out_frames) in groups {
        hasher.update(g.id.as_bytes());
        vids.extend(g.video_clip_ids.iter().cloned());
        aids.extend(g.audio_clip_ids.iter().cloned());
        let mut inserted = 0usize;
        for a in g.actions {
            let a = match a {
                Action::InsertClip {
                    track_id,
                    index,
                    clip,
                } => {
                    inserted += 1;
                    Action::InsertClip {
                        track_id,
                        index: index + video_offset,
                        clip,
                    }
                }
                Action::AddAudioClip { track_id, mut clip } => {
                    clip.start = Ticks(clip.start.0 + audio_offset.0);
                    Action::AddAudioClip { track_id, clip }
                }
                other => other,
            };
            actions.push(a);
        }
        video_offset += inserted;
        audio_offset = fps.frame_to_ticks(fps.ticks_to_frame(audio_offset) + out_frames);
    }
    ActionGroup {
        id: format!("autocut-{}", &hasher.finalize()[..8]),
        label: "Auto-edit (multi-source)".to_string(),
        video_clip_ids: vids,
        audio_clip_ids: aids,
        actions,
    }
}

pub fn m_autoedit_apply(
    dir: &Path,
    params: &Value,
    env: &Env,
    dry_run: bool,
    progress: &mut dyn FnMut(&Value),
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        source: Option<String>,
        sources: Option<Vec<String>>,
        preset: Option<String>,
        review: Option<Value>,
        context: Option<Value>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let preset = cut_preset(q.preset.as_deref().unwrap_or("natural"))?;
    let tuned = rules_cmd::tuned_config(preset, dir, env, q.context.as_ref())?;
    let p = Project::open(dir, Access::Read)?;
    let ids = target_sources(&p, q.source.as_deref(), q.sources.as_deref())?;
    let needs_tools = ids.iter().any(|id| !transcript_exists(&p, id.as_str()));
    let tools = if needs_tools {
        Some(media_tools(env)?)
    } else {
        None
    };
    let mut planned = Vec::new();
    for id in &ids {
        progress(&json!({"event": "autoedit.analyze.source", "source": id.as_str()}));
        planned.push(plan_source(tools.as_ref(), &p, id, &tuned.cfg)?);
    }
    mark_cross_file(&mut planned);

    let mut resolved = Vec::new();
    for pl in &planned {
        let rv = review_for(q.review.as_ref(), &pl.record_id);
        let review = build_review(&pl.plan, rv)?;
        // Video-only sources (silent screen captures) get no linked audio clip.
        let opts = crate::cuts::RealiseOptions {
            with_audio: p
                .timeline()
                .source(&pl.record_id)
                .is_none_or(|s| s.has_audio),
            ..Default::default()
        };
        let r = match (&pl.mode, &pl.transcript) {
            (Mode::Speech, Some(t)) => pl
                .plan
                .resolve(
                    t,
                    load_envelope(&p, pl.record_id.as_str()).as_deref(),
                    &review,
                    &opts,
                )
                .map_err(|e| bad(e.to_string()))?,
            _ => pl
                .plan
                .resolve_visual(&review, &opts)
                .map_err(|e| bad(e.to_string()))?,
        };
        resolved.push((pl, r));
    }

    if resolved.iter().all(|(_, r)| r.cut_plan.kept.is_empty()) {
        return Err(bad(crate::cuts::CutError::NothingKept.to_string()));
    }

    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "rules": tuned.report.clone(),
            "would": {"command": "autoedit.apply", "sources": ids.iter().map(|i| i.as_str()).collect::<Vec<_>>()},
            "plans": resolved.iter().map(|(pl, r)| json!({
                "source": pl.record_id.as_str(),
                "mode": match pl.mode { Mode::Speech => "speech", Mode::Visual => "visual" },
                "kept_ranges": r.cut_plan.kept.len(),
                "removed": r.cut_plan.removed.len(),
                "output_frames": r.time_map.output_frames(),
            })).collect::<Vec<_>>(),
        }));
    }
    // Rejected items become correction rules before the cut lands, so the next auto-edit on
    // this channel resolves them. A dry run returned above and writes no rule.
    let mut rejections = Vec::new();
    for pl in &planned {
        let Some(rv) = review_for(q.review.as_ref(), &pl.record_id) else {
            continue;
        };
        let Some(ids) = rv.get("reject").and_then(Value::as_array) else {
            continue;
        };
        for item in &pl.plan.items {
            if !ids.iter().any(|id| id.as_str() == Some(item.id.as_str())) {
                continue;
            }
            match item.kind {
                editorial::ItemKind::Silence | editorial::ItemKind::LongPause => {
                    rejections.push(rules_cmd::Rejection::Pause {
                        edit_id: item.id.clone(),
                        // Keeping the pause means a ceiling above the whole pause, not just the removed part.
                        duration_ms: item
                            .evidence
                            .signals
                            .get("pause_ms")
                            .map(|v| *v as i64)
                            .unwrap_or(item.end_ms - item.start_ms),
                    })
                }
                editorial::ItemKind::Filler => rejections.push(rules_cmd::Rejection::Filler {
                    edit_id: item.id.clone(),
                }),
                _ => {}
            }
        }
    }
    rules_cmd::record_corrections(dir, env, q.context.as_ref(), &rejections)?;
    drop(p);
    let mut pw = Project::open(dir, Access::Write)?;
    let group = merge_groups(
        resolved
            .iter()
            .map(|(_, r)| (r.group.clone(), r.time_map.output_frames()))
            .collect(),
        pw.meta.fps,
    );
    let save_evidence = || -> Result<(), ProjectError> {
        for (pl, r) in &resolved {
            write_json(
                &analysis_dir(dir).join(format!("editplan-{}.json", pl.plan.source_blake3)),
                &serde_json::from_slice::<Value>(&pl.plan.to_json())
                    .map_err(|e| bad(e.to_string()))?,
            )?;
            write_json(&timemap_path(dir, &pl.plan.source_blake3), &r.time_map)?;
        }
        Ok(())
    };
    // Retry of the same plan: every clip of this group is already placed, so
    // the edit landed before (group ids hash the plan and placement).
    let placed: std::collections::BTreeSet<&str> = pw
        .timeline()
        .video_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id.as_str()))
        .collect();
    if !group.video_clip_ids.is_empty()
        && group
            .video_clip_ids
            .iter()
            .all(|id| placed.contains(id.as_str()))
    {
        save_evidence()?;
        return Ok(json!({
            "status": "already_applied",
            "group": group.id,
            "sources": resolved.len(),
            "revision": pw.revision_summary(),
        }));
    }
    crate::cuts::apply_group(&mut pw.store, &group).map_err(|e| bad(e.to_string()))?;
    pw.save_timeline()?;
    save_evidence()?;
    let mut out = json!({
        "status": "applied",
        "rules": tuned.report,
        "group": group.id,
        "sources": resolved.len(),
        "revision": pw.revision_summary(),
        "applied": resolved.iter().map(|(pl, r)| json!({
            "source": pl.record_id.as_str(),
            "mode": match pl.mode { Mode::Speech => "speech", Mode::Visual => "visual" },
            "kept_ranges": r.cut_plan.kept.len(),
            "removed": r.cut_plan.removed.len(),
            "output_frames": r.time_map.output_frames(),
            "outcomes": r.outcomes,
        })).collect::<Vec<_>>(),
    });
    if resolved.len() == 1 {
        let (pl, r) = &resolved[0];
        out["source"] = json!(pl.record_id.as_str());
        out["mode"] = json!(match pl.mode {
            Mode::Speech => "speech",
            Mode::Visual => "visual",
        });
        out["kept_ranges"] = json!(r.cut_plan.kept.len());
        out["removed"] = json!(r.cut_plan.removed.len());
        out["output_frames"] = json!(r.time_map.output_frames());
        out["outcomes"] = json!(r.outcomes);
    }
    Ok(out)
}
