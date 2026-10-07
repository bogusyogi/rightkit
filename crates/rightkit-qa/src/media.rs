//! Content validators for generated artifacts: a file existing is not a result.
//! Pure Rust for WAV/PNG; ffprobe/ffmpeg only where decoding is genuinely needed.
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{err, sha256_hex, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::time::Duration;

pub fn file_stat(path: &Path) -> Value {
    match fs::read(path) {
        Ok(b) => json!({"exists": true, "size": b.len(), "sha256": sha256_hex(&b)}),
        Err(_) => json!({"exists": false, "size": 0, "sha256": null}),
    }
}

pub fn png_size(path: &Path) -> Result<Value> {
    let b = fs::read(path)?;
    if b.len() < 24 || b[..8] != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        return err(format!("{} is not a PNG", path.display()));
    }
    Ok(
        json!({"width": u32::from_be_bytes(b[16..20].try_into().unwrap()), "height": u32::from_be_bytes(b[20..24].try_into().unwrap())}),
    )
}

/// Parse a PCM WAV: channels, rate, duration, RMS/peak dB, silence flag.
pub fn wav_stats(path: &Path) -> Result<Value> {
    let b = fs::read(path)?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return err(format!("{} is not a RIFF/WAVE file", path.display()));
    }
    let (mut off, mut fmt, mut data) = (12usize, None, None);
    while off + 8 <= b.len() {
        let id = &b[off..off + 4];
        let size = u32::from_le_bytes(b[off + 4..off + 8].try_into().unwrap()) as usize;
        let end = (off + 8 + size).min(b.len());
        if id == b"fmt " {
            fmt = Some(&b[off + 8..end]);
        }
        if id == b"data" {
            data = Some(&b[off + 8..end]);
        }
        off += 8 + size + (size % 2);
    }
    let (Some(fmt), Some(data)) = (fmt, data) else {
        return err("WAV is missing fmt or data chunk");
    };
    if fmt.len() < 16 {
        return err("WAV fmt chunk too short");
    }
    let channels = u16::from_le_bytes([fmt[2], fmt[3]]) as f64;
    let rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap()) as f64;
    let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
    let duration = data.len() as f64 / (rate * channels * (bits as f64 / 8.0)).max(1.0);
    let (mut sum_sq, mut peak, mut n) = (0f64, 0f64, 0usize);
    if bits == 16 {
        for c in data.as_chunks::<2>().0 {
            let v = i16::from_le_bytes([c[0], c[1]]) as f64 / 32768.0;
            sum_sq += v * v;
            peak = peak.max(v.abs());
            n += 1;
        }
    }
    let rms = (sum_sq / n.max(1) as f64).sqrt();
    let db = |v: f64| 20.0 * (if v > 0.0 { v } else { 1e-9 }).log10();
    Ok(
        json!({"channels": channels, "rate": rate, "bits": bits, "duration_s": duration, "rms_db": db(rms), "peak_db": db(peak), "silent": rms < 0.0001}),
    )
}

/// Deterministic 16 kHz mono speech-like WAV (same generator the Node suite used).
pub fn write_reference_wav(path: &Path, seconds: f64) -> Result<()> {
    let rate = 16000f64;
    let n = (rate * seconds) as usize;
    let mut data = Vec::with_capacity(n * 2);
    let (mut s, mut lp) = (12345i64, 0f64);
    for i in 0..n {
        s = (s * 1103515245 + 12345) & 0x7fffffff;
        let rnd = s as f64 / 0x40000000 as f64 - 1.0;
        let t = i as f64;
        let env = 0.4 + 0.6 * (2.0 * std::f64::consts::PI * 3.3 * t / rate).sin().abs();
        lp = 0.85 * lp + 0.15 * rnd;
        let v = env
            * (0.35 * (2.0 * std::f64::consts::PI * 190.0 * t / rate).sin()
                + 0.2 * (2.0 * std::f64::consts::PI * 380.0 * t / rate + 1.0).sin()
                + 0.25 * lp);
        data.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32000.0) as i16).to_le_bytes());
    }
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&32000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(path, out)?;
    Ok(())
}

pub fn ffprobe_bin() -> String {
    std::env::var("RIGHTKIT_QA_FFPROBE")
        .or_else(|_| std::env::var("GENRIGHT_FFPROBE"))
        .unwrap_or_else(|_| "ffprobe".into())
}

/// `ffprobe -show_format -show_streams` as JSON; errors if ffprobe is absent.
pub fn ffprobe(path: &Path, tracker: &Tracker) -> Result<Value> {
    let out = run_owned(
        &ffprobe_bin(),
        &[
            "-v".into(),
            "quiet".into(),
            "-print_format".into(),
            "json".into(),
            "-show_format".into(),
            "-show_streams".into(),
            path.to_string_lossy().into(),
        ],
        &RunOptions {
            timeout: Some(Duration::from_secs(60)),
            label: "ffprobe".into(),
            ..Default::default()
        },
        tracker,
    )?;
    if out.code != Some(0) {
        return err(format!(
            "ffprobe failed ({:?}): {}",
            out.code,
            crate::util::tail(&out.stderr, 500)
        ));
    }
    Ok(serde_json::from_str(&out.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_wav_round_trips_through_stats() {
        let p = std::env::temp_dir().join(format!("rkqa-{}.wav", crate::util::new_id()));
        write_reference_wav(&p, 1.0).unwrap();
        let s = wav_stats(&p).unwrap();
        assert_eq!(s["rate"], 16000.0);
        assert_eq!(s["silent"], false);
        assert!((s["duration_s"].as_f64().unwrap() - 1.0).abs() < 0.01);
        assert!(png_size(&p).is_err());
        let _ = fs::remove_file(p);
    }
}
