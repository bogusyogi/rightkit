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
//!
//! ## Caller control (additive)
//!
//! - Cancel: [`WhisperOptions::cancel`] ([`CancelToken`]) is polled inside
//!   whisper.cpp's abort callback and between windows.
//! - Progress: [`WhisperOptions::progress`] ([`ProgressCallback`]) reports
//!   [`WhisperProgress`] per window and from whisper.cpp's own within-window callback.
//!   [`WhisperOptions::progress_probe`] ([`ProgressProbe`]) gives the same figures as
//!   atomics to poll from any thread, with no callback bound.
//! - Whole-recording bound: [`WhisperOptions::total_deadline`], with the old
//!   CLI-route policy as [`recording_deadline`]. Language detection takes
//!   [`WhisperOptions::detect_deadline`] and the same cancel token.
//! - A cancel or deadline returns [`Error::Interrupted`] carrying the windows
//!   that completed as a partial [`WhisperTranscript`].
//! - Decode quality: [`WhisperOptions::quality`] ([`DecodeQuality`]): beam size,
//!   best-of and context carry. The default keeps the 0.2.0 greedy decode.
//! - Text: [`WhisperSegment::raw_text`] / [`WhisperTranscript::raw_text`] keep
//!   whisper.cpp's untrimmed text; `text` joins without inserted spaces for
//!   [`UNSPACED_LANGUAGES`].
//! - [`load_wav_mono_16k`] / [`load_audio`](crate::load_audio) read WAV input.

pub(crate) mod control;
pub(crate) mod cpp;
#[cfg(target_os = "macos")]
pub mod coreml;
pub mod languages;
mod windowing;

use crate::audio::{load_audio, AudioSource};
use control::{check_interrupt, deadline_after, earliest, interrupted_error};
use crate::decoder::{TimedToken, TranscriptionResult};
use crate::error::{Error, Result};
use crate::timestamps::TimestampMode;
use crate::transcriber::Transcriber;
use rightkit_transcript::TimedWord;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use cpp::WhisperCpp;
#[cfg(target_os = "macos")]
pub use coreml::ComputeUnits;
#[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
pub use coreml::WhisperCoreMl;
pub use control::{
    audio_duration, recording_deadline, CancelToken, InterruptReason, ProgressCallback,
    ProgressProbe, WhisperInterrupted, WhisperProgress, RECORDING_DEADLINE_AUDIO_FACTOR, RECORDING_DEADLINE_BASE,
};
pub use languages::{
    is_unspaced_language, is_whisper_language, join_segment_text, validate_language,
    UNSPACED_LANGUAGES, WHISPER_LANGUAGES,
};
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
    /// per 30 s window, measured from that window's start. Use
    /// [`total_deadline`](Self::total_deadline) to cap the whole recording.
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
    /// Caller cancellation (default `None`). Tripping it stops an in-flight
    /// whisper.cpp window promptly (polled in the abort callback) and any run
    /// between windows, for every backend. A cancelled run returns
    /// [`Error::Interrupted`] with [`InterruptReason::Cancelled`] and the
    /// completed windows as the partial result ([`Error::is_cancelled`] is true).
    /// Mid-window stop is whisper.cpp only; CoreML stops at the next window boundary.
    pub cancel: Option<CancelToken>,
    /// Progress callback (default `None`); see [`WhisperProgress`]. Invoked on the
    /// decoding thread only while a run is in progress, never after it returns.
    /// The within-window events come from whisper.cpp's own progress callback
    /// (whisper.cpp backend only; CoreML reports once per window).
    pub progress: Option<ProgressCallback>,
    /// Polled progress (default `None`): the same figures as [`progress`](Self::progress),
    /// read from any thread through [`ProgressProbe`] with no callback. Both may be set.
    pub progress_probe: Option<ProgressProbe>,
    /// Whole-recording time bound, started when the run begins (default `None`:
    /// only the per-window bound applies). On expiry the run returns
    /// [`Error::Interrupted`] with [`InterruptReason::DeadlineExceeded`] and the
    /// windows that finished. [`recording_deadline`] gives the old CLI route's
    /// policy (120 s plus 4x the audio duration). The bound also caps each window.
    pub total_deadline: Option<Duration>,
    /// Time bound for language identification (default `None`, as before). It is
    /// cooperative: whisper.cpp 1.9.3 offers no abort hook for
    /// `whisper_pcm_to_mel` / `whisper_lang_auto_detect`, so the cancel token and
    /// this deadline are checked before, between and after the two calls, and a
    /// result that finishes past the deadline is reported as exceeded.
    pub detect_deadline: Option<Duration>,
    /// Beam size, best-of and context carry (default: the 0.2.0 greedy decode).
    pub quality: DecodeQuality,
}

/// Decode-quality knobs for whisper.cpp. The default (beam 1, best-of 1, no
/// context carry) is the crate's long-standing behaviour. The CoreML backend
/// accepts the default only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeQuality {
    /// Beam width. `1` = greedy sampling; above `1` switches whisper.cpp to beam search.
    pub beam_size: u32,
    /// Candidates kept by greedy sampling (whisper.cpp `greedy.best_of`).
    pub best_of: u32,
    /// Condition each window after the first on the text of the previous ones
    /// (`no_context = false` for windows 2..). The first window always starts
    /// clean, so state never leaks between recordings. Off by default: windows
    /// are decoded independently.
    pub carry_context: bool,
}

impl Default for DecodeQuality {
    fn default() -> Self {
        Self { beam_size: 1, best_of: 1, carry_context: false }
    }
}

impl DecodeQuality {
    /// whisper.cpp builds at most this many decoders; larger beam or best-of
    /// values are rejected (`WHISPER_MAX_DECODERS`, src/whisper.cpp:148, :7066).
    pub const MAX_CANDIDATES: u32 = 8;

    /// The decode whisper-cli runs when no `-bs`, `-bo` or `-mc` flag is given.
    ///
    /// Read from the whisper.cpp sources checked out at
    /// `genright/.cache/devin-completion-20261006/batch5/pipelines/vendor/whisper.cpp`
    /// (reports itself as 1.9.4; this crate pins the 1.9.3 ABI, so re-check on a
    /// version bump). Paths below are relative to that directory:
    /// - `examples/cli/cli.cpp:44-45`: the CLI's `best_of` / `beam_size` defaults
    ///   are the library's per-strategy defaults, and `cli.cpp:1227` picks beam
    ///   search whenever `beam_size > 1`.
    /// - `src/whisper.cpp:6181`: greedy `best_of` default is `5`.
    /// - `src/whisper.cpp:6187`: beam search `beam_size` default is `5`.
    /// - `src/whisper.cpp:6099`: `no_context` defaults to `true`, and the CLI
    ///   never changes it. That is per `whisper_full` call (`src/whisper.cpp:7095`
    ///   clears the history only at the start of a call); the CLI makes a single
    ///   call over the whole recording, so the history still accumulates across
    ///   its internal 30 s steps. This crate calls `whisper_full` once per window,
    ///   so the equivalent is `carry_context: true`.
    ///
    /// The CLI's temperature fallback and thresholds (`cli.cpp:49-54`) equal the
    /// `whisper_full_default_params` values this crate starts from.
    pub const fn whisper_cli() -> Self {
        Self { beam_size: 5, best_of: 5, carry_context: true }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        for (name, v) in [("beam_size", self.beam_size), ("best_of", self.best_of)] {
            if v == 0 || v > Self::MAX_CANDIDATES {
                return Err(Error::Config(format!(
                    "{name} must be 1..={} (got {v})",
                    Self::MAX_CANDIDATES
                )));
            }
        }
        Ok(())
    }
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
            cancel: None,
            progress: None,
            progress_probe: None,
            total_deadline: None,
            detect_deadline: None,
            quality: DecodeQuality::default(),
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
    /// Trimmed text (as in 0.2.0).
    pub text: String,
    /// whisper.cpp's segment text exactly as returned, with its leading space
    /// and any trailing whitespace.
    pub raw_text: String,
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
    /// Trimmed segment texts joined with one space, or with nothing for
    /// [`UNSPACED_LANGUAGES`] (languages written without spaces).
    pub text: String,
    /// The segments' [`raw_text`](WhisperSegment::raw_text) concatenated as is.
    pub raw_text: String,
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
    pub beam_size: u32,
    pub best_of: u32,
    pub carry_context: bool,
}

/// One window's decode request. `lang` is the language to force (`None` =
/// whisper.cpp auto).
pub(crate) struct WindowRequest<'a> {
    pub lang: Option<&'a str>,
    pub timed: bool,
    pub token_timestamps: bool,
    pub max_len: u32,
    pub split_on_word: bool,
    pub beam_size: u32,
    pub best_of: u32,
    pub carry_context: bool,
    /// First window of the run: never conditioned on earlier text.
    pub first_window: bool,
    /// Caller cancel flag, polled from whisper.cpp's abort callback.
    pub cancel: Option<&'a CancelToken>,
    /// Absolute stop time for this window (window bound capped by the whole-recording bound).
    pub deadline: Option<Instant>,
    /// Within-window progress as a fraction of this window in `[0, 1]`.
    pub progress: Option<&'a (dyn Fn(f32) + Sync)>,
}

/// One decoded segment, window-relative: seconds (`f32`) and samples.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    pub text: String,
    pub raw_text: String,
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
    /// [`detect_window`](Self::detect_window) with the caller's cancel token and
    /// an absolute deadline. The default only checks them before starting;
    /// whisper.cpp overrides it with checks around its two native calls.
    fn detect_window_controlled(
        &mut self,
        samples: &[f32],
        cancel: Option<&CancelToken>,
        deadline: Option<Instant>,
    ) -> Result<LanguageDetection> {
        if let Some(reason) = check_interrupt(cancel, deadline) {
            return Err(interrupted_error(reason));
        }
        self.detect_window(samples)
    }
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
                return Err(Error::unsupported(
                    "the CoreML Whisper backend needs macOS and the `whisper-tokenizers` feature",
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

    /// Replace the cancel token for later calls (a resident model serves many
    /// recordings, each with its own token).
    pub fn set_cancel(&mut self, cancel: Option<CancelToken>) {
        self.options.cancel = cancel;
    }

    /// Replace the progress callback for later calls.
    pub fn set_progress(&mut self, progress: Option<ProgressCallback>) {
        self.options.progress = progress;
    }

    /// Replace the polled progress probe for later calls.
    pub fn set_progress_probe(&mut self, probe: Option<ProgressProbe>) {
        self.options.progress_probe = probe;
    }

    /// Replace the whole-recording bound for later calls (see
    /// [`WhisperOptions::total_deadline`], [`recording_deadline`]).
    pub fn set_total_deadline(&mut self, deadline: Option<Duration>) {
        self.options.total_deadline = deadline;
    }

    /// Replace the language-detection bound for later calls.
    pub fn set_detect_deadline(&mut self, deadline: Option<Duration>) {
        self.options.detect_deadline = deadline;
    }

    /// Replace the decode-quality knobs for later calls; validated when a run starts.
    pub fn set_quality(&mut self, quality: DecodeQuality) {
        self.options.quality = quality;
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
    ///
    /// Honors [`WhisperOptions::cancel`] and [`WhisperOptions::detect_deadline`]
    /// (cooperatively; see the latter) and returns [`Error::Interrupted`] when
    /// either hits.
    pub fn detect_language(&self, pcm16k: &[f32]) -> Result<LanguageDetection> {
        match &self.backend {
            #[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
            Backend::CoreMl(_) => Err(Error::Config(
                "language detection without decoding is implemented for the whisper.cpp backend only".into(),
            )),
            Backend::Cpp(e) => {
                let deadline = deadline_after(Instant::now(), self.options.detect_deadline);
                e.detect_pcm_controlled(pcm16k, self.options.cancel.as_ref(), deadline)
            }
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
        let run_start = Instant::now();
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
        opts.quality.validate()?;
        let forced = match &opts.language {
            Language::Auto => None,
            Language::Code(c) => Some(validate_language(c)?.to_string()),
        };
        let decode = DecodeOptions {
            timed,
            token_timestamps: opts.token_timestamps,
            max_len: opts.max_len,
            split_on_word: opts.split_on_word,
            beam_size: opts.quality.beam_size,
            best_of: opts.quality.best_of,
            carry_context: opts.quality.carry_context,
        };
        let total_deadline = deadline_after(run_start, opts.total_deadline);
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
                if opts.quality != DecodeQuality::default() {
                    return Err(Error::Config(
                        "beam size, best-of and context carry are whisper.cpp options; the CoreML backend supports the default decode only".into(),
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
            let deadline = earliest(deadline_after(run_start, opts.detect_deadline), total_deadline);
            Some(decoder.detect_window_controlled(head, opts.cancel.as_ref(), deadline)?)
        } else {
            None
        };
        let forced = detection.as_ref().map(|d| d.lang.clone()).or(forced);
        let ctl = windowing::RunControl {
            cancel: opts.cancel.as_ref(),
            total_deadline,
            window_deadline: opts.window_deadline,
            progress: opts.progress.as_ref(),
            probe: opts.progress_probe.as_ref(),
        };
        let out = match windowing::transcribe_windowed(decoder, &audio, forced, decode, &ctl) {
            Ok(out) => out,
            Err(windowing::Windowed::Failed(e)) => return Err(e),
            Err(windowing::Windowed::Interrupted { reason, partial, windows_completed, windows_estimated }) => {
                return Err(Error::Interrupted(Box::new(WhisperInterrupted {
                    reason,
                    partial: assemble_transcript(partial, detection),
                    windows_completed,
                    windows_estimated,
                })));
            }
        };
        self.detected = Some(out.language.clone());
        Ok(assemble_transcript(out, detection))
    }
}

/// Turn the windowed output into the public transcript. Shared by the complete
/// and the partial (interrupted) paths so both are assembled identically.
pub(crate) fn assemble_transcript(out: WindowOutput, detection: Option<LanguageDetection>) -> WhisperTranscript {
    let text = join_segment_text(out.segments.iter().map(|s| s.text.as_str()), &out.language);
    let raw_text = out.segments.iter().map(|s| s.raw_text.as_str()).collect::<String>();
    let segments = out
        .segments
        .into_iter()
        .map(|s| WhisperSegment {
            text: s.text,
            raw_text: s.raw_text,
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
    WhisperTranscript { text, raw_text, language: out.language, detection, segments, words }
}

/// Read a WAV file into mono 16 kHz `f32` samples ready for
/// [`AudioSource::Pcm16k`]: 16-bit PCM or 32-bit float input, multi-channel
/// input averaged to mono. The file must already be 16 kHz (this crate does not
/// resample); anything else is an `Error::Audio`. Use it to hold the samples
/// yourself (for example to size [`recording_deadline`] with [`audio_duration`]
/// before decoding).
pub fn load_wav_mono_16k<P: AsRef<Path>>(path: P) -> Result<Vec<f32>> {
    let (samples, spec) = load_audio(path)?;
    windowing::to_mono_16k(samples, spec.sample_rate, spec.channels)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, raw: &str, start_sample: u64) -> Segment {
        Segment {
            text: text.into(),
            raw_text: raw.into(),
            start: start_sample as f32 / 16_000.0,
            end: (start_sample + 160) as f32 / 16_000.0,
            start_sample,
            end_sample: start_sample + 160,
        }
    }

    fn out(language: &str, segs: Vec<Segment>) -> WindowOutput {
        WindowOutput { segments: segs, words: vec![], language: language.into() }
    }

    #[test]
    fn spaced_languages_join_with_one_space_and_keep_raw_text() {
        let t = assemble_transcript(
            out("en", vec![seg("Hello there.", " Hello there. ", 0), seg("Bye.", " Bye.", 320)]),
            None,
        );
        assert_eq!(t.text, "Hello there. Bye.", "unchanged from 0.2.0");
        assert_eq!(t.raw_text, " Hello there.  Bye.", "raw segment text concatenated as returned");
        assert_eq!(t.segments[0].text, "Hello there.");
        assert_eq!(t.segments[0].raw_text, " Hello there. ");
        assert_eq!(t.language, "en");
    }

    #[test]
    fn unspaced_languages_join_without_inserted_spaces() {
        for lang in ["ja", "zh", "yue", "th", "lo", "km"] {
            let t = assemble_transcript(out(lang, vec![seg("あ", "あ", 0), seg("い", "い", 160)]), None);
            assert_eq!(t.text, "あい", "{lang}");
            assert_eq!(t.raw_text, "あい");
        }
    }

    #[test]
    fn partial_transcript_is_assembled_like_a_full_one() {
        let partial = out("", vec![seg("one", " one", 0)]);
        let t = assemble_transcript(partial, None);
        assert_eq!(t.text, "one");
        assert_eq!(t.language, "");
        assert_eq!(t.segments.len(), 1);
        let e = Error::Interrupted(Box::new(WhisperInterrupted {
            reason: InterruptReason::DeadlineExceeded,
            partial: t.clone(),
            windows_completed: 1,
            windows_estimated: 3,
        }));
        assert!(!e.is_cancelled());
        assert_eq!(e.interrupted().unwrap().partial, t);
        assert!(e.to_string().contains("deadline exceeded after 1 of about 3"), "{e}");
        let c = Error::Interrupted(Box::new(WhisperInterrupted::empty(InterruptReason::Cancelled)));
        assert!(c.is_cancelled());
        assert!(Error::Cancelled.is_cancelled());
        assert!(!Error::Config("x".into()).is_cancelled());
        assert!(Error::Config("x".into()).interrupted().is_none());
    }

    #[test]
    fn option_defaults_keep_existing_values_and_add_nothing_active() {
        let o = WhisperOptions::default();
        assert!(o.cancel.is_none() && o.progress.is_none() && o.progress_probe.is_none());
        assert_eq!(o.total_deadline, None);
        assert_eq!(o.detect_deadline, None);
        assert_eq!(o.window_deadline, Some(Duration::from_secs(180)));
        assert_eq!(o.quality, DecodeQuality { beam_size: 1, best_of: 1, carry_context: false });
        assert_eq!(DecodeQuality::default(), o.quality);
    }

    #[test]
    fn quality_validation_and_cli_preset() {
        assert!(DecodeQuality::default().validate().is_ok());
        assert!(DecodeQuality::whisper_cli().validate().is_ok());
        assert_eq!(DecodeQuality::whisper_cli(), DecodeQuality { beam_size: 5, best_of: 5, carry_context: true });
        let zero = DecodeQuality { beam_size: 0, ..DecodeQuality::default() };
        assert!(matches!(zero.validate(), Err(Error::Config(_))));
        let big = DecodeQuality { best_of: DecodeQuality::MAX_CANDIDATES + 1, ..DecodeQuality::default() };
        assert!(matches!(big.validate(), Err(Error::Config(_))));
        assert!(DecodeQuality { beam_size: 8, best_of: 8, carry_context: false }.validate().is_ok());
    }

    #[test]
    fn options_with_callbacks_are_clone_and_debug() {
        let o = WhisperOptions {
            cancel: Some(CancelToken::new()),
            progress: Some(ProgressCallback::new(|_| {})),
            ..WhisperOptions::default()
        };
        let c = o.clone();
        assert!(format!("{c:?}").contains("ProgressCallback"));
    }

    #[test]
    fn default_detect_window_controlled_checks_before_starting() {
        struct D(usize);
        impl WindowDecoder for D {
            fn decode_window(&mut self, _: &[f32], _: &WindowRequest<'_>) -> Result<WindowOutput> {
                unreachable!()
            }
            fn detect_window(&mut self, _: &[f32]) -> Result<LanguageDetection> {
                self.0 += 1;
                Ok(LanguageDetection { lang: "en".into(), probability: 1.0, candidates: vec![] })
            }
        }
        let mut d = D(0);
        let token = CancelToken::new();
        token.cancel();
        let err = d.detect_window_controlled(&[0.0], Some(&token), None).unwrap_err();
        assert!(err.is_cancelled());
        assert_eq!(d.0, 0, "no native work after a cancel");
        let err = d.detect_window_controlled(&[0.0], None, Some(Instant::now())).unwrap_err();
        assert_eq!(err.interrupted().unwrap().reason, InterruptReason::DeadlineExceeded);
        assert!(d.detect_window_controlled(&[0.0], None, None).is_ok());
        assert_eq!(d.0, 1);
    }
}
