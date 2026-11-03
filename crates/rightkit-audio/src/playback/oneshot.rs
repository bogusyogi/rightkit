//! One-shot file playback (feature `playback`): decode a WAV, convert it to the default output
//! device's rate and channel count, play it to the end, then release the stream.
//!
//! Intended for short cues such as a session chime. Blocking: the call returns when the device
//! has consumed the last sample, or with an error at the deadline.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::session::OutputSession;
use crate::error::AudioError;
use crate::wav::read_wav_mono_f32;

/// Slack added to the file duration before playback is treated as stalled.
const DEADLINE_SLACK: Duration = Duration::from_secs(5);
/// Back-off while the ring is full or the device is draining.
const POLL: Duration = Duration::from_millis(5);

/// Play the WAV at `path` once through the default output device, blocking until it finishes.
///
/// Any PCM or float WAV is accepted; multichannel input is averaged to mono, then resampled
/// (linear interpolation) and duplicated across the device's channels. Errors come back as
/// [`AudioError`]: decode failures as `Wav`, device failures as `Playback`, and a stall past the
/// file duration plus five seconds as `Playback`. Nothing is retained after the call.
pub fn play_once(path: &Path) -> Result<(), AudioError> {
    let audio = read_wav_mono_f32(path)?;
    let mut out = OutputSession::open(None)?;
    let format = out.format();
    let mono = resample_linear(&audio.samples, audio.sample_rate, format.sample_rate);
    let samples = expand_channels(&mono, usize::from(format.channels));
    let total = samples.len() as u64;
    let duration = Duration::from_secs_f64(mono.len() as f64 / f64::from(format.sample_rate));
    let deadline = Instant::now() + duration + DEADLINE_SLACK;

    let mut offset = 0;
    while offset < samples.len() {
        if Instant::now() >= deadline {
            return Err(stalled());
        }
        let accepted = out.push_samples(&samples[offset..]);
        if let Some(err) = out.take_last_error() {
            return Err(err);
        }
        offset += accepted;
        if accepted == 0 {
            thread::sleep(POLL);
        }
    }
    while out.consumed_samples() < total {
        if Instant::now() >= deadline {
            return Err(stalled());
        }
        if let Some(err) = out.take_last_error() {
            return Err(err);
        }
        thread::sleep(POLL);
    }
    // The last sample has left the queue but is still in the device's output path. Played
    // position lags by the latency, so wait that out (bounded by the deadline) instead of
    // polling for a position that cannot arrive.
    if let Some(err) = out.take_last_error() {
        return Err(err);
    }
    let now = Instant::now();
    thread::sleep(tail_wait(out.latency(), deadline, now));
    Ok(())
}

/// How long to hold after the queue drains: the reported output latency, capped at the time
/// left before the deadline.
fn tail_wait(latency: Duration, deadline: Instant, now: Instant) -> Duration {
    latency.min(deadline.saturating_duration_since(now))
}

fn stalled() -> AudioError {
    AudioError::Playback("one-shot playback did not finish before its deadline".into())
}

/// Linear-interpolation resample of mono samples. Empty input or a zero rate gives an empty
/// output; equal rates copy the input.
fn resample_linear(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if input.is_empty() || from_rate == 0 || to_rate == 0 {
        return Vec::new();
    }
    if from_rate == to_rate {
        return input.to_vec();
    }
    let step = f64::from(from_rate) / f64::from(to_rate);
    let out_len = (input.len() as f64 / step).round() as usize;
    let last = input.len() - 1;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * step;
            let idx = (pos.floor() as usize).min(last);
            let next = (idx + 1).min(last);
            let frac = (pos - idx as f64) as f32;
            input[idx] + (input[next] - input[idx]) * frac
        })
        .collect()
}

/// Repeat each mono sample across `channels` interleaved channels.
fn expand_channels(mono: &[f32], channels: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(mono.len() * channels);
    for &s in mono {
        out.extend(std::iter::repeat_n(s, channels));
    }
    out
}
