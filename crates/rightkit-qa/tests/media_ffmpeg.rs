//! Cross-checks the pure-Rust validators against ffmpeg, the measurement GenRight ships
//! (`ebur128=peak=true`, decoded frames, decoded PNGs). Skips (printing why) when no ffmpeg is
//! found: settings key `RIGHTKIT_QA_FFMPEG`, then `PATH`, then common install directories.
use rightkit_qa::loudness::{ebur128_ffmpeg, measure, measure_file, FFMPEG_AGREEMENT_LU};
use rightkit_qa::media::{
    frames_non_trivial, locate_tool, png_non_trivial, read_wav, write_reference_wav, write_wav_f32,
    Pcm,
};
use rightkit_qa::process::{run_owned, RunOptions, Tracker};
use rightkit_qa::settings::Settings;
use std::path::{Path, PathBuf};

fn ffmpeg() -> Option<PathBuf> {
    let s = Settings::discover(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let f = locate_tool("ffmpeg", &s, "RIGHTKIT_QA_FFMPEG");
    if f.is_none() {
        println!("[qa] SKIP ffmpeg cross-check: no ffmpeg binary found");
    }
    f
}

fn tmp(ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rkqa-ff-{}.{ext}", rightkit_qa::util::new_id()))
}

fn ff(ffmpeg: &Path, args: &[&str], t: &Tracker) {
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-v", "error", "-y"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    a.extend(args.iter().map(|s| s.to_string()));
    let o = run_owned(
        &ffmpeg.to_string_lossy(),
        &a,
        &RunOptions {
            label: "ffmpeg fixture".into(),
            ..Default::default()
        },
        t,
    )
    .unwrap();
    assert_eq!(o.code, Some(0), "ffmpeg {args:?}: {}", o.stderr);
}

/// Speech-like stereo at 48 kHz: two detuned partials, slow amplitude swells, quiet gaps.
fn program(rate: u32, secs: f64) -> Pcm {
    let n = (rate as f64 * secs) as usize;
    let mut samples = Vec::with_capacity(n * 2);
    let tau = std::f64::consts::TAU;
    for i in 0..n {
        let t = i as f64 / rate as f64;
        let env = 0.15 + 0.85 * (tau * 0.4 * t).sin().abs();
        let gap = if (t % 2.5) > 2.1 { 0.02 } else { 1.0 };
        let l = env * gap * (0.4 * (tau * 220.0 * t).sin() + 0.2 * (tau * 3150.0 * t).sin());
        let r = env * gap * (0.3 * (tau * 330.0 * t).sin() + 0.25 * (tau * 1250.0 * t).sin());
        samples.push(l as f32);
        samples.push(r as f32);
    }
    Pcm {
        sample_rate: rate,
        channels: 2,
        samples,
    }
}

#[test]
fn loudness_agrees_with_ffmpeg_ebur128() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let t = Tracker::new();
    let speech = tmp("wav");
    write_reference_wav(&speech, 6.0).unwrap(); // 16 kHz mono int16
    let stereo = tmp("wav");
    write_wav_f32(&stereo, &program(48000, 12.0)).unwrap();
    let cd = tmp("wav");
    let mut p = program(44100, 8.0);
    p.samples.iter_mut().for_each(|s| *s *= 0.2);
    write_wav_f32(&cd, &p).unwrap();
    for f in [&speech, &stereo, &cd] {
        let ours = measure_file(f, None, &t).unwrap();
        let theirs = ebur128_ffmpeg(&ffmpeg, f, &t).unwrap();
        let (a, b) = (
            ours.integrated_lufs.unwrap(),
            theirs.integrated_lufs.unwrap(),
        );
        println!("[qa] {}: ours {ours:?} ffmpeg {theirs:?}", f.display());
        assert!(
            (a - b).abs() <= FFMPEG_AGREEMENT_LU,
            "integrated {a} vs ffmpeg {b}"
        );
        assert!(
            (ours.true_peak_dbtp - theirs.true_peak_dbtp).abs() <= 0.5,
            "true peak {} vs ffmpeg {}",
            ours.true_peak_dbtp,
            theirs.true_peak_dbtp
        );
        if let (Some(x), Some(y)) = (ours.loudness_range_lu, theirs.loudness_range_lu) {
            assert!((x - y).abs() <= 1.0, "LRA {x} vs ffmpeg {y}");
        }
        assert!(t.assert_no_orphans().is_ok());
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn normalized_export_meets_genright_tolerance_through_flac() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let t = Tracker::new();
    // Gain a program to -16 LUFS using the pure-Rust measure (GenRight's export target).
    let mut p = program(48000, 10.0);
    let gain = 10f64.powf((-16.0 - measure(&p).integrated_lufs.unwrap()) / 20.0) as f32;
    p.samples.iter_mut().for_each(|s| *s *= gain);
    let wav = tmp("wav");
    write_wav_f32(&wav, &p).unwrap();
    let flac = tmp("flac");
    ff(
        &ffmpeg,
        &[
            "-i",
            &wav.to_string_lossy(),
            "-c:a",
            "flac",
            "-sample_fmt",
            "s32",
            &flac.to_string_lossy(),
        ],
        &t,
    );
    // FLAC needs ffmpeg to decode; WAV does not.
    assert!(measure_file(&flac, None, &t).is_err());
    let ours = measure_file(&flac, Some(&ffmpeg), &t).unwrap();
    let theirs = ebur128_ffmpeg(&ffmpeg, &flac, &t).unwrap();
    assert!(ours.within(-16.0, 0.05), "{ours:?}");
    assert!(
        theirs.within(-16.0, 1.0),
        "GenRight's +-1 LU export tolerance: {theirs:?}"
    );
    let decoded = rightkit_qa::media::decode_audio_ffmpeg(&ffmpeg, &flac, &t).unwrap();
    assert_eq!((decoded.sample_rate, decoded.channels), (48000, 2));
    assert_eq!(decoded.frames(), read_wav(&wav).unwrap().frames());
    for f in [wav, flac] {
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn frames_and_png_verdicts_on_ffmpeg_output() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let t = Tracker::new();
    let busy = tmp("mp4");
    ff(
        &ffmpeg,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=10:duration=1",
            "-c:v",
            "mpeg4",
            "-q:v",
            "3",
            &busy.to_string_lossy(),
        ],
        &t,
    );
    let flat = tmp("mp4");
    ff(
        &ffmpeg,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=0x406080:size=160x120:rate=10:duration=1",
            "-c:v",
            "mpeg4",
            "-q:v",
            "3",
            &flat.to_string_lossy(),
        ],
        &t,
    );
    let r = frames_non_trivial(&ffmpeg, &busy, 3, &t).unwrap();
    assert!(r.non_trivial && r.frames.len() == 3, "{r:?}");
    let r = frames_non_trivial(&ffmpeg, &flat, 3, &t).unwrap();
    assert!(!r.non_trivial && !r.frames.is_empty(), "{r:?}");
    let not_video = tmp("txt");
    std::fs::write(&not_video, "hello").unwrap();
    assert!(frames_non_trivial(&ffmpeg, &not_video, 3, &t).is_err());

    let png = tmp("png");
    ff(
        &ffmpeg,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=128x128:duration=0.04:rate=25",
            "-frames:v",
            "1",
            &png.to_string_lossy(),
        ],
        &t,
    );
    assert!(png_non_trivial(&png).unwrap().non_trivial);
    let black = tmp("png");
    ff(
        &ffmpeg,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:size=64x64:duration=0.04:rate=25",
            "-frames:v",
            "1",
            &black.to_string_lossy(),
        ],
        &t,
    );
    assert!(!png_non_trivial(&black).unwrap().non_trivial);
    assert!(t.assert_no_orphans().is_ok());
    for f in [busy, flat, not_video, png, black] {
        let _ = std::fs::remove_file(f);
    }
}
