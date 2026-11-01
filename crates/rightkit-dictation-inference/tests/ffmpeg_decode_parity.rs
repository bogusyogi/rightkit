//! Sample-identity proof for the file-transcription decode path.
//!
//! Parakeet's accuracy was measured on the legacy decode (temp `pcm_s16le` WAV read back
//! with hound). The shared `rightkit_ffmpeg::decode::decode_pcm_s16` pipes `s16le` from
//! stdout instead. This test decodes every audio fixture both ways and requires identical
//! `i16` vectors, so a silent change to the decoded samples fails here before it can reach
//! ASR. Compressed and resampled inputs are produced from the fixtures with the same
//! ffmpeg; an encoder the build lacks is reported and skipped, never failed.

use std::path::{Path, PathBuf};
use std::process::Command;

use rightkit_ffmpeg::decode::{decode_pcm_s16, DecodeOptions};
use rightkit_ffmpeg::{CancellationToken, DevFfmpeg};

/// Every distinct ffmpeg this host offers: production search first, then the staged vendor build.
fn ffmpeg_candidates() -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if path.is_file() && !found.contains(&path) {
            found.push(path);
        }
    };
    if let Some(explicit) = std::env::var_os("RIGHTKIT_FFMPEG_PATH") {
        let explicit = PathBuf::from(explicit);
        if explicit.is_absolute() {
            push(explicit);
        }
    }
    push(manifest.join("../src-tauri/resources/runtime").join(name));
    push(manifest.join("../src-tauri/resources/runtime/ffmpeg"));
    push(PathBuf::from(
        "D:/Claude/vendor/ffmpeg-lgpl/ffmpeg-win-x64.exe",
    ));
    push(PathBuf::from(
        "/Volumes/D/claude/vendor/ffmpeg-lgpl/ffmpeg-macos-arm64",
    ));
    found
}

/// The legacy decode, verbatim: temp WAV, owner WAV reader, i16 samples.
fn legacy_decode(ffmpeg: &Path, input: &Path, tmp: &Path) -> Result<Vec<i16>, String> {
    let out = Command::new(ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(tmp)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let wav = rightkit_audio::wav::read_wav_mono_f32(tmp).map_err(|e| e.to_string())?;
    assert_eq!(wav.sample_rate, 16_000);
    Ok(wav
        .samples
        .iter()
        .map(|v| (v * i16::MAX as f32).round() as i16)
        .collect())
}

fn shared_decode(ffmpeg: &Path, input: &Path) -> Result<Vec<i16>, String> {
    let binary = DevFfmpeg::unverified(ffmpeg);
    let options = DecodeOptions {
        sample_rate: 16_000,
        channels: 1,
        cancel: CancellationToken::new(),
        timeout: None,
        max_duration: None,
        audio_stream_index: None,
    };
    decode_pcm_s16(&binary, input, &options).map_err(|e| e.to_string())
}

/// Derive a compressed or resampled input from a fixture; `None` when this build cannot encode it.
fn derive(ffmpeg: &Path, source: &Path, out: &Path, args: &[&str]) -> Option<PathBuf> {
    let status = Command::new(ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .args(args)
        .arg(out)
        .output()
        .ok()?;
    if status.status.success() && out.is_file() {
        Some(out.to_path_buf())
    } else {
        println!(
            "SKIP derived {}: {}",
            out.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        );
        None
    }
}

#[test]
fn shared_s16_decode_is_sample_identical_to_legacy_wav_path() {
    let candidates = ffmpeg_candidates();
    if candidates.is_empty() {
        println!("SKIP: no ffmpeg found (RIGHTKIT_FFMPEG_PATH, src-tauri/resources/runtime, D:/Claude/vendor/ffmpeg-lgpl)");
        return;
    }
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rightkit-dictation-engine/tests/fixtures");
    let scratch = std::env::temp_dir().join(format!("hr_decode_parity_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();

    let mut mismatches = Vec::new();
    for ffmpeg in &candidates {
        println!("ffmpeg: {}", ffmpeg.display());
        let mut inputs: Vec<PathBuf> = ["short_9s.wav", "long_38s.wav", "filler_retake.wav"]
            .iter()
            .map(|n| fixtures.join(n))
            .filter(|p| p.is_file())
            .collect();
        assert!(
            !inputs.is_empty(),
            "no wav fixtures under {}",
            fixtures.display()
        );
        let base = fixtures.join("long_38s.wav");
        let tag = candidates.iter().position(|c| c == ffmpeg).unwrap();
        for (file, args) in [
            ("stereo44k.flac", &["-ac", "2", "-ar", "44100"][..]),
            (
                "aac.m4a",
                &["-c:a", "aac", "-b:a", "96k", "-ar", "44100"][..],
            ),
            ("mp3.mp3", &["-c:a", "libmp3lame", "-b:a", "96k"][..]),
            ("opus.ogg", &["-c:a", "libopus", "-b:a", "48k"][..]),
            ("odd48k_mono.wav", &["-ar", "48000", "-ac", "1"][..]),
        ] {
            if let Some(path) = derive(ffmpeg, &base, &scratch.join(format!("{tag}_{file}")), args)
            {
                inputs.push(path);
            }
        }
        for input in &inputs {
            let tmp = scratch.join(format!("legacy_{tag}.wav"));
            let legacy = legacy_decode(ffmpeg, input, &tmp);
            let shared = shared_decode(ffmpeg, input);
            let label = input.file_name().unwrap().to_string_lossy().into_owned();
            match (legacy, shared) {
                (Ok(a), Ok(b)) => {
                    let identical = a == b;
                    println!(
                        "PARITY {label}: legacy={} shared={} identical={identical}",
                        a.len(),
                        b.len()
                    );
                    assert!(!a.is_empty(), "{label} decoded empty");
                    if !identical {
                        let first = a.iter().zip(&b).position(|(x, y)| x != y);
                        mismatches.push(format!(
                            "{label}: len legacy={} shared={}, first diff {:?}",
                            a.len(),
                            b.len(),
                            first.map(|i| (i, a[i], b[i]))
                        ));
                    }
                }
                (a, b) => mismatches.push(format!(
                    "{label}: legacy={:?} shared={:?}",
                    a.err(),
                    b.err()
                )),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        mismatches.is_empty(),
        "decode parity broken:\n{}",
        mismatches.join("\n")
    );
}
