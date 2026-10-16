//! Which engines this build target can host.
//!
//! The ONNX Parakeet path is portable. CoreML (Parakeet and Whisper) is macOS-only: its modules and
//! its `objc2` dependencies sit behind `cfg(target_os = "macos")`, so no Apple code is compiled for
//! Android (or Windows, Linux, iOS). whisper.cpp is refused on Android (see
//! [`EngineSupport::whisper_cpp`]). Nothing here has been compiled for Android.

/// Engine availability for one target. [`EngineSupport::current`] is this build; the pure
/// [`EngineSupport::for_target`] exists so the rule is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineSupport {
    /// Parakeet through ONNX Runtime (TDT, EOU, CTC decoders). Every target.
    pub parakeet_onnx: bool,
    /// Parakeet through CoreML (`parakeet_coreml`). macOS only.
    pub parakeet_coreml: bool,
    /// Whisper through CoreML (`WhisperCoreMl`). macOS with `whisper-tokenizers`.
    pub whisper_coreml: bool,
    /// Whisper through whisper.cpp (`WhisperCpp`). Refused on Android: that build and its ABI are
    /// not verified, and Whisper on Android is deferred.
    pub whisper_cpp: bool,
}

impl EngineSupport {
    /// The rule for a target, from its OS and whether the `whisper-tokenizers` feature is on.
    pub const fn for_target(macos: bool, android: bool, whisper_tokenizers: bool) -> Self {
        Self {
            parakeet_onnx: true,
            parakeet_coreml: macos,
            whisper_coreml: macos && whisper_tokenizers,
            whisper_cpp: !android,
        }
    }

    /// The target this crate was compiled for.
    pub const fn current() -> Self {
        Self::for_target(
            cfg!(target_os = "macos"),
            cfg!(target_os = "android"),
            cfg!(feature = "whisper-tokenizers"),
        )
    }
}

/// The refusal for whisper.cpp on Android, `None` elsewhere. `android` is a parameter so the rule
/// is testable on any host.
pub(crate) fn whisper_cpp_refusal(android: bool) -> Option<crate::Error> {
    android.then(|| {
        crate::Error::unsupported(
            "whisper.cpp is not supported on Android yet (Whisper on Android is deferred; use the Parakeet ONNX engines)",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_hosts_only_the_onnx_parakeet_engine_among_the_apple_and_cpp_paths() {
        let android = EngineSupport::for_target(false, true, true);
        assert!(android.parakeet_onnx);
        assert!(!android.parakeet_coreml && !android.whisper_coreml && !android.whisper_cpp);
        let mac = EngineSupport::for_target(true, false, true);
        assert!(mac.parakeet_coreml && mac.whisper_coreml && mac.whisper_cpp);
        assert!(!EngineSupport::for_target(true, false, false).whisper_coreml);
        let windows = EngineSupport::for_target(false, false, true);
        assert!(windows.parakeet_onnx && windows.whisper_cpp && !windows.parakeet_coreml);
    }

    #[test]
    fn current_matches_the_compile_target_cfgs() {
        let now = EngineSupport::current();
        assert_eq!(now.parakeet_coreml, cfg!(target_os = "macos"));
        assert_eq!(now.whisper_cpp, !cfg!(target_os = "android"));
        assert!(now.parakeet_onnx);
    }

    #[test]
    fn whisper_cpp_refusal_is_unsupported_on_android_only() {
        let error = whisper_cpp_refusal(true).expect("refused on Android");
        assert!(error.is_unsupported());
        assert!(error.to_string().contains("Android"));
        assert!(whisper_cpp_refusal(false).is_none());
    }
}
