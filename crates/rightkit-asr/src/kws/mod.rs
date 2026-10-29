//! Streaming keyword spotting on a Zipformer2 transducer.
//!
//! A Rust port of the Sherpa-ONNX 1.13.4 keyword spotter (`KeywordSpotter`,
//! `OnlineStream`, `TransducerKeywordDecoder`, `ContextGraph`, the streaming Zipformer2
//! model wrapper) and of the kaldi-native-fbank features it uses, running the same
//! encoder/decoder/joiner ONNX files on [`rightkit_onnx`]. One [`KeywordSpotter`] holds
//! the three sessions and a keyword graph; each [`KeywordStream`] holds one audio
//! stream's features, encoder cache and beam.
//!
//! Two behaviours differ from upstream and are configured, not patched:
//! - [`KeywordSpotterConfig::active_path_ttl_frames`] drops a beam path whose current
//!   keyword began more than that many encoder frames ago (HeardRight uses 38 frames,
//!   1.52 s). Off by default.
//! - Upstream silently resets a stream after 1.5 s of trailing blanks, restarting its
//!   timestamps. That reset happens only with
//!   [`KeywordSpotterConfig::trailing_blank_reset`] set; by default the caller owns resets.
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
mod model;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub use fbank::FbankConfig;
pub use keywords::{encode_keywords, EncodedKeywords, SymbolTable};

use crate::error::{Error, Result};
use context_graph::ContextGraph;
use decoder::{DecoderOptions, DecoderResult};
use fbank::{FbankTables, OnlineFbank};
use model::{StateTensor, Zipformer2Transducer};

/// Where the keyword list comes from. One keyword per line: tokens from `tokens.txt`,
/// then optional `:boost`, `#threshold` and `@label` (Sherpa's format), for example
/// `Z EH1 F ER0 S T AA1 P :2 #0.15 @ZEPHYR_STOP`.
#[derive(Debug, Clone, PartialEq)]
pub enum Keywords {
    File(PathBuf),
    Text(String),
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

static NEXT_SPOTTER_ID: AtomicU64 = AtomicU64::new(1);

/// Model sessions plus the keyword graph shared by its streams.
pub struct KeywordSpotter {
    id: u64,
    model: Zipformer2Transducer,
    symbols: SymbolTable,
    graph: Arc<ContextGraph>,
    fbank: Arc<FbankTables>,
    sample_rate: u32,
    options: DecoderOptions,
    trailing_blank_reset: bool,
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
}

impl KeywordSpotter {
    pub fn new(config: &KeywordSpotterConfig) -> Result<Self> {
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
        let fbank = FbankTables::new(&config.features)?;
        let model = Zipformer2Transducer::load(
            &config.encoder,
            &config.decoder,
            &config.joiner,
            fbank.dim(),
            config.threads,
        )?;
        let options = DecoderOptions {
            max_active_paths: config.max_active_paths,
            num_trailing_blanks: config.num_trailing_blanks,
            unk_id: symbols.id("<unk>").map(i64::from),
            active_path_ttl_frames: config.active_path_ttl_frames,
            softmax_f64: config.softmax_f64,
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
        })
    }

    pub fn create_stream(&self) -> KeywordStream {
        KeywordStream {
            spotter_id: self.id,
            fbank: OnlineFbank::new(Arc::clone(&self.fbank)),
            sample_rate: self.sample_rate,
            processed_frames: 0,
            states: self.model.init_states(),
            result: DecoderResult::new(self.model.context_size()),
            reported_last_frame: None,
        }
    }

    /// True when the stream holds a full encoder chunk beyond what was decoded.
    pub fn is_ready(&self, stream: &KeywordStream) -> bool {
        stream.processed_frames + self.model.chunk_size() < stream.fbank.num_frames_ready()
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
                self.reset(stream);
            }
        }
        let features = stream
            .fbank
            .frames(stream.processed_frames, self.model.chunk_size())?;
        stream.processed_frames += self.model.chunk_shift();
        let encoder_out = self.model.run_encoder(&features, &mut stream.states)?;
        decoder::decode_chunk(
            &mut self.model,
            &self.graph,
            &self.options,
            &encoder_out,
            &mut stream.result,
        )
    }

    /// The newest detection, reported once: later calls return an empty result until
    /// a detection that starts after the reported one.
    pub fn result(&self, stream: &mut KeywordStream) -> KeywordResult {
        let Some(result) = stream.take_new_result() else {
            return KeywordResult::default();
        };
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
    /// reported-result memory are kept; timestamps restart from zero.
    pub fn reset(&self, stream: &mut KeywordStream) {
        stream.result = DecoderResult::new(self.model.context_size());
        stream.states = self.model.init_states();
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
