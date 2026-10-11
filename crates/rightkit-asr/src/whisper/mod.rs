//! Whisper decoding behind the shared [`Transcriber`] interface.
//!
//! Two native engines, no Python, no subprocess, no CLI fallback:
//! - macOS: [`coreml`] — WhisperKit-format `.mlmodelc` bundle (AudioEncoder +
//!   TextDecoder) plus `tokenizer.json`, run in-process through CoreML.framework
//!   (ANE/GPU/CPU). The one-time ANE compile is not performed by this crate.
//! - all platforms: whisper.cpp via dynamic FFI (`libwhisper.dylib` built with
//!   Metal on macOS, `whisper.dll` built with Vulkan/CUDA on Windows,
//!   `libwhisper.so` on Linux), pinned to ABI 1.9.3.
//!
//! Models and the whisper library are never bundled: callers pass paths
//! (typically resolved through `rightkit-models` manifests).
//!
//! Hosting and dictation policy (when to decode, which language the user
//! wants, model residency) stay in the caller.
//!
//! ## Modes and options (0.2.0)
//!
//! - Language lock: [`Language::Auto`] detects on the first window and locks
//!   that code for the rest of the call (one recording never changes language);
//!   [`Language::Code`] fixes it. [`Language::validated`] rejects codes outside
//!   [`WHISPER_LANGUAGES`].
//! - [`WhisperOptions::detect_language`] runs identification on the first 30 s
//!   and reports the probabilities in [`WhisperTranscript::detection`].
//! - [`DecodeMode::Timed`] (default) keeps segment timings; [`DecodeMode::TextOnly`]
//!   suppresses timestamps and is greedy with no cross-window context.
//! - [`WhisperOptions::token_timestamps`] yields per-word timings in
//!   [`WhisperTranscript::words`]. See the caveats on that field.
//! - Deadline: [`WhisperOptions::window_deadline`] bounds each window's
//!   `whisper_full` call. The caller chooses the value; this crate hard-codes none.

pub(crate) mod cpp;
#[cfg(target_os = "macos")]
pub mod coreml;
pub mod languages;
mod windowing;

use crate::audio::{load_audio, AudioSource};
use crate::decoder::{TimedToken, TranscriptionResult};
use crate::error::{Error, Result};
use crate::timestamps::TimestampMode;
use crate::transcriber::Transcriber;
use rightkit_transcript::TimedWord;
use std::path::PathBuf;
use std::time::Duration;

pub use cpp::WhisperCpp;
#[cfg(target_os = "macos")]
pub use coreml::ComputeUnits;
#[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
pub use coreml::WhisperCoreMl;
pub use languages::{is_whisper_language, validate_language, WHISPER_LANGUAGES};
pub use windowing::WHISPER_WINDOW_SAMPLES;
#[cfg(target_os = "macos")] // only the macOS Parakeet Core ML path reuses it
pub(crate) use windowing::to_mono_16k;

/// Sample rate every Whisper output is timed against.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Recognition language: `Auto` detects on the first window and then locks the
/// result for the rest of the call, so a transcript cannot flip language
/// mid-recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Language {
    Auto,
    /// Whisper language code, e.g. `"en"`, `"ja"`, `"yue"`.
    Code(String),
}

impl Language {
    /// Lenient parse kept from 0.1.0: any non-empty string becomes a code, not
    /// checked against the table. Prefer [`Language::validated`].
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("auto") {
            Language::Auto
        } else {
            Language::Code(s.to_ascii_lowercase())
        }
    }

    /// Strict parse: `""` and `"auto"` mean [`Language::Auto`]; any other value
    /// must be one of the [`WHISPER_LANGUAGES`] codes (case-insensitive).
    pub fn validated(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("auto") {
            Ok(Language::Auto)
        } else {
            Ok(Language::Code(validate_language(s)?.to_string()))
        }
    }
}

/// Which native backend and model files to use.
#[derive(Debug, Clone)]
pub enum WhisperModel {
    /// macOS only: directory with `AudioEncoder.mlmodelc`, `TextDecoder.mlmodelc`
    /// and `tokenizer.json` (WhisperKit layout).
    CoreMlDir { dir: PathBuf },
    /// whisper.cpp: the shared library and a ggml model file.
    Ggml { library: PathBuf, model: PathBuf },
}

/// Output mode of one decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecodeMode {
    /// Segment timestamps on (whisper.cpp default). Word timings need this.
    #[default]
    Timed,
    /// Timestamps suppressed (`no_timestamps`), greedy, no cross-window context.
    /// Text only: segment and word times are not meaningful. Whisper.cpp only.
    TextOnly,
}

#[derive(Debug, Clone)]
pub struct WhisperOptions {
    /// Language lock for the call (see [`Language`]).
    pub language: Language,
    /// whisper.cpp CPU threads (0 = auto, capped at 8).
    pub threads: usize,
    /// Cooperative per-window time bound for whisper.cpp (`None` = unbounded).
    /// The caller picks the value. Example policy (HeardRight's Windows route,
    /// not a crate default): `max(10 s, 6 x audio duration)` per call. Applied
    /// per 30 s window here, so a long recording needs the caller's own bound
    /// per call if it must cap the whole recording.
    pub window_deadline: Option<Duration>,
    /// Timed (default) or text-only decode. Whisper.cpp only for `TextOnly`.
    pub mode: DecodeMode,
    /// Ask whisper.cpp for token timings and assemble per-word times into
    /// [`WhisperTranscript::words`]. Whisper.cpp only; requires `DecodeMode::Timed`.
    ///
    /// Caveats (whisper.cpp token timestamps):
    /// - Times come from Whisper's timestamp tokens plus its attention-based
    ///   heuristic, not from forced alignment. They are approximate, and word
    ///   edges can be off by a noticeable margin. Treat them as navigation
    ///   hints, not as sub-word-accurate boundaries.
    /// - Word boundaries are inferred from the leading space of each BPE token.
    ///   Scripts without spaces (for example Chinese, Japanese, Cantonese) yield
    ///   runs of characters rather than words.
    /// - Token probabilities are model confidences, not calibrated. A word's
    ///   probability is the mean of its token probabilities.
    /// - Special tokens (EOT, timestamp, language, task) are excluded.
    /// - Enabling token timestamps adds decode cost.
    pub token_timestamps: bool,
    /// Maximum characters per segment (whisper.cpp `max_len`). `0` = no limit.
    /// Whisper.cpp only. Segmentation depends on whisper.cpp's token logic.
    pub max_len: u32,
    /// With `max_len > 0`, split on word boundaries rather than tokens
    /// (whisper.cpp `split_on_word`). Whisper.cpp only.
    pub split_on_word: bool,
    /// Identify the language on the first 30 s, lock it for the call, and report
    /// [`WhisperTranscript::detection`]. Requires [`Language::Auto`]. Whisper.cpp
    /// only (the CoreML route returns an error).
    pub detect_language: bool,
    /// CoreML compute units (macOS CoreML backend only).
    #[cfg(target_os = "macos")]
    pub compute_units: ComputeUnits,
}

impl Default for WhisperOptions {
    fn default() -> Self {
        Self {
            language: Language::Auto,
            threads: 0,
            window_deadline: Some(Duration::from_secs(180)),
            mode: DecodeMode::Timed,
            token_timestamps: false,
            max_len: 0,
            split_on_word: false,
            detect_language: false,
            #[cfg(target_os = "macos")]
            compute_units: ComputeUnits::default(),
        }
    }
}

/// Result of a language identification pass.
#[derive(Debug, Clone, PartialEq)]
pub struct LanguageDetection {
    /// Most probable code (whisper.cpp table spelling).
    pub lang: String,
    /// Probability of `lang`.
    pub probability: f32,
    /// Every language in whisper.cpp's table with its probability, most probable first.
    pub candidates: Vec<(String, f32)>,
}

/// One decoded segment. Times are seconds from the start of the audio (`f32`,
/// as in 0.1.0) and the matching sample indices at [`WHISPER_SAMPLE_RATE`].
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperSegment {
    pub text: String,
    pub start: f32,
    pub end: f32,
    pub start_sample: u64,
    pub end_sample: u64,
    pub sample_rate: u32,
}

/// One word with token-derived timing (see [`WhisperOptions::token_timestamps`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperWord {
    /// Millisecond timing and text (`rightkit-transcript` type).
    pub word: TimedWord,
    pub start_sample: u64,
    pub end_sample: u64,
    pub sample_rate: u32,
    /// Mean token probability in `[0, 1]`.
    pub probability: f32,
}

/// A full decode: text, segments, optional words and detection info.
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperTranscript {
    pub text: String,
    /// Language used (locked or detected).
    pub language: String,
    /// Present when [`WhisperOptions::detect_language`] was set.
    pub detection: Option<LanguageDetection>,
    pub segments: Vec<WhisperSegment>,
    /// Empty unless [`WhisperOptions::token_timestamps`] was set.
    pub words: Vec<WhisperWord>,
}

/// Milliseconds for a sample index at `sample_rate`, rounded down, integer
/// arithmetic only (no float round-trip).
pub fn samples_to_ms(samples: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    ((samples as u128 * 1000) / sample_rate as u128) as u64
}

/// Decode options a single window needs (copied per window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodeOptions {
    pub timed: bool,
    pub token_timestamps: bool,
    pub max_len: u32,
    pub split_on_word: bool,
}

/// One window's decode request. `lang` is the language to force (`None` =
/// whisper.cpp auto).
pub(crate) struct WindowRequest<'a> {
    pub lang: Option<&'a str>,
    pub timed: bool,
    pub token_timestamps: bool,
    pub max_len: u32,
    pub split_on_word: bool,
}

/// One decoded segment, window-relative: seconds (`f32`) and samples.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    pub text: String,
    pub start: f32,
    pub end: f32,
    pub start_sample: u64,
    pub end_sample: u64,
}

/// One word, window-relative sample span plus mean token probability.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WordSpan {
    pub text: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub probability: f32,
}

/// What one window decode produced.
#[derive(Debug, Clone, Default)]
pub(crate) struct WindowOutput {
    pub segments: Vec<Segment>,
    pub words: Vec<WordSpan>,
    pub language: String,
}

pub(crate) trait WindowDecoder {
    /// Decode one <=30 s window. Returns window-relative segments and words plus
    /// the language that was used or detected.
    fn decode_window(&mut self, samples: &[f32], req: &WindowRequest<'_>) -> Result<WindowOutput>;
    /// Identify the language from the first 30 s of `samples` without decoding text.
    fn detect_window(&mut self, samples: &[f32]) -> Result<LanguageDetection>;
}

enum Backend {
    #[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
    CoreMl(Box<WhisperCoreMl>),
    Cpp(WhisperCpp),
}

/// Whisper transcriber. Implements [`Transcriber`] exactly like the Parakeet
/// variants; `TranscriptionResult::tokens` holds one entry per Whisper segment
/// (not per word) and the `TimestampMode` argument is accepted for interface
/// parity but not used (Whisper emits segment-level timing only through that
/// trait). Use [`Whisper::transcribe_detailed`] for words, sample positions and
/// detection info.
pub struct Whisper {
    backend: Backend,
    options: WhisperOptions,
    detected: Option<String>,
}

impl Whisper {
    pub fn load(model: WhisperModel, options: WhisperOptions) -> Result<Self> {
        let backend = match model {
            #[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
            WhisperModel::CoreMlDir { dir } => {
                Backend::CoreMl(Box::new(WhisperCoreMl::load(&dir, options.compute_units)?))
            }
            #[cfg(not(all(target_os = "macos", feature = "whisper-tokenizers")))]
            WhisperModel::CoreMlDir { .. } => {
                return Err(Error::Config(
                    "the CoreML Whisper backend needs macOS and the `whisper-tokenizers` feature".into(),
                ))
            }
            WhisperModel::Ggml { library, model } => {
                Backend::Cpp(WhisperCpp::load(&library, &model, options.threads, options.window_deadline)?)
            }
        };
        Ok(Self { backend, options, detected: None })
    }

    pub fn language(&self) -> &Language {
        &self.options.language
    }

    pub fn set_language(&mut self, language: Language) {
        self.options.language = language;
        self.detected = None;
    }

    pub fn options(&self) -> &WhisperOptions {
        &self.options
    }

    /// Language detected by the most recent `Auto` transcription.
    pub fn detected_language(&self) -> Option<&str> {
        self.detected.as_deref()
    }

    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            #[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
            Backend::CoreMl(_) => "whisper-coreml",
            Backend::Cpp(_) => "whisper-cpp",
        }
    }

    /// Language identification only: the first 30 s of `pcm16k` (mono, 16 kHz,
    /// f32) through `whisper_pcm_to_mel` and `whisper_lang_auto_detect`. No text
    /// decode runs. Whisper.cpp backend only; the CoreML backend returns
    /// `Error::Config`. Needs a multilingual model.
    pub fn detect_language(&self, pcm16k: &[f32]) -> Result<LanguageDetection> {
        match &self.backend {
            #[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
            Backend::CoreMl(_) => Err(Error::Config(
                "language detection without decoding is implemented for the whisper.cpp backend only".into(),
            )),
            Backend::Cpp(e) => e.detect_pcm(pcm16k),
        }
    }

    /// Full decode with timings, word timings (when enabled) and detection info.
    /// Accepts WAV (read by this crate) or caller-decoded PCM through
    /// [`AudioSource`].
    pub fn transcribe_detailed(&mut self, source: AudioSource) -> Result<WhisperTranscript> {
        match source {
            AudioSource::Wav(path) => {
                let (samples, spec) = load_audio(path)?;
                self.run(samples, spec.sample_rate, spec.channels)
            }
            AudioSource::Pcm16k(samples) => self.run(samples, WHISPER_SAMPLE_RATE, 1),
        }
    }

    fn run(&mut self, audio: Vec<f32>, sample_rate: u32, channels: u16) -> Result<WhisperTranscript> {
        let audio = windowing::to_mono_16k(audio, sample_rate, channels)?;
        let opts = self.options.clone();
        let timed = opts.mode == DecodeMode::Timed;
        if !timed && opts.token_timestamps {
            return Err(Error::Config("token_timestamps requires DecodeMode::Timed".into()));
        }
        if opts.detect_language && opts.language != Language::Auto {
            return Err(Error::Config(
                "detect_language requires Language::Auto (a fixed language is already locked)".into(),
            ));
        }
        let forced = match &opts.language {
            Language::Auto => None,
            Language::Code(c) => Some(validate_language(c)?.to_string()),
        };
        let decode = DecodeOptions {
            timed,
            token_timestamps: opts.token_timestamps,
            max_len: opts.max_len,
            split_on_word: opts.split_on_word,
        };
        // Off macOS `Backend` has only the whisper.cpp variant.
        #[allow(clippy::infallible_destructuring_match)]
        let decoder: &mut dyn WindowDecoder = match &mut self.backend {
            #[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
            Backend::CoreMl(e) => {
                if !timed || decode.token_timestamps || decode.max_len > 0 || decode.split_on_word {
                    return Err(Error::Config(
                        "text-only decode, token timestamps, max_len and split_on_word are whisper.cpp options; the CoreML backend supports the default timed decode only".into(),
                    ));
                }
                if opts.detect_language {
                    return Err(Error::Config(
                        "detect_language is implemented for the whisper.cpp backend only".into(),
                    ));
                }
                e.as_mut()
            }
            Backend::Cpp(e) => e,
        };
        let detection = if opts.detect_language {
            let head = &audio[..audio.len().min(WHISPER_WINDOW_SAMPLES)];
            Some(decoder.detect_window(head)?)
        } else {
            None
        };
        let forced = detection.as_ref().map(|d| d.lang.clone()).or(forced);
        let out = windowing::transcribe_windowed(decoder, &audio, forced, decode)?;
        let text = out
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let segments = out
            .segments
            .into_iter()
            .map(|s| WhisperSegment {
                text: s.text,
                start: s.start,
                end: s.end,
                start_sample: s.start_sample,
                end_sample: s.end_sample,
                sample_rate: WHISPER_SAMPLE_RATE,
            })
            .collect();
        let words = out
            .words
            .into_iter()
            .map(|w| WhisperWord {
                word: TimedWord::new(
                    w.text,
                    samples_to_ms(w.start_sample, WHISPER_SAMPLE_RATE),
                    samples_to_ms(w.end_sample, WHISPER_SAMPLE_RATE),
                ),
                start_sample: w.start_sample,
                end_sample: w.end_sample,
                sample_rate: WHISPER_SAMPLE_RATE,
                probability: w.probability,
            })
            .collect();
        self.detected = Some(out.language.clone());
        Ok(WhisperTranscript {
            text,
            language: out.language,
            detection,
            segments,
            words,
        })
    }
}

impl Transcriber for Whisper {
    fn transcribe_samples(
        &mut self,
        audio: Vec<f32>,
        sample_rate: u32,
        channels: u16,
        _mode: Option<TimestampMode>,
    ) -> Result<TranscriptionResult> {
        let t = self.run(audio, sample_rate, channels)?;
        Ok(TranscriptionResult {
            text: t.text,
            tokens: t
                .segments
                .into_iter()
                .map(|s| TimedToken { text: s.text, start: s.start, end: s.end })
                .collect(),
        })
    }
}
