//! Streaming keyword-spotting cascade (wake word, then command confirmation).
//!
//! The cascade runs on `rightkit_asr::kws`, the Rust keyword spotter (a port of the
//! upstream streaming transducer spotter), configured to the contract this app
//! qualified (one CPU thread, eight active paths, no trailing blanks, an
//! active-keyword TTL).
//! The same model, graph, thresholds, VAD pre-roll and timestamp cut policy apply
//! on every platform. Primary dictation ASR remains independent.
//!
//! Every wake/confirm detection also carries onset candidates (where the keyword
//! began, not where its first token was emitted), logged as one
//! `kws_onset_candidates` event; [`KwsSwitches`] picks which candidate becomes the
//! cut start, plus the runtime's accuracy levers and its speed levers (static
//! encoder batch, decoder-row cache, one model shared by the three spotters).
//! Defaults reproduce the emission-timestamp cut exactly.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rightkit_asr::kws as rust_kws;
/// The spotter's lever types, as [`KwsSwitches`] carries them.
pub use rightkit_asr::kws::{
    AcousticSearch, BeamStart, DecoderTableReport, FiringTrace, FricativeAbstain, FricativeParams,
    JoinerBatching, MergeTimestamps, StepTrace, ThresholdTokens, TraceClock, TraceConfig,
    TraceExtension, TraceGapDrop, TraceHypothesis, TraceIdentity, TraceMatch, TraceMerge,
    TraceMergeSide, TracePath, TraceRecords, TraceTop,
};
use rightkit_asr::parakeet_core::{TimedToken, TranscriptionResult};

/// The detail the spotter attaches to a result: score, threshold and onset candidates.
type KeywordDetection = rust_kws::KeywordDetection;

const SAMPLE_RATE: i32 = 16_000;
const FEATURE_DIM: i32 = 80;
pub const NUM_THREADS: i32 = 1;
pub const MAX_ACTIVE_PATHS: i32 = 8;
pub const NUM_TRAILING_BLANKS: i32 = 0;
const KEYWORDS_SCORE: f32 = 1.0;
const KEYWORDS_THRESHOLD: f32 = 0.25;
const KWS_FEED_CHUNK_SAMPLES: usize = SAMPLE_RATE as usize / 50;
const KWS_DECODE_STEP_SAMPLES: usize = SAMPLE_RATE as usize / 20;
const WAKE_WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * 3 / 2;
const WAKE_DECODE_TAIL_SAMPLES: usize = SAMPLE_RATE as usize / 5;
/// Fresh confirmation gets its own 200 ms flush. Streaming confirmation is
/// still decoded only at accepted sample boundaries, with no synthetic tail.
pub const FRESH_CONFIRMATION_TAIL_SAMPLES: usize = SAMPLE_RATE as usize / 5;
// Confirm needs the full allowed two-second keyword span before the wake
// timestamp. A 300 ms anchor clipped the shipped blended-Zephyr Send incident
// even though the higher-threshold confirm model recognized the intact window.
const CONFIRM_CONTEXT_SAMPLES: usize = SAMPLE_RATE as usize * 2;
pub const CONFIRM_STEP_SAMPLES: usize = SAMPLE_RATE as usize / 20;
pub const CONFIRM_TIMEOUT_SAMPLES: usize = SAMPLE_RATE as usize * 3 / 2;
/// How far before the wake candidate's start a cut or first-token sample may fall and still be
/// plausible for that candidate (0.6 s). A temporal plausibility window, not an occurrence
/// identity test. Used only by a non-default [`KwsCutSource`].
const CONFIRM_ALIGNED_GUARD_LEAD_SAMPLES: usize = SAMPLE_RATE as usize * 3 / 5;
/// `KwsCutSource::WakeWithinConfirm`: the wake candidate's cut is kept only when it lies within
/// this many samples (400 ms) of the confirmed command path's own cut.
const WAKE_CONFIRM_AGREE_SAMPLES: usize = SAMPLE_RATE as usize * 2 / 5;
/// A real two-word command completed in 0.72-1.12s across the 14-clip
/// production corpus. Reject paths spanning more than 2s so the spotter cannot
/// combine an old wake-like prefix with a much later action word.
const MAX_KEYWORD_SPAN_SECONDS: f32 = 2.0;
/// Active-keyword TTL (`ActivePath` expiry in `rightkit_asr::kws`): drop a beam path
/// whose active keyword began more than 38 encoder frames (1.52 s at 40 ms) ago, so
/// an old partial context path cannot survive into a later command attempt.
/// `resources/kws/manifest.json` records the same value as
/// `native_patch.active_keyword_ttl_frames`.
pub const ACTIVE_KEYWORD_TTL_FRAMES: i32 = 38;
/// The upstream spotter silently resets the stream after 1.5 s of trailing blanks (it
/// restarts timestamps while audio keeps flowing). The app's VAD/reset generation is
/// the sole reset owner, so the spotter keeps it off.
const TRAILING_BLANK_RESET: bool = false;

const ENCODER: &str = "encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx";
const DECODER: &str = "decoder-epoch-13-avg-2-chunk-16-left-64.onnx";
const JOINER: &str = "joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx";

struct KeywordResult {
    keyword: String,
    timestamps: Vec<f32>,
    /// Score, threshold and onset candidates of the detection.
    detection: Option<Box<KeywordDetection>>,
}

/// The spotter's onset-history and estimator settings
/// ([`rust_kws::OnsetConfig`] without the acoustic search and the D4 estimator, which
/// [`KwsSwitches`] carries separately); `Copy` so [`KwsSwitches`] stays `Copy`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KwsOnsetParams {
    pub encoder_history: usize,
    pub feature_history: usize,
    pub align_lead_frames: usize,
    pub posterior_threshold: f32,
    pub posterior_max_frames: usize,
    pub acoustic_max_frames: usize,
}

impl KwsOnsetParams {
    /// The values of [`rust_kws::OnsetConfig::default`].
    pub const DEFAULT: Self = Self {
        encoder_history: 64,
        feature_history: 320,
        align_lead_frames: 10,
        posterior_threshold: 1e-2,
        posterior_max_frames: 15,
        acoustic_max_frames: 30,
    };

    /// Bound retention to the shared front end's supported maximum (4096
    /// encoder frames, with four feature frames per encoder frame). Estimator
    /// lookbacks cannot request history beyond these retained windows.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=4096).contains(&self.encoder_history) {
            return Err("onset encoder_history must be 1 to 4096 frames".into());
        }
        if !(1..=16_384).contains(&self.feature_history) {
            return Err("onset feature_history must be 1 to 16384 frames".into());
        }
        if self.align_lead_frames > self.encoder_history
            || self.posterior_max_frames > self.encoder_history
            || self.align_lead_frames + self.posterior_max_frames > self.encoder_history
        {
            return Err("onset alignment and posterior lookbacks must fit encoder_history".into());
        }
        if self.acoustic_max_frames > self.feature_history {
            return Err("onset acoustic_max_frames must fit feature_history".into());
        }
        if !self.posterior_threshold.is_finite() || !(0.0..=1.0).contains(&self.posterior_threshold)
        {
            return Err("onset posterior_threshold must be finite and 0 to 1".into());
        }
        Ok(())
    }
}

/// Per-spotter timing the cascade's switches set.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SpotterTiming {
    decode_step_samples: usize,
    max_keyword_span_seconds: f32,
}

/// Decoder knobs of the spotter beyond the fixed model contract. Production
/// uses [`RustKwsTuning::PRODUCTION`] with any [`KwsSwitches`] applied.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RustKwsTuning {
    active_path_ttl_frames: Option<i32>,
    trailing_blank_reset: bool,
    /// Evaluate the joiner softmax in f64 (stock upstream GCC builds do); the
    /// production contract uses f32.
    softmax_f64: bool,
    /// Keep onset history and estimate every detection's onset. Never changes
    /// what is detected; costs one decoder and one joiner call per detection.
    onset: bool,
    acoustic_search: AcousticSearch,
    /// D4 fricative onset estimator (default off).
    fricative: FricativeParams,
    threshold_tokens: rust_kws::ThresholdTokens,
    joiner_batching: rust_kws::JoinerBatching,
    merge_timestamps: rust_kws::MergeTimestamps,
    portable_math: bool,
    /// Speed levers; none changes a result where its precondition holds (see
    /// [`KwsSwitches::static_batch`]).
    static_batch: bool,
    decoder_cache: bool,
    /// Wake, confirm and hey-wake spotters run on one [`rust_kws::KwsModel`].
    shared_model: bool,
    /// Firing and no-fire trace; observation only (default off).
    trace: Option<KwsTraceConfig>,
    /// Stale-token gap guard (default off).
    stale_gap_frames: Option<i32>,
    /// Speed levers C7 and C8 (default off).
    encoder_state_reuse: bool,
    decoder_table: bool,
    /// Shared front end C6 (default off).
    shared_front_end: bool,
    /// Intra-op threads per ONNX session ([`KwsSwitches::threads`]).
    threads: usize,
    num_trailing_blanks: i32,
    onset_params: KwsOnsetParams,
    /// Samples between streaming decode checkpoints.
    decode_step_samples: usize,
    /// Longest accepted first-to-last keyword token span.
    max_keyword_span_seconds: f32,
}

impl RustKwsTuning {
    /// The production contract (active-keyword TTL, no trailing-blank reset, f32
    /// softmax), with onset estimation on (logging only: the cut stays on the emission timestamp).
    const PRODUCTION: Self = Self {
        active_path_ttl_frames: Some(ACTIVE_KEYWORD_TTL_FRAMES),
        trailing_blank_reset: TRAILING_BLANK_RESET,
        softmax_f64: false,
        onset: true,
        acoustic_search: AcousticSearch::Legacy,
        fricative: FricativeParams::DEFAULT,
        threshold_tokens: rust_kws::ThresholdTokens::Upstream,
        joiner_batching: rust_kws::JoinerBatching::Beam,
        merge_timestamps: rust_kws::MergeTimestamps::First,
        portable_math: false,
        static_batch: false,
        decoder_cache: false,
        shared_model: false,
        trace: None,
        stale_gap_frames: None,
        encoder_state_reuse: false,
        decoder_table: false,
        shared_front_end: false,
        threads: NUM_THREADS as usize,
        num_trailing_blanks: NUM_TRAILING_BLANKS,
        onset_params: KwsOnsetParams::DEFAULT,
        decode_step_samples: KWS_DECODE_STEP_SAMPLES,
        max_keyword_span_seconds: MAX_KEYWORD_SPAN_SECONDS,
    };
}

/// Which onset candidate becomes the wake candidate's cut start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KwsOnsetMethod {
    /// The first token's emission timestamp (today's cut).
    Emission,
    /// Earliest frame of the first token's probability run before its frame.
    Posterior,
    /// Viterbi forced alignment of the keyword's tokens: first token's frame.
    Aligned,
    /// An acoustic candidate from the configured energy-envelope search.
    Acoustic,
    /// The D4 fricative estimator's hiss-run start (needs `KwsSwitches::fricative.enabled`).
    Fricative,
}

impl KwsOnsetMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Emission => "emission",
            Self::Posterior => "posterior",
            Self::Aligned => "aligned",
            Self::Acoustic => "acoustic",
            Self::Fricative => "fricative",
        }
    }

    /// This method's candidate, when it was estimated.
    fn candidate(self, onset: &rust_kws::KeywordOnset) -> Option<rust_kws::OnsetEstimate> {
        match self {
            Self::Emission => Some(onset.emission),
            Self::Posterior => onset.posterior,
            Self::Aligned => onset.aligned,
            Self::Acoustic => onset.acoustic,
            Self::Fricative => onset.fricative,
        }
    }
}

/// Where a confirmed command's cut starts. Only [`KwsCutSource::WakeCandidate`] (the default)
/// keeps the wake candidate's cut ([`KwsSwitches::onset`]); the others re-decide it from the
/// confirm path's own first-token sample when the command latches, and are a host option, not a
/// qualified default. A confirm sample outside the wake candidate's plausibility window (see
/// the guard in `guarded_confirm_cut`) is not trusted for that candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KwsCutSource {
    /// The wake candidate's chosen onset ([`KwsSwitches::onset`]): today's cut.
    WakeCandidate,
    /// The confirm path's forced-alignment first-token frame, shifted by
    /// [`KwsSwitches::cut_source_shift_ms`]. Falls back to the wake candidate's cut when that
    /// estimate or its stream origin is missing or outside the candidate's plausibility window.
    ConfirmAligned,
    /// The confirm path's forced-alignment first-token sample with ZERO shift
    /// (`cut_source_shift_ms` is ignored). A missing estimate or one outside the candidate's
    /// plausibility window yields NO trusted boundary (the command keeps its text and carries
    /// no timed token, so the worker attempts no audio cut), never the wake candidate's cut.
    ConfirmFirstToken,
    /// The earlier of the confirm first-token sample (aligned, zero shift) and the wake
    /// candidate's cut. Abstains like `ConfirmFirstToken`.
    ConfirmEarlier,
    /// The wake candidate's cut when it lies within 400 ms of the confirm first-token sample
    /// (aligned, zero shift), else the confirm sample. Abstains like `ConfirmFirstToken`.
    WakeWithinConfirm,
    /// Like `ConfirmFirstToken` with the confirm path's first-token EMISSION timestamp (stream
    /// origin + first keyword timestamp, a missing origin, missing or negative or non-finite
    /// timestamp being "no estimate") instead of the aligned estimate.
    ConfirmFirstTokenEmission,
    /// `ConfirmEarlier` with the emission confirm sample.
    ConfirmEarlierEmission,
    /// `WakeWithinConfirm` with the emission confirm sample.
    WakeWithinConfirmEmission,
}

/// Which timestamp of the confirm path a cut source reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfirmTimestamp {
    /// The forced-alignment first-token estimate (`onset.aligned`).
    Aligned,
    /// The confirm stream's first keyword timestamp (emission), independent of the optional
    /// onset estimators.
    Emission,
}

impl ConfirmTimestamp {
    fn as_str(self) -> &'static str {
        match self {
            ConfirmTimestamp::Aligned => "aligned",
            ConfirmTimestamp::Emission => "emission",
        }
    }
}

/// Which side a cut label selected, for the onset report.
fn cut_side(label: &str) -> &'static str {
    match label {
        "confirm_aligned"
        | "confirm_first_token"
        | "confirm_earlier"
        | "confirm_beyond_400ms"
        | "confirm_first_token_emission"
        | "confirm_earlier_emission"
        | "confirm_beyond_400ms_emission" => "confirm",
        l if l.starts_with("no_trusted_boundary") => "none",
        _ => "wake",
    }
}

/// Cut evidence of one latched command under a non-default source, as reported.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CutEvidence {
    wake_cut: Option<usize>,
    /// The confirm sample the source used, on the outer clock (origin + raw).
    confirm_estimate: Option<usize>,
    /// The timestamp method that produced `confirm_estimate`: `aligned` or `emission`.
    method: &'static str,
    /// The RAW confirm sample before any origin mapping (stream-relative).
    raw_confirm_sample: Option<usize>,
    selected: &'static str,
}

impl KwsCutSource {
    fn confirm_timestamp(self) -> ConfirmTimestamp {
        match self {
            KwsCutSource::ConfirmFirstTokenEmission
            | KwsCutSource::ConfirmEarlierEmission
            | KwsCutSource::WakeWithinConfirmEmission => ConfirmTimestamp::Emission,
            _ => ConfirmTimestamp::Aligned,
        }
    }
}

/// Switches for the Rust keyword spotter. No environment is read; every build uses
/// [`KwsSwitches::default`], which is today's behaviour. The onset switches move
/// only the cut; decoder switches can change detections and require qualification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KwsSwitches {
    /// `emission` | `posterior` | `aligned` | `acoustic` | `fricative` |
    /// `off`: which candidate becomes the cut start; `None` (`off`) also stops
    /// estimating and logging candidates. Default `emission`.
    pub onset: Option<KwsOnsetMethod>,
    /// Search used to estimate the acoustic candidate. Default `Legacy`.
    pub acoustic_search: AcousticSearch,
    /// D4 fricative estimator: `enabled`, `window_ms`, `floor_ms`, `rise`, `min_run_ms`,
    /// `vowel_ms`, `tilt_margin`. Default off; checked by [`KwsSwitches::validate`].
    pub fricative: FricativeParams,
    /// Strict D4: with `onset` `fricative`, a wake detection whose D4 candidate is absent or
    /// abstained gets NO cut boundary (the confirmed command carries no timed token and the
    /// worker skips the audio cut, `piece_not_found`) instead of falling back to the
    /// emission timestamp. Default off: the absent-candidate fallback to emission stays an
    /// explicit legacy-control path (`chosen_source` `emission`). Needs `onset` `fricative`.
    pub fricative_strict: bool,
    /// signed milliseconds added to the chosen
    /// candidate, -1000..=1000 (ignored when `onset` is `None`). Default 0.
    pub onset_shift_ms: i32,
    /// `upstream` | `matched`. Default `upstream`.
    pub threshold_tokens: rust_kws::ThresholdTokens,
    /// `beam` | `per_hypothesis`. Default `beam`.
    pub joiner_batching: rust_kws::JoinerBatching,
    /// `first` | `likelier`: whose timestamps a
    /// merged beam path keeps. Default `first` (upstream, which can keep a stale
    /// early first-token time).
    pub merge_timestamps: rust_kws::MergeTimestamps,
    /// `0` | `1`. Default `0`.
    pub portable_math: bool,
    /// frames (1..=1000) | `off`. Default 38.
    pub ttl_frames: Option<i32>,
    /// `0` | `1`: pin the encoder's batch dimension to
    /// 1 (~10% less KWS CPU). Bit-identical for the shipped int8 encoder on
    /// x86-64 Linux; re-verify on each OS (`kws_static_batch_encoder_is_bit_identical`
    /// in rightkit-asr). Default `0`.
    pub static_batch: bool,
    /// `0` | `1`: reuse decoder rows per token context
    /// (~5% less KWS CPU). Exact where the decoder is batch-invariant
    /// (`kws_decoder_is_batch_invariant` in rightkit-asr). Default `0`.
    pub decoder_cache: bool,
    /// `0` | `1`: the wake, confirm and hey-wake
    /// spotters share one set of ONNX sessions (~46 MB less RSS for three).
    /// Default `0`.
    pub shared_model: bool,
    /// Firing and no-fire trace (default off); see [`KwsTraceConfig`] and
    /// [`Kws::take_trace`]. Observation only: detections, frames, probabilities and cuts
    /// are identical with it on or off.
    pub trace: Option<KwsTraceConfig>,
    /// Stale-token gap guard (arms B28 to B30): `Some(G)`, G in 1..=1000 encoder frames
    /// (40 ms each), drops a beam path whose newest keyword token came more than G frames
    /// after the previous keyword token of the same path. `None` (default) is today's
    /// behaviour, unchanged. It changes what can be detected and requires qualification; it
    /// applies to the wake and confirm spotters, not the hey-wake spotter.
    pub gap_guard: Option<i32>,
    /// `0` | `1` (plan arm C7): write the encoder's next states into the previous call's
    /// buffers instead of allocating new ones. Identical outputs by construction (the same
    /// bytes are copied); any speedup is unverified until a replay on the same binary compares
    /// detections, timestamps, scores and timings. Default `0`.
    pub encoder_state_reuse: bool,
    /// `0` | `1` (plan arm C8): precompute the decoder rows of every context a keyword path
    /// can produce and serve them from that table (the same batch-invariance precondition as
    /// `decoder_cache`, which it overrides when both are on). Identical outputs where the
    /// decoder is batch-invariant; unverified until the same replay comparison. Default `0`.
    pub decoder_table: bool,
    /// `0` | `1` (plan arm C6): ONE encoder run feeds both beams. The wake stream keeps a ring
    /// of its encoder frames on the wake clock and the LIVE confirm beam decodes them, from
    /// the frame 2.0 s before the wake candidate (clamped to the stream origin, then to the
    /// ring, each reported), instead of running a second encoder from that anchor. It changes
    /// confirm's inputs and so can change detections. Fresh confirmation is unchanged and its
    /// outcomes are reported separately. Thresholds are not changed. The cascade entry points
    /// (`transcribe_cascade_result_with_wake_start` and the scheduled probe, the route
    /// `kws_onset_replay` uses) support it; the separate experimental full-command entry
    /// `transcribe_continuous_full_command_result` has no wake stream and REJECTS it before it
    /// changes any state. Default `0`.
    pub shared_front_end: bool,
    /// Where a confirmed command's cut starts. Default [`KwsCutSource::WakeCandidate`] (today's
    /// cut). Any other source needs [`KwsSwitches::onset`] estimation for the aligned sources,
    /// cannot be combined with `fricative_strict` or `shared_front_end` (checked by
    /// [`KwsSwitches::validate`]), and changes only the cut, never what is detected.
    pub cut_source: KwsCutSource,
    /// Milliseconds added to the [`KwsCutSource::ConfirmAligned`] cut only; every other source
    /// ignores it. Default -200.
    pub cut_source_shift_ms: i32,
    /// A wake detection whose START is less than this many samples after the START of the
    /// previous ACCEPTED wake detection is dropped, whether or not that candidate is still
    /// held (it survives candidate expiry), until the clock resets (`reset_stream` or a cascade
    /// invalidation). Applied before the overlap rule, which still runs. Default 0 = off
    /// (overlap-only debounce, today's rule). 16_000 = 1 s.
    pub wake_refractory_samples: usize,
    /// Also drop a wake detection whose start is within this many samples, either side, of the
    /// HELD candidate's start, on top of the overlap rule. Only acts while a candidate is held.
    /// Default 0 = off.
    pub wake_start_proximity_samples: usize,
    /// Beam width of the wake and confirm spotters, 1 to 16 (the hey-wake
    /// spotter keeps [`MAX_ACTIVE_PATHS`]). Default 8.
    pub max_active_paths: i32,
    /// Blank frames a keyword must be followed by before it fires, 0 to 160. Default 0.
    pub num_trailing_blanks: i32,
    /// The upstream 1.5 s trailing-blank stream reset. Default `false`.
    pub trailing_blank_reset: bool,
    /// Joiner log-softmax in `f64`. Default `false`.
    pub softmax_f64: bool,
    /// Onset history and estimator settings. Default the spotter's own.
    pub onset_params: KwsOnsetParams,
    /// Samples between streaming decode checkpoints, at most 102400 (6.4 s).
    /// Default 800 (50 ms); 0 uses one sample.
    pub decode_step_samples: usize,
    /// Audio the confirmation stream starts before the wake onset. Default 2 s. Not supported
    /// with `shared_front_end` (checked by [`KwsSwitches::validate`]). At most 6.4 s.
    pub confirm_context_samples: usize,
    /// How long a wake candidate stays open after its end, at most 6.4 s. Default 1.5 s.
    pub confirm_timeout_samples: usize,
    /// Synthetic silence a fresh confirmation appends, at most 6.4 s. Default 200 ms.
    pub fresh_tail_samples: usize,
    /// Longest accepted first-to-last keyword token span, finite, positive & at most 6.4 s.
    /// Default 2.0 s.
    pub max_keyword_span_seconds: f32,
    /// Intra-op threads of each ONNX session, at most 16. Default 1 (`NUM_THREADS`);
    /// 0 is treated as 1.
    pub threads: usize,
}

/// Which trace records to keep. Default off (`KwsSwitches::trace` `None`); every value is
/// bounded and a record dropped for capacity is counted, never silently omitted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KwsTraceConfig {
    /// Snapshot every firing at its pre-reset beam event, on the wake and the confirm
    /// stream (`FiringTrace`).
    pub firing: bool,
    /// No-fire window on the CONFIRM stream: record every decoded encoder frame within this
    /// many frames (40 ms each) before the wake candidate's first token and after its end
    /// (`StepTrace`). `0` records no steps.
    pub window_frames: u32,
    /// Most steps held between drains (needs `window_frames` above `0`; see
    /// [`KwsTraceConfig::DEFAULT_CAPACITY_STEPS`]).
    pub capacity_steps: usize,
    /// Most entries of each path history kept per record (the last ones; original lengths
    /// and a `truncated` flag are recorded). `0` means 16.
    pub history_tokens: usize,
}

impl KwsTraceConfig {
    pub const DEFAULT_CAPACITY_STEPS: usize = 512;
}

/// The decoder path each cascade spotter takes ([`Kws::decoder_table_report`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KwsDecoderTableReport {
    pub wake: rust_kws::DecoderTableReport,
    pub confirm: rust_kws::DecoderTableReport,
}

/// One audio hand-off or expiry on a cascade stream, on the outer sample clock: the feed
/// and expiry schedule the no-fire window needs.
#[derive(Clone, Debug, PartialEq)]
pub struct KwsFeedEvent {
    /// `wake` or `confirm`.
    pub stage: &'static str,
    /// `create`, `suffix`, `duplicate`, `stale`, `gap`, `destroy` or `expire`; with the shared
    /// front end the live confirm beam logs `shared_create` and `shared_follow`.
    pub event: &'static str,
    /// Cascade resets and invalidations since load.
    pub epoch: u64,
    pub snapshot_start_sample: Option<usize>,
    pub live_total_sample: Option<usize>,
    /// Absolute sample where the stream's clock starts (confirm origin).
    pub origin_sample: Option<usize>,
    pub fed_until_sample: Option<usize>,
    pub accepted_until_sample: Option<usize>,
    pub pending_samples: Option<usize>,
    pub candidate_id: Option<u64>,
    pub candidate_start_sample: Option<usize>,
    pub candidate_end_sample: Option<usize>,
    /// The sample after which the candidate expires: its end plus the confirm timeout.
    pub expiry_deadline_sample: Option<usize>,
}

/// One stream's beam records, collected before the stream was destroyed or drained.
#[derive(Clone, Debug, PartialEq)]
pub struct KwsStreamTrace {
    /// `wake`, `confirm`, `fresh_confirm` or `idle_wake`.
    pub stage: &'static str,
    /// Absolute sample where that stream's clock starts.
    pub origin_sample: Option<usize>,
    /// The wake candidate this stream belongs to (`confirm`, `fresh_confirm`).
    pub candidate_id: Option<u64>,
    /// How the step window was chosen: `candidate` (frames around the wake candidate on the
    /// stream's clock), `unknown_origin` (the stream's absolute origin was unknown, so the
    /// newest steps are kept instead), or `None` (no step window).
    pub window: Option<&'static str>,
    /// `fresh_confirm` only: real PCM samples and synthetic tail samples actually fed before
    /// the decode stopped. Use these, not `TraceClock::samples_fed`, for real-audio extent.
    pub real_samples_fed: Option<usize>,
    pub tail_samples_fed: Option<usize>,
    /// Cascade epoch when collected.
    pub epoch: u64,
    pub records: rust_kws::TraceRecords,
}

/// What [`Kws::take_trace`] returns. Dropped counters are nonzero when a buffer filled;
/// then the record is incomplete.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KwsTrace {
    pub streams: Vec<KwsStreamTrace>,
    pub streams_dropped: u64,
    pub feeds: Vec<KwsFeedEvent>,
    pub feeds_dropped: u64,
}

impl Default for KwsSwitches {
    fn default() -> Self {
        let tuning = RustKwsTuning::PRODUCTION;
        Self {
            onset: Some(KwsOnsetMethod::Emission),
            acoustic_search: tuning.acoustic_search,
            fricative: tuning.fricative,
            fricative_strict: false,
            onset_shift_ms: 0,
            threshold_tokens: tuning.threshold_tokens,
            joiner_batching: tuning.joiner_batching,
            merge_timestamps: tuning.merge_timestamps,
            portable_math: tuning.portable_math,
            ttl_frames: tuning.active_path_ttl_frames,
            static_batch: tuning.static_batch,
            decoder_cache: tuning.decoder_cache,
            shared_model: tuning.shared_model,
            trace: tuning.trace,
            gap_guard: tuning.stale_gap_frames,
            encoder_state_reuse: tuning.encoder_state_reuse,
            decoder_table: tuning.decoder_table,
            shared_front_end: tuning.shared_front_end,
            cut_source: KwsCutSource::WakeCandidate,
            cut_source_shift_ms: -200,
            wake_refractory_samples: 0,
            wake_start_proximity_samples: 0,
            max_active_paths: MAX_ACTIVE_PATHS,
            num_trailing_blanks: tuning.num_trailing_blanks,
            trailing_blank_reset: tuning.trailing_blank_reset,
            softmax_f64: tuning.softmax_f64,
            onset_params: tuning.onset_params,
            decode_step_samples: tuning.decode_step_samples,
            confirm_context_samples: CONFIRM_CONTEXT_SAMPLES,
            confirm_timeout_samples: CONFIRM_TIMEOUT_SAMPLES,
            fresh_tail_samples: FRESH_CONFIRMATION_TAIL_SAMPLES,
            max_keyword_span_seconds: tuning.max_keyword_span_seconds,
            threads: tuning.threads,
        }
    }
}

impl KwsSwitches {
    /// Reject D4 settings the estimator could not honour (see [`FricativeParams::validate`]),
    /// and D4 selected or enabled while onset estimation is off. Run on every load.
    pub fn validate(&self) -> Result<(), String> {
        self.fricative.validate()?;
        self.onset_params.validate()?;
        // Keep replay allocations and scheduling within the existing 6.4 s
        // shared-front-end horizon. Zero retains its documented meanings:
        // decode step / threads become one; context, timeout and tail may be off.
        const MAX_REPLAY_SAMPLES: usize = rust_kws::DEFAULT_RING_FRAMES * ENCODER_FRAME_SAMPLES;
        const MAX_REPLAY_SECONDS: f32 = MAX_REPLAY_SAMPLES as f32 / SAMPLE_RATE as f32;
        if !(1..=16).contains(&self.max_active_paths) {
            return Err("max_active_paths must be 1 to 16".into());
        }
        if !(0..=rust_kws::DEFAULT_RING_FRAMES as i32).contains(&self.num_trailing_blanks) {
            return Err("num_trailing_blanks must be 0 to 160 encoder frames".into());
        }
        if self.threads > 16 {
            return Err("threads must be 0 to 16 (0 uses one thread)".into());
        }
        for (name, samples) in [
            ("decode_step_samples", self.decode_step_samples),
            ("confirm_context_samples", self.confirm_context_samples),
            ("confirm_timeout_samples", self.confirm_timeout_samples),
            ("fresh_tail_samples", self.fresh_tail_samples),
        ] {
            if samples > MAX_REPLAY_SAMPLES {
                return Err(format!("{name} must be 0 to {MAX_REPLAY_SAMPLES} samples"));
            }
        }
        if !self.max_keyword_span_seconds.is_finite()
            || self.max_keyword_span_seconds <= 0.0
            || self.max_keyword_span_seconds > MAX_REPLAY_SECONDS
        {
            return Err(format!(
                "max_keyword_span_seconds must be finite, positive and at most {MAX_REPLAY_SECONDS}"
            ));
        }
        if self.fricative.enabled && self.onset.is_none() {
            return Err("fricative.enabled needs onset estimation (onset is off)".into());
        }
        if self.onset == Some(KwsOnsetMethod::Fricative) && !self.fricative.enabled {
            return Err("onset fricative needs fricative.enabled".into());
        }
        if self.fricative_strict && self.onset != Some(KwsOnsetMethod::Fricative) {
            return Err("fricative_strict needs onset fricative".into());
        }
        if self.gap_guard.is_some_and(|gap| !(1..=1000).contains(&gap)) {
            return Err("gap_guard must be 1 to 1000 encoder frames".into());
        }
        if self.cut_source != KwsCutSource::WakeCandidate {
            if self.fricative_strict {
                return Err(
                    "cut_source needs the wake candidate to carry a cut: not with fricative_strict"
                        .into(),
                );
            }
            if self.shared_front_end {
                return Err("cut_source is not qualified with shared_front_end".into());
            }
        }
        if self.shared_front_end && self.confirm_context_samples != CONFIRM_CONTEXT_SAMPLES {
            return Err("confirm_context_samples is not supported with shared_front_end".into());
        }
        if let Some(trace) = self.trace {
            if !trace.firing && trace.window_frames == 0 {
                return Err("trace records nothing: set firing or window_frames".into());
            }
            if trace.window_frames > 0 && !(1..=65_536).contains(&trace.capacity_steps) {
                return Err("trace capacity_steps must be 1 to 65536 with a window".into());
            }
        }
        Ok(())
    }

    fn tuning(&self) -> RustKwsTuning {
        RustKwsTuning {
            active_path_ttl_frames: self.ttl_frames,
            onset: self.onset.is_some(),
            acoustic_search: self.acoustic_search,
            fricative: self.fricative,
            threshold_tokens: self.threshold_tokens,
            joiner_batching: self.joiner_batching,
            merge_timestamps: self.merge_timestamps,
            portable_math: self.portable_math,
            static_batch: self.static_batch,
            decoder_cache: self.decoder_cache,
            shared_model: self.shared_model,
            trace: self.trace,
            stale_gap_frames: self.gap_guard,
            encoder_state_reuse: self.encoder_state_reuse,
            decoder_table: self.decoder_table,
            shared_front_end: self.shared_front_end,
            threads: self.threads.max(1),
            num_trailing_blanks: self.num_trailing_blanks,
            trailing_blank_reset: self.trailing_blank_reset,
            softmax_f64: self.softmax_f64,
            onset_params: self.onset_params,
            decode_step_samples: self.decode_step_samples.max(1),
            max_keyword_span_seconds: self.max_keyword_span_seconds,
        }
    }

    fn label_threshold_tokens(&self) -> &'static str {
        match self.threshold_tokens {
            rust_kws::ThresholdTokens::Upstream => "upstream",
            rust_kws::ThresholdTokens::MatchedKeyword => "matched",
        }
    }

    fn label_joiner_batching(&self) -> &'static str {
        match self.joiner_batching {
            rust_kws::JoinerBatching::Beam => "beam",
            rust_kws::JoinerBatching::PerHypothesis => "per_hypothesis",
        }
    }

    fn label_merge_timestamps(&self) -> &'static str {
        match self.merge_timestamps {
            rust_kws::MergeTimestamps::First => "first",
            rust_kws::MergeTimestamps::Likelier => "likelier",
        }
    }
}

/// One detection's onset candidates on the outer sample clock,
/// as logged in the `kws_onset_candidates` event. Sample positions are
/// absolute when `origin_sample` is known, else relative to the decoded audio.
#[derive(Clone, Debug, PartialEq)]
pub struct KwsOnsetReport {
    /// `wake`, `confirm`, `fresh_confirm` or `idle_wake`.
    pub stage: &'static str,
    pub keyword: String,
    /// Absolute sample where the stream's clock starts.
    pub origin_sample: Option<usize>,
    /// Live (accepted) sample count when the detection was decoded.
    pub live_sample: Option<usize>,
    /// What today's cut uses: origin + first token timestamp.
    pub emission_sample: usize,
    pub aligned_sample: Option<usize>,
    pub posterior_sample: Option<usize>,
    pub acoustic_sample: Option<usize>,
    pub acoustic_rule: Option<&'static str>,
    /// D4 hiss-run start; `None` unless enabled, and when it abstained.
    pub fricative_sample: Option<usize>,
    /// D4 abstention label (`abstain:no_transition`, ...); `Some` only when enabled and it abstained.
    pub fricative_abstain: Option<&'static str>,
    /// The D4 estimate as a feature frame (10 ms) on the stream's own clock, whatever the
    /// origin; `None` when D4 had no estimate.
    pub fricative_relative_feature: Option<i32>,
    /// `Some("no_origin")` when D4 has an estimate that cannot be placed on the outer
    /// sample clock because `origin_sample` is unknown (then `fricative_sample` is `None`).
    pub fricative_unmappable: Option<&'static str>,
    /// Emission (encoder, 40 ms) frame of each token, on the stream's clock.
    pub token_frames: Vec<i32>,
    /// Forced-alignment frame of each token.
    pub aligned_frames: Vec<i32>,
    pub fired_frame: i32,
    /// Paths the stale-token gap guard had dropped on this stream since its last reset, up to
    /// this firing (`0` with the guard off), and of those the ones dropped in the firing's
    /// chunk.
    pub gap_drops: u64,
    pub gap_drops_chunk: u64,
    /// The largest gap between consecutive emitted keyword tokens, in encoder frames
    /// (observed with the guard on or off), and whether it exceeds the guard's limit.
    pub max_gap_frames: i32,
    pub gap_violation: bool,
    pub token_probs: Vec<f32>,
    pub score: f32,
    pub threshold: f32,
    /// The configured method and selected cut. For a latched confirmation,
    /// the selection comes from the wake candidate that owns the command.
    pub chosen: &'static str,
    pub chosen_sample: Option<usize>,
    /// The method whose candidate `chosen_sample` used (`emission` on fallback, which for a
    /// non-emission method is the legacy-control fallback, not a success of that method;
    /// `no_boundary` under `fricative_strict` when D4 had no candidate, with `chosen_sample`
    /// `None`).
    pub chosen_source: Option<&'static str>,
    /// The wake candidate's cut start a latch would use after this detection.
    pub cut_sample: Option<usize>,
    /// The cut path's emission sample: origin + first keyword timestamp, `None` when the
    /// origin or the timestamp is missing or invalid (unlike `emission_sample`, which
    /// defaults them to 0).
    pub emission_sample_checked: Option<usize>,
    /// Non-default cut sources, latched confirm stages: the wake candidate's cut.
    pub cut_wake_sample: Option<usize>,
    /// The confirm path's first-token sample for the source's timestamp method (None when
    /// missing).
    pub cut_confirm_sample: Option<usize>,
    /// Which side the cut took: `wake`, `confirm` or `none` (no trusted boundary).
    pub cut_selected: Option<&'static str>,
    /// The timestamp method `cut_confirm_sample` came from: `aligned` or `emission`.
    pub cut_method: Option<&'static str>,
    /// The RAW (stream-relative, before origin mapping) confirm sample. With
    /// `origin_sample` this reproduces `cut_confirm_sample`.
    pub cut_raw_confirm_sample: Option<usize>,
    /// Confirm stages: this detection latched a command.
    pub latched: bool,
    /// Identity of the wake candidate this report belongs to: on a `wake` report, the id the
    /// clock assigned when it ADOPTED the candidate (`None` when it was rejected as an
    /// overlap); on `confirm` and `fresh_confirm`, the id of the candidate that owns the
    /// command. `None` for `idle_wake`.
    pub candidate_id: Option<u64>,
    /// `wake` reports only: whether the clock adopted this detection as the candidate.
    pub adopted: Option<bool>,
    /// Shared front end only, `confirm` reports on a shared beam: where the beam started and
    /// the ring state. `None` for every other report, and always with the switch off.
    pub shared: Option<KwsSharedInfo>,
}

/// Shared-front-end counters ([`Kws::shared_front_end_stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KwsSharedStats {
    pub beams_created: u64,
    pub origin_clamped: u64,
    pub ring_truncated: u64,
    /// `feed_shared_confirmation` failures (a missing wake stream, an evicted or replaced
    /// source, a reset): the cascade drops the beam and reports the error.
    pub errors: u64,
}

/// A shared confirm beam's context and retention state, so a clamped or truncated start is
/// never read as the full requested context.
#[derive(Clone, Debug, PartialEq)]
pub struct KwsSharedInfo {
    /// `full`, `origin_clamped`, `ring_truncated` or `origin_clamped_and_ring_truncated`.
    pub state: &'static str,
    /// Wake-clock encoder frame the beam started on, and the requested one (negative before
    /// the stream's origin; the requested start is floor((sample - origin) / 640)).
    pub start_frame: i32,
    pub requested_frame: i64,
    pub origin_clamped: bool,
    pub ring_truncated: bool,
    /// Frames from the requested and from the actual start to the newest retained frame at
    /// creation.
    pub requested_context_frames: i64,
    pub actual_context_frames: i64,
    /// Ring frames and bytes held when the beam was made.
    pub ring_frames: usize,
    pub ring_bytes: usize,
    /// Why this detection's onset is missing although estimation is on (evicted history).
    pub onset_unavailable: Option<&'static str>,
}

/// The latest latched confirm report in `reports` (oldest first; `confirm` or
/// `fresh_confirm`, both carry the owning candidate's id) and, separately, its owning wake
/// report: the adopted `wake` report BEFORE it whose `candidate_id` equals the confirm
/// report's. The confirm evidence is returned even when the owner is `None` (ring
/// truncation, absent), so callers can record it and mark the owner missing. Identity,
/// never boundary equality: a rejected overlap or an adopted detection with identical sample
/// values is never mistaken for the owner. `None` when no confirm report latched.
pub fn latched_owner(
    reports: &[KwsOnsetReport],
) -> Option<(Option<&KwsOnsetReport>, &KwsOnsetReport)> {
    let at = reports.iter().rposition(|report| {
        matches!(report.stage, "confirm" | "fresh_confirm") && report.latched
    })?;
    let confirm = &reports[at];
    let wake = confirm.candidate_id.and_then(|id| {
        reports[..at].iter().rev().find(|report| {
            report.stage == "wake"
                && report.adopted == Some(true)
                && report.candidate_id == Some(id)
        })
    });
    Some((wake, confirm))
}

/// Reports kept for [`Kws::take_onset_reports`]; older ones are dropped.
const ONSET_REPORTS_KEPT: usize = 16;
/// Trace buffers between [`Kws::take_trace`] calls; older entries are dropped and counted.
const TRACE_STREAMS_KEPT: usize = 64;
const TRACE_FEEDS_KEPT: usize = 1024;
/// Samples per encoder frame (40 ms): four 10 ms feature frames.
const ENCODER_FRAME_SAMPLES: usize = SAMPLE_RATE as usize * 2 / 50;
/// The confirm spotter's keyword file; only its stream carries the no-fire window.
const CONFIRM_KEYWORDS_FILE: &str = "keywords-confirm.txt";
/// The wake spotter's keyword file; only its streams own the shared front end's ring.
const WAKE_KEYWORDS_FILE: &str = "keywords-wake.txt";

/// The loaded keyword-spotting runtime; it creates every spotter of one cascade.
///
/// The spotters' ONNX sessions run on the process-wide ONNX Runtime that
/// `rightkit_onnx` bound: the engine binds it at startup on macOS/Windows
/// (`rightkit-dictation-runtime` `entry.rs` `configure_ort_dylib`), and tests bind it
/// themselves. Nothing here loads or binds a runtime. With
/// [`RustKwsTuning::shared_model`] every spotter runs on `model`.
struct KwsRuntime {
    tuning: RustKwsTuning,
    model: Option<Arc<rust_kws::KwsModel>>,
}

impl KwsRuntime {
    fn load(tuning: RustKwsTuning, model_dir: &Path) -> Result<Self, String> {
        let model =
            if tuning.shared_model {
                // Any keyword file: the model reads only the session fields.
                let config = spotter_config(
                    tuning,
                    model_dir,
                    "keywords-wake.txt",
                    KEYWORDS_THRESHOLD,
                    MAX_ACTIVE_PATHS,
                )?;
                Some(rust_kws::KwsModel::load(&config).map_err(|error| {
                    format!("keyword spotter shared model load failed: {error}")
                })?)
            } else {
                None
            };
        Ok(Self { tuning, model })
    }

    fn create_spotter(
        &self,
        model_dir: &Path,
        keywords_file: &str,
        threshold: f32,
        paths: i32,
    ) -> Result<KwsSpotter, String> {
        create_spotter(
            self.tuning,
            self.model.as_ref(),
            model_dir,
            keywords_file,
            threshold,
            paths,
        )
        .map(|spotter| {
            KwsSpotter(
                RefCell::new(spotter),
                SpotterTiming {
                    decode_step_samples: self.tuning.decode_step_samples,
                    max_keyword_span_seconds: self.tuning.max_keyword_span_seconds,
                },
            )
        })
    }
}

/// One keyword spotter (model sessions + keyword graph). `decode` takes `&mut`;
/// a spotter is only touched by its one ASR worker thread, one call at a time,
/// so the borrow never overlaps.
struct KwsSpotter(RefCell<rust_kws::KeywordSpotter>, SpotterTiming);

/// One audio stream of a [`KwsSpotter`].
struct KwsStream(rust_kws::KeywordStream);

/// The step window of a traced fresh-confirmation stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreshTraceWindow {
    /// Traced, no step window (firing records only, or every frame until capacity).
    Unwindowed,
    /// Encoder frames `from..to` on the stream's own clock.
    Frames(i32, i32),
    /// The stream's absolute origin is unknown: keep the newest steps.
    Rolling,
}

/// What a fresh decode produced.
struct FreshDecode {
    found: Option<KeywordResult>,
    records: Option<rust_kws::TraceRecords>,
    /// Real PCM samples fed before the decode stopped.
    real_samples: usize,
    /// Synthetic tail samples fed before the decode stopped.
    tail_samples: usize,
}

/// Optional third spotter: the hey-prefixed always-on wake word. Kept OUT of
/// [`validate_model_dir`] on purpose — a payload without this file must still
/// load the shipped tail cascade, so the spotter is simply absent and
/// [`Kws::hey_wake_available`] reports false.
const HEY_WAKE_KEYWORDS_FILE: &str = "keywords-hey-wake.txt";
const HEY_WAKE_KEYWORD: &str = "HEY_ZEPHYR";

pub struct Kws {
    runtime: KwsRuntime,
    model_dir: PathBuf,
    wake_spotter: KwsSpotter,
    confirm_spotter: KwsSpotter,
    /// None until idle wake starts, and always when the payload predates the
    /// hey-wake keyword file.
    hey_wake_spotter: Option<KwsSpotter>,
    hey_wake_available: bool,
    hey_wake_threshold: Option<f32>,
    idle_wake_stream: Option<WakeStream>,
    clock: CascadeClock,
    wake_stream: Option<WakeStream>,
    confirmation_stream: Option<ConfirmationStream>,
    /// The confirm beam when `shared_front_end` is on (then `confirmation_stream` stays `None`).
    shared_confirmation: Option<SharedConfirmation>,
    /// Shared-front-end counters since load.
    shared_stats: KwsSharedStats,
    latched: bool,
    /// Live switches.
    switches: KwsSwitches,
    max_active_paths: i32,
    onset_reports: VecDeque<KwsOnsetReport>,
    /// Trace buffers; stay empty unless [`KwsSwitches::trace`] is set.
    trace_streams: Vec<KwsStreamTrace>,
    trace_streams_dropped: u64,
    trace_feeds: VecDeque<KwsFeedEvent>,
    trace_feeds_dropped: u64,
    /// Resets and invalidations since load (trace only).
    trace_epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WakeCandidate {
    /// Identity assigned when the clock ADOPTS this candidate: monotonically increasing from
    /// 1, never reused (0 marks the synthetic fallback candidate). Copied into the wake
    /// report and the latched confirmation reports, so a command is attributed to its wake
    /// by identity, never by sample equality.
    id: u64,
    /// The wake word's emission onset: owns debounce, confirmation anchor and
    /// expiry exactly as before onset candidates existed.
    start_sample: usize,
    end_sample: usize,
    /// Where a confirmed command cuts: the chosen onset candidate
    /// ([`KwsSwitches::onset`]); equal to `start_sample` by default. `None` is "no trusted
    /// boundary" (strict D4 with no candidate): no cut is attempted for the command.
    cut_start_sample: Option<usize>,
    cut_source: &'static str,
}

/// `cut_source` of a candidate that has no boundary (strict D4 without a D4 candidate).
const NO_BOUNDARY_SOURCE: &str = "no_boundary";

impl WakeCandidate {
    fn cut(self) -> (Option<usize>, &'static str) {
        (self.cut_start_sample, self.cut_source)
    }
}

/// A confirm beam on the wake stream's shared front end (switch `shared_front_end`): no audio
/// of its own, only the wake stream's retained encoder frames.
struct KwsBeam(rust_kws::BeamStream);

struct SharedConfirmation {
    beam: KwsBeam,
    candidate: WakeCandidate,
    /// The wake stream's origin sample: the clock the beam's frames are on.
    origin_sample: usize,
    fed_until_sample: usize,
}

impl SharedConfirmation {
    fn info(&self, onset_unavailable: Option<&'static str>) -> KwsSharedInfo {
        let start = self.beam.0.start();
        let (ring_frames, ring_bytes) = self.beam.0.ring_size();
        KwsSharedInfo {
            state: start.state(),
            start_frame: start.start_frame,
            requested_frame: start.requested_frame,
            origin_clamped: start.origin_clamped,
            ring_truncated: start.ring_truncated,
            requested_context_frames: start.requested_context_frames,
            actual_context_frames: start.actual_context_frames,
            ring_frames,
            ring_bytes,
            onset_unavailable,
        }
    }
}

struct ConfirmationStream {
    stream: KwsStream,
    candidate: WakeCandidate,
    /// Absolute sample the confirmation stream's clock starts at.
    origin_sample: usize,
    fed_until_sample: usize,
    accepted_until_sample: usize,
    pending_audio: Vec<f32>,
    next_decode_sample: usize,
}

struct WakeStream {
    stream: KwsStream,
    seed_origin_sample: usize,
    fed_until_sample: usize,
    accepted_until_sample: usize,
    pending_audio: Vec<f32>,
    next_decode_sample: usize,
}

#[derive(Debug, Eq, PartialEq)]
enum ConfirmationFeed {
    Duplicate,
    Suffix { offset: usize },
    Stale,
    Gap,
}

fn next_decode_boundary(sample: usize, step: usize) -> usize {
    sample.saturating_add(step)
}

fn classify_confirmation_feed(
    fed_until_sample: usize,
    snapshot_start_sample: usize,
    snapshot_end_sample: usize,
) -> ConfirmationFeed {
    if snapshot_end_sample < fed_until_sample {
        ConfirmationFeed::Stale
    } else if fed_until_sample < snapshot_start_sample {
        ConfirmationFeed::Gap
    } else if snapshot_end_sample == fed_until_sample {
        ConfirmationFeed::Duplicate
    } else {
        ConfirmationFeed::Suffix {
            offset: fed_until_sample - snapshot_start_sample,
        }
    }
}

#[derive(Default)]
struct CascadeClock {
    candidate: Option<WakeCandidate>,
    /// Last id handed out by [`CascadeClock::observe_wake`].
    last_id: u64,
    /// Start of the previous ACCEPTED wake detection; survives candidate expiry, cleared with
    /// the clock.
    last_accepted_start: Option<usize>,
    /// [`KwsSwitches::wake_refractory_samples`]; 0 = off.
    refractory_samples: usize,
    /// [`KwsSwitches::wake_start_proximity_samples`]; 0 = off.
    proximity_samples: usize,
}

/// Refractory: true when `next_start` is less than `refractory_samples` after the previous
/// accepted detection's start. Off (false) when 0 or when nothing was accepted yet.
fn refractory_blocks(
    last_accepted_start: Option<usize>,
    next_start: usize,
    refractory_samples: usize,
) -> bool {
    refractory_samples > 0
        && last_accepted_start
            .is_some_and(|last| next_start < last.saturating_add(refractory_samples))
}

/// True when `next` repeats the held `current`: overlapping intervals (always), or, with a
/// proximity window, starting within `proximity_samples` of its start.
fn wake_is_duplicate(
    current: WakeCandidate,
    next: WakeCandidate,
    proximity_samples: usize,
) -> bool {
    (current.start_sample <= next.end_sample && next.start_sample <= current.end_sample)
        || (proximity_samples > 0
            && next.start_sample.abs_diff(current.start_sample) < proximity_samples)
}

impl CascadeClock {
    fn with_switches(switches: &KwsSwitches) -> Self {
        Self {
            refractory_samples: switches.wake_refractory_samples,
            proximity_samples: switches.wake_start_proximity_samples,
            ..Self::default()
        }
    }

    fn observe_wake(
        &mut self,
        start_sample: usize,
        end_sample: usize,
        cut: (Option<usize>, &'static str),
    ) -> Option<u64> {
        let next = WakeCandidate {
            id: self.last_id.saturating_add(1),
            start_sample,
            end_sample: end_sample.max(start_sample),
            cut_start_sample: cut.0,
            cut_source: cut.1,
        };
        // The spotter may report the same acoustic occurrence more than once as
        // its rolling stream advances. Debounce only overlapping intervals.
        // A fixed start-time refractory window is unsafe: a user can say a
        // second Zephyr shortly after an unconfirmed first one, and that
        // second command must own the cut boundary.
        if refractory_blocks(
            self.last_accepted_start,
            next.start_sample,
            self.refractory_samples,
        ) {
            return None;
        }
        if self
            .candidate
            .is_some_and(|current| wake_is_duplicate(current, next, self.proximity_samples))
        {
            return None;
        }
        self.last_id = next.id;
        self.candidate = Some(next);
        self.last_accepted_start = Some(next.start_sample);
        Some(next.id)
    }

    fn candidate(&self) -> Option<WakeCandidate> {
        self.candidate
    }

    fn expire_if_needed(&mut self, live_sample: usize, timeout_samples: usize) -> bool {
        let expired = self.candidate.is_some_and(|candidate| {
            live_sample > candidate.end_sample.saturating_add(timeout_samples)
        });
        if expired {
            self.candidate = None;
        }
        expired
    }

    /// Drop the candidate. `last_id` survives: onset reports outlive a reset, so an id is
    /// never reused across `clear`.
    fn clear(&mut self) {
        self.candidate = None;
        self.last_accepted_start = None;
    }
}

// The cascade is created, mutated & destroyed by one ASR worker thread; every
// spotter and stream lives inside it. The spotters sit in `RefCell`s that are
// never shared across threads.
unsafe impl Send for Kws {}

impl Kws {
    /// Load the cascade with [`KwsSwitches::default`].
    pub fn load(model_dir: &Path) -> Result<Self, String> {
        Self::load_with_switches(model_dir, KwsSwitches::default())
    }

    /// Load the cascade with explicit switches.
    pub fn load_with_switches(model_dir: &Path, switches: KwsSwitches) -> Result<Self, String> {
        Self::load_with_paths_and_switches(
            model_dir,
            switches.max_active_paths,
            switches.tuning(),
            switches,
        )
    }

    /// Bounded replay only; production keeps MAX_ACTIVE_PATHS.
    #[doc(hidden)]
    pub fn load_for_replay_with_paths(model_dir: &Path, paths: i32) -> Result<Self, String> {
        if !matches!(paths, 4 | 8 | 16) {
            return Err("replay paths must be 4, 8 or 16".into());
        }
        Self::load_with_switches(
            model_dir,
            KwsSwitches {
                max_active_paths: paths,
                ..KwsSwitches::default()
            },
        )
    }

    fn load_with_paths_and_switches(
        model_dir: &Path,
        paths: i32,
        tuning: RustKwsTuning,
        switches: KwsSwitches,
    ) -> Result<Self, String> {
        switches.validate()?;
        validate_model_dir(model_dir)?;
        let runtime = KwsRuntime::load(tuning, model_dir)?;

        let wake_spotter =
            runtime.create_spotter(model_dir, "keywords-wake.txt", KEYWORDS_THRESHOLD, paths)?;
        let confirm_spotter =
            runtime.create_spotter(model_dir, "keywords-confirm.txt", KEYWORDS_THRESHOLD, paths)?;
        let hey_wake_available = model_dir.join(HEY_WAKE_KEYWORDS_FILE).is_file();
        tracing::info!(
            "KWS cascade loaded: model={} cpu_threads={} max_active_paths={} trailing_blanks={} switches={:?}",
            model_dir.display(),
            switches.threads.max(1),
            paths,
            switches.num_trailing_blanks,
            switches
        );
        if switches.decoder_table {
            let wake = wake_spotter.0.borrow().decoder_table_report();
            let confirm = confirm_spotter.0.borrow().decoder_table_report();
            tracing::info!(
                wake_mode = wake.mode,
                wake_declined = ?wake.declined,
                wake_precompute_ms = wake.precompute_ms,
                wake_table_rows = wake.model_rows,
                wake_table_bytes = wake.model_bytes,
                confirm_mode = confirm.mode,
                confirm_declined = ?confirm.declined,
                confirm_precompute_ms = confirm.precompute_ms,
                confirm_table_rows = confirm.model_rows,
                confirm_table_bytes = confirm.model_bytes,
                "kws_decoder_mode"
            );
        }
        Ok(Self {
            runtime,
            model_dir: model_dir.to_path_buf(),
            wake_spotter,
            confirm_spotter,
            hey_wake_spotter: None,
            hey_wake_available,
            hey_wake_threshold: None,
            idle_wake_stream: None,
            clock: CascadeClock::with_switches(&switches),
            wake_stream: None,
            confirmation_stream: None,
            shared_confirmation: None,
            shared_stats: KwsSharedStats::default(),
            latched: false,
            switches,
            max_active_paths: paths,
            onset_reports: VecDeque::new(),
            trace_streams: Vec::new(),
            trace_streams_dropped: 0,
            trace_feeds: VecDeque::new(),
            trace_feeds_dropped: 0,
            trace_epoch: 0,
        })
    }

    /// The effective decoder path and table cost of the wake and confirm spotters (`mode`
    /// is `table`, `cache` or `plain`; a declined table says why). Each spotter reports its
    /// own table build time; with a shared model the row and byte totals are the model's.
    pub fn decoder_table_report(&self) -> KwsDecoderTableReport {
        KwsDecoderTableReport {
            wake: self.wake_spotter.0.borrow().decoder_table_report(),
            confirm: self.confirm_spotter.0.borrow().decoder_table_report(),
        }
    }

    /// What the shared front end has done since load: beams created, how many started clamped
    /// to the origin or truncated by the ring (a beam can be both), and shared-confirm errors.
    /// The ring's 160-frame capacity is a budget, not proof of enough latency: these counts
    /// are how a shortfall shows.
    pub fn shared_front_end_stats(&self) -> KwsSharedStats {
        self.shared_stats
    }

    /// The live switches this cascade runs with.
    pub fn switches(&self) -> KwsSwitches {
        self.switches
    }

    /// Onset reports of the most recent detections (at most 16;
    /// older ones are dropped), oldest first. Each was also logged.
    pub fn take_onset_reports(&mut self) -> Vec<KwsOnsetReport> {
        self.onset_reports.drain(..).collect()
    }

    /// Drain the firing and no-fire trace (stream beam records of the wake and confirm
    /// streams, collected before any stream was destroyed, plus the feed and expiry
    /// schedule). `None` unless [`KwsSwitches::trace`] is set. Streams destroyed by a
    /// reset keep their records until drained. Fresh-confirmation streams (stage
    /// `fresh_confirm`, the whole decode including a no-fire) and the idle-wake stream (stage
    /// `idle_wake`, collected when it is stopped or drained) are collected too; the replay-only
    /// full-command decode and the hey-wake window check run untraced.
    pub fn take_trace(&mut self) -> Option<KwsTrace> {
        self.switches.trace.as_ref()?;
        self.collect_stream_trace("wake");
        self.collect_stream_trace("confirm");
        self.collect_stream_trace("idle_wake");
        Some(KwsTrace {
            streams: std::mem::take(&mut self.trace_streams),
            streams_dropped: std::mem::take(&mut self.trace_streams_dropped),
            feeds: self.trace_feeds.drain(..).collect(),
            feeds_dropped: std::mem::take(&mut self.trace_feeds_dropped),
        })
    }

    /// Move one live stream's beam records into the trace buffer (trace only).
    fn collect_stream_trace(&mut self, stage: &'static str) {
        if self.switches.trace.is_none() {
            return;
        }
        let taken = match stage {
            "confirm" => self
                .confirmation_stream
                .as_mut()
                .map(|state| {
                    (
                        state.origin_sample,
                        Some(state.candidate.id),
                        state.stream.0.take_trace(),
                    )
                })
                .or_else(|| {
                    self.shared_confirmation.as_mut().map(|shared| {
                        (
                            shared.origin_sample,
                            Some(shared.candidate.id),
                            shared.beam.0.take_trace(),
                        )
                    })
                }),
            "idle_wake" => self
                .idle_wake_stream
                .as_mut()
                .map(|state| (state.seed_origin_sample, None, state.stream.0.take_trace())),
            _ => self
                .wake_stream
                .as_mut()
                .map(|state| (state.seed_origin_sample, None, state.stream.0.take_trace())),
        };
        let Some((origin, candidate_id, records)) = taken else {
            return;
        };
        let window = (stage == "confirm"
            && self
                .switches
                .trace
                .is_some_and(|trace| trace.window_frames > 0))
        .then_some("candidate");
        self.store_stream_trace(stage, Some(origin), candidate_id, window, None, records);
    }

    /// Keep one stream's drained records (logging each firing); empty records are dropped.
    fn store_stream_trace(
        &mut self,
        stage: &'static str,
        origin_sample: Option<usize>,
        candidate_id: Option<u64>,
        window: Option<&'static str>,
        fed: Option<(usize, usize)>,
        records: Option<rust_kws::TraceRecords>,
    ) {
        let Some(records) = records else {
            return;
        };
        if records.is_empty() {
            return;
        }
        for firing in &records.firings {
            tracing::info!(
                stage,
                keyword = %firing.keyword,
                occurrence = firing.occurrence,
                epoch_occurrence = firing.epoch_occurrence,
                fired_frame = firing.fired_frame,
                context = ?firing.context,
                ys = ?firing.ys,
                timestamps = ?firing.timestamps,
                ys_probs = ?firing.ys_probs,
                truncated = firing.truncated,
                num_trailing_blanks = firing.num_trailing_blanks,
                score = firing.score,
                threshold = firing.threshold,
                hypotheses_total = firing.hypotheses_total,
                clock = ?firing.clock,
                "kws_firing_trace"
            );
        }
        if self.trace_streams.len() == TRACE_STREAMS_KEPT {
            self.trace_streams.remove(0);
            self.trace_streams_dropped += 1;
        }
        self.trace_streams.push(KwsStreamTrace {
            stage,
            origin_sample,
            candidate_id,
            window,
            real_samples_fed: fed.map(|fed| fed.0),
            tail_samples_fed: fed.map(|fed| fed.1),
            epoch: self.trace_epoch,
            records,
        });
    }

    /// Record one feed or expiry event of `stage`'s stream and the clock's candidate
    /// (trace only).
    fn trace_feed(
        &mut self,
        stage: &'static str,
        event: &'static str,
        snapshot_start_sample: Option<usize>,
        live_total_sample: Option<usize>,
    ) {
        if self.switches.trace.is_none() {
            return;
        }
        let state: Option<(usize, usize, usize, usize)> = if stage == "confirm" {
            self.confirmation_stream
                .as_ref()
                .map(|state| {
                    (
                        state.origin_sample,
                        state.fed_until_sample,
                        state.accepted_until_sample,
                        state.pending_audio.len(),
                    )
                })
                .or_else(|| {
                    self.shared_confirmation
                        .as_ref()
                        .map(|shared| (shared.origin_sample, shared.fed_until_sample, 0, 0))
                })
        } else {
            self.wake_stream.as_ref().map(|state| {
                (
                    state.seed_origin_sample,
                    state.fed_until_sample,
                    state.accepted_until_sample,
                    state.pending_audio.len(),
                )
            })
        };
        let candidate = self.clock.candidate();
        self.push_trace_feed(KwsFeedEvent {
            stage,
            event,
            epoch: self.trace_epoch,
            snapshot_start_sample,
            live_total_sample,
            origin_sample: state.map(|state| state.0),
            fed_until_sample: state.map(|state| state.1),
            accepted_until_sample: state.map(|state| state.2),
            pending_samples: state.map(|state| state.3),
            candidate_id: candidate.map(|candidate| candidate.id),
            candidate_start_sample: candidate.map(|candidate| candidate.start_sample),
            candidate_end_sample: candidate.map(|candidate| candidate.end_sample),
            expiry_deadline_sample: candidate.map(|candidate| {
                candidate
                    .end_sample
                    .saturating_add(self.switches.confirm_timeout_samples)
            }),
        });
    }

    fn push_trace_feed(&mut self, event: KwsFeedEvent) {
        if self.trace_feeds.len() == TRACE_FEEDS_KEPT {
            self.trace_feeds.pop_front();
            self.trace_feeds_dropped += 1;
        }
        self.trace_feeds.push_back(event);
    }

    /// `CascadeClock::expire_if_needed`, recording the expiry when tracing.
    fn expire_clock(&mut self, live_sample: usize) -> bool {
        let candidate = if self.switches.trace.is_some() {
            self.clock.candidate()
        } else {
            None
        };
        let expired = self
            .clock
            .expire_if_needed(live_sample, self.switches.confirm_timeout_samples);
        if let (true, Some(candidate)) = (expired, candidate) {
            self.push_trace_feed(KwsFeedEvent {
                stage: "confirm",
                event: "expire",
                epoch: self.trace_epoch,
                snapshot_start_sample: None,
                live_total_sample: Some(live_sample),
                origin_sample: self
                    .confirmation_stream
                    .as_ref()
                    .map(|state| state.origin_sample),
                fed_until_sample: self
                    .confirmation_stream
                    .as_ref()
                    .map(|state| state.fed_until_sample),
                accepted_until_sample: self
                    .confirmation_stream
                    .as_ref()
                    .map(|state| state.accepted_until_sample),
                pending_samples: self
                    .confirmation_stream
                    .as_ref()
                    .map(|state| state.pending_audio.len()),
                candidate_id: Some(candidate.id),
                candidate_start_sample: Some(candidate.start_sample),
                candidate_end_sample: Some(candidate.end_sample),
                expiry_deadline_sample: Some(
                    candidate
                        .end_sample
                        .saturating_add(self.switches.confirm_timeout_samples),
                ),
            });
        }
        expired
    }

    /// The cut start for a wake detection whose emission onset is
    /// `emission_start`: the chosen candidate on `origin`'s clock (the emission
    /// when that candidate was not estimated), shifted by `onset_shift_ms`.
    /// Returns the sample and the method whose candidate it used. The `emission` source on a
    /// non-emission method is the legacy-control fallback. Under
    /// [`KwsSwitches::fricative_strict`] an absent D4 candidate returns `(None, "no_boundary")`.
    fn cut_start(
        &self,
        emission_start: usize,
        origin: usize,
        result: &KeywordResult,
    ) -> (Option<usize>, &'static str) {
        let candidate = match self.switches.onset {
            None | Some(KwsOnsetMethod::Emission) => None,
            Some(method) => result
                .detection
                .as_ref()
                .and_then(|detection| detection.onset.as_ref())
                .and_then(|onset| method.candidate(onset))
                .map(|estimate| onset_sample(origin, estimate)),
        };
        select_cut(
            self.switches.onset,
            self.switches.fricative_strict,
            self.switches.onset_shift_ms,
            emission_start,
            candidate,
        )
    }

    /// The candidate a latched command cuts with, under [`KwsSwitches::cut_source`]. The
    /// default source returns `candidate` unchanged (cut and source label included). Another
    /// source returns it with the cut and label re-decided from the confirm result
    /// (`origin` is the confirm stream's absolute origin); no trusted boundary is a candidate
    /// with no cut, so the command carries no timed token and the worker attempts no cut.
    fn latched_candidate(
        &self,
        candidate: WakeCandidate,
        result: &KeywordResult,
        origin: Option<usize>,
    ) -> (WakeCandidate, Option<CutEvidence>) {
        if self.switches.cut_source == KwsCutSource::WakeCandidate {
            return (candidate, None);
        }
        let aligned = result
            .detection
            .as_ref()
            .and_then(|detection| detection.onset.as_ref())
            .and_then(|onset| onset.aligned);
        // One stream clock: both timestamp methods map through the confirm stream origin.
        let method = self.switches.cut_source.confirm_timestamp();
        let (raw_confirm, estimate) = match method {
            ConfirmTimestamp::Emission => {
                emission_confirm_sample(origin, result.timestamps.first().copied())
            }
            ConfirmTimestamp::Aligned => (
                aligned.map(|a| a.feature_frame.max(0) as usize * FEATURE_SHIFT_SAMPLES),
                origin
                    .zip(aligned)
                    .map(|(origin, aligned)| onset_sample(origin, aligned)),
            ),
        };
        let evidence = |label: &'static str| CutEvidence {
            wake_cut: candidate.cut_start_sample,
            confirm_estimate: estimate,
            method: method.as_str(),
            raw_confirm_sample: raw_confirm,
            selected: cut_side(label),
        };
        match decide_latched_cut(
            self.switches.cut_source,
            candidate,
            origin.is_some(),
            estimate,
            self.switches.cut_source_shift_ms,
        ) {
            LatchedCut::Cut(cut, label) => (
                WakeCandidate {
                    cut_start_sample: Some(cut),
                    cut_source: label,
                    ..candidate
                },
                Some(evidence(label)),
            ),
            LatchedCut::NoTrustedBoundary(label) => (
                WakeCandidate {
                    cut_start_sample: None,
                    cut_source: label,
                    ..candidate
                },
                Some(evidence(label)),
            ),
        }
    }

    /// Log one detection's onset candidates (`kws_onset_candidates`) and keep the
    /// report. A result without detection detail logs nothing.
    #[allow(clippy::too_many_arguments)] // one call per cascade stage; all are report fields
    fn report_onset(
        &mut self,
        stage: &'static str,
        result: &KeywordResult,
        origin: Option<usize>,
        live_sample: Option<usize>,
        chosen: Option<(Option<usize>, &'static str)>,
        cut_sample: Option<usize>,
        latched: bool,
        candidate_id: Option<u64>,
        adopted: Option<bool>,
        evidence: Option<CutEvidence>,
    ) {
        let Some(detection) = result.detection.as_deref() else {
            return;
        };
        // A shared beam whose onset history was evicted still reports, with no onset. The cut
        // evidence of a non-default source does not depend on onset data, so a report is also
        // built when evidence exists; for the default source the early return is unchanged.
        let onset = match (detection.onset.as_ref(), detection.onset_unavailable) {
            (Some(onset), _) => Some(onset),
            (None, Some(_)) => None,
            (None, None) if evidence.is_some() => None,
            (None, None) => return,
        };
        let base = origin.unwrap_or(0);
        let at = |estimate: Option<rust_kws::OnsetEstimate>| {
            estimate.map(|estimate| onset_sample(base, estimate))
        };
        let report = KwsOnsetReport {
            stage,
            keyword: result.keyword.clone(),
            origin_sample: origin,
            live_sample,
            emission_sample: base.saturating_add(seconds_to_samples(
                result.timestamps.first().copied().unwrap_or(0.0),
            )),
            aligned_sample: at(onset.and_then(|onset| onset.aligned)),
            posterior_sample: at(onset.and_then(|onset| onset.posterior)),
            acoustic_sample: at(onset.and_then(|onset| onset.acoustic)),
            acoustic_rule: onset
                .and_then(|onset| onset.acoustic_rule)
                .map(rust_kws::AcousticRule::as_str),
            // Only with a known origin: never a fabricated absolute coordinate.
            fricative_sample: origin
                .and(onset.and_then(|onset| onset.fricative))
                .map(|e| onset_sample(base, e)),
            fricative_abstain: onset
                .and_then(|onset| onset.fricative_abstain)
                .map(FricativeAbstain::as_str),
            fricative_relative_feature: onset
                .and_then(|onset| onset.fricative)
                .map(|e| e.feature_frame),
            fricative_unmappable: (onset.is_some_and(|onset| onset.fricative.is_some())
                && origin.is_none())
            .then_some("no_origin"),
            token_frames: detection.result.frames.clone(),
            aligned_frames: onset
                .map(|onset| onset.aligned_frames.clone())
                .unwrap_or_default(),
            fired_frame: detection.fired_frame,
            gap_drops: detection.gap_drops,
            gap_drops_chunk: detection.gap_drops_chunk,
            max_gap_frames: detection.max_gap_frames,
            gap_violation: detection.gap_violation,
            token_probs: detection.token_probs.clone(),
            score: detection.score,
            threshold: detection.threshold,
            chosen: self.switches.onset.map_or("off", KwsOnsetMethod::as_str),
            chosen_sample: chosen.and_then(|(sample, _)| sample),
            chosen_source: chosen.map(|(_, source)| source),
            cut_sample,
            emission_sample_checked: emission_confirm_sample(
                origin,
                result.timestamps.first().copied(),
            )
            .1,
            cut_wake_sample: evidence.and_then(|e| e.wake_cut),
            cut_confirm_sample: evidence.and_then(|e| e.confirm_estimate),
            cut_selected: evidence.map(|e| e.selected),
            cut_method: evidence.map(|e| e.method),
            cut_raw_confirm_sample: evidence.and_then(|e| e.raw_confirm_sample),
            latched,
            candidate_id,
            adopted,
            shared: if stage == "confirm" {
                self.shared_confirmation
                    .as_ref()
                    .map(|shared| shared.info(detection.onset_unavailable))
            } else {
                None
            },
        };
        let switches = self.switches;
        tracing::info!(
            stage = report.stage,
            keyword = %report.keyword,
            origin_sample = ?report.origin_sample,
            live_sample = ?report.live_sample,
            emission_sample = report.emission_sample,
            aligned_sample = ?report.aligned_sample,
            posterior_sample = ?report.posterior_sample,
            acoustic_sample = ?report.acoustic_sample,
            acoustic_rule = ?report.acoustic_rule,
            fricative_sample = ?report.fricative_sample,
            fricative_abstain = ?report.fricative_abstain,
            fricative_relative_feature = ?report.fricative_relative_feature,
            fricative_unmappable = ?report.fricative_unmappable,
            candidate_id = ?report.candidate_id,
            adopted = ?report.adopted,
            chosen = report.chosen,
            chosen_sample = ?report.chosen_sample,
            chosen_source = ?report.chosen_source,
            cut_sample = ?report.cut_sample,
            latched = report.latched,
            keyword_frames = ?report.token_frames,
            aligned_frames = ?report.aligned_frames,
            fired_frame = report.fired_frame,
            keyword_probs = ?report.token_probs,
            score = report.score,
            threshold = report.threshold,
            onset_shift_ms = switches.onset_shift_ms,
            fricative = ?switches.fricative,
            fricative_strict = switches.fricative_strict,
            threshold_mode = switches.label_threshold_tokens(),
            joiner_batching = switches.label_joiner_batching(),
            merge_timestamps = switches.label_merge_timestamps(),
            portable_math = switches.portable_math,
            ttl_frames = ?switches.ttl_frames,
            static_batch = switches.static_batch,
            decoder_cache = switches.decoder_cache,
            shared_model = switches.shared_model,
            max_active_paths = self.max_active_paths,
            "kws_onset_candidates"
        );
        if self.onset_reports.len() == ONSET_REPORTS_KEPT {
            self.onset_reports.pop_front();
        }
        self.onset_reports.push_back(report);
    }

    /// True when the payload shipped `keywords-hey-wake.txt`. The spotter stays
    /// lazy until the marker-gated owner starts wake listening.
    pub fn hey_wake_available(&self) -> bool {
        self.hey_wake_available
    }

    pub fn start_idle_wake(&mut self, threshold: f32) -> Result<(), String> {
        if !self.hey_wake_available {
            return Err(format!("{HEY_WAKE_KEYWORDS_FILE} is not packaged"));
        }
        let threshold = threshold.clamp(0.01, 0.99);
        self.stop_idle_wake();
        if self
            .hey_wake_threshold
            .is_none_or(|current| (current - threshold).abs() > f32::EPSILON)
        {
            // Dropping the old spotter destroys it before its replacement loads.
            self.hey_wake_spotter = None;
            self.hey_wake_spotter = Some(create_spotter_with_threshold(
                &self.runtime,
                &self.model_dir,
                HEY_WAKE_KEYWORDS_FILE,
                threshold,
            )?);
            self.hey_wake_threshold = Some(threshold);
        }
        let Some(stream) = self
            .hey_wake_spotter
            .as_ref()
            .map(KwsSpotter::create_stream)
        else {
            return Err("KWS idle wake spotter missing".into());
        };
        self.idle_wake_stream = Some(WakeStream {
            stream,
            seed_origin_sample: 0,
            fed_until_sample: 0,
            accepted_until_sample: 0,
            pending_audio: Vec::new(),
            next_decode_sample: self.switches.decode_step_samples.max(1),
        });
        Ok(())
    }

    pub fn stop_idle_wake(&mut self) {
        // Keep the stream's firing records before it goes (trace only).
        if self.switches.trace.is_some() {
            self.collect_stream_trace("idle_wake");
        }
        // Dropping the stream destroys it.
        self.idle_wake_stream = None;
    }

    pub fn feed_idle_wake(
        &mut self,
        audio: &[f32],
        start_sample: usize,
        live_total_sample: usize,
    ) -> Result<Option<(usize, usize)>, String> {
        // An idle stream exists only beside the hey-wake spotter that made it.
        let (Some(state), Some(spotter)) = (
            self.idle_wake_stream.as_mut(),
            self.hey_wake_spotter.as_ref(),
        ) else {
            return Err("idle wake stream is not active".into());
        };
        if state.fed_until_sample != start_sample {
            return Err(format!(
                "idle wake PCM gap: expected={} actual={start_sample}",
                state.fed_until_sample
            ));
        }
        if start_sample.saturating_add(audio.len()) != live_total_sample {
            return Err(format!(
                "idle wake PCM cursor mismatch: start={start_sample} len={} total={live_total_sample}",
                audio.len()
            ));
        }
        let seed_origin_sample = state.seed_origin_sample;
        state.stream.accept_audio(audio)?;
        state.fed_until_sample = live_total_sample;
        let Some(result) = spotter.decode_ready_result(&mut state.stream)? else {
            return Ok(None);
        };
        if result.keyword.trim().trim_start_matches('@') != HEY_WAKE_KEYWORD {
            return Ok(None);
        }
        let start = seed_origin_sample.saturating_add(seconds_to_samples(
            result.timestamps.first().copied().unwrap_or(0.0),
        ));
        let end = seed_origin_sample
            .saturating_add(seconds_to_samples(
                result.timestamps.last().copied().unwrap_or(0.0),
            ))
            .saturating_add(SAMPLE_RATE as usize / 20);
        self.stop_idle_wake();
        self.report_onset(
            "idle_wake",
            &result,
            Some(seed_origin_sample),
            Some(live_total_sample),
            None,
            None,
            false,
            None,
            None,
            None,
        );
        Ok(Some((start, end.max(start))))
    }

    /// Score the trailing wake window for the hey-prefixed wake word.
    ///
    /// Independent of the tail cascade: no `CascadeClock` state, no latch, no
    /// action authority. Refractory/debounce is the caller's job because the
    /// idle listener owns its own sample clock. Threshold and boosting score
    /// come from `keywords-hey-wake.txt`, not from code.
    pub fn detect_hey_wake(&self, audio: &[f32]) -> Result<bool, String> {
        let Some(spotter) = self.hey_wake_spotter.as_ref() else {
            return Ok(false);
        };
        let window_start = audio.len().saturating_sub(WAKE_WINDOW_SAMPLES);
        let Some(hit) = spotter.decode_fresh(&audio[window_start..], WAKE_DECODE_TAIL_SAMPLES)?
        else {
            return Ok(false);
        };
        Ok(hit.keyword.trim().trim_start_matches('@') == HEY_WAKE_KEYWORD)
    }

    pub fn transcribe(&mut self, audio: &[f32]) -> Result<String, String> {
        self.transcribe_result(audio).map(|result| result.text)
    }

    pub fn transcribe_result(&mut self, audio: &[f32]) -> Result<TranscriptionResult, String> {
        self.transcribe_cascade_result(audio, 0, audio.len(), false)
    }

    /// Experimental matched-replay entry: one persistent full-command decoder,
    /// with no separate wake decoder or fresh rescans. Not selected by worker.
    /// Use a dedicated instance (or reset_stream) when switching strategies.
    /// Native timestamps remain hypotheses, never unconditional cleanup cuts.
    #[doc(hidden)]
    pub fn transcribe_continuous_full_command_result(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
    ) -> Result<(TranscriptionResult, Option<usize>), String> {
        if self.switches.shared_front_end {
            // This entry feeds confirmation with no wake stream, so there is no shared front
            // end to follow. Refused before any mutation; the cascade entry points are the
            // C6 route.
            return Err("KWS shared front end is not supported by the full-command entry".into());
        }
        if self.latched {
            return Ok((empty_result(), None));
        }
        if snapshot_start_sample.checked_add(audio.len()) != Some(live_total_sample) {
            self.invalidate_cascade();
            return Err("KWS full-command snapshot cursor mismatch".into());
        }
        if self.confirmation_stream.as_ref().is_some_and(|s| {
            matches!(
                classify_confirmation_feed(
                    s.fed_until_sample,
                    snapshot_start_sample,
                    live_total_sample
                ),
                ConfirmationFeed::Gap | ConfirmationFeed::Stale
            )
        }) {
            self.invalidate_cascade();
            return Err("KWS full-command stream lost contiguous audio".into());
        }
        let candidate = self
            .confirmation_stream
            .as_ref()
            .map(|s| s.candidate)
            .unwrap_or(WakeCandidate {
                id: 0,
                start_sample: snapshot_start_sample,
                end_sample: snapshot_start_sample,
                cut_start_sample: Some(snapshot_start_sample),
                cut_source: KwsOnsetMethod::Emission.as_str(),
            });
        let Some(result) =
            self.feed_confirmation(audio, snapshot_start_sample, live_total_sample, candidate)?
        else {
            return Ok((empty_result(), None));
        };
        let Some(text) = command_text(&result.keyword).filter(|text| *text != "zephyr") else {
            return Ok((empty_result(), None));
        };
        let onset = candidate
            .start_sample
            .saturating_add(seconds_to_samples(result.timestamps[0]));
        let end = candidate
            .start_sample
            .saturating_add(seconds_to_samples(*result.timestamps.last().unwrap()))
            .saturating_add(SAMPLE_RATE as usize / 20);
        self.latched = true;
        self.destroy_confirmation_stream();
        Ok((
            TranscriptionResult {
                text: text.into(),
                tokens: vec![TimedToken {
                    text: " zephyr".into(),
                    start: onset as f32 / SAMPLE_RATE as f32,
                    end: end as f32 / SAMPLE_RATE as f32,
                }],
            },
            Some(onset),
        ))
    }

    pub fn transcribe_fresh_confirmation_result(
        &mut self,
        audio: &[f32],
    ) -> Result<TranscriptionResult, String> {
        self.transcribe_fresh_confirmation_result_with_wake_start(audio)
            .map(|(result, _)| result)
    }

    /// Nonshipping follow-up: fresh full phrases without a separate wake gate.
    #[doc(hidden)]
    pub fn transcribe_fresh_full_command_for_replay(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
    ) -> Result<(TranscriptionResult, Option<usize>), String> {
        if self.latched {
            return Ok((empty_result(), None));
        }
        let Some(result) = self
            .confirm_spotter
            .decode_fresh(audio, self.switches.fresh_tail_samples)?
        else {
            return Ok((empty_result(), None));
        };
        let Some(text) = command_text(&result.keyword).filter(|text| *text != "zephyr") else {
            return Ok((empty_result(), None));
        };
        let onset = snapshot_start_sample.saturating_add(seconds_to_samples(result.timestamps[0]));
        self.latched = true;
        Ok((
            TranscriptionResult {
                text: text.into(),
                tokens: Vec::new(),
            },
            Some(onset),
        ))
    }

    /// Confirm a pending prefix only when its acoustic candidate is still
    /// available. A fresh confirmation decode without a candidate cannot own
    /// an audio boundary, so it remains non-firing.
    pub fn transcribe_fresh_confirmation_result_with_wake_start(
        &mut self,
        audio: &[f32],
    ) -> Result<(TranscriptionResult, Option<usize>), String> {
        self.fresh_confirmation(audio, None)
    }

    /// [`Self::transcribe_fresh_confirmation_result_with_wake_start`];
    /// `snapshot_start_sample` (when known) places its onset report on the
    /// outer sample clock.
    fn fresh_confirmation(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: Option<usize>,
    ) -> Result<(TranscriptionResult, Option<usize>), String> {
        if self.latched {
            return Ok((empty_result(), None));
        }
        let Some(candidate) = self.clock.candidate() else {
            return Ok((empty_result(), None));
        };
        // The fresh stream is traced and its records kept even when nothing fires. Its step
        // window is the wake candidate's region on the stream's own clock (the stream starts
        // at `snapshot_start_sample`); with no known origin the newest steps are kept and the
        // record says `unknown_origin`.
        let (trace_window, window_label) = match self.switches.trace {
            None => (None, None),
            Some(trace) if trace.window_frames == 0 => (Some(FreshTraceWindow::Unwindowed), None),
            Some(trace) => match snapshot_start_sample {
                Some(origin) => {
                    let (from_frame, to_frame) = confirm_trace_window(
                        origin,
                        candidate.start_sample,
                        candidate.end_sample,
                        trace.window_frames,
                    );
                    (
                        Some(FreshTraceWindow::Frames(from_frame, to_frame)),
                        Some("candidate"),
                    )
                }
                None => (Some(FreshTraceWindow::Rolling), Some("unknown_origin")),
            },
        };
        let fresh = self.confirm_spotter.decode_fresh_with(
            audio,
            self.switches.fresh_tail_samples,
            trace_window,
        )?;
        self.store_stream_trace(
            "fresh_confirm",
            snapshot_start_sample,
            Some(candidate.id),
            window_label,
            Some((fresh.real_samples, fresh.tail_samples)),
            fresh.records,
        );
        let found = fresh.found;
        let Some(result) = found else {
            return Ok((empty_result(), None));
        };
        let latch = command_text(&result.keyword).is_some_and(|text| text != "zephyr");
        let (candidate, evidence) = if latch {
            self.latched_candidate(candidate, &result, snapshot_start_sample)
        } else {
            (candidate, None)
        };
        self.report_onset(
            "fresh_confirm",
            &result,
            snapshot_start_sample,
            snapshot_start_sample.map(|start| start.saturating_add(audio.len())),
            latch.then_some(candidate.cut()),
            candidate.cut_start_sample,
            latch,
            Some(candidate.id),
            None,
            evidence,
        );
        if !latch {
            return Ok((empty_result(), None));
        }
        self.latched = true;
        self.destroy_confirmation_stream();
        Ok((
            transcription_result_at_wake(result, candidate, 0),
            candidate.cut_start_sample,
        ))
    }

    /// Run the worker's full-tail scheduling decision: fresh confirmation
    /// first when requested, then cascade on the identical snapshot if fresh
    /// returns empty.
    pub fn transcribe_scheduled_probe_result_with_wake_start(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
        confirmation_armed: bool,
        use_fresh_confirmation: bool,
    ) -> Result<(TranscriptionResult, Option<usize>, bool), String> {
        // Fresh confirmation must obey the same acoustic TTL & cursor contract
        // as persistent confirmation before it can latch an action.
        if snapshot_start_sample.checked_add(audio.len()) != Some(live_total_sample) {
            self.invalidate_cascade();
            return Err("KWS cascade snapshot cursor mismatch".into());
        }
        if self.expire_clock(live_total_sample) {
            self.destroy_confirmation_stream();
        }
        if use_fresh_confirmation {
            let (fresh, wake_start) =
                self.fresh_confirmation(audio, Some(snapshot_start_sample))?;
            if !fresh.text.trim().is_empty() {
                return Ok((fresh, wake_start, true));
            }
        }
        self.transcribe_cascade_result_with_wake_start(
            audio,
            snapshot_start_sample,
            live_total_sample,
            confirmation_armed,
        )
        .map(|(result, wake_start)| (result, wake_start, false))
    }

    pub fn transcribe_cascade_result(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
        _confirmation_armed: bool,
    ) -> Result<TranscriptionResult, String> {
        self.transcribe_cascade_result_with_wake_start(
            audio,
            snapshot_start_sample,
            live_total_sample,
            _confirmation_armed,
        )
        .map(|(result, _)| result)
    }

    /// Decode one cascade snapshot while returning the absolute onset owned by
    /// the acoustic wake candidate that paired with confirmation. The
    /// confirmation model owns command intent, but its emitted timestamps are
    /// not a safe cut boundary.
    pub fn transcribe_cascade_result_with_wake_start(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
        _confirmation_armed: bool,
    ) -> Result<(TranscriptionResult, Option<usize>), String> {
        if self.latched {
            return Ok((empty_result(), None));
        }
        let snapshot_end_sample = match snapshot_start_sample.checked_add(audio.len()) {
            Some(end) => end,
            None => {
                self.invalidate_cascade();
                return Err("KWS snapshot cursor overflow".into());
            }
        };
        if snapshot_end_sample != live_total_sample {
            self.invalidate_cascade();
            return Err("KWS cascade snapshot cursor mismatch".into());
        }

        if self.expire_clock(live_total_sample) {
            self.destroy_confirmation_stream();
        }
        let previous_candidate = self.clock.candidate();
        {
            let wake = match self.feed_wake(audio, snapshot_start_sample, live_total_sample) {
                Ok(wake) => wake,
                Err(error) => {
                    self.invalidate_cascade();
                    return Err(error);
                }
            };
            if let Some(wake) = wake {
                if wake.keyword.trim().trim_start_matches('@') == "ZEPHYR" {
                    let base = self
                        .wake_stream
                        .as_ref()
                        .map(|stream| stream.seed_origin_sample)
                        .unwrap_or(snapshot_start_sample);
                    let start = base.saturating_add(seconds_to_samples(
                        wake.timestamps.first().copied().unwrap_or(0.0),
                    ));
                    let end = base
                        .saturating_add(seconds_to_samples(
                            wake.timestamps.last().copied().unwrap_or(0.0),
                        ))
                        .saturating_add(SAMPLE_RATE as usize / 20);
                    let cut = self.cut_start(start, base, &wake);
                    let adopted_id = self.clock.observe_wake(start, end, cut);
                    if self.clock.candidate() != previous_candidate {
                        self.destroy_confirmation_stream();
                    }
                    let cut_now = self.clock.candidate().and_then(|c| c.cut_start_sample);
                    self.report_onset(
                        "wake",
                        &wake,
                        Some(base),
                        Some(live_total_sample),
                        Some(cut),
                        cut_now,
                        false,
                        adopted_id,
                        Some(adopted_id.is_some()),
                        None,
                    );
                }
            }
        }

        let acoustic_candidate = self.clock.candidate();
        if let Some(candidate) = acoustic_candidate {
            let result = match self.feed_confirmation(
                audio,
                snapshot_start_sample,
                live_total_sample,
                candidate,
            ) {
                Ok(result) => result,
                Err(error) => {
                    self.invalidate_cascade();
                    return Err(error);
                }
            };
            if let Some(result) = result {
                let latch = command_text(&result.keyword).is_some_and(|text| text != "zephyr");
                let origin = self
                    .confirmation_stream
                    .as_ref()
                    .map(|stream| stream.origin_sample)
                    .or(self
                        .shared_confirmation
                        .as_ref()
                        .map(|shared| shared.origin_sample));
                let (candidate, evidence) = if latch {
                    self.latched_candidate(candidate, &result, origin)
                } else {
                    (candidate, None)
                };
                self.report_onset(
                    "confirm",
                    &result,
                    origin,
                    Some(live_total_sample),
                    latch.then_some(candidate.cut()),
                    candidate.cut_start_sample,
                    latch,
                    Some(candidate.id),
                    None,
                    evidence,
                );
                if latch {
                    self.latched = true;
                    self.destroy_confirmation_stream();
                    return Ok((
                        transcription_result_at_wake(result, candidate, snapshot_start_sample),
                        candidate.cut_start_sample,
                    ));
                }
            }
        } else {
            self.destroy_confirmation_stream();
        }

        if let Some(candidate) = acoustic_candidate {
            let tokens = wake_tokens(
                candidate.cut_start_sample,
                candidate.end_sample,
                snapshot_start_sample,
                false,
            );
            return Ok((
                TranscriptionResult {
                    text: "zephyr".into(),
                    tokens,
                },
                None,
            ));
        }
        Ok((empty_result(), None))
    }

    fn feed_wake(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
    ) -> Result<Option<KeywordResult>, String> {
        if self.wake_stream.is_none() {
            let mut stream = self.wake_spotter.create_stream();
            let next_decode_sample = next_decode_boundary(
                snapshot_start_sample,
                self.switches.decode_step_samples.max(1),
            );
            let (result, accepted_until_sample, pending_audio, next_decode_sample) =
                self.wake_spotter.accept_and_decode_at_sample_boundaries(
                    &mut stream,
                    audio,
                    snapshot_start_sample,
                    snapshot_start_sample,
                    Vec::new(),
                    next_decode_sample,
                )?;
            self.wake_stream = Some(WakeStream {
                stream,
                seed_origin_sample: snapshot_start_sample,
                fed_until_sample: live_total_sample,
                accepted_until_sample,
                pending_audio,
                next_decode_sample,
            });
            self.trace_feed(
                "wake",
                "create",
                Some(snapshot_start_sample),
                Some(live_total_sample),
            );
            return Ok(result);
        }

        let feed = {
            let state = self.wake_stream.as_ref().expect("wake stream exists");
            classify_confirmation_feed(
                state.fed_until_sample,
                snapshot_start_sample,
                live_total_sample,
            )
        };
        match feed {
            ConfirmationFeed::Duplicate => {
                self.trace_feed(
                    "wake",
                    "duplicate",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                Ok(None)
            }
            ConfirmationFeed::Stale => {
                self.trace_feed(
                    "wake",
                    "stale",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                Err("KWS wake snapshot is stale".into())
            }
            ConfirmationFeed::Gap => {
                self.trace_feed(
                    "wake",
                    "gap",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                self.destroy_wake_stream();
                self.feed_wake(audio, snapshot_start_sample, live_total_sample)
            }
            ConfirmationFeed::Suffix { offset } => {
                let state = self.wake_stream.as_mut().expect("wake stream exists");
                let next_decode_sample = state.next_decode_sample;
                let accepted_until_sample = state.accepted_until_sample;
                let pending_audio = std::mem::take(&mut state.pending_audio);
                let suffix_start = snapshot_start_sample.saturating_add(offset);
                let (result, accepted_until_sample, pending_audio, next_decode_sample) =
                    self.wake_spotter.accept_and_decode_at_sample_boundaries(
                        &mut state.stream,
                        &audio[offset..],
                        suffix_start,
                        accepted_until_sample,
                        pending_audio,
                        next_decode_sample,
                    )?;
                state.fed_until_sample = live_total_sample;
                state.accepted_until_sample = accepted_until_sample;
                state.pending_audio = pending_audio;
                state.next_decode_sample = next_decode_sample;
                self.trace_feed(
                    "wake",
                    "suffix",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                Ok(result)
            }
        }
    }

    /// The LIVE confirm beam on the wake stream's shared front end. The requested start is the
    /// frame `CONFIRM_CONTEXT_SAMPLES` before the candidate on the wake clock, rounded by floor
    /// (toward minus infinity) in integer frames; the beam clamps it to the wake stream's
    /// origin and then to the ring, and the report says which. It needs the wake stream fed in
    /// the same call; the audio itself is never fed here.
    fn feed_shared_confirmation(
        &mut self,
        snapshot_start_sample: usize,
        live_total_sample: usize,
        candidate: WakeCandidate,
    ) -> Result<Option<KeywordResult>, String> {
        if self
            .shared_confirmation
            .as_ref()
            .is_some_and(|shared| shared.candidate != candidate)
        {
            self.destroy_confirmation_stream();
        }
        let created = self.shared_confirmation.is_none();
        if created {
            let Some(wake) = self.wake_stream.as_ref() else {
                return Err("KWS shared front end needs the wake stream".into());
            };
            let origin = wake.seed_origin_sample;
            let requested_frame = shared_requested_frame(candidate.start_sample, origin);
            let mut beam = self.confirm_spotter.create_beam(
                &self.wake_spotter,
                &wake.stream,
                requested_frame,
            )?;
            if let Some(trace) = self.switches.trace.filter(|trace| trace.window_frames > 0) {
                let (from_frame, to_frame) = confirm_trace_window(
                    origin,
                    candidate.start_sample,
                    candidate.end_sample,
                    trace.window_frames,
                );
                beam.0.set_trace_window(from_frame, to_frame);
            }
            let start = beam.0.start();
            self.shared_stats.beams_created += 1;
            self.shared_stats.origin_clamped += u64::from(start.origin_clamped);
            self.shared_stats.ring_truncated += u64::from(start.ring_truncated);
            self.shared_confirmation = Some(SharedConfirmation {
                beam,
                candidate,
                origin_sample: origin,
                fed_until_sample: live_total_sample,
            });
        }
        let (Some(wake), Some(shared)) =
            (self.wake_stream.as_ref(), self.shared_confirmation.as_mut())
        else {
            return Err("KWS shared front end needs the wake stream".into());
        };
        let result = self
            .confirm_spotter
            .decode_beam_ready(&mut shared.beam, &wake.stream)?;
        shared.fed_until_sample = live_total_sample;
        self.trace_feed(
            "confirm",
            if created {
                "shared_create"
            } else {
                "shared_follow"
            },
            Some(snapshot_start_sample),
            Some(live_total_sample),
        );
        Ok(result)
    }

    fn feed_confirmation(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
        candidate: WakeCandidate,
    ) -> Result<Option<KeywordResult>, String> {
        if self.switches.shared_front_end {
            let result =
                self.feed_shared_confirmation(snapshot_start_sample, live_total_sample, candidate);
            if result.is_err() {
                self.shared_stats.errors += 1;
            }
            return result;
        }
        if self
            .confirmation_stream
            .as_ref()
            .is_some_and(|stream| stream.candidate != candidate)
        {
            self.destroy_confirmation_stream();
        }

        if self.confirmation_stream.is_none() {
            let anchor_sample = candidate
                .start_sample
                .saturating_sub(self.switches.confirm_context_samples)
                .max(snapshot_start_sample)
                .min(live_total_sample);
            let anchor_offset = anchor_sample - snapshot_start_sample;
            let mut stream = self.confirm_spotter.create_stream();
            if let Some(trace) = self.switches.trace.filter(|trace| trace.window_frames > 0) {
                let (from_frame, to_frame) = confirm_trace_window(
                    anchor_sample,
                    candidate.start_sample,
                    candidate.end_sample,
                    trace.window_frames,
                );
                stream.0.set_trace_window(from_frame, to_frame);
            }
            let next_decode_sample =
                next_decode_boundary(anchor_sample, self.switches.decode_step_samples.max(1));
            // On error the new stream drops, which destroys it.
            let (result, accepted_until_sample, pending_audio, next_decode_sample) = self
                .confirm_spotter
                .accept_and_decode_at_sample_boundaries(
                    &mut stream,
                    &audio[anchor_offset..],
                    anchor_sample,
                    anchor_sample,
                    Vec::new(),
                    next_decode_sample,
                )?;
            self.confirmation_stream = Some(ConfirmationStream {
                stream,
                candidate,
                origin_sample: anchor_sample,
                fed_until_sample: live_total_sample,
                accepted_until_sample,
                pending_audio,
                next_decode_sample,
            });
            self.trace_feed(
                "confirm",
                "create",
                Some(snapshot_start_sample),
                Some(live_total_sample),
            );
            return Ok(result);
        }

        let feed = {
            let state = self
                .confirmation_stream
                .as_ref()
                .expect("confirmation stream checked above");
            classify_confirmation_feed(
                state.fed_until_sample,
                snapshot_start_sample,
                live_total_sample,
            )
        };
        match feed {
            ConfirmationFeed::Duplicate => {
                self.trace_feed(
                    "confirm",
                    "duplicate",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                Ok(None)
            }
            ConfirmationFeed::Stale => {
                self.trace_feed(
                    "confirm",
                    "stale",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                Err("KWS confirmation snapshot is stale".into())
            }
            ConfirmationFeed::Gap => {
                self.trace_feed(
                    "confirm",
                    "gap",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                self.destroy_confirmation_stream();
                self.feed_confirmation(audio, snapshot_start_sample, live_total_sample, candidate)
            }
            ConfirmationFeed::Suffix { offset } => {
                let suffix = &audio[offset..];
                let state = self
                    .confirmation_stream
                    .as_mut()
                    .expect("confirmation stream checked above");
                let next_decode_sample = state.next_decode_sample;
                let accepted_until_sample = state.accepted_until_sample;
                let pending_audio = std::mem::take(&mut state.pending_audio);
                let suffix_start = snapshot_start_sample.saturating_add(offset);
                let (result, accepted_until_sample, pending_audio, next_decode_sample) = self
                    .confirm_spotter
                    .accept_and_decode_at_sample_boundaries(
                        &mut state.stream,
                        suffix,
                        suffix_start,
                        accepted_until_sample,
                        pending_audio,
                        next_decode_sample,
                    )?;
                state.fed_until_sample = live_total_sample;
                state.accepted_until_sample = accepted_until_sample;
                state.pending_audio = pending_audio;
                state.next_decode_sample = next_decode_sample;
                self.trace_feed(
                    "confirm",
                    "suffix",
                    Some(snapshot_start_sample),
                    Some(live_total_sample),
                );
                Ok(result)
            }
        }
    }

    fn destroy_confirmation_stream(&mut self) {
        // Keep the stream's beam records before it goes (trace only).
        if self.switches.trace.is_some()
            && (self.confirmation_stream.is_some() || self.shared_confirmation.is_some())
        {
            self.collect_stream_trace("confirm");
            self.trace_feed("confirm", "destroy", None, None);
        }
        // Dropping the stream destroys it.
        self.confirmation_stream = None;
        self.shared_confirmation = None;
    }

    fn destroy_wake_stream(&mut self) {
        // A shared confirm beam lives on the wake stream's frames: it goes with the stream.
        if self.shared_confirmation.is_some() {
            self.destroy_confirmation_stream();
        }
        if self.switches.trace.is_some() && self.wake_stream.is_some() {
            self.collect_stream_trace("wake");
            self.trace_feed("wake", "destroy", None, None);
        }
        // Dropping the stream destroys it.
        self.wake_stream = None;
    }

    fn invalidate_cascade(&mut self) {
        self.destroy_wake_stream();
        self.destroy_confirmation_stream();
        self.clock.clear();
        // After the destroys, so their records carry the epoch they were made in.
        if self.switches.trace.is_some() {
            self.trace_epoch += 1;
        }
    }

    pub fn reset_stream(&mut self) -> Result<(), String> {
        self.destroy_wake_stream();
        self.destroy_confirmation_stream();
        self.clock.clear();
        if self.switches.trace.is_some() {
            self.trace_epoch += 1;
        }
        self.latched = false;
        Ok(())
    }
}

impl Drop for Kws {
    fn drop(&mut self) {
        // Streams before spotters; the spotters (wake, confirm, hey-wake) then
        // drop in field order and destroy themselves.
        self.stop_idle_wake();
        self.destroy_wake_stream();
        self.destroy_confirmation_stream();
    }
}

impl KwsSpotter {
    fn create_stream(&self) -> KwsStream {
        KwsStream(self.0.borrow().create_stream())
    }

    /// A confirm beam on `front_end` (a stream of `wake`, which keeps the shared ring).
    fn create_beam(
        &self,
        wake: &KwsSpotter,
        front_end: &KwsStream,
        requested_frame: i64,
    ) -> Result<KwsBeam, String> {
        self.0
            .borrow()
            .create_beam_stream(&wake.0.borrow(), &front_end.0, requested_frame)
            .map(KwsBeam)
            .map_err(|error| format!("KWS shared beam: {error}"))
    }

    /// [`Self::decode_ready_result`] for a shared beam: decode the retained frames it has not
    /// seen, returning the first accepted result with the same plausibility check.
    fn decode_beam_ready(
        &self,
        beam: &mut KwsBeam,
        front_end: &KwsStream,
    ) -> Result<Option<KeywordResult>, String> {
        let mut decode_steps = 0usize;
        while self.0.borrow().beam_ready(&beam.0, &front_end.0) {
            self.0
                .borrow_mut()
                .decode_beam(&mut beam.0, &front_end.0)
                .map_err(|error| format!("KWS beam decode: {error}"))?;
            decode_steps += 1;
            if decode_steps > 4_096 {
                return Err("KWS beam readiness did not converge".into());
            }
            let Some(detection) = self.0.borrow().beam_detection(&mut beam.0) else {
                continue;
            };
            let result = KeywordResult {
                keyword: detection.result.keyword.clone(),
                timestamps: detection
                    .result
                    .timestamps
                    .iter()
                    .copied()
                    .map(round_timestamp)
                    .collect(),
                detection: Some(Box::new(detection)),
            };
            if !accepts_keyword_result(&result.keyword) {
                continue;
            }
            if !keyword_timestamps_are_plausible(
                &result.timestamps,
                self.1.max_keyword_span_seconds,
            ) {
                let start = result.timestamps.first().copied();
                let end = result.timestamps.last().copied();
                tracing::warn!(
                    keyword = result.keyword,
                    ?start,
                    ?end,
                    "KWS rejected non-contiguous keyword path"
                );
                return Err(format!(
                    "KWS keyword timing is non-contiguous: start={start:?} end={end:?}"
                ));
            }
            return Ok(Some(result));
        }
        Ok(None)
    }

    /// A stream with no trace state even when tracing is on.
    fn create_stream_untraced(&self) -> KwsStream {
        KwsStream(self.0.borrow().create_stream_untraced())
    }

    fn is_ready(&self, stream: &KwsStream) -> bool {
        self.0.borrow().is_ready(&stream.0)
    }

    fn decode(&self, stream: &mut KwsStream) -> Result<(), String> {
        self.0
            .borrow_mut()
            .decode(&mut stream.0)
            .map_err(|error| format!("KWS decode: {error}"))
    }

    /// The newest detection, reported once; the keyword is empty when nothing new
    /// fired.
    fn take_result(&self, stream: &mut KwsStream) -> Result<KeywordResult, String> {
        // `detection` shares `result`'s once-only gate and reports the same hits,
        // plus score, threshold and onset candidates.
        let Some(detection) = self.0.borrow().detection(&mut stream.0) else {
            return Ok(KeywordResult {
                keyword: String::new(),
                timestamps: Vec::new(),
                detection: None,
            });
        };
        Ok(KeywordResult {
            keyword: detection.result.keyword.clone(),
            timestamps: detection
                .result
                .timestamps
                .iter()
                .copied()
                .map(round_timestamp)
                .collect(),
            detection: Some(Box::new(detection)),
        })
    }

    /// An untraced fresh decode: for callers that never drain a trace.
    fn decode_fresh(
        &self,
        audio: &[f32],
        tail_samples: usize,
    ) -> Result<Option<KeywordResult>, String> {
        self.decode_fresh_with(audio, tail_samples, None)
            .map(|fresh| fresh.found)
    }

    /// A fresh decode on its own stream: the same feed and decode schedule, and the same
    /// stop at the first result, as [`Self::decode_fresh`]. With a `trace_window` the
    /// stream's records come back too, on a hit and on a no-fire alike, with the window
    /// applied before the first sample is fed and the synthetic tail declared as padding;
    /// with `None` the stream carries no trace state.
    fn decode_fresh_with(
        &self,
        audio: &[f32],
        tail_samples: usize,
        trace_window: Option<FreshTraceWindow>,
    ) -> Result<FreshDecode, String> {
        let mut stream = if trace_window.is_some() {
            self.create_stream()
        } else {
            self.create_stream_untraced()
        };
        match trace_window {
            Some(FreshTraceWindow::Frames(from_frame, to_frame)) => {
                stream.0.set_trace_window(from_frame, to_frame);
            }
            Some(FreshTraceWindow::Rolling) => stream.0.set_trace_rolling(),
            Some(FreshTraceWindow::Unwindowed) | None => {}
        }
        let mut found = None;
        let mut real_samples = 0usize;
        let mut tail_fed = 0usize;
        'feed: {
            for chunk in audio.chunks(KWS_FEED_CHUNK_SAMPLES) {
                stream.accept_audio(chunk)?;
                real_samples += chunk.len();
                if let Some(result) = self.decode_ready_result(&mut stream)? {
                    found = Some(result);
                    break 'feed;
                }
            }
            // From here on every sample is the synthetic tail, not recognition latency.
            stream.0.declare_trace_padding(true);
            let silence = vec![0.0; tail_samples];
            for chunk in silence.chunks(KWS_FEED_CHUNK_SAMPLES) {
                stream.accept_audio(chunk)?;
                tail_fed += chunk.len();
                if let Some(result) = self.decode_ready_result(&mut stream)? {
                    found = Some(result);
                    break 'feed;
                }
            }
        }
        Ok(FreshDecode {
            found,
            records: stream.0.take_trace(),
            real_samples,
            tail_samples: tail_fed,
        })
    }

    /// Keep native decoder checkpoints independent of executor backlog. Audio
    /// arrival stays event-driven; this only preserves model input geometry
    /// when latest-only mailbox replacement coalesces several capture blocks.
    fn accept_and_decode_at_sample_boundaries(
        &self,
        stream: &mut KwsStream,
        audio: &[f32],
        audio_start_sample: usize,
        mut accepted_until_sample: usize,
        mut pending_audio: Vec<f32>,
        mut next_decode_sample: usize,
    ) -> Result<(Option<KeywordResult>, usize, Vec<f32>, usize), String> {
        let expected_start = accepted_until_sample.saturating_add(pending_audio.len());
        if audio_start_sample != expected_start {
            return Err(format!(
                "KWS buffered cursor mismatch: expected={expected_start} got={audio_start_sample}"
            ));
        }
        pending_audio.extend_from_slice(audio);
        let mut first_result = None;
        let mut consumed = 0usize;
        loop {
            while next_decode_sample <= accepted_until_sample {
                next_decode_sample = next_decode_sample.saturating_add(self.1.decode_step_samples);
            }
            let needed = next_decode_sample.saturating_sub(accepted_until_sample);
            if pending_audio.len().saturating_sub(consumed) < needed {
                break;
            }
            stream.accept_audio(&pending_audio[consumed..consumed + needed])?;
            consumed += needed;
            accepted_until_sample = next_decode_sample;
            if let Some(result) = self.decode_ready_result(stream)? {
                first_result.get_or_insert(result);
            }
            next_decode_sample = next_decode_sample.saturating_add(self.1.decode_step_samples);
        }
        if consumed > 0 {
            pending_audio.drain(..consumed);
        }
        Ok((
            first_result,
            accepted_until_sample,
            pending_audio,
            next_decode_sample,
        ))
    }

    fn decode_ready_result(&self, stream: &mut KwsStream) -> Result<Option<KeywordResult>, String> {
        let mut decode_steps = 0usize;
        while self.is_ready(stream) {
            self.decode(stream)?;
            decode_steps += 1;
            if decode_steps > 4_096 {
                return Err("KWS decode readiness did not converge".into());
            }
            if let Some(result) = self.current_result(stream)? {
                if !keyword_timestamps_are_plausible(
                    &result.timestamps,
                    self.1.max_keyword_span_seconds,
                ) {
                    let start = result.timestamps.first().copied();
                    let end = result.timestamps.last().copied();
                    tracing::warn!(
                        keyword = result.keyword,
                        ?start,
                        ?end,
                        "KWS rejected non-contiguous keyword path"
                    );
                    return Err(format!(
                        "KWS keyword timing is non-contiguous: start={start:?} end={end:?}"
                    ));
                }
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    fn current_result(&self, stream: &mut KwsStream) -> Result<Option<KeywordResult>, String> {
        let result = self.take_result(stream)?;
        Ok(accepts_keyword_result(&result.keyword).then_some(result))
    }
}

impl KwsStream {
    fn accept_audio(&mut self, audio: &[f32]) -> Result<(), String> {
        if audio.is_empty() {
            return Ok(());
        }
        self.0
            .accept_waveform(SAMPLE_RATE as u32, audio)
            .map_err(|error| format!("KWS accept waveform: {error}"))
    }
}

/// Timestamps are rounded to two decimals (10 ms), the precision the cascade's
/// timing policy and corpus thresholds were qualified on.
fn round_timestamp(seconds: f32) -> f32 {
    format!("{seconds:.2}").parse().unwrap_or(seconds)
}

fn create_spotter_with_threshold(
    runtime: &KwsRuntime,
    model_dir: &Path,
    keywords_file: &str,
    threshold: f32,
) -> Result<KwsSpotter, String> {
    runtime.create_spotter(model_dir, keywords_file, threshold, MAX_ACTIVE_PATHS)
}

/// One spotter on the production decoder contract, on `model` when the cascade
/// shares one.
fn create_spotter(
    tuning: RustKwsTuning,
    model: Option<&Arc<rust_kws::KwsModel>>,
    model_dir: &Path,
    keywords_file: &str,
    threshold: f32,
    paths: i32,
) -> Result<rust_kws::KeywordSpotter, String> {
    let config = spotter_config(tuning, model_dir, keywords_file, threshold, paths)?;
    match model {
        Some(model) => rust_kws::KeywordSpotter::with_model(&config, Arc::clone(model)),
        None => rust_kws::KeywordSpotter::new(&config),
    }
    .map_err(|error| format!("keyword spotter creation failed for {keywords_file}: {error}"))
}

fn spotter_config(
    tuning: RustKwsTuning,
    model_dir: &Path,
    keywords_file: &str,
    threshold: f32,
    paths: i32,
) -> Result<rust_kws::KeywordSpotterConfig, String> {
    let mut config = rust_kws::KeywordSpotterConfig::new(
        model_dir.join(ENCODER),
        model_dir.join(DECODER),
        model_dir.join(JOINER),
        model_dir.join("tokens.txt"),
        rust_kws::Keywords::File(model_dir.join(keywords_file)),
    );
    config.features.sample_rate = SAMPLE_RATE as u32;
    config.features.num_bins = FEATURE_DIM as usize;
    config.threads = tuning.threads;
    config.max_active_paths = usize::try_from(paths)
        .map_err(|_| format!("KWS max_active_paths must be positive: {paths}"))?;
    config.num_trailing_blanks = tuning.num_trailing_blanks;
    config.keywords_score = KEYWORDS_SCORE;
    config.keywords_threshold = threshold;
    config.active_path_ttl_frames = tuning.active_path_ttl_frames;
    config.trailing_blank_reset = tuning.trailing_blank_reset;
    config.softmax_f64 = tuning.softmax_f64;
    config.onset = tuning.onset.then_some(rust_kws::OnsetConfig {
        encoder_history: tuning.onset_params.encoder_history,
        feature_history: tuning.onset_params.feature_history,
        align_lead_frames: tuning.onset_params.align_lead_frames,
        posterior_threshold: tuning.onset_params.posterior_threshold,
        posterior_max_frames: tuning.onset_params.posterior_max_frames,
        acoustic_max_frames: tuning.onset_params.acoustic_max_frames,
        acoustic_search: tuning.acoustic_search,
        fricative: tuning.fricative,
    });
    config.threshold_tokens = tuning.threshold_tokens;
    config.joiner_batching = tuning.joiner_batching;
    config.merge_timestamps = tuning.merge_timestamps;
    config.portable_math = tuning.portable_math;
    config.static_batch = tuning.static_batch;
    config.decoder_cache = tuning.decoder_cache;
    config.encoder_state_reuse = tuning.encoder_state_reuse;
    config.decoder_table = tuning.decoder_table;
    // Only the wake spotter's streams own a ring; the confirm beam borrows it.
    config.shared_front_end = (tuning.shared_front_end && keywords_file == WAKE_KEYWORDS_FILE)
        .then(rust_kws::FrontEndConfig::default);
    // The no-fire window rides the confirm stream only; every stream may trace firings.
    // The guard runs on the wake and confirm spotters, as in the offline port runs.
    config.stale_gap_frames = if keywords_file == HEY_WAKE_KEYWORDS_FILE {
        None
    } else {
        tuning.stale_gap_frames
    };
    config.trace = tuning.trace.map(|trace| rust_kws::TraceConfig {
        history_tokens: trace.history_tokens,
        firing: trace.firing,
        window_steps: if keywords_file == CONFIRM_KEYWORDS_FILE && trace.window_frames > 0 {
            trace.capacity_steps
        } else {
            0
        },
    });
    Ok(config)
}

fn seconds_to_samples(seconds: f32) -> usize {
    (seconds.max(0.0) * SAMPLE_RATE as f32).round() as usize
}

/// Samples per feature frame (10 ms).
const FEATURE_SHIFT_SAMPLES: usize = SAMPLE_RATE as usize / 100;

/// The cut selection behind [`Kws::cut_start`], on plain values: `candidate` is the chosen
/// method's estimate on the outer clock (`None` when absent or `method` is emission).
/// Absent candidate: the legacy-control fallback to `emission_start`, or under `strict` with
/// the fricative method `(None, "no_boundary")`. The shift applies to any cut found.
fn select_cut(
    method: Option<KwsOnsetMethod>,
    strict: bool,
    shift_ms: i32,
    emission_start: usize,
    candidate: Option<usize>,
) -> (Option<usize>, &'static str) {
    let Some(method) = method else {
        return (Some(emission_start), KwsOnsetMethod::Emission.as_str());
    };
    let (base, source) = match candidate {
        Some(sample) => (sample, method.as_str()),
        None if strict && method == KwsOnsetMethod::Fricative => {
            return (None, NO_BOUNDARY_SOURCE);
        }
        None => (emission_start, KwsOnsetMethod::Emission.as_str()),
    };
    let shift = i64::from(shift_ms) * i64::from(SAMPLE_RATE) / 1_000;
    (Some((base as i64 + shift).max(0) as usize), source)
}

/// Seconds to samples for a keyword timestamp, `None` for a non-finite, negative or
/// implausibly large value (one hour of audio is the ceiling).
fn checked_seconds_to_samples(seconds: f32) -> Option<usize> {
    (seconds.is_finite() && (0.0..=3_600.0).contains(&seconds))
        .then(|| (seconds * SAMPLE_RATE as f32).round() as usize)
}

/// Emission confirm evidence: the stream-relative RAW sample of the first keyword
/// timestamp and its sample on the outer clock (`origin` + raw). The raw value is
/// `None` when the timestamp is missing or invalid; the mapped value is `None` when
/// the raw value is `None` OR the stream origin is missing (the raw value is still
/// returned without an origin). Never origin 0 or a default timestamp.
fn emission_confirm_sample(
    origin: Option<usize>,
    first_timestamp: Option<f32>,
) -> (Option<usize>, Option<usize>) {
    let raw = first_timestamp.and_then(checked_seconds_to_samples);
    let mapped = origin
        .zip(raw)
        .map(|(origin, raw)| origin.saturating_add(raw));
    (raw, mapped)
}

/// The cut a non-default [`KwsCutSource`] picks from the wake candidate's cut and the (already
/// guarded and shifted) confirm-path cut, with its source label. Pure: the temporal
/// plausibility guard has run before this is called. The same choice code serves the aligned
/// and the emission timestamp; only the label differs.
fn resolve_cut(source: KwsCutSource, wake_cut: usize, confirm_cut: usize) -> (usize, &'static str) {
    let emission = source.confirm_timestamp() == ConfirmTimestamp::Emission;
    let label = |aligned: &'static str, emission_label: &'static str| {
        if emission {
            emission_label
        } else {
            aligned
        }
    };
    match source {
        KwsCutSource::WakeCandidate => (wake_cut, "wake_candidate"),
        KwsCutSource::ConfirmAligned => (confirm_cut, "confirm_aligned"),
        KwsCutSource::ConfirmFirstToken | KwsCutSource::ConfirmFirstTokenEmission => (
            confirm_cut,
            label("confirm_first_token", "confirm_first_token_emission"),
        ),
        KwsCutSource::ConfirmEarlier | KwsCutSource::ConfirmEarlierEmission => {
            if confirm_cut <= wake_cut {
                (
                    confirm_cut,
                    label("confirm_earlier", "confirm_earlier_emission"),
                )
            } else {
                (wake_cut, label("wake_earlier", "wake_earlier_emission"))
            }
        }
        KwsCutSource::WakeWithinConfirm | KwsCutSource::WakeWithinConfirmEmission => {
            // Agreement filter only: the two cuts being close is not a proof that either is
            // right.
            if wake_cut.abs_diff(confirm_cut) <= WAKE_CONFIRM_AGREE_SAMPLES {
                (
                    wake_cut,
                    label("wake_within_400ms", "wake_within_400ms_emission"),
                )
            } else {
                (
                    confirm_cut,
                    label("confirm_beyond_400ms", "confirm_beyond_400ms_emission"),
                )
            }
        }
    }
}

/// The confirm-path cut for one latched command, or `None` when the command path's first token
/// (`estimate`, absolute sample) lies outside the wake candidate's span (allowing
/// [`CONFIRM_ALIGNED_GUARD_LEAD_SAMPLES`] before its start). A temporal PLAUSIBILITY window
/// only: outside it the estimate is not trusted for this candidate, inside it is not proof of
/// the same occurrence. Nothing here identifies which occurrence a token belongs to.
fn guarded_confirm_cut(candidate: WakeCandidate, estimate: usize, shift_ms: i32) -> Option<usize> {
    if estimate + CONFIRM_ALIGNED_GUARD_LEAD_SAMPLES < candidate.start_sample
        || estimate > candidate.end_sample
    {
        return None;
    }
    let shift = i64::from(shift_ms) * i64::from(SAMPLE_RATE) / 1_000;
    Some((estimate as i64 + shift).max(0) as usize)
}

/// True when a cut sample is plausible for `candidate`: the same temporal window
/// [`guarded_confirm_cut`] applies to the confirm estimate. Plausibility only, not provenance.
fn cut_is_plausible(candidate: WakeCandidate, cut: usize) -> bool {
    cut.saturating_add(CONFIRM_ALIGNED_GUARD_LEAD_SAMPLES) >= candidate.start_sample
        && cut <= candidate.end_sample
}

/// What a latched command cuts at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LatchedCut {
    /// A cut sample with its source label.
    Cut(usize, &'static str),
    /// The command is known but this source has no trustworthy boundary: it still fires, with
    /// no timed token and no cut.
    NoTrustedBoundary(&'static str),
}

/// The cut a latched command takes under a non-default `source`. Pure. `origin_known` and
/// `estimate` (the confirm sample the source uses on the outer clock, only computable with an
/// origin: the aligned first-token sample, or for the `*Emission` sources the first-token
/// emission sample) describe the confirm evidence. `ConfirmAligned` falls back to the wake
/// cut; the other sources use zero shift and abstain instead of falling back. A wake candidate
/// without a cut (strict D4; [`KwsSwitches::validate`] rejects that combination) has no
/// trusted boundary under any source.
fn decide_latched_cut(
    source: KwsCutSource,
    candidate: WakeCandidate,
    origin_known: bool,
    estimate: Option<usize>,
    legacy_shift_ms: i32,
) -> LatchedCut {
    let Some(wake_cut) = candidate.cut_start_sample else {
        return LatchedCut::NoTrustedBoundary("no_trusted_boundary_no_wake_cut");
    };
    match source {
        KwsCutSource::WakeCandidate => LatchedCut::Cut(wake_cut, "wake_candidate"),
        KwsCutSource::ConfirmAligned => match estimate {
            None => LatchedCut::Cut(wake_cut, "fallback_no_aligned_onset"),
            Some(estimate) => match guarded_confirm_cut(candidate, estimate, legacy_shift_ms) {
                Some(cut) => LatchedCut::Cut(cut, "confirm_aligned"),
                None => LatchedCut::Cut(wake_cut, "fallback_occurrence_mismatch"),
            },
        },
        KwsCutSource::ConfirmFirstToken
        | KwsCutSource::ConfirmEarlier
        | KwsCutSource::WakeWithinConfirm
        | KwsCutSource::ConfirmFirstTokenEmission
        | KwsCutSource::ConfirmEarlierEmission
        | KwsCutSource::WakeWithinConfirmEmission => {
            if !origin_known {
                return LatchedCut::NoTrustedBoundary("no_trusted_boundary_no_origin");
            }
            let emission = source.confirm_timestamp() == ConfirmTimestamp::Emission;
            let Some(estimate) = estimate else {
                return LatchedCut::NoTrustedBoundary(if emission {
                    "no_trusted_boundary_no_emission_timestamp"
                } else {
                    "no_trusted_boundary_no_estimate"
                });
            };
            match guarded_confirm_cut(candidate, estimate, 0) {
                Some(confirm_cut) => {
                    // The sources that can select the wake cut need the same plausibility
                    // check; a guard-passing confirm estimate does not vouch for it. The
                    // first-token sources never select it.
                    if !matches!(
                        source,
                        KwsCutSource::ConfirmFirstToken | KwsCutSource::ConfirmFirstTokenEmission
                    ) && !cut_is_plausible(candidate, wake_cut)
                    {
                        return LatchedCut::NoTrustedBoundary(
                            "no_trusted_boundary_wake_cut_out_of_range",
                        );
                    }
                    let (cut, label) = resolve_cut(source, wake_cut, confirm_cut);
                    LatchedCut::Cut(cut, label)
                }
                // A failed guard never falls back to the wake cut.
                None => LatchedCut::NoTrustedBoundary(if emission {
                    "no_trusted_boundary_outside_candidate_window"
                } else {
                    "no_trusted_boundary_occurrence_mismatch"
                }),
            }
        }
    }
}

/// The timed `zephyr` token a wake or command result carries, relative to the snapshot; no
/// token when there is no trusted boundary (`cut` `None`), so the worker attempts no cut
/// (source-traced `piece_not_found`, pending authorized replay). `clamp_end` keeps
/// `end >= start`.
fn wake_tokens(
    cut: Option<usize>,
    end_sample: usize,
    snapshot_start_sample: usize,
    clamp_end: bool,
) -> Vec<TimedToken> {
    cut.map(|cut| {
        let start = cut.saturating_sub(snapshot_start_sample) as f32 / SAMPLE_RATE as f32;
        let end = end_sample.saturating_sub(snapshot_start_sample) as f32 / SAMPLE_RATE as f32;
        vec![TimedToken {
            text: " zephyr".to_string(),
            start,
            end: if clamp_end { end.max(start) } else { end },
        }]
    })
    .unwrap_or_default()
}

/// An onset estimate on a stream whose clock starts at `origin`, in samples.
/// Integer arithmetic: feature frame `f` is exactly `160 f` samples.
fn onset_sample(origin: usize, estimate: rust_kws::OnsetEstimate) -> usize {
    origin.saturating_add(estimate.feature_frame.max(0) as usize * FEATURE_SHIFT_SAMPLES)
}

/// The shared confirm beam's requested start on the wake clock, in encoder frames: the FULL
/// signed expression `candidate_start - CONFIRM_CONTEXT_SAMPLES - wake_origin` floored toward
/// minus infinity by 640. Negative when the request precedes the wake stream's origin; the
/// beam, not this function, clamps it and reports `origin_clamped`. No unsigned subtraction
/// happens first, so the sign is never lost.
fn shared_requested_frame(candidate_start_sample: usize, wake_origin_sample: usize) -> i64 {
    let signed = |sample: usize| i64::try_from(sample).unwrap_or(i64::MAX);
    signed(candidate_start_sample)
        .saturating_sub(CONFIRM_CONTEXT_SAMPLES as i64)
        .saturating_sub(signed(wake_origin_sample))
        .div_euclid(ENCODER_FRAME_SAMPLES as i64)
}

/// A frame on the confirm stream's own encoder clock, saturated to `i32`.
fn trace_frame(frame: i64) -> i32 {
    frame.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// The confirm stream's no-fire window as encoder frames `from..to` on its own clock: from
/// `margin_frames` before the wake candidate's first token to `margin_frames` after its end.
fn confirm_trace_window(
    anchor_sample: usize,
    start_sample: usize,
    end_sample: usize,
    margin_frames: u32,
) -> (i32, i32) {
    let frame = |sample: usize| {
        (sample as i64 - anchor_sample as i64).div_euclid(ENCODER_FRAME_SAMPLES as i64)
    };
    let margin = i64::from(margin_frames);
    (
        trace_frame(frame(start_sample) - margin),
        trace_frame(frame(end_sample) + margin + 1),
    )
}

fn transcription_result_at_wake(
    result: KeywordResult,
    candidate: WakeCandidate,
    snapshot_start_sample: usize,
) -> TranscriptionResult {
    let text = command_text(&result.keyword).expect("validated keyword label");
    // No trusted boundary (strict D4 without a candidate): the command keeps its text but
    // carries no timed token, so the worker attempts no audio cut.
    let tokens = wake_tokens(
        candidate.cut_start_sample,
        candidate.end_sample,
        snapshot_start_sample,
        true,
    );
    TranscriptionResult {
        text: text.to_string(),
        tokens,
    }
}

fn empty_result() -> TranscriptionResult {
    TranscriptionResult {
        text: String::new(),
        tokens: Vec::new(),
    }
}

fn command_text(keyword: &str) -> Option<&'static str> {
    match keyword.trim().trim_start_matches('@') {
        "ZEPHYR_STOP" => Some("zephyr stop"),
        "ZEPHYR_SEND" => Some("zephyr send"),
        "ZEPHYR_CANCEL" => Some("zephyr cancel"),
        "ZEPHYR" => Some("zephyr"),
        _ => None,
    }
}

fn accepts_keyword_result(keyword: &str) -> bool {
    let normalized = keyword.trim().trim_start_matches('@');
    normalized == HEY_WAKE_KEYWORD || command_text(normalized).is_some()
}

/// Validate one emitted acoustic path. Request wall age, live-audio lag &
/// stream identity are enforced by the outer worker because the spotter's timestamp
/// clock may omit leading silence & cannot be compared to accepted PCM count.
fn keyword_timestamps_are_plausible(timestamps: &[f32], max_span_seconds: f32) -> bool {
    let (Some(start), Some(end)) = (timestamps.first(), timestamps.last()) else {
        return false;
    };
    start.is_finite()
        && end.is_finite()
        && *start >= 0.0
        && *end >= *start
        && *end - *start <= max_span_seconds
        && timestamps
            .windows(2)
            .all(|pair| pair[0].is_finite() && pair[1].is_finite() && pair[1] >= pair[0])
}

fn validate_model_dir(dir: &Path) -> Result<(), String> {
    for file in [
        ENCODER,
        DECODER,
        JOINER,
        "tokens.txt",
        "keywords-wake.txt",
        "keywords-confirm.txt",
    ] {
        let path = dir.join(file);
        if !path.is_file() {
            return Err(format!("bundled KWS resource missing {}", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        stage: &'static str,
        candidate_id: Option<u64>,
        adopted: Option<bool>,
        latched: bool,
    ) -> KwsOnsetReport {
        // Every sample field is identical across reports on purpose.
        KwsOnsetReport {
            stage,
            keyword: "ZEPHYR".into(),
            origin_sample: Some(0),
            live_sample: Some(48_000),
            emission_sample: 16_000,
            aligned_sample: None,
            posterior_sample: None,
            acoustic_sample: None,
            acoustic_rule: None,
            fricative_sample: None,
            fricative_abstain: Some("abstain:no_transition"),
            fricative_relative_feature: None,
            fricative_unmappable: None,
            token_frames: Vec::new(),
            aligned_frames: Vec::new(),
            fired_frame: 0,
            gap_drops: 0,
            gap_drops_chunk: 0,
            max_gap_frames: 0,
            gap_violation: false,
            token_probs: Vec::new(),
            score: 0.0,
            threshold: 0.0,
            chosen: "fricative",
            chosen_sample: None,
            chosen_source: Some(NO_BOUNDARY_SOURCE),
            cut_sample: None,
            emission_sample_checked: None,
            cut_wake_sample: None,
            cut_confirm_sample: None,
            cut_selected: None,
            cut_method: None,
            cut_raw_confirm_sample: None,
            latched,
            candidate_id,
            adopted,
            shared: None,
        }
    }

    #[test]
    fn strict_with_an_absent_d4_candidate_has_no_boundary() {
        let fricative = Some(KwsOnsetMethod::Fricative);
        assert_eq!(
            select_cut(fricative, true, 0, 1_000, None),
            (None, "no_boundary")
        );
        // The shift never turns "no boundary" into one.
        assert_eq!(
            select_cut(fricative, true, -100, 1_000, None),
            (None, "no_boundary")
        );
        // Default (non-strict): the explicit legacy-control fallback to emission.
        assert_eq!(
            select_cut(fricative, false, 0, 1_000, None),
            (Some(1_000), "emission")
        );
        // A D4 candidate is cut directly, shifted (-100 ms = -1600 samples).
        assert_eq!(
            select_cut(fricative, true, -100, 1_000, Some(8_000)),
            (Some(6_400), "fricative")
        );
        assert_eq!(
            select_cut(None, true, 0, 1_000, None),
            (Some(1_000), "emission")
        );
    }

    #[test]
    fn strict_needs_the_fricative_onset() {
        let mut switches = KwsSwitches {
            fricative_strict: true,
            ..KwsSwitches::default()
        };
        assert!(switches.validate().is_err());
        switches.onset = Some(KwsOnsetMethod::Fricative);
        switches.fricative.enabled = true;
        assert!(switches.validate().is_ok());
    }

    #[test]
    fn trace_switch_must_record_something_and_has_bounded_capacity() {
        let mut switches = KwsSwitches::default();
        assert_eq!(switches.trace, None);
        switches.trace = Some(KwsTraceConfig::default());
        assert!(switches.validate().is_err());
        switches.trace = Some(KwsTraceConfig {
            firing: true,
            ..KwsTraceConfig::default()
        });
        assert!(switches.validate().is_ok());
        switches.trace = Some(KwsTraceConfig {
            window_frames: 10,
            capacity_steps: 0,
            ..KwsTraceConfig::default()
        });
        assert!(switches.validate().is_err());
        switches.trace = Some(KwsTraceConfig {
            window_frames: 10,
            capacity_steps: KwsTraceConfig::DEFAULT_CAPACITY_STEPS,
            ..KwsTraceConfig::default()
        });
        assert!(switches.validate().is_ok());
    }

    #[test]
    fn gap_guard_is_off_by_default_and_range_checked() {
        let mut switches = KwsSwitches::default();
        assert_eq!(switches.gap_guard, None);
        for gap in [0, -1, 1001] {
            switches.gap_guard = Some(gap);
            assert!(switches.validate().is_err(), "{gap}");
        }
        for gap in [1, 8, 12, 16, 1000] {
            switches.gap_guard = Some(gap);
            assert!(switches.validate().is_ok(), "{gap}");
        }
    }

    #[test]
    fn shared_beam_request_keeps_the_sign_before_flooring() {
        // origin 0, candidate 16,000: 16,000 - 32,000 = -16,000 samples = -25 frames, which
        // the beam clamps and reports as origin_clamped (the old saturating subtraction asked
        // for frame 0 and reported full context).
        assert_eq!(shared_requested_frame(16_000, 0), -25);
        // A normal case: (100,000 - 32,000 - 0) / 640 = 106.25 floors to 106.
        assert_eq!(shared_requested_frame(100_000, 0), 106);
        // The origin counts: (100,000 - 32,000 - 8,000) / 640 = 93.75 floors to 93.
        assert_eq!(shared_requested_frame(100_000, 8_000), 93);
        // One sample before the origin is frame -1, not 0 (floor, not truncation).
        assert_eq!(shared_requested_frame(32_000, 1), -1);
        // Exactly on the origin is frame 0: full, not clamped.
        assert_eq!(shared_requested_frame(32_000, 0), 0);
        assert!(shared_requested_frame(0, usize::MAX) < 0);
    }

    #[test]
    fn confirm_trace_window_is_on_the_stream_clock() {
        // Stream starts at sample 32_000; the candidate spans frames 10..=20 of that clock.
        let anchor = 32_000;
        let start = anchor + 10 * ENCODER_FRAME_SAMPLES;
        let end = anchor + 20 * ENCODER_FRAME_SAMPLES;
        assert_eq!(confirm_trace_window(anchor, start, end, 5), (5, 26));
        // A candidate before the anchor maps to negative frames, not a wrapped number.
        assert_eq!(
            confirm_trace_window(anchor, anchor - ENCODER_FRAME_SAMPLES, anchor, 0),
            (-1, 1)
        );
    }

    #[test]
    fn no_boundary_carries_no_timed_token() {
        assert!(wake_tokens(None, 20_000, 0, true).is_empty());
        assert!(wake_tokens(None, 20_000, 0, false).is_empty());
        let tokens = wake_tokens(Some(8_000), 20_000, 0, true);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].start, 0.5);
    }

    #[test]
    fn rejected_overlap_is_not_adopted_and_ids_are_never_reused() {
        let mut clock = CascadeClock::default();
        let none = (None, NO_BOUNDARY_SOURCE);
        assert_eq!(clock.observe_wake(1_000, 2_000, none), Some(1));
        // Overlaps the adopted candidate: rejected, the candidate keeps its identity.
        assert_eq!(clock.observe_wake(1_500, 2_500, none), None);
        assert_eq!(clock.candidate().map(|c| c.id), Some(1));
        assert_eq!(clock.candidate().and_then(|c| c.cut_start_sample), None);
        // An identical detection after the real `clear` (the invalidate and reset path,
        // which leaves retained reports in place) is a new identity.
        clock.clear();
        assert!(clock.candidate().is_none());
        assert_eq!(clock.observe_wake(1_000, 2_000, none), Some(2));
        clock.clear();
        assert_eq!(clock.observe_wake(1_000, 2_000, none), Some(3));
    }

    #[test]
    fn latched_command_is_attributed_by_identity_not_by_sample_equality() {
        // Adopted wake 1, a rejected overlap, and a later adopted wake 2: every sample field
        // is identical (all None under strict abstention).
        let reports = vec![
            report("wake", Some(1), Some(true), false),
            report("wake", None, Some(false), false),
            report("wake", Some(2), Some(true), false),
            report("confirm", Some(1), None, true),
        ];
        let (wake, confirm) = latched_owner(&reports).expect("latched");
        assert!(std::ptr::eq(wake.expect("owner"), &reports[0]));
        assert!(std::ptr::eq(confirm, &reports[3]));
        assert_ne!(reports[0].candidate_id, reports[2].candidate_id);
        // The rejected report is never the owner, even when it is the newest wake report.
        let reports = vec![
            report("wake", Some(1), Some(true), false),
            report("confirm", Some(1), None, true),
            report("wake", None, Some(false), false),
        ];
        let (wake, _) = latched_owner(&reports).expect("latched");
        assert!(std::ptr::eq(wake.expect("owner"), &reports[0]));
        // No adopted wake with the confirm's id: the confirm evidence stays, owner missing.
        let reports = vec![
            report("wake", None, Some(false), false),
            report("confirm", Some(7), None, true),
        ];
        let (wake, confirm) = latched_owner(&reports).expect("latched");
        assert!(wake.is_none());
        assert!(std::ptr::eq(confirm, &reports[1]));
        // A matching wake that only appears AFTER the confirm is not its owner.
        let reports = vec![
            report("confirm", Some(1), None, true),
            report("wake", Some(1), Some(true), false),
        ];
        assert!(latched_owner(&reports).expect("latched").0.is_none());
        // `fresh_confirm` is supported deliberately: it carries the same candidate id.
        let reports = vec![
            report("wake", Some(4), Some(true), false),
            report("fresh_confirm", Some(4), None, true),
        ];
        let (wake, confirm) = latched_owner(&reports).expect("latched");
        assert!(std::ptr::eq(wake.expect("owner"), &reports[0]));
        assert_eq!(confirm.stage, "fresh_confirm");
    }
}
