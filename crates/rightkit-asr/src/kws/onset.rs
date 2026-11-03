//! Keyword onset estimates: where the spoken keyword began, as opposed to the frame on
//! which the transducer emitted its first token.
//!
//! The beam reports the encoder frame on which each token was *emitted*. A streaming
//! transducer emits a token once it has heard enough of it, typically near the end of the
//! phone, so the first token's emission trails the acoustic onset (by 40-170 ms on the
//! upstream test clips). It can also lead it: when two beam paths with the same tokens
//! merge, the surviving path keeps the first path's timestamps, which can belong to an
//! earlier, unrelated emission of the same token. With [`KeywordSpotterConfig::onset`] set,
//! the stream keeps the last few seconds of encoder frames and a per-frame energy envelope,
//! and every detection carries four candidates (see [`KeywordOnset`]). Estimation runs only
//! on a detection and never changes what is detected.
//!
//! [`KeywordSpotterConfig::onset`]: super::KeywordSpotterConfig::onset

use std::collections::VecDeque;

use super::math::Math;
use super::model::{EncoderOut, Zipformer2Transducer};
use super::JoinerBatching;
use crate::error::Result;

/// Feature frames (10 ms) per encoder frame (40 ms).
const SUBSAMPLING: i32 = 4;
const FEATURE_SECONDS: f32 = 0.01;
/// Lowest frequency of the band whose energy marks a fricative (/z/, /s/): 3.5 kHz.
pub(crate) const HIGH_BAND_HZ: f32 = 3_500.0;

/// Onset estimation settings. Off unless [`KeywordSpotterConfig::onset`] is set.
///
/// [`KeywordSpotterConfig::onset`]: super::KeywordSpotterConfig::onset
#[derive(Debug, Clone, PartialEq)]
pub struct OnsetConfig {
    /// Encoder frames (40 ms) kept per stream for `aligned` and `posterior`
    /// (320 floats each). Default 64 (2.56 s).
    pub encoder_history: usize,
    /// Feature frames (10 ms) of energy envelope kept per stream for `acoustic`
    /// (two floats each). Default 320 (3.2 s); the noise floor is read from it.
    pub feature_history: usize,
    /// `aligned`: encoder frames before the first emitted token where the alignment window
    /// starts. Default 10 (400 ms).
    pub align_lead_frames: usize,
    /// `posterior`: probability of the keyword's first token (in the context the keyword
    /// began in) a frame needs to belong to the keyword's start. Default 1e-2: the encoder
    /// attends across its whole 320 ms chunk, so lower values walk into a preceding pause.
    pub posterior_threshold: f32,
    /// `posterior`: furthest the walk goes back from its anchor, encoder frames. Default 15.
    pub posterior_max_frames: usize,
    /// `acoustic`: furthest the search goes back from its anchor, feature frames. Default 30
    /// (300 ms): in running speech a longer reach finds a gap inside the previous word.
    pub acoustic_max_frames: usize,
}

impl Default for OnsetConfig {
    fn default() -> Self {
        Self {
            encoder_history: 64,
            feature_history: 320,
            align_lead_frames: 10,
            posterior_threshold: 1e-2,
            posterior_max_frames: 15,
            acoustic_max_frames: 30,
        }
    }
}

/// One onset candidate, on the clock of [`KeywordResult::frames`] (counted from the
/// stream's last reset).
///
/// [`KeywordResult::frames`]: super::KeywordResult::frames
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnsetEstimate {
    /// Feature frame (10 ms); encoder frame `t` is feature frame `4 t`.
    pub feature_frame: i32,
    /// `feature_frame` in seconds (feature frame `f` is `f * 0.01`).
    pub seconds: f32,
}

impl OnsetEstimate {
    fn at_feature(feature_frame: i32) -> Self {
        Self {
            feature_frame,
            seconds: feature_frame as f32 * FEATURE_SECONDS,
        }
    }

    fn at_encoder(frame: i32) -> Self {
        Self::at_feature(frame * SUBSAMPLING)
    }
}

/// Which rule produced the `acoustic` estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcousticRule {
    /// The energy envelope fell near the noise floor before the keyword (a pause, or a
    /// stop closure in running speech): the onset is the first frame after that gap.
    Gap,
    /// No quiet frame within reach (continuous speech): the onset is the energy valley
    /// before the keyword.
    Valley,
    /// As `Valley`, moved back to where high-band (fricative) energy began.
    ValleyHighBand,
}

impl AcousticRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Valley => "valley",
            Self::ValleyHighBand => "valley_high_band",
        }
    }
}

/// Onset candidates for one detection. Every candidate but `emission` is `None` when the
/// retained history cannot support it (a stream younger than the window, say).
#[derive(Debug, Clone, PartialEq)]
pub struct KeywordOnset {
    /// The first token's emission frame (`frames[0]`): what Sherpa reports.
    pub emission: OnsetEstimate,
    /// First token frame of a Viterbi forced alignment of the keyword's tokens (one token
    /// or blank per frame) over the retained encoder frames from `align_lead_frames` before
    /// the first emission to the firing frame. Free of stale merged-path timestamps, but
    /// still an emission time.
    pub aligned: Option<OnsetEstimate>,
    /// Encoder frame of every keyword token in that alignment.
    pub aligned_frames: Vec<i32>,
    /// Walking back from the anchor (`aligned`, else `emission`): the earliest frame of
    /// the contiguous run where the first token's probability is at least
    /// `posterior_threshold` and no other token dominates. One batched joiner call.
    pub posterior: Option<OnsetEstimate>,
    /// Start of the speech burst holding the anchor, from the 10 ms energy envelope with a
    /// noise floor read from the retained history (see [`AcousticRule`]).
    pub acoustic: Option<OnsetEstimate>,
    pub acoustic_rule: Option<AcousticRule>,
}

/// What a stream keeps for onset estimation.
pub(crate) struct OnsetHistory {
    config: OnsetConfig,
    /// Retained encoder frames, oldest first; the front is encoder frame `encoder_first`
    /// (counted from the last reset, as the beam counts).
    encoder: VecDeque<Vec<f32>>,
    encoder_first: i32,
    /// Smoothed-later energies per feature frame: (mean log-mel, mean high-band log-mel);
    /// the front is absolute feature frame `energy_first`.
    energy: VecDeque<(f32, f32)>,
    energy_first: usize,
    /// Absolute feature frame where encoder frame 0 of the current reset epoch begins.
    feature_origin: usize,
    high_bin: usize,
}

impl OnsetHistory {
    pub(crate) fn new(config: OnsetConfig, high_bin: usize) -> Self {
        Self {
            config,
            encoder: VecDeque::new(),
            encoder_first: 0,
            energy: VecDeque::new(),
            energy_first: 0,
            feature_origin: 0,
            high_bin,
        }
    }

    /// The encoder clock restarts at `feature_frame` (a stream reset); energies are kept.
    pub(crate) fn reset(&mut self, feature_frame: usize) {
        self.encoder.clear();
        self.encoder_first = 0;
        self.feature_origin = feature_frame;
    }

    /// Record the energy of feature frames `[start, start + n)` given row-major `frames`,
    /// skipping frames already recorded (encoder chunks overlap).
    pub(crate) fn push_features(&mut self, start: usize, frames: &[f32], dim: usize) {
        let next = self.energy_first + self.energy.len();
        if self.energy.is_empty() {
            self.energy_first = start;
        } else if start > next {
            // A gap cannot happen while frames are read in order; restart the envelope.
            self.energy.clear();
            self.energy_first = start;
        }
        let next = self.energy_first + self.energy.len();
        for (i, frame) in frames.chunks_exact(dim).enumerate() {
            if start + i < next {
                continue;
            }
            self.energy.push_back(frame_energy(frame, self.high_bin));
        }
        while self.energy.len() > self.config.feature_history.max(1) {
            self.energy.pop_front();
            self.energy_first += 1;
        }
    }

    /// Record one encoder chunk whose first frame is encoder frame `first`. Once the ring
    /// is full, the oldest frame's buffer is reused: no allocation in steady state.
    pub(crate) fn push_encoder(&mut self, first: i32, out: &EncoderOut) {
        if self.encoder.is_empty() || first != self.encoder_first + self.encoder.len() as i32 {
            self.encoder.clear();
            self.encoder_first = first;
        }
        let capacity = self.config.encoder_history.max(1);
        for t in 0..out.frames {
            let mut frame = if self.encoder.len() >= capacity {
                self.encoder_first += 1;
                self.encoder.pop_front().unwrap_or_default()
            } else {
                Vec::with_capacity(out.dim)
            };
            frame.clear();
            frame.extend_from_slice(out.frame(t));
            self.encoder.push_back(frame);
        }
    }

    fn encoder_end(&self) -> i32 {
        self.encoder_first + self.encoder.len() as i32
    }
}

/// Mean log-mel over every bin and over bins `high_bin..`. The mean of logs, not the log
/// of the sum: the wide high-frequency mel bins would otherwise dominate.
fn frame_energy(frame: &[f32], high_bin: usize) -> (f32, f32) {
    let mean = |v: &[f32]| {
        if v.is_empty() {
            f32::NAN
        } else {
            v.iter().sum::<f32>() / v.len() as f32
        }
    };
    (mean(frame), mean(&frame[high_bin.min(frame.len())..]))
}

/// The detection a firing describes.
pub(crate) struct Firing<'a> {
    /// Decoder context the keyword's first token was emitted in.
    pub context: &'a [i64],
    pub tokens: &'a [i64],
    /// Emission frame of each token.
    pub frames: &'a [i32],
    /// Encoder frame on which the keyword fired.
    pub fired_frame: i32,
}

/// All four candidates for `firing` from `history`.
pub(crate) fn estimate(
    model: &mut Zipformer2Transducer,
    history: &OnsetHistory,
    firing: &Firing<'_>,
    batching: JoinerBatching,
    math: Math,
) -> Result<KeywordOnset> {
    let config = &history.config;
    let emitted = firing.frames.first().copied().unwrap_or(0);
    let mut onset = KeywordOnset {
        emission: OnsetEstimate::at_encoder(emitted),
        aligned: None,
        aligned_frames: Vec::new(),
        posterior: None,
        acoustic: None,
        acoustic_rule: None,
    };
    let k = firing.tokens.len();
    let window_start = history
        .encoder_first
        .max(emitted - config.align_lead_frames as i32);
    let window_end = history.encoder_end().min(firing.fired_frame + 1);
    let mut anchor = emitted;

    if k > 0 && window_end - window_start >= k as i32 {
        let posterior_start = history
            .encoder_first
            .max(window_start - config.posterior_max_frames as i32);
        let scores = joiner_window(
            model,
            history,
            firing,
            posterior_start,
            window_start,
            window_end,
            batching,
            math,
        )?;
        let vocab = model.vocab_size();
        let t_align = (window_end - window_start) as usize;
        // Row block k (k >= 1) holds context k over [window_start, window_end); block 0 holds
        // context 0 over [posterior_start, window_end).
        let lead = (window_start - posterior_start) as usize;
        let block0 = lead + t_align;
        let logp = |ctx: usize, t: usize, token: usize| -> f32 {
            let row = if ctx == 0 {
                lead + t
            } else {
                block0 + (ctx - 1) * t_align + t
            };
            scores[row * vocab + token]
        };
        let ids: Vec<usize> = firing.tokens.iter().map(|t| *t as usize).collect();
        if let Some(path) = viterbi_align(t_align, &ids, logp) {
            onset.aligned_frames = path.iter().map(|t| window_start + *t as i32).collect();
            anchor = onset.aligned_frames[0];
            onset.aligned = Some(OnsetEstimate::at_encoder(anchor));
        }
        // Context-0 probabilities from posterior_start up to the anchor.
        let span = (anchor - posterior_start + 1).max(0) as usize;
        let mut first = Vec::with_capacity(span);
        let mut blank = Vec::with_capacity(span);
        for t in 0..span {
            let row = t;
            first.push(math.expf(scores[row * vocab + ids[0]]));
            blank.push(math.expf(scores[row * vocab]));
        }
        if span > 0 {
            if let Some(start) = posterior_onset(
                &first,
                &blank,
                span - 1,
                config.posterior_threshold,
                config.posterior_max_frames,
            ) {
                onset.posterior = Some(OnsetEstimate::at_encoder(posterior_start + start as i32));
            }
        }
    }

    // Acoustic, on absolute feature frames.
    let anchor_feature = history.feature_origin as i64 + (anchor * SUBSAMPLING) as i64;
    if anchor_feature >= history.energy_first as i64 {
        let (full, high): (Vec<f32>, Vec<f32>) = history.energy.iter().copied().unzip();
        let at = (anchor_feature - history.energy_first as i64) as usize;
        if at < full.len() {
            if let Some((index, rule)) =
                acoustic_onset(&full, &high, at, config.acoustic_max_frames)
            {
                let absolute = history.energy_first + index;
                let since_reset = absolute as i64 - history.feature_origin as i64;
                if since_reset >= 0 {
                    onset.acoustic = Some(OnsetEstimate::at_feature(since_reset as i32));
                    onset.acoustic_rule = Some(rule);
                }
            }
        }
    }
    Ok(onset)
}

/// Joiner log-probabilities: context 0 over `[posterior_start, end)`, then each later
/// context `k` (after `k` keyword tokens) over `[start, end)`, row-major `[rows, vocab]`.
#[allow(clippy::too_many_arguments)] // one call site; the window bounds are its inputs
fn joiner_window(
    model: &mut Zipformer2Transducer,
    history: &OnsetHistory,
    firing: &Firing<'_>,
    posterior_start: i32,
    start: i32,
    end: i32,
    batching: JoinerBatching,
    math: Math,
) -> Result<Vec<f32>> {
    let context_size = model.context_size();
    let k = firing.tokens.len();
    // Context k: the last `context_size` tokens of context ++ tokens[..k].
    let mut path: Vec<i64> = firing.context.to_vec();
    let mut contexts = Vec::with_capacity((k + 1) * context_size);
    for i in 0..=k {
        if i > 0 {
            path.push(firing.tokens[i - 1]);
        }
        let mut ctx: Vec<i64> = path[path.len().saturating_sub(context_size)..].to_vec();
        while ctx.len() < context_size {
            ctx.insert(0, -1);
        }
        contexts.extend_from_slice(&ctx);
    }
    let (dec_dim, decoder_out) = model.run_decoder(contexts)?;
    let frame = |t: i32| &history.encoder[(t - history.encoder_first) as usize];
    let mut encoder_rows = Vec::new();
    let mut decoder_rows = Vec::new();
    let mut push = |t: i32, ctx: usize| {
        encoder_rows.extend_from_slice(frame(t));
        decoder_rows.extend_from_slice(&decoder_out[ctx * dec_dim..(ctx + 1) * dec_dim]);
    };
    for t in posterior_start..end {
        push(t, 0);
    }
    for ctx in 1..=k {
        for t in start..end {
            push(t, ctx);
        }
    }
    let enc_dim = history.encoder.front().map_or(0, Vec::len);
    let rows = decoder_rows.len() / dec_dim;
    let mut scores = match batching {
        JoinerBatching::Beam => model.run_joiner(&encoder_rows, &decoder_rows, rows)?,
        JoinerBatching::PerHypothesis => {
            let mut all = Vec::with_capacity(rows * model.vocab_size());
            for r in 0..rows {
                all.extend(model.run_joiner(
                    &encoder_rows[r * enc_dim..(r + 1) * enc_dim],
                    &decoder_rows[r * dec_dim..(r + 1) * dec_dim],
                    1,
                )?);
            }
            all
        }
    };
    for row in scores.chunks_mut(model.vocab_size()) {
        super::decoder::log_softmax(row, false, math);
    }
    Ok(scores)
}

/// Viterbi alignment of `tokens` over `frames` frames in the modified-beam-search
/// topology: each frame emits one token or a blank (token 0). `logp(k, t, token)` is the
/// log-probability of `token` at frame `t` after `k` keyword tokens. Returns the frame of
/// each token, or `None` when the tokens do not fit.
pub(crate) fn viterbi_align(
    frames: usize,
    tokens: &[usize],
    logp: impl Fn(usize, usize, usize) -> f32,
) -> Option<Vec<usize>> {
    let k = tokens.len();
    if k == 0 || k > frames {
        return None;
    }
    let width = k + 1;
    let mut score = vec![f64::NEG_INFINITY; (frames + 1) * width];
    // true: reached by emitting a token on the previous frame.
    let mut emitted = vec![false; (frames + 1) * width];
    score[0] = 0.0;
    for t in 0..frames {
        for j in 0..=k.min(t) {
            let here = score[t * width + j];
            if here == f64::NEG_INFINITY {
                continue;
            }
            let stay = here + logp(j, t, 0) as f64;
            let next = (t + 1) * width + j;
            if stay > score[next] {
                score[next] = stay;
                emitted[next] = false;
            }
            if j < k {
                let go = here + logp(j, t, tokens[j]) as f64;
                let next = (t + 1) * width + j + 1;
                if go > score[next] {
                    score[next] = go;
                    emitted[next] = true;
                }
            }
        }
    }
    if score[frames * width + k] == f64::NEG_INFINITY {
        return None;
    }
    let mut path = vec![0usize; k];
    let mut j = k;
    for t in (1..=frames).rev() {
        if j > 0 && emitted[t * width + j] {
            path[j - 1] = t - 1;
            j -= 1;
        }
    }
    (j == 0).then_some(path)
}

/// From `anchor` back at most `max_frames`: the earliest frame of the contiguous run
/// (ending at the anchor) where the first token's probability is at least `threshold`
/// and the mass on other tokens stays below one half. `first` and `blank` are per-frame
/// probabilities; the anchor itself always belongs to the run.
pub(crate) fn posterior_onset(
    first: &[f32],
    blank: &[f32],
    anchor: usize,
    threshold: f32,
    max_frames: usize,
) -> Option<usize> {
    if anchor >= first.len() || anchor >= blank.len() {
        return None;
    }
    let mut start = anchor;
    while start > 0 && anchor - start < max_frames {
        let u = start - 1;
        let other = 1.0 - blank[u] - first[u];
        if first[u] < threshold || other >= 0.5 {
            break;
        }
        start = u;
    }
    Some(start)
}

/// Three-frame moving average (edges average what exists).
fn smooth(v: &[f32]) -> Vec<f32> {
    (0..v.len())
        .map(|i| {
            let lo = i.saturating_sub(1);
            let hi = (i + 2).min(v.len());
            v[lo..hi].iter().sum::<f32>() / (hi - lo) as f32
        })
        .collect()
}

/// The `q`-quantile (nearest rank) of finite values.
fn quantile(values: &[f32], q: f32) -> Option<f32> {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let rank = ((v.len() - 1) as f32 * q).round() as usize;
    Some(v[rank])
}

/// Peak search half-width around the anchor, feature frames.
const PEAK_HALF_WIDTH: usize = 10;
/// Frames of history the noise floor needs.
const MIN_FLOOR_FRAMES: usize = 20;
/// Smallest peak-over-floor range (natural-log units of mean mel power, ~6 dB) that counts
/// as a speech burst.
const MIN_BURST_RANGE: f32 = 1.4;
/// Threshold above the floor, as a fraction of the burst range, and its minimum.
const BURST_FRACTION: f32 = 0.3;
const MIN_BURST_MARGIN: f32 = 0.5;
/// How far before the anchor the valley may lie (300 ms).
const VALLEY_REACH: usize = 30;
/// Spectral tilt (high-band minus overall mean log-mel) by which a fricative exceeds the
/// keyword's voiced peak, and how far back (150 ms) the fricative walk may go.
const FRICATIVE_TILT: f32 = 1.0;
const FRICATIVE_REACH: usize = 15;
const HIGH_BAND_FRACTION: f32 = 0.3;

/// Speech onset of the burst holding `anchor`, from per-frame energies (`full` = mean
/// log-mel, `high` = mean high-band log-mel). The noise floor is the 10th percentile of
/// every frame before the anchor.
///
/// From the energy peak within 100 ms of the anchor, walk back (at most `max_back` frames
/// before the anchor) to the first frame below `floor + max(0.3 range, 0.5)`: the frame
/// after it starts the burst ([`AcousticRule::Gap`]). Without such a frame the speech is
/// continuous: take the deepest valley of the smoothed envelope in the 300 ms before the
/// anchor ([`AcousticRule::Valley`]); when that valley is fricative-like (its spectral tilt
/// exceeds the peak's by `FRICATIVE_TILT` and its high band is active), walk back while the
/// tilt stays fricative-like, up to 150 ms ([`AcousticRule::ValleyHighBand`]): a /z/ after
/// a vowel starts where the high band rises.
pub(crate) fn acoustic_onset(
    full: &[f32],
    high: &[f32],
    anchor: usize,
    max_back: usize,
) -> Option<(usize, AcousticRule)> {
    if anchor >= full.len() || full.len() != high.len() || anchor < MIN_FLOOR_FRAMES {
        return None;
    }
    let full_s = smooth(full);
    let high_s = smooth(high);
    let lo = anchor.saturating_sub(max_back);
    let peak_lo = anchor.saturating_sub(PEAK_HALF_WIDTH).max(lo);
    let peak_hi = (anchor + PEAK_HALF_WIDTH + 1).min(full.len());
    let floor = quantile(&full[..anchor], 0.1)?;
    let (peak_at, peak) = (peak_lo..peak_hi)
        .map(|i| (i, full_s[i]))
        .filter(|(_, v)| v.is_finite())
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let range = peak - floor;
    if range < MIN_BURST_RANGE {
        return None;
    }
    let threshold = floor + (BURST_FRACTION * range).max(MIN_BURST_MARGIN);
    let mut f = peak_at;
    while f > lo && full[f - 1] >= threshold {
        f -= 1;
    }
    if f > lo {
        return Some((f, AcousticRule::Gap));
    }
    // Continuous speech: the deepest valley shortly before the anchor.
    let valley_lo = anchor.saturating_sub(VALLEY_REACH).max(lo);
    let valley_hi = anchor.saturating_sub(1).max(valley_lo + 1);
    let (valley, _) = (valley_lo..valley_hi)
        .map(|i| (i, full_s[i]))
        .filter(|(_, v)| v.is_finite())
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    let tilt = |i: usize| high_s[i] - full_s[i];
    let high_floor = quantile(&high[..anchor], 0.1)?;
    let high_peak = high_s[peak_lo..peak_hi]
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(f32::NEG_INFINITY, f32::max)
        .max(high_s[valley]);
    let high_threshold = high_floor + HIGH_BAND_FRACTION * (high_peak - high_floor);
    let fricative = |i: usize, bar: f32| tilt(i) >= bar && high_s[i] >= high_threshold;
    if tilt(valley) - tilt(peak_at) >= FRICATIVE_TILT && fricative(valley, f32::NEG_INFINITY) {
        let bar = (tilt(valley) + tilt(peak_at)) / 2.0;
        let mut g = valley;
        while g > lo && valley - g < FRICATIVE_REACH && fricative(g - 1, bar) {
            g -= 1;
        }
        return Some((g, AcousticRule::ValleyHighBand));
    }
    Some((valley, AcousticRule::Valley))
}
