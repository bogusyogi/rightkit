//! Cross-platform Sherpa-ONNX streaming keyword probe.
//!
//! macOS & Windows use identical model, graph, thresholds, CPU thread count,
//! active paths, VAD pre-roll & timestamp cut policy. Primary dictation ASR
//! remains independent.
//!
//! The cascade policy runs unchanged on either keyword-spotting runtime
//! ([`KwsBackend`]): the Sherpa-ONNX C API (the the app-patched
//! `libsherpa-onnx-c-api`) or `rightkit_asr::kws`, its Rust port configured to
//! the same decoder contract. Only the spotter/stream handles below
//! (`KwsSpotter`, `KwsStream`) know which one is loaded.
//!
//! The Rust runtime is compiled only with the default-off cargo feature `rust-kws`,
//! which needs the `rightkit-asr` `kws` module (unpublished; the workspace
//! `[patch.crates-io]` that supplies it lives on branch `levers/rust-kws`, see
//! `vendor/rightkit-asr/RIGHTKIT_PATCH.md`). Without the feature [`KwsBackend::Rust`]
//! still names the backend but cannot be loaded, `RIGHTKIT_KWS_BACKEND=rust` falls back to
//! [`KwsBackend::DEFAULT`] (Sherpa), and every Rust-only code path is absent.
//!
//! On the Rust backend every wake/confirm detection also carries onset
//! candidates (where the keyword began, not where its first token was
//! emitted), logged as one `kws_onset_candidates` event; [`KwsSwitches`] picks
//! which candidate becomes the cut start, plus the runtime's accuracy levers and
//! its speed levers (static encoder batch, decoder-row cache, one model shared by
//! the three spotters). Defaults reproduce today's cut (the emission timestamp) exactly.

#[cfg(feature = "rust-kws")]
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_char, c_float, c_int, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::Library;
#[cfg(feature = "rust-kws")]
use rightkit_asr::kws as rust_kws;
/// The Rust spotter's lever types, as [`KwsSwitches`] carries them.
#[cfg(feature = "rust-kws")]
pub use rightkit_asr::kws::{JoinerBatching, MergeTimestamps, ThresholdTokens};
use rightkit_asr::parakeet_core::{TimedToken, TranscriptionResult};
use serde::Deserialize;

/// Without `rust-kws` the lever types are local mirrors of `rightkit_asr::kws`'s, so
/// [`KwsSwitches`] and its `RIGHTKIT_KWS_*` parsing keep one shape in every build.
#[cfg(not(feature = "rust-kws"))]
mod rust_kws {
    /// Which token probabilities a keyword's threshold is compared against.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum ThresholdTokens {
        #[default]
        Upstream,
        MatchedKeyword,
    }

    /// How the joiner is batched across the beam.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum JoinerBatching {
        #[default]
        Beam,
        PerHypothesis,
    }

    /// Whose timestamps a merged beam path keeps when two paths reach the same tokens.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum MergeTimestamps {
        #[default]
        First,
        Likelier,
    }
}
#[cfg(not(feature = "rust-kws"))]
pub use rust_kws::{JoinerBatching, MergeTimestamps, ThresholdTokens};

/// The detail the Rust spotter attaches to a result (an uninhabited stand-in without
/// `rust-kws`, where no result ever carries one).
#[cfg(feature = "rust-kws")]
type KeywordDetection = rust_kws::KeywordDetection;
#[cfg(not(feature = "rust-kws"))]
type KeywordDetection = std::convert::Infallible;

/// Returned by `KwsRuntime::load` for [`KwsBackend::Rust`] in a build without `rust-kws`.
#[cfg(not(feature = "rust-kws"))]
const RUST_KWS_NOT_COMPILED: &str = "the Rust KWS backend is not compiled into this build; \
     enable cargo feature `rust-kws` (it needs the rightkit-asr `kws` patch, see \
     vendor/rightkit-asr/RIGHTKIT_PATCH.md)";

const SAMPLE_RATE: c_int = 16_000;
const FEATURE_DIM: c_int = 80;
pub const NUM_THREADS: c_int = 1;
pub const MAX_ACTIVE_PATHS: c_int = 8;
pub const NUM_TRAILING_BLANKS: c_int = 0;
const KEYWORDS_SCORE: c_float = 1.0;
const KEYWORDS_THRESHOLD: c_float = 0.25;
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
/// production corpus. Reject paths spanning more than 2s so Sherpa cannot
/// combine an old wake-like prefix with a much later action word.
const MAX_KEYWORD_SPAN_SECONDS: f32 = 2.0;
/// Rust-backend twin of the the app native Sherpa patch
/// (`rightkit-dictation-engine/native/sherpa/patches/0001-expire-active-keyword-hypotheses.patch`,
/// `kMaxKeywordActiveFrames`): drop a beam path whose active keyword began more than
/// 38 encoder frames (1.52 s at 40 ms) ago, so an old partial context path cannot
/// survive into a later command attempt. `resources/kws/manifest.json` records the
/// same value as `native_patch.active_keyword_ttl_frames`.
pub const ACTIVE_KEYWORD_TTL_FRAMES: i32 = 38;
/// The same patch removed upstream Sherpa's silent stream reset after 1.5 s of
/// trailing blanks (it restarts timestamps while audio keeps flowing). the app's
/// VAD/reset generation is the sole reset owner, so the Rust backend keeps it off.
const TRAILING_BLANK_RESET: bool = false;

const ENCODER: &str = "encoder-epoch-13-avg-2-chunk-16-left-64.int8.onnx";
const DECODER: &str = "decoder-epoch-13-avg-2-chunk-16-left-64.onnx";
const JOINER: &str = "joiner-epoch-13-avg-2-chunk-16-left-64.int8.onnx";

#[cfg(target_os = "macos")]
const SHERPA_LIBRARY: &str = "libsherpa-onnx-c-api.dylib";
#[cfg(target_os = "windows")]
const SHERPA_LIBRARY: &str = "sherpa-onnx-c-api.dll";
/// No other platform bundles a Sherpa runtime; tests name one with
/// `RIGHTKIT_SHERPA_LIBRARY_PATH` (test-mode only).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const SHERPA_LIBRARY: &str = "libsherpa-onnx-c-api.so";

/// Which keyword-spotting runtime executes the cascade. Every policy decision in
/// this file is shared; only the nine Sherpa C API touchpoints are dispatched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KwsBackend {
    /// Sherpa-ONNX C API through `libloading` (the shipped macOS/Windows runtime).
    Sherpa,
    /// `rightkit_asr::kws`, the Rust port of the same keyword spotter.
    Rust,
}

impl KwsBackend {
    /// Sherpa stays the shipped runtime on macOS and Windows until the Rust
    /// backend passes on-device qualification; no other platform bundles Sherpa.
    /// Without the `rust-kws` feature the Rust backend is not compiled, so Sherpa is
    /// the default on every platform.
    #[cfg(any(target_os = "macos", target_os = "windows", not(feature = "rust-kws")))]
    pub const DEFAULT: Self = Self::Sherpa;
    #[cfg(all(
        not(any(target_os = "macos", target_os = "windows")),
        feature = "rust-kws"
    ))]
    pub const DEFAULT: Self = Self::Rust;

    /// [`Self::DEFAULT`], unless the test/dev-only switch `RIGHTKIT_KWS_BACKEND`
    /// (`rust` | `sherpa`) names another. Release builds never read it. `rust` in a
    /// build without the `rust-kws` feature is ignored with a warning (Sherpa runs).
    pub fn from_env() -> Self {
        let Ok(value) = rightkit_dictation_core::dev_var!("RIGHTKIT_KWS_BACKEND") else {
            return Self::DEFAULT;
        };
        Self::resolve(&value)
    }

    /// The backend `value` (an `RIGHTKIT_KWS_BACKEND` string) selects in this build.
    fn resolve(value: &str) -> Self {
        match Self::parse(value) {
            Some(Self::Rust) if !cfg!(feature = "rust-kws") => {
                tracing::warn!(
                    "ignoring RIGHTKIT_KWS_BACKEND={value:?}: the Rust KWS backend needs cargo feature rust-kws; using {:?}",
                    Self::DEFAULT
                );
                Self::DEFAULT
            }
            Some(backend) => backend,
            None => {
                tracing::warn!(
                    "ignoring invalid RIGHTKIT_KWS_BACKEND={value:?}; expected rust or sherpa"
                );
                Self::DEFAULT
            }
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rust" => Some(Self::Rust),
            "sherpa" => Some(Self::Sherpa),
            _ => None,
        }
    }
}

#[repr(C)]
struct OnlineTransducerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
struct OnlineParaformerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
}

#[repr(C)]
struct OnlineZipformer2CtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
struct OnlineNemoCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
struct OnlineToneCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
struct OnlineModelConfig {
    transducer: OnlineTransducerModelConfig,
    paraformer: OnlineParaformerModelConfig,
    zipformer2_ctc: OnlineZipformer2CtcModelConfig,
    tokens: *const c_char,
    num_threads: c_int,
    provider: *const c_char,
    debug: c_int,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    tokens_buf: *const c_char,
    tokens_buf_size: c_int,
    nemo_ctc: OnlineNemoCtcModelConfig,
    t_one_ctc: OnlineToneCtcModelConfig,
}

#[repr(C)]
struct FeatureConfig {
    sample_rate: c_int,
    feature_dim: c_int,
}

#[repr(C)]
struct KeywordSpotterConfig {
    feat_config: FeatureConfig,
    model_config: OnlineModelConfig,
    max_active_paths: c_int,
    num_trailing_blanks: c_int,
    keywords_score: c_float,
    keywords_threshold: c_float,
    keywords_file: *const c_char,
    keywords_buf: *const c_char,
    keywords_buf_size: c_int,
}

enum KeywordSpotter {}
enum OnlineStream {}

type CreateSpotter = unsafe extern "C" fn(*const KeywordSpotterConfig) -> *const KeywordSpotter;
type DestroySpotter = unsafe extern "C" fn(*const KeywordSpotter);
type CreateStream = unsafe extern "C" fn(*const KeywordSpotter) -> *const OnlineStream;
type DestroyStream = unsafe extern "C" fn(*const OnlineStream);
type AcceptWaveform = unsafe extern "C" fn(*const OnlineStream, c_int, *const c_float, c_int);
type IsReady = unsafe extern "C" fn(*const KeywordSpotter, *const OnlineStream) -> c_int;
type Decode = unsafe extern "C" fn(*const KeywordSpotter, *const OnlineStream);
type GetResultJson =
    unsafe extern "C" fn(*const KeywordSpotter, *const OnlineStream) -> *const c_char;
type FreeResultJson = unsafe extern "C" fn(*const c_char);

struct SherpaApi {
    create_spotter: CreateSpotter,
    destroy_spotter: DestroySpotter,
    create_stream: CreateStream,
    destroy_stream: DestroyStream,
    accept_waveform: AcceptWaveform,
    is_ready: IsReady,
    decode: Decode,
    get_result_json: GetResultJson,
    free_result_json: FreeResultJson,
    _library: Library,
    #[cfg(target_os = "windows")]
    _ort_library: Library,
}

impl SherpaApi {
    unsafe fn load(path: &Path) -> Result<Self, String> {
        // Sherpa imports ORT by DLL name. Preload its bundled sibling before
        // loading Sherpa so Windows cannot bind the System32 WinML ORT 1.17
        // instead of the packaged API-24 runtime. A missing dependency now
        // fails here with a path, before any spotter/VAD state is mutated.
        #[cfg(target_os = "windows")]
        let ort_library =
            unsafe { Library::new(path.with_file_name("onnxruntime.dll")) }.map_err(|error| {
                format!(
                    "preload bundled ONNX Runtime beside {}: {error}",
                    path.display()
                )
            })?;
        let library = unsafe { Library::new(path) }
            .map_err(|error| format!("load Sherpa runtime {}: {error}", path.display()))?;
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {
                *unsafe { library.get::<$ty>(concat!($name, "\0").as_bytes()) }
                    .map_err(|error| format!("Sherpa runtime symbol {}: {error}", $name))?
            };
        }
        Ok(Self {
            create_spotter: symbol!("SherpaOnnxCreateKeywordSpotter", CreateSpotter),
            destroy_spotter: symbol!("SherpaOnnxDestroyKeywordSpotter", DestroySpotter),
            create_stream: symbol!("SherpaOnnxCreateKeywordStream", CreateStream),
            destroy_stream: symbol!("SherpaOnnxDestroyOnlineStream", DestroyStream),
            accept_waveform: symbol!("SherpaOnnxOnlineStreamAcceptWaveform", AcceptWaveform),
            is_ready: symbol!("SherpaOnnxIsKeywordStreamReady", IsReady),
            decode: symbol!("SherpaOnnxDecodeKeywordStream", Decode),
            get_result_json: symbol!("SherpaOnnxGetKeywordResultAsJson", GetResultJson),
            free_result_json: symbol!("SherpaOnnxFreeKeywordResultJson", FreeResultJson),
            _library: library,
            #[cfg(target_os = "windows")]
            _ort_library: ort_library,
        })
    }
}

#[derive(Deserialize)]
struct KeywordResult {
    #[serde(default)]
    keyword: String,
    #[serde(default)]
    timestamps: Vec<f32>,
    /// Rust backend only: score, threshold and onset candidates of the detection.
    #[serde(skip)]
    #[cfg_attr(not(feature = "rust-kws"), allow(dead_code))]
    detection: Option<Box<KeywordDetection>>,
}

/// Rust-backend decoder knobs outside the Sherpa C config struct. Production
/// uses [`RustKwsTuning::PRODUCTION`] with any [`KwsSwitches`] applied; tests
/// also run the unpatched upstream behaviour to compare against a stock Sherpa
/// library.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(feature = "rust-kws"), allow(dead_code))] // read only by the Rust spotter config
struct RustKwsTuning {
    active_path_ttl_frames: Option<i32>,
    trailing_blank_reset: bool,
    /// The GCC (Linux) Sherpa build evaluates the joiner softmax in f64;
    /// the shipped macOS/Windows builds use f32.
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
    /// The shipped Sherpa config plus the the app native patch, with onset
    /// estimation on (logging only: the cut stays on the emission timestamp).
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
    /// The first token's emission timestamp (today's cut; the only one the
    /// Sherpa backend has).
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
    #[cfg(feature = "rust-kws")]
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
    /// merged beam path keeps. Default `first` (Sherpa, which can keep a stale
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

    #[cfg(feature = "rust-kws")]
    fn label_threshold_tokens(&self) -> &'static str {
        match self.threshold_tokens {
            rust_kws::ThresholdTokens::Upstream => "upstream",
            rust_kws::ThresholdTokens::MatchedKeyword => "matched",
        }
    }

    #[cfg(feature = "rust-kws")]
    fn label_joiner_batching(&self) -> &'static str {
        match self.joiner_batching {
            rust_kws::JoinerBatching::Beam => "beam",
            rust_kws::JoinerBatching::PerHypothesis => "per_hypothesis",
        }
    }

    #[cfg(feature = "rust-kws")]
    fn label_merge_timestamps(&self) -> &'static str {
        match self.merge_timestamps {
            rust_kws::MergeTimestamps::First => "first",
            rust_kws::MergeTimestamps::Likelier => "likelier",
        }
    }
}

/// One Rust-backend detection's onset candidates on the outer sample clock,
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

/// Reports kept for [`SherpaKws::take_onset_reports`]; older ones are dropped.
#[cfg(feature = "rust-kws")]
const ONSET_REPORTS_KEPT: usize = 16;

/// The loaded keyword-spotting runtime; it creates every spotter of one cascade.
enum KwsRuntime {
    Sherpa(Arc<SherpaApi>),
    /// The spotters' ONNX sessions run on the process-wide ONNX Runtime that
    /// `rightkit_onnx` bound: the engine binds it at startup on macOS/Windows
    /// (`rightkit-dictation-runtime` `entry.rs` `configure_ort_dylib`), and tests bind it
    /// themselves. Nothing here loads or binds a runtime. With
    /// [`RustKwsTuning::shared_model`] every spotter runs on `model`.
    #[cfg(feature = "rust-kws")]
    Rust {
        tuning: RustKwsTuning,
        model: Option<Arc<rust_kws::KwsModel>>,
    },
}

impl KwsRuntime {
    fn load(backend: KwsBackend, tuning: RustKwsTuning, model_dir: &Path) -> Result<Self, String> {
        match backend {
            KwsBackend::Sherpa => {
                let library_path = resolve_library_path().ok_or_else(missing_sherpa_runtime)?;
                let api = unsafe { SherpaApi::load(&library_path) }?;
                Ok(Self::Sherpa(Arc::new(api)))
            }
            #[cfg(feature = "rust-kws")]
            KwsBackend::Rust => {
                let model = if tuning.shared_model {
                    // Any keyword file: the model reads only the session fields.
                    let config = rust_spotter_config(
                        tuning,
                        model_dir,
                        "keywords-wake.txt",
                        KEYWORDS_THRESHOLD,
                        MAX_ACTIVE_PATHS,
                    )?;
                    Some(rust_kws::KwsModel::load(&config).map_err(|error| {
                        format!("Rust keyword spotter shared model load failed: {error}")
                    })?)
                } else {
                    None
                };
                Ok(Self::Rust { tuning, model })
            }
            #[cfg(not(feature = "rust-kws"))]
            KwsBackend::Rust => {
                let _ = (tuning, model_dir);
                Err(RUST_KWS_NOT_COMPILED.into())
            }
        }
    }

    fn backend(&self) -> KwsBackend {
        match self {
            Self::Sherpa(_) => KwsBackend::Sherpa,
            #[cfg(feature = "rust-kws")]
            Self::Rust { .. } => KwsBackend::Rust,
        }
    }

    fn create_spotter(
        &self,
        model_dir: &Path,
        keywords_file: &str,
        threshold: f32,
        paths: c_int,
    ) -> Result<KwsSpotter, String> {
        match self {
            Self::Sherpa(api) => {
                create_spotter_with_paths(api, model_dir, keywords_file, threshold, paths).map(
                    |ptr| KwsSpotter::Sherpa {
                        ptr,
                        api: Arc::clone(api),
                    },
                )
            }
            #[cfg(feature = "rust-kws")]
            Self::Rust { tuning, model } => create_rust_spotter(
                *tuning,
                model.as_ref(),
                model_dir,
                keywords_file,
                threshold,
                paths,
            )
            .map(|spotter| KwsSpotter::Rust(RefCell::new(spotter))),
        }
    }
}

/// One keyword spotter (model sessions + keyword graph) on the loaded backend.
/// Dropping it destroys the native spotter.
enum KwsSpotter {
    Sherpa {
        ptr: *const KeywordSpotter,
        api: Arc<SherpaApi>,
    },
    /// Rust `decode` takes `&mut`; a spotter is only touched by its one ASR
    /// worker thread, one call at a time, so the borrow never overlaps.
    #[cfg(feature = "rust-kws")]
    Rust(RefCell<rust_kws::KeywordSpotter>),
}

/// One audio stream of a [`KwsSpotter`]. Dropping it destroys the native stream.
enum KwsStream {
    Sherpa {
        ptr: *const OnlineStream,
        api: Arc<SherpaApi>,
    },
    #[cfg(feature = "rust-kws")]
    Rust(rust_kws::KeywordStream),
}

impl Drop for KwsSpotter {
    #[cfg_attr(not(feature = "rust-kws"), allow(irrefutable_let_patterns))]
    fn drop(&mut self) {
        if let Self::Sherpa { ptr, api } = self {
            if !ptr.is_null() {
                unsafe { (api.destroy_spotter)(*ptr) };
            }
        }
    }
}

impl Drop for KwsStream {
    #[cfg_attr(not(feature = "rust-kws"), allow(irrefutable_let_patterns))]
    fn drop(&mut self) {
        if let Self::Sherpa { ptr, api } = self {
            if !ptr.is_null() {
                unsafe { (api.destroy_stream)(*ptr) };
            }
        }
    }
}

/// Unreachable by construction: every stream is created by its own spotter.
const KWS_BACKEND_MISMATCH: &str = "KWS stream and spotter belong to different backends";

/// Optional third spotter: the hey-prefixed always-on wake word. Kept OUT of
/// [`validate_model_dir`] on purpose — a payload without this file must still
/// load the shipped tail cascade, so the spotter is simply absent and
/// [`SherpaKws::hey_wake_available`] reports false.
const HEY_WAKE_KEYWORDS_FILE: &str = "keywords-hey-wake.txt";
const HEY_WAKE_KEYWORD: &str = "HEY_ZEPHYR";

pub struct SherpaKws {
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
    /// Live switches (Rust backend; the Sherpa backend honours only the defaults).
    switches: KwsSwitches,
    #[cfg_attr(not(feature = "rust-kws"), allow(dead_code))]
    max_active_paths: c_int,
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
        // Sherpa may report the same acoustic occurrence more than once as
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

// Probe is created, mutated & destroyed by one ASR worker thread; every
// spotter, stream and `Arc<SherpaApi>` clone lives inside it.
unsafe impl Send for SherpaKws {}

impl SherpaKws {
    /// Load the cascade on [`KwsBackend::from_env`] with [`KwsSwitches::from_env`].
    pub fn load(model_dir: &Path) -> Result<Self, String> {
        Self::load_with_backend(model_dir, KwsBackend::from_env())
    }

    /// Load the cascade on an explicit backend (qualification and A/B replay),
    /// with [`KwsSwitches::from_env`].
    pub fn load_with_backend(model_dir: &Path, backend: KwsBackend) -> Result<Self, String> {
        Self::load_with_switches(model_dir, backend, KwsSwitches::from_env())
    }

    /// Load the cascade on an explicit backend with explicit switches.
    pub fn load_with_switches(
        model_dir: &Path,
        backend: KwsBackend,
        switches: KwsSwitches,
    ) -> Result<Self, String> {
        Self::load_with_paths_and_switches(
            model_dir,
            MAX_ACTIVE_PATHS,
            backend,
            switches.tuning(RustKwsTuning::PRODUCTION),
            switches,
        )
    }

    /// Bounded replay only; production keeps MAX_ACTIVE_PATHS.
    #[doc(hidden)]
    pub fn load_for_replay_with_paths(model_dir: &Path, paths: i32) -> Result<Self, String> {
        Self::load_for_replay_with_paths_and_backend(model_dir, paths, KwsBackend::from_env())
    }

    /// Bounded replay on an explicit backend; production keeps MAX_ACTIVE_PATHS.
    #[doc(hidden)]
    pub fn load_for_replay_with_paths_and_backend(
        model_dir: &Path,
        paths: i32,
        backend: KwsBackend,
    ) -> Result<Self, String> {
        if !matches!(paths, 4 | 8 | 16) {
            return Err("replay paths must be 4, 8 or 16".into());
        }
        let switches = KwsSwitches::from_env();
        Self::load_with_paths_and_switches(
            model_dir,
            paths,
            backend,
            switches.tuning(RustKwsTuning::PRODUCTION),
            switches,
        )
    }

    /// The runtime this cascade decodes on.
    pub fn backend(&self) -> KwsBackend {
        self.runtime.backend()
    }

    /// The default switches over an explicit tuning (A/B against a Sherpa build).
    #[cfg(test)]
    #[cfg_attr(not(feature = "rust-kws"), allow(dead_code))]
    fn load_with_paths(
        model_dir: &Path,
        paths: c_int,
        backend: KwsBackend,
        tuning: RustKwsTuning,
    ) -> Result<Self, String> {
        let switches = KwsSwitches {
            onset: tuning.onset.then_some(KwsOnsetMethod::Emission),
            static_batch: tuning.static_batch,
            decoder_cache: tuning.decoder_cache,
            shared_model: tuning.shared_model,
            ..KwsSwitches::default()
        };
        Self::load_with_paths_and_switches(model_dir, paths, backend, tuning, switches)
    }

    fn load_with_paths_and_switches(
        model_dir: &Path,
        paths: c_int,
        backend: KwsBackend,
        tuning: RustKwsTuning,
        switches: KwsSwitches,
    ) -> Result<Self, String> {
        validate_model_dir(model_dir)?;
        let switches = match backend {
            KwsBackend::Rust => switches,
            KwsBackend::Sherpa => {
                if switches != KwsSwitches::default() {
                    tracing::warn!(
                        ?switches,
                        "KWS switches need the Rust backend; the Sherpa backend runs its defaults"
                    );
                }
                KwsSwitches::default()
            }
        };
        let runtime = KwsRuntime::load(backend, tuning, model_dir)?;

        let wake_spotter =
            runtime.create_spotter(model_dir, "keywords-wake.txt", KEYWORDS_THRESHOLD, paths)?;
        // On failure the wake spotter drops, which destroys it.
        let confirm_spotter =
            runtime.create_spotter(model_dir, "keywords-confirm.txt", KEYWORDS_THRESHOLD, paths)?;
        let hey_wake_available = model_dir.join(HEY_WAKE_KEYWORDS_FILE).is_file();
        tracing::info!(
            "Sherpa KWS cascade loaded: model={} cpu_threads={} max_active_paths={} trailing_blanks={} backend={:?} switches={:?}",
            model_dir.display(),
            NUM_THREADS,
            paths,
            NUM_TRAILING_BLANKS,
            backend,
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

    /// Onset reports of the most recent Rust-backend detections (at most 16;
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
        #[cfg(feature = "rust-kws")]
        let candidate = match method {
            KwsOnsetMethod::Emission => None,
            _ => result
                .detection
                .as_ref()
                .and_then(|detection| detection.onset.as_ref())
                .and_then(|onset| method.candidate(onset))
                .map(|estimate| onset_sample(origin, estimate)),
        };
        // Sherpa results carry no onset detail: every method falls back to the emission.
        #[cfg(not(feature = "rust-kws"))]
        let candidate: Option<usize> = {
            let _ = (method, origin, result);
            None
        };
        let (base, source) = match candidate {
            Some(sample) => (sample, method.as_str()),
            None => (emission_start, KwsOnsetMethod::Emission.as_str()),
        };
        let shift = i64::from(self.switches.onset_shift_ms) * i64::from(SAMPLE_RATE) / 1_000;
        ((base as i64 + shift).max(0) as usize, source)
    }

    /// Log one Rust-backend detection's onset candidates (`kws_onset_candidates`)
    /// and keep the report. A Sherpa result (no detection detail) logs nothing.
    #[cfg(feature = "rust-kws")]
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

    /// Without `rust-kws` no result carries onset detail, so there is nothing to report.
    #[cfg(not(feature = "rust-kws"))]
    #[allow(clippy::too_many_arguments)] // same call sites as the Rust-backend version
    fn report_onset(
        &mut self,
        _stage: &'static str,
        _result: &KeywordResult,
        _origin: Option<usize>,
        _live_sample: Option<usize>,
        _chosen: Option<(usize, &'static str)>,
        _cut_sample: Option<usize>,
        _latched: bool,
    ) {
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
            .and_then(KwsSpotter::create_stream)
        else {
            return Err("Sherpa idle wake stream creation failed".into());
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
            return Err("Sherpa full-command snapshot cursor mismatch".into());
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
            return Err("Sherpa full-command stream lost contiguous audio".into());
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
            return Err("Sherpa KWS cascade snapshot cursor mismatch".into());
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
                return Err("Sherpa KWS snapshot cursor overflow".into());
            }
        };
        if snapshot_end_sample != live_total_sample {
            self.invalidate_cascade();
            return Err("Sherpa KWS cascade snapshot cursor mismatch".into());
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
            let Some(mut stream) = self.wake_spotter.create_stream() else {
                return Err("Sherpa wake stream creation failed".into());
            };
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
            ConfirmationFeed::Stale => Err("Sherpa wake snapshot is stale".into()),
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
            let Some(mut stream) = self.confirm_spotter.create_stream() else {
                return Err("Sherpa confirmation stream creation failed".into());
            };
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
            ConfirmationFeed::Stale => Err("Sherpa confirmation snapshot is stale".into()),
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

impl Drop for SherpaKws {
    fn drop(&mut self) {
        // Streams before spotters; the spotters (wake, confirm, hey-wake) then
        // drop in field order and destroy themselves.
        self.stop_idle_wake();
        self.destroy_wake_stream();
        self.destroy_confirmation_stream();
    }
}

impl KwsSpotter {
    /// `None` when the native runtime returns no stream; each caller owns its
    /// error message.
    fn create_stream(&self) -> Option<KwsStream> {
        match self {
            Self::Sherpa { ptr, api } => {
                let stream = unsafe { (api.create_stream)(*ptr) };
                (!stream.is_null()).then(|| KwsStream::Sherpa {
                    ptr: stream,
                    api: Arc::clone(api),
                })
            }
            #[cfg(feature = "rust-kws")]
            Self::Rust(spotter) => Some(KwsStream::Rust(spotter.borrow().create_stream())),
        }
    }

    #[cfg_attr(not(feature = "rust-kws"), allow(unreachable_patterns))]
    fn is_ready(&self, stream: &KwsStream) -> Result<bool, String> {
        match (self, stream) {
            (Self::Sherpa { ptr, api }, KwsStream::Sherpa { ptr: stream, .. }) => {
                Ok(unsafe { (api.is_ready)(*ptr, *stream) } != 0)
            }
            #[cfg(feature = "rust-kws")]
            (Self::Rust(spotter), KwsStream::Rust(stream)) => Ok(spotter.borrow().is_ready(stream)),
            _ => Err(KWS_BACKEND_MISMATCH.into()),
        }
    }

    #[cfg_attr(not(feature = "rust-kws"), allow(unreachable_patterns))]
    fn decode(&self, stream: &mut KwsStream) -> Result<(), String> {
        match (self, stream) {
            (Self::Sherpa { ptr, api }, KwsStream::Sherpa { ptr: stream, .. }) => {
                unsafe { (api.decode)(*ptr, *stream) };
                Ok(())
            }
            #[cfg(feature = "rust-kws")]
            (Self::Rust(spotter), KwsStream::Rust(stream)) => spotter
                .borrow_mut()
                .decode(stream)
                .map_err(|error| format!("Rust KWS decode: {error}")),
            _ => Err(KWS_BACKEND_MISMATCH.into()),
        }
    }

    /// The newest detection, reported once (Sherpa `GetKeywordResult`); the
    /// keyword is empty when nothing new fired.
    #[cfg_attr(not(feature = "rust-kws"), allow(unreachable_patterns))]
    fn take_result(&self, stream: &mut KwsStream) -> Result<KeywordResult, String> {
        match (self, stream) {
            (Self::Sherpa { ptr, api }, KwsStream::Sherpa { ptr: stream, .. }) => {
                let json_ptr = unsafe { (api.get_result_json)(*ptr, *stream) };
                if json_ptr.is_null() {
                    return Err("Sherpa KWS returned null result JSON".into());
                }
                let json = unsafe { CStr::from_ptr(json_ptr) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { (api.free_result_json)(json_ptr) };
                serde_json::from_str(&json)
                    .map_err(|error| format!("parse Sherpa KWS result: {error}"))
            }
            #[cfg(feature = "rust-kws")]
            (Self::Rust(spotter), KwsStream::Rust(stream)) => {
                // `detection` shares `result`'s once-only gate and reports the same
                // hits, plus score, threshold and onset candidates.
                let Some(detection) = spotter.borrow().detection(stream) else {
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
                        .map(sherpa_json_timestamp)
                        .collect(),
                    detection: Some(Box::new(detection)),
                })
            }
            _ => Err(KWS_BACKEND_MISMATCH.into()),
        }
    }

    fn decode_fresh(
        &self,
        audio: &[f32],
        tail_samples: usize,
    ) -> Result<Option<KeywordResult>, String> {
        let Some(mut stream) = self.create_stream() else {
            return Err("Sherpa keyword stream creation failed".into());
        };
        // The stream drops (and is destroyed) on every return path.
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
                "Sherpa KWS buffered cursor mismatch: expected={expected_start} got={audio_start_sample}"
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
        while self.is_ready(stream)? {
            self.decode(stream)?;
            decode_steps += 1;
            if decode_steps > 4_096 {
                return Err("Sherpa KWS decode readiness did not converge".into());
            }
            if let Some(result) = self.current_result(stream)? {
                if !keyword_timestamps_are_plausible(&result.timestamps) {
                    let start = result.timestamps.first().copied();
                    let end = result.timestamps.last().copied();
                    tracing::warn!(
                        keyword = result.keyword,
                        ?start,
                        ?end,
                        "Sherpa KWS rejected non-contiguous keyword path"
                    );
                    return Err(format!(
                        "Sherpa KWS keyword timing is non-contiguous: start={start:?} end={end:?}"
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
        let count = c_int::try_from(audio.len())
            .map_err(|_| "Sherpa KWS audio chunk exceeds i32".to_string())?;
        match self {
            Self::Sherpa { ptr, api } => unsafe {
                (api.accept_waveform)(*ptr, SAMPLE_RATE, audio.as_ptr(), count);
            },
            #[cfg(feature = "rust-kws")]
            Self::Rust(stream) => stream
                .accept_waveform(SAMPLE_RATE as u32, audio)
                .map_err(|error| format!("Rust KWS accept waveform: {error}"))?,
        }
        Ok(())
    }
}

/// Sherpa's result JSON prints each timestamp with `%.2f` and the Sherpa path
/// parses that text back. Rust timestamps take the same rounding so both
/// backends hand the policy identical values.
#[cfg(feature = "rust-kws")]
fn sherpa_json_timestamp(seconds: f32) -> f32 {
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

/// The Rust spotter on the same decoder contract as [`create_spotter_with_paths`],
/// on `model` when the cascade shares one.
#[cfg(feature = "rust-kws")]
fn create_rust_spotter(
    tuning: RustKwsTuning,
    model: Option<&Arc<rust_kws::KwsModel>>,
    model_dir: &Path,
    keywords_file: &str,
    threshold: f32,
    paths: c_int,
) -> Result<rust_kws::KeywordSpotter, String> {
    let config = rust_spotter_config(tuning, model_dir, keywords_file, threshold, paths)?;
    match model {
        Some(model) => rust_kws::KeywordSpotter::with_model(&config, Arc::clone(model)),
        None => rust_kws::KeywordSpotter::new(&config),
    }
    .map_err(|error| format!("Rust keyword spotter creation failed for {keywords_file}: {error}"))
}

#[cfg(feature = "rust-kws")]
fn rust_spotter_config(
    tuning: RustKwsTuning,
    model_dir: &Path,
    keywords_file: &str,
    threshold: f32,
    paths: c_int,
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
        .map_err(|_| format!("Rust KWS max_active_paths must be positive: {paths}"))?;
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

fn create_spotter_with_paths(
    api: &SherpaApi,
    model_dir: &Path,
    keywords_file: &str,
    threshold: f32,
    paths: c_int,
) -> Result<*const KeywordSpotter, String> {
    let encoder = path_cstring(&model_dir.join(ENCODER))?;
    let decoder = path_cstring(&model_dir.join(DECODER))?;
    let joiner = path_cstring(&model_dir.join(JOINER))?;
    let tokens = path_cstring(&model_dir.join("tokens.txt"))?;
    let keywords = path_cstring(&model_dir.join(keywords_file))?;
    let cpu = CString::new("cpu").expect("static CPU provider");
    let mut config: KeywordSpotterConfig = unsafe { std::mem::zeroed() };
    config.feat_config.sample_rate = SAMPLE_RATE;
    config.feat_config.feature_dim = FEATURE_DIM;
    config.model_config.transducer.encoder = encoder.as_ptr();
    config.model_config.transducer.decoder = decoder.as_ptr();
    config.model_config.transducer.joiner = joiner.as_ptr();
    config.model_config.tokens = tokens.as_ptr();
    config.model_config.num_threads = NUM_THREADS;
    config.model_config.provider = cpu.as_ptr();
    config.max_active_paths = paths;
    config.num_trailing_blanks = NUM_TRAILING_BLANKS;
    config.keywords_score = KEYWORDS_SCORE;
    config.keywords_threshold = threshold;
    config.keywords_file = keywords.as_ptr();
    let spotter = unsafe { (api.create_spotter)(&config) };
    if spotter.is_null() {
        Err(format!(
            "Sherpa keyword spotter creation failed for {keywords_file}"
        ))
    } else {
        Ok(spotter)
    }
}

fn seconds_to_samples(seconds: f32) -> usize {
    (seconds.max(0.0) * SAMPLE_RATE as f32).round() as usize
}

/// Samples per feature frame (10 ms).
#[cfg(feature = "rust-kws")]
const FEATURE_SHIFT_SAMPLES: usize = SAMPLE_RATE as usize / 100;

/// An onset estimate on a stream whose clock starts at `origin`, in samples.
/// Integer arithmetic: feature frame `f` is exactly `160 f` samples.
#[cfg(feature = "rust-kws")]
fn onset_sample(origin: usize, estimate: rust_kws::OnsetEstimate) -> usize {
    origin.saturating_add(estimate.feature_frame.max(0) as usize * FEATURE_SHIFT_SAMPLES)
}

#[cfg(test)]
fn transcription_result(result: KeywordResult, offset_samples: usize) -> TranscriptionResult {
    let text = command_text(&result.keyword).expect("validated Sherpa keyword label");
    let offset = offset_samples as f32 / SAMPLE_RATE as f32;
    let start = offset + result.timestamps.first().copied().unwrap_or(0.0);
    let end = offset + result.timestamps.last().copied().unwrap_or(start - offset) + 0.08;
    TranscriptionResult {
        text: text.to_string(),
        tokens: vec![TimedToken {
            text: " zephyr".to_string(),
            start,
            end: end.max(start),
        }],
    }
}

fn transcription_result_at_wake(
    result: KeywordResult,
    candidate: WakeCandidate,
    snapshot_start_sample: usize,
) -> TranscriptionResult {
    let text = command_text(&result.keyword).expect("validated Sherpa keyword label");
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
/// stream identity are enforced by the outer worker because Sherpa's timestamp
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
            return Err(format!(
                "bundled Sherpa KWS resource missing {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn path_cstring(path: &Path) -> Result<CString, String> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| format!("Sherpa resource path contains NUL: {}", path.display()))
}

fn missing_sherpa_runtime() -> String {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        format!("bundled Sherpa runtime {SHERPA_LIBRARY} missing beside ONNX Runtime")
    } else {
        format!(
            "no Sherpa runtime is bundled on this platform; name {SHERPA_LIBRARY} with RIGHTKIT_SHERPA_LIBRARY_PATH (test-mode) or use the Rust KWS backend"
        )
    }
}

fn resolve_library_path() -> Option<PathBuf> {
    if let Some(path) = rightkit_dictation_core::dev_var_os!("RIGHTKIT_SHERPA_LIBRARY_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    bundled_library_path()
}

/// Only macOS and Windows bundle a Sherpa runtime.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn bundled_library_path() -> Option<PathBuf> {
    None
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn bundled_library_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    // The runtime directory the engine's ONNX binding resolved at startup (rightkit-onnx
    // records it; this is not an environment read).
    if let Some(path) = rightkit_onnx::configured_runtime() {
        if let Some(parent) = path.parent() {
            candidates.push(parent.join(SHERPA_LIBRARY));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(SHERPA_LIBRARY));
            candidates.push(dir.join("runtime").join(SHERPA_LIBRARY));
            candidates.push(dir.join("resources/runtime").join(SHERPA_LIBRARY));
            #[cfg(target_os = "macos")]
            if let Some(contents) = dir.parent() {
                candidates.push(contents.join("Resources/runtime").join(SHERPA_LIBRARY));
                candidates.push(
                    contents
                        .join("Resources/resources/runtime")
                        .join(SHERPA_LIBRARY),
                );
            }
        }
    }
    // Unit-test binaries and test-mode builds: the staged copy, then the committed source the
    // bundle copies it from. `cfg(test)` keeps a plain `cargo test` of this crate from failing to
    // find the bundled runtime; release builds never compile either form.
    #[cfg(any(test, feature = "test-mode"))]
    for dir in [
        "../src-tauri/resources/runtime",
        "../src-tauri/native-sources",
    ] {
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(dir)
                .join(SHERPA_LIBRARY),
        );
    }
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(test)]
#[path = "sherpa_kws_tests.rs"]
mod tests;

#[cfg(all(test, feature = "rust-kws"))]
#[path = "sherpa_kws_rust_tests.rs"]
mod rust_tests;
