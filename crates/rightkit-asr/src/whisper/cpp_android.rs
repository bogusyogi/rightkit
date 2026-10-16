//! whisper.cpp on Android: absent.
//!
//! The real binding is `cpp.rs` (dynamic FFI over `libloading`, pinned to whisper.cpp 1.9.3). It is
//! not compiled for `target_os = "android"`, and neither is the `libloading` dependency (see
//! `Cargo.toml`). This stub keeps the public [`WhisperCpp`] name and signature so downstream code
//! and `WhisperModel::Ggml` still type-check; `load` always returns the typed
//! `Error::unsupported` refusal, so no value of the type can exist. Whisper on Android is deferred.

use super::control::CancelToken;
use super::{LanguageDetection, WindowDecoder, WindowOutput, WindowRequest};
use crate::error::{Error, Result};
use std::convert::Infallible;
use std::path::Path;
use std::time::{Duration, Instant};

/// Placeholder for the whisper.cpp context. Uninhabited: [`WhisperCpp::load`] never returns `Ok`.
pub struct WhisperCpp {
    never: Infallible,
}

impl WhisperCpp {
    /// Always `Err(Error::unsupported(..))` (`Error::is_unsupported()` is true).
    pub fn load(
        _library: &Path,
        _model: &Path,
        _threads: usize,
        _deadline: Option<Duration>,
    ) -> Result<Self> {
        Err(crate::platform::whisper_cpp_refusal(true).unwrap_or_else(|| {
            Error::unsupported("whisper.cpp is not supported on Android")
        }))
    }

    pub(crate) fn detect_pcm_controlled(
        &self,
        _samples: &[f32],
        _cancel: Option<&CancelToken>,
        _deadline: Option<Instant>,
    ) -> Result<LanguageDetection> {
        match self.never {}
    }
}

impl WindowDecoder for WhisperCpp {
    fn decode_window(
        &mut self,
        _samples: &[f32],
        _req: &WindowRequest<'_>,
    ) -> Result<WindowOutput> {
        match self.never {}
    }

    fn detect_window(&mut self, _samples: &[f32]) -> Result<LanguageDetection> {
        match self.never {}
    }
}
