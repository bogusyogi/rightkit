import Foundation
import Testing

@testable import RightKitSpeech

private let hasParakeetModel = Fixtures.parakeetModel != nil
private let hasWhisperSpeech = Fixtures.whisperModel != nil && Fixtures.hasSay

/// Real Core ML engines with injected canonical providers: every production entry point must route
/// through them exactly once, honor `alreadyConditioned`, and propagate provider failure.
@Suite(.serialized)
struct ParakeetCanonicalProviderTests {
    @Test(.enabled(if: hasParakeetModel))
    func everyParakeetEntryPointRoutesThroughInjectedProviders() async throws {
        let probe = ProviderProbe()
        var options = ParakeetOptions()
        options.conditioningPolicy = "asr_simple_gain_hpf"
        let engine = try ParakeetEngine(
            modelDirectory: try #require(Fixtures.parakeetModel), options: options,
            conditioning: probe.conditioning, detokenization: probe.detokenization
        )
        let silence = [Float](repeating: 0, count: 16_000)

        let text = try await engine.transcribe(samples: silence)
        #expect(probe.conditioningCount == 1)
        #expect(probe.detokenizationStreams.count == 1)
        #expect(text.hasPrefix("canonical:"), "detokenized text comes from the provider: \(text)")

        _ = try engine.transcribeStaticWindow(samples: silence, alreadyConditioned: true)
        #expect(probe.conditioningCount == 1, "already-conditioned window is not conditioned again")
        #expect(probe.detokenizationStreams.count == 2)

        _ = try engine.transcribeStaticWindow(samples: silence)
        #expect(probe.conditioningCount == 2)

        _ = try engine.transcribeBlocking(samples: silence)
        #expect(probe.conditioningCount == 3)

        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let wav = dir.appendingPathComponent("silence.wav")
        try AudioSamples.writeWAV(silence, to: wav)
        _ = try await engine.transcribe(fileURL: wav)
        #expect(probe.conditioningCount == 4)

        try engine.prime()
        #expect(probe.conditioningCount == 5)
        #expect(Set(probe.conditioningPolicies) == ["asr_simple_gain_hpf"])
        #expect(Set(probe.conditioningRates) == [16_000])
        #expect(probe.detokenizationStreams.count == 6)

        probe.failConditioning = true
        await #expect(throws: ProviderFailure.self) { try await engine.transcribe(samples: silence) }
        #expect(throws: ProviderFailure.self) { try engine.transcribeStaticWindow(samples: silence) }
        probe.failConditioning = false
        probe.failDetokenization = true
        await #expect(throws: ProviderFailure.self) { try await engine.transcribe(samples: silence) }
        #expect(throws: ProviderFailure.self) { try engine.transcribeStaticWindow(samples: silence, alreadyConditioned: true) }
    }
}

@Suite(.serialized)
struct WhisperStaticWindowEngineTests {
    @Test(.enabled(if: hasWhisperSpeech))
    func staticWindowReturnsTextLanguageAndPhaseTimingsThroughProvider() async throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let audio = try AudioSamples.loadMono16k(from: try Fixtures.synthesize(
            "The quick brown fox jumps over the lazy dog near the old stone bridge.", in: dir))
        let probe = ProviderProbe()
        probe.useSwiftConditioning = true
        let engine = try WhisperEngine(modelDirectory: try #require(Fixtures.whisperModel), conditioning: probe.conditioning)

        #expect(engine.languageToken(for: "auto") == WhisperEngine.autoLanguageToken)
        #expect(engine.languageCode(for: WhisperEngine.autoLanguageToken) == "auto")
        let english = engine.languageToken(for: "en")
        #expect(engine.languageCode(for: english) == "en")
        #expect(engine.languageCode(for: engine.languageToken(for: "de")) == "de")
        #expect(engine.languageCode(for: 0) == nil)

        let phases = EventLog()
        let result = try await engine.transcribeStaticWindow(
            samples: audio, languageToken: WhisperEngine.autoLanguageToken, alreadyConditioned: false,
            checkCancellation: {}, onPhase: { name, _ in phases.append(name) }
        )
        let words = Fixtures.words(result.text)
        for expected in ["quick", "brown", "fox", "lazy", "dog", "bridge"] {
            #expect(words.contains(expected), "missing \(expected) in \(result.text)")
        }
        #expect(result.resolvedLanguage == english, "AUTO resolves to the detected language token")
        #expect(phases.all == ["conditioning", "mel", "encoder", "token_decode"])
        for key in ["conditioning_ms", "mel_ms", "encoder_ms", "token_decode_ms", "total_ms"] {
            #expect(result.phaseBreakdownMS[key] != nil, "missing \(key)")
        }
        #expect(probe.conditioningCount == 1)

        // Locked language on a later window, already conditioned: no second conditioning.
        let locked = try await engine.transcribeStaticWindow(
            samples: AudioConditioning.conditionForASR(audio), languageToken: result.resolvedLanguage,
            alreadyConditioned: true, checkCancellation: {}, onPhase: { _, _ in }
        )
        #expect(locked.resolvedLanguage == english)
        #expect(probe.conditioningCount == 1)

        _ = try await engine.transcribe(samples: audio)
        #expect(probe.conditioningCount == 2, "whole-utterance decode routes through the provider")

        probe.failConditioning = true
        await #expect(throws: ProviderFailure.self) {
            try await engine.transcribeStaticWindow(
                samples: audio, languageToken: english, alreadyConditioned: false,
                checkCancellation: {}, onPhase: { _, _ in })
        }
        await #expect(throws: ProviderFailure.self) { try await engine.transcribe(samples: audio) }
    }

    @Test(.enabled(if: hasWhisperSpeech))
    func canceledWindowStopsBetweenTokensAndNextWindowRuns() async throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let audio = try AudioSamples.loadMono16k(from: try Fixtures.synthesize(
            "Please schedule the quarterly budget meeting for Thursday afternoon.", in: dir))
        let engine = try WhisperEngine(modelDirectory: try #require(Fixtures.whisperModel))
        let english = engine.languageToken(for: "en")

        // Caller-side cancellation: trip after the encoder, i.e. between decoder token predictions.
        let log = EventLog()
        let encoded = CancellationFlag()
        await #expect(throws: CancellationError.self) {
            try await engine.transcribeStaticWindow(
                samples: audio, languageToken: english, alreadyConditioned: false,
                checkCancellation: { if encoded.isSet { throw CancellationError() } },
                onPhase: { name, _ in
                    log.append(name)
                    if name == WhisperWindowPhase.encoder { encoded.set() }
                }
            )
        }
        #expect(log.all == ["conditioning", "mel", "encoder"], "token decode never completed")

        // Outer-task cancellation once the encoder is running: the call drains, then throws.
        let started = AsyncGate()
        let window = Task {
            try await engine.transcribeStaticWindow(
                samples: audio, languageToken: english, alreadyConditioned: false,
                checkCancellation: {}, onPhase: { name, _ in if name == WhisperWindowPhase.mel { started.open() } }
            )
        }
        await started.wait()
        window.cancel()
        await #expect(throws: CancellationError.self) { try await window.value }

        // The engine admits the next generation and decodes it normally.
        let next = try await engine.transcribeStaticWindow(
            samples: audio, languageToken: english, alreadyConditioned: false,
            checkCancellation: {}, onPhase: { _, _ in }
        )
        #expect(Fixtures.words(next.text).contains("budget"), "next window decodes: \(next.text)")
    }
}
