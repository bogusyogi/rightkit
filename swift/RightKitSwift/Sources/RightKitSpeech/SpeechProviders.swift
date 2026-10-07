import Foundation

/// Canonical ASR conditioning boundary. Products bind this to their canonical implementation
/// (HeardRight: Rust `heardright_core::audio_conditioning::condition_for_asr` through
/// `heardright_core_condition_for_asr`). The provider owns the policy, including `"raw"`.
///
/// When an engine is given a provider, every transcription path conditions through it exactly
/// once (never when the caller passes `alreadyConditioned: true`), and a thrown error fails the
/// decode: the engine never falls back to the Swift `AudioConditioning` implementation.
public typealias ConditioningProvider = @Sendable (
    _ samples: [Float], _ sampleRate: UInt32, _ policy: String
) throws -> [Float]

/// Canonical Parakeet detokenization boundary. Receives the complete selected SentencePiece stream
/// of one decode (special tokens and fillers included, before any Swift spacing or filler edit);
/// the provider owns spacing, N0 filler removal and special-token policy (HeardRight: Rust
/// `DetokPolicy::canonical_rust()` through `heardright_core_detokenize_json`).
///
/// A thrown error fails the decode: the engine never falls back to the Swift detokenizer.
public typealias DetokenizationProvider = @Sendable (_ pieces: [String]) throws -> String

/// The conditioning policy every RightKit speech path uses unless configured otherwise.
public let defaultASRConditioningPolicy = "asr_simple_gain_hpf"

/// Resolves the conditioning step for one engine: the injected provider when present, else the
/// built-in Swift `AudioConditioning` (the pre-0.3.2 behaviour, kept for existing consumers).
struct ASRConditioner: Sendable {
    let provider: ConditioningProvider?
    let policy: String

    init(provider: ConditioningProvider?, policy: String = defaultASRConditioningPolicy) {
        self.provider = provider
        self.policy = policy
    }

    func condition(_ samples: [Float], sampleRate: UInt32 = 16_000) throws -> [Float] {
        if let provider {
            return try provider(samples, sampleRate, policy)
        }
        return AudioConditioning.conditionForASR(samples, sampleRate: sampleRate, policy: policy)
    }

    /// Honors `alreadyConditioned`: conditioned input is never conditioned a second time.
    func prepare(_ samples: [Float], alreadyConditioned: Bool) throws -> [Float] {
        alreadyConditioned ? samples : try condition(samples)
    }
}
