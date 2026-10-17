//! # rightkit-asr
//!
//! Rust bindings for NVIDIA's Parakeet speech recognition model using ONNX Runtime.
//!
//! Parakeet is a state-of-the-art automatic speech recognition (ASR) model developed by NVIDIA,
//! based on the FastConformer-TDT architecture with 600 million parameters.
//!
//! ## Features
//!
//! - Easy-to-use API for speech-to-text transcription
//! - Support for ONNX format models
//! - 16kHz mono audio input
//! - Punctuation and capitalization included in output
//! - Fast inference using ONNX Runtime
//!
//! ## Quick Start
//!
//! ```ignore
//! use rightkit_asr::{Parakeet, Transcriber, TimestampMode};
//!
//! // Load the model
//! let mut parakeet = Parakeet::from_pretrained(".")?;
//!
//! // Transcribe audio samples (see examples/raw.rs for audio loading)
//! let result = parakeet.transcribe_samples(audio, sample_rate, channels, Some(TimestampMode::Words))?;
//! println!("Transcription: {}", result.text);
//! ```
//!
//! ## Model Requirements
//!
//! Your model directory should contain:
//! - `model.onnx` - The ONNX model file
//! - `model.onnx_data` - External model weights
//! - `config.json` - Model configuration
//! - `preprocessor_config.json` - Audio preprocessing configuration
//! - `tokenizer.json` - Tokenizer vocabulary
//! - `tokenizer_config.json` - Tokenizer configuration
//!
//! ## Audio Requirements
//!
//! - Format: WAV
//! - Sample Rate: 16kHz
//! - Channels: Mono (stereo will be converted automatically)
//! - Bit Depth: 16-bit PCM or 32-bit float

mod audio;
mod config;
mod decoder;
mod decoder_tdt;
mod error;
mod execution;
mod model;
mod model_eou;
mod model_nemotron;
mod model_tdt;
mod model_transducer;
mod onnx_beam;
mod model_unified;
mod nemotron;
pub mod platform;
// HF tokenizer users (CTC, EOU, Cohere, Whisper CoreML) are gated behind the
// `whisper-tokenizers` feature so the Parakeet CoreML / ONNX TDT paths build
// without the tokenizers C dependency.
#[cfg(feature = "whisper-tokenizers")]
mod decoder_ctc;
#[cfg(feature = "whisper-tokenizers")]
mod parakeet;
#[cfg(feature = "whisper-tokenizers")]
mod parakeet_eou;
mod parakeet_tdt;
mod parakeet_unified;
mod transducer;
#[cfg(feature = "sortformer")]
pub mod sortformer;
#[cfg(feature = "multitalker")]
mod model_multitalker;
#[cfg(feature = "multitalker")]
pub mod multitalker;
#[cfg(feature = "cohere")]
mod model_cohere;
#[cfg(feature = "cohere")]
pub mod cohere;
mod timestamps;
mod transcriber;
mod vocab;
pub mod whisper;
pub mod routing;
pub mod chunked;
pub mod conditioning;
pub mod parakeet_core;
pub mod longform;
#[cfg(target_os = "macos")]
pub mod parakeet_coreml;

pub use error::{Error, Result, UNSUPPORTED_PREFIX};
pub use platform::EngineSupport;
pub use execution::{ExecutionProvider, ModelConfig as ExecutionConfig};
#[cfg(feature = "whisper-tokenizers")]
pub use parakeet::Parakeet;
pub use parakeet_tdt::ParakeetTDT;
pub use transducer::{BeamOptions, StrictLayout, Transducer, TransducerOptions};
pub use timestamps::{process_timestamps, TimestampMode};
pub use transcriber::*;
pub use whisper::{
    audio_duration, is_unspaced_language, is_whisper_language, join_segment_text, load_wav_mono_16k,
    recording_deadline, samples_to_ms, validate_language, CancelToken, DecodeMode, DecodeQuality,
    InterruptReason, Language, LanguageDetection, ProgressCallback, ProgressProbe, Whisper,
    WhisperCpp, WhisperInterrupted, WhisperModel, WhisperOptions, WhisperProgress, WhisperSegment,
    WhisperTranscript, WhisperWord, RECORDING_DEADLINE_AUDIO_FACTOR, RECORDING_DEADLINE_BASE,
    UNSPACED_LANGUAGES, WHISPER_LANGUAGES, WHISPER_SAMPLE_RATE,
};
pub use audio::{load_audio, load_wav_mono_16k_head, AudioSource, WavLoadError};
pub use hound::WavSpec;
#[cfg(target_os = "macos")]
pub use whisper::ComputeUnits;
#[cfg(all(target_os = "macos", feature = "whisper-tokenizers"))]
pub use whisper::WhisperCoreMl;
pub use routing::{select_engine, Availability, EngineKind, EngineSelection, RoutingPolicy};
pub use chunked::{transcribe_spans, ChunkResult};
#[cfg(target_os = "macos")]
pub use parakeet_coreml::{CoreMlParakeet, CoreMlParakeetOptions, LongFormTranscript, PipelineMeta};
pub use longform::{
    pad_terminal_silence, samples_to_secs, LongFormOptions, LongFormProgress, PadPolicy, SeamPolicy,
    TimedExports, TimedWord,
};

pub use config::{ModelConfig as ModelConfigJson, PreprocessorConfig};

pub use decoder::{LanguageSource, LanguageTranscription, ReportedLanguage, TimedPiece, TimedToken, TranscriptionResult};
pub use decoder_tdt::LAST_TOKEN_MIN_DURATION_MS;
#[cfg(feature = "whisper-tokenizers")]
pub use decoder_ctc::ParakeetDecoder;
pub use model::ParakeetModel;
pub use model_eou::ParakeetEOUModel;
pub use model_nemotron::{NemotronEncoderCache, NemotronModel, NemotronModelConfig};
pub use model_unified::{ParakeetUnifiedModel, UnifiedModelConfig};
pub use nemotron::{Nemotron, NemotronHandle, SentencePieceVocab};
#[cfg(feature = "whisper-tokenizers")]
pub use parakeet_eou::{ParakeetEOU, ParakeetEOUHandle};
pub use parakeet_unified::{ParakeetUnified, ParakeetUnifiedHandle, UnifiedStreamingConfig};

#[cfg(feature = "multitalker")]
pub use multitalker::{LatencyMode, MultitalkerASR, MultitalkerConfig, SpeakerTranscript, WordTimestamp};

#[cfg(feature = "cohere")]
pub use cohere::CohereASR;
