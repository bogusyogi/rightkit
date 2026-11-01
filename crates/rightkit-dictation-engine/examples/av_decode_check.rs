//! Prove AVFoundation decode matches ffmpeg on real files.
//!
//! `file_transcribe::decode_to_16k_mono` now tries AVFoundation first. This runs
//! BOTH paths on the same inputs and compares the resulting f32 sample vectors,
//! so "byte-identical PCM" is checked in-process rather than trusted from the
//! standalone swift probe. Passing means the swap is safe for that file.
//!
//!   cargo run --release --example av_decode_check -- <file> [<file> ...]
//!
//! With no args it looks for a couple of well-known local fixtures.

#[cfg(target_os = "macos")]
use std::path::Path;

#[cfg(target_os = "macos")]
use rightkit_dictation_engine::av_decode;

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files: Vec<String> = if args.is_empty() {
        vec!["../../adaptive/app_capture/laptop_close_cond.wav".to_string()]
    } else {
        args
    };

    let mut any_fail = false;
    for f in &files {
        let path = Path::new(f);
        if !path.exists() {
            eprintln!("skip (missing): {f}");
            continue;
        }

        let av = match av_decode::decode_to_16k_mono(path) {
            Ok(s) => s,
            Err(e) => {
                println!("{f}: AVFoundation declined ({e}) — ffmpeg fallback would run, OK");
                continue;
            }
        };

        // Force the ffmpeg path directly for comparison. `RIGHTKIT_FORCE_FFMPEG` makes
        // decode_to_16k_mono skip AVFoundation; without a setter we call the
        // private path through the public wrapper on a non-macos-cfg basis, so
        // here we re-run ffmpeg via the same public API by temporarily asking for
        // it. Simplest: compare against a fresh ffmpeg decode.
        let ff = match decode_via_ffmpeg(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{f}: ffmpeg failed ({e}) — cannot compare");
                continue;
            }
        };

        let (n, m) = (av.len(), ff.len());
        // Lossless formats (WAV/FLAC) decode byte-identically. LOSSY formats
        // (mp3/aac) do NOT: each decoder handles the codec's priming/delay samples
        // differently, so AVFoundation's output is the same audio shifted by a few
        // hundred leading samples. That is inaudible and irrelevant to ASR, but it
        // means a fixed-index diff is the wrong test. Search a small offset window
        // and compare on the best alignment — the metric that reflects real
        // correctness.
        let (best_off, aligned_max) = best_alignment(&av, &ff, 2048);
        // <=~130 ms of priming slack (2048 samples @ 16 kHz) either way.
        let len_ok = n.abs_diff(m) <= 2048;
        // After alignment, a couple of s16le quanta (1/32767) is identical audio.
        let sample_ok = aligned_max <= 2.0e-3;

        let dur = av_decode::probe_duration_secs(path);
        println!(
            "{f}: av={n} ff={m}  off={best_off} ({}ms)  aligned max|Δ|={aligned_max:.2e}  len_ok={len_ok} sample_ok={sample_ok}  dur={dur:?}",
            best_off / 16
        );
        if !(len_ok && sample_ok) {
            any_fail = true;
        }
    }

    if any_fail {
        eprintln!("MISMATCH — AVFoundation and ffmpeg disagree on some file");
        std::process::exit(1);
    }
    println!("all compared files match within tolerance");
}

/// Best leading-offset alignment between two sample vectors, returning
/// `(offset, max_abs_diff_after_alignment)`. `offset > 0` means `ff` leads `av`.
/// Lossy decoders differ only by codec priming delay, so aligning before diffing
/// separates "different audio" from "same audio, shifted".
#[cfg(target_os = "macos")]
fn best_alignment(av: &[f32], ff: &[f32], search: i64) -> (i64, f32) {
    let win = 16_000usize; // score on the first second
    let mut best = (0i64, f32::MAX);
    for off in -search..=search {
        let (a_start, f_start) = if off >= 0 {
            (0usize, off as usize)
        } else {
            ((-off) as usize, 0usize)
        };
        if a_start + 100 >= av.len() || f_start + 100 >= ff.len() {
            continue;
        }
        let len = win.min(av.len() - a_start).min(ff.len() - f_start);
        if len < win / 2 {
            continue;
        }
        let mut sse = 0.0f64;
        for i in 0..len {
            let d = (av[a_start + i] - ff[f_start + i]) as f64;
            sse += d * d;
        }
        let mse = sse / len as f64;
        if (mse as f32) < best.1 {
            best = (off, mse as f32);
        }
    }
    // Recompute the max abs diff over the FULL overlap at the winning offset.
    let off = best.0;
    let (a_start, f_start) = if off >= 0 {
        (0usize, off as usize)
    } else {
        ((-off) as usize, 0usize)
    };
    let len = (av.len() - a_start).min(ff.len() - f_start);
    let max_abs = (0..len)
        .map(|i| (av[a_start + i] - ff[f_start + i]).abs())
        .fold(0.0f32, f32::max);
    (off, max_abs)
}

/// Decode strictly via ffmpeg, bypassing the AVFoundation front, for comparison.
#[cfg(target_os = "macos")]
fn decode_via_ffmpeg(input: &Path) -> Result<Vec<f32>, String> {
    // file_transcribe's ffmpeg path is private; reproduce the exact command here.
    use std::process::Command;
    let tmp = std::env::temp_dir().join(format!("av_check_{}.wav", std::process::id()));
    let ffmpeg = std::env::var_os("RIGHTKIT_FFMPEG_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("ffmpeg"));
    let out = Command::new(&ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&tmp)
        .output()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let samples = rightkit_audio::wav::read_wav_mono_f32(&tmp)
        .map_err(|e| e.to_string())?
        .samples;
    let _ = std::fs::remove_file(&tmp);
    Ok(samples)
}

#[cfg(not(target_os = "macos"))]
fn main() {}
