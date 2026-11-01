//! Dubbing: explicit retiming of synthesised segments, the aligned SRT and the dub track.
//!
//! The dubbing template (`opencreator_dubbing`) is: transcribe -> translate -> dub (one voice job
//! per translated segment) -> `dub_plan` (this module) -> `srt_export` -> compose. This module owns
//! the parts that decide timing, so nothing about a dub is implicit:
//!
//! - `plan`: pure segment-slot arithmetic. For each segment it compares the synthesised duration
//!   with the slot the source segment occupies and decides, per segment and reported, one of
//!   `fits`, `short`, `tempo`, `extend`, `overflow` (or `empty` for a blank segment).
//! - the dub track: each clip is placed at its final start (ffmpeg `adelay`), time-compressed with
//!   the pitch-preserving `atempo` filter when the plan says `tempo`/`extend`, and mixed into one
//!   mono WAV. Filters are built with RightKit's `Filter`; the clip placement graph itself is
//!   host glue because rightkit-audio has no timeline placement or tempo helper (a gap).
//! - the aligned SRT: written from the FINAL placed timings with the pipeline's existing SRT writer
//!   (`rightkit_transcript::to_srt` via `srt_text`). Alignment is `segment` level: each cue is one
//!   translated segment between its placed start and placed end. Word-level alignment needs a
//!   RightKit alignment capability (rightkit-align) that is not vendored; none is faked here.
//!
//! Retiming policy (slot = from the segment's start, bounded by the next audible segment's start and
//! the segment's own end; with `allow_shift` the start is the later of the source start and the end
//! of the previous placed clip plus `min_gap_ms`):
//!
//! | synthesised duration vs slot            | action     | audio                                   |
//! |-----------------------------------------|------------|-----------------------------------------|
//! | within 60 ms under the slot, or equal   | `fits`     | placed at start                         |
//! | shorter than the slot by over 60 ms     | `short`    | placed at start, silence after          |
//! | longer, ratio <= `max_tempo` (def 1.15) | `tempo`    | sped up by exactly the ratio (atempo)   |
//! | longer, ratio > `max_tempo`, no shift   | `overflow` | natural speed, NOT truncated, flagged   |
//! | longer, ratio > `max_tempo`, shift      | `extend`   | sped up by `max_tempo`, runs past slot  |
//!
//! Any `overflow` (or a dub track that ends after the media) makes the plan `needs_review`: no track
//! is rendered, no SRT is produced, and the options are returned for the caller to choose
//! (`max_tempo`, `allow_shift`, a shorter translation). Audio is never dropped, truncated or
//! overlapped silently. The tempo never slows speech down, and never exceeds ffmpeg's single-stage
//! `atempo` ceiling of 2.0.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rightkit_audio::filter::render_chain;
use rightkit_audio::Filter;
use rightkit_ffmpeg::FfmpegBinary;
use serde_json::{json, Value};

use super::media;
use crate::{Context, GenError, GenResult};

/// Default speed-up limit (a parameter: `max_tempo`).
pub const DEFAULT_MAX_TEMPO: f64 = 1.15;
/// ffmpeg's `atempo` accepts 0.5 to 2.0 per stage; a single stage keeps quality, so 2.0 is the cap.
pub const MAX_TEMPO_LIMIT: f64 = 2.0;
/// A clip no more than this far under its slot (and not over it) counts as `fits`, not `short`.
const FIT_TOLERANCE_MS: i64 = 60;
/// A synthesised-duration claim and the file it names may differ by this much.
const DURATION_CLAIM_TOLERANCE_MS: i64 = 100;
/// The rendered track may differ from the planned end by this much (atempo/adelay rounding).
const TRACK_TOLERANCE_MS: i64 = 250;

fn invalid(message: impl Into<String>) -> GenError {
    GenError::new("invalid_input", message)
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(t)) => {
            matches!(t.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        }
        _ => false,
    }
}

/// The explicit `passthrough: true` flag (same language, glossary-only translation allowed).
pub(super) fn is_passthrough(inputs: &Value) -> bool {
    truthy(inputs.get("passthrough"))
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(t)) => t.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn norm_lang(lang: &str) -> String {
    lang.trim().to_ascii_lowercase().replace('_', "-")
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// The dubbing language contract, from run inputs or a plan request: `source_lang` and
/// `target_lang` are both explicit and different. `passthrough: true` is the explicit test flag
/// that allows the same language (and, in the run, glossary-only translation) so the media path can
/// be exercised without a translation provider; the result says so.
pub(super) fn check_languages(inputs: &Value) -> GenResult<()> {
    let source = norm_lang(&s(inputs, "source_lang"));
    let target = norm_lang(&s(inputs, "target_lang"));
    if source.is_empty() || target.is_empty() {
        return Err(GenError::new(
            "invalid_input",
            "dubbing needs explicit source_lang and target_lang (no language is assumed)",
        ));
    }
    if source == target && !truthy(inputs.get("passthrough")) {
        return Err(GenError::new(
            "dubbing_same_language",
            format!("source_lang and target_lang are both '{source}': nothing to dub (set passthrough: true only for a media-path test)"),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// plan arithmetic (pure)

struct Seg {
    id: String,
    start: i64,
    end: i64,
    src: String,
    text: String,
    /// 0 for a blank segment.
    synth: i64,
    audio: Option<String>,
}

struct Placed {
    action: &'static str,
    tempo: Option<f64>,
    start: i64,
    ms: i64,
    slot: i64,
    shift: i64,
    excess: i64,
    overlap: i64,
}

fn ms_of(seg: &Value, ms_key: &str, s_key: &str) -> Option<i64> {
    if let Some(v) = seg.get(ms_key).and_then(Value::as_f64) {
        return Some(v.round() as i64);
    }
    seg.get(s_key)
        .and_then(Value::as_f64)
        .map(|v| (v * 1000.0).round() as i64)
}

fn id_of(seg: &Value, pos: usize) -> String {
    match seg.get("id").or_else(|| seg.get("index")) {
        Some(Value::String(t)) if !t.trim().is_empty() => t.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => (pos + 1).to_string(),
    }
}

fn probe_ms(path: &str) -> GenResult<i64> {
    super::audio_duration(path)
        .map(|d| (d * 1000.0).round() as i64)
        .ok_or_else(|| invalid(format!("cannot read the duration of {path}")))
}

fn parse_segments(input: &Value) -> GenResult<Vec<Seg>> {
    let arr = input
        .get("segments")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| invalid("a dubbing plan needs a non-empty segments array"))?;
    let mut out: Vec<Seg> = Vec::with_capacity(arr.len());
    let mut seen: HashSet<String> = HashSet::new();
    for (pos, v) in arr.iter().enumerate() {
        let id = id_of(v, pos);
        if !seen.insert(id.clone()) {
            return Err(invalid(format!("duplicate segment id {id}")));
        }
        let start = ms_of(v, "start_ms", "start_s")
            .ok_or_else(|| invalid(format!("segment {id}: missing start_ms/start_s")))?;
        let end = ms_of(v, "end_ms", "end_s")
            .ok_or_else(|| invalid(format!("segment {id}: missing end_ms/end_s")))?;
        if start < 0 || end <= start {
            return Err(invalid(format!(
                "segment {id}: needs 0 <= start < end (got {start}..{end} ms)"
            )));
        }
        if let Some(prev) = out.last() {
            if start <= prev.start {
                return Err(invalid(format!(
                    "segment {id}: segments must be in order with increasing starts"
                )));
            }
        }
        let text = s(v, "text");
        let src = {
            let t = s(v, "source_text");
            if t.is_empty() {
                s(v, "text_source")
            } else {
                t
            }
        };
        let audio = Some(s(v, "audio_path")).filter(|p| !p.is_empty());
        let given = ms_of(v, "synth_ms", "synth_s");
        let probed = match &audio {
            Some(p) => Some(probe_ms(p)?),
            None => None,
        };
        let synth = match (given, probed) {
            (Some(g), Some(p)) => {
                if (g - p).abs() > DURATION_CLAIM_TOLERANCE_MS {
                    return Err(invalid(format!(
                        "segment {id}: synth_ms {g} disagrees with the audio file ({p} ms)"
                    )));
                }
                Some(g)
            }
            (Some(g), None) => Some(g),
            (None, Some(p)) => Some(p),
            (None, None) => None,
        };
        let synth = if text.is_empty() {
            if synth.unwrap_or(0) != 0 {
                return Err(invalid(format!("segment {id}: has audio but no text")));
            }
            0
        } else {
            match synth {
                Some(d) if d > 0 => d,
                _ => return Err(invalid(format!("segment {id}: has text but no synthesised duration (synth_ms/synth_s or audio_path)"))),
            }
        };
        out.push(Seg {
            id,
            start,
            end,
            src,
            text,
            synth,
            audio,
        });
    }
    if out.iter().all(|g| g.synth == 0) {
        return Err(invalid("every segment is blank: nothing to dub"));
    }
    Ok(out)
}

fn place(segs: &[Seg], max_tempo: f64, allow_shift: bool, gap: i64) -> Vec<Placed> {
    let mut out: Vec<Placed> = Vec::with_capacity(segs.len());
    let mut prev_end: Option<i64> = None;
    for (i, g) in segs.iter().enumerate() {
        let next_start = segs[i + 1..].iter().find(|n| n.synth > 0).map(|n| n.start);
        let bound = next_start.map_or(g.end, |n| n.min(g.end));
        let slot = bound - g.start;
        if g.synth == 0 {
            out.push(Placed {
                action: "empty",
                tempo: None,
                start: g.start,
                ms: 0,
                slot,
                shift: 0,
                excess: 0,
                overlap: 0,
            });
            continue;
        }
        let start = match (allow_shift, prev_end) {
            (true, Some(pe)) => g.start.max(pe + gap),
            _ => g.start,
        };
        let avail = bound - start;
        let synth = g.synth;
        let (action, tempo, ms, excess) = if avail > 0 && synth <= avail {
            (
                if avail - synth <= FIT_TOLERANCE_MS {
                    "fits"
                } else {
                    "short"
                },
                None,
                synth,
                0,
            )
        } else if avail > 0 && (synth as f64) / (avail as f64) <= max_tempo + 1e-9 {
            ("tempo", Some(round4(synth as f64 / avail as f64)), avail, 0)
        } else if allow_shift {
            if max_tempo > 1.0 {
                (
                    "extend",
                    Some(round4(max_tempo)),
                    (synth as f64 / max_tempo).ceil() as i64,
                    0,
                )
            } else {
                ("extend", None, synth, 0)
            }
        } else {
            let overlap = next_start.map_or(0, |n| (start + synth - n).max(0));
            out.push(Placed {
                action: "overflow",
                tempo: None,
                start,
                ms: synth,
                slot,
                shift: 0,
                excess: synth - slot,
                overlap,
            });
            prev_end = Some(start + synth);
            continue;
        };
        out.push(Placed {
            action,
            tempo,
            start,
            ms,
            slot,
            shift: start - g.start,
            excess,
            overlap: 0,
        });
        prev_end = Some(start + ms);
    }
    out
}

/// `studio.pipelines.dubbing_plan` core. Pure: no files, no ffmpeg, unless a segment names an
/// `audio_path` (its duration is then read with ffprobe).
pub fn plan_value(input: &Value) -> GenResult<Value> {
    check_languages(input)?;
    let max_tempo = number(input.get("max_tempo")).unwrap_or(DEFAULT_MAX_TEMPO);
    if !(1.0..=MAX_TEMPO_LIMIT).contains(&max_tempo) {
        return Err(invalid(format!(
            "max_tempo must be between 1.0 and {MAX_TEMPO_LIMIT} (got {max_tempo})"
        )));
    }
    let allow_shift = truthy(input.get("allow_shift"));
    let gap = number(input.get("min_gap_ms")).map_or(0, |v| v.round() as i64);
    if gap < 0 {
        return Err(invalid("min_gap_ms must be >= 0"));
    }
    let media_ms = number(input.get("media_duration_ms"))
        .map(|v| v.round() as i64)
        .filter(|v| *v > 0);
    let segs = parse_segments(input)?;
    let placed = place(&segs, max_tempo, allow_shift, gap);

    let mut report: Vec<Value> = Vec::with_capacity(segs.len());
    let mut cues: Vec<(f64, f64, String)> = Vec::new();
    let mut overflow_ids: Vec<String> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let (mut max_shift, mut last_end, mut total_excess, mut max_ratio) =
        (0_i64, 0_i64, 0_i64, 0.0_f64);
    let mut final_shift = 0_i64;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for (g, p) in segs.iter().zip(placed.iter()) {
        *counts.entry(p.action.to_string()).or_insert(0) += 1;
        if g.synth == 0 {
            report.push(json!({
                "id": g.id, "source_text": g.src, "target_text": g.text,
                "source_start_ms": g.start, "source_end_ms": g.end, "slot_ms": p.slot,
                "synth_ms": 0, "action": "empty", "tempo": Value::Null, "shift_ms": 0,
                "placed_start_ms": Value::Null, "placed_end_ms": Value::Null, "placed_ms": 0,
                "excess_ms": 0, "overlaps_next_ms": 0,
            }));
            continue;
        }
        let end = p.start + p.ms;
        report.push(json!({
            "id": g.id, "source_text": g.src, "target_text": g.text,
            "source_start_ms": g.start, "source_end_ms": g.end, "slot_ms": p.slot,
            "synth_ms": g.synth, "action": p.action, "tempo": p.tempo, "shift_ms": p.shift,
            "placed_start_ms": p.start, "placed_end_ms": end, "placed_ms": p.ms,
            "excess_ms": p.excess, "overlaps_next_ms": p.overlap,
        }));
        if p.action == "overflow" {
            overflow_ids.push(g.id.clone());
            total_excess += p.excess;
            let ratio = g.synth as f64 / p.slot.max(1) as f64;
            max_ratio = max_ratio.max(ratio);
            reasons.push(format!(
                "segment {}: synthesised {} ms is {} ms over its {} ms slot and needs x{:.3} (max_tempo {:.2})",
                g.id, g.synth, p.excess, p.slot, ratio, max_tempo
            ));
        }
        max_shift = max_shift.max(p.shift);
        final_shift = p.shift;
        last_end = last_end.max(end);
        cues.push((p.start as f64 / 1000.0, end as f64 / 1000.0, g.text.clone()));
    }
    if let Some(m) = media_ms {
        if last_end > m {
            reasons.push(format!(
                "the dub track ends at {last_end} ms, {} ms past the media ({m} ms); composing would cut it",
                last_end - m
            ));
        }
    }

    let status = if reasons.is_empty() {
        "ok"
    } else {
        "needs_review"
    };
    let mut options: Vec<String> = Vec::new();
    if status == "needs_review" {
        if !overflow_ids.is_empty() {
            options.push(format!(
                "shorten the translation of segment(s) {} and re-run the dub",
                overflow_ids.join(", ")
            ));
            if max_ratio <= MAX_TEMPO_LIMIT {
                options.push(format!(
                    "allow a larger stretch: max_tempo of at least {:.2} (speech that much faster than natural)",
                    ((max_ratio * 100.0) - 1e-9).ceil() / 100.0
                ));
            } else {
                options.push(format!(
                    "a larger stretch is not enough: x{max_ratio:.2} exceeds the {MAX_TEMPO_LIMIT} single-stage atempo ceiling"
                ));
            }
            let preview = place(&segs, max_tempo, true, gap);
            let drift = preview.iter().map(|p| p.shift).max().unwrap_or(0);
            options.push(format!("allow_shift: true moves later segments after longer ones (preview: up to {drift} ms later)"));
        }
        if media_ms.is_some_and(|m| last_end > m) {
            options.push(
                "lengthen the video or shorten the translation so the dub ends inside the media"
                    .to_string(),
            );
        }
    }

    let srt = if status == "ok" {
        json!(super::srt_text(&cues))
    } else {
        Value::Null
    };
    Ok(json!({
        "status": status,
        "alignment": "segment",
        "word_alignment": "unavailable: needs a RightKit alignment capability (rightkit-align is not vendored); cue times are segment-level",
        "source_lang": norm_lang(&s(input, "source_lang")),
        "target_lang": norm_lang(&s(input, "target_lang")),
        "passthrough": truthy(input.get("passthrough")),
        "policy": {
            "max_tempo": max_tempo, "allow_shift": allow_shift, "min_gap_ms": gap,
            "fit_tolerance_ms": FIT_TOLERANCE_MS, "tempo_filter": "atempo (pitch preserving, never slows speech)",
            "slot": "start to min(own end, next audible segment's source start); with allow_shift the start is max(source start, previous placed end + min_gap_ms)",
        },
        "segments": report,
        "summary": {
            "counts": counts, "overflow_count": overflow_ids.len(), "overflow_ids": overflow_ids,
            "total_excess_ms": total_excess, "max_shift_ms": max_shift, "final_shift_ms": final_shift,
            "track_end_ms": last_end, "media_duration_ms": media_ms,
        },
        "reasons": reasons,
        "options": options,
        "srt": srt,
    }))
}

// ---------------------------------------------------------------------------
// the dub track (ffmpeg)

fn render_track(segs: &[Seg], placed: &[Placed], total_ms: i64, out: &Path) -> GenResult<()> {
    let t = media::tools();
    let ffmpeg = t.ffmpeg.clone().ok_or_else(|| {
        GenError::new(
            "tool_missing",
            format!(
                "no verified ffmpeg: {}",
                t.ffmpeg_error.as_deref().unwrap_or("unresolved")
            ),
        )
    })?;
    let legs: Vec<(&Seg, &Placed)> = segs
        .iter()
        .zip(placed.iter())
        .filter(|(g, _)| g.synth > 0)
        .collect();
    let n = legs.len();
    let total_s = format!("{:.3}", total_ms as f64 / 1000.0);
    let mut args: Vec<String> = ["-y", "-hide_banner", "-loglevel", "error"]
        .iter()
        .map(|x| x.to_string())
        .collect();
    let mut graph: Vec<String> = Vec::with_capacity(n + 1);
    for (k, (g, p)) in legs.iter().enumerate() {
        let path = g
            .audio
            .clone()
            .ok_or_else(|| invalid(format!("segment {}: no audio_path to place", g.id)))?;
        if !PathBuf::from(&path).is_file() {
            return Err(invalid(format!(
                "segment {}: audio not found: {path}",
                g.id
            )));
        }
        args.extend(["-i".to_string(), path]);
        let mut chain = vec![Filter::new("aformat")
            .opt("sample_rates", 44100)
            .opt("channel_layouts", "mono")];
        if let Some(t) = p.tempo.filter(|t| *t > 1.0 + 1e-6) {
            chain.push(Filter::new("atempo").pos(format!("{t:.4}")));
        }
        chain.push(Filter::new("adelay").pos(format!("{0}|{0}", p.start)));
        // Every leg runs to the same length, so amix's input scaling stays constant (1/N); the
        // volume stage below undoes it. Legs do not overlap in an `ok` plan, so this is a sum of
        // silences and one clip at a time.
        chain.push(Filter::new("apad").opt("whole_dur", &total_s));
        graph.push(format!("[{k}:a]{}[l{k}]", render_chain(&chain)));
    }
    let out_label = if n == 1 {
        "l0".to_string()
    } else {
        let ins: String = (0..n).map(|k| format!("[l{k}]")).collect();
        let mix = render_chain(&[
            Filter::new("amix")
                .opt("inputs", n)
                .opt("duration", "longest")
                .opt("dropout_transition", 0),
            Filter::new("volume").pos(n),
        ]);
        graph.push(format!("{ins}{mix}[dubout]"));
        "dubout".to_string()
    };
    args.extend([
        "-filter_complex".to_string(),
        graph.join(";"),
        "-map".to_string(),
        format!("[{out_label}]"),
        "-ar".to_string(),
        "44100".to_string(),
        "-ac".to_string(),
        "1".to_string(),
        "-c:a".to_string(),
        "pcm_s16le".to_string(),
        out.to_string_lossy().into_owned(),
    ]);
    let (ok, _, stderr) = media::run(ffmpeg.ffmpeg_path(), &args, "ffmpeg")?;
    if !ok || !out.is_file() {
        let tail: String = stderr
            .trim()
            .chars()
            .rev()
            .take(900)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(GenError::new(
            "dub_assemble_failed",
            format!("ffmpeg could not place the dub clips: {tail}"),
        ));
    }
    Ok(())
}

/// `studio.pipelines.dubbing_plan` `{source_lang, target_lang, segments:[{id, start_ms|start_s,
/// end_ms|end_s, text, source_text?, synth_ms|synth_s|audio_path}], max_tempo?, allow_shift?,
/// min_gap_ms?, media_duration_ms?, passthrough?, out_dir?}` -> the placement plan. With `out_dir`
/// and every audible segment carrying `audio_path`, an `ok` plan also renders `dub_track.wav` and
/// writes `dub_plan.json`; a `needs_review` plan writes only `dub_plan.json`.
pub fn plan_command(_ctx: &Context, input: &Value) -> GenResult<Value> {
    let mut result = plan_value(input)?;
    let out_dir = s(input, "out_dir");
    if out_dir.is_empty() {
        return Ok(result);
    }
    let dir = PathBuf::from(&out_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| GenError::new("io", format!("cannot create {}: {e}", dir.display())))?;
    if result["status"] == "ok" {
        let segs = parse_segments(input)?;
        let max_tempo = number(input.get("max_tempo")).unwrap_or(DEFAULT_MAX_TEMPO);
        let placed = place(
            &segs,
            max_tempo,
            truthy(input.get("allow_shift")),
            number(input.get("min_gap_ms")).map_or(0, |v| v.round() as i64),
        );
        let missing: Vec<String> = segs
            .iter()
            .filter(|g| g.synth > 0 && g.audio.is_none())
            .map(|g| g.id.clone())
            .collect();
        if !missing.is_empty() {
            return Err(invalid(format!(
                "out_dir given but segment(s) {} have no audio_path",
                missing.join(", ")
            )));
        }
        let total_ms = result["summary"]["track_end_ms"].as_i64().unwrap_or(0);
        let track = dir.join("dub_track.wav");
        render_track(&segs, &placed, total_ms, &track)?;
        let got_ms = probe_ms(&track.to_string_lossy())?;
        if (got_ms - total_ms).abs() > TRACK_TOLERANCE_MS {
            return Err(GenError::new(
                "dub_track_mismatch",
                format!("rendered track is {got_ms} ms but the plan ends at {total_ms} ms; the clip placement did not do what the plan says"),
            ));
        }
        let bytes = std::fs::read(&track).map_err(|e| GenError::new("io", e.to_string()))?;
        result["track_path"] = json!(track.to_string_lossy());
        result["track_sha256"] = json!(rightkit_media_gen::library::sha256_hex(&bytes));
        result["track_bytes"] = json!(bytes.len());
        result["track_duration_ms"] = json!(got_ms);
    }
    let plan_path = dir.join("dub_plan.json");
    rightkit_fs::atomic_write(
        &plan_path,
        &serde_json::to_vec_pretty(&result).unwrap_or_default(),
    )
    .map_err(|e| GenError::new("io", format!("cannot write {}: {e}", plan_path.display())))?;
    result["plan_path"] = json!(plan_path.to_string_lossy());
    Ok(result)
}

// ---------------------------------------------------------------------------
// run glue (native step functions)

fn read_segments(asset: &Value) -> GenResult<Vec<Value>> {
    let path = s(asset, "segments_path");
    if path.is_empty() {
        return Err(invalid(
            "the translate step recorded no segments_path (re-run translate)",
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| GenError::new("io", format!("cannot read {path}: {e}")))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| GenError::new("io", format!("{path} is not JSON: {e}")))?;
    Ok(v["segments"].as_array().cloned().unwrap_or_default())
}

fn translate_segments(run: &Value) -> GenResult<(Value, Vec<Value>)> {
    let step = super::run_step(run, "translate")
        .ok_or_else(|| invalid("this run has no translate step"))?;
    let asset = step["assets"]
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .ok_or_else(|| invalid("the translate step has not produced its asset"))?;
    let segs = read_segments(&asset)?;
    Ok((asset, segs))
}

fn segment_id(seg: &Value, pos: usize) -> String {
    id_of(seg, pos)
}

/// One voice request per translated segment, for `step_begin` of the `dub` step. Each entry is
/// `{segment_id, start_ms, end_ms, source_text, request}`; the caller submits every `request`
/// through the voice route (so consent and asset facts apply per job) and settles the step with
/// one audio asset per segment, each tagged with its `segment_id`.
pub(super) fn voice_requests(run: &Value, base: &Value) -> GenResult<Vec<Value>> {
    let (_, segs) = translate_segments(run)?;
    let inputs = &run["inputs"];
    let mut out = Vec::new();
    for (pos, seg) in segs.iter().enumerate() {
        let text = s(seg, "text");
        if text.is_empty() {
            continue;
        }
        let mut request = base.clone();
        request["prompt"] = json!(text);
        if !request["params"].is_object() {
            request["params"] = json!({});
        }
        for key in ["voice_prompt", "voice_id"] {
            let v = s(inputs, key);
            if !v.is_empty() {
                request["params"][key] = json!(v);
            }
        }
        out.push(json!({
            "segment_id": segment_id(seg, pos),
            "start_ms": ms_of(seg, "start_ms", "start_s"),
            "end_ms": ms_of(seg, "end_ms", "end_s"),
            "source_text": s(seg, "text_source"),
            "request": request,
        }));
    }
    if out.is_empty() {
        return Err(invalid("the translation has no non-blank segment to voice"));
    }
    Ok(out)
}

/// The `dub_plan` native step: join the translated segments with the per-segment dub clips, plan,
/// and (when the plan is `ok`) render the dub track. Returns the step asset; a `needs_review` plan
/// returns a data asset carrying `review_status: "needs_review"` (the caller marks the step).
pub(super) fn plan_step(ctx: &Context, run: &Value, step: &Value, dir: &Path) -> GenResult<Value> {
    let inputs = &run["inputs"];
    check_languages(inputs)?;
    let passthrough = truthy(inputs.get("passthrough"));
    let step_id = super::tstr(step, "id");
    let asset_id = format!("{}-{}", super::tstr(run, "run_id"), step_id);
    let (tr_asset, segs) = translate_segments(run)?;
    if !passthrough && tr_asset["method"] != "llm" {
        return Err(GenError::new(
            "dubbing_needs_llm_translation",
            "a dub needs a sentence translation (translate_provider); glossary substitution is not a translation",
        ));
    }
    let dub = super::run_step(run, "dub").ok_or_else(|| invalid("this run has no dub step"))?;
    let clips: Vec<&Value> = dub["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|x| {
                    x["kind"] == "audio" && Path::new(super::tstr(x, "path").as_str()).is_file()
                })
                .collect()
        })
        .unwrap_or_default();
    if clips.is_empty() {
        return Err(invalid("the dub step produced no audio on disk"));
    }
    let tagged: Vec<(&Value, String)> = clips
        .iter()
        .filter_map(|c| {
            let id = match c.get("segment_id") {
                Some(Value::String(t)) if !t.trim().is_empty() => Some(t.trim().to_string()),
                Some(Value::Number(n)) => Some(n.to_string()),
                _ => None,
            };
            id.map(|id| (*c, id))
        })
        .collect();
    if tagged.is_empty() {
        if passthrough {
            // Whole-script narration, no per-segment clips: no retiming is possible and none is claimed.
            let a = clips[0];
            return Ok(json!({
                "asset_id": asset_id, "path": a["path"], "sha256": a["sha256"], "bytes": a["bytes"],
                "mime": "audio/wav", "kind": "audio", "duration_s": a["duration_s"],
                "review_status": "ok", "alignment": "none", "retiming": "none (passthrough: one whole-script clip)",
            }));
        }
        return Err(GenError::new(
            "dub_segments_missing",
            "the dub step needs one audio asset per translated segment, each tagged with its segment_id (see the requests step_begin returned)",
        ));
    }
    let known: HashSet<String> = segs
        .iter()
        .enumerate()
        .map(|(i, g)| segment_id(g, i))
        .collect();
    let stray: Vec<&str> = tagged
        .iter()
        .map(|(_, id)| id.as_str())
        .filter(|id| !known.contains(*id))
        .collect();
    if !stray.is_empty() {
        return Err(GenError::new(
            "dub_unknown_segment",
            format!(
                "dub clips name segment(s) {} that the translation does not have",
                stray.join(", ")
            ),
        ));
    }
    let mut plan_segs: Vec<Value> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for (pos, g) in segs.iter().enumerate() {
        let id = segment_id(g, pos);
        let text = s(g, "text");
        let mut entry = json!({
            "id": id, "start_ms": ms_of(g, "start_ms", "start_s"), "end_ms": ms_of(g, "end_ms", "end_s"),
            "text": text, "source_text": s(g, "text_source"),
        });
        if !text.is_empty() {
            match tagged.iter().find(|(_, tid)| *tid == id) {
                Some((c, _)) => {
                    entry["audio_path"] = c["path"].clone();
                }
                None => missing.push(id.clone()),
            }
        }
        plan_segs.push(entry);
    }
    if !missing.is_empty() {
        return Err(GenError::new(
            "dub_incomplete",
            format!("no dub clip for segment(s) {}: voice every translated segment before planning (no audio is dropped)", missing.join(", ")),
        ));
    }
    let sm = super::clean(inputs.get("source_media"));
    let media_ms = if sm.is_empty() {
        None
    } else {
        media::probe(Path::new(&sm))
            .ok()
            .filter(|p| p["video"].is_object())
            .and_then(|p| p["duration_s"].as_f64())
            .map(|d| (d * 1000.0).round() as i64)
    };
    let mut req = json!({
        "source_lang": s(inputs, "source_lang"), "target_lang": s(inputs, "target_lang"),
        "passthrough": passthrough, "segments": plan_segs, "out_dir": dir.to_string_lossy(),
    });
    for key in ["max_tempo", "allow_shift", "min_gap_ms"] {
        if let Some(v) = inputs
            .get(key)
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
        {
            req[key] = v.clone();
        }
    }
    if let Some(m) = media_ms {
        req["media_duration_ms"] = json!(m);
    }
    let plan = plan_command(ctx, &req)?;
    if plan["status"] != "ok" {
        let plan_path = s(&plan, "plan_path");
        let bytes = std::fs::read(&plan_path).map_err(|e| GenError::new("io", e.to_string()))?;
        return Ok(json!({
            "asset_id": asset_id, "path": plan_path, "sha256": rightkit_media_gen::library::sha256_hex(&bytes), "bytes": bytes.len(),
            "mime": "application/json", "kind": "data", "plan_path": plan_path,
            "review_status": "needs_review", "review_summary": plan["reasons"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")).unwrap_or_default(),
            "options": plan["options"], "alignment": "segment", "summary": plan["summary"], "report": plan["segments"],
        }));
    }
    Ok(json!({
        "asset_id": asset_id, "path": plan["track_path"], "sha256": plan["track_sha256"], "bytes": plan["track_bytes"],
        "mime": "audio/wav", "kind": "audio", "duration_s": plan["track_duration_ms"].as_f64().map(|m| m / 1000.0),
        "plan_path": plan["plan_path"], "review_status": "ok", "alignment": "segment",
        "summary": plan["summary"], "report": plan["segments"], "policy": plan["policy"],
    }))
}

/// The aligned SRT text of a dubbing run's plan, when the run has one.
pub(super) fn aligned_srt(run: &Value) -> Option<String> {
    let step = super::run_step(run, "dub_plan")?;
    let asset = step["assets"].as_array()?.first()?;
    let path = asset["plan_path"].as_str()?;
    let plan: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    plan["srt"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(String::from)
}
