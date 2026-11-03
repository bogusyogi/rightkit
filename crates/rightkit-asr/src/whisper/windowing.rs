//! Whisper's native 30 s window: windows are cut at the quietest 200 ms span
//! in the last 2.24 s, so a word is not split across two decodes. Windows are
//! disjoint (decode `[start, cut)`, next window starts at `cut`), so no text
//! overlap rule is needed and none is applied.
//!
//! Sample offsets are integers: window starts are sample indices, so segment
//! and word sample positions across windows add without float arithmetic.
use super::control::{
    check_interrupt, deadline_after, earliest, CancelToken, ProgressCallback, ProgressProbe,
};
use super::{
    DecodeOptions, InterruptReason, Segment, WhisperProgress, WindowDecoder, WindowOutput,
    WindowRequest, WordSpan,
};
use crate::error::{Error, Result};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub const WHISPER_WINDOW_SAMPLES: usize = 480_000;
const SAMPLE_RATE: usize = 16_000;
const SEARCH_MS: usize = 2_240;
const QUIET_WIN_MS: usize = 200;
const QUIET_HOP_MS: usize = 100;

pub(crate) fn to_mono_16k(mut audio: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>> {
    if sample_rate as usize != SAMPLE_RATE {
        return Err(Error::Audio(format!(
            "Audio sample rate {sample_rate} doesn't match expected {SAMPLE_RATE}. Please resample your audio first."
        )));
    }
    if channels == 0 {
        return Err(Error::Audio("zero channels".into()));
    }
    if channels > 1 {
        audio = audio
            .chunks(channels as usize)
            .map(|c| c.iter().sum::<f32>() / channels as f32)
            .collect();
    }
    if audio.iter().any(|x| !x.is_finite()) {
        return Err(Error::Audio("non-finite samples".into()));
    }
    Ok(audio)
}

fn ms(n: usize) -> usize {
    SAMPLE_RATE * n / 1000
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

fn quiet_cut(samples: &[f32], lo: usize, hi: usize) -> Option<usize> {
    let (win, hop) = (ms(QUIET_WIN_MS), ms(QUIET_HOP_MS));
    if hi <= lo || hi - lo < win {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    let mut i = lo;
    while i + win <= hi {
        let v = rms(&samples[i..i + win]);
        if best.map(|(_, b)| v < b).unwrap_or(true) {
            best = Some((i + win / 2, v));
        }
        i += hop;
    }
    best.map(|(c, _)| c)
}

/// Caller control for one windowed run (all optional).
pub(crate) struct RunControl<'a> {
    pub cancel: Option<&'a CancelToken>,
    /// Whole-recording bound, absolute.
    pub total_deadline: Option<Instant>,
    /// Bound for each window's decode, measured from that window's start.
    pub window_deadline: Option<Duration>,
    pub progress: Option<&'a ProgressCallback>,
    /// Polled progress sink, updated alongside `progress`.
    pub probe: Option<&'a ProgressProbe>,
}

impl RunControl<'_> {

    /// True when some progress sink (callback or probe) wants events.
    fn reports(&self) -> bool {
        self.progress.is_some() || self.probe.is_some()
    }
}

/// Why `transcribe_windowed` did not return a full result.
#[derive(Debug)]
pub(crate) enum Windowed {
    /// Cancel or deadline: the windows that finished are in `partial`
    /// (`partial.language` is empty when no language was ever known).
    Interrupted {
        reason: InterruptReason,
        partial: WindowOutput,
        windows_completed: usize,
        windows_estimated: usize,
    },
    Failed(Error),
}

impl From<Error> for Windowed {
    fn from(e: Error) -> Self {
        Windowed::Failed(e)
    }
}

/// Windows needed at the nominal 30 s (at least one).
pub(crate) fn estimate_windows(total_samples: usize) -> usize {
    total_samples.div_ceil(WHISPER_WINDOW_SAMPLES).max(1)
}

/// Emit one progress event. `fraction` never decreases within a run: f32 bit
/// patterns of non-negative floats order like the floats, so `fetch_max` on the
/// bits keeps the largest value seen.
fn emit_progress(
    ctl: &RunControl<'_>,
    last_bits: &AtomicU32,
    windows_completed: usize,
    windows_estimated: usize,
    samples_completed: usize,
    samples_total: usize,
) {
    if !ctl.reports() {
        return;
    }
    let raw = if samples_total == 0 { 1.0 } else { samples_completed as f32 / samples_total as f32 };
    let bits = raw.clamp(0.0, 1.0).to_bits();
    let prev = last_bits.fetch_max(bits, Ordering::SeqCst);
    let p = WhisperProgress {
        windows_completed,
        windows_estimated: windows_estimated.max(windows_completed),
        samples_completed: samples_completed.min(samples_total),
        samples_total,
        fraction: f32::from_bits(prev.max(bits)),
    };
    if let Some(probe) = ctl.probe {
        probe.publish(&p);
    }
    if let Some(cb) = ctl.progress {
        cb.call(&p);
    }
}

/// Decode `audio` window by window. `forced` fixes the language; otherwise it
/// is detected on the first window and locked for the rest. Returns segments
/// and words timed from the start of `audio` (seconds and samples) and the
/// language used.
///
/// `ctl` is checked before every window (cancel, then the whole-recording
/// deadline) and handed to the decoder for the window itself. On a stop the
/// windows already finished come back in [`Windowed::Interrupted`].
pub(crate) fn transcribe_windowed(
    decoder: &mut dyn WindowDecoder,
    audio: &[f32],
    forced: Option<String>,
    opts: DecodeOptions,
    ctl: &RunControl<'_>,
) -> std::result::Result<WindowOutput, Windowed> {
    let mut lang = forced;
    let mut out = WindowOutput::default();
    let total = audio.len();
    let estimated = estimate_windows(total);
    let last_bits = AtomicU32::new(0f32.to_bits());
    if let Some(probe) = ctl.probe {
        probe.begin(estimated, total);
    }
    let mut windows_done = 0usize;
    let mut start = 0usize;
    let stop = |reason: InterruptReason, mut out: WindowOutput, lang: Option<String>, done: usize| {
        out.language = lang.unwrap_or_default();
        Windowed::Interrupted { reason, partial: out, windows_completed: done, windows_estimated: estimated }
    };
    loop {
        let remaining = total - start;
        let end = if remaining <= WHISPER_WINDOW_SAMPLES {
            total
        } else {
            let hard = start + WHISPER_WINDOW_SAMPLES;
            let target = hard - ms(SEARCH_MS);
            quiet_cut(audio, target, hard).unwrap_or(hard)
        };
        let window = &audio[start..end];
        if !window.is_empty() {
            if let Some(reason) = check_interrupt(ctl.cancel, ctl.total_deadline) {
                return Err(stop(reason, out, lang, windows_done));
            }
            let window_deadline = earliest(
                deadline_after(Instant::now(), ctl.window_deadline),
                ctl.total_deadline,
            );
            let done_before = windows_done;
            let in_window = |p: f32| {
                let at = start + (p.clamp(0.0, 1.0) * window.len() as f32) as usize;
                emit_progress(ctl, &last_bits, done_before, estimated, at, total);
            };
            let progress: Option<&(dyn Fn(f32) + Sync)> =
                if ctl.reports() { Some(&in_window) } else { None };
            let req = WindowRequest {
                lang: lang.as_deref(),
                timed: opts.timed,
                token_timestamps: opts.token_timestamps,
                max_len: opts.max_len,
                split_on_word: opts.split_on_word,
                beam_size: opts.beam_size,
                best_of: opts.best_of,
                carry_context: opts.carry_context,
                first_window: start == 0,
                cancel: ctl.cancel,
                deadline: window_deadline,
                progress,
            };
            let got = match decoder.decode_window(window, &req) {
                Ok(g) => g,
                Err(Error::Interrupted(i)) => return Err(stop(i.reason, out, lang, windows_done)),
                Err(Error::Cancelled) => return Err(stop(InterruptReason::Cancelled, out, lang, windows_done)),
                Err(e) => return Err(Windowed::Failed(e)),
            };
            lang = Some(got.language.clone());
            let offset_s = start as f32 / SAMPLE_RATE as f32;
            let offset_n = start as u64;
            out.segments.extend(got.segments.into_iter().map(|s| Segment {
                text: s.text,
                raw_text: s.raw_text,
                start: s.start + offset_s,
                end: s.end + offset_s,
                start_sample: s.start_sample + offset_n,
                end_sample: s.end_sample + offset_n,
            }));
            out.words.extend(got.words.into_iter().map(|w| WordSpan {
                text: w.text,
                start_sample: w.start_sample + offset_n,
                end_sample: w.end_sample + offset_n,
                probability: w.probability,
            }));
            windows_done += 1;
            let est = if end >= total { windows_done } else { estimated };
            emit_progress(ctl, &last_bits, windows_done, est, end, total);
        }
        if end >= total {
            break;
        }
        start = end;
    }
    out.language = lang.unwrap_or_else(|| "en".into());
    Ok(out)
}
