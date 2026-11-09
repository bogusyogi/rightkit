//! Default-off firing and no-fire trace of the keyword beam.
//!
//! Pure observation: nothing here is read back by the decoder, so beam order, pruning,
//! `ys_probs`, timestamps and firing cannot depend on it. With
//! [`super::KeywordSpotterConfig::trace`] `None` (the default) a stream holds no
//! [`TraceState`], and the decoder pays one `Option` check per chunk and one `bool` check
//! per step.
//!
//! Two records, both bounded:
//! - [`FiringTrace`]: the firing as it exists at the exact pre-reset event (the decoder
//!   context, the selected path's `ys`, `timestamps` and `ys_probs`, the fired frame, the
//!   trailing blanks, the top hypotheses), snapshotted at the firing step before a later
//!   hit in the same chunk can overwrite the result. At most [`FIRINGS_KEPT`] are held
//!   between drains; older ones are counted in `firings_dropped`.
//! - [`StepTrace`]: one record per decoded encoder frame inside an optional window, so a
//!   run that never fires still shows what the beam did. At most
//!   [`TraceConfig::window_steps`] are held between drains; later steps are counted in
//!   `steps_dropped`, never silently omitted. Per-step event lists (TTL expiries, pruned
//!   paths, keyword merges) are bounded by the beam width and carry no separate cap.
//!
//! Memory is bounded in bytes per record as well as in record count: every path history
//! (`ys`, `timestamps`, `ys_probs`) keeps only its last [`TraceConfig::history_tokens`]
//! entries, with the original lengths and a `truncated` flag beside it.
//!
//! Clocks are kept distinct: [`TraceClock`] counts feature frames and samples handed to
//! the stream, never recognition wall time. The crate cannot tell real from synthetic
//! samples, so padding provenance is the caller's to supply.

use std::sync::Arc;

use super::context_graph::ContextGraph;
use super::decoder::{Firing, Hypothesis};

/// Firing records held between drains.
const FIRINGS_KEPT: usize = 16;
/// Gap-guard drop records held between drains.
const GAP_DROPS_KEPT: usize = 64;
/// Hypotheses recorded at a firing: the selected path plus the best others.
const TOP_HYPOTHESES: usize = 4;
/// Path history kept per record when [`TraceConfig::history_tokens`] is `0`.
const DEFAULT_HISTORY_TOKENS: usize = 16;
/// Keyword-extension candidates recorded per step; the rest are counted as omitted.
pub(crate) const MAX_STEP_EXTENSIONS: usize = 8;

/// The last `cap` items of a path history.
fn tail<T: Clone>(items: &[T], cap: usize) -> Vec<T> {
    items[items.len().saturating_sub(cap)..].to_vec()
}

/// What to record. The default records nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceConfig {
    /// Snapshot every firing at its pre-reset event.
    pub firing: bool,
    /// Capacity, in decoded encoder frames, of the per-step window record; `0` records no
    /// steps. Which frames qualify is set per stream by
    /// [`super::KeywordStream::set_trace_window`] (every frame until capacity otherwise).
    pub window_steps: usize,
    /// Most entries of each path history (`ys`, `timestamps`, `ys_probs`) kept per record;
    /// `0` means 16. Longer histories keep their last entries and set `truncated`.
    pub history_tokens: usize,
}

impl TraceConfig {
    /// The effective per-record history cap.
    pub fn history(&self) -> usize {
        if self.history_tokens == 0 {
            DEFAULT_HISTORY_TOKENS
        } else {
            self.history_tokens
        }
    }

    /// True when this config records anything.
    pub fn is_active(&self) -> bool {
        self.firing || self.window_steps > 0
    }
}

/// Which spotter produced a record. Hashes are FNV-1a 64 and identify, not authenticate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceIdentity {
    /// `rightkit-asr` crate version.
    pub crate_version: &'static str,
    /// Hash of the keyword list text.
    pub keywords_fnv64: u64,
    /// Hash of the decoder options and keyword defaults (`Debug` text).
    pub config_fnv64: u64,
    /// Byte length of the encoder, decoder and joiner files (`0` when unreadable). A length
    /// is NOT a content hash of the model.
    pub model_bytes: [u64; 3],
}

impl TraceIdentity {
    pub(crate) fn new(
        keywords: &str,
        config_repr: &str,
        model_files: [&std::path::Path; 3],
    ) -> Self {
        let mut model_bytes = [0u64; 3];
        for (slot, path) in model_bytes.iter_mut().zip(model_files) {
            *slot = std::fs::metadata(path).map_or(0, |meta| meta.len());
        }
        Self {
            crate_version: env!("CARGO_PKG_VERSION"),
            keywords_fnv64: fnv1a64(keywords.as_bytes()),
            config_fnv64: fnv1a64(config_repr.as_bytes()),
            model_bytes,
        }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The stream's input clocks at a record. Feature frames are 10 ms; an encoder frame
/// (the unit of `KeywordResult::frames`) is four of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceClock {
    /// Resets since the stream was created.
    pub epoch: u64,
    /// Feature frame at which the current epoch's frame `0` starts: the encoder frame
    /// origin on the clock of `KeywordResult::frames` (`origin / 4` encoder frames).
    pub epoch_origin_feature_frame: usize,
    /// Feature frames available to the encoder.
    pub feature_frames_ready: usize,
    /// End (exclusive) of the feature window the encoder consumed for this chunk.
    pub encoder_window_end_feature_frame: usize,
    /// Feature frames the encoder has advanced past, after this chunk.
    pub encoder_consumed_feature_frames: usize,
    /// Samples handed to the stream by `accept_waveform` since it was created, INCLUDING any
    /// the caller declared as padding. Real audio fed is `samples_fed - padding_samples_fed`;
    /// never read `samples_fed` as real-audio latency on a stream that was given padding.
    pub samples_fed: u64,
    /// Samples fed while the caller had declared them padding
    /// ([`super::KeywordStream::declare_trace_padding`]); the crate cannot tell real from
    /// synthetic samples itself. `0` when nothing was declared.
    pub padding_samples_fed: u64,
}

/// One hypothesis alive at the firing.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceHypothesis {
    /// This is the path the firing selected.
    pub selected: bool,
    /// The last `history_tokens` entries of each history; see the `*_len` fields.
    pub ys: Vec<i64>,
    pub timestamps: Vec<i32>,
    pub ys_probs: Vec<f32>,
    /// Original history lengths.
    pub ys_len: usize,
    pub timestamps_len: usize,
    pub ys_probs_len: usize,
    /// Some history was cut to its last entries.
    pub truncated: bool,
    pub log_prob: f64,
    pub context_state: usize,
    /// Keyword-prefix progress: the graph depth of `context_state`.
    pub level: usize,
    pub num_trailing_blanks: i32,
}

impl TraceHypothesis {
    fn of(graph: &ContextGraph, h: &Hypothesis, selected: bool, cap: usize) -> Self {
        Self {
            selected,
            ys: tail(&h.ys, cap),
            timestamps: tail(&h.timestamps, cap),
            ys_probs: tail(&h.ys_probs, cap),
            ys_len: h.ys.len(),
            timestamps_len: h.timestamps.len(),
            ys_probs_len: h.ys_probs.len(),
            truncated: h.ys.len().max(h.timestamps.len()).max(h.ys_probs.len()) > cap,
            log_prob: h.log_prob,
            context_state: h.context_state,
            level: graph.state(h.context_state).level,
            num_trailing_blanks: h.num_trailing_blanks,
        }
    }
}

/// A keyword firing at its pre-reset event.
#[derive(Debug, Clone, PartialEq)]
pub struct FiringTrace {
    /// Firings recorded by this stream, from 1, surviving resets.
    pub occurrence: u64,
    /// Firings since the last reset (`DecoderResult::firings`).
    pub epoch_occurrence: u64,
    pub keyword: String,
    pub clock: TraceClock,
    /// `Firing::context`: the decoder context of the keyword's first token.
    pub context: Vec<i64>,
    /// The selected path at the firing step, before the beam resets: its last
    /// `history_tokens` entries (original lengths in the `*_len` fields).
    pub ys: Vec<i64>,
    pub timestamps: Vec<i32>,
    pub ys_probs: Vec<f32>,
    pub ys_len: usize,
    pub timestamps_len: usize,
    pub ys_probs_len: usize,
    /// Some history was cut to its last entries.
    pub truncated: bool,
    pub fired_frame: i32,
    pub num_trailing_blanks: i32,
    pub score: f32,
    pub threshold: f32,
    /// The selected path first, then the best other hypotheses alive, at most four in all.
    pub hypotheses: Vec<TraceHypothesis>,
    /// Hypotheses alive at the firing step (`hypotheses` may hold fewer).
    pub hypotheses_total: usize,
}

impl FiringTrace {
    pub(crate) fn capture(
        graph: &ContextGraph,
        best: &Hypothesis,
        hyps: &[Hypothesis],
        firing: &Firing,
        keyword: &str,
        epoch_occurrence: u64,
        cap: usize,
    ) -> Self {
        let mut others: Vec<&Hypothesis> =
            hyps.iter().filter(|h| !std::ptr::eq(*h, best)).collect();
        others.sort_by(|a, b| {
            b.log_prob
                .partial_cmp(&a.log_prob)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut hypotheses = vec![TraceHypothesis::of(graph, best, true, cap)];
        hypotheses.extend(
            others
                .into_iter()
                .take(TOP_HYPOTHESES - 1)
                .map(|h| TraceHypothesis::of(graph, h, false, cap)),
        );
        Self {
            occurrence: 0,
            epoch_occurrence,
            keyword: keyword.to_owned(),
            clock: TraceClock::default(),
            context: firing.context.clone(),
            ys: tail(&best.ys, cap),
            timestamps: tail(&best.timestamps, cap),
            ys_probs: tail(&best.ys_probs, cap),
            ys_len: best.ys.len(),
            timestamps_len: best.timestamps.len(),
            ys_probs_len: best.ys_probs.len(),
            truncated: best.ys.len().max(best.timestamps.len()).max(best.ys_probs.len()) > cap,
            fired_frame: firing.frame,
            num_trailing_blanks: best.num_trailing_blanks,
            score: firing.score,
            threshold: firing.threshold,
            hypotheses,
            hypotheses_total: hyps.len(),
        }
    }
}

/// The frame a path's active keyword began on: the timestamp the TTL counts from.
pub(crate) fn keyword_first_frame(h: &Hypothesis, level: usize) -> Option<i32> {
    if level > 0 && h.timestamps.len() >= level {
        Some(h.timestamps[h.timestamps.len() - level])
    } else {
        h.timestamps.first().copied()
    }
}

/// A path that left the beam this step (TTL expiry, or pruning).
#[derive(Debug, Clone, PartialEq)]
pub struct TracePath {
    /// Encoder frame its active keyword began on (the one the TTL counts from).
    pub first_token_frame: Option<i32>,
    /// Keyword-prefix progress of its context state.
    pub level: usize,
    pub context_state: usize,
    pub log_prob: f64,
}

impl TracePath {
    pub(crate) fn of(graph: &ContextGraph, h: &Hypothesis) -> Self {
        let level = graph.state(h.context_state).level;
        Self {
            first_token_frame: keyword_first_frame(h, level),
            level,
            context_state: h.context_state,
            log_prob: h.log_prob,
        }
    }
}

/// The top hypothesis after a step's merge.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceTop {
    pub context_state: usize,
    /// Keyword-prefix progress: the graph depth of `context_state`.
    pub level: usize,
    pub first_token_frame: Option<i32>,
    pub log_prob: f64,
    pub num_trailing_blanks: i32,
    /// The last `history_tokens` token probabilities.
    pub ys_probs: Vec<f32>,
    pub ys_probs_len: usize,
    pub truncated: bool,
}

impl TraceTop {
    pub(crate) fn of(graph: &ContextGraph, h: &Hypothesis, cap: usize) -> Self {
        let level = graph.state(h.context_state).level;
        Self {
            context_state: h.context_state,
            level,
            first_token_frame: keyword_first_frame(h, level),
            log_prob: h.log_prob,
            num_trailing_blanks: h.num_trailing_blanks,
            ys_probs: tail(&h.ys_probs, cap),
            ys_probs_len: h.ys_probs.len(),
            truncated: h.ys_probs.len() > cap,
        }
    }
}

/// The fields `Hypotheses::add` keeps or drops for one side of a merge.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceMergeSide {
    /// The last `history_tokens` entries (original lengths below).
    pub timestamps: Vec<i32>,
    pub ys_probs: Vec<f32>,
    pub timestamps_len: usize,
    pub ys_probs_len: usize,
    pub truncated: bool,
    pub log_prob: f64,
}

impl TraceMergeSide {
    fn of(h: &Hypothesis, cap: usize) -> Self {
        Self {
            timestamps: tail(&h.timestamps, cap),
            ys_probs: tail(&h.ys_probs, cap),
            timestamps_len: h.timestamps.len(),
            ys_probs_len: h.ys_probs.len(),
            truncated: h.timestamps.len().max(h.ys_probs.len()) > cap,
            log_prob: h.log_prob,
        }
    }
}

/// Two paths with identical `ys` merged in the beam, on a keyword path.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceMerge {
    /// State of the entry already in the beam.
    pub context_state: usize,
    pub incoming_context_state: usize,
    /// The larger keyword-prefix progress of the two states.
    pub level: usize,
    pub existing: TraceMergeSide,
    pub incoming: TraceMergeSide,
    /// The incoming path's timestamps and probabilities replaced the existing ones
    /// (`MergeTimestamps::Likelier` and a higher score); otherwise the existing ones stay.
    pub incoming_kept: bool,
    /// The log-added score of the merged entry.
    pub merged_log_prob: f64,
}

impl TraceMerge {
    pub(crate) fn observe(
        existing: &Hypothesis,
        incoming: &Hypothesis,
        incoming_kept: bool,
        merged_log_prob: f64,
        cap: usize,
    ) -> Self {
        Self {
            context_state: existing.context_state,
            incoming_context_state: incoming.context_state,
            level: 0,
            existing: TraceMergeSide::of(existing, cap),
            incoming: TraceMergeSide::of(incoming, cap),
            incoming_kept,
            merged_log_prob,
        }
    }
}

/// The keyword check on the top hypothesis, when its state completes a keyword.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceMatch {
    /// The mean token probability the keyword's threshold is compared against.
    pub score: f32,
    pub threshold: f32,
    /// The top path has more trailing blanks than the configured minimum.
    pub trailing_blanks_ok: bool,
}

/// A keyword-extension candidate of one parent hypothesis at one step: the token that
/// would advance the parent along a keyword, and how its score compared with the beam's
/// cutoff and with the parent's best selected child. This is what explains an extension
/// that lost while the blank or another child survived. Scores are the beam's selection
/// scores (token log-probability plus the parent's score, before any keyword boost).
/// Ranks count strictly greater scores over the whole step; ties are not broken.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceExtension {
    pub parent_context_state: usize,
    /// Keyword-prefix progress of the parent (`0` is the root: a first-token candidate).
    pub parent_level: usize,
    pub parent_first_token_frame: Option<i32>,
    pub token: i64,
    /// The extension's token probability (`exp` of its log-softmax).
    pub token_prob: f32,
    pub score: f32,
    /// Candidates with a strictly greater score this step.
    pub rank: usize,
    /// The extension entered the top-k beam.
    pub selected: bool,
    /// The k-th (lowest selected) score: the beam's cutoff.
    pub cutoff_score: Option<f32>,
    /// The parent's best other selected child, when it had one.
    pub sibling_token: Option<i64>,
    pub sibling_score: Option<f32>,
}

/// A path the stale-token gap guard dropped when its newest keyword token was emitted.
/// Recorded whenever tracing is on and the guard is set, independent of the step window.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceGapDrop {
    /// Encoder frame the offending token was emitted on.
    pub frame: i32,
    pub clock: TraceClock,
    /// The offending gap in encoder frames (newest token minus the previous one).
    pub gap: i32,
    /// Keyword depth of the path's new state.
    pub level: usize,
    /// The active keyword's tokens and frames, last `history_tokens` of them (`level` gives
    /// the original count).
    pub tokens: Vec<i64>,
    pub frames: Vec<i32>,
    /// The offending consecutive pair.
    pub pair_tokens: [i64; 2],
    pub pair_frames: [i32; 2],
    /// The path's score BEFORE this emission (the parent's), as the port logs it.
    pub log_prob: f64,
}

impl TraceGapDrop {
    pub(crate) fn capture(h: &Hypothesis, level: usize, frame: i32, gap: i32, cap: usize) -> Self {
        let tokens_all = &h.ys[h.ys.len().saturating_sub(level)..];
        let frames_all = &h.timestamps[h.timestamps.len().saturating_sub(level)..];
        let (ys, ts) = (&h.ys, &h.timestamps);
        Self {
            frame,
            clock: TraceClock::default(),
            gap,
            level,
            tokens: tail(tokens_all, cap),
            frames: tail(frames_all, cap),
            pair_tokens: [ys[ys.len() - 2], ys[ys.len() - 1]],
            pair_frames: [ts[ts.len() - 2], ts[ts.len() - 1]],
            log_prob: h.log_prob,
        }
    }
}

/// One decoded encoder frame inside the window.
#[derive(Debug, Clone, PartialEq)]
pub struct StepTrace {
    /// Encoder frame on the clock of `KeywordResult::frames`.
    pub frame: i32,
    pub clock: TraceClock,
    /// Hypotheses scored this step (after TTL expiry).
    pub hypotheses_in: usize,
    /// Hypotheses after the beam's selection and merge, before a firing resets it.
    pub hypotheses_out: usize,
    /// TTL expiry emptied the beam and a blank hypothesis was added.
    pub refilled_blank: bool,
    pub top: Option<TraceTop>,
    /// `Some` when the top hypothesis completes a keyword (fired or not).
    pub matched: Option<TraceMatch>,
    /// A keyword fired on this step.
    pub fired: bool,
    /// Paths dropped by the active-path TTL at the start of the step.
    pub expired: Vec<TracePath>,
    /// Keyword paths none of whose children entered the top-k beam.
    pub pruned: Vec<TracePath>,
    /// Merges on keyword paths.
    pub merges: Vec<TraceMerge>,
    /// Keyword-extension candidates of this step's parents, keyword parents first, at most
    /// eight; never the whole vocabulary.
    pub extensions: Vec<TraceExtension>,
    /// Extension candidates not recorded because of that cap.
    pub extensions_omitted: usize,
}

/// What a stream recorded since the last [`super::KeywordStream::take_trace`].
#[derive(Debug, Clone, PartialEq)]
pub struct TraceRecords {
    pub identity: Arc<TraceIdentity>,
    pub firings: Vec<FiringTrace>,
    /// Firings dropped for capacity since the last drain.
    pub firings_dropped: u64,
    pub steps: Vec<StepTrace>,
    /// Window steps dropped for capacity since the last drain; nonzero means the window
    /// record is incomplete.
    pub steps_dropped: u64,
    /// Paths the stale-token gap guard dropped (empty when the guard is off).
    pub gap_drops: Vec<TraceGapDrop>,
    /// Gap-guard drop records dropped for capacity since the last drain.
    pub gap_drops_dropped: u64,
}

impl TraceRecords {
    pub fn is_empty(&self) -> bool {
        self.firings.is_empty()
            && self.steps.is_empty()
            && self.gap_drops.is_empty()
            && self.firings_dropped == 0
            && self.steps_dropped == 0
            && self.gap_drops_dropped == 0
    }

    /// Some record was dropped for capacity.
    pub fn truncated(&self) -> bool {
        self.firings_dropped > 0 || self.steps_dropped > 0 || self.gap_drops_dropped > 0
    }
}

/// Per-stream trace state; present only when tracing is on.
pub(crate) struct TraceState {
    pub(crate) config: TraceConfig,
    identity: Arc<TraceIdentity>,
    clock: TraceClock,
    window: Option<(i32, i32)>,
    /// Keep the NEWEST `window_steps` instead of the first ones (no window known).
    rolling: bool,
    /// Samples now being fed were declared padding by the caller.
    padding: bool,
    occurrences: u64,
    firings: Vec<FiringTrace>,
    firings_dropped: u64,
    steps: Vec<StepTrace>,
    steps_dropped: u64,
    gap_drops: Vec<TraceGapDrop>,
    gap_drops_dropped: u64,
}

impl TraceState {
    pub(crate) fn new(config: TraceConfig, identity: Arc<TraceIdentity>) -> Self {
        Self {
            config,
            identity,
            clock: TraceClock::default(),
            window: None,
            rolling: false,
            padding: false,
            occurrences: 0,
            firings: Vec::new(),
            firings_dropped: 0,
            steps: Vec::new(),
            steps_dropped: 0,
            gap_drops: Vec::new(),
            gap_drops_dropped: 0,
        }
    }

    pub(crate) fn push_gap_drop(&mut self, mut drop: TraceGapDrop) {
        drop.clock = self.clock;
        if self.gap_drops.len() >= GAP_DROPS_KEPT {
            self.gap_drops.remove(0);
            self.gap_drops_dropped += 1;
        }
        self.gap_drops.push(drop);
    }

    pub(crate) fn note_samples(&mut self, samples: usize) {
        self.clock.samples_fed = self.clock.samples_fed.saturating_add(samples as u64);
        if self.padding {
            self.clock.padding_samples_fed =
                self.clock.padding_samples_fed.saturating_add(samples as u64);
        }
    }

    pub(crate) fn declare_padding(&mut self, padding: bool) {
        self.padding = padding;
    }

    pub(crate) fn set_rolling(&mut self) {
        self.window = None;
        self.rolling = true;
    }

    /// Set the clocks for the chunk about to be decoded. `processed_frames` is the encoder
    /// position AFTER this chunk's advance of `chunk_shift`.
    pub(crate) fn set_clock(
        &mut self,
        feature_frames_ready: usize,
        processed_frames: usize,
        chunk_shift: usize,
        chunk_size: usize,
    ) {
        self.clock.feature_frames_ready = feature_frames_ready;
        self.clock.encoder_consumed_feature_frames = processed_frames;
        self.clock.encoder_window_end_feature_frame =
            processed_frames.saturating_sub(chunk_shift).saturating_add(chunk_size);
    }

    pub(crate) fn on_reset(&mut self, processed_frames: usize) {
        self.clock.epoch += 1;
        self.clock.epoch_origin_feature_frame = processed_frames;
        // Frames restart from zero, so a window on the old clock no longer applies.
        self.window = None;
        self.rolling = false;
    }

    pub(crate) fn set_window(&mut self, from_frame: i32, to_frame: i32) {
        self.rolling = false;
        self.window = Some((from_frame, to_frame));
    }

    /// Whether a step at `frame` is recorded; a qualifying step beyond capacity is counted.
    pub(crate) fn begin_step(&mut self, frame: i32) -> bool {
        if self.config.window_steps == 0 {
            return false;
        }
        if let Some((from, to)) = self.window {
            if frame < from || frame >= to {
                return false;
            }
        }
        if self.steps.len() >= self.config.window_steps {
            self.steps_dropped += 1;
            if !self.rolling {
                return false;
            }
            // Rolling: the oldest step makes room (counted above), the newest is kept.
            self.steps.remove(0);
        }
        true
    }

    pub(crate) fn push_step(&mut self, mut step: StepTrace) {
        step.clock = self.clock;
        self.steps.push(step);
    }

    pub(crate) fn push_firing(&mut self, mut firing: FiringTrace) {
        self.occurrences += 1;
        firing.occurrence = self.occurrences;
        firing.clock = self.clock;
        if self.firings.len() >= FIRINGS_KEPT {
            self.firings.remove(0);
            self.firings_dropped += 1;
        }
        self.firings.push(firing);
    }

    pub(crate) fn take(&mut self) -> TraceRecords {
        TraceRecords {
            identity: Arc::clone(&self.identity),
            firings: std::mem::take(&mut self.firings),
            firings_dropped: std::mem::take(&mut self.firings_dropped),
            steps: std::mem::take(&mut self.steps),
            steps_dropped: std::mem::take(&mut self.steps_dropped),
            gap_drops: std::mem::take(&mut self.gap_drops),
            gap_drops_dropped: std::mem::take(&mut self.gap_drops_dropped),
        }
    }
}
