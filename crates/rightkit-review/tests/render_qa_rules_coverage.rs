//! Coverage for matrix rules 55, 56, 71, 73 and 74 (docs/editing-rules-matrix.md).
//!
//! Unit-level on purpose, through the public `run::judge` seam. `judge` is the pure
//! judgement over probe measurements and starts no process. Each rule needs a render
//! with a specific frame rate, pixel format or audio stream, or a caption file with a
//! specific reading speed or tail. Reaching them through `run_qa` would mean rendering
//! that media for each case, so the judgement is tested directly. Rules 55 and 56 are
//! advisory captions checks with no other public entry point. Rules 73 and 74 are
//! Warning-severity; the assertions follow the code, not the matrix's "refuse" label.

use rightkit_review::render_qa::probe::{AudioStream, ClipCounter, Ebur128, Probe, VideoStream};
use rightkit_review::render_qa::report::{CheckStatus, QaCheck, Severity};
use rightkit_review::render_qa::run::{judge, Measurements};
use rightkit_review::render_qa::spec::{QaExpect, QaSpec};
use rightkit_timeline::Fps;

/// A render that passes every check on the youtube preset (same fixture as run.rs tests).
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
        loudness: Some(Ebur128 {
            integrated_lufs: Some(-14.1),
            true_peak_dbtp: Some(-1.6),
        }),
        clip: Some(ClipCounter::new(2, 0.999, 3)),
    }
}

fn timeline_expect(captions: Option<String>) -> QaExpect {
    QaExpect {
        duration_ms: Some(10_000),
        fps: Some(Fps::new(30, 1).unwrap()),
        captions_srt: captions,
        ..QaExpect::default()
    }
}

fn find<'a>(checks: &'a [QaCheck], id: &str) -> &'a QaCheck {
    checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no check {id}: {checks:#?}"))
}

fn srt_time(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1_000) % 60,
        ms % 1_000
    )
}

fn srt_cue(index: usize, start: u64, end: u64, lines: &[String]) -> String {
    format!(
        "{index}\n{} --> {}\n{}\n\n",
        srt_time(start),
        srt_time(end),
        lines.join("\n")
    )
}

// Rule 55: reading speed above max_chars_per_second is a Warning, never blocking.
// Judged here because `captions.reading_speed` is only produced by `caption_checks`
// inside `judge`; the test feeds it a dense cue and a sparse one.

#[test]
fn rule_55_reading_speed_warns_without_blocking() {
    let spec = QaSpec::preset("youtube").unwrap();
    let max_cps = spec.captions.max_chars_per_second;
    let line_len = spec.captions.max_chars_per_line;
    let lines = spec.captions.max_lines;

    // A full cue of max_lines lines, line_len characters each, shown for one second.
    let full: Vec<String> = (0..lines).map(|_| "a".repeat(line_len)).collect();
    let dense_chars = (lines * line_len) as f64;
    assert!(
        dense_chars > max_cps,
        "fixture must exceed the reading speed"
    );
    let fast = srt_cue(1, 0, 1_000, &full);

    let checks = judge(&spec, &timeline_expect(Some(fast)), &good());
    let c = find(&checks, "captions.reading_speed");
    assert_eq!(c.status, CheckStatus::Fail);
    assert_eq!(c.severity, Severity::Warning);
    assert!(!c.blocking(), "reading speed is advisory: {}", c.message);

    // The same text over nine seconds is comfortable and passes.
    let slow = srt_cue(1, 0, 9_000, &["hi".to_string()]);
    let checks = judge(&spec, &timeline_expect(Some(slow)), &good());
    assert_eq!(
        find(&checks, "captions.reading_speed").status,
        CheckStatus::Pass
    );
}

// Rule 56: captions must reach within max_tail_gap_ms of the render end. Warning only.
// Judged here because `captions.coverage` is produced only inside `caption_checks`.

#[test]
fn rule_56_tail_gap_warns_without_blocking() {
    let spec = QaSpec::preset("youtube").unwrap();
    let gap = spec.captions.max_tail_gap_ms;
    let lag_end = 10_000 - gap - 1_000;
    assert!(lag_end > 0, "fixture must leave a cue inside the render");

    let early = srt_cue(1, 0, lag_end as u64, &["early".to_string()]);
    let checks = judge(&spec, &timeline_expect(Some(early)), &good());
    let c = find(&checks, "captions.coverage");
    assert_eq!(c.status, CheckStatus::Fail);
    assert_eq!(c.severity, Severity::Warning);
    assert!(!c.blocking(), "tail gap is advisory: {}", c.message);

    let to_end = srt_cue(1, 0, 10_000, &["to the end".to_string()]);
    let checks = judge(&spec, &timeline_expect(Some(to_end)), &good());
    assert_eq!(find(&checks, "captions.coverage").status, CheckStatus::Pass);
}

// Rule 71: video.frame_rate must match the timeline and is an Error that blocks.
// Judged here: a 25 fps render against a 30 fps timeline needs a real re-encode to
// reach through run_qa.

#[test]
fn rule_71_frame_rate_mismatch_blocks() {
    let spec = QaSpec::preset("youtube").unwrap();
    let expect = timeline_expect(None);

    let checks = judge(&spec, &expect, &good());
    assert_eq!(find(&checks, "video.frame_rate").status, CheckStatus::Pass);

    let mut wrong = good();
    wrong.probe.video.as_mut().unwrap().fps = Some(25.0);
    let checks = judge(&spec, &expect, &wrong);
    let c = find(&checks, "video.frame_rate");
    assert_eq!(c.status, CheckStatus::Fail);
    assert_eq!(c.severity, Severity::Error);
    assert!(c.blocking(), "{}", c.message);
}

// Rule 73: video.pixel_format is a Warning (matrix says refuse; the code reports it).
// Judged here: a non-platform pixel format is a stream property no fixture clip
// reaches without a dedicated encode.

#[test]
fn rule_73_pixel_format_warns_without_blocking() {
    let spec = QaSpec::preset("youtube").unwrap();
    let expect = timeline_expect(None);

    let mut m = good();
    m.probe.video.as_mut().unwrap().pix_fmt = Some("not-a-format".into());
    let checks = judge(&spec, &expect, &m);
    let c = find(&checks, "video.pixel_format");
    assert_eq!(c.status, CheckStatus::Fail);
    assert_eq!(c.severity, Severity::Warning);
    assert!(!c.blocking(), "{}", c.message);

    let mut unknown = good();
    unknown.probe.video.as_mut().unwrap().pix_fmt = None;
    let checks = judge(&spec, &expect, &unknown);
    assert_eq!(
        find(&checks, "video.pixel_format").status,
        CheckStatus::Fail,
        "an unreadable pixel format is not platform-safe"
    );

    let checks = judge(&spec, &expect, &good());
    assert_eq!(
        find(&checks, "video.pixel_format").status,
        CheckStatus::Pass
    );
}

// Rule 74: audio.codec is required when audio is required. A missing stream is an
// Error that blocks; an unaccepted codec is a Warning. Judged here: the stream
// presence and codec are probe fields no shipped fixture varies.

#[test]
fn rule_74_missing_required_audio_blocks_and_codec_warns() {
    let mut spec = QaSpec::preset("youtube").unwrap();
    spec.require_audio = true;
    let expect = timeline_expect(None);

    let mut silent = good();
    silent.probe.audio = None;
    let checks = judge(&spec, &expect, &silent);
    let c = find(&checks, "audio.codec");
    assert_eq!(c.status, CheckStatus::Fail);
    assert_eq!(c.severity, Severity::Error);
    assert!(c.blocking(), "{}", c.message);

    let mut odd = good();
    odd.probe.audio.as_mut().unwrap().codec = "not-a-codec".into();
    let checks = judge(&spec, &expect, &odd);
    let c = find(&checks, "audio.codec");
    assert_eq!(c.status, CheckStatus::Fail);
    assert_eq!(c.severity, Severity::Warning);
    assert!(!c.blocking(), "{}", c.message);

    // When audio is not required, its absence is not applicable, not a failure.
    spec.require_audio = false;
    let mut none = good();
    none.probe.audio = None;
    let checks = judge(&spec, &expect, &none);
    assert_eq!(
        find(&checks, "audio.codec").status,
        CheckStatus::NotApplicable
    );
}
