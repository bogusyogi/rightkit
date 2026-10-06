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
        Self { threshold: 0.5, neg_threshold: 0.35, min_speech_ms: 100, min_silence_ms: 120, pad_ms: 30 }
    }
}

impl VadConfig {
    fn validate(&self) -> Result<()> {
        let ok = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if !ok(self.threshold) || !ok(self.neg_threshold) || self.neg_threshold > self.threshold {
            return Err(crate::Error::Input("thresholds must be in [0,1] with neg_threshold <= threshold".into()));
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
    if samples.iter().any(|x| !x.is_finite()) {
        return Err(crate::Error::Input("non-finite samples".into()));
    }
    scorer.reset();
    let mut scores = Vec::with_capacity(samples.len() / FRAME_SAMPLES + 1);
    let mut frame = [0f32; FRAME_SAMPLES];
    for chunk in samples.chunks(FRAME_SAMPLES) {
        frame.fill(0.0);
        frame[..chunk.len()].copy_from_slice(chunk);
        scores.push(scorer.score(&frame)?);
    }
    Ok(scores_to_spans(&scores, cfg, samples.len() as u64 * 1000 / SAMPLE_RATE as u64))
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
            let crossings = frame.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
            let zcr = crossings as f32 / frame.len().max(1) as f32;
            if level > floor && !(zcr > 0.35 && level < floor * 2.0) { 1.0 } else { 0.0 }
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
        Self { merge_gap_ms: 300, max_chunk_ms: 30_000, min_chunk_ms: 200 }
    }
}

/// Merge near spans and split over-long ones. With `scores` the split lands on
/// the quietest frame in the last quarter of the window instead of mid-word.
pub fn chunk_spans(spans: &[Span], cfg: &ChunkConfig, scores: Option<&[f32]>) -> Vec<Span> {
    let max = cfg.max_chunk_ms.max(FRAME_MS * 4);
    let mut merged: Vec<Span> = Vec::new();
    for &s in spans {
        match merged.last_mut() {
            Some(p) if s.start_ms.saturating_sub(p.end_ms) < cfg.merge_gap_ms && s.end_ms - p.start_ms <= max => {
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
            out.push(Span { start_ms: start, end_ms: cut });
            start = cut;
        }
        out.push(Span { start_ms: start, end_ms: s.end_ms });
    }
    out.retain(|s| s.len_ms() >= cfg.min_chunk_ms);
    out
}
