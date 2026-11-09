//! Streaming keyword spotting on a Zipformer2 transducer.
//!
//! A Rust port of the Sherpa-ONNX 1.13.4 keyword spotter (`KeywordSpotter`,
//! `OnlineStream`, `TransducerKeywordDecoder`, `ContextGraph`, the streaming Zipformer2
//! model wrapper) and of the kaldi-native-fbank features it uses, running the same
//! encoder/decoder/joiner ONNX files on [`rightkit_onnx`]. One [`KeywordSpotter`] holds
//! the three sessions (a [`KwsModel`], which several spotters may share) and a keyword
//! graph; each [`KeywordStream`] holds one audio stream's features, encoder cache and beam.
//!
//! Two behaviours differ from upstream and are configured, not patched:
//! - [`KeywordSpotterConfig::active_path_ttl_frames`] drops a beam path whose current
//!   keyword began more than that many encoder frames ago (HeardRight uses 38 frames,
//!   1.52 s). Off by default.
//! - Upstream silently resets a stream after 1.5 s of trailing blanks, restarting its
//!   timestamps. That reset happens only with
//!   [`KeywordSpotterConfig::trailing_blank_reset`] set; by default the caller owns resets.
//!
//! Opt-in additions, all off by default (the defaults are Sherpa's behaviour):
//! - [`KeywordSpotterConfig::onset`]: estimate where each detected keyword began
//!   ([`KeywordOnset`], read through [`KeywordSpotter::detection`]) without changing what is
//!   detected.
//! - [`KeywordSpotterConfig::threshold_tokens`], [`KeywordSpotterConfig::joiner_batching`],
//!   [`KeywordSpotterConfig::merge_timestamps`], [`KeywordSpotterConfig::portable_math`]:
//!   accuracy and portability levers.
//! - [`KeywordSpotterConfig::trace`]: record each firing at its pre-reset event and, in an
//!   optional per-stream window, what the beam did on every step (hypotheses, pruning, TTL
//!   expiry, merges, threshold evidence), read through [`KeywordStream::take_trace`].
//!   Observation only: it changes nothing the decoder does.
//! - [`KeywordSpotterConfig::static_batch`], [`KeywordSpotterConfig::decoder_cache`] and a
//!   shared [`KwsModel`] ([`KeywordSpotter::with_model`]): speed and memory levers that leave
//!   every detection bit-identical where their preconditions hold (see each).
//!
//! ```no_run
//! # fn run(pcm: &[f32]) -> rightkit_asr::Result<()> {
//! use rightkit_asr::kws::{KeywordSpotter, KeywordSpotterConfig, Keywords};
//! let dir = std::path::Path::new("kws");
//! let config = KeywordSpotterConfig::new(
//!     dir.join("encoder.int8.onnx"),
//!     dir.join("decoder.onnx"),
//!     dir.join("joiner.int8.onnx"),
//!     dir.join("tokens.txt"),
//!     Keywords::File(dir.join("keywords.txt")),
//! );
//! let mut spotter = KeywordSpotter::new(&config)?;
//! let mut stream = spotter.create_stream();
//! stream.accept_waveform(16_000, pcm)?;
//! while spotter.is_ready(&stream) {
//!     spotter.decode(&mut stream)?;
//!     let hit = spotter.result(&mut stream);
//!     if !hit.keyword.is_empty() {
//!         println!("{} at {:?}", hit.keyword, hit.timestamps);
//!     }
//! }
//! # Ok(()) }
//! ```

mod context_graph;
mod decoder;
mod fbank;
mod keywords;
mod kissfft;
mod math;
mod model;
mod onset;
mod shared;
mod trace;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub use fbank::FbankConfig;
pub use keywords::{encode_keywords, EncodedKeywords, SymbolTable};
pub use onset::{
    AcousticRule, AcousticSearch, FricativeAbstain, FricativeParams, KeywordOnset, OnsetConfig,
    OnsetEstimate,
};

use crate::error::{Error, Result};
use context_graph::ContextGraph;
use decoder::{DecoderOptions, DecoderResult};
use fbank::{FbankTables, OnlineFbank};
use math::Math;
use model::{StateTensor, Zipformer2Transducer};
use onset::OnsetHistory;
pub use trace::{
    FiringTrace, StepTrace, TraceClock, TraceConfig, TraceHypothesis, TraceIdentity, TraceMatch,
    TraceGapDrop, TraceMerge, TraceMergeSide, TracePath, TraceRecords, TraceTop,
};
use shared::{plan_beam_start, shared_onset_retention, FrameRing, SharedFrontEnd};
pub use shared::{BeamStart, BeamStream, FrontEndConfig, DEFAULT_RING_FRAMES};
use trace::TraceState;

/// Where the keyword list comes from. One keyword per line: tokens from `tokens.txt`,
/// then optional `:boost`, `#threshold` and `@label` (Sherpa's format), for example
/// `Z EH1 F ER0 S T AA1 P :2 #0.15 @ZEPHYR_STOP`.
#[derive(Debug, Clone, PartialEq)]
pub enum Keywords {
    File(PathBuf),
    Text(String),
}

/// Which token probabilities a keyword's threshold is compared against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThresholdTokens {
    /// The mean of the FIRST `n` token probabilities on the path, `n` the keyword's length
    /// (Sherpa). When a fail transition left earlier tokens on the path, those are read
    /// instead of the keyword's own.
    #[default]
    Upstream,
    /// The mean of the matched keyword's own `n` token probabilities.
    MatchedKeyword,
}

/// How the joiner is batched across the beam.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JoinerBatching {
    /// Every live hypothesis in one call per frame (Sherpa). The int8 joiner quantizes the
    /// batch together, so a path's scores depend on the other paths in the beam.
    #[default]
    Beam,
    /// One call per hypothesis: each path is scored on its own. Costs one joiner call per
    /// path per frame.
    PerHypothesis,
}

/// Whose timestamps a merged beam path keeps when two paths reach the same tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MergeTimestamps {
    /// The path inserted first (Sherpa). The beam inserts in acoustic-score order, before
    /// the keyword boost of a newly emitted token is added, so an old low-probability
    /// emission of the keyword's first token can keep its timestamp when the real
    /// keyword arrives: the reported first-token time is then early, and the active-path
    /// TTL counts from it.
    #[default]
    First,
    /// The path with the higher total score (boost included).
    Likelier,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeywordSpotterConfig {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
    pub keywords: Keywords,
    pub features: FbankConfig,
    /// Intra-op threads for each session (Sherpa `num_threads`).
    pub threads: usize,
    /// Beam width (Sherpa default 4).
    pub max_active_paths: usize,
    /// Blank frames required after a keyword's last token before it fires; a path
    /// fires once it has strictly more than this many (Sherpa default 1).
    pub num_trailing_blanks: i32,
    /// Boost for keywords whose line has no `:boost` (Sherpa default 1.0).
    pub keywords_score: f32,
    /// Mean token probability a keyword needs when its line has no `#threshold`
    /// (Sherpa default 0.25).
    pub keywords_threshold: f32,
    /// Drop a path whose active keyword started more than this many encoder frames
    /// (40 ms each) before the current frame. `None` keeps paths indefinitely, as
    /// upstream does.
    pub active_path_ttl_frames: Option<i32>,
    /// Reset a stream, as upstream Sherpa does, when its best path has carried more than
    /// 1.5 s of trailing blanks. The reset restarts timestamps from zero while the
    /// audio keeps flowing, so it is off unless a caller asks for Sherpa parity.
    pub trailing_blank_reset: bool,
    /// Evaluate the joiner log-softmax and token probabilities in `f64` as the GCC build
    /// of Sherpa does; production Sherpa builds (macOS, Windows) use `f32`. Parity
    /// tests against the Linux reference only.
    #[doc(hidden)]
    pub softmax_f64: bool,
    /// Estimate each detection's onset ([`KeywordOnset`]). `None` (the default) keeps no
    /// history and adds no work; with it, each stream keeps a bounded ring of encoder
    /// frames and energies, and each detection costs one decoder and one joiner call.
    /// Detection itself is unchanged.
    pub onset: Option<OnsetConfig>,
    /// Which probabilities a keyword's threshold applies to. Default: Sherpa's.
    pub threshold_tokens: ThresholdTokens,
    /// Joiner batching across the beam. Default: Sherpa's.
    pub joiner_batching: JoinerBatching,
    /// Whose timestamps a merged beam path keeps. Default: Sherpa's.
    pub merge_timestamps: MergeTimestamps,
    /// Compute the filterbank's per-frame logarithm, window, FFT twiddles and the softmax's
    /// `exp`/`log` with the pure-Rust `libm` crate, identical on every OS, instead of the
    /// platform C library Sherpa uses (whose last-ulp results differ between OSes). The mel
    /// weight table keeps the platform `logf`: under `libm` it moves loud bins by up to
    /// 1.7e-4, near the fbank parity gate. Default off.
    pub portable_math: bool,
    /// Build the encoder session with its free batch dimension `N` pinned to 1 (the spotter
    /// always runs one stream per call), so ONNX Runtime folds the encoder's shape
    /// subgraphs at load: about 10% less CPU per decode. Bit-identical output is proven for
    /// the int8 chunk-16 encoder on x86-64 (`kws_static_batch_encoder_is_bit_identical`); the
    /// fp32 encoder's output moves in the last ulps with it. A model whose encoder has no
    /// dimension `N` fails to load. Default off.
    pub static_batch: bool,
    /// Keep each decoder output row, keyed by its `context_size` tokens, and run the decoder
    /// only on contexts not seen yet: about 5% less CPU. Exact only where the decoder's rows
    /// do not depend on the batch they were computed in, which holds for the fp32 decoder on
    /// x86-64 (`kws_decoder_is_batch_invariant`; the int8 joiner does NOT have this property
    /// and is never cached). The cache lives in the [`KwsModel`], holds at most 2048 rows
    /// (2.6 MB for the 320-dim decoder) and is cleared by [`KeywordSpotter::reset`]. Default off.
    pub decoder_cache: bool,
    /// Record firings and, optionally, a per-step window of the beam ([`TraceConfig`]).
    /// `None` (the default) records nothing and adds no work beyond an `Option` check per
    /// decoded chunk and a `bool` check per step. Never changes what is detected.
    pub trace: Option<TraceConfig>,
    /// Stale-token gap guard: drop a beam path whose newest keyword token is more than this
    /// many encoder frames (40 ms each) after the previous keyword token of the same path,
    /// checked when the token is emitted. `None` (the default) is Sherpa's behaviour, bit for
    /// bit; `Some(g)` needs `g` in 1..=1000 and changes what can be detected.
    pub stale_gap_frames: Option<i32>,
    /// Write the encoder's next states into the buffers the previous call left, instead of
    /// allocating new ones (one set of spare buffers per model, a few MB). Every output is
    /// identical: the same bytes are copied from the same encoder outputs. Default off.
    pub encoder_state_reuse: bool,
    /// Compute decoder rows once, at spotter creation, for every context a keyword path can
    /// produce (a few hundred rows), and serve them from that table; a context outside it
    /// runs the decoder as usual. Exact only where the decoder's rows do not depend on the
    /// batch they are computed in (the fp32 decoder on x86-64:
    /// `kws_decoder_is_batch_invariant`), as for [`Self::decoder_cache`], which it overrides
    /// when both are set. Default off.
    pub decoder_table: bool,
    /// Shared front end (plan arm C6): the streams of THIS spotter keep a ring of their
    /// encoder frames so a confirm beam ([`KeywordSpotter::create_beam_stream`]) can decode
    /// them without an encoder of its own. Set on the wake spotter only. `None` (the default)
    /// keeps no ring and adds nothing to a stream. See [`FrontEndConfig`].
    pub shared_front_end: Option<FrontEndConfig>,
}

impl KeywordSpotterConfig {
    /// Sherpa's defaults for everything but the files.
    pub fn new(
        encoder: PathBuf,
        decoder: PathBuf,
        joiner: PathBuf,
        tokens: PathBuf,
        keywords: Keywords,
    ) -> Self {
        Self {
            encoder,
            decoder,
            joiner,
            tokens,
            keywords,
            features: FbankConfig::default(),
            threads: 1,
            max_active_paths: 4,
            num_trailing_blanks: 1,
            keywords_score: 1.0,
            keywords_threshold: 0.25,
            active_path_ttl_frames: None,
            trailing_blank_reset: false,
            softmax_f64: false,
            onset: None,
            threshold_tokens: ThresholdTokens::Upstream,
            joiner_batching: JoinerBatching::Beam,
            merge_timestamps: MergeTimestamps::First,
            portable_math: false,
            static_batch: false,
            decoder_cache: false,
            trace: None,
            stale_gap_frames: None,
            encoder_state_reuse: false,
            decoder_table: false,
            shared_front_end: None,
        }
    }
}

/// A detection. `keyword` is empty when there is nothing new to report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeywordResult {
    /// The keyword's `@label`, or its tokens joined when it has none.
    pub keyword: String,
    pub tokens: Vec<String>,
    /// Encoder frame of each token, counted from the stream's last reset.
    pub frames: Vec<i32>,
    /// `frames` in seconds (40 ms per frame), as Sherpa reports them.
    pub timestamps: Vec<f32>,
}

impl KeywordResult {
    pub fn is_empty(&self) -> bool {
        self.keyword.is_empty()
    }
}

/// A detection with what the spotter knew about it ([`KeywordSpotter::detection`]).
#[derive(Debug, Clone, PartialEq)]
pub struct KeywordDetection {
    pub result: KeywordResult,
    /// Probability of each keyword token when it was emitted.
    pub token_probs: Vec<f32>,
    /// The mean token probability that passed `threshold` (see [`ThresholdTokens`]).
    pub score: f32,
    /// The keyword's threshold: its `#threshold`, else `keywords_threshold`.
    pub threshold: f32,
    /// Encoder frame (on the clock of `result.frames`) on which the keyword fired, after
    /// its last token and the trailing blanks the beam waited for.
    pub fired_frame: i32,
    /// Paths the stale-token gap guard has dropped since the stream's last reset, up to this
    /// firing (`0` with the guard off).
    pub gap_drops: u64,
    /// Of those, the ones dropped in the chunk this detection fired in, up to the firing.
    pub gap_drops_chunk: u64,
    /// The largest gap between consecutive emitted keyword tokens, in encoder frames: an
    /// observation of the output (`0` for one token), recorded with the guard on or off.
    pub max_gap_frames: i32,
    /// The guard is on and `max_gap_frames` exceeds its limit; always `false` with it off.
    pub gap_violation: bool,
    /// Onset estimates; `None` unless [`KeywordSpotterConfig::onset`] is set.
    pub onset: Option<KeywordOnset>,
    /// Shared confirm beams only: why `onset` is `None` although estimation is on (the history
    /// the estimator needs was evicted); always `None` for an ordinary stream.
    pub onset_unavailable: Option<&'static str>,
}

/// What building the decoder table did for one spotter.
#[derive(Debug, Clone, Copy, Default)]
struct TableBuild {
    requested_rows: usize,
    precompute_ms: f64,
    declined: Option<&'static str>,
}

/// The effective decoder mode of a spotter and what its table cost
/// ([`KeywordSpotter::decoder_table_report`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecoderTableReport {
    /// `table`, `cache` or `plain`: the decoder path decode actually takes (a declined table
    /// falls back to `cache` when [`KeywordSpotterConfig::decoder_cache`] is set).
    pub mode: &'static str,
    /// Why a requested table was not used: `graph_over_cap` or `model_table_full`.
    pub declined: Option<&'static str>,
    /// Startup time of the table build (graph walk, one decoder batch, admission), in ms;
    /// `0` when no table was requested.
    pub precompute_ms: f64,
    /// Rows this spotter's graph asked for.
    pub spotter_rows: usize,
    /// Rows the model's table holds in all (shared with other spotters on the model), and
    /// the bytes of their values and context keys (not the map's overhead).
    pub model_rows: usize,
    pub model_bytes: usize,
}

/// Most rows [`KeywordSpotterConfig::decoder_table`] will precompute (1.3 KB each for the
/// 320-dim decoder); a keyword graph needing more keeps the plain decoder path.
const DECODER_TABLE_MAX_ROWS: usize = 4096;

static NEXT_SPOTTER_ID: AtomicU64 = AtomicU64::new(1);

/// The encoder, decoder and joiner sessions of one model, shareable by several spotters
/// ([`KeywordSpotter::with_model`]) that differ in keywords or beam settings: one set of
/// sessions instead of one per spotter. Calls from the spotters sharing it run one at a time
/// (a mutex around the sessions, held for one [`KeywordSpotter::decode`]). Built from a
/// [`KeywordSpotterConfig`]'s `encoder`, `decoder`, `joiner`, `features`, `threads` and
/// `static_batch`; every spotter on it must name the same.
pub struct KwsModel {
    sessions: Mutex<Zipformer2Transducer>,
    encoder: PathBuf,
    decoder: PathBuf,
    joiner: PathBuf,
    feature_dim: usize,
    threads: usize,
    static_batch: bool,
    chunk_size: usize,
    chunk_shift: usize,
    context_size: usize,
    init_states: Vec<StateTensor>,
}

impl KwsModel {
    /// Load the three sessions `config` names. Keywords and decoder settings are ignored.
    pub fn load(config: &KeywordSpotterConfig) -> Result<Arc<Self>> {
        let feature_dim = FbankTables::new(&config.features, Math::new(config.portable_math))?.dim();
        let sessions = Zipformer2Transducer::load(
            &config.encoder,
            &config.decoder,
            &config.joiner,
            feature_dim,
            config.threads,
            config.static_batch,
        )?;
        Ok(Arc::new(Self {
            encoder: config.encoder.clone(),
            decoder: config.decoder.clone(),
            joiner: config.joiner.clone(),
            feature_dim,
            threads: config.threads,
            static_batch: config.static_batch,
            chunk_size: sessions.chunk_size(),
            chunk_shift: sessions.chunk_shift(),
            context_size: sessions.context_size(),
            init_states: sessions.init_states(),
            sessions: Mutex::new(sessions),
        }))
    }

    /// The sessions. A panic inside a call leaves them usable (the decoder cache is
    /// insert-only), so a poisoned lock is taken over.
    fn lock(&self) -> MutexGuard<'_, Zipformer2Transducer> {
        self.sessions.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Refuse a config that describes another model than this one.
    fn check_config(&self, config: &KeywordSpotterConfig, feature_dim: usize) -> Result<()> {
        let mut differs = Vec::new();
        if config.encoder != self.encoder {
            differs.push("encoder");
        }
        if config.decoder != self.decoder {
            differs.push("decoder");
        }
        if config.joiner != self.joiner {
            differs.push("joiner");
        }
        if feature_dim != self.feature_dim {
            differs.push("features");
        }
        if config.threads != self.threads {
            differs.push("threads");
        }
        if config.static_batch != self.static_batch {
            differs.push("static_batch");
        }
        if differs.is_empty() {
            Ok(())
        } else {
            Err(Error::Config(format!(
                "keyword spotter config differs from its shared model in {}",
                differs.join(", ")
            )))
        }
    }
}

/// Model sessions plus the keyword graph shared by its streams.
pub struct KeywordSpotter {
    id: u64,
    model: Arc<KwsModel>,
    symbols: SymbolTable,
    graph: Arc<ContextGraph>,
    fbank: Arc<FbankTables>,
    sample_rate: u32,
    options: DecoderOptions,
    trailing_blank_reset: bool,
    onset: Option<OnsetConfig>,
    encoder_state_reuse: bool,
    /// Ring settings of the shared front end; `None` when it is off.
    shared: Option<FrontEndConfig>,
    /// What `decoder_table` did at creation (default when it was not requested).
    table_build: TableBuild,
    /// Set only when [`KeywordSpotterConfig::trace`] records something.
    trace: Option<(TraceConfig, Arc<TraceIdentity>)>,
}

/// One audio stream's features, encoder cache and beam.
pub struct KeywordStream {
    spotter_id: u64,
    fbank: OnlineFbank,
    sample_rate: u32,
    processed_frames: usize,
    states: Vec<StateTensor>,
    result: DecoderResult,
    /// Last timestamp of the result most recently reported (Sherpa's
    /// `prev_keyword_result_`), so one detection is reported once.
    reported_last_frame: Option<i32>,
    /// Retained frames for onset estimation; `None` when it is off.
    onset: Option<OnsetHistory>,
    /// Trace records; `None` when tracing is off.
    trace: Option<TraceState>,
    /// Encoder-frame ring and confirm-beam onset history; `None` when the shared front end is
    /// off, so an ordinary stream holds nothing extra.
    shared: Option<SharedFrontEnd>,
}

impl KeywordSpotter {
    /// A spotter with its own [`KwsModel`].
    pub fn new(config: &KeywordSpotterConfig) -> Result<Self> {
        Self::build(config, None)
    }

    /// A spotter on a shared [`KwsModel`] (from [`KwsModel::load`] or another spotter's
    /// [`Self::model`]), with `config`'s keywords and decoder settings. `config` must name the
    /// model's files, features, threads and `static_batch`; anything else is refused.
    pub fn with_model(config: &KeywordSpotterConfig, model: Arc<KwsModel>) -> Result<Self> {
        Self::build(config, Some(model))
    }

    /// The model this spotter runs on, to share with another spotter.
    pub fn model(&self) -> &Arc<KwsModel> {
        &self.model
    }

    fn build(config: &KeywordSpotterConfig, model: Option<Arc<KwsModel>>) -> Result<Self> {
        if config.max_active_paths == 0 {
            return Err(Error::Config("max_active_paths must be at least 1".into()));
        }
        if config.shared_front_end.is_some_and(|front_end| !front_end.valid()) {
            return Err(Error::Config("shared_front_end ring_frames must be 16 to 4096".into()));
        }
        if config
            .stale_gap_frames
            .is_some_and(|gap| !(1..=1000).contains(&gap))
        {
            return Err(Error::Config("stale_gap_frames must be 1 to 1000".into()));
        }
        let symbols = SymbolTable::parse(&std::fs::read_to_string(&config.tokens).map_err(
            |e| Error::Config(format!("read {}: {e}", config.tokens.display())),
        )?)?;
        let keywords_text = match &config.keywords {
            Keywords::File(path) => std::fs::read_to_string(path)
                .map_err(|e| Error::Config(format!("read {}: {e}", path.display())))?,
            Keywords::Text(text) => text.clone(),
        };
        let encoded = encode_keywords(&keywords_text, &symbols)?;
        let graph = ContextGraph::new(
            &encoded.ids,
            config.keywords_score,
            config.keywords_threshold,
            &encoded.scores,
            &encoded.phrases,
            &encoded.thresholds,
        );
        let math = Math::new(config.portable_math);
        let fbank = FbankTables::new(&config.features, math)?;
        let model = match model {
            Some(model) => {
                model.check_config(config, fbank.dim())?;
                model
            }
            None => KwsModel::load(config)?,
        };
        let mut options = DecoderOptions {
            max_active_paths: config.max_active_paths,
            num_trailing_blanks: config.num_trailing_blanks,
            unk_id: symbols.id("<unk>").map(i64::from),
            active_path_ttl_frames: config.active_path_ttl_frames,
            softmax_f64: config.softmax_f64,
            threshold_tokens: config.threshold_tokens,
            joiner_batching: config.joiner_batching,
            merge_timestamps: config.merge_timestamps,
            math,
            decoder_cache: config.decoder_cache,
            decoder_table: config.decoder_table,
            stale_gap_frames: config.stale_gap_frames,
        };
        let mut table_build = TableBuild::default();
        if config.decoder_table {
            // Precompute the decoder rows of every keyword-path context in one batch. A
            // declined table (this graph alone over the cap, or the model's table would pass
            // it) turns the table OFF for this spotter, so it falls back to the configured
            // `decoder_cache`, else the plain decoder; it never half-uses a table.
            let started = std::time::Instant::now();
            let context_size = model.context_size;
            match decoder::keyword_context_table(
                &graph.keyword_tokens(),
                context_size,
                DECODER_TABLE_MAX_ROWS,
            ) {
                Some(contexts) => {
                    table_build.requested_rows = contexts.len() / context_size;
                    if !model
                        .lock()
                        .extend_keyword_table(&contexts, DECODER_TABLE_MAX_ROWS)?
                    {
                        table_build.declined = Some("model_table_full");
                    }
                }
                None => table_build.declined = Some("graph_over_cap"),
            }
            table_build.precompute_ms = started.elapsed().as_secs_f64() * 1000.0;
            if table_build.declined.is_some() {
                options.decoder_table = false;
            }
        }
        let trace = config.trace.filter(TraceConfig::is_active).map(|trace| {
            let config_repr = format!(
                "{options:?}|{}|{}|{:?}",
                config.keywords_score, config.keywords_threshold, config.features
            );
            let identity = TraceIdentity::new(
                &keywords_text,
                &config_repr,
                [&config.encoder, &config.decoder, &config.joiner],
            );
            (trace, Arc::new(identity))
        });
        Ok(Self {
            id: NEXT_SPOTTER_ID.fetch_add(1, Ordering::Relaxed),
            model,
            symbols,
            graph: Arc::new(graph),
            fbank: Arc::new(fbank),
            sample_rate: config.features.sample_rate,
            options,
            trailing_blank_reset: config.trailing_blank_reset,
            onset: config.onset.clone(),
            encoder_state_reuse: config.encoder_state_reuse,
            shared: config.shared_front_end,
            table_build,
            trace,
        })
    }

    pub fn create_stream(&self) -> KeywordStream {
        self.build_stream(true)
    }

    /// [`Self::create_stream`] without trace state even when [`KeywordSpotterConfig::trace`]
    /// is set, for a stream whose records nobody will drain (nothing allocates invisibly).
    pub fn create_stream_untraced(&self) -> KeywordStream {
        self.build_stream(false)
    }

    fn build_stream(&self, traced: bool) -> KeywordStream {
        KeywordStream {
            spotter_id: self.id,
            fbank: OnlineFbank::new(Arc::clone(&self.fbank)),
            sample_rate: self.sample_rate,
            processed_frames: 0,
            states: self.model.init_states.clone(),
            result: DecoderResult::new(self.model.context_size),
            reported_last_frame: None,
            onset: self.onset.clone().map(|config| {
                OnsetHistory::new(config, self.fbank.first_bin_from(onset::HIGH_BAND_HZ))
            }),
            trace: self
                .trace
                .as_ref()
                .filter(|_| traced)
                .map(|(config, identity)| TraceState::new(*config, Arc::clone(identity))),
            // An untraced stream is a throwaway one (a fresh decode); it never owns a ring.
            shared: self.shared.filter(|_| traced).map(|front_end| SharedFrontEnd {
                id: shared::next_front_end_id(),
                ring: FrameRing::new(front_end.ring_frames),
                onset: self.onset.as_ref().map(|config| {
                    let mut config = config.clone();
                    let (encoder, feature) = shared_onset_retention(front_end.ring_frames, &config);
                    config.encoder_history = encoder;
                    config.feature_history = feature;
                    OnsetHistory::new(config, self.fbank.first_bin_from(onset::HIGH_BAND_HZ))
                }),
                epoch: 0,
            }),
        }
    }

    /// The decoder path this spotter actually takes and what its table cost.
    pub fn decoder_table_report(&self) -> DecoderTableReport {
        let (model_rows, model_bytes) = self.model.lock().keyword_table_stats();
        DecoderTableReport {
            mode: if self.options.decoder_table {
                "table"
            } else if self.options.decoder_cache {
                "cache"
            } else {
                "plain"
            },
            declined: self.table_build.declined,
            precompute_ms: self.table_build.precompute_ms,
            spotter_rows: self.table_build.requested_rows,
            model_rows,
            model_bytes,
        }
    }

    /// True when the stream holds a full encoder chunk beyond what was decoded.
    pub fn is_ready(&self, stream: &KeywordStream) -> bool {
        stream.processed_frames + self.model.chunk_size < stream.fbank.num_frames_ready()
    }

    /// Decode one encoder chunk. Call while [`Self::is_ready`] holds.
    pub fn decode(&mut self, stream: &mut KeywordStream) -> Result<()> {
        self.check_owner(stream)?;
        if !self.is_ready(stream) {
            return Err(Error::Model("keyword stream is not ready to decode".into()));
        }
        if self.trailing_blank_reset {
            let blanks = stream.peek_result_for_reset().num_trailing_blanks;
            let trailing_seconds = ((blanks * 4) as f64 * 0.01) as f32;
            if trailing_seconds > 1.5 {
                // Before the sessions are locked: `reset` may clear the decoder cache.
                self.reset(stream);
            }
        }
        let features = stream
            .fbank
            .frames(stream.processed_frames, self.model.chunk_size)?;
        if let Some(history) = stream.onset.as_mut() {
            history.push_features(stream.processed_frames, &features, self.fbank.dim());
        }
        if let Some(history) = stream.shared.as_mut().and_then(|shared| shared.onset.as_mut()) {
            history.push_features(stream.processed_frames, &features, self.fbank.dim());
        }
        stream.processed_frames += self.model.chunk_shift;
        if let Some(trace) = stream.trace.as_mut() {
            trace.set_clock(
                stream.fbank.num_frames_ready(),
                stream.processed_frames,
                self.model.chunk_shift,
                self.model.chunk_size,
            );
        }
        let mut model = self.model.lock();
        let encoder_out = if self.encoder_state_reuse {
            model.run_encoder_reuse(&features, &mut stream.states)?
        } else {
            model.run_encoder(&features, &mut stream.states)?
        };
        if let Some(history) = stream.onset.as_mut() {
            history.push_encoder(stream.result.frame_offset, &encoder_out);
        }
        if let Some(shared) = stream.shared.as_mut() {
            shared.ring.push_chunk(stream.result.frame_offset, &encoder_out);
            if let Some(history) = shared.onset.as_mut() {
                history.push_encoder(stream.result.frame_offset, &encoder_out);
            }
        }
        let firings = stream.result.firings;
        decoder::decode_chunk(
            &mut model,
            &self.graph,
            &self.options,
            &encoder_out,
            &mut stream.result,
            stream.trace.as_mut(),
        )?;
        if stream.result.firings != firings {
            self.estimate_onset(&mut model, stream)?;
        }
        Ok(())
    }

    /// Fill the newest firing's onset estimates, when onset estimation is on.
    fn estimate_onset(
        &self,
        model: &mut Zipformer2Transducer,
        stream: &mut KeywordStream,
    ) -> Result<()> {
        let (Some(history), Some(firing)) = (stream.onset.as_ref(), stream.result.firing.as_ref())
        else {
            return Ok(());
        };
        let onset = onset::estimate(
            model,
            history,
            &onset::Firing {
                context: &firing.context,
                tokens: &stream.result.tokens,
                frames: &stream.result.timestamps,
                fired_frame: firing.frame,
            },
            self.options.joiner_batching,
            self.options.math,
        )?;
        if let Some(firing) = stream.result.firing.as_mut() {
            firing.onset = Some(onset);
        }
        Ok(())
    }

    /// The newest detection, reported once: later calls return an empty result until
    /// a detection that starts after the reported one.
    pub fn result(&self, stream: &mut KeywordStream) -> KeywordResult {
        let Some(result) = stream.take_new_result() else {
            return KeywordResult::default();
        };
        self.keyword_result(result)
    }

    /// [`Self::result`] with the detection's token probabilities, score, threshold and
    /// onset estimates. `None` when there is nothing new to report; it shares
    /// [`Self::result`]'s once-only gate, so use one or the other on a stream.
    pub fn detection(&self, stream: &mut KeywordStream) -> Option<KeywordDetection> {
        let result = stream.take_new_result()?;
        let firing = result.firing.clone();
        let result = self.keyword_result(result);
        if result.is_empty() {
            return None;
        }
        let firing = firing?;
        Some(KeywordDetection {
            result,
            token_probs: firing.token_probs,
            score: firing.score,
            threshold: firing.threshold,
            fired_frame: firing.frame,
            gap_drops: firing.gap_drops,
            gap_drops_chunk: firing.gap_drops_chunk,
            max_gap_frames: firing.max_gap_frames,
            gap_violation: firing.gap_violation,
            onset: firing.onset,
            onset_unavailable: firing.onset_unavailable,
        })
    }

    fn keyword_result(&self, result: &DecoderResult) -> KeywordResult {
        let tokens: Vec<String> = result
            .tokens
            .iter()
            .map(|t| self.symbols.symbol(*t as i32).to_string())
            .collect();
        let keyword = if result.keyword.is_empty() {
            let joined: String = tokens.concat();
            joined.chars().skip(1).collect()
        } else {
            result.keyword.clone()
        };
        // Convert(): 10 ms feature shift times subsampling 4, as float.
        let frame_seconds = (10.0f64 / 1000.0 * 4.0) as f32;
        KeywordResult {
            keyword,
            tokens,
            timestamps: result
                .timestamps
                .iter()
                .map(|t| frame_seconds * *t as f32)
                .collect(),
            frames: result.timestamps.clone(),
        }
    }

    /// Clear the beam and encoder cache (Sherpa `Reset`). Buffered features and the
    /// reported-result memory are kept; timestamps restart from zero. With
    /// [`KeywordSpotterConfig::decoder_cache`] the model's decoder rows are dropped too.
    pub fn reset(&self, stream: &mut KeywordStream) {
        if self.options.decoder_cache {
            self.model.lock().clear_decoder_cache();
        }
        stream.result = DecoderResult::new(self.model.context_size);
        stream.states = self.model.init_states.clone();
        if let Some(history) = stream.onset.as_mut() {
            history.reset(stream.processed_frames);
        }
        if let Some(trace) = stream.trace.as_mut() {
            trace.on_reset(stream.processed_frames);
        }
        if let Some(shared) = stream.shared.as_mut() {
            // The wake clock restarts: the ring is emptied and beams made before are dead.
            shared.ring.clear();
            shared.epoch += 1;
            if let Some(history) = shared.onset.as_mut() {
                history.reset(stream.processed_frames);
            }
        }
    }

    /// A confirm beam that decodes the encoder frames `front_end` (a stream of THIS or another
    /// spotter built with [`KeywordSpotterConfig::shared_front_end`]) keeps, starting at the
    /// wake-clock encoder frame `requested_frame` (FLOOR of the requested sample over 640;
    /// negative when it precedes the stream's origin). `front_end_spotter` is the spotter that
    /// owns `front_end`: both must run the same model files and feature settings. The start is
    /// clamped to the origin and then to the ring, each reported separately in
    /// [`BeamStream::start`]. The beam's frames are on the front end's clock.
    pub fn create_beam_stream(
        &self,
        front_end_spotter: &KeywordSpotter,
        front_end: &KeywordStream,
        requested_frame: i64,
    ) -> Result<BeamStream> {
        let (ours, theirs) = (&self.model, &front_end_spotter.model);
        if ours.encoder != theirs.encoder
            || ours.decoder != theirs.decoder
            || ours.joiner != theirs.joiner
            || ours.feature_dim != theirs.feature_dim
            || ours.context_size != theirs.context_size
        {
            return Err(Error::Config(
                "shared front end: the beam and the front end differ in model files or features".into(),
            ));
        }
        front_end_spotter.check_owner(front_end)?;
        let Some(shared) = front_end.shared.as_ref() else {
            return Err(Error::Config("stream has no shared front end".into()));
        };
        let start = plan_beam_start(requested_frame, shared.ring.first(), shared.ring.end());
        let mut result = DecoderResult::new(self.model.context_size);
        result.frame_offset = start.start_frame;
        Ok(BeamStream {
            spotter_id: self.id,
            result,
            reported_last_frame: None,
            next_frame: start.start_frame,
            start,
            epoch: shared.epoch,
            front_end_id: shared.id,
            ring_frames: shared.ring.len(),
            ring_bytes: shared.ring.bytes(),
            trace: self
                .trace
                .as_ref()
                .map(|(config, identity)| TraceState::new(*config, Arc::clone(identity))),
        })
    }

    /// True when the front end holds frames this beam has not decoded. A front end that is not
    /// the one the beam was made on (another stream, or reset) also answers true, so the next
    /// [`Self::decode_beam`] fails with the explicit reason instead of the beam stalling.
    pub fn beam_ready(&self, beam: &BeamStream, front_end: &KeywordStream) -> bool {
        front_end.shared.as_ref().is_some_and(|shared| {
            beam.check_source(shared.id, shared.epoch).is_err() || beam.next_frame < shared.ring.end()
        })
    }

    /// Decode the next chunk (as many frames as one encoder call yields) of retained frames
    /// into `beam`. Call while [`Self::beam_ready`] holds. Fails when the front end was reset
    /// since the beam was made, or the frames the beam needs were evicted: the caller drops the
    /// beam.
    pub fn decode_beam(&mut self, beam: &mut BeamStream, front_end: &KeywordStream) -> Result<()> {
        if beam.spotter_id != self.id {
            return Err(Error::Model("beam belongs to a different spotter".into()));
        }
        let Some(shared) = front_end.shared.as_ref() else {
            return Err(Error::Config("stream has no shared front end".into()));
        };
        beam.check_source(shared.id, shared.epoch)
            .map_err(|reason| Error::Model(reason.into()))?;
        let end = shared.ring.end();
        if beam.next_frame >= end {
            return Err(Error::Model("beam has no retained frames to decode".into()));
        }
        let per_call = (self.model.chunk_shift / 4).max(1) as i32;
        let to = (beam.next_frame + per_call).min(end);
        let Some(out) = shared.ring.slice(beam.next_frame, to) else {
            return Err(Error::Model("frames the beam needs were evicted from the ring".into()));
        };
        if let Some(trace) = beam.trace.as_mut() {
            trace.set_clock(
                front_end.fbank.num_frames_ready(),
                front_end.processed_frames,
                self.model.chunk_shift,
                self.model.chunk_size,
            );
        }
        let mut model = self.model.lock();
        let firings = beam.result.firings;
        decoder::decode_chunk(
            &mut model,
            &self.graph,
            &self.options,
            &out,
            &mut beam.result,
            beam.trace.as_mut(),
        )?;
        beam.next_frame = to;
        if beam.result.firings != firings {
            self.estimate_beam_onset(&mut model, beam, shared)?;
        }
        Ok(())
    }

    /// The beam's onset from the front end's confirm history, or an explicit reason it is
    /// unavailable (never an estimate from a shorter window).
    fn estimate_beam_onset(
        &self,
        model: &mut Zipformer2Transducer,
        beam: &mut BeamStream,
        shared: &SharedFrontEnd,
    ) -> Result<()> {
        let Some(history) = shared.onset.as_ref() else {
            return Ok(());
        };
        let Some(firing) = beam.result.firing.as_ref() else {
            return Ok(());
        };
        let emitted = beam.result.timestamps.first().copied().unwrap_or(0);
        match history.lookback_available(emitted, firing.frame) {
            Err(reason) => {
                if let Some(firing) = beam.result.firing.as_mut() {
                    firing.onset_unavailable = Some(reason);
                }
            }
            Ok(()) => {
                let onset = onset::estimate(
                    model,
                    history,
                    &onset::Firing {
                        context: &firing.context,
                        tokens: &beam.result.tokens,
                        frames: &beam.result.timestamps,
                        fired_frame: firing.frame,
                    },
                    self.options.joiner_batching,
                    self.options.math,
                )?;
                if let Some(firing) = beam.result.firing.as_mut() {
                    firing.onset = Some(onset);
                }
            }
        }
        Ok(())
    }

    /// [`Self::detection`] for a beam: the newest detection, reported once.
    pub fn beam_detection(&self, beam: &mut BeamStream) -> Option<KeywordDetection> {
        let result = beam.take_new_result()?;
        let firing = result.firing.clone();
        let result = self.keyword_result(result);
        if result.is_empty() {
            return None;
        }
        let firing = firing?;
        Some(KeywordDetection {
            result,
            token_probs: firing.token_probs,
            score: firing.score,
            threshold: firing.threshold,
            fired_frame: firing.frame,
            gap_drops: firing.gap_drops,
            gap_drops_chunk: firing.gap_drops_chunk,
            max_gap_frames: firing.max_gap_frames,
            gap_violation: firing.gap_violation,
            onset: firing.onset,
            onset_unavailable: firing.onset_unavailable,
        })
    }

    fn check_owner(&self, stream: &KeywordStream) -> Result<()> {
        if stream.spotter_id == self.id {
            Ok(())
        } else {
            Err(Error::Model(
                "keyword stream belongs to a different spotter".into(),
            ))
        }
    }
}

impl KeywordStream {
    /// Append mono PCM in `[-1, 1]`. Resampling is not supported: `sample_rate` must
    /// equal the configured feature rate.
    pub fn accept_waveform(&mut self, sample_rate: u32, samples: &[f32]) -> Result<()> {
        if sample_rate != self.sample_rate {
            return Err(Error::Audio(format!(
                "keyword stream expects {} Hz audio, got {sample_rate} Hz",
                self.sample_rate
            )));
        }
        let accepted = self.fbank.accept_waveform(samples);
        if accepted.is_ok() {
            if let Some(trace) = self.trace.as_mut() {
                trace.note_samples(samples.len());
            }
        }
        accepted
    }

    /// Record per-step window traces only for encoder frames in `from_frame..to_frame` (on
    /// the clock of [`KeywordResult::frames`], counted from the last reset). Without a
    /// window every frame is recorded until [`TraceConfig::window_steps`] is reached. A
    /// reset clears the window. No effect when tracing is off.
    pub fn set_trace_window(&mut self, from_frame: i32, to_frame: i32) {
        if let Some(trace) = self.trace.as_mut() {
            trace.set_window(from_frame, to_frame);
        }
    }

    /// For a stream with no known window: keep the NEWEST [`TraceConfig::window_steps`]
    /// steps (older ones are dropped and counted) instead of the first ones. Replaces any
    /// window; a reset clears it. No effect when tracing is off.
    pub fn set_trace_rolling(&mut self) {
        if let Some(trace) = self.trace.as_mut() {
            trace.set_rolling();
        }
    }

    /// Declare the samples fed from now on as padding (until declared `false`), so records
    /// carry `padding_samples_fed` apart from real audio. The crate cannot tell them
    /// apart itself. No effect when tracing is off.
    pub fn declare_trace_padding(&mut self, padding: bool) {
        if let Some(trace) = self.trace.as_mut() {
            trace.declare_padding(padding);
        }
    }

    /// Drain what this stream recorded since the last call. `None` when tracing is off.
    pub fn take_trace(&mut self) -> Option<TraceRecords> {
        self.trace.as_mut().map(TraceState::take)
    }

    /// Append precomputed feature frames (`dim` values each) instead of audio, so a
    /// parity test can drive the encoder and beam with a reference filterbank.
    #[doc(hidden)]
    pub fn accept_features(&mut self, frames: &[f32]) {
        self.fbank.push_frames(frames);
    }

    /// Flush the last frames; no audio may follow.
    pub fn input_finished(&mut self) {
        self.fbank.input_finished();
    }

    /// `GetKeywordResult(true)`: the current result unless it was already reported.
    fn take_new_result(&mut self) -> Option<&DecoderResult> {
        let first = self.result.timestamps.first().copied();
        if let (Some(reported), Some(first)) = (self.reported_last_frame, first) {
            if first <= reported {
                return None;
            }
        }
        self.reported_last_frame = self.result.timestamps.last().copied();
        Some(&self.result)
    }

    /// The upstream reset check reads the result through the same once-only gate, so
    /// an unreported detection counts as reported afterwards.
    fn peek_result_for_reset(&mut self) -> DecoderResult {
        self.take_new_result()
            .cloned()
            .unwrap_or_else(DecoderResult::empty)
    }
}
