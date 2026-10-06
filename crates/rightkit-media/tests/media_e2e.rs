//! End-to-end media tests against a real FFmpeg process and real files.
//! FFmpeg is resolved through the catalog path first; a host FFmpeg that is not the
//! approved artifact runs as `DevUnverified` and the test says so on stderr.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use rightkit_media::{
    dev_resolve::{resolve_for_development, DevResolved, ResolutionKind},
    loudnorm::{measure_loudness, normalize_file, LoudnessTarget},
    probe::probe_media,
    quickprobe, run_checked, run_with_progress, transcode_audio_command, AudioTranscode,
    CancellationToken, FfmpegBinary, FfmpegCommand, RunError, RunOptions,
};

const T: Duration = Duration::from_secs(120);

fn ffmpeg() -> DevResolved {
    let (bin, kind) = resolve_for_development().expect("an FFmpeg must exist on this host");
    match &kind {
        ResolutionKind::CatalogVerified { artifact_id } => {
            eprintln!("ffmpeg: catalog-verified {artifact_id}")
        }
        ResolutionKind::DevUnverified { path, reason } => {
            eprintln!("ffmpeg: DEV-UNVERIFIED {} ({reason})", path.display())
        }
    }
    bin
}

fn run(bin: &dyn FfmpegBinary, args: &[&str]) {
    let mut c = FfmpegCommand::base("error");
    c.texts(args.iter().copied());
    run_checked(bin, &c, T, &CancellationToken::new()).expect("fixture generation");
}

fn p(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

/// A real stereo 48 kHz WAV of pink noise at a known low level.
fn noise_wav(bin: &dyn FfmpegBinary, path: &Path, seconds: u32, amplitude: &str) {
    run(
        bin,
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("anoisesrc=color=pink:amplitude={amplitude}:sample_rate=48000:seed=3:duration={seconds}"),
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
            path.to_str().unwrap(),
        ],
    );
}

#[test]
fn probe_reports_streams_duration_and_rotation_and_matches_pure_rust_probe() {
    let bin = ffmpeg();
    let dir = tempfile::tempdir().unwrap();

    // Audio-only WAV: 3 s stereo 44.1 kHz.
    let wav = p(dir.path(), "tone.wav");
    run(
        &bin,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=3",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
            wav.to_str().unwrap(),
        ],
    );
    let pr = probe_media(&bin, &wav, &CancellationToken::new()).unwrap();
    assert!(pr.video.is_empty());
    let a = pr.first_audio().unwrap();
    assert_eq!(
        (a.sample_rate, a.channels, a.codec.as_str()),
        (44_100, 2, "pcm_s16le")
    );
    assert_eq!(pr.duration_ms, Some(3000));
    // Pure-Rust probe agrees exactly on duration.
    let q = quickprobe::probe_path(&wav).unwrap();
    assert!((q.duration_s.unwrap() - 3.0).abs() < 0.001);

    // Video + audio MP4: 320x240 @ 25 fps, 2 s.
    let mp4 = p(dir.path(), "clip.mp4");
    run(
        &bin,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=48000:duration=2",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-shortest",
            mp4.to_str().unwrap(),
        ],
    );
    let pr = probe_media(&bin, &mp4, &CancellationToken::new()).unwrap();
    let v = pr.first_video().unwrap();
    assert_eq!((v.width, v.height, v.rotation), (320, 240, 0));
    assert_eq!(v.fps, Some(25.0));
    assert_eq!(pr.first_audio().unwrap().sample_rate, 48_000);
    assert!(pr.format_names.iter().any(|n| n == "mp4"));
    let q = quickprobe::probe_path(&mp4).unwrap();
    assert_eq!((q.width, q.height), (Some(320), Some(240)));
    let qd = q.duration_s.unwrap();
    assert!(
        (qd - pr.duration_ms.unwrap() as f64 / 1000.0).abs() < 0.1,
        "{qd}"
    );

    // Display rotation swaps display size (iPhone portrait).
    let rot = p(dir.path(), "rot.mp4");
    let mut c = FfmpegCommand::base("error");
    c.texts(["-display_rotation", "90"])
        .input(&mp4)
        .texts(["-c", "copy"])
        .path(&rot);
    run_checked(&bin, &c, T, &CancellationToken::new()).unwrap();
    let pr = probe_media(&bin, &rot, &CancellationToken::new()).unwrap();
    let v = pr.first_video().unwrap();
    assert_eq!(v.rotation, 90, "rotation parsed from displaymatrix");
    assert_eq!(v.display_size(), (240, 320));

    // Garbage input is a typed error, not a panic.
    let junk = p(dir.path(), "junk.bin");
    std::fs::write(&junk, b"definitely not media").unwrap();
    assert!(probe_media(&bin, &junk, &CancellationToken::new()).is_err());
    assert!(probe_media(
        &bin,
        &p(dir.path(), "missing.wav"),
        &CancellationToken::new()
    )
    .is_err());
}

#[test]
fn transcode_reports_progress_and_produces_the_requested_format() {
    let bin = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let src = p(dir.path(), "src.wav");
    noise_wav(&bin, &src, 30, "0.1");
    let out = p(dir.path(), "out16k.wav");
    // A moderately heavy filter so progress ticks arrive before completion.
    let spec = AudioTranscode {
        filter: Some("afftdn=nr=12".into()),
        ..AudioTranscode::pcm_s16le(16_000, 1)
    };
    let cmd = transcode_audio_command(&src, &out, &spec);
    let seen = Arc::new(AtomicUsize::new(0));
    let last_ms = Arc::new(AtomicUsize::new(0));
    let (s2, l2) = (Arc::clone(&seen), Arc::clone(&last_ms));
    let mut cb = move |ev: &rightkit_media::ProgressEvent| {
        s2.fetch_add(1, Ordering::SeqCst);
        l2.store(ev.out_time_ms as usize, Ordering::SeqCst);
    };
    let mut opts = RunOptions::new(T, CancellationToken::new());
    opts.on_progress = Some(&mut cb);
    let report = run_with_progress(&bin, &cmd, opts).unwrap();
    assert!(seen.load(Ordering::SeqCst) >= 1, "at least the end event");
    assert!(report.progress_events >= 1);
    // The reported final position matches the real output duration (30 s).
    assert!(
        (report.out_time_ms - 30_000).abs() < 100,
        "{}",
        report.out_time_ms
    );
    let pr = probe_media(&bin, &out, &CancellationToken::new()).unwrap();
    let a = pr.first_audio().unwrap();
    assert_eq!(
        (a.sample_rate, a.channels, a.codec.as_str()),
        (16_000, 1, "pcm_s16le")
    );
    assert_eq!(pr.duration_ms, Some(30_000));

    // A bad filter fails with FFmpeg's own message in the bounded tail.
    let bad = AudioTranscode {
        filter: Some("nosuchfilter=1".into()),
        ..AudioTranscode::pcm_s16le(16_000, 1)
    };
    let err = run_with_progress(
        &bin,
        &transcode_audio_command(&src, &p(dir.path(), "bad.wav"), &bad),
        RunOptions::new(T, CancellationToken::new()),
    )
    .unwrap_err();
    match err {
        RunError::Failed { stderr_tail, .. } => {
            assert!(
                stderr_tail.to_lowercase().contains("nosuchfilter"),
                "{stderr_tail}"
            )
        }
        other => panic!("expected Failed, got {other}"),
    }
}

fn process_alive_with_arg(needle: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", needle])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

#[test]
fn cancel_mid_run_stops_ffmpeg_promptly_and_reaps_the_process() {
    let bin = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let src = p(dir.path(), "long.wav");
    noise_wav(&bin, &src, 600, "0.1");
    let out = p(dir.path(), "cancel_victim_out.wav");
    // anlmdn runs far slower than real time, so the job cannot finish before cancel.
    let spec = AudioTranscode {
        filter: Some("anlmdn=s=0.0001".into()),
        ..AudioTranscode::pcm_s16le(48_000, 2)
    };
    let cmd = transcode_audio_command(&src, &out, &spec);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let started = Instant::now();
    let mut cb = move |_: &rightkit_media::ProgressEvent| c2.cancel();
    let mut opts = RunOptions::new(Duration::from_secs(300), cancel);
    opts.on_progress = Some(&mut cb);
    let res = run_with_progress(&bin, &cmd, opts);
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    thread::sleep(Duration::from_millis(300));
    assert!(
        !process_alive_with_arg("cancel_victim_out.wav"),
        "ffmpeg child must be gone after cancel"
    );

    // Cancel before spawn never starts a process.
    let pre = CancellationToken::new();
    pre.cancel();
    let r = run_with_progress(&bin, &cmd, RunOptions::new(T, pre));
    assert!(matches!(r, Err(RunError::Cancelled)));

    // Timeout kills as well.
    let r = run_with_progress(
        &bin,
        &cmd,
        RunOptions::new(Duration::from_millis(1500), CancellationToken::new()),
    );
    assert!(matches!(r, Err(RunError::TimedOut(_))), "{r:?}");
    thread::sleep(Duration::from_millis(300));
    assert!(!process_alive_with_arg("cancel_victim_out.wav"));
}

/// Independent integrated-loudness measurement with `ebur128` (not `loudnorm`).
fn ebur128_integrated(bin: &dyn FfmpegBinary, file: &Path) -> f64 {
    let mut c = FfmpegCommand::base("info");
    c.input(file)
        .texts(["-af", "ebur128=peak=true", "-f", "null", "-"]);
    let out = run_checked(bin, &c, T, &CancellationToken::new()).unwrap();
    out.stderr
        .lines()
        .rev()
        .find_map(|l| {
            l.trim()
                .strip_prefix("I:")
                .and_then(|r| r.split_whitespace().next())
                .and_then(|t| t.parse().ok())
        })
        .expect("ebur128 summary")
}

#[test]
fn loudnorm_two_pass_lands_within_one_lu_of_target_confirmed_by_ebur128() {
    let bin = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let quiet = p(dir.path(), "quiet.wav");
    noise_wav(&bin, &quiet, 20, "0.02");
    let target = LoudnessTarget {
        integrated_lufs: -16.0,
        true_peak_db: -1.5,
        lra: 20.0,
    };
    let before = measure_loudness(&bin, &quiet, &target, T, &CancellationToken::new()).unwrap();
    assert!(
        before.input_i < -25.0,
        "fixture must start quiet: {}",
        before.input_i
    );

    let out = p(dir.path(), "norm.wav");
    let (report, run) = normalize_file(
        &bin,
        &quiet,
        &out,
        &target,
        "pcm_s16le",
        48_000,
        T,
        &CancellationToken::new(),
        None,
    )
    .unwrap();
    assert!(report.within_tolerance, "{report:?}");
    assert!(run.out_time_ms > 19_000);
    let independent = ebur128_integrated(&bin, &out);
    assert!(
        (independent - -16.0).abs() <= 1.0,
        "ebur128 says {independent} LUFS"
    );
    // Rate/channels preserved after loudnorm's internal 192 kHz stage.
    let pr = probe_media(&bin, &out, &CancellationToken::new()).unwrap();
    assert_eq!(pr.first_audio().unwrap().sample_rate, 48_000);

    // Silence is refused rather than normalised.
    let silent = p(dir.path(), "silent.wav");
    run_checked(
        &bin,
        FfmpegCommand::base("error")
            .texts([
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=stereo",
                "-t",
                "3",
                "-c:a",
                "pcm_s16le",
            ])
            .path(&silent),
        T,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(normalize_file(
        &bin,
        &silent,
        &p(dir.path(), "s_out.wav"),
        &target,
        "pcm_s16le",
        48_000,
        T,
        &CancellationToken::new(),
        None,
    )
    .is_err());
}
