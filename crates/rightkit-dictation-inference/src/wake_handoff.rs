//! Pure arithmetic of the idle-wake to recording handoff, shared by the
//! runtime's `WakeListener` and the Hey-Zephyr replay harness so both use one
//! definition.
//!
//! The `end` that `Kws::feed_idle_wake` returns is the last keyword
//! token timestamp plus 50 ms (`kws.rs`). It is NOT a measured acoustic
//! phrase end. The handoff starts a further 40 ms after it.

use std::collections::VecDeque;

const SAMPLE_RATE: usize = 16_000;

/// Idle audio kept while listening and while a handoff is pending (4 s).
pub const IDLE_AUDIO_WINDOW_SAMPLES: usize = SAMPLE_RATE * 4;

/// Margin added to the returned wake end before the handoff audio is copied (40 ms).
pub const WAKE_HANDOFF_MARGIN_SAMPLES: usize = SAMPLE_RATE / 25;

/// Idle-clock sample at which the recording's audio starts.
pub fn wake_handoff_start(end_sample: usize) -> usize {
    end_sample.saturating_add(WAKE_HANDOFF_MARGIN_SAMPLES)
}

/// Audio of the idle window from `handoff_start` on. `window_origin` is the idle
/// sample index of the window's first sample.
pub fn wake_handoff_audio(
    window: &VecDeque<f32>,
    window_origin: usize,
    handoff_start: usize,
) -> Vec<f32> {
    let offset = handoff_start.saturating_sub(window_origin);
    window.iter().skip(offset).copied().collect()
}
