//! condition_audio — apply the exact shipped ASR conditioning policy to a raw
//! f32 stream, for offline parity/sweep harnesses (verify_audio_conditioning_parity.py,
//! conditioning_sweep.py).
//!
//!   condition_audio <sample_rate> <policy>   (e.g. 16000 asr_simple_gain_hpf | raw)
//!
//! stdin  = little-endian f32 mono samples
//! stdout = little-endian f32 mono samples (conditioned, same length)
use std::io::{Read, Write};

fn main() {
    let mut args = std::env::args().skip(1);
    let sr: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(16_000);
    let policy = args
        .next()
        .unwrap_or_else(|| "asr_simple_gain_hpf".to_string());

    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf).expect("read stdin");
    let (sample_bytes, _) = buf.as_chunks::<4>();
    let samples: Vec<f32> = sample_bytes
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();

    let out = rightkit_dictation_core::audio_conditioning::condition_for_asr(&samples, sr, &policy);

    let mut bytes = Vec::with_capacity(out.len() * 4);
    for s in out {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::io::stdout().write_all(&bytes).expect("write stdout");
}
