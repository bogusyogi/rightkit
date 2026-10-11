//! End-to-end checks for the stdin runner, ffprobe JSON, and loudness acceptance against a
//! real FFmpeg. Each test resolves the binary through the same gate `media_e2e.rs` uses
//! (`dev_resolve::resolve_for_development`) and returns early after a SKIP line when the host
//! has no FFmpeg (a panic under `RIGHTKIT_REQUIRE_ASSETS=1`). The ffprobe test also needs an
//! `ffprobe` beside that binary.

mod common;

use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use rightkit_media::{
    dev_resolve::{resolve_for_development, DevResolved},
    loudnorm::{measure_loudness_verified, LoudnessAcceptance, LoudnessError, LoudnessTarget},
    probe::run_ffprobe_json,
    quickprobe,
    runner::{run_checked, run_with_stdin, RunOptions},
    CancellationToken, DevFfmpeg, FfmpegBinary, FfmpegCommand,
};

const T: Duration = Duration::from_secs(120);

/// The development FFmpeg, or `None` after a SKIP line.
fn host_ffmpeg() -> Option<DevResolved> {
    match resolve_for_development() {
        Some((bin, _)) => Some(bin),
        None => {
            common::skip_missing("FFmpeg", &common::ffmpeg_looked_at());
            None
        }
    }
}

/// `ffmpeg` writes a 440 Hz tone at amplitude 0.5 into `out` (mono 16 kHz WAV, 0.5 s).
fn sine_wav(bin: &dyn FfmpegBinary, out: &Path) {
    let mut cmd = FfmpegCommand::base("error");
    cmd.texts([
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=0.5*sin(2*PI*440*t):s=16000:d=0.5",
        "-ac",
        "1",
    ])
    .path(out);
    run_checked(bin, &cmd, T, &CancellationToken::new()).expect("sine fixture");
}

#[test]
fn piped_pcm_reaches_ffmpeg_and_encodes_the_expected_duration() {
    let Some(bin) = host_ffmpeg() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let out: PathBuf = dir.path().join("piped.wav");
    // One second of 16-bit mono 16 kHz samples: a ramp that is never silent.
    let pcm: Vec<u8> = (0..16_000i32)
        .flat_map(|i| ((i % 200 - 100) as i16 * 200).to_le_bytes())
        .collect();

    let mut cmd = FfmpegCommand::base("error");
    cmd.stdin_piped(true)
        .texts([
            "-f",
            "s16le",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-i",
            "pipe:0",
            "-c:a",
            "pcm_s16le",
        ])
        .path(&out);
    let token = CancellationToken::new();
    run_with_stdin(&bin, &cmd, RunOptions::new(T, token), |mut stdin| {
        stdin.write_all(&pcm)
    })
    .expect("piped encode");

    let duration = quickprobe::probe_path(&out)
        .expect("written WAV")
        .duration_s
        .expect("WAV duration");
    assert!((duration - 1.0).abs() < 0.01, "duration {duration}");
}

#[test]
fn ffprobe_json_of_a_generated_wav_reports_its_audio_stream() {
    let Some(bin) = host_ffmpeg() else {
        return;
    };
    let ffprobe_path = bin.ffmpeg_path().with_file_name("ffprobe");
    if !ffprobe_path.is_file() {
        common::skip_missing(
            "ffprobe beside the FFmpeg binary",
            &ffprobe_path.display().to_string(),
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    sine_wav(&bin, &wav);

    let ffprobe = DevFfmpeg::unverified(ffprobe_path);
    let probe =
        run_ffprobe_json(&ffprobe, &wav, Some(T), &CancellationToken::new()).expect("ffprobe JSON");
    let audio = probe.first_audio().expect("audio stream");
    assert_eq!(audio.codec_name.as_deref(), Some("pcm_s16le"));
    assert_eq!(audio.sample_rate, Some(16_000));
    assert_eq!(audio.channels, Some(1));
    assert!(probe.first_video().is_none());
    let duration = probe.format.duration_s.expect("format duration");
    assert!((duration - 0.5).abs() < 0.05, "duration {duration}");
    assert!(probe
        .format
        .format_name
        .as_deref()
        .is_some_and(|n| n.contains("wav")));
}

#[test]
fn acceptance_rejects_a_level_far_from_target_and_accepts_it_when_widened() {
    let Some(bin) = host_ffmpeg() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    sine_wav(&bin, &wav);
    let token = CancellationToken::new();
    let target = LoudnessTarget::SPEECH_EXPORT;

    // The 0.5-amplitude tone sits around -9 LUFS, well away from the -16 LUFS target.
    let strict = LoudnessAcceptance::default();
    let err = measure_loudness_verified(&bin, &wav, &target, &strict, T, &token)
        .expect_err("must be out of tolerance");
    assert!(matches!(err, LoudnessError::OutOfTolerance { .. }), "{err}");

    let wide = LoudnessAcceptance {
        lufs_tolerance_lu: 100.0,
        true_peak_max_dbtp: None,
    };
    let measured = measure_loudness_verified(&bin, &wav, &target, &wide, T, &token)
        .expect("widened tolerance accepts");
    assert!(measured.input_i.is_finite() && measured.input_tp.is_finite());

    let ceiling = LoudnessAcceptance {
        lufs_tolerance_lu: 100.0,
        true_peak_max_dbtp: Some(-100.0),
    };
    assert!(matches!(
        measure_loudness_verified(&bin, &wav, &target, &ceiling, T, &token),
        Err(LoudnessError::TruePeakExceeded { .. })
    ));
}
