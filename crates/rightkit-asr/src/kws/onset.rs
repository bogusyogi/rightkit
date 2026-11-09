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

/// Search used for the acoustic onset candidate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AcousticSearch {
    /// Existing speech-burst and valley search.
    #[default]
    Legacy,
    /// Experimental search for an isolated rise in high-band energy before the anchor.
    /// Select only for keywords expected to begin with frication.
    InitialFricative,
}

/// Settings for the D4 native fricative onset estimator (`KeywordOnset::fricative`). Off
/// unless `enabled` is set and [`KeywordSpotterConfig::onset`] is on. Durations are
/// milliseconds, rounded up to whole 10 ms feature frames; `rise` and `tilt_margin` are
/// natural-log mel units.
///
/// [`KeywordSpotterConfig::onset`]: super::KeywordSpotterConfig::onset
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FricativeParams {
    /// Run the estimator. Default off.
    pub enabled: bool,
    /// Search window W before the anchor. Default 250 (sweep 150/250/350).
    pub window_ms: u32,
    /// Background floor window F ending at `anchor - W`. Default 500 (sweep 200/500).
    pub floor_ms: u32,
    /// Hiss rise H of the high band above its floor (also the vowel's rise of the
    /// low-band mean over its floor). Default 1.0 (sweep 0.5/1.0/1.5).
    pub rise: f32,
    /// Minimum hiss run D. Default 40 (sweep 20/40/60).
    pub min_run_ms: u32,
    /// The hiss run's vowel entry must lie within V before the anchor. Default 60.
    pub vowel_ms: u32,
    /// Hiss tilt margin above the background tilt median over the floor window.
    /// Default 1.0 (the `FRICATIVE_TILT` margin).
    pub tilt_margin: f32,
}

impl FricativeParams {
    /// The default settings (off), usable in `const` context.
    pub const DEFAULT: Self = Self {
        enabled: false,
        window_ms: 250,
        floor_ms: 500,
        rise: 1.0,
        min_run_ms: 40,
        vowel_ms: 60,
        tilt_margin: FRICATIVE_TILT,
    };

    /// Reject values the estimator could not honour, so none is silently ignored or clamped.
    /// Ranges: `window_ms` 30..=1000, `floor_ms` 10..=2000, `min_run_ms` 10..=1000,
    /// `vowel_ms` 0..=1000, `rise` 0..=10 and `tilt_margin` -10..=10 (finite), and the
    /// window plus floor must fit in the default 320-frame energy history.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let range = |name: &str, value: u32, low: u32, high: u32| {
            if (low..=high).contains(&value) {
                Ok(())
            } else {
                Err(format!("fricative {name} must be {low} to {high} ms, got {value}"))
            }
        };
        range("window_ms", self.window_ms, 30, 1_000)?;
        range("floor_ms", self.floor_ms, 10, 2_000)?;
        range("min_run_ms", self.min_run_ms, 10, 1_000)?;
        range("vowel_ms", self.vowel_ms, 0, 1_000)?;
        if !self.rise.is_finite() || !(0.0..=10.0).contains(&self.rise) {
            return Err(format!("fricative rise must be 0 to 10, got {}", self.rise));
        }
        if !self.tilt_margin.is_finite() || !(-10.0..=10.0).contains(&self.tilt_margin) {
            return Err(format!(
                "fricative tilt_margin must be -10 to 10, got {}",
                self.tilt_margin
            ));
        }
        if self.window_ms + self.floor_ms >= 3_000 {
            return Err(format!(
                "fricative window_ms + floor_ms must be under 3000, got {}",
                self.window_ms + self.floor_ms
            ));
        }
        Ok(())
    }
}

impl Default for FricativeParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Why the D4 estimator produced no estimate (labels follow the offline prototype).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FricativeAbstain {
    /// A non-finite statistic in the floor window or search interval, or unusable settings.
    InvalidEvidence,
    /// The floor window, the search window or the anchor is not retained (or too few frames).
    FloorUnsupported,
    /// The estimate exists but starts before this reset epoch (`feature_origin`), so it is
    /// not on the stream's clock. Distinct from an unsupported floor.
    BeforeEpoch,
    /// No hiss run is followed by a vowel entry inside `[anchor - vowel_ms, anchor]`.
    NoTransition,
    /// A hiss run enters the vowel in range but none lasts `min_run_ms`.
    RunTooShort,
    /// More than one run qualifies.
    Ambiguous,
    /// The chosen run reaches the start of the search window.
    WindowLimit,
}

impl FricativeAbstain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEvidence => "abstain:invalid_evidence",
            Self::FloorUnsupported => "abstain:floor_short",
            Self::BeforeEpoch => "abstain:before_epoch",
            Self::NoTransition => "abstain:no_transition",
            Self::RunTooShort => "abstain:run_too_short",
            Self::Ambiguous => "abstain:ambiguous_transitions",
            Self::WindowLimit => "abstain:window_limit",
        }
    }
}

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
    /// Acoustic candidate strategy. Defaults to [`AcousticSearch::Legacy`].
    pub acoustic_search: AcousticSearch,
    /// D4 fricative estimator settings (default off).
    pub fricative: FricativeParams,
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
            acoustic_search: AcousticSearch::Legacy,
            fricative: FricativeParams::default(),
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
    /// Leading edge of an isolated high-band rise before the anchor.
    InitialFricative,
}

impl AcousticRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Valley => "valley",
            Self::ValleyHighBand => "valley_high_band",
            Self::InitialFricative => "initial_fricative",
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
    /// Acoustic boundary candidate from the 10 ms energy envelope, using the configured
    /// search (see [`AcousticRule`]).
    pub acoustic: Option<OnsetEstimate>,
    pub acoustic_rule: Option<AcousticRule>,
    /// D4: leading edge of the hiss run that enters the keyword's vowel (see
    /// [`FricativeParams`]). `None` unless enabled, and when the estimator abstains.
    pub fricative: Option<OnsetEstimate>,
    /// Why D4 produced no estimate: `Some` only when enabled and it abstained.
    pub fricative_abstain: Option<FricativeAbstain>,
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
    /// Mel bins per feature frame, as last pushed (0 until the first push).
    bins: usize,
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
            bins: 0,
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
        self.bins = dim;
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

    /// Whether this history still holds everything [`estimate`] would read for a firing whose
    /// first token was emitted on encoder frame `emitted` and that fired on `fired_frame`:
    /// encoder frames from `emitted - align_lead - posterior_max` (not before the epoch's frame
    /// 0) to the firing, and energy frames from the acoustic or D4 reach before the first
    /// token's feature frame (not before feature frame 0). `Err` names what was evicted; the
    /// shared front end then reports the onset unavailable instead of estimating from a
    /// shorter window. Used only for shared beams.
    pub(crate) fn lookback_available(
        &self,
        emitted: i32,
        fired_frame: i32,
    ) -> std::result::Result<(), &'static str> {
        let c = &self.config;
        let encoder_need = (emitted - c.align_lead_frames as i32 - c.posterior_max_frames as i32).max(0);
        if self.encoder.is_empty() || self.encoder_first > encoder_need || self.encoder_end() < fired_frame + 1 {
            return Err("encoder_history_evicted");
        }
        let d4 = if c.fricative.enabled {
            (c.fricative.window_ms + c.fricative.floor_ms) as usize / 10
        } else {
            0
        };
        let reach = c.acoustic_max_frames.max(d4) as i64;
        let anchor = self.feature_origin as i64 + 4 * i64::from(emitted);
        if self.energy.is_empty() || (self.energy_first as i64) > (anchor - reach).max(0) {
            return Err("energy_history_evicted");
        }
        Ok(())
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
        fricative: None,
        fricative_abstain: None,
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
            let candidate = match config.acoustic_search {
                AcousticSearch::Legacy => {
                    acoustic_onset(&full, &high, at, config.acoustic_max_frames)
                }
                AcousticSearch::InitialFricative => {
                    let epoch_start = history.feature_origin.saturating_sub(history.energy_first);
                    at.checked_sub(epoch_start)
                        .and_then(|anchor| {
                            initial_fricative_onset(
                                &full[epoch_start..=at],
                                &high[epoch_start..=at],
                                anchor,
                                config.acoustic_max_frames,
                            )
                        })
                        .map(|index| (epoch_start + index, AcousticRule::InitialFricative))
                }
            };
            if let Some((index, rule)) = candidate {
                let absolute = history.energy_first + index;
                let since_reset = absolute as i64 - history.feature_origin as i64;
                if since_reset >= 0 {
                    onset.acoustic = Some(OnsetEstimate::at_feature(since_reset as i32));
                    onset.acoustic_rule = Some(rule);
                }
            }
        }
    }

    // D4 fricative estimator, on absolute feature frames; same anchor and clock as above.
    if config.fricative.enabled {
        let outcome = if anchor_feature >= history.energy_first as i64 {
            let energy: Vec<(f32, f32)> = history.energy.iter().copied().collect();
            fricative_onset(
                &energy,
                history.energy_first,
                anchor_feature as usize,
                history.bins,
                history.high_bin,
                &config.fricative,
            )
        } else {
            Err(FricativeAbstain::FloorUnsupported)
        };
        match outcome {
            Ok(index) => match fricative_stream_frame(index, history.feature_origin) {
                Ok(frame) => onset.fricative = Some(OnsetEstimate::at_feature(frame)),
                // The run starts before this reset epoch: not on the stream's clock.
                Err(reason) => onset.fricative_abstain = Some(reason),
            },
            Err(reason) => onset.fricative_abstain = Some(reason),
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

/// Require at least two 10 ms frames of evidence at the rise and on either side.
/// This is a replay criterion pending corpus qualification.
const FRICATIVE_STABLE_FRAMES: usize = 2;

/// Search before the anchor for one high-band rise that leads into sustained speech.
/// The legacy valley can land in a vowel or on a later fricative. This search requires
/// one sustained run with an observed left edge and contrast against the final active
/// frames. Return an acoustic candidate for comparison. Phone identity and previous
/// word boundaries require separate evidence.
fn initial_fricative_onset(
    full: &[f32],
    high: &[f32],
    anchor: usize,
    max_back: usize,
) -> Option<usize> {
    if full.len() != high.len()
        || anchor >= full.len()
        || anchor < MIN_FLOOR_FRAMES
        || full[..=anchor]
            .iter()
            .chain(&high[..=anchor])
            .any(|value| !value.is_finite())
    {
        return None;
    }

    let lo = anchor.saturating_sub(max_back);
    let full_floor = quantile(&full[..anchor], 0.1)?;
    let high_floor = quantile(&high[..anchor], 0.1)?;
    let high_peak = high[lo..=anchor].iter().copied().reduce(f32::max)?;
    let high_range = high_peak - high_floor;
    if !high_range.is_finite() || high_range < MIN_BURST_RANGE {
        return None;
    }
    let high_threshold = high_floor + (HIGH_BAND_FRACTION * high_range).max(MIN_BURST_MARGIN);
    let speech_threshold = full_floor + MIN_BURST_MARGIN;
    let tilt = |i: usize| f64::from(high[i]) - f64::from(full[i]);
    let reference_start = anchor + 1 - FRICATIVE_STABLE_FRAMES;
    let reference_tilt = (reference_start..=anchor).map(tilt).reduce(f64::max)?;
    let fricative = |i: usize| {
        full[i] > speech_threshold
            && high[i] >= high_threshold
            && tilt(i) >= reference_tilt + f64::from(FRICATIVE_TILT)
    };
    let start = (lo..reference_start).find(|&i| fricative(i))?;
    let end = (start..=anchor).find(|&i| !fricative(i))?;
    if start - lo < FRICATIVE_STABLE_FRAMES
        || end - start < FRICATIVE_STABLE_FRAMES
        || (end..=anchor).any(fricative)
        || full[end..=anchor]
            .iter()
            .any(|&energy| energy <= speech_threshold)
        || high[start - FRICATIVE_STABLE_FRAMES..start]
            .iter()
            .any(|&energy| high[start] - energy < MIN_BURST_MARGIN)
    {
        return None;
    }
    Some(start)
}

/// D4: leading edge of the hiss (high-band-led) run that enters the keyword's vowel.
///
/// `energy[i]` is (mean log-mel over all `bins`, mean over bins `high_bin..`) for absolute
/// feature frame `energy_first + i`; `anchor` is an absolute feature frame. Returns an
/// absolute feature frame, or the reason it abstained ([`FricativeAbstain`]): the floor
/// window is not retained, evidence or settings are invalid, no hiss run enters a vowel
/// within `vowel_ms` of the anchor, a run was too short, several runs qualify, or the
/// chosen run touches the start of the search window.
///
/// The energy history is continuous audio: `OnsetHistory::reset` clears only the encoder
/// history and moves `feature_origin`, and `push_features` restarts the envelope on a gap,
/// so the floor window never spans a discontinuity. Any non-finite frame in the floor window
/// or search interval abstains, as does a vowel entry more than `ADJACENT_GAP` frames after
/// a run.
///
/// The low-band mean is derived from the two stored means:
/// `rest = (bins * all - n_high * high) / (bins - n_high)`, `tilt = high - rest`.
fn fricative_onset(
    energy: &[(f32, f32)],
    energy_first: usize,
    anchor: usize,
    bins: usize,
    high_bin: usize,
    params: &FricativeParams,
) -> std::result::Result<usize, FricativeAbstain> {
    use FricativeAbstain::{
        Ambiguous, FloorUnsupported, InvalidEvidence, NoTransition, RunTooShort, WindowLimit,
    };
    if !params.rise.is_finite() || !params.tilt_margin.is_finite() {
        return Err(InvalidEvidence);
    }
    let n_high = bins.checked_sub(high_bin).ok_or(InvalidEvidence)?;
    if n_high == 0 || high_bin == 0 {
        return Err(InvalidEvidence);
    }
    let frames = |ms: u32| ms.saturating_add(9) as usize / 10;
    let window = frames(params.window_ms);
    let floor_len = frames(params.floor_ms);
    let vowel = frames(params.vowel_ms);
    let min_run = frames(params.min_run_ms).max(1);
    if window < 3 || floor_len == 0 {
        return Err(InvalidEvidence);
    }
    let end = energy_first
        .checked_add(energy.len())
        .ok_or(FloorUnsupported)?;
    if anchor < energy_first || anchor >= end {
        return Err(FloorUnsupported);
    }
    let search_start = anchor.checked_sub(window).ok_or(FloorUnsupported)?;
    let floor_start = search_start.checked_sub(floor_len).ok_or(FloorUnsupported)?;
    if floor_start < energy_first {
        return Err(FloorUnsupported);
    }

    let at = |t: usize| energy[t - energy_first];
    let (all_n, high_n, rest_n) = (bins as f32, n_high as f32, high_bin as f32);
    let rest = |t: usize| (all_n * at(t).0 - high_n * at(t).1) / rest_n;
    let tilt = |t: usize| at(t).1 - rest(t);

    // Invalid evidence abstains: every frame from the floor window through the anchor must
    // have finite statistics, so no floor is built from a few surviving samples and no
    // invalid frame sits between a hiss and a vowel.
    if !(floor_start..=anchor).all(|t| {
        at(t).0.is_finite() && at(t).1.is_finite() && rest(t).is_finite() && tilt(t).is_finite()
    }) {
        return Err(InvalidEvidence);
    }

    let highs: Vec<f32> = (floor_start..search_start).map(|t| at(t).1).collect();
    let rests: Vec<f32> = (floor_start..search_start).map(rest).collect();
    let tilts: Vec<f32> = (floor_start..search_start).map(tilt).collect();
    let floor_high = quantile(&highs, 0.1).ok_or(InvalidEvidence)?;
    let floor_rest = quantile(&rests, 0.1).ok_or(InvalidEvidence)?;
    let hiss_bar = quantile(&tilts, 0.5).ok_or(InvalidEvidence)? + params.tilt_margin;
    if !hiss_bar.is_finite() || !floor_high.is_finite() || !floor_rest.is_finite() {
        return Err(InvalidEvidence);
    }
    let rise = params.rise;

    let hiss = |t: usize| at(t).1 - floor_high >= rise && tilt(t) >= hiss_bar;
    let vowel_at = |t: usize| tilt(t) <= -FRICATIVE_TILT && rest(t) - floor_rest >= rise;

    // Temporal-association heuristic, not proof of the same occurrence (the offline
    // prototype's rule): a hiss run counts only when a vowel entry starts within
    // `ADJACENT_GAP` frames after its last frame, and that entry lies in
    // [anchor - V, anchor]. The D filter is applied before counting ambiguity, as the
    // prototype does; a run that enters the vowel in range but is too short only sets
    // `short_adjacent`, which names the abstention when no run is eligible.
    // (run start, run end), inclusive.
    let mut eligible: Vec<(usize, usize)> = Vec::new();
    let mut short_adjacent = false;
    let mut t = search_start;
    while t <= anchor {
        if !hiss(t) {
            t += 1;
            continue;
        }
        let run_start = t;
        while t <= anchor && hiss(t) {
            t += 1;
        }
        let run_end = t - 1;
        let last = (run_end + 1 + ADJACENT_GAP).min(anchor);
        let Some(entry) = (run_end + 1..=last).find(|&u| vowel_at(u)) else {
            continue;
        };
        if entry + vowel < anchor {
            continue;
        }
        if run_end + 1 - run_start >= min_run {
            eligible.push((run_start, run_end));
        } else {
            short_adjacent = true;
        }
    }
    match eligible.as_slice() {
        [] if short_adjacent => Err(RunTooShort),
        [] => Err(NoTransition),
        [(run_start, _)] if *run_start == search_start => Err(WindowLimit),
        [(run_start, _)] => Ok(*run_start),
        _ => Err(Ambiguous),
    }
}

/// A D4 estimate (absolute feature frame) as a feature frame since `feature_origin`.
/// `BeforeEpoch` when it precedes the origin; `InvalidEvidence` when it does not fit `i32`.
fn fricative_stream_frame(
    index: usize,
    feature_origin: usize,
) -> std::result::Result<i32, FricativeAbstain> {
    let since_reset = index
        .checked_sub(feature_origin)
        .ok_or(FricativeAbstain::BeforeEpoch)?;
    i32::try_from(since_reset).map_err(|_| FricativeAbstain::InvalidEvidence)
}

/// Frames between the end of a hiss run and the vowel entry that still count as the same
/// occurrence (the offline prototype: adjacency within 1 frame).
const ADJACENT_GAP: usize = 1;

#[cfg(test)]
mod tests {
    use super::{
        fricative_onset, fricative_stream_frame, initial_fricative_onset, FricativeAbstain,
        FricativeParams,
    };

    fn envelope() -> ([f32; 40], [f32; 40]) {
        let mut full = [-10.0; 40];
        let mut high = [-11.0; 40];
        full[24..29].fill(-5.5);
        high[24..29].fill(-4.0);
        full[29..].fill(-1.0);
        high[29..].fill(-7.0);
        (full, high)
    }

    #[test]
    fn finds_the_rise_after_quiet_or_connected_speech() {
        let (mut full, mut high) = envelope();
        assert_eq!(initial_fricative_onset(&full, &high, 34, 30), Some(24));
        full[18..24].fill(-1.0);
        high[18..24].fill(-7.0);
        assert_eq!(initial_fricative_onset(&full, &high, 34, 30), Some(24));
    }

    #[test]
    fn needs_a_visible_left_edge_and_enough_finite_history() {
        let (mut full, high) = envelope();
        assert_eq!(initial_fricative_onset(&full, &high, 34, 11), None);
        assert_eq!(initial_fricative_onset(&full, &high, 34, 12), Some(24));
        assert_eq!(initial_fricative_onset(&full[20..], &high[20..], 14, 30), None);
        full[22] = f32::NAN;
        assert_eq!(initial_fricative_onset(&full, &high, 34, 30), None);
    }

    #[test]
    fn abstains_on_multiple_runs_an_impulse_or_a_gap_before_the_anchor() {
        let (mut full, mut high) = envelope();
        full[19..21].fill(-5.5);
        high[19..21].fill(-4.0);
        assert_eq!(initial_fricative_onset(&full, &high, 34, 30), None);

        let (mut full, mut high) = envelope();
        full[25..29].fill(-1.0);
        high[25..29].fill(-7.0);
        assert_eq!(initial_fricative_onset(&full, &high, 34, 30), None);

        let (mut full, mut high) = envelope();
        full[29] = -10.0;
        high[29] = -11.0;
        assert_eq!(initial_fricative_onset(&full, &high, 34, 30), None);
    }

    // D4 synthetic energies: 2 bins, high bin 1, so rest = 2 * all - high. Silence is
    // (high 0, rest 0), hiss (3, 0), vowel (0, 3): exact in f32. With the default settings
    // (W 250 -> 25 frames, F 500 -> 50 frames, D 40 -> 4 frames, V 60 -> 6 frames, rise 1,
    // tilt margin 1) and anchor 80: search_start 55, floor frames 5..55 all silent, so
    // floor_high = floor_rest = 0 and hiss_bar = 1; a hiss frame has high >= 1 and tilt >= 1,
    // a vowel frame has tilt <= -1 and rest >= 1. A vowel entry counts from frame 74 (80 - 6).
    const ANCHOR: usize = 80;

    fn d4_energy(
        hiss: &[std::ops::Range<usize>],
        vowel: &[std::ops::Range<usize>],
    ) -> Vec<(f32, f32)> {
        let mut frames = vec![(0.0_f32, 0.0_f32); 100]; // (high, rest)
        for range in hiss {
            frames[range.clone()].fill((3.0, 0.0));
        }
        for range in vowel {
            frames[range.clone()].fill((0.0, 3.0));
        }
        frames
            .into_iter()
            .map(|(high, rest)| ((high + rest) / 2.0, high))
            .collect()
    }

    fn d4(
        energy: &[(f32, f32)],
        anchor: usize,
        params: &FricativeParams,
    ) -> Result<usize, FricativeAbstain> {
        fricative_onset(energy, 0, anchor, 2, 1, params)
    }

    #[test]
    fn d4_cuts_at_the_hiss_that_enters_the_keyword_vowel() {
        // Hiss 70..=75, vowel 76..: entry 76 >= 74, run 6 >= 4 frames: cut at 70.
        let energy = d4_energy(&[70..76], &[76..82]);
        assert_eq!(d4(&energy, ANCHOR, &FricativeParams::DEFAULT), Ok(70));
    }

    #[test]
    fn d4_ignores_a_preceding_word_hiss_followed_by_silence() {
        // Hiss 60..=65, silence, vowel only at 78: no vowel within one frame of the run.
        let energy = d4_energy(&[60..66], &[78..82]);
        assert_eq!(
            d4(&energy, ANCHOR, &FricativeParams::DEFAULT),
            Err(FricativeAbstain::NoTransition)
        );
    }

    #[test]
    fn d4_abstains_on_a_non_finite_frame_between_hiss_and_vowel() {
        let mut energy = d4_energy(&[70..75], &[76..82]);
        energy[75] = (f32::NAN, f32::NAN);
        assert_eq!(
            d4(&energy, ANCHOR, &FricativeParams::DEFAULT),
            Err(FricativeAbstain::InvalidEvidence)
        );
    }

    #[test]
    fn d4_abstains_when_two_runs_qualify() {
        // D 20 -> 2 frames. Runs 72..=73 (entry 74) and 76..=77 (entry 78) both qualify.
        let energy = d4_energy(&[72..74, 76..78], &[74..76, 78..80]);
        let params = FricativeParams {
            min_run_ms: 20,
            ..FricativeParams::DEFAULT
        };
        assert_eq!(d4(&energy, ANCHOR, &params), Err(FricativeAbstain::Ambiguous));
    }

    #[test]
    fn d4_selects_the_valid_run_beside_a_short_one() {
        // Short run 74 (1 frame, entry 75) is set aside; run 76..=79 (4 frames, entry 80)
        // qualifies: cut at 76.
        let energy = d4_energy(&[74..75, 76..80], &[75..76, 80..81]);
        assert_eq!(d4(&energy, ANCHOR, &FricativeParams::DEFAULT), Ok(76));
    }

    #[test]
    fn d4_rejects_a_30_ms_hiss_at_a_40_ms_minimum() {
        // Hiss 73..=75 is 3 frames < 4, entry 76: no eligible run, a short adjacent one.
        let energy = d4_energy(&[73..76], &[76..82]);
        assert_eq!(
            d4(&energy, ANCHOR, &FricativeParams::DEFAULT),
            Err(FricativeAbstain::RunTooShort)
        );
    }

    #[test]
    fn d4_abstains_when_the_run_touches_the_window_start() {
        // Hiss 55..=75 starts at search_start 55: not a clipped boundary.
        let energy = d4_energy(&[55..76], &[76..82]);
        assert_eq!(
            d4(&energy, ANCHOR, &FricativeParams::DEFAULT),
            Err(FricativeAbstain::WindowLimit)
        );
    }

    #[test]
    fn d4_abstains_when_the_floor_window_is_not_retained() {
        // Anchor 20 is inside the 25-frame window: no room for a search start or floor.
        let energy = d4_energy(&[10..16], &[16..22]);
        assert_eq!(
            d4(&energy, 20, &FricativeParams::DEFAULT),
            Err(FricativeAbstain::FloorUnsupported)
        );
    }

    #[test]
    fn d4_estimate_before_the_reset_epoch_is_before_epoch_not_floor_short() {
        // Origin 50: an estimate at 49 precedes the epoch; 50 and 70 map to 0 and 20.
        assert_eq!(
            fricative_stream_frame(49, 50),
            Err(FricativeAbstain::BeforeEpoch)
        );
        assert_eq!(fricative_stream_frame(50, 50), Ok(0));
        assert_eq!(fricative_stream_frame(70, 50), Ok(20));
        assert_eq!(FricativeAbstain::BeforeEpoch.as_str(), "abstain:before_epoch");
        assert_ne!(
            FricativeAbstain::BeforeEpoch.as_str(),
            FricativeAbstain::FloorUnsupported.as_str()
        );
    }
}
