//! rightkit-audio: WAV I/O, level maths and the resampler on a generated PCM buffer, then the real FFmpeg filter
//! chain (high-pass) and loudness measurement on rendered tones, and input-device enumeration (no microphone
//! permission: nothing is opened).
//!
//! Needs: the catalog-verified ffmpeg resolved by `probes::ffmpeg::ffmpeg_handle` (see that module for where the
//! crate looks). Features used: default (`capture`); the `playback` feature is not enabled by the engine.
use std::time::Duration;

use rightkit_audio::capture::{list_input_devices, StreamResampler};
use rightkit_audio::chain::{Denoise, DialogueChain, HighPass, Limiter};
use rightkit_audio::measure::{peak_db, rms_db, AudioTools};
use rightkit_audio::wav::{read_wav_16k_mono, read_wav_mono_f32, write_wav_mono_i16};
use serde_json::{json, Value};

fn sine(hz: f32, rate: u32, seconds: f32, amp: f32) -> Vec<f32> {
    let n = (rate as f32 * seconds) as usize;
    (0..n)
        .map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
        .collect()
}

fn near(what: &str, got: f64, want: f64, tol: f64) -> Result<(), String> {
    if (got - want).abs() <= tol {
        Ok(())
    } else {
        Err(format!("{what}: got {got:.3}, want {want:.3} +/- {tol}"))
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("audio")?;

    // 1. A 1 s 440 Hz burst (amplitude 0.5) followed by 1 s of silence, 16 kHz mono.
    let mut pcm = sine(440.0, 16_000, 1.0, 0.5);
    pcm.extend(std::iter::repeat(0.0).take(16_000));
    let wav = dir.join("burst.wav");
    write_wav_mono_i16(&wav, 16_000, &pcm).map_err(|e| format!("write_wav_mono_i16: {e}"))?;
    let back = read_wav_16k_mono(&wav).map_err(|e| format!("read_wav_16k_mono: {e}"))?;
    if back.len() != pcm.len() {
        return Err(format!(
            "WAV round trip changed length {} -> {}",
            pcm.len(),
            back.len()
        ));
    }
    let worst = pcm
        .iter()
        .zip(&back)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    if worst > 1.0 / 16_000.0 {
        return Err(format!(
            "WAV round trip error {worst} exceeds one 16-bit step"
        ));
    }
    let mono = read_wav_mono_f32(&wav).map_err(|e| format!("read_wav_mono_f32: {e}"))?;
    if mono.sample_rate != 16_000 || mono.samples.len() != pcm.len() {
        return Err("read_wav_mono_f32 disagrees with read_wav_16k_mono".into());
    }
    // A non-16k file must be refused by the 16 kHz reader.
    let wav8 = dir.join("rate8k.wav");
    write_wav_mono_i16(&wav8, 8_000, &pcm[..8_000]).map_err(|e| format!("write 8k: {e}"))?;
    if read_wav_16k_mono(&wav8).is_ok() {
        return Err("read_wav_16k_mono accepted an 8 kHz file".into());
    }

    // 2. Level maths: a 0.5 amplitude sine is -9.03 dB RMS and -6.02 dB peak; silence is far below.
    let burst = &back[..16_000];
    near("burst rms dB", rms_db(burst), -9.03, 0.2)?;
    near("burst peak dB", peak_db(burst), -6.02, 0.1)?;
    let silent = rms_db(&back[16_000..]);
    if silent > -100.0 {
        return Err(format!("silence measures {silent} dB"));
    }

    // 3. Resampler: 16 kHz -> 48 kHz keeps the duration and the level.
    let mut up =
        StreamResampler::new(16_000, 48_000).map_err(|e| format!("StreamResampler: {e}"))?;
    let tone = sine(440.0, 16_000, 2.0, 0.5);
    let up_samples = up
        .process_offline(&tone)
        .map_err(|e| format!("process_offline: {e}"))?;
    let want = tone.len() * 3;
    if up_samples.len().abs_diff(want) > want / 50 {
        return Err(format!(
            "resampled {} samples, expected about {want}",
            up_samples.len()
        ));
    }
    let mid = &up_samples[up_samples.len() / 4..up_samples.len() * 3 / 4];
    near("resampled rms dB", rms_db(mid), rms_db(&tone), 0.5)?;

    // 4. FFmpeg: render real tones, run the dialogue chain, measure.
    let ffmpeg = super::ffmpeg::ffmpeg_handle()?;
    let tools = AudioTools {
        ffmpeg: &ffmpeg,
        timeout: Duration::from_secs(60),
        cancel: rightkit_ffmpeg::CancellationToken::new(),
    };
    let low = dir.join("tone50.wav");
    let high = dir.join("tone2000.wav");
    super::ffmpeg::render_sine(&ffmpeg, &low, 50, 2)?;
    super::ffmpeg::render_sine(&ffmpeg, &high, 2000, 2)?;
    let chain = DialogueChain {
        high_pass: Some(HighPass {
            hz: 500.0,
            poles: 2,
        }),
        denoise: Denoise::None,
        expander: None,
        compressor: None,
        deesser: None,
        limiter: Limiter {
            ceiling_db: -1.5,
            attack_ms: 5.0,
            release_ms: 50.0,
        },
    };
    let low_out = dir.join("tone50.out.wav");
    let high_out = dir.join("tone2000.out.wav");
    tools
        .apply_chain(&low, &low_out, &chain)
        .map_err(|e| format!("apply_chain 50 Hz: {e}"))?;
    tools
        .apply_chain(&high, &high_out, &chain)
        .map_err(|e| format!("apply_chain 2 kHz: {e}"))?;
    let rms = |p: &std::path::Path| -> Result<f64, String> {
        let s = tools
            .decode_mono_f32(p, 48_000, Some((500, 1000)), None)
            .map_err(|e| format!("decode {}: {e}", p.display()))?;
        if s.is_empty() {
            return Err(format!("{} decoded to nothing", p.display()));
        }
        Ok(rms_db(&s))
    };
    let low_drop = rms(&low)? - rms(&low_out)?;
    let high_drop = (rms(&high)? - rms(&high_out)?).abs();
    if low_drop < 20.0 {
        return Err(format!(
            "a 500 Hz high-pass cut a 50 Hz tone by only {low_drop:.1} dB"
        ));
    }
    if high_drop > 1.5 {
        return Err(format!(
            "a 500 Hz high-pass changed a 2 kHz tone by {high_drop:.1} dB"
        ));
    }
    let lm = tools
        .measure_loudness(&high_out)
        .map_err(|e| format!("measure_loudness: {e}"))?;
    let lufs = lm
        .integrated_lufs
        .ok_or("ebur128 gave no integrated loudness")?;
    let tp = lm.true_peak_dbtp.ok_or("ebur128 gave no true peak")?;
    if !(-40.0..=0.0).contains(&lufs) || tp > 0.0 {
        return Err(format!(
            "loudness {lufs} LUFS / true peak {tp} dBTP is not plausible"
        ));
    }
    let st = tools
        .astats(&high_out, None, None)
        .map_err(|e| format!("astats: {e}"))?;
    if !st.rms_db.is_finite() || st.peak_db > 0.0 {
        return Err(format!("astats rms {} peak {}", st.rms_db, st.peak_db));
    }
    // An invalid chain is rejected before FFmpeg runs.
    let mut bad = chain.clone();
    bad.limiter.ceiling_db = 3.0;
    if tools
        .apply_chain(&high, &dir.join("never.wav"), &bad)
        .is_ok()
    {
        return Err("apply_chain accepted a limiter ceiling above 0 dBFS".into());
    }

    // 5. Input device enumeration works without microphone permission (nothing is opened).
    let devices = list_input_devices().map_err(|e| format!("list_input_devices: {e}"))?;
    let defaults = devices.iter().filter(|d| d.is_default).count();
    if defaults > 1 {
        return Err(format!("{defaults} input devices claim to be the default"));
    }

    Ok(json!({
        "wav_roundtrip_max_error": worst,
        "burst_rms_db": rms_db(burst),
        "resampled_samples": up_samples.len(),
        "highpass_drop_50hz_db": low_drop,
        "highpass_change_2khz_db": high_drop,
        "integrated_lufs": lufs,
        "input_devices": devices.len(),
    }))
}
