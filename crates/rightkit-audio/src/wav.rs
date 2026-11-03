//! WAV read/write through `hound`. Dedupes ScrapeRight `crates/audio/src/wav.rs`
//! (16 kHz mono reader) and the PCM writers apps hand-rolled around capture.

use std::path::Path;

use crate::error::AudioError;

fn wav_err(e: hound::Error) -> AudioError {
    AudioError::Wav(e.to_string())
}

/// Decoded mono samples and their sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct MonoAudio {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

/// Read any PCM/float WAV as mono `f32` (multichannel is averaged).
pub fn read_wav_mono_f32(path: &Path) -> Result<MonoAudio, AudioError> {
    let reader = hound::WavReader::open(path).map_err(wav_err)?;
    decode_mono_f32(reader, None)
}

/// Read only the first `seconds` of a WAV as mono `f32` (#740). Decoding stops after
/// `ceil(seconds * sample_rate)` frames, so the rest of the file is never read. A file
/// shorter than the prefix returns all of its samples. `seconds` must be finite and positive.
pub fn read_wav_mono_f32_prefix(path: &Path, seconds: f64) -> Result<MonoAudio, AudioError> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(AudioError::Wav(format!(
            "prefix length must be a positive finite number of seconds, got {seconds}"
        )));
    }
    let reader = hound::WavReader::open(path).map_err(wav_err)?;
    let frames = (seconds * f64::from(reader.spec().sample_rate)).ceil() as u64;
    decode_mono_f32(reader, Some(frames))
}

/// Decode an opened reader to mono `f32`, at most `max_frames` frames when given. The
/// sample buffer is sized from the header's data length (`reader.len()`, total samples across
/// channels, capped at the limit), so it is filled in one pass without regrowth.
fn decode_mono_f32<R: std::io::Read>(
    mut reader: hound::WavReader<R>,
    max_frames: Option<u64>,
) -> Result<MonoAudio, AudioError> {
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let limit = max_frames
        .map(|f| {
            usize::try_from(f)
                .unwrap_or(usize::MAX)
                .saturating_mul(channels)
        })
        .unwrap_or(usize::MAX);
    let mut interleaved: Vec<f32> = Vec::with_capacity((reader.len() as usize).min(limit));
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>().take(limit) {
                interleaved.push(s.map_err(wav_err)?);
            }
        }
        hound::SampleFormat::Int => {
            // Same scale as the writer (and ScrapeRight): full scale is 2^(bits-1) - 1.
            let scale = ((1i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            for s in reader.samples::<i32>().take(limit) {
                interleaved.push(s.map_err(wav_err)? as f32 / scale);
            }
        }
    }
    let samples = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok(MonoAudio {
        sample_rate: spec.sample_rate,
        samples,
    })
}

/// ScrapeRight's contract: the file must already be 16 kHz mono; returns its samples.
pub fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>, AudioError> {
    let reader = hound::WavReader::open(path).map_err(wav_err)?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 {
        return Err(AudioError::Wav(format!(
            "expected 16 kHz mono, found {} Hz x{}",
            spec.sample_rate, spec.channels
        )));
    }
    Ok(decode_mono_f32(reader, None)?.samples)
}

/// Write mono `f32` samples as 16-bit PCM (clamped, scaled by 32767).
pub fn write_wav_mono_i16(
    path: &Path,
    sample_rate: u32,
    samples: &[f32],
) -> Result<(), AudioError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(wav_err)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .map_err(wav_err)?;
    }
    w.finalize().map_err(wav_err)
}
