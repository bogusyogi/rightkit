//! Minimal mono PCM16 WAV helpers.
use crate::SAMPLE_RATE_HZ;

pub fn encode_pcm16(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

pub fn silence(ms: u32) -> Vec<i16> {
    vec![0; (SAMPLE_RATE_HZ as u64 * ms as u64 / 1000) as usize]
}

/// `(sample_rate, samples)` of a mono/stereo PCM16 WAV (first channel); for tests and reports.
pub fn decode_pcm16(bytes: &[u8]) -> Option<(u32, Vec<i16>)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let (mut rate, mut channels, mut data) = (0u32, 1usize, None);
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().ok()?) as usize;
        let body = bytes.get(i + 8..(i + 8 + len).min(bytes.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]) as usize;
            rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
        } else if id == b"data" {
            data = Some(body);
            break;
        }
        i += 8 + len + (len & 1);
    }
    let data = data?;
    let s: Vec<i16> = data
        .chunks_exact(2 * channels.max(1))
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some((rate, s))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let w = encode_pcm16(&[0, 1, -1, 32767], 24_000);
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(decode_pcm16(&w).unwrap(), (24_000, vec![0, 1, -1, 32767]));
    }
}
