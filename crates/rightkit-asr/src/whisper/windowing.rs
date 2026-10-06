//! Whisper's native 30 s window: windows are cut at the quietest 200 ms span
//! in the last 2.24 s, so a word is not split across two decodes. Windows are
//! disjoint (decode `[start, cut)`, next window starts at `cut`), so no text
//! overlap rule is needed and none is applied.
use super::{Segment, WindowDecoder};
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
/// timed from the start of `audio` and the language used.
pub(crate) fn transcribe_windowed(
    decoder: &mut dyn WindowDecoder,
    audio: &[f32],
    forced: Option<String>,
) -> Result<(Vec<Segment>, String)> {
    let mut lang = forced;
    let mut out = Vec::new();
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
            let (segs, used) = decoder.decode_window(window, lang.as_deref())?;
            lang = Some(used);
            let offset = start as f32 / SAMPLE_RATE as f32;
            out.extend(segs.into_iter().map(|s| Segment {
                text: s.text,
                start: s.start + offset,
                end: s.end + offset,
            }));
        }
        if end >= audio.len() {
            break;
        }
        start = end;
    }
    Ok((out, lang.unwrap_or_else(|| "en".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mock(Vec<usize>);
    impl WindowDecoder for Mock {
        fn decode_window(&mut self, s: &[f32], lang: Option<&str>) -> Result<(Vec<Segment>, String)> {
            self.0.push(s.len());
            Ok((
                vec![Segment { text: format!("w{}", self.0.len()), start: 0.0, end: s.len() as f32 / 16_000.0 }],
                lang.unwrap_or("fr").to_string(),
            ))
        }
    }

    #[test]
    fn long_audio_is_cut_in_the_quiet_gap_and_language_locks() {
        // 70 s of tone with a silent gap at ~28.5 s (inside the 27.76-30 s search region).
        let mut audio: Vec<f32> = (0..16_000 * 70).map(|i| ((i as f32) * 0.05).sin() * 0.3).collect();
        for s in &mut audio[16_000 * 28..16_000 * 29] {
            *s = 0.0;
        }
        let mut m = Mock(vec![]);
        let (segs, lang) = transcribe_windowed(&mut m, &audio, None).unwrap();
        assert_eq!(lang, "fr");
        assert_eq!(m.0.iter().sum::<usize>(), audio.len(), "windows must tile the audio exactly");
        assert!(m.0.iter().all(|n| *n <= WHISPER_WINDOW_SAMPLES));
        let first_cut = m.0[0] as f32 / 16_000.0;
        assert!((28.0..29.0).contains(&first_cut), "cut {first_cut} not in the silent gap");
        assert!(segs.windows(2).all(|w| w[1].start >= w[0].end - 1e-3), "segment offsets must be monotonic");
    }
}
