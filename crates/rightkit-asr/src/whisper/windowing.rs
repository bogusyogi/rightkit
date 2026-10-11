//! Whisper's native 30 s window: windows are cut at the quietest 200 ms span
//! in the last 2.24 s, so a word is not split across two decodes. Windows are
//! disjoint (decode `[start, cut)`, next window starts at `cut`), so no text
//! overlap rule is needed and none is applied.
//!
//! Sample offsets are integers: window starts are sample indices, so segment
//! and word sample positions across windows add without float arithmetic.
use super::{DecodeOptions, Segment, WindowDecoder, WindowOutput, WindowRequest, WordSpan};
use crate::error::{Error, Result};

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

/// Decode `audio` window by window. `forced` fixes the language; otherwise it
/// is detected on the first window and locked for the rest. Returns segments
/// and words timed from the start of `audio` (seconds and samples) and the
/// language used.
pub(crate) fn transcribe_windowed(
    decoder: &mut dyn WindowDecoder,
    audio: &[f32],
    forced: Option<String>,
    opts: DecodeOptions,
) -> Result<WindowOutput> {
    let mut lang = forced;
    let mut out = WindowOutput::default();
    let mut start = 0usize;
    loop {
        let remaining = audio.len() - start;
        let end = if remaining <= WHISPER_WINDOW_SAMPLES {
            audio.len()
        } else {
            let hard = start + WHISPER_WINDOW_SAMPLES;
            let target = hard - ms(SEARCH_MS);
            quiet_cut(audio, target, hard).unwrap_or(hard)
        };
        let window = &audio[start..end];
        if !window.is_empty() {
            let req = WindowRequest {
                lang: lang.as_deref(),
                timed: opts.timed,
                token_timestamps: opts.token_timestamps,
                max_len: opts.max_len,
                split_on_word: opts.split_on_word,
            };
            let got = decoder.decode_window(window, &req)?;
            lang = Some(got.language.clone());
            let offset_s = start as f32 / SAMPLE_RATE as f32;
            let offset_n = start as u64;
            out.segments.extend(got.segments.into_iter().map(|s| Segment {
                text: s.text,
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
        }
        if end >= audio.len() {
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
    use crate::whisper::LanguageDetection;

    /// Records window lengths; reports one segment and one word per window.
    struct Mock(Vec<usize>);
    impl WindowDecoder for Mock {
        fn decode_window(&mut self, s: &[f32], req: &WindowRequest<'_>) -> Result<WindowOutput> {
            self.0.push(s.len());
            let lang = req.lang.unwrap_or("fr").to_string();
            Ok(WindowOutput {
                segments: vec![Segment {
                    text: format!("w{}", self.0.len()),
                    start: 0.0,
                    end: s.len() as f32 / 16_000.0,
                    start_sample: 0,
                    end_sample: s.len() as u64,
                }],
                words: vec![WordSpan {
                    text: format!("x{}", self.0.len()),
                    start_sample: 160,
                    end_sample: 320,
                    probability: 0.5,
                }],
                language: lang,
            })
        }
        fn detect_window(&mut self, _s: &[f32]) -> Result<LanguageDetection> {
            Err(Error::Config("mock does not detect".into()))
        }
    }

    const DEFAULT: DecodeOptions =
        DecodeOptions { timed: true, token_timestamps: false, max_len: 0, split_on_word: false };

    #[test]
    fn long_audio_is_cut_in_the_quiet_gap_and_language_locks() {
        // 70 s of tone with a silent gap at ~28.5 s (inside the 27.76-30 s search region).
        let mut audio: Vec<f32> = (0..16_000 * 70).map(|i| ((i as f32) * 0.05).sin() * 0.3).collect();
        for s in &mut audio[16_000 * 28..16_000 * 29] {
            *s = 0.0;
        }
        let mut m = Mock(vec![]);
        let out = transcribe_windowed(&mut m, &audio, None, DEFAULT).unwrap();
        assert_eq!(out.language, "fr");
        assert_eq!(m.0.iter().sum::<usize>(), audio.len(), "windows must tile the audio exactly");
        assert!(m.0.iter().all(|n| *n <= WHISPER_WINDOW_SAMPLES));
        let first_cut = m.0[0] as f32 / 16_000.0;
        assert!((28.0..29.0).contains(&first_cut), "cut {first_cut} not in the silent gap");
        assert!(out.segments.windows(2).all(|w| w[1].start >= w[0].end - 1e-3), "segment offsets must be monotonic");
    }

    #[test]
    fn window_offsets_are_exact_sample_sums() {
        // Two windows: the second segment's sample offset must equal the first window's length exactly.
        let audio = vec![0.1f32; WHISPER_WINDOW_SAMPLES + 40_000];
        let mut m = Mock(vec![]);
        let out = transcribe_windowed(&mut m, &audio, Some("en".into()), DEFAULT).unwrap();
        assert_eq!(m.0.len(), 2);
        let first_len = m.0[0] as u64;
        assert_eq!(out.segments[1].start_sample, first_len, "second window starts at the first window's end");
        assert_eq!(out.segments[1].end_sample, first_len + m.0[1] as u64);
        assert_eq!(out.words[1].start_sample, first_len + 160);
        assert_eq!(out.words[1].end_sample, first_len + 320);
        assert_eq!(out.language, "en");
    }
}
