//! The QA gate: probes a final render and judges it against a [`QaSpec`].

use std::path::Path;
use std::time::Duration;

use crate::{
    rule_ids, ArtifactDescription, ArtifactKind, Declarations, Finding, Rule, RuleKind, RuleSet,
    Subject, Verdict,
};
use serde_json::{json, Value};

use crate::delivery::{
    self, CaptionCue, CaptionRule, DecodeIntegrity, DurationRule, FormatRule, FrameRateRule,
    StreamSyncRule, StreamTiming,
};
use crate::qa::{unexpected_intervals, AllowedWindow, Interval as QaInterval};
use crate::render_qa::critic::{
    checks_from_findings, critic_unavailable_check, default_critic_binary, CriticInput,
    CriticRunner, Span, HOLD_SLACK_MS, RULE_BLACK, RULE_SILENCE,
};
use crate::render_qa::error::QaError;
use crate::render_qa::probe::{self, Interval, Probe, QaTools};
use crate::render_qa::report::{
    CheckStatus, MediaFacts, OverallStatus, QaCheck, QaReport, RenderIdentity, Severity,
    CHECK_VERSION, REPORT_SCHEMA_VERSION,
};
use crate::render_qa::spec::{parse_srt, QaExpect, QaSpec};
use rightkit_fs::digest::{blake3_bytes, blake3_file};

fn missing(id: &str, what: &str) -> QaCheck {
    QaCheck::measured(
        id,
        Severity::Error,
        false,
        "",
        format!("no {what} stream to check"),
        Value::Null,
    )
}

fn ms(s: Option<f64>) -> Option<i64> {
    s.map(|v| (v * 1000.0).round() as i64)
}

/// Length that closes an interval a detector leaves open at end of stream: the container
/// duration, else the video stream's. 0 when neither is readable; `media.duration` then fails.
fn detector_end_ms(p: &Probe) -> i64 {
    ms(p.format_duration_s)
        .or_else(|| ms(p.video.as_ref().and_then(|v| v.duration_s)))
        .unwrap_or(0)
}

/// Note for a detector check whose pass did not run because the decode was not clean.
/// `file.integrity` is Fail exactly when the decode is not clean, so it carries the blocking failure.
const UNDECODED: &str =
    "stream did not decode cleanly, so the detector result is unavailable (file.integrity fails)";

fn intervals_json(v: &[Interval]) -> Value {
    Value::Array(
        v.iter()
            .map(|i| json!({"start_ms": i.start_ms, "end_ms": i.end_ms, "duration_ms": i.len_ms()}))
            .collect(),
    )
}

/// Intervals outside timeline allowances, scoped to one check with 100 ms slack.
fn unexpected(
    id: &str,
    found: &[Interval],
    allowed: &[(i64, i64)],
) -> Result<Vec<Interval>, crate::ReviewError> {
    let measured: Vec<_> = found
        .iter()
        .map(|i| QaInterval {
            start_ms: i.start_ms,
            end_ms: i.end_ms,
        })
        .collect();
    let windows: Vec<_> = allowed
        .iter()
        .map(|&(start_ms, end_ms)| AllowedWindow {
            check_id: id.into(),
            interval: QaInterval { start_ms, end_ms },
            slack_ms: 100,
            reason: "timeline-declared allowance".into(),
        })
        .collect();
    Ok(unexpected_intervals(id, &measured, &windows)?
        .into_iter()
        .map(|i| Interval {
            start_ms: i.start_ms,
            end_ms: i.end_ms,
        })
        .collect())
}

fn checked(id: &str, value: Result<QaCheck, crate::ReviewError>) -> QaCheck {
    value.unwrap_or_else(|e| {
        QaCheck::measured(id, Severity::Error, false, "", e.to_string(), Value::Null)
    })
}

fn delivery_limits(spec: &QaSpec) -> Result<[u64; 4], crate::ReviewError> {
    let mut limits = [0; 4];
    for (i, (name, value)) in [
        ("duration_tolerance_ms", spec.duration_tolerance_ms),
        ("max_av_start_offset_ms", spec.max_av_start_offset_ms),
        ("max_av_drift_ms", spec.max_av_drift_ms),
        ("max_tail_gap_ms", spec.captions.max_tail_gap_ms),
    ]
    .into_iter()
    .enumerate()
    {
        limits[i] = u64::try_from(value).map_err(|_| {
            crate::ReviewError::InvalidRuleSet(format!("{name} must be nonnegative"))
        })?;
    }
    Ok(limits)
}

/// Video geometry against the preset, judged by the shared review rule `det.resolution`
/// (`RuleKind::ResolutionMatchesDelivery`, rightkit-review rules.rs:592-615). That rule is
/// exact equality, the same comparison this check made locally. The message and evidence
/// stay the host's. A rule set that cannot be built counts as a failed check.
fn resolution_matches(width: u32, height: u32, want_width: u32, want_height: u32) -> bool {
    let Ok(rules) = RuleSet::new(
        "cutright.qa.geometry",
        CHECK_VERSION,
        vec![Rule {
            id: rule_ids::RESOLUTION.to_string(),
            required: true,
            kind: RuleKind::ResolutionMatchesDelivery,
        }],
    ) else {
        return false;
    };
    let subject = Subject {
        artifact: ArtifactDescription {
            artifact_id: "render".into(),
            kind: ArtifactKind::Video,
            width: Some(width),
            height: Some(height),
            duration_ms: None,
            frames: None,
            max_flashes_per_second: None,
            text_regions: None,
            determinism: None,
            licence: None,
            text: None,
        },
        declarations: Declarations {
            target_width: Some(want_width),
            target_height: Some(want_height),
            ..Declarations::default()
        },
    };
    matches!(
        rules.evaluate(&subject).as_deref(),
        Ok([Finding {
            verdict: Verdict::Verified,
            ..
        }])
    )
}

/// Pure judgement of the probe results; no process is started here.
pub struct Measurements {
    pub probe: Probe,
    pub decode: (bool, String),
    pub black: Vec<Interval>,
    pub freeze: Vec<Interval>,
    pub silence: Vec<Interval>,
    pub loudness: Option<probe::Ebur128>,
    pub clip: Option<probe::ClipCounter>,
}

pub fn judge(spec: &QaSpec, expect: &QaExpect, m: &Measurements) -> Vec<QaCheck> {
    use Severity::{Error, Warning};
    let [duration_tolerance_ms, max_av_start_offset_ms, max_av_drift_ms, _] =
        match delivery_limits(spec) {
            Ok(limits) => limits,
            Err(e) => {
                return vec![QaCheck::measured(
                    "policy.delivery",
                    Error,
                    false,
                    "",
                    e.to_string(),
                    Value::Null,
                )]
            }
        };
    let mut checks = Vec::new();
    let p = &m.probe;
    let duration_ms = ms(p.format_duration_s);

    checks.push(delivery::decode_integrity(Some(&DecodeIntegrity {
        completed: true,
        success: m.decode.0,
        errors: m.decode.1.clone(),
    })));

    let has_video = p.video.is_some();
    let has_audio = p.audio.is_some();
    checks.push(delivery::required_streams(
        has_video,
        has_audio,
        spec.require_audio,
    ));
    checks.push(checked(
        "media.duration",
        DurationRule {
            expected_ms: expect.duration_ms,
            tolerance_ms: duration_tolerance_ms,
        }
        .check(duration_ms),
    ));

    // Video geometry, rate, codec.
    match &p.video {
        Some(v) => {
            checks.push(QaCheck::measured(
                "video.resolution",
                Error,
                resolution_matches(v.width, v.height, spec.width, spec.height),
                format!("{}x{} matches the {} preset", v.width, v.height, spec.preset),
                format!(
                    "{}x{} differs from the {} preset ({}x{})",
                    v.width, v.height, spec.preset, spec.width, spec.height
                ),
                json!({"expected": {"width": spec.width, "height": spec.height}, "probed": {"width": v.width, "height": v.height}}),
            ));
            checks.push(checked(
                "video.frame_rate",
                FrameRateRule {
                    expected_fps: expect.fps.map(|f| f.num as f64 / f.den as f64),
                    absolute_tolerance: spec.fps_tolerance,
                    relative_tolerance: 0.0005,
                }
                .check(v.fps),
            ));
            checks.push(checked(
                "video.codec",
                FormatRule {
                    allowed: spec.video_codecs.clone(),
                }
                .check("video.codec", Error, Some(&v.codec)),
            ));
            checks.push(checked(
                "video.pixel_format",
                FormatRule {
                    allowed: spec.pixel_formats.clone(),
                }
                .check("video.pixel_format", Warning, v.pix_fmt.as_deref()),
            ));
        }
        None => {
            for id in ["video.resolution", "video.frame_rate", "video.codec"] {
                checks.push(missing(id, "video"));
            }
        }
    }
    match &p.audio {
        Some(a) => checks.push(checked(
            "audio.codec",
            FormatRule {
                allowed: spec.audio_codecs.clone(),
            }
            .check("audio.codec", Warning, Some(&a.codec)),
        )),
        None if spec.require_audio => checks.push(missing("audio.codec", "audio")),
        None => checks.push(QaCheck::not_applicable("audio.codec", "audio not required")),
    }

    // Black frames.
    if !has_video {
        checks.push(missing("video.black_frames", "video"));
        checks.push(missing("video.frozen_frames", "video"));
    } else if !m.decode.0 {
        checks.push(QaCheck::not_applicable("video.black_frames", UNDECODED));
        checks.push(QaCheck::not_applicable("video.frozen_frames", UNDECODED));
    } else {
        let bad = match unexpected("video.black_frames", &m.black, &expect.allowed_black) {
            Ok(bad) => bad,
            Err(e) => {
                checks.push(QaCheck::measured(
                    "video.black_frames",
                    Error,
                    false,
                    "",
                    e.to_string(),
                    Value::Null,
                ));
                return checks;
            }
        };
        let worst = bad.iter().map(Interval::len_ms).max().unwrap_or(0);
        checks.push(QaCheck::measured(
            "video.black_frames",
            Error,
            worst <= spec.max_black_ms,
            "no unexpected black frames",
            format!("black frames for {worst} ms (limit {} ms)", spec.max_black_ms),
            json!({"found": intervals_json(&m.black), "unexpected": intervals_json(&bad), "limit_ms": spec.max_black_ms}),
        ));
        let bad = match unexpected("video.frozen_frames", &m.freeze, &expect.allowed_freeze) {
            Ok(bad) => bad,
            Err(e) => {
                checks.push(QaCheck::measured(
                    "video.frozen_frames",
                    Error,
                    false,
                    "",
                    e.to_string(),
                    Value::Null,
                ));
                return checks;
            }
        };
        let worst = bad.iter().map(Interval::len_ms).max().unwrap_or(0);
        checks.push(QaCheck::measured(
            "video.frozen_frames",
            Error,
            worst <= spec.max_freeze_ms,
            "no unexpected frozen frames",
            format!("frozen picture for {worst} ms (limit {} ms)", spec.max_freeze_ms),
            json!({"found": intervals_json(&m.freeze), "unexpected": intervals_json(&bad), "limit_ms": spec.max_freeze_ms}),
        ));
    }

    // Audio: silence, loudness, true peak, clipping.
    if has_audio {
        if m.decode.0 {
            let bad = match unexpected("audio.silence", &m.silence, &expect.allowed_silence) {
                Ok(bad) => bad,
                Err(e) => {
                    checks.push(QaCheck::measured(
                        "audio.silence",
                        Error,
                        false,
                        "",
                        e.to_string(),
                        Value::Null,
                    ));
                    return checks;
                }
            };
            let worst = bad.iter().map(Interval::len_ms).max().unwrap_or(0);
            checks.push(QaCheck::measured(
                "audio.silence",
                Error,
                worst <= spec.max_silence_ms,
                "no silence gaps beyond what is expected",
                format!("silence gap of {worst} ms (limit {} ms)", spec.max_silence_ms),
                json!({"found": intervals_json(&m.silence), "unexpected": intervals_json(&bad), "limit_ms": spec.max_silence_ms}),
            ));
        } else {
            checks.push(QaCheck::not_applicable("audio.silence", UNDECODED));
        }
        match &m.loudness {
            Some(l) => {
                checks.push(match l.integrated_lufs {
                    Some(i) => QaCheck::measured(
                        "audio.loudness",
                        Error,
                        (i - spec.loudness_target_lufs).abs() <= spec.loudness_tolerance_lu,
                        format!("{i:.1} LUFS is on target ({} LUFS)", spec.loudness_target_lufs),
                        format!(
                            "{i:.1} LUFS is off target ({} LUFS +/- {} LU)",
                            spec.loudness_target_lufs, spec.loudness_tolerance_lu
                        ),
                        json!({"integrated_lufs": i, "target_lufs": spec.loudness_target_lufs, "tolerance_lu": spec.loudness_tolerance_lu}),
                    ),
                    None => QaCheck::measured(
                        "audio.loudness",
                        Error,
                        false,
                        "",
                        "audio is silent (below the loudness gate)",
                        json!({"integrated_lufs": Value::Null}),
                    ),
                });
                checks.push(match l.true_peak_dbtp {
                    Some(tp) => QaCheck::measured(
                        "audio.true_peak",
                        Error,
                        tp <= spec.true_peak_ceiling_dbtp,
                        format!("true peak {tp:.1} dBTP is under the ceiling"),
                        format!(
                            "true peak {tp:.1} dBTP exceeds the {} dBTP ceiling",
                            spec.true_peak_ceiling_dbtp
                        ),
                        json!({"true_peak_dbtp": tp, "ceiling_dbtp": spec.true_peak_ceiling_dbtp}),
                    ),
                    None => QaCheck::not_applicable("audio.true_peak", "no signal above the gate"),
                });
            }
            None => {
                for id in ["audio.loudness", "audio.true_peak"] {
                    checks.push(QaCheck::measured(
                        id,
                        Error,
                        false,
                        "",
                        "ebur128 produced no summary",
                        Value::Null,
                    ));
                }
            }
        }
        match &m.clip {
            Some(c) => checks.push(QaCheck::measured(
                "audio.clipping",
                Error,
                c.clipped_runs <= spec.max_clipped_runs,
                "no clipped audio",
                format!(
                    "{} clipped runs ({} full-scale samples)",
                    c.clipped_runs, c.clipped_samples
                ),
                json!({"clipped_runs": c.clipped_runs, "clipped_samples": c.clipped_samples, "sample_peak": c.peak, "threshold": spec.clip_threshold, "min_run": spec.clip_min_run, "samples": c.samples}),
            )),
            None => checks.push(QaCheck::measured("audio.clipping", Error, false, "", "clip scan did not run", Value::Null)),
        }
    } else if spec.require_audio {
        for id in [
            "audio.silence",
            "audio.loudness",
            "audio.true_peak",
            "audio.clipping",
        ] {
            checks.push(missing(id, "audio"));
        }
    } else {
        for id in [
            "audio.silence",
            "audio.loudness",
            "audio.true_peak",
            "audio.clipping",
        ] {
            checks.push(QaCheck::not_applicable(id, "audio not required"));
        }
    }

    let video_time = p.video.as_ref().map(|v| StreamTiming {
        start_s: v.start_s,
        duration_s: v.duration_s,
    });
    let audio_time = p.audio.as_ref().map(|a| StreamTiming {
        start_s: a.start_s,
        duration_s: a.duration_s,
    });
    checks.push(
        StreamSyncRule {
            max_start_offset_ms: max_av_start_offset_ms,
            max_duration_drift_ms: max_av_drift_ms,
        }
        .check(video_time.as_ref(), audio_time.as_ref()),
    );

    // Captions.
    checks.extend(caption_checks(spec, expect, duration_ms));
    checks
}

fn caption_checks(spec: &QaSpec, expect: &QaExpect, duration_ms: Option<i64>) -> Vec<QaCheck> {
    let failed = |error: String| {
        vec![QaCheck::measured(
            "captions.sanity",
            Severity::Error,
            false,
            "",
            error,
            Value::Null,
        )]
    };
    let [duration_tolerance_ms, _, _, max_tail_gap_ms] = match delivery_limits(spec) {
        Ok(limits) => limits,
        Err(e) => return failed(e.to_string()),
    };
    let rule = CaptionRule {
        required: spec.captions.required,
        max_lines: spec.captions.max_lines,
        max_chars_per_line: spec.captions.max_chars_per_line,
        max_chars_per_second: spec.captions.max_chars_per_second,
        max_tail_gap_ms,
        duration_tolerance_ms,
    };
    let cues = match expect.captions_srt.as_deref() {
        None => None,
        Some(srt) => match parse_srt(srt) {
            Ok(cues) => Some(
                cues.into_iter()
                    .map(|c| CaptionCue {
                        start_ms: c.start_ms,
                        end_ms: c.end_ms,
                        lines: c.lines,
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(e) => return failed(format!("captions unreadable: {e}")),
        },
    };
    match rule.check(cues.as_deref(), duration_ms) {
        Ok(checks) => checks,
        Err(e) => failed(e.to_string()),
    }
}

/// Runs every probe on `render` and writes nothing. The report is bound to the
/// render's BLAKE3.
pub fn run_qa(
    tools: &QaTools,
    render: &Path,
    spec: &QaSpec,
    expect: &QaExpect,
) -> Result<QaReport, QaError> {
    let meta = std::fs::metadata(render)
        .map_err(|e| QaError::io(format!("reading {}", render.display()), e))?;
    if !meta.is_file() {
        return Err(QaError::BadInput(format!(
            "{} is not a file",
            render.display()
        )));
    }
    // Hash first: the report is about exactly these bytes.
    let blake3 =
        blake3_file(render).map_err(|e| QaError::io(format!("hashing {}", render.display()), e))?;
    let decode = probe::decode_clean(tools, render)?;
    let probed = match probe::probe(tools, render) {
        Ok(p) => p,
        Err(QaError::Probe(why)) => {
            // Unreadable container: report it as a failed integrity check.
            return Ok(unreadable_report(
                tools,
                render,
                spec,
                expect,
                blake3,
                meta.len(),
                why,
            ));
        }
        Err(e) => return Err(e),
    };
    let duration_ms = detector_end_ms(&probed);

    let (black, freeze) = if probed.video.is_some() && decode.0 {
        probe::detect_video(
            tools,
            render,
            spec.black_min_ms,
            spec.black_pix_threshold,
            spec.freeze_min_ms,
            spec.freeze_noise_db,
            duration_ms,
        )?
    } else {
        (Vec::new(), Vec::new())
    };
    let (silence, loudness, clip) = match (&probed.audio, decode.0) {
        (Some(a), true) => {
            let (silence, loud) = probe::detect_audio(
                tools,
                render,
                spec.silence_noise_db,
                spec.silence_min_ms,
                duration_ms,
            )?;
            let clip = probe::scan_clipping(
                tools,
                render,
                a.channels,
                spec.clip_threshold,
                spec.clip_min_run,
            )?;
            (silence, loud, Some(clip))
        }
        _ => (Vec::new(), None, None),
    };
    let measurements = Measurements {
        probe: probed.clone(),
        decode,
        black,
        freeze,
        silence,
        loudness,
        clip,
    };
    let mut checks = judge(spec, expect, &measurements);
    checks.extend(critic_checks(
        spec,
        expect,
        &measurements,
        duration_ms,
        probed.video.is_some(),
        probed.audio.is_some(),
    ));
    Ok(assemble(
        tools,
        render,
        spec,
        expect,
        blake3,
        meta.len(),
        Some(&probed),
        checks,
    ))
}

/// Wall-clock bound for one critic child. The critic only does arithmetic on spans.
const CRITIC_TIMEOUT: Duration = Duration::from_secs(60);

fn spans(v: &[Interval]) -> Vec<Span> {
    v.iter()
        .map(|i| Span {
            start_ms: i.start_ms.max(0) as u64,
            end_ms: i.end_ms.max(0) as u64,
        })
        .collect()
}

/// Allowed ranges widened by the same slack `unexpected` gives them, so the critic and the
/// measured checks agree on what is held.
fn holds(ranges: &[(i64, i64)]) -> Vec<Span> {
    ranges
        .iter()
        .map(|&(a, b)| Span {
            start_ms: (a - HOLD_SLACK_MS).max(0) as u64,
            end_ms: (b + HOLD_SLACK_MS).max(0) as u64,
        })
        .collect()
}

/// Runs the out-of-process critic on the silence and black spans this render measured and
/// maps its verdicts into checks. Silence is judged only with audio and a clean decode,
/// black only with video and a clean decode; otherwise that critic check is not applicable.
fn critic_checks(
    spec: &QaSpec,
    expect: &QaExpect,
    m: &Measurements,
    duration_ms: i64,
    has_video: bool,
    has_audio: bool,
) -> Vec<QaCheck> {
    let Some(binary) = default_critic_binary() else {
        // Not installed beside the engine: reported, not blocking; the measured checks
        // above still judge silence and black. A critic that runs and fails still blocks.
        return vec![QaCheck {
            id: "critic.run".into(),
            status: CheckStatus::NotApplicable,
            severity: Severity::Warning,
            message:
                "critic not run: no rightkit-render-critic binary found (install it beside the host executable)"
                    .into(),
            evidence: serde_json::json!({"critic": {"verdict": "unverified", "required": false}}),
        }];
    };
    let decoded = m.decode.0;
    let silence_measured = has_audio && decoded;
    let black_measured = has_video && decoded;
    let input = CriticInput {
        duration_ms: Some(duration_ms.max(0) as u64),
        silence: silence_measured.then(|| spans(&m.silence)),
        black: black_measured.then(|| spans(&m.black)),
        holds: Vec::new(),
        silence_holds: holds(&expect.allowed_silence),
        black_holds: holds(&expect.allowed_black),
        max_silence_ms: spec.max_silence_ms.max(0) as u64,
        max_black_ms: spec.max_black_ms.max(0) as u64,
    };
    let runner = CriticRunner::new(binary, CRITIC_TIMEOUT);
    let mut checks = match runner.run(&input) {
        Ok(findings) => checks_from_findings(&findings),
        Err(e) => return vec![critic_unavailable_check(&e.to_string())],
    };
    let not_applicable = |id: &str, why: &str, checks: &mut Vec<QaCheck>| {
        checks.retain(|c| c.id != id);
        checks.push(QaCheck {
            id: id.into(),
            status: CheckStatus::NotApplicable,
            severity: Severity::Warning,
            message: why.into(),
            evidence: json!({"critic": {"verdict": "not_applicable", "required": false, "reason": why}}),
        });
    };
    if !silence_measured {
        let why = if has_audio {
            UNDECODED
        } else {
            "no audio stream"
        };
        not_applicable(RULE_SILENCE, why, &mut checks);
    }
    if !black_measured {
        let why = if has_video {
            UNDECODED
        } else {
            "no video stream"
        };
        not_applicable(RULE_BLACK, why, &mut checks);
    }
    checks
}

fn unreadable_report(
    tools: &QaTools,
    render: &Path,
    spec: &QaSpec,
    expect: &QaExpect,
    blake3: String,
    size: u64,
    why: String,
) -> QaReport {
    let checks = vec![QaCheck::measured(
        "file.integrity",
        Severity::Error,
        false,
        "",
        format!("render cannot be read: {why}"),
        json!({"error": why}),
    )];
    assemble(tools, render, spec, expect, blake3, size, None, checks)
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    tools: &QaTools,
    render: &Path,
    spec: &QaSpec,
    expect: &QaExpect,
    blake3: String,
    size: u64,
    probed: Option<&Probe>,
    checks: Vec<QaCheck>,
) -> QaReport {
    let media = MediaFacts {
        width: probed.and_then(|p| p.video.as_ref()).map(|v| v.width),
        height: probed.and_then(|p| p.video.as_ref()).map(|v| v.height),
        duration_ms: probed.and_then(|p| ms(p.format_duration_s)),
        fps: probed.and_then(|p| p.video.as_ref()).and_then(|v| v.fps),
        video_codec: probed
            .and_then(|p| p.video.as_ref())
            .map(|v| v.codec.clone()),
        audio_codec: probed
            .and_then(|p| p.audio.as_ref())
            .map(|a| a.codec.clone()),
        has_video: probed.is_some_and(|p| p.video.is_some()),
        has_audio: probed.is_some_and(|p| p.audio.is_some()),
    };
    let status = if checks.iter().any(QaCheck::blocking) {
        OverallStatus::Fail
    } else {
        OverallStatus::Pass
    };
    QaReport {
        schema_version: REPORT_SCHEMA_VERSION,
        preset: spec.preset.clone(),
        status,
        render: RenderIdentity {
            file_name: render
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            blake3,
            size_bytes: size,
        },
        media,
        captions_blake3: expect
            .captions_srt
            .as_ref()
            .map(|s| blake3_bytes(s.as_bytes())),
        spec: spec.clone(),
        check_version: CHECK_VERSION,
        checks,
        ffmpeg_identity: tools.identity(),
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_qa::probe::{AudioStream, VideoStream};

    fn good() -> Measurements {
        Measurements {
            probe: Probe {
                format_duration_s: Some(10.0),
                video: Some(VideoStream {
                    codec: "h264".into(),
                    pix_fmt: Some("yuv420p".into()),
                    width: 1920,
                    height: 1080,
                    fps: Some(30.0),
                    start_s: Some(0.0),
                    duration_s: Some(10.0),
                }),
                audio: Some(AudioStream {
                    codec: "aac".into(),
                    channels: 2,
                    sample_rate: Some(48000),
                    start_s: Some(0.0),
                    duration_s: Some(10.02),
                }),
            },
            decode: (true, String::new()),
            black: vec![],
            freeze: vec![],
            silence: vec![],
            loudness: Some(probe::Ebur128 {
                integrated_lufs: Some(-14.1),
                true_peak_dbtp: Some(-1.6),
            }),
            clip: Some(probe::ClipCounter::new(2, 0.999, 3)),
        }
    }

    fn expect() -> QaExpect {
        QaExpect {
            duration_ms: Some(10_000),
            fps: Some(rightkit_timeline::Fps { num: 30, den: 1 }),
            ..QaExpect::default()
        }
    }

    fn failing(checks: &[QaCheck]) -> Vec<&str> {
        checks
            .iter()
            .filter(|c| c.blocking())
            .map(|c| c.id.as_str())
            .collect()
    }

    #[test]
    fn a_good_render_passes_every_check() {
        let spec = QaSpec::preset("youtube").unwrap();
        let checks = judge(&spec, &expect(), &good());
        assert!(failing(&checks).is_empty(), "{checks:#?}");
    }

    #[test]
    fn each_defect_trips_its_own_check() {
        let spec = QaSpec::preset("youtube").unwrap();
        let mut m = good();
        m.black = vec![Interval {
            start_ms: 3000,
            end_ms: 4500,
        }];
        m.freeze = vec![Interval {
            start_ms: 5000,
            end_ms: 9000,
        }];
        m.silence = vec![Interval {
            start_ms: 1000,
            end_ms: 5000,
        }];
        m.loudness = Some(probe::Ebur128 {
            integrated_lufs: Some(-24.0),
            true_peak_dbtp: Some(0.3),
        });
        m.probe.video.as_mut().unwrap().width = 1280;
        m.probe.video.as_mut().unwrap().codec = "mpeg4".into();
        m.probe.audio.as_mut().unwrap().duration_s = Some(10.5);
        let mut e = expect();
        e.duration_ms = Some(12_000);
        let checks = judge(&spec, &e, &m);
        let f = failing(&checks);
        // file.integrity and the detector checks under an unclean decode are covered by
        // `unclean_decode_makes_detector_checks_not_applicable`.
        for id in [
            "media.duration",
            "video.resolution",
            "video.codec",
            "video.black_frames",
            "video.frozen_frames",
            "audio.silence",
            "audio.loudness",
            "audio.true_peak",
            "av.sync",
        ] {
            assert!(f.contains(&id), "{id} should fail: {f:?}");
        }
    }

    #[test]
    fn expected_black_and_silence_are_not_defects() {
        let spec = QaSpec::preset("youtube").unwrap();
        let mut m = good();
        m.black = vec![Interval {
            start_ms: 0,
            end_ms: 1500,
        }];
        m.silence = vec![Interval {
            start_ms: 9000,
            end_ms: 10_000,
        }];
        let mut e = expect();
        e.allowed_black = vec![(0, 1500)];
        e.allowed_silence = vec![(8500, 10_000)];
        assert!(failing(&judge(&spec, &e, &m)).is_empty());
    }

    #[test]
    fn clipped_runs_fail() {
        let spec = QaSpec::preset("youtube").unwrap();
        let mut m = good();
        let mut c = probe::ClipCounter::new(1, 0.999, 3);
        c.feed(
            &[1.0f32; 5]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        m.clip = Some(c);
        assert_eq!(
            failing(&judge(&spec, &expect(), &m)),
            vec!["audio.clipping"]
        );
    }

    #[test]
    fn caption_rules() {
        let spec = QaSpec::preset("reels").unwrap();
        let mut e = QaExpect {
            duration_ms: Some(10_000),
            fps: None,
            ..QaExpect::default()
        };
        e.captions_srt = Some(
            "1\n00:00:00,000 --> 00:00:02,000\nhello there\n\n2\n00:00:01,500 --> 00:00:03,000\nthis line is far too long for the safe zone\n\n3\n00:00:09,000 --> 00:00:12,000\nafter the end\n"
                .into(),
        );
        let m = good();
        let checks = judge(&spec, &e, &m);
        let c = checks.iter().find(|c| c.id == "captions.sanity").unwrap();
        assert!(c.blocking());
        assert!(c.message.contains("overlaps earlier cue"), "{}", c.message);
        assert!(c.message.contains("overflows safe zone"), "{}", c.message);
        assert!(c.message.contains("after render"), "{}", c.message);

        e.captions_srt = Some("1\n00:00:00,000 --> 00:00:03,000\nfine\n\n2\n00:00:04,000 --> 00:00:09,500\nalso fine\n".into());
        let checks = judge(&spec, &e, &m);
        assert!(!checks
            .iter()
            .find(|c| c.id == "captions.sanity")
            .unwrap()
            .blocking());
    }

    #[test]
    fn missing_streams_fail_dependent_checks() {
        let spec = QaSpec::preset("youtube").unwrap();
        let mut m = good();
        m.probe.audio = None;
        m.loudness = None;
        m.clip = None;
        let f = failing(&judge(&spec, &expect(), &m)).join(",");
        assert!(
            f.contains("media.streams") && f.contains("audio.loudness"),
            "{f}"
        );
    }

    // Unit test: a decode error needs a corrupt input, which no e2e render produces on demand.
    #[test]
    fn unclean_decode_makes_detector_checks_not_applicable() {
        let spec = QaSpec::preset("youtube").unwrap();
        let mut m = good();
        m.decode = (false, "error".into());
        let checks = judge(&spec, &expect(), &m);
        for id in ["video.black_frames", "video.frozen_frames", "audio.silence"] {
            let c = checks.iter().find(|c| c.id == id).unwrap();
            assert_eq!(c.status, CheckStatus::NotApplicable, "{c:?}");
            assert!(!c.blocking(), "{c:?}");
        }
        let integrity = checks.iter().find(|c| c.id == "file.integrity").unwrap();
        assert!(integrity.blocking(), "{integrity:?}");
    }

    // Unit test: both comparisons being absent needs a probe with no timestamps, which no e2e render gives.
    #[test]
    fn av_sync_without_any_comparison_is_not_applicable() {
        let spec = QaSpec::preset("youtube").unwrap();
        let mut m = good();
        let v = m.probe.video.as_mut().unwrap();
        v.start_s = None;
        v.duration_s = None;
        let a = m.probe.audio.as_mut().unwrap();
        a.start_s = None;
        a.duration_s = None;
        let checks = judge(&spec, &expect(), &m);
        let c = checks.iter().find(|c| c.id == "av.sync").unwrap();
        assert_eq!(c.status, CheckStatus::NotApplicable, "{c:?}");
        assert!(!c.blocking(), "{c:?}");
    }

    // Unit test: a freeze still open at end of stream needs a stream that stays frozen to its last
    // frame, which no e2e render produces on demand.
    #[test]
    fn open_freeze_at_end_of_stream_is_judged_by_its_length() {
        let spec = QaSpec::preset("youtube").unwrap();
        let mut m = good();
        // The container reports no duration; the video stream reports 10 s.
        let mut p = m.probe.clone();
        p.format_duration_s = None;
        m.freeze =
            probe::parse_freezedetect("lavfi.freezedetect.freeze_start: 2\n", detector_end_ms(&p));
        assert_eq!(
            m.freeze,
            vec![Interval {
                start_ms: 2000,
                end_ms: 10_000
            }]
        );
        let checks = judge(&spec, &expect(), &m);
        assert!(
            failing(&checks).contains(&"video.frozen_frames"),
            "{checks:#?}"
        );
    }
}
