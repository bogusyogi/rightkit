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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub use fbank::FbankConfig;
pub use keywords::{encode_keywords, EncodedKeywords, SymbolTable};
pub use onset::{AcousticRule, KeywordOnset, OnsetConfig, OnsetEstimate};

use crate::error::{Error, Result};
use context_graph::ContextGraph;
use decoder::{DecoderOptions, DecoderResult};
use fbank::{FbankTables, OnlineFbank};
use math::Math;
use model::{StateTensor, Zipformer2Transducer};
use onset::OnsetHistory;

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
    /// Onset estimates; `None` unless [`KeywordSpotterConfig::onset`] is set.
    pub onset: Option<KeywordOnset>,
}

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
        let options = DecoderOptions {
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
        };
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
        })
    }

    pub fn create_stream(&self) -> KeywordStream {
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
        stream.processed_frames += self.model.chunk_shift;
        let mut model = self.model.lock();
        let encoder_out = model.run_encoder(&features, &mut stream.states)?;
        if let Some(history) = stream.onset.as_mut() {
            history.push_encoder(stream.result.frame_offset, &encoder_out);
        }
        let firings = stream.result.firings;
        decoder::decode_chunk(
            &mut model,
            &self.graph,
            &self.options,
            &encoder_out,
            &mut stream.result,
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
            onset: firing.onset,
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
        self.fbank.accept_waveform(samples)
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

#[cfg(test)]
mod parity_tests;
