//! Pure per-block rendering used by the output callback: pop from the ring, pad with silence,
//! apply volume/mute. No cpal types, no allocation, no locks, so it is unit-testable and
//! provably safe to run on the audio thread.

use super::ring::SampleRing;

/// Result of rendering one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockOutcome {
    /// Samples taken from the ring.
    pub(crate) popped: usize,
    /// Samples of silence substituted for missing data (zero when paused).
    pub(crate) silent: usize,
}

/// Scale `buf` (interleaved, `channels` per frame) by a gain that moves linearly from `from`
/// to `to` across the block, then clamp to [-1, 1]. Ramping avoids a click when volume or mute
/// changes. Unity-to-unity is left untouched.
pub(crate) fn apply_gain_ramp(buf: &mut [f32], channels: usize, from: f32, to: f32) {
    if from == 1.0 && to == 1.0 {
        return;
    }
    if from == to {
        for s in buf.iter_mut() {
            *s = (*s * to).clamp(-1.0, 1.0);
        }
        return;
    }
    let channels = channels.max(1);
    let frames = buf.len().div_ceil(channels).max(1);
    let step = (to - from) / frames as f32;
    for (i, frame) in buf.chunks_mut(channels).enumerate() {
        let gain = from + step * (i + 1) as f32;
        for s in frame {
            *s = (*s * gain).clamp(-1.0, 1.0);
        }
    }
}

/// Fill `block` for the device. Paused: silence, nothing consumed. Otherwise pop what is
/// queued, pad the rest with silence (underrun) and apply the gain ramp to the real samples.
pub(crate) fn fill_block(
    ring: &SampleRing,
    block: &mut [f32],
    channels: usize,
    paused: bool,
    gain_from: f32,
    gain_to: f32,
) -> BlockOutcome {
    if paused {
        block.fill(0.0);
        return BlockOutcome {
            popped: 0,
            silent: 0,
        };
    }
    let popped = ring.pop_into(block);
    block[popped..].fill(0.0);
    apply_gain_ramp(&mut block[..popped], channels, gain_from, gain_to);
    BlockOutcome {
        popped,
        silent: block.len() - popped,
    }
}
