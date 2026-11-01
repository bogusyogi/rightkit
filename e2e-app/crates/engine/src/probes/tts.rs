//! rightkit-tts (system backend): render real speech with the OS voice (macOS `say` + `afconvert`, Windows SAPI) to a
//! WAV file, never to a speaker, and check the audio and the word-timed transcript. The crate's QTS worker backend
//! needs the `engine` feature and a model; it is exercised by the `tts_qwen` probe's model path, not here.
//!
//! Needs: nothing beyond the OS voice (macOS `say`/`afconvert` on PATH, Windows PowerShell with System.Speech).
use rightkit_audio::capture::StreamResampler;
use rightkit_tts::wav::decode_pcm16;
use rightkit_tts::{chunk_sentences, SystemTts, SystemTtsOptions, SystemTtsResult};
use serde_json::{json, Value};

fn rms(samples: &[i16]) -> f64 {
    (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
}

/// Real OS-voice speech as 16 kHz mono f32 (shared by the vad and asr probes), with the synthesis result.
pub fn speech_16k(text: &str) -> Result<(Vec<f32>, SystemTtsResult), String> {
    let result = SystemTts.synthesize(text, &SystemTtsOptions::default()).map_err(|e| format!("system TTS: {e}"))?;
    let (rate, pcm) = decode_pcm16(&result.wav).ok_or("system TTS did not return mono PCM16")?;
    let f: Vec<f32> = pcm.iter().map(|s| *s as f32 / 32768.0).collect();
    let out = if rate == 16_000 {
        f
    } else {
        StreamResampler::new(rate, 16_000).map_err(|e| format!("resampler {rate}->16000: {e}"))?.process_offline(&f).map_err(|e| format!("resample: {e}"))?
    };
    Ok((out, result))
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let chunks = chunk_sentences("Dr. Smith said hello. It worked.");
    if chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>() != ["Dr. Smith said hello.", "It worked."] {
        return Err(format!("chunk_sentences split wrongly: {chunks:?}"));
    }

    let text = "RightKit speech output is written to a file. Every word gets a time.";
    let result = SystemTts.synthesize(text, &SystemTtsOptions::default()).map_err(|e| format!("synthesize: {e}"))?;
    let expected_backend = if cfg!(target_os = "macos") { "macos-say" } else { "windows-sapi" };
    if result.backend != expected_backend {
        return Err(format!("backend {:?}, expected {expected_backend}", result.backend));
    }
    let dir = ctx.dir("tts")?;
    let path = dir.join("speech.wav");
    std::fs::write(&path, &result.wav).map_err(|e| format!("write {}: {e}", path.display()))?;
    let on_disk = std::fs::read(&path).map_err(|e| format!("read back: {e}"))?;
    if on_disk.len() < 44 || &on_disk[..4] != b"RIFF" || &on_disk[8..12] != b"WAVE" {
        return Err("the file is not a RIFF/WAVE".into());
    }
    let (rate, pcm) = decode_pcm16(&on_disk).ok_or("the file is not mono PCM16")?;
    let seconds = pcm.len() as f64 / rate as f64;
    if !(1.5..=30.0).contains(&seconds) {
        return Err(format!("speech lasts {seconds:.2}s for a two-sentence text"));
    }
    let level = rms(&pcm);
    if level < 50.0 {
        return Err(format!("speech is silent (rms {level:.1} of 32768)"));
    }
    if result.manifest.sample_rate != rate || result.manifest.segments.len() != 2 {
        return Err(format!("manifest: rate {} segments {}", result.manifest.sample_rate, result.manifest.segments.len()));
    }
    let dur = result.manifest.total_duration_ms as f64 / 1000.0;
    if (dur - seconds).abs() > 0.05 {
        return Err(format!("manifest duration {dur}s differs from the audio {seconds}s"));
    }
    result.transcript.validate().map_err(|e| format!("transcript: {e}"))?;
    let words = &result.transcript.words;
    if words.len() < 10 || !result.transcript.text().contains("RightKit") {
        return Err(format!("transcript has {} words: {:?}", words.len(), result.transcript.text()));
    }
    if words.windows(2).any(|w| w[1].start_ms < w[0].end_ms) || words.last().is_some_and(|w| w.end_ms > result.transcript.duration_ms) {
        return Err("word timings overlap or run past the audio".into());
    }
    // The audio is voiced across its length, not one burst: both halves carry energy.
    let (a, b) = pcm.split_at(pcm.len() / 2);
    if rms(a) < 20.0 || rms(b) < 20.0 {
        return Err("one half of the speech is silent".into());
    }

    Ok(json!({
        "backend": result.backend,
        "sample_rate": rate,
        "seconds": seconds,
        "rms": level,
        "wav_bytes": on_disk.len(),
        "words": words.len(),
        "segments": result.manifest.segments.len(),
    }))
}
