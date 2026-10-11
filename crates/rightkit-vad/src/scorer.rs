//! Scorer construction with a backend report, and the energy fallback.

use crate::spans::{duration_ms, ensure_finite, score_samples};
use crate::{energy_scores, scores_to_spans, Result, Silero, SileroOptions, Span, VadConfig};
use std::path::Path;

/// The scorer that produced a set of speech scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Silero on the ONNX Runtime CPU provider.
    SileroCpu,
    /// Silero on the ONNX Runtime CoreML provider (`CPUOnly` compute units).
    SileroCoreMl,
    /// Silero on the ONNX Runtime DirectML provider.
    SileroDirectMl,
    /// Model-free energy and zero-crossing pseudo-scores ([`crate::energy_scores`]).
    Energy,
}

impl Backend {
    /// Stable name for logs and diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::SileroCpu => "silero-cpu",
            Self::SileroCoreMl => "silero-coreml",
            Self::SileroDirectMl => "silero-directml",
            Self::Energy => "energy-zcr-fallback",
        }
    }
}

/// What scorer construction produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScorerBackend {
    pub backend: Backend,
    /// `Some` only when [`SileroOptions::energy_fallback`] replaced a failed
    /// Silero load; it says why Silero was not used.
    pub fallback_reason: Option<String>,
}

/// A constructed speech scorer.
///
/// [`SpeechScorer::Silero`] is a [`crate::FrameScorer`], so it works with
/// [`crate::FrameStream`] and [`crate::detect`]. [`SpeechScorer::Energy`] is
/// batch-only: its noise floor is taken over the whole signal, so it cannot
/// score frames one at a time. Use [`SpeechScorer::scores`] or
/// [`SpeechScorer::detect`] for it.
// `Silero` stays inline: one scorer lives per session, and boxing the variant would change the
// public `SpeechScorer::Silero(Silero)` shape, a public boundary this crate does not change.
#[allow(clippy::large_enum_variant)]
pub enum SpeechScorer {
    Silero(Silero),
    Energy,
}

impl SpeechScorer {
    /// Load Silero from an ONNX file. With [`SileroOptions::energy_fallback`],
    /// a failed load returns the energy scorer and the reason.
    pub fn from_path(
        model: impl AsRef<Path>,
        options: SileroOptions,
    ) -> Result<(Self, ScorerBackend)> {
        Self::select(Silero::load_with(model, options), &options)
    }

    /// Load Silero from embedded ONNX bytes. With
    /// [`SileroOptions::energy_fallback`], a failed load returns the energy
    /// scorer and the reason.
    pub fn from_bytes(model: &[u8], options: SileroOptions) -> Result<(Self, ScorerBackend)> {
        Self::select(Silero::from_bytes(model, options), &options)
    }

    fn select(loaded: Result<Silero>, options: &SileroOptions) -> Result<(Self, ScorerBackend)> {
        match loaded {
            Ok(silero) => {
                let backend = silero.backend();
                Ok((
                    Self::Silero(silero),
                    ScorerBackend {
                        backend,
                        fallback_reason: None,
                    },
                ))
            }
            Err(error) if options.energy_fallback => Ok((
                Self::Energy,
                ScorerBackend {
                    backend: Backend::Energy,
                    fallback_reason: Some(format!("silero unavailable: {error}")),
                },
            )),
            Err(error) => Err(error),
        }
    }

    /// The backend this scorer runs on.
    pub fn backend(&self) -> Backend {
        match self {
            Self::Silero(silero) => silero.backend(),
            Self::Energy => Backend::Energy,
        }
    }

    /// One score per 512-sample frame (final partial frame zero-padded).
    pub fn scores(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        match self {
            Self::Silero(silero) => score_samples(silero, samples),
            Self::Energy => {
                ensure_finite(samples)?;
                Ok(energy_scores(samples))
            }
        }
    }

    /// Scores, then spans under `cfg`, as [`crate::detect`] does.
    pub fn detect(&mut self, samples: &[f32], cfg: &VadConfig) -> Result<Vec<Span>> {
        cfg.validate()?;
        let scores = self.scores(samples)?;
        Ok(scores_to_spans(&scores, cfg, duration_ms(samples.len())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, ExecutionProvider};

    const MISSING: &str = "/nonexistent/silero.onnx";

    fn tone() -> Vec<f32> {
        (0..16_000)
            .map(|i| ((i as f32) * 0.05).sin() * 0.3)
            .collect()
    }

    #[test]
    fn default_options_are_cpu_strict() {
        let o = SileroOptions::default();
        assert_eq!(
            (
                o.intra_threads,
                o.inter_threads,
                o.execution_provider,
                o.device_id,
                o.energy_fallback
            ),
            (1, 1, ExecutionProvider::Cpu, None, false)
        );
    }

    #[test]
    fn missing_model_is_an_error_without_fallback() {
        let err = match SpeechScorer::from_path(MISSING, SileroOptions::default()) {
            Ok(_) => panic!("a missing model must not load"),
            Err(err) => err,
        };
        assert!(matches!(err, Error::Model(_)), "{err}");
    }

    #[test]
    fn missing_model_falls_back_to_energy_with_reason() {
        let options = SileroOptions {
            energy_fallback: true,
            ..SileroOptions::default()
        };
        let (mut scorer, report) = SpeechScorer::from_path(MISSING, options).unwrap();
        assert_eq!(report.backend, Backend::Energy);
        assert_eq!(scorer.backend(), Backend::Energy);
        let reason = report
            .fallback_reason
            .expect("a fallback records its reason");
        assert!(reason.contains("model not found"), "{reason}");
        let samples = tone();
        assert_eq!(scorer.scores(&samples).unwrap(), energy_scores(&samples));
    }

    #[test]
    fn unsupported_provider_is_refused_before_any_load() {
        // Windows hosts DirectML but not CoreML; every other host lacks DirectML.
        let other = if cfg!(target_os = "windows") {
            ExecutionProvider::CoreMl
        } else {
            ExecutionProvider::DirectMl
        };
        let strict = SileroOptions {
            execution_provider: other,
            ..SileroOptions::default()
        };
        let err = match SpeechScorer::from_path(MISSING, strict) {
            Ok(_) => panic!("an unavailable provider must be refused"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("not available"), "{err}");

        let lenient = SileroOptions {
            energy_fallback: true,
            ..strict
        };
        let (_, report) = SpeechScorer::from_path(MISSING, lenient).unwrap();
        assert_eq!(report.backend, Backend::Energy);
        assert!(report
            .fallback_reason
            .expect("reason")
            .contains("not available"));
    }

    #[test]
    fn backend_names_are_stable() {
        assert_eq!(Backend::SileroCpu.name(), "silero-cpu");
        assert_eq!(Backend::SileroCoreMl.name(), "silero-coreml");
        assert_eq!(Backend::SileroDirectMl.name(), "silero-directml");
        assert_eq!(Backend::Energy.name(), "energy-zcr-fallback");
    }
}
