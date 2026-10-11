//! Whisper's native 30 s window: windows are cut at the quietest 200 ms span
//! in the last 2.24 s, so a word is not split across two decodes. Windows are
//! disjoint (decode `[start, cut)`, next window starts at `cut`), so no text
//! overlap rule is needed and none is applied.
//!
//! Sample offsets are integers: window starts are sample indices, so segment
//! and word sample positions across windows add without float arithmetic.
use super::control::{check_interrupt, deadline_after, earliest, CancelToken, ProgressCallback};
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
}

impl RunControl<'_> {
    pub fn none() -> Self {
        RunControl { cancel: None, total_deadline: None, window_deadline: None, progress: None }
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
    cb: &ProgressCallback,
    last_bits: &AtomicU32,
    windows_completed: usize,
    windows_estimated: usize,
    samples_completed: usize,
    samples_total: usize,
) {
    let raw = if samples_total == 0 { 1.0 } else { samples_completed as f32 / samples_total as f32 };
    let bits = raw.clamp(0.0, 1.0).to_bits();
    let prev = last_bits.fetch_max(bits, Ordering::SeqCst);
    cb.call(&WhisperProgress {
        windows_completed,
        windows_estimated: windows_estimated.max(windows_completed),
        samples_completed: samples_completed.min(samples_total),
        samples_total,
        fraction: f32::from_bits(prev.max(bits)),
    });
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
                if let Some(cb) = ctl.progress {
                    let at = start + (p.clamp(0.0, 1.0) * window.len() as f32) as usize;
                    emit_progress(cb, &last_bits, done_before, estimated, at, total);
                }
            };
            let progress: Option<&(dyn Fn(f32) + Sync)> =
                if ctl.progress.is_some() { Some(&in_window) } else { None };
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
            if let Some(cb) = ctl.progress {
                let est = if end >= total { windows_done } else { estimated };
                emit_progress(cb, &last_bits, windows_done, est, end, total);
            }
        }
        if end >= total {
            break;
        }
        start = end;
    }
    out.language = lang.unwrap_or_else(|| "en".into());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whisper::control::interrupted_error;
    use crate::whisper::LanguageDetection;
    use std::sync::{Arc, Mutex};

    /// Fake decoder (the test seam for the windowing layer). Records window
    /// lengths and reports one segment and one word per window. Behaviour knobs:
    /// - `cancel_in_window`: trip this token inside the given 1-based window,
    ///   the way the abort flag fires mid `whisper_full`, then return the
    ///   placeholder interruption a real decoder returns.
    /// - `deadline_in_window`: return a deadline interruption in that window.
    /// - `sleep`: sleep inside each window (lets a wall-clock deadline pass).
    /// - `progress_points`: in-window progress fractions to report.
    #[derive(Default)]
    struct Fake {
        lens: Vec<usize>,
        seen: Vec<(bool, bool, bool)>, // (first_window, has_cancel, has_deadline)
        cancel_in_window: Option<(usize, CancelToken)>,
        deadline_in_window: Option<usize>,
        sleep: Option<Duration>,
        progress_points: Vec<f32>,
    }
    impl WindowDecoder for Fake {
        fn decode_window(&mut self, s: &[f32], req: &WindowRequest<'_>) -> Result<WindowOutput> {
            self.lens.push(s.len());
            let n = self.lens.len();
            self.seen.push((req.first_window, req.cancel.is_some(), req.deadline.is_some()));
            if let Some(d) = self.sleep {
                std::thread::sleep(d);
            }
            if let Some(f) = req.progress {
                for p in &self.progress_points {
                    f(*p);
                }
            }
            if let Some((at, token)) = &self.cancel_in_window {
                if *at == n {
                    token.cancel();
                    assert!(req.cancel.is_some_and(CancelToken::is_cancelled), "the decoder sees the same flag");
                    return Err(interrupted_error(InterruptReason::Cancelled));
                }
            }
            if self.deadline_in_window == Some(n) {
                return Err(interrupted_error(InterruptReason::DeadlineExceeded));
            }
            Ok(WindowOutput {
                segments: vec![Segment {
                    text: format!("w{n}"),
                    raw_text: format!(" w{n}"),
                    start: 0.0,
                    end: s.len() as f32 / 16_000.0,
                    start_sample: 0,
                    end_sample: s.len() as u64,
                }],
                words: vec![WordSpan {
                    text: format!("x{n}"),
                    start_sample: 160,
                    end_sample: 320,
                    probability: 0.5,
                }],
                language: req.lang.unwrap_or("fr").to_string(),
            })
        }
        fn detect_window(&mut self, _s: &[f32]) -> Result<LanguageDetection> {
            Err(Error::Config("fake does not detect".into()))
        }
    }

    const DEFAULT: DecodeOptions = DecodeOptions {
        timed: true,
        token_timestamps: false,
        max_len: 0,
        split_on_word: false,
        beam_size: 1,
        best_of: 1,
        carry_context: false,
    };

    fn tone(secs: usize) -> Vec<f32> {
        (0..16_000 * secs).map(|i| ((i as f32) * 0.05).sin() * 0.3).collect()
    }

    fn run(m: &mut Fake, audio: &[f32], ctl: &RunControl<'_>) -> std::result::Result<WindowOutput, Windowed> {
        transcribe_windowed(m, audio, None, DEFAULT, ctl)
    }

    #[test]
    fn long_audio_is_cut_in_the_quiet_gap_and_language_locks() {
        // 70 s of tone with a silent gap at ~28.5 s (inside the 27.76-30 s search region).
        let mut audio = tone(70);
        for s in &mut audio[16_000 * 28..16_000 * 29] {
            *s = 0.0;
        }
        let mut m = Fake::default();
        let out = run(&mut m, &audio, &RunControl::none()).unwrap();
        assert_eq!(out.language, "fr");
        assert_eq!(m.lens.iter().sum::<usize>(), audio.len(), "windows must tile the audio exactly");
        assert!(m.lens.iter().all(|n| *n <= WHISPER_WINDOW_SAMPLES));
        let first_cut = m.lens[0] as f32 / 16_000.0;
        assert!((28.0..29.0).contains(&first_cut), "cut {first_cut} not in the silent gap");
        assert!(out.segments.windows(2).all(|w| w[1].start >= w[0].end - 1e-3), "segment offsets must be monotonic");
        assert_eq!(m.seen.iter().filter(|s| s.0).count(), 1, "exactly the first window is marked first");
        assert!(m.seen[0].0);
    }

    #[test]
    fn window_offsets_are_exact_sample_sums() {
        // Two windows: the second segment's sample offset must equal the first window's length exactly.
        let audio = vec![0.1f32; WHISPER_WINDOW_SAMPLES + 40_000];
        let mut m = Fake::default();
        let out =
            transcribe_windowed(&mut m, &audio, Some("en".into()), DEFAULT, &RunControl::none()).unwrap();
        assert_eq!(m.lens.len(), 2);
        let first_len = m.lens[0] as u64;
        assert_eq!(out.segments[1].start_sample, first_len, "second window starts at the first window's end");
        assert_eq!(out.segments[1].end_sample, first_len + m.lens[1] as u64);
        assert_eq!(out.words[1].start_sample, first_len + 160);
        assert_eq!(out.words[1].end_sample, first_len + 320);
        assert_eq!(out.language, "en");
        assert_eq!(out.segments[1].raw_text, " w2", "raw text is carried through untouched");
    }

    fn expect_interrupted(r: std::result::Result<WindowOutput, Windowed>) -> (InterruptReason, WindowOutput, usize, usize) {
        match r {
            Err(Windowed::Interrupted { reason, partial, windows_completed, windows_estimated }) => {
                (reason, partial, windows_completed, windows_estimated)
            }
            other => panic!("expected an interruption, got {other:?}"),
        }
    }

    #[test]
    fn cancel_between_windows_keeps_completed_windows() {
        // 70 s -> three windows. Trip the token after window 1 finishes: the
        // check before window 2 stops the run without calling the decoder again.
        let audio = tone(70);
        let token = CancelToken::new();
        let tripped = Arc::new(Mutex::new(false));
        let t2 = token.clone();
        let flag = Arc::clone(&tripped);
        let progress = ProgressCallback::new(move |p| {
            if p.windows_completed == 1 {
                t2.cancel();
                *flag.lock().unwrap() = true;
            }
        });
        let ctl = RunControl { cancel: Some(&token), progress: Some(&progress), ..RunControl::none() };
        let mut m = Fake::default();
        let (reason, partial, done, est) = expect_interrupted(run(&mut m, &audio, &ctl));
        assert_eq!(reason, InterruptReason::Cancelled);
        assert!(*tripped.lock().unwrap());
        assert_eq!(m.lens.len(), 1, "no decode after the cancel");
        assert_eq!(done, 1);
        assert_eq!(est, 3);
        assert_eq!(partial.segments.len(), 1);
        assert_eq!(partial.segments[0].text, "w1");
        assert_eq!(partial.words.len(), 1);
        assert_eq!(partial.language, "fr", "the language locked by the finished window is reported");
    }

    #[test]
    fn cancel_before_the_first_window_returns_an_empty_partial_without_decoding() {
        let token = CancelToken::new();
        token.cancel();
        let ctl = RunControl { cancel: Some(&token), ..RunControl::none() };
        let mut m = Fake::default();
        let (reason, partial, done, _) = expect_interrupted(run(&mut m, &tone(5), &ctl));
        assert_eq!(reason, InterruptReason::Cancelled);
        assert!(m.lens.is_empty());
        assert_eq!(done, 0);
        assert!(partial.segments.is_empty());
        assert_eq!(partial.language, "", "no window finished and none was forced");
    }

    #[test]
    fn cancel_mid_window_via_the_abort_flag_keeps_earlier_windows_only() {
        // The fake trips the token inside window 2 and reports the abort like the
        // whisper.cpp decoder does. Window 1 stays; window 2's work is dropped.
        let audio = tone(70);
        let token = CancelToken::new();
        let ctl = RunControl { cancel: Some(&token), ..RunControl::none() };
        let mut m = Fake { cancel_in_window: Some((2, token.clone())), ..Fake::default() };
        let (reason, partial, done, _) = expect_interrupted(run(&mut m, &audio, &ctl));
        assert_eq!(reason, InterruptReason::Cancelled);
        assert_eq!(m.lens.len(), 2, "window 3 never runs");
        assert_eq!(done, 1);
        assert_eq!(partial.segments.len(), 1);
        assert_eq!(partial.segments[0].text, "w1");
    }

    #[test]
    fn deadline_mid_window_keeps_the_windows_that_finished() {
        let audio = tone(70);
        let mut m = Fake { deadline_in_window: Some(3), ..Fake::default() };
        let (reason, partial, done, est) = expect_interrupted(run(&mut m, &audio, &RunControl::none()));
        assert_eq!(reason, InterruptReason::DeadlineExceeded);
        assert_eq!(done, 2);
        assert_eq!(est, 3);
        let texts: Vec<_> = partial.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["w1", "w2"]);
        assert!(partial.segments[1].start_sample > 0, "kept windows keep their absolute offsets");
    }

    #[test]
    fn whole_recording_deadline_stops_between_windows_and_keeps_partial() {
        // Each fake window takes ~60 ms; the whole-recording bound is 20 ms from
        // now, so the check before window 2 fires.
        let audio = tone(70);
        let ctl = RunControl {
            total_deadline: Some(Instant::now() + Duration::from_millis(20)),
            ..RunControl::none()
        };
        let mut m = Fake { sleep: Some(Duration::from_millis(60)), ..Fake::default() };
        let (reason, partial, done, _) = expect_interrupted(run(&mut m, &audio, &ctl));
        assert_eq!(reason, InterruptReason::DeadlineExceeded);
        assert_eq!(m.lens.len(), 1);
        assert_eq!(done, 1);
        assert_eq!(partial.segments.len(), 1);
    }

    #[test]
    fn decoder_receives_the_earlier_of_window_and_total_deadline() {
        let audio = vec![0.1f32; 16_000];
        let mut m = Fake::default();
        let far = Instant::now() + Duration::from_secs(3_600);
        let ctl = RunControl {
            total_deadline: Some(far),
            window_deadline: Some(Duration::from_secs(10)),
            ..RunControl::none()
        };
        run(&mut m, &audio, &ctl).unwrap();
        assert!(m.seen[0].2);
        let mut m = Fake::default();
        run(&mut m, &audio, &RunControl::none()).unwrap();
        assert!(!m.seen[0].2, "no bound anywhere means no deadline in the request");
    }

    #[test]
    fn non_interruption_errors_stay_plain_failures() {
        struct Bad;
        impl WindowDecoder for Bad {
            fn decode_window(&mut self, _: &[f32], _: &WindowRequest<'_>) -> Result<WindowOutput> {
                Err(Error::Model("boom".into()))
            }
            fn detect_window(&mut self, _: &[f32]) -> Result<LanguageDetection> {
                unreachable!()
            }
        }
        let r = transcribe_windowed(&mut Bad, &tone(2), None, DEFAULT, &RunControl::none());
        assert!(matches!(r, Err(Windowed::Failed(Error::Model(_)))));
    }

    #[test]
    fn progress_is_ordered_monotonic_and_ends_at_one() {
        let audio = tone(70);
        let events = Arc::new(Mutex::new(Vec::<WhisperProgress>::new()));
        let sink = Arc::clone(&events);
        let progress = ProgressCallback::new(move |p| sink.lock().unwrap().push(*p));
        let ctl = RunControl { progress: Some(&progress), ..RunControl::none() };
        let mut m = Fake { progress_points: vec![0.25, 0.75], ..Fake::default() };
        run(&mut m, &audio, &ctl).unwrap();
        let ev = events.lock().unwrap().clone();
        // Per window: two in-window events then one completion event.
        assert_eq!(ev.len(), 3 * 3);
        assert!(ev.windows(2).all(|w| w[1].fraction >= w[0].fraction), "fraction never decreases: {ev:?}");
        assert!(ev.windows(2).all(|w| w[1].windows_completed >= w[0].windows_completed));
        assert!(ev.windows(2).all(|w| w[1].samples_completed >= w[0].samples_completed));
        let completions: Vec<usize> = ev.iter().map(|e| e.windows_completed).collect();
        assert_eq!(completions, [0, 0, 1, 1, 1, 2, 2, 2, 3]);
        let last = ev.last().unwrap();
        assert_eq!(last.fraction, 1.0);
        assert_eq!(last.samples_completed, audio.len());
        assert_eq!(last.windows_estimated, last.windows_completed, "exact once the last window is done");
        assert!(ev[0].fraction > 0.0 && ev[0].fraction < 0.5);
        assert!(ev.iter().all(|e| e.samples_total == audio.len()));
    }

    #[test]
    fn progress_is_not_requested_from_the_decoder_when_unused() {
        struct Probe(bool);
        impl WindowDecoder for Probe {
            fn decode_window(&mut self, _: &[f32], req: &WindowRequest<'_>) -> Result<WindowOutput> {
                self.0 = req.progress.is_some();
                Ok(WindowOutput { language: "en".into(), ..Default::default() })
            }
            fn detect_window(&mut self, _: &[f32]) -> Result<LanguageDetection> {
                unreachable!()
            }
        }
        let mut p = Probe(true);
        transcribe_windowed(&mut p, &tone(1), None, DEFAULT, &RunControl::none()).unwrap();
        assert!(!p.0);
    }

    #[test]
    fn estimate_windows_rounds_up_and_is_at_least_one() {
        assert_eq!(estimate_windows(0), 1);
        assert_eq!(estimate_windows(1), 1);
        assert_eq!(estimate_windows(WHISPER_WINDOW_SAMPLES), 1);
        assert_eq!(estimate_windows(WHISPER_WINDOW_SAMPLES + 1), 2);
    }
}
