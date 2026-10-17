//! Public API adoption, persisted CutRight report compatibility & legacy isolation.

mod support;

use rightkit_review::{
    delivery::{
        self, CaptionCue, CaptionRule, DecodeIntegrity, DurationRule, FormatRule, FrameRateRule,
        StreamSyncRule, StreamTiming,
    },
    disposition,
    qa::{
        self, AllowedWindow, Check, CheckStatus, Interval, OverallStatus, Severity, VerdictRecord,
    },
    Disposition, Finding, Verdict,
};
use serde_json::{json, Value};

type Report = VerdictRecord<Value, Value, Value>;

fn report(checks: Vec<Check>) -> Report {
    Report {
        schema_version: qa::REPORT_SCHEMA_VERSION,
        preset: "youtube".into(),
        status: qa::overall_status(&checks),
        render: json!({"blake3": "render-hash", "size_bytes": 32, "file_name": "render.mp4"}),
        media: json!({"width": 1920, "height": 1080}),
        captions_blake3: Some("srt-hash".into()),
        spec: json!({"duration_tolerance_ms": 100}),
        check_version: 4,
        checks,
        ffmpeg_identity: "ffmpeg version fixture".into(),
        generated_at: "2026-10-17T00:00:00Z".into(),
    }
}

#[test]
fn delivery_checks_persist_with_warning_failures_without_blocking() {
    let decode = DecodeIntegrity {
        completed: true,
        success: true,
        errors: String::new(),
    };
    let time = StreamTiming {
        start_s: Some(0.0),
        duration_s: Some(10.0),
    };
    let mut checks = vec![
        delivery::decode_integrity(Some(&decode)),
        delivery::required_streams(true, true, true),
        DurationRule {
            expected_ms: Some(10_000),
            tolerance_ms: 100,
        }
        .check(Some(10_000))
        .unwrap(),
        FrameRateRule {
            expected_fps: Some(30.0),
            absolute_tolerance: 0.01,
            relative_tolerance: 0.0005,
        }
        .check(Some(30.0))
        .unwrap(),
        FormatRule {
            allowed: vec!["h264".into()],
        }
        .check("video.codec", Severity::Error, Some("h264"))
        .unwrap(),
        FormatRule {
            allowed: vec!["yuv420p".into()],
        }
        .check("video.pixel_format", Severity::Warning, Some("yuv444p"))
        .unwrap(),
        StreamSyncRule {
            max_start_offset_ms: 100,
            max_duration_drift_ms: 100,
        }
        .check(Some(&time), Some(&time)),
    ];
    checks.extend(
        CaptionRule {
            required: true,
            max_lines: 2,
            max_chars_per_line: 40,
            max_chars_per_second: 5.0,
            max_tail_gap_ms: 1000,
            duration_tolerance_ms: 100,
        }
        .check(
            Some(&[CaptionCue {
                start_ms: 0,
                end_ms: 1000,
                lines: vec!["Fast captions".into()],
            }]),
            Some(10_000),
        )
        .unwrap(),
    );
    let report = report(checks);
    assert!(report.passed());
    assert!(report.blocking_ids().is_empty());
    assert_eq!(
        report.check("video.pixel_format").unwrap().status,
        CheckStatus::Fail
    );
    assert_eq!(
        Report::from_json(&report.to_json().unwrap()).unwrap(),
        report
    );
    let failed = report.with_check(
        DurationRule {
            expected_ms: Some(10_000),
            tolerance_ms: 100,
        }
        .check(Some(9000))
        .unwrap(),
    );
    assert!(!failed.passed());
    assert_eq!(failed.blocking_ids(), vec!["media.duration"]);
    assert_eq!(
        failed
            .checks
            .iter()
            .filter(|c| c.id == "media.duration")
            .count(),
        1
    );
}

#[test]
fn legacy_cutright_shape_round_trips_without_new_fields() {
    let original = json!({"schema_version":1,"preset":"youtube","status":"pass",
        "render":{"file_name":"render.mp4","blake3":"hash","size_bytes":42},
        "media":{"duration_ms":10000},"captions_blake3":null,"spec":{"preset":"youtube"},
        "check_version":3,"checks":[{"id":"video.pixel_format","status":"fail",
            "severity":"warning","message":"unsafe format","evidence":{"pix_fmt":"yuv444p"}}],
        "ffmpeg_identity":"ffmpeg version fixture","generated_at":"2026-10-17T00:00:00Z"});
    let parsed = Report::from_json(&original.to_string()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), original);
    let mut legacy = original.clone();
    legacy.as_object_mut().unwrap().remove("check_version");
    assert_eq!(
        Report::from_json(&legacy.to_string())
            .unwrap()
            .check_version,
        1
    );
    for field in ["status", "schema_version"] {
        let mut bad = original.clone();
        bad[field] = if field == "status" {
            json!("fail")
        } else {
            json!(2)
        };
        assert!(Report::from_json(&bad.to_string()).is_err());
    }
    let mut bad = parsed;
    bad.checks.push(bad.checks[0].clone());
    assert!(bad.to_json().is_err());
}

#[test]
fn allowed_windows_do_not_merge_or_exempt_other_checks() {
    let windows = vec![
        AllowedWindow {
            check_id: "audio.silence".into(),
            interval: Interval {
                start_ms: 0,
                end_ms: 1000,
            },
            slack_ms: 0,
            reason: "intro".into(),
        },
        AllowedWindow {
            check_id: "audio.silence".into(),
            interval: Interval {
                start_ms: 1000,
                end_ms: 2000,
            },
            slack_ms: 0,
            reason: "outro".into(),
        },
    ];
    let spanning = Interval {
        start_ms: 0,
        end_ms: 2000,
    };
    assert_eq!(
        qa::unexpected_intervals("audio.silence", &[spanning], &windows).unwrap(),
        vec![spanning]
    );
    let covered = windows[0].interval;
    assert!(
        qa::unexpected_intervals("audio.silence", &[covered], &windows)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        qa::unexpected_intervals("video.black_frames", &[covered], &windows).unwrap(),
        vec![covered]
    );
    let invalid = AllowedWindow {
        reason: String::new(),
        ..windows[0].clone()
    };
    assert!(qa::unexpected_intervals("audio.silence", &[], &[invalid]).is_err());
}

#[test]
fn old_content_pipeline_failures_still_block_regardless_of_required_flag() {
    assert_eq!(
        disposition(&[Finding {
            rule_id: "legacy".into(),
            required: false,
            verdict: Verdict::Failed {
                reason: "defect".into()
            }
        }]),
        Disposition::Block
    );
    assert_eq!(
        disposition(
            &rightkit_review::RuleSet::standard()
                .evaluate(&support::clean_video())
                .unwrap()
        ),
        Disposition::Pass
    );
    assert_eq!(report(vec![]).status, OverallStatus::Pass);
}
