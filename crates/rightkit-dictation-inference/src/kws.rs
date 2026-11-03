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
pub use rightkit_asr::kws::{JoinerBatching, MergeTimestamps, ThresholdTokens};
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
}

impl RustKwsTuning {
    /// The production contract (active-keyword TTL, no trailing-blank reset, f32
    /// softmax), with onset estimation on (logging only: the cut stays on the emission timestamp).
    const PRODUCTION: Self = Self {
        active_path_ttl_frames: Some(ACTIVE_KEYWORD_TTL_FRAMES),
        trailing_blank_reset: TRAILING_BLANK_RESET,
        softmax_f64: false,
        onset: true,
        threshold_tokens: rust_kws::ThresholdTokens::Upstream,
        joiner_batching: rust_kws::JoinerBatching::Beam,
        merge_timestamps: rust_kws::MergeTimestamps::First,
        portable_math: false,
        static_batch: false,
        decoder_cache: false,
        shared_model: false,
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
    /// Start of the speech burst holding the keyword, from the energy envelope.
    Acoustic,
}

impl KwsOnsetMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Emission => "emission",
            Self::Posterior => "posterior",
            Self::Aligned => "aligned",
            Self::Acoustic => "acoustic",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "emission" => Some(Self::Emission),
            "posterior" => Some(Self::Posterior),
            "aligned" => Some(Self::Aligned),
            "acoustic" => Some(Self::Acoustic),
            _ => None,
        }
    }

    /// This method's candidate, when it was estimated.
    fn candidate(self, onset: &rust_kws::KeywordOnset) -> Option<rust_kws::OnsetEstimate> {
        match self {
            Self::Emission => Some(onset.emission),
            Self::Posterior => onset.posterior,
            Self::Aligned => onset.aligned,
            Self::Acoustic => onset.acoustic,
        }
    }
}

/// Live switches for the Rust keyword spotter, read from `RIGHTKIT_KWS_*` by
/// [`KwsSwitches::from_env`] in test / `test-mode` builds (release builds read
/// nothing and get [`KwsSwitches::default`]). The defaults are today's
/// behaviour. `onset` and `onset_shift_ms` move only the cut; the other
/// switches change what is detected, so a run with them is a new qualification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KwsSwitches {
    /// `RIGHTKIT_KWS_ONSET` = `emission` | `posterior` | `aligned` | `acoustic` |
    /// `off`: which candidate becomes the cut start; `None` (`off`) also stops
    /// estimating and logging candidates. Default `emission`.
    pub onset: Option<KwsOnsetMethod>,
    /// `RIGHTKIT_KWS_ONSET_SHIFT_MS`: signed milliseconds added to the chosen
    /// candidate, -1000..=1000 (ignored with `RIGHTKIT_KWS_ONSET=off`). Default 0.
    pub onset_shift_ms: i32,
    /// `RIGHTKIT_KWS_THRESHOLD_TOKENS` = `upstream` | `matched`. Default `upstream`.
    pub threshold_tokens: rust_kws::ThresholdTokens,
    /// `RIGHTKIT_KWS_JOINER_BATCHING` = `beam` | `per_hypothesis`. Default `beam`.
    pub joiner_batching: rust_kws::JoinerBatching,
    /// `RIGHTKIT_KWS_MERGE_TIMESTAMPS` = `first` | `likelier`: whose timestamps a
    /// merged beam path keeps. Default `first` (upstream, which can keep a stale
    /// early first-token time).
    pub merge_timestamps: rust_kws::MergeTimestamps,
    /// `RIGHTKIT_KWS_PORTABLE_MATH` = `0` | `1`. Default `0`.
    pub portable_math: bool,
    /// `RIGHTKIT_KWS_TTL_FRAMES` = frames (1..=1000) | `off`. Default 38.
    pub ttl_frames: Option<i32>,
    /// `RIGHTKIT_KWS_STATIC_BATCH` = `0` | `1`: pin the encoder's batch dimension to
    /// 1 (~10% less KWS CPU). Bit-identical for the shipped int8 encoder on
    /// x86-64 Linux; re-verify on each OS (`kws_static_batch_encoder_is_bit_identical`
    /// in rightkit-asr). Default `0`.
    pub static_batch: bool,
    /// `RIGHTKIT_KWS_DECODER_CACHE` = `0` | `1`: reuse decoder rows per token context
    /// (~5% less KWS CPU). Exact where the decoder is batch-invariant
    /// (`kws_decoder_is_batch_invariant` in rightkit-asr). Default `0`.
    pub decoder_cache: bool,
    /// `RIGHTKIT_KWS_SHARED_MODEL` = `0` | `1`: the wake, confirm and hey-wake
    /// spotters share one set of ONNX sessions (~46 MB less RSS for three).
    /// Default `0`.
    pub shared_model: bool,
}

impl Default for KwsSwitches {
    fn default() -> Self {
        let tuning = RustKwsTuning::PRODUCTION;
        Self {
            onset: Some(KwsOnsetMethod::Emission),
            onset_shift_ms: 0,
            threshold_tokens: tuning.threshold_tokens,
            joiner_batching: tuning.joiner_batching,
            merge_timestamps: tuning.merge_timestamps,
            portable_math: tuning.portable_math,
            ttl_frames: tuning.active_path_ttl_frames,
            static_batch: tuning.static_batch,
            decoder_cache: tuning.decoder_cache,
            shared_model: tuning.shared_model,
        }
    }
}

impl KwsSwitches {
    /// [`Self::default`] with any `RIGHTKIT_KWS_*` switch applied. Test and
    /// `test-mode` builds only: release builds never read the environment.
    pub fn from_env() -> Self {
        let (switches, warnings) = Self::from_lookup(|name| match name {
            "RIGHTKIT_KWS_ONSET" => rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_ONSET").ok(),
            "RIGHTKIT_KWS_ONSET_SHIFT_MS" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_ONSET_SHIFT_MS").ok()
            }
            "RIGHTKIT_KWS_THRESHOLD_TOKENS" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_THRESHOLD_TOKENS").ok()
            }
            "RIGHTKIT_KWS_JOINER_BATCHING" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_JOINER_BATCHING").ok()
            }
            "RIGHTKIT_KWS_MERGE_TIMESTAMPS" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_MERGE_TIMESTAMPS").ok()
            }
            "RIGHTKIT_KWS_PORTABLE_MATH" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_PORTABLE_MATH").ok()
            }
            "RIGHTKIT_KWS_TTL_FRAMES" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_TTL_FRAMES").ok()
            }
            "RIGHTKIT_KWS_STATIC_BATCH" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_STATIC_BATCH").ok()
            }
            "RIGHTKIT_KWS_DECODER_CACHE" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_DECODER_CACHE").ok()
            }
            "RIGHTKIT_KWS_SHARED_MODEL" => {
                rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_SHARED_MODEL").ok()
            }
            _ => None,
        });
        for warning in warnings {
            tracing::warn!("{warning}");
        }
        switches
    }

    /// Parse the switches from `lookup`; invalid values keep the default and
    /// produce a warning.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> (Self, Vec<String>) {
        let mut switches = Self::default();
        let mut warnings = Vec::new();
        let mut read = |name: &str, apply: &mut dyn FnMut(&str) -> bool, expected: &str| {
            if let Some(value) = lookup(name) {
                if !apply(value.trim()) {
                    warnings.push(format!(
                        "ignoring invalid {name}={value:?}; expected {expected}"
                    ));
                }
            }
        };
        read(
            "RIGHTKIT_KWS_ONSET",
            &mut |v| match v.to_ascii_lowercase().as_str() {
                "off" => {
                    switches.onset = None;
                    true
                }
                other => KwsOnsetMethod::parse(other)
                    .map(|method| switches.onset = Some(method))
                    .is_some(),
            },
            "emission, posterior, aligned, acoustic or off",
        );
        read(
            "RIGHTKIT_KWS_ONSET_SHIFT_MS",
            &mut |v| {
                v.parse::<i32>()
                    .ok()
                    .filter(|ms| (-1_000..=1_000).contains(ms))
                    .map(|ms| switches.onset_shift_ms = ms)
                    .is_some()
            },
            "milliseconds in -1000..=1000",
        );
        read(
            "RIGHTKIT_KWS_THRESHOLD_TOKENS",
            &mut |v| match v.to_ascii_lowercase().as_str() {
                "upstream" => {
                    switches.threshold_tokens = rust_kws::ThresholdTokens::Upstream;
                    true
                }
                "matched" | "matched_keyword" => {
                    switches.threshold_tokens = rust_kws::ThresholdTokens::MatchedKeyword;
                    true
                }
                _ => false,
            },
            "upstream or matched",
        );
        read(
            "RIGHTKIT_KWS_JOINER_BATCHING",
            &mut |v| match v.to_ascii_lowercase().replace('-', "_").as_str() {
                "beam" => {
                    switches.joiner_batching = rust_kws::JoinerBatching::Beam;
                    true
                }
                "per_hypothesis" => {
                    switches.joiner_batching = rust_kws::JoinerBatching::PerHypothesis;
                    true
                }
                _ => false,
            },
            "beam or per_hypothesis",
        );
        read(
            "RIGHTKIT_KWS_MERGE_TIMESTAMPS",
            &mut |v| match v.to_ascii_lowercase().as_str() {
                "first" => {
                    switches.merge_timestamps = rust_kws::MergeTimestamps::First;
                    true
                }
                "likelier" => {
                    switches.merge_timestamps = rust_kws::MergeTimestamps::Likelier;
                    true
                }
                _ => false,
            },
            "first or likelier",
        );
        let flag = |v: &str| match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "on" => Some(true),
            "0" | "false" | "off" => Some(false),
            _ => None,
        };
        read(
            "RIGHTKIT_KWS_PORTABLE_MATH",
            &mut |v| flag(v).map(|on| switches.portable_math = on).is_some(),
            "0 or 1",
        );
        read(
            "RIGHTKIT_KWS_STATIC_BATCH",
            &mut |v| flag(v).map(|on| switches.static_batch = on).is_some(),
            "0 or 1",
        );
        read(
            "RIGHTKIT_KWS_DECODER_CACHE",
            &mut |v| flag(v).map(|on| switches.decoder_cache = on).is_some(),
            "0 or 1",
        );
        read(
            "RIGHTKIT_KWS_SHARED_MODEL",
            &mut |v| flag(v).map(|on| switches.shared_model = on).is_some(),
            "0 or 1",
        );
        read(
            "RIGHTKIT_KWS_TTL_FRAMES",
            &mut |v| {
                if v.eq_ignore_ascii_case("off") {
                    switches.ttl_frames = None;
                    return true;
                }
                v.parse::<i32>()
                    .ok()
                    .filter(|frames| (1..=1_000).contains(frames))
                    .map(|frames| switches.ttl_frames = Some(frames))
                    .is_some()
            },
            "encoder frames in 1..=1000 or off",
        );
        (switches, warnings)
    }

    /// `base` with these switches applied.
    fn tuning(&self, base: RustKwsTuning) -> RustKwsTuning {
        RustKwsTuning {
            active_path_ttl_frames: self.ttl_frames,
            onset: self.onset.is_some(),
            threshold_tokens: self.threshold_tokens,
            joiner_batching: self.joiner_batching,
            merge_timestamps: self.merge_timestamps,
            portable_math: self.portable_math,
            static_batch: self.static_batch,
            decoder_cache: self.decoder_cache,
            shared_model: self.shared_model,
            ..base
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
    /// Emission (encoder, 40 ms) frame of each token, on the stream's clock.
    pub token_frames: Vec<i32>,
    /// Forced-alignment frame of each token.
    pub aligned_frames: Vec<i32>,
    pub fired_frame: i32,
    pub token_probs: Vec<f32>,
    pub score: f32,
    pub threshold: f32,
    /// The configured method and the cut it gives for this detection (wake stages).
    pub chosen: &'static str,
    pub chosen_sample: Option<usize>,
    /// The method whose candidate `chosen_sample` used (`emission` on fallback).
    pub chosen_source: Option<&'static str>,
    /// The wake candidate's cut start a latch would use after this detection.
    pub cut_sample: Option<usize>,
    /// Confirm stages: this detection latched a command.
    pub latched: bool,
}

/// Reports kept for [`Kws::take_onset_reports`]; older ones are dropped.
const ONSET_REPORTS_KEPT: usize = 16;

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
        .map(|spotter| KwsSpotter(RefCell::new(spotter)))
    }
}

/// One keyword spotter (model sessions + keyword graph). `decode` takes `&mut`;
/// a spotter is only touched by its one ASR worker thread, one call at a time,
/// so the borrow never overlaps.
struct KwsSpotter(RefCell<rust_kws::KeywordSpotter>);

/// One audio stream of a [`KwsSpotter`].
struct KwsStream(rust_kws::KeywordStream);

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
    latched: bool,
    /// Live switches.
    switches: KwsSwitches,
    max_active_paths: i32,
    onset_reports: VecDeque<KwsOnsetReport>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WakeCandidate {
    /// The wake word's emission onset: owns debounce, confirmation anchor and
    /// expiry exactly as before onset candidates existed.
    start_sample: usize,
    end_sample: usize,
    /// Where a confirmed command cuts: the chosen onset candidate
    /// ([`KwsSwitches::onset`]); equal to `start_sample` by default.
    cut_start_sample: usize,
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

fn next_decode_boundary(sample: usize) -> usize {
    sample.saturating_add(KWS_DECODE_STEP_SAMPLES)
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
}

impl CascadeClock {
    fn observe_wake(
        &mut self,
        start_sample: usize,
        end_sample: usize,
        cut_start_sample: usize,
    ) -> bool {
        let next = WakeCandidate {
            start_sample,
            end_sample: end_sample.max(start_sample),
            cut_start_sample,
        };
        // The spotter may report the same acoustic occurrence more than once as
        // its rolling stream advances. Debounce only overlapping intervals.
        // A fixed start-time refractory window is unsafe: a user can say a
        // second Zephyr shortly after an unconfirmed first one, and that
        // second command must own the cut boundary.
        if self.candidate.is_some_and(|current| {
            current.start_sample <= next.end_sample && next.start_sample <= current.end_sample
        }) {
            return false;
        }
        self.candidate = Some(next);
        true
    }

    fn candidate(&self) -> Option<WakeCandidate> {
        self.candidate
    }

    fn expire_if_needed(&mut self, live_sample: usize) -> bool {
        let expired = self.candidate.is_some_and(|candidate| {
            live_sample > candidate.end_sample.saturating_add(CONFIRM_TIMEOUT_SAMPLES)
        });
        if expired {
            self.candidate = None;
        }
        expired
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

// The cascade is created, mutated & destroyed by one ASR worker thread; every
// spotter and stream lives inside it. The spotters sit in `RefCell`s that are
// never shared across threads.
unsafe impl Send for Kws {}

impl Kws {
    /// Load the cascade with [`KwsSwitches::from_env`].
    pub fn load(model_dir: &Path) -> Result<Self, String> {
        Self::load_with_switches(model_dir, KwsSwitches::from_env())
    }

    /// Load the cascade with explicit switches.
    pub fn load_with_switches(model_dir: &Path, switches: KwsSwitches) -> Result<Self, String> {
        Self::load_with_paths_and_switches(
            model_dir,
            MAX_ACTIVE_PATHS,
            switches.tuning(RustKwsTuning::PRODUCTION),
            switches,
        )
    }

    /// Bounded replay only; production keeps MAX_ACTIVE_PATHS.
    #[doc(hidden)]
    pub fn load_for_replay_with_paths(model_dir: &Path, paths: i32) -> Result<Self, String> {
        if !matches!(paths, 4 | 8 | 16) {
            return Err("replay paths must be 4, 8 or 16".into());
        }
        let switches = KwsSwitches::from_env();
        Self::load_with_paths_and_switches(
            model_dir,
            paths,
            switches.tuning(RustKwsTuning::PRODUCTION),
            switches,
        )
    }

    fn load_with_paths_and_switches(
        model_dir: &Path,
        paths: i32,
        tuning: RustKwsTuning,
        switches: KwsSwitches,
    ) -> Result<Self, String> {
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
            NUM_THREADS,
            paths,
            NUM_TRAILING_BLANKS,
            switches
        );
        Ok(Self {
            runtime,
            model_dir: model_dir.to_path_buf(),
            wake_spotter,
            confirm_spotter,
            hey_wake_spotter: None,
            hey_wake_available,
            hey_wake_threshold: None,
            idle_wake_stream: None,
            clock: CascadeClock::default(),
            wake_stream: None,
            confirmation_stream: None,
            latched: false,
            switches,
            max_active_paths: paths,
            onset_reports: VecDeque::new(),
        })
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

    /// The cut start for a wake detection whose emission onset is
    /// `emission_start`: the chosen candidate on `origin`'s clock (the emission
    /// when that candidate was not estimated), shifted by `onset_shift_ms`.
    /// Returns the sample and the method whose candidate it used.
    fn cut_start(
        &self,
        emission_start: usize,
        origin: usize,
        result: &KeywordResult,
    ) -> (usize, &'static str) {
        let Some(method) = self.switches.onset else {
            return (emission_start, KwsOnsetMethod::Emission.as_str());
        };
        let candidate = match method {
            KwsOnsetMethod::Emission => None,
            _ => result
                .detection
                .as_ref()
                .and_then(|detection| detection.onset.as_ref())
                .and_then(|onset| method.candidate(onset))
                .map(|estimate| onset_sample(origin, estimate)),
        };
        let (base, source) = match candidate {
            Some(sample) => (sample, method.as_str()),
            None => (emission_start, KwsOnsetMethod::Emission.as_str()),
        };
        let shift = i64::from(self.switches.onset_shift_ms) * i64::from(SAMPLE_RATE) / 1_000;
        ((base as i64 + shift).max(0) as usize, source)
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
        chosen: Option<(usize, &'static str)>,
        cut_sample: Option<usize>,
        latched: bool,
    ) {
        let Some(detection) = result.detection.as_deref() else {
            return;
        };
        let Some(onset) = detection.onset.as_ref() else {
            return;
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
            aligned_sample: at(onset.aligned),
            posterior_sample: at(onset.posterior),
            acoustic_sample: at(onset.acoustic),
            acoustic_rule: onset.acoustic_rule.map(rust_kws::AcousticRule::as_str),
            token_frames: detection.result.frames.clone(),
            aligned_frames: onset.aligned_frames.clone(),
            fired_frame: detection.fired_frame,
            token_probs: detection.token_probs.clone(),
            score: detection.score,
            threshold: detection.threshold,
            chosen: self.switches.onset.map_or("off", KwsOnsetMethod::as_str),
            chosen_sample: chosen.map(|(sample, _)| sample),
            chosen_source: chosen.map(|(_, source)| source),
            cut_sample,
            latched,
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
            next_decode_sample: KWS_DECODE_STEP_SAMPLES,
        });
        Ok(())
    }

    pub fn stop_idle_wake(&mut self) {
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
                start_sample: snapshot_start_sample,
                end_sample: snapshot_start_sample,
                cut_start_sample: snapshot_start_sample,
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
            .decode_fresh(audio, FRESH_CONFIRMATION_TAIL_SAMPLES)?
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
        let Some(result) = self
            .confirm_spotter
            .decode_fresh(audio, FRESH_CONFIRMATION_TAIL_SAMPLES)?
        else {
            return Ok((empty_result(), None));
        };
        let latch = command_text(&result.keyword).is_some_and(|text| text != "zephyr");
        self.report_onset(
            "fresh_confirm",
            &result,
            snapshot_start_sample,
            snapshot_start_sample.map(|start| start.saturating_add(audio.len())),
            None,
            Some(candidate.cut_start_sample),
            latch,
        );
        if !latch {
            return Ok((empty_result(), None));
        }
        self.latched = true;
        self.destroy_confirmation_stream();
        Ok((
            transcription_result_at_wake(result, candidate, 0),
            Some(candidate.cut_start_sample),
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
        if self.clock.expire_if_needed(live_total_sample) {
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

        if self.clock.expire_if_needed(live_total_sample) {
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
                    self.clock.observe_wake(start, end, cut.0);
                    if self.clock.candidate() != previous_candidate {
                        self.destroy_confirmation_stream();
                    }
                    let cut_now = self.clock.candidate().map(|c| c.cut_start_sample);
                    self.report_onset(
                        "wake",
                        &wake,
                        Some(base),
                        Some(live_total_sample),
                        Some(cut),
                        cut_now,
                        false,
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
                    .map(|stream| stream.origin_sample);
                self.report_onset(
                    "confirm",
                    &result,
                    origin,
                    Some(live_total_sample),
                    None,
                    Some(candidate.cut_start_sample),
                    latch,
                );
                if latch {
                    self.latched = true;
                    self.destroy_confirmation_stream();
                    return Ok((
                        transcription_result_at_wake(result, candidate, snapshot_start_sample),
                        Some(candidate.cut_start_sample),
                    ));
                }
            }
        } else {
            self.destroy_confirmation_stream();
        }

        if let Some(candidate) = acoustic_candidate {
            let relative_start = candidate
                .cut_start_sample
                .saturating_sub(snapshot_start_sample);
            return Ok((
                TranscriptionResult {
                    text: "zephyr".into(),
                    tokens: vec![TimedToken {
                        text: " zephyr".into(),
                        start: relative_start as f32 / SAMPLE_RATE as f32,
                        end: candidate.end_sample.saturating_sub(snapshot_start_sample) as f32
                            / SAMPLE_RATE as f32,
                    }],
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
            let next_decode_sample = next_decode_boundary(snapshot_start_sample);
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
            ConfirmationFeed::Duplicate => Ok(None),
            ConfirmationFeed::Stale => Err("KWS wake snapshot is stale".into()),
            ConfirmationFeed::Gap => {
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
                Ok(result)
            }
        }
    }

    fn feed_confirmation(
        &mut self,
        audio: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
        candidate: WakeCandidate,
    ) -> Result<Option<KeywordResult>, String> {
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
                .saturating_sub(CONFIRM_CONTEXT_SAMPLES)
                .max(snapshot_start_sample)
                .min(live_total_sample);
            let anchor_offset = anchor_sample - snapshot_start_sample;
            let mut stream = self.confirm_spotter.create_stream();
            let next_decode_sample = next_decode_boundary(anchor_sample);
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
            ConfirmationFeed::Duplicate => Ok(None),
            ConfirmationFeed::Stale => Err("KWS confirmation snapshot is stale".into()),
            ConfirmationFeed::Gap => {
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
                Ok(result)
            }
        }
    }

    fn destroy_confirmation_stream(&mut self) {
        // Dropping the stream destroys it.
        self.confirmation_stream = None;
    }

    fn destroy_wake_stream(&mut self) {
        // Dropping the stream destroys it.
        self.wake_stream = None;
    }

    fn invalidate_cascade(&mut self) {
        self.destroy_wake_stream();
        self.destroy_confirmation_stream();
        self.clock.clear();
    }

    pub fn reset_stream(&mut self) -> Result<(), String> {
        self.destroy_wake_stream();
        self.destroy_confirmation_stream();
        self.clock.clear();
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

    fn decode_fresh(
        &self,
        audio: &[f32],
        tail_samples: usize,
    ) -> Result<Option<KeywordResult>, String> {
        let mut stream = self.create_stream();
        for chunk in audio.chunks(KWS_FEED_CHUNK_SAMPLES) {
            stream.accept_audio(chunk)?;
            if let Some(result) = self.decode_ready_result(&mut stream)? {
                return Ok(Some(result));
            }
        }
        let silence = vec![0.0; tail_samples];
        for chunk in silence.chunks(KWS_FEED_CHUNK_SAMPLES) {
            stream.accept_audio(chunk)?;
            if let Some(result) = self.decode_ready_result(&mut stream)? {
                return Ok(Some(result));
            }
        }
        Ok(None)
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
                next_decode_sample = next_decode_sample.saturating_add(KWS_DECODE_STEP_SAMPLES);
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
            next_decode_sample = next_decode_sample.saturating_add(KWS_DECODE_STEP_SAMPLES);
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
                if !keyword_timestamps_are_plausible(&result.timestamps) {
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
    config.threads = NUM_THREADS as usize;
    config.max_active_paths = usize::try_from(paths)
        .map_err(|_| format!("KWS max_active_paths must be positive: {paths}"))?;
    config.num_trailing_blanks = NUM_TRAILING_BLANKS;
    config.keywords_score = KEYWORDS_SCORE;
    config.keywords_threshold = threshold;
    config.active_path_ttl_frames = tuning.active_path_ttl_frames;
    config.trailing_blank_reset = tuning.trailing_blank_reset;
    config.softmax_f64 = tuning.softmax_f64;
    config.onset = tuning.onset.then(rust_kws::OnsetConfig::default);
    config.threshold_tokens = tuning.threshold_tokens;
    config.joiner_batching = tuning.joiner_batching;
    config.merge_timestamps = tuning.merge_timestamps;
    config.portable_math = tuning.portable_math;
    config.static_batch = tuning.static_batch;
    config.decoder_cache = tuning.decoder_cache;
    Ok(config)
}

fn seconds_to_samples(seconds: f32) -> usize {
    (seconds.max(0.0) * SAMPLE_RATE as f32).round() as usize
}

/// Samples per feature frame (10 ms).
const FEATURE_SHIFT_SAMPLES: usize = SAMPLE_RATE as usize / 100;

/// An onset estimate on a stream whose clock starts at `origin`, in samples.
/// Integer arithmetic: feature frame `f` is exactly `160 f` samples.
fn onset_sample(origin: usize, estimate: rust_kws::OnsetEstimate) -> usize {
    origin.saturating_add(estimate.feature_frame.max(0) as usize * FEATURE_SHIFT_SAMPLES)
}

fn transcription_result_at_wake(
    result: KeywordResult,
    candidate: WakeCandidate,
    snapshot_start_sample: usize,
) -> TranscriptionResult {
    let text = command_text(&result.keyword).expect("validated keyword label");
    let start = candidate
        .cut_start_sample
        .saturating_sub(snapshot_start_sample) as f32
        / SAMPLE_RATE as f32;
    let end =
        candidate.end_sample.saturating_sub(snapshot_start_sample) as f32 / SAMPLE_RATE as f32;
    TranscriptionResult {
        text: text.to_string(),
        tokens: vec![TimedToken {
            text: " zephyr".to_string(),
            start,
            end: end.max(start),
        }],
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
fn keyword_timestamps_are_plausible(timestamps: &[f32]) -> bool {
    let (Some(start), Some(end)) = (timestamps.first(), timestamps.last()) else {
        return false;
    };
    start.is_finite()
        && end.is_finite()
        && *start >= 0.0
        && *end >= *start
        && *end - *start <= MAX_KEYWORD_SPAN_SECONDS
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
