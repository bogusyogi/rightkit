use crate::{FrameScorer, Result, FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE};

/// Hysteresis/padding policy (Silero `get_speech_timestamps` shape; the
/// CutRight-proven policy, which is a strict superset of ScrapeRight's).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    /// Score at or above which speech starts.
    pub threshold: f32,
    /// Speech continues while score stays at or above this.
    pub neg_threshold: f32,
    pub min_speech_ms: u64,
    pub min_silence_ms: u64,
    pub pad_ms: u64,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            neg_threshold: 0.35,
            min_speech_ms: 100,
            min_silence_ms: 120,
            pad_ms: 30,
        }
    }
}

impl VadConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        let ok = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if !ok(self.threshold) || !ok(self.neg_threshold) || self.neg_threshold > self.threshold {
            return Err(crate::Error::Input(
                "thresholds must be in [0,1] with neg_threshold <= threshold".into(),
            ));
        }
        Ok(())
    }
}

/// A speech span in source milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Span {
    pub fn len_ms(&self) -> u64 {
        self.end_ms - self.start_ms
    }
    /// Sample range at 16 kHz, clamped to `len`.
    pub fn sample_range(&self, len: usize) -> std::ops::Range<usize> {
        let s = (self.start_ms * SAMPLE_RATE as u64 / 1000) as usize;
        let e = (self.end_ms * SAMPLE_RATE as u64 / 1000) as usize;
        s.min(len)..e.min(len)
    }
}

/// Score every frame of `samples` (final partial frame zero-padded) and turn
/// the scores into spans.
pub fn detect(scorer: &mut dyn FrameScorer, samples: &[f32], cfg: &VadConfig) -> Result<Vec<Span>> {
    cfg.validate()?;
    let scores = score_samples(scorer, samples)?;
    Ok(scores_to_spans(&scores, cfg, duration_ms(samples.len())))
}

/// Reset `scorer`, then score every frame of `samples` (final partial frame
/// zero-padded). Rejects non-finite samples before touching the scorer, and a
/// non-finite score with `Error::Model`, as `Silero::score` does for its own output.
pub(crate) fn score_samples(scorer: &mut dyn FrameScorer, samples: &[f32]) -> Result<Vec<f32>> {
    ensure_finite(samples)?;
    scorer.reset();
    let mut scores = Vec::with_capacity(samples.len() / FRAME_SAMPLES + 1);
    let mut frame = [0f32; FRAME_SAMPLES];
    for chunk in samples.chunks(FRAME_SAMPLES) {
        frame.fill(0.0);
        frame[..chunk.len()].copy_from_slice(chunk);
        scores.push(ensure_finite_score(scorer.score(&frame)?)?);
    }
    Ok(scores)
}

/// `Err(Model)` when a frame score is NaN or infinite. Shared by `score_samples`
/// and `FrameStream::push`, so both paths reject the same scores.
pub(crate) fn ensure_finite_score(score: f32) -> Result<f32> {
    if !score.is_finite() {
        return Err(crate::Error::Model(format!(
            "non-finite frame score {score}"
        )));
    }
    Ok(score)
}

/// `Err(Input)` when any sample is NaN or infinite.
pub(crate) fn ensure_finite(samples: &[f32]) -> Result<()> {
    if samples.iter().any(|x| !x.is_finite()) {
        return Err(crate::Error::Input("non-finite samples".into()));
    }
    Ok(())
}

/// Duration of `len` samples at 16 kHz, in whole milliseconds.
pub(crate) fn duration_ms(len: usize) -> u64 {
    len as u64 * 1000 / SAMPLE_RATE as u64
}

/// Turn per-frame scores into padded, merged spans.
pub fn scores_to_spans(scores: &[f32], cfg: &VadConfig, duration_ms: u64) -> Vec<Span> {
    let min_silence_frames = cfg.min_silence_ms.div_ceil(FRAME_MS).max(1) as usize;
    let mut raw: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_speech = 0usize;
    let mut silent_run = 0usize;
    for (i, &s) in scores.iter().enumerate() {
        match start {
            None if s >= cfg.threshold => {
                start = Some(i);
                last_speech = i + 1;
                silent_run = 0;
            }
            None => {}
            Some(st) => {
                if s >= cfg.neg_threshold {
                    last_speech = i + 1;
                    silent_run = 0;
                } else {
                    silent_run += 1;
                    if silent_run >= min_silence_frames {
                        raw.push((st, last_speech));
                        start = None;
                    }
                }
            }
        }
    }
    if let Some(st) = start {
        raw.push((st, last_speech.max(st + 1)));
    }
    let min_speech_frames = cfg.min_speech_ms.div_ceil(FRAME_MS).max(1) as usize;
    let mut spans: Vec<Span> = Vec::new();
    for (s, e) in raw {
        if e - s < min_speech_frames {
            continue;
        }
        let start_ms = (s as u64 * FRAME_MS).saturating_sub(cfg.pad_ms);
        let end_ms = (e as u64 * FRAME_MS + cfg.pad_ms).min(duration_ms);
        if end_ms <= start_ms {
            continue;
        }
        match spans.last_mut() {
            Some(prev) if start_ms <= prev.end_ms => prev.end_ms = prev.end_ms.max(end_ms),
            _ => spans.push(Span { start_ms, end_ms }),
        }
    }
    spans
}

/// Energy + zero-crossing pseudo-scores (1.0 speech / 0.0 not) for callers that
/// need a model-free fallback; flows through the same hysteresis.
pub fn energy_scores(samples: &[f32]) -> Vec<f32> {
    let rms = |f: &[f32]| (f.iter().map(|x| x * x).sum::<f32>() / f.len().max(1) as f32).sqrt();
    let frames: Vec<&[f32]> = samples.chunks(FRAME_SAMPLES).collect();
    if frames.is_empty() {
        return Vec::new();
    }
    let levels: Vec<f32> = frames.iter().map(|f| rms(f)).collect();
    let mut sorted = levels.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let floor = (sorted[sorted.len() / 10] * 4.0).max(0.003_162);
    frames
        .iter()
        .zip(&levels)
        .map(|(frame, &level)| {
            let crossings = frame
                .windows(2)
                .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
                .count();
            let zcr = crossings as f32 / frame.len().max(1) as f32;
            if level > floor && !(zcr > 0.35 && level < floor * 2.0) {
                1.0
            } else {
                0.0
            }
        })
        .collect()
}

/// Decode-chunking knobs. Policy-free: a chunk is just "audio worth decoding
/// in one call".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkConfig {
    /// Spans separated by less than this are merged into one chunk.
    pub merge_gap_ms: u64,
    /// Chunks longer than this are split (at the lowest-score frame near the
    /// limit when scores are supplied, else hard).
    pub max_chunk_ms: u64,
    /// Chunks shorter than this are dropped.
    pub min_chunk_ms: u64,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            merge_gap_ms: 300,
            max_chunk_ms: 30_000,
            min_chunk_ms: 200,
        }
    }
}

/// Merge near spans and split over-long ones. With `scores` the split lands on
/// the quietest frame in the last quarter of the window instead of mid-word.
pub fn chunk_spans(spans: &[Span], cfg: &ChunkConfig, scores: Option<&[f32]>) -> Vec<Span> {
    let max = cfg.max_chunk_ms.max(FRAME_MS * 4);
    let mut merged: Vec<Span> = Vec::new();
    for &s in spans {
        match merged.last_mut() {
            Some(p)
                if s.start_ms.saturating_sub(p.end_ms) < cfg.merge_gap_ms
                    && s.end_ms - p.start_ms <= max =>
            {
                p.end_ms = s.end_ms
            }
            _ => merged.push(s),
        }
    }
    let mut out = Vec::new();
    for s in merged {
        let mut start = s.start_ms;
        while s.end_ms - start > max {
            let hard = start + max;
            let lo = hard - max / 4;
            let cut = scores
                .and_then(|sc| {
                    (lo / FRAME_MS..=hard / FRAME_MS)
                        .filter_map(|f| sc.get(f as usize).map(|v| (f, *v)))
                        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                        .map(|(f, _)| f * FRAME_MS)
                })
                .filter(|c| *c > start && *c < hard + FRAME_MS)
                .unwrap_or(hard);
            out.push(Span {
                start_ms: start,
                end_ms: cut,
            });
            start = cut;
        }
        out.push(Span {
            start_ms: start,
            end_ms: s.end_ms,
        });
    }
    out.retain(|s| s.len_ms() >= cfg.min_chunk_ms);
    out
}

/// Speech thresholds for span detection, optionally calibrated from ambient
/// audio. `resolve` turns these into a [`VadConfig`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpanOptions {
    /// Score at or above which speech starts (`VadConfig::threshold`).
    pub speech_threshold: f32,
    /// Speech continues while the score stays at or above this (`VadConfig::neg_threshold`).
    pub silence_threshold: f32,
    /// When set, the speech threshold comes from ambient RMS instead of `speech_threshold`.
    pub rms_calibration: Option<RmsCalibration>,
    /// With calibration, cap the silence threshold at the calibrated speech threshold
    /// (default `true`, the behaviour before this option). `false` passes the silence
    /// threshold through unchanged. A silence threshold above the calibrated one then
    /// fails [`VadConfig::validate`] with an `Input` error; nothing is capped silently.
    pub cap_silence_at_calibrated: bool,
}

impl Default for SpanOptions {
    /// The same thresholds as [`VadConfig::default`], without calibration. The silence
    /// cap is on, as it was before the option existed.
    fn default() -> Self {
        Self {
            speech_threshold: 0.5,
            silence_threshold: 0.35,
            rms_calibration: None,
            cap_silence_at_calibrated: true,
        }
    }
}

/// Speech threshold from measured ambient RMS:
/// `threshold = clamp(max(minimum_threshold, max(rms, noise_floor) * multiplier), 0, 1)`.
/// The defaults are HeardRight's: 8x RMS, a 0.005 noise floor, and a 0.20 minimum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RmsCalibration {
    pub multiplier: f32,
    /// Lower bound on the RMS used, so a silent room cannot drive the threshold to zero.
    pub noise_floor: f32,
    /// Lower bound on the resulting threshold (HeardRight's runtime floor).
    pub minimum_threshold: f32,
}

impl Default for RmsCalibration {
    fn default() -> Self {
        Self {
            multiplier: 8.0,
            noise_floor: 0.005,
            minimum_threshold: 0.20,
        }
    }
}

/// [`SpanOptions`] resolved against ambient audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedSpans {
    /// The thresholds to pass to [`detect`] or `scores_to_spans`.
    pub config: VadConfig,
    /// The RMS used for calibration (after the noise floor), when calibration ran.
    pub noise_floor_rms: Option<f32>,
}

impl SpanOptions {
    /// Build the detection config. `base` supplies padding and duration policy.
    ///
    /// With `rms_calibration`, `ambient` must be non-empty, finite audio that
    /// holds no speech; anything else is an error, not a silent uncalibrated run.
    /// The calibrated threshold replaces `speech_threshold`. The silence threshold is
    /// capped at it so the hysteresis stays valid, unless `cap_silence_at_calibrated`
    /// is off, in which case an uncapped silence threshold above it is an error.
    pub fn resolve(&self, base: VadConfig, ambient: Option<&[f32]>) -> Result<ResolvedSpans> {
        let mut config = VadConfig {
            threshold: self.speech_threshold,
            neg_threshold: self.silence_threshold,
            ..base
        };
        let mut noise_floor_rms = None;
        if let Some(cal) = self.rms_calibration {
            let ambient = ambient.filter(|a| !a.is_empty()).ok_or_else(|| {
                crate::Error::Input("rms calibration needs non-empty ambient samples".into())
            })?;
            ensure_finite(ambient)?;
            let rms = rms(ambient).max(cal.noise_floor);
            config.threshold = cal
                .minimum_threshold
                .max(rms * cal.multiplier)
                .clamp(0.0, 1.0);
            if self.cap_silence_at_calibrated {
                config.neg_threshold = config.neg_threshold.min(config.threshold);
            }
            noise_floor_rms = Some(rms);
        }
        config.validate()?;
        Ok(ResolvedSpans {
            config,
            noise_floor_rms,
        })
    }
}

/// Root-mean-square level of `samples`; zero for an empty slice.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    // Accumulate in f64: a sequential f32 sum over a second of audio drifts by about 1e-5,
    // enough to move the calibrated threshold in its fifth decimal.
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod span_options_tests {
    use super::*;

    fn base() -> VadConfig {
        VadConfig::default()
    }

    #[test]
    fn default_options_resolve_to_the_default_config() {
        let resolved = SpanOptions::default().resolve(base(), None).unwrap();
        assert_eq!(resolved.config, VadConfig::default());
        assert_eq!(resolved.noise_floor_rms, None);
    }

    #[test]
    fn calibrated_threshold_is_eight_times_rms() {
        let ambient = vec![0.05f32; 16_000];
        let opts = SpanOptions {
            rms_calibration: Some(RmsCalibration::default()),
            ..SpanOptions::default()
        };
        let resolved = opts.resolve(base(), Some(&ambient)).unwrap();
        assert!(
            (resolved.config.threshold - 0.40).abs() < 1e-6,
            "{resolved:?}"
        );
        assert!((resolved.noise_floor_rms.unwrap() - 0.05).abs() < 1e-6);
    }

    #[test]
    fn calibration_never_drops_below_the_minimum() {
        // RMS 0.001 is floored to 0.005, so 8x gives 0.04, below the 0.20 minimum.
        let ambient = vec![0.001f32; 16_000];
        let opts = SpanOptions {
            rms_calibration: Some(RmsCalibration::default()),
            ..SpanOptions::default()
        };
        let resolved = opts.resolve(base(), Some(&ambient)).unwrap();
        assert!((resolved.config.threshold - 0.20).abs() < 1e-6);
        assert!((resolved.noise_floor_rms.unwrap() - 0.005).abs() < 1e-6);
    }

    #[test]
    fn loud_ambient_clamps_the_threshold_to_one() {
        let ambient = vec![0.5f32; 16_000];
        let opts = SpanOptions {
            rms_calibration: Some(RmsCalibration::default()),
            ..SpanOptions::default()
        };
        let resolved = opts.resolve(base(), Some(&ambient)).unwrap();
        assert_eq!(resolved.config.threshold, 1.0);
        // The silence threshold is never above the speech threshold.
        assert!(resolved.config.neg_threshold <= resolved.config.threshold);
    }

    #[test]
    fn silence_threshold_is_capped_at_the_calibrated_speech_threshold() {
        let ambient = vec![0.001f32; 16_000]; // calibrates to 0.20
        let opts = SpanOptions {
            silence_threshold: 0.35,
            rms_calibration: Some(RmsCalibration::default()),
            ..SpanOptions::default()
        };
        let resolved = opts.resolve(base(), Some(&ambient)).unwrap();
        assert!((resolved.config.threshold - 0.20).abs() < 1e-6);
        assert!((resolved.config.neg_threshold - 0.20).abs() < 1e-6);
    }

    #[test]
    fn calibration_refuses_missing_empty_or_non_finite_ambient() {
        let opts = SpanOptions {
            rms_calibration: Some(RmsCalibration::default()),
            ..SpanOptions::default()
        };
        assert!(opts.resolve(base(), None).is_err());
        assert!(opts.resolve(base(), Some(&[])).is_err());
        assert!(opts.resolve(base(), Some(&[0.1, f32::NAN])).is_err());
    }

    #[test]
    fn inverted_thresholds_are_rejected() {
        let opts = SpanOptions {
            speech_threshold: 0.3,
            silence_threshold: 0.5,
            rms_calibration: None,
            ..SpanOptions::default()
        };
        assert!(opts.resolve(base(), None).is_err());
    }

    #[test]
    fn silence_cap_is_on_by_default() {
        assert!(SpanOptions::default().cap_silence_at_calibrated);
    }

    #[test]
    fn disabling_the_cap_keeps_a_silence_threshold_below_the_calibrated_one() {
        // Calibrates to 0.20; a silence threshold under that is valid uncapped too.
        let ambient = vec![0.001f32; 16_000];
        let opts = SpanOptions {
            silence_threshold: 0.15,
            rms_calibration: Some(RmsCalibration::default()),
            cap_silence_at_calibrated: false,
            ..SpanOptions::default()
        };
        let resolved = opts.resolve(base(), Some(&ambient)).unwrap();
        assert!((resolved.config.threshold - 0.20).abs() < 1e-6);
        assert!((resolved.config.neg_threshold - 0.15).abs() < 1e-6);
    }

    #[test]
    fn disabling_the_cap_refuses_a_silence_threshold_above_the_calibrated_one() {
        // Calibrates to 0.20. With the cap off, 0.35 would invert the hysteresis, so
        // resolve refuses it rather than capping it silently.
        let ambient = vec![0.001f32; 16_000];
        let opts = SpanOptions {
            silence_threshold: 0.35,
            rms_calibration: Some(RmsCalibration::default()),
            cap_silence_at_calibrated: false,
            ..SpanOptions::default()
        };
        assert!(matches!(
            opts.resolve(base(), Some(&ambient)),
            Err(crate::Error::Input(_))
        ));
        // The default still caps the same input.
        let capped = SpanOptions {
            cap_silence_at_calibrated: true,
            ..opts
        };
        let resolved = capped.resolve(base(), Some(&ambient)).unwrap();
        assert!((resolved.config.neg_threshold - 0.20).abs() < 1e-6);
    }
}

#[cfg(test)]
mod score_samples_tests {
    use super::*;

    /// A mock scorer that returns a fixed script, one score per frame.
    struct Scripted {
        scores: Vec<f32>,
        next: usize,
    }

    impl Scripted {
        fn new(scores: &[f32]) -> Self {
            Self {
                scores: scores.to_vec(),
                next: 0,
            }
        }
    }

    impl FrameScorer for Scripted {
        fn reset(&mut self) {
            self.next = 0;
        }
        fn score(&mut self, _frame: &[f32; FRAME_SAMPLES]) -> Result<f32> {
            let score = self.scores[self.next];
            self.next += 1;
            Ok(score)
        }
    }

    #[test]
    fn non_finite_generic_score_is_a_model_error() {
        let samples = vec![0.0f32; 3 * FRAME_SAMPLES];
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut scorer = Scripted::new(&[0.9, bad, 0.9]);
            let err = score_samples(&mut scorer, &samples).unwrap_err();
            assert!(matches!(err, crate::Error::Model(_)), "{bad}: {err}");
            // The same rejection reaches `detect`, the public path.
            let mut scorer = Scripted::new(&[0.9, bad, 0.9]);
            assert!(
                detect(&mut scorer, &samples, &VadConfig::default()).is_err(),
                "{bad}"
            );
        }
    }

    /// The last span ends at the audio length, never beyond: the 30 ms pad would
    /// otherwise carry it past the final, partial frame.
    #[test]
    fn last_span_is_clamped_to_the_audio_length() {
        // 9 full frames and a 300-sample tail: 10 frames over 4908 samples (306 ms).
        let samples = vec![0.1f32; 9 * FRAME_SAMPLES + 300];
        // Silence for frames 0 and 1, speech from frame 2 through the partial last frame.
        let mut scores = vec![0.0f32, 0.0];
        scores.extend_from_slice(&[0.9; 8]);
        let mut scorer = Scripted::new(&scores);
        let cfg = VadConfig {
            min_speech_ms: 0,
            ..VadConfig::default()
        };
        let spans = detect(&mut scorer, &samples, &cfg).unwrap();
        // Start: frame 2 begins at 64 ms, less the 30 ms pad, so 34 ms. End: frame 10
        // ends at 320 ms, plus the pad is 350 ms, clamped to the 306 ms audio length.
        assert_eq!(
            spans,
            vec![Span {
                start_ms: 34,
                end_ms: 306
            }]
        );
        assert_eq!(spans[0].end_ms, duration_ms(samples.len()));
    }
}
