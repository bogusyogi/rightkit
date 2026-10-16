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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_scales_buffer() {
        let mut buf = [0.5f32, -0.5, 0.25, -0.25];
        apply_gain_ramp(&mut buf, 2, 0.5, 0.5);
        assert_eq!(buf, [0.25, -0.25, 0.125, -0.125]);
    }

    #[test]
    fn mute_silences_and_unity_is_untouched() {
        let mut muted = [0.5f32, -0.5, 0.25, -0.25];
        apply_gain_ramp(&mut muted, 2, 0.0, 0.0);
        assert_eq!(muted, [0.0; 4]);
        let mut unity = [0.5f32, -0.5, 2.0];
        apply_gain_ramp(&mut unity, 1, 1.0, 1.0);
        assert_eq!(unity, [0.5, -0.5, 2.0]);
    }

    #[test]
    fn gain_above_unity_clamps() {
        let mut buf = [0.75f32, -0.75];
        apply_gain_ramp(&mut buf, 1, 2.0, 2.0);
        assert_eq!(buf, [1.0, -1.0]);
    }

    #[test]
    fn ramp_is_monotonic_and_per_frame() {
        // Stereo, two frames, ramp 1.0 -> 0.0: frame 0 at 0.5, frame 1 at 0.0.
        let mut buf = [1.0f32, 1.0, 1.0, 1.0];
        apply_gain_ramp(&mut buf, 2, 1.0, 0.0);
        assert_eq!(buf, [0.5, 0.5, 0.0, 0.0]);
    }

    #[test]
    fn underrun_plays_silence_for_the_shortfall() {
        let ring = SampleRing::new(16);
        ring.push(&[0.5, 0.5, 0.5, 0.5], 2);
        let mut block = [7.0f32; 8];
        let out = fill_block(&ring, &mut block, 2, false, 1.0, 1.0);
        assert_eq!(
            out,
            BlockOutcome {
                popped: 4,
                silent: 4
            }
        );
        assert_eq!(block, [0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn paused_outputs_silence_and_consumes_nothing() {
        let ring = SampleRing::new(16);
        ring.push(&[0.5; 4], 2);
        let mut block = [7.0f32; 4];
        let out = fill_block(&ring, &mut block, 2, true, 1.0, 1.0);
        assert_eq!(
            out,
            BlockOutcome {
                popped: 0,
                silent: 0
            }
        );
        assert_eq!(block, [0.0; 4]);
        assert_eq!(ring.len(), 4);
    }
}
