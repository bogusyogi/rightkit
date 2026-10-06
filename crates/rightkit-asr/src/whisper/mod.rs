//! Whisper decoding behind the shared [`Transcriber`] interface.
//!
//! Two native engines, no Python, no subprocess:
//! - macOS: [`coreml`] — WhisperKit-format `.mlmodelc` bundle (AudioEncoder +
//!   TextDecoder) run in-process through CoreML.framework (ANE/GPU/CPU).
//! - all platforms: [`cpp`] — whisper.cpp via dynamic FFI (`libwhisper.dylib`
//!   built with Metal on macOS, `whisper.dll` built with Vulkan/CUDA on
//!   Windows, `libwhisper.so` on Linux), pinned to ABI 1.9.3.
//!
//! Models and the whisper library are never bundled: callers pass paths
//! (typically resolved through `rightkit-models` manifests).
//!
//! Hosting and dictation policy (when to decode, which language the user
//! wants, model residency) stay in the caller.

pub(crate) mod cpp;
#[cfg(target_os = "macos")]
pub mod coreml;
mod windowing;

use crate::decoder::{TimedToken, TranscriptionResult};
use crate::error::Result;
use crate::timestamps::TimestampMode;
use crate::transcriber::Transcriber;
use std::path::PathBuf;
use std::time::Duration;

pub use cpp::WhisperCpp;
#[cfg(target_os = "macos")]
pub use coreml::{ComputeUnits, WhisperCoreMl};
pub use windowing::WHISPER_WINDOW_SAMPLES;
pub(crate) use windowing::to_mono_16k;

/// Recognition language: `Auto` detects on the first window and then locks the
/// result for the rest of the call, so a transcript cannot flip language
/// mid-recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Language {
    Auto,
    /// Whisper language code, e.g. `"en"`, `"ja"`.
    Code(String),
}

impl Language {
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("auto") {
            Language::Auto
        } else {
            Language::Code(s.to_ascii_lowercase())
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

#[derive(Debug, Clone)]
pub struct WhisperOptions {
    pub language: Language,
    /// whisper.cpp CPU threads (0 = auto, capped at 8).
    pub threads: usize,
    /// Cooperative per-window time bound for whisper.cpp (`None` = unbounded).
    pub window_deadline: Option<Duration>,
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
            #[cfg(target_os = "macos")]
            compute_units: ComputeUnits::default(),
        }
    }
}

/// One decoded segment, timed in seconds from the start of the audio.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

pub(crate) trait WindowDecoder {
    /// Decode one <=30 s window. `lang` is the language to force (`None` =
    /// detect). Returns segments relative to the window plus the language that
    /// was used or detected.
    fn decode_window(&mut self, samples: &[f32], lang: Option<&str>) -> Result<(Vec<Segment>, String)>;
}

enum Backend {
    #[cfg(target_os = "macos")]
    CoreMl(Box<WhisperCoreMl>),
    Cpp(WhisperCpp),
}

/// Whisper transcriber. Implements [`Transcriber`] exactly like the Parakeet
/// variants; `TranscriptionResult::tokens` holds one entry per Whisper segment
/// (not per word) and the `TimestampMode` argument is accepted for interface
/// parity but not used (Whisper emits segment-level timing only).
pub struct Whisper {
    backend: Backend,
    options: WhisperOptions,
    detected: Option<String>,
}

impl Whisper {
    pub fn load(model: WhisperModel, options: WhisperOptions) -> Result<Self> {
        let backend = match model {
            #[cfg(target_os = "macos")]
            WhisperModel::CoreMlDir { dir } => {
                Backend::CoreMl(Box::new(WhisperCoreMl::load(&dir, options.compute_units)?))
            }
            #[cfg(not(target_os = "macos"))]
            WhisperModel::CoreMlDir { .. } => {
                return Err(crate::error::Error::Config("the CoreML Whisper backend is macOS-only".into()))
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

    /// Language detected by the most recent `Auto` transcription.
    pub fn detected_language(&self) -> Option<&str> {
        self.detected.as_deref()
    }

    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            #[cfg(target_os = "macos")]
            Backend::CoreMl(_) => "whisper-coreml",
            Backend::Cpp(_) => "whisper-cpp",
        }
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
        let audio = windowing::to_mono_16k(audio, sample_rate, channels)?;
        let forced = match &self.options.language {
            Language::Auto => None,
            Language::Code(c) => Some(c.clone()),
        };
        let decoder: &mut dyn WindowDecoder = match &mut self.backend {
            #[cfg(target_os = "macos")]
            Backend::CoreMl(e) => e.as_mut(),
            Backend::Cpp(e) => e,
        };
        let (segments, lang) = windowing::transcribe_windowed(decoder, &audio, forced)?;
        self.detected = Some(lang);
        let text = segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(TranscriptionResult {
            text,
            tokens: segments
                .into_iter()
                .map(|s| TimedToken { text: s.text, start: s.start, end: s.end })
                .collect(),
        })
    }
}
