//! App-neutral unified transducer wrapper — load any compatible model dir (RNN-T or TDT) and
//! transcribe with the same API. Self-configures from the model's `config.json`
//! (`model_type` rnnt/tdt, `features_size` 80/128). This is the "both plug in
//! seamlessly" entry point: `Transducer::load(dir)?.transcribe(audio)` works for
//! whichever model the customer downloaded — no app-side branching.
use rightkit_ort::ort;
use crate::audio;
use crate::config::PreprocessorConfig;
use crate::decoder::TranscriptionResult;
use crate::decoder_tdt::ParakeetTDTDecoder;
use crate::error::{Error, Result};
use crate::execution::{ExecutionProvider, ModelConfig as ExecutionConfig};
use crate::model_transducer::{ContextBias, TransducerConfig, TransducerModel};
use crate::timestamps::{process_timestamps, TimestampMode};
use crate::vocab::Vocabulary;
use ndarray::Array2;
use rightkit_ort::ort::session::Session;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct ModelJson {
    #[serde(default)]
    model_type: String,
    #[serde(default = "default_feat")]
    features_size: usize,
    #[serde(default)]
    max_tokens_per_step: Option<usize>,
}
fn default_feat() -> usize { 80 }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransducerOptions {
    pub max_tokens_per_step: Option<usize>,
    pub duration_cap: usize,
    pub window_overlap_mel: usize,
    pub debug_diagnostics: bool,
}

impl Default for TransducerOptions {
    fn default() -> Self {
        Self {
            max_tokens_per_step: None,
            duration_cap: 2,
            window_overlap_mel: 0,
            debug_diagnostics: false,
        }
    }
}

impl TransducerOptions {
    pub fn new() -> Self { Self::default() }

    pub fn with_max_tokens_per_step(mut self, value: usize) -> Self {
        self.max_tokens_per_step = Some(value);
        self
    }

    pub fn with_duration_cap(mut self, value: usize) -> Self {
        self.duration_cap = value;
        self
    }

    pub fn with_window_overlap_mel(mut self, value: usize) -> Self {
        self.window_overlap_mel = value;
        self
    }

    pub fn with_debug_diagnostics(mut self, enabled: bool) -> Self {
        self.debug_diagnostics = enabled;
        self
    }
}

pub struct Transducer {
    model: TransducerModel,
    decoder: ParakeetTDTDecoder,
    vocab: Vocabulary,
    pre: PreprocessorConfig,
    model_dir: PathBuf,
    pub model_type: String,
    /// When the model dir ships `mel_nemo.onnx`, the log-mel is computed by that
    /// ONNX stage (a real-DFT port of NeMo's exact preprocessor) instead of the
    /// hand-written Rust `audio::extract_features_raw`. This bit-matches NeMo's
    /// mel (cosine 0.9995) and removes the Rust-mel drift (giga 1.89% -> 1.63% on
    /// pico-100). The stage takes conditioned 16k mono audio in, log-mel out;
    /// conditioning stays upstream, unchanged. Runs on CPU (tiny: cos/sin/fb matmuls).
    mel_session: Option<Session>,
}

impl Transducer {
    /// Load any compatible transducer model dir using reconciled defaults.
    pub fn load<P: AsRef<Path>>(path: P, config: Option<ExecutionConfig>) -> Result<Self> {
        Self::load_with_options(path, config, TransducerOptions::default())
    }

    /// Load any compatible transducer model dir with explicit shared-runtime options.
    /// Encoder on `config`'s provider
    /// (DML), decoder_joint on CPU (locked hybrid).
    pub fn load_with_options<P: AsRef<Path>>(
        path: P,
        config: Option<ExecutionConfig>,
        options: TransducerOptions,
    ) -> Result<Self> {
        let path = path.as_ref();
        if !path.is_dir() {
            return Err(Error::Config(format!("model path must be a directory: {}", path.display())));
        }
        let vocab_path = path.join("vocab.txt");
        let cfg_path = path.join("config.json");
        if !vocab_path.exists() || !cfg_path.exists() {
            return Err(Error::Config(format!("vocab.txt / config.json missing in {}", path.display())));
        }
        let mj: ModelJson = serde_json::from_str(&std::fs::read_to_string(&cfg_path)?)
            .map_err(|e| Error::Config(format!("parse config.json: {e}")))?;
        let has_duration = mj.model_type.contains("tdt");

        let pre = PreprocessorConfig {
            feature_extractor_type: "ParakeetFeatureExtractor".to_string(),
            feature_size: mj.features_size,            // 80 (rnnt) or 128 (tdt) from config
            hop_length: 160,
            n_fft: 512,
            padding_side: "right".to_string(),
            padding_value: 0.0,
            preemphasis: 0.97,
            processor_class: "ParakeetProcessor".to_string(),
            return_attention_mask: true,
            sampling_rate: 16000,
            win_length: 400,
        };

        let vocab = Vocabulary::from_file(&vocab_path)?;
        let vocab_size = vocab.size();
        let max_tokens = options.max_tokens_per_step.or(mj.max_tokens_per_step).unwrap_or(5);

        let tcfg = TransducerConfig {
            vocab_size,
            max_tokens_per_step: max_tokens,
            has_duration,
            duration_cap: options.duration_cap,
            window_overlap_mel: options.window_overlap_mel,
            debug_diagnostics: options.debug_diagnostics,
        };
        let model = TransducerModel::from_pretrained(path, config.unwrap_or_default(), tcfg)?;

        // Optional NeMo-exact mel stage (real-DFT ONNX). CPU EP — it's a few small matmuls.
        let mel_path = path.join("mel_nemo.onnx");
        let mel_session = if mel_path.exists() {
            let cfg = ExecutionConfig::new().with_execution_provider(ExecutionProvider::Cpu);
            let builder = Session::builder()?;
            let mut builder = cfg.apply_to_session_builder(builder)?;
            Some(builder.commit_from_file(&mel_path)?)
        } else {
            None
        };

        Ok(Self {
            model,
            decoder: ParakeetTDTDecoder::from_vocab(vocab.clone()),
            vocab,
            pre,
            model_dir: path.to_path_buf(),
            model_type: mj.model_type,
            mel_session,
        })
    }

    /// Run the `mel_nemo.onnx` stage: conditioned 16k audio [T] -> log-mel [frames, 128].
    fn run_mel(&mut self, audio: &[f32]) -> Result<Array2<f32>> {
        let sess = self
            .mel_session
            .as_mut()
            .ok_or_else(|| Error::Model("mel_nemo session missing".to_string()))?;
        let input = Array2::from_shape_vec((1, audio.len()), audio.to_vec())
            .map_err(|e| Error::Model(format!("mel input shape: {e}")))?;
        let outputs = sess.run(ort::inputs!(
            "audio" => ort::value::Value::from_array(input)?
        ))?;
        let mel_value = &outputs["mel"];
        let (shape, data) = mel_value
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Model(format!("extract mel: {e}")))?;
        // mel_nemo output is [1, 128, frames]; transpose to [frames, 128] (time-major),
        // matching `audio::extract_features_raw` so the encoder forward is unchanged.
        let feat = shape[1] as usize;
        let frames = shape[2] as usize;
        let mut out = Array2::<f32>::zeros((frames, feat));
        for c in 0..feat {
            for t in 0..frames {
                out[[t, c]] = data[c * frames + t];
            }
        }
        Ok(out)
    }

    pub fn model_dir(&self) -> &Path { &self.model_dir }

    fn extract_features(&mut self, audio: &[f32]) -> Result<Array2<f32>> {
        if self.mel_session.is_some() {
            self.run_mel(audio)
        } else {
            audio::extract_features_raw(audio.to_vec(), 16000, 1, &self.pre)
        }
    }

    /// Configure runtime phrase bias for the greedy transducer decoder.
    ///
    /// Biasing happens at decode time by adding `score` to the next token when
    /// it would start or continue one of the supplied phrase token paths. This
    /// is intentionally phrase-level and tokenized through the model's own
    /// fixed `vocab.txt`; it cannot make the ONNX model emit token ids that do
    /// not already exist in that vocabulary.
    ///
    /// Returns the number of phrases that were representable by the model
    /// vocabulary and therefore installed.
    pub fn set_context_bias_phrases<I, S>(&mut self, phrases: I, score: f32) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut seen = BTreeSet::new();
        let mut tokenized = Vec::new();
        for phrase in phrases {
            let phrase = phrase.as_ref().trim();
            if phrase.is_empty() || !seen.insert(phrase.to_ascii_lowercase()) {
                continue;
            }
            if let Some(ids) = self.vocab.encode_phrase_greedy(phrase) {
                tokenized.push(ids);
            }
        }
        let installed = tokenized.len();
        self.model
            .set_context_bias(Some(ContextBias::new(tokenized, score)));
        installed
    }

    pub fn clear_context_bias(&mut self) {
        self.model.set_context_bias(None);
    }

    /// Offline transcribe of 16k mono samples -> text. Same call for RNN-T and TDT.
    pub fn transcribe(&mut self, audio: &[f32]) -> Result<String> {
        let result = self.transcribe_result(audio)?;
        Ok(result.tokens.iter().map(|t| t.text.as_str()).collect::<String>().trim().to_string())
    }

    /// Offline transcribe of 16k mono samples with token timestamps.
    ///
    /// This exposes the same decoder result already computed by `transcribe`.
    /// It is used by buffered/partial experiments to decide which overlapped
    /// window tokens are stable enough to commit.
    pub fn transcribe_result(&mut self, audio: &[f32]) -> Result<TranscriptionResult> {
        let features = self.extract_features(audio)?;
        let (tokens, frames, durations) = self.model.forward(features)?;
        self.decoder.decode_with_timestamps(
            &tokens, &frames, &durations, self.pre.hop_length, self.pre.sampling_rate,
        )
    }

    /// Offline transcribe of 16k mono samples -> text plus grouped timestamps.
    ///
    /// `mode` controls how the decoder's token-level timings are grouped. Use
    /// [`TimestampMode::Sentences`] for subtitle cues when punctuation is
    /// present, or [`TimestampMode::Words`] for word-level metadata. This is the
    /// Subtitle entry point for callers that need grouped timestamps.
    pub fn transcribe_with_timestamps(
        &mut self,
        audio: &[f32],
        mode: TimestampMode,
    ) -> Result<TranscriptionResult> {
        let result = self.transcribe_result(audio)?;
        let grouped = process_timestamps(&result.tokens, mode);
        Ok(TranscriptionResult {
            text: result.text.trim().to_string(),
            tokens: grouped,
        })
    }
}

// ---------------------------------------------------------------------------
// Production decode: beam search + shared detokenizer + long-form route
// ---------------------------------------------------------------------------

use crate::longform::{self, LongFormOptions, LongFormProgress, SeamPolicy};
use crate::onnx_beam::{onnx_decode_policy, Hit};
use crate::parakeet_core::detok::{assemble_timed_pieces, DetokPolicy};
use crate::parakeet_core::policy::DecodePolicy;
use crate::parakeet_core::DecodedPiece;
use std::sync::atomic::AtomicBool;

/// Beam decode options for the ONNX path. Defaults are the Windows /
/// ONNX production policy (width 2, emission reward 0.10, duration-index cap 2,
/// 10 symbols per frame, canonical N0 detokenizer).
#[derive(Debug, Clone)]
pub struct BeamOptions {
    pub policy: DecodePolicy,
    pub max_symbols: usize,
    pub detok: DetokPolicy,
    pub seam: SeamPolicy,
}

impl Default for BeamOptions {
    fn default() -> Self {
        Self {
            policy: onnx_decode_policy(),
            max_symbols: 10,
            detok: DetokPolicy::canonical_rust(),
            seam: SeamPolicy::DedupOverlap,
        }
    }
}

impl BeamOptions {
    /// Keep `uh`/`um` (verbatim, quotable transcripts).
    pub fn verbatim(mut self) -> Self {
        self.detok.strip_fillers = false;
        self
    }

    /// The pre-beam greedy loop (width 1, no reward).
    pub fn greedy(mut self) -> Self {
        self.policy.beam_width = 1;
        self.policy.emit_reward = 0.0;
        self
    }
}

impl Transducer {
    /// Model window in samples: the static encoder's fixed mel window, else the
    /// 15 s padded-window contract.
    pub fn window_samples(&self) -> usize {
        self.model
            .static_window
            .map(|w| w * self.pre.hop_length)
            .unwrap_or_else(longform::padded_window_samples)
    }

    /// Decode ONE window with beam search, fresh predictor state and the shared
    /// first-party detokenizer. Times are relative to the window start. Use
    /// [`transcribe_long`](Self::transcribe_long) for longer audio; the greedy
    /// `transcribe*` methods keep the legacy hard-cut window loop.
    pub fn transcribe_window_beam(
        &mut self,
        audio: &[f32],
        opts: &BeamOptions,
    ) -> Result<TranscriptionResult> {
        if audio.len() > self.window_samples() + self.pre.hop_length {
            return Err(Error::Audio(format!(
                "audio is {:.1}s but this primitive decodes one {:.1}s window; use transcribe_long",
                audio.len() as f32 / 16_000.0,
                self.window_samples() as f32 / 16_000.0
            )));
        }
        let features = self.extract_features(audio)?;
        let bias = self.model.context_bias.clone();
        let bonuses = |emitted: &[usize]| {
            bias.as_ref()
                .map(|b| b.next_token_bonuses(emitted))
                .unwrap_or_default()
        };
        let hits: Vec<Hit> = self.model.decode_window_beam(
            features,
            &opts.policy,
            opts.max_symbols,
            0.0,
            &bonuses,
        )?;
        let pieces: Vec<DecodedPiece> = hits
            .iter()
            .map(|&(id, start, dur)| DecodedPiece {
                token_id: id,
                piece: self.vocab.id_to_text(id).unwrap_or("").to_string(),
                start,
                end: start + dur as f32 * 0.08,
            })
            .collect();
        Ok(assemble_timed_pieces(&pieces, opts.detok))
    }

    /// The one long-form route (padded window, quiet cut, seam stitching).
    pub fn transcribe_long(
        &mut self,
        audio: &[f32],
        opts: &BeamOptions,
        cancel: Option<&AtomicBool>,
        progress: &dyn Fn(&LongFormProgress),
    ) -> Result<TranscriptionResult> {
        let long_opts = LongFormOptions {
            window_samples: self.window_samples(),
            terminal_silence: cfg!(target_os = "windows"),
            seam: opts.seam,
        };
        longform::transcribe_padded_window(audio, &long_opts, cancel, progress, |seg| {
            self.transcribe_window_beam(seg, opts)
        })
    }
}
