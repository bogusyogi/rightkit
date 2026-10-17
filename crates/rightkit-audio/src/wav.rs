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
    decode_mono_f32(reader)
}

/// Decode an opened reader to mono `f32`. The sample buffer is sized from the
/// header's data length (`reader.len()`, total samples across channels), so
/// it is filled in one pass without regrowth.
fn decode_mono_f32<R: std::io::Read>(
    mut reader: hound::WavReader<R>,
) -> Result<MonoAudio, AudioError> {
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let mut interleaved: Vec<f32> = Vec::with_capacity(reader.len() as usize);
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                interleaved.push(s.map_err(wav_err)?);
            }
        }
        hound::SampleFormat::Int => {
            // Same scale as the writer (and ScrapeRight): full scale is 2^(bits-1) - 1.
            let scale = ((1i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            for s in reader.samples::<i32>() {
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
    Ok(decode_mono_f32(reader)?.samples)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_i16(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for s in samples {
            w.write_sample(*s).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn read_16k_mono_matches_mono_f32_and_sizes_from_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let pcm: Vec<i16> = (0..1000).map(|i| ((i * 37) % 2000 - 1000) as i16).collect();
        write_i16(&path, 16_000, 1, &pcm);

        let got = read_wav_16k_mono(&path).unwrap();
        let expected: Vec<f32> = pcm.iter().map(|v| *v as f32 / 32767.0).collect();
        assert_eq!(got, expected);
        assert_eq!(got, read_wav_mono_f32(&path).unwrap().samples);

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.len() as usize, pcm.len());
        let decoded = decode_mono_f32(reader).unwrap();
        assert_eq!(decoded.samples.capacity(), pcm.len());
    }

    #[test]
    fn read_16k_mono_rejects_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.wav");
        write_i16(&path, 44_100, 1, &[0, 1, 2]);
        assert!(read_wav_16k_mono(&path).is_err());
        write_i16(&path, 16_000, 2, &[0, 1, 2, 3]);
        assert!(read_wav_16k_mono(&path).is_err());
    }

    #[test]
    fn read_16k_mono_empty_data_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.wav");
        write_i16(&path, 16_000, 1, &[]);
        assert!(read_wav_16k_mono(&path).unwrap().is_empty());
    }
}
