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
    let mut reader = hound::WavReader::open(path).map_err(wav_err)?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(wav_err)?,
        hound::SampleFormat::Int => {
            // Same scale as the writer (and ScrapeRight): full scale is 2^(bits-1) - 1.
            let scale = ((1i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()
                .map_err(wav_err)?
        }
    };
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
    let spec = hound::WavReader::open(path).map_err(wav_err)?.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 {
        return Err(AudioError::Wav(format!(
            "expected 16 kHz mono, found {} Hz x{}",
            spec.sample_rate, spec.channels
        )));
    }
    Ok(read_wav_mono_f32(path)?.samples)
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
