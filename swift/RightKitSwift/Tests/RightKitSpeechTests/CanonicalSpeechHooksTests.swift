import Foundation
import Testing

@testable import RightKitSpeech

struct ProviderFailure: Error, Equatable {}

/// Records every provider call; optionally fails.
final class ProviderProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var conditioningCalls: [(count: Int, rate: UInt32, policy: String)] = []
    private var detokCalls: [[String]] = []
    var failConditioning = false
    var failDetokenization = false
    /// Real-speech tests delegate to the Swift implementation while still counting calls.
    var useSwiftConditioning = false

    var conditioningCount: Int { lock.withLock { conditioningCalls.count } }
    var conditioningPolicies: [String] { lock.withLock { conditioningCalls.map(\.policy) } }
    var conditioningRates: [UInt32] { lock.withLock { conditioningCalls.map(\.rate) } }
    var detokenizationStreams: [[String]] { lock.withLock { detokCalls } }

    var conditioning: ConditioningProvider {
        { [self] samples, rate, policy in
            lock.withLock { conditioningCalls.append((samples.count, rate, policy)) }
            if lock.withLock({ failConditioning }) { throw ProviderFailure() }
            if useSwiftConditioning {
                return AudioConditioning.conditionForASR(samples, sampleRate: rate, policy: policy)
            }
            // Marker transform so tests can tell provider output from Swift conditioning.
            return samples.map { $0 * 0.5 }
        }
    }

    var detokenization: DetokenizationProvider {
        { [self] pieces in
            lock.withLock { detokCalls.append(pieces) }
            if lock.withLock({ failDetokenization }) { throw ProviderFailure() }
            return "  canonical:\(pieces.joined(separator: "|"))  "
        }
    }
}

// MARK: - Provider routing (no model)

@Suite struct CanonicalProviderRoutingTests {
    @Test func conditionerRoutesThroughProviderWithPolicyAndRate() throws {
        let probe = ProviderProbe()
        let conditioner = ASRConditioner(provider: probe.conditioning, policy: "custom_policy")
        #expect(try conditioner.condition([1, 2], sampleRate: 16_000) == [0.5, 1])
        #expect(probe.conditioningPolicies == ["custom_policy"])
        #expect(probe.conditioningRates == [16_000])
    }

    @Test func alreadyConditionedInputIsNeverConditionedAgain() throws {
        let probe = ProviderProbe()
        let conditioner = ASRConditioner(provider: probe.conditioning)
        #expect(try conditioner.prepare([0.25, -0.25], alreadyConditioned: true) == [0.25, -0.25])
        #expect(probe.conditioningCount == 0)
        _ = try conditioner.prepare([0.25], alreadyConditioned: false)
        #expect(probe.conditioningCount == 1)
        #expect(probe.conditioningPolicies == [defaultASRConditioningPolicy])
    }

    @Test func conditioningFailurePropagatesWithoutSwiftFallback() {
        let probe = ProviderProbe()
        probe.failConditioning = true
        let conditioner = ASRConditioner(provider: probe.conditioning)
        #expect(throws: ProviderFailure.self) { try conditioner.prepare([0.1], alreadyConditioned: false) }
    }

    @Test func noProviderKeepsBuiltInSwiftConditioning() throws {
        let samples = (0..<1_600).map { Float(sin(Double($0) * 0.2)) * 0.001 }
        #expect(try ASRConditioner(provider: nil).condition(samples) == AudioConditioning.conditionForASR(samples))
    }

    @Test func detokenizerReceivesCompleteSelectedPieceStreamBeforeSwiftEdits() throws {
        let probe = ProviderProbe()
        let pieces = ["<pad>", "\u{2581}Uh", ",", "\u{2581}at", "60", "\u{2581}hello"]
        // Specials, fillers and digit pieces arrive untouched and in decode order; out-of-range
        // ids are not pieces and are dropped.
        let text = try ParakeetCoreMLRuntime.detokenize(ids: [0, 1, 2, 3, 4, 99, 5], pieces: pieces, provider: probe.detokenization)
        #expect(probe.detokenizationStreams == [["<pad>", "\u{2581}Uh", ",", "\u{2581}at", "60", "\u{2581}hello"]])
        #expect(text == "canonical:<pad>|\u{2581}Uh|,|\u{2581}at|60|\u{2581}hello", "provider output is trimmed only")
    }

    @Test func detokenizationFailurePropagatesWithoutSwiftFallback() {
        let probe = ProviderProbe()
        probe.failDetokenization = true
        #expect(throws: ProviderFailure.self) {
            try ParakeetCoreMLRuntime.detokenize(ids: [0], pieces: ["\u{2581}hi"], provider: probe.detokenization)
        }
    }

    @Test func noDetokenizerKeepsSwiftParityPort() throws {
        let pieces = ["<pad>", "\u{2581}Uh", ",", "\u{2581}at", "60"]
        #expect(try ParakeetCoreMLRuntime.detokenize(ids: [0, 1, 2, 3, 4], pieces: pieces, provider: nil) == "at 60")
    }
}

// MARK: - Whisper window pipeline (fake stages)

private final class FakeWhisperStages: WhisperWindowStages, @unchecked Sendable {
    let log: EventLog
    let tokensPerSegment: Int
    let detected: Int32
    private(set) var decodeLanguages: [Int32] = []
    private(set) var conditionedFlags: [Bool] = []

    init(log: EventLog, tokensPerSegment: Int = 3, detected: Int32 = 50_261) {
        self.log = log
        self.tokensPerSegment = tokensPerSegment
        self.detected = detected
    }

    func condition(_ samples: [Float], alreadyConditioned: Bool) throws -> [Float] {
        log.append("condition")
        conditionedFlags.append(alreadyConditioned)
        return samples
    }
    func logMel(_ segment: [Float]) throws -> [Float] { log.append("mel"); return [Float(segment.count)] }
    func encode(_ mel: [Float]) throws -> Int { log.append("encode"); return Int(mel[0]) }
    func decodeTokens(
        _ encoded: Int, languageToken: Int32, checkCancellation: () throws -> Void
    ) throws -> (tokens: [Int], resolvedLanguage: Int32) {
        decodeLanguages.append(languageToken)
        var tokens: [Int] = []
        for index in 0..<tokensPerSegment {
            if index > 0 { try checkCancellation() }
            log.append("predict")
            tokens.append(encoded)
        }
        return (tokens, languageToken == WhisperCoreMLRuntime.autoLanguage ? detected : languageToken)
    }
    func text(_ tokens: [Int]) throws -> String {
        log.append("text")
        return "segment\(decodeLanguages.count)"
    }
}

private final class CheckCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0
    let failAt: Int?
    let log: EventLog
    init(log: EventLog, failAt: Int? = nil) { self.log = log; self.failAt = failAt }
    var count: Int { lock.withLock { calls } }
    func check() throws {
        let call = lock.withLock { () -> Int in calls += 1; return calls }
        log.append("check")
        if let failAt, call >= failAt { throw CancellationError() }
    }
}

@Suite struct WhisperWindowPipelineTests {
    @Test func checksCancellationAtEveryRequiredPointInOrder() throws {
        let log = EventLog()
        let stages = FakeWhisperStages(log: log)
        let checks = CheckCounter(log: log)
        var phases: [String] = []
        let result = try WhisperWindowPipeline.run(
            stages: stages, samples: [Float](repeating: 0.1, count: 16_000), languageToken: 50_259,
            alreadyConditioned: false, checkCancellation: checks.check, onPhase: { name, _ in phases.append(name) }
        )
        #expect(log.all == [
            "check", "condition",          // before conditioning
            "check", "mel", "check", "encode", // before mel and encoder
            "predict", "check", "predict", "check", "predict", "text", // between token predictions
            "check",                        // before result publication
        ])
        #expect(phases == ["conditioning", "mel", "encoder", "token_decode"])
        #expect(Set(result.phaseBreakdownMS.keys) == ["conditioning_ms", "mel_ms", "encoder_ms", "token_decode_ms", "total_ms", "segments"])
        #expect(result.phaseBreakdownMS["segments"] == 1)
        #expect(result.text == "segment1")
        #expect(result.resolvedLanguage == 50_259)
        #expect(stages.conditionedFlags == [false])
    }

    @Test func cancellationBetweenTokenPredictionsPublishesNothing() {
        let log = EventLog()
        let checks = CheckCounter(log: log, failAt: 5) // conditioning, mel, encoder, token 1, token 2
        #expect(throws: CancellationError.self) {
            try WhisperWindowPipeline.run(
                stages: FakeWhisperStages(log: log), samples: [0.1], languageToken: 50_259,
                alreadyConditioned: true, checkCancellation: checks.check, onPhase: { _, _ in }
            )
        }
        #expect(log.count("predict") == 2)
        #expect(log.count("text") == 0)
    }

    @Test func cancellationBeforeConditioningRunsNoStage() {
        let log = EventLog()
        #expect(throws: CancellationError.self) {
            try WhisperWindowPipeline.run(
                stages: FakeWhisperStages(log: log), samples: [0.1], languageToken: 50_259,
                alreadyConditioned: false, checkCancellation: CheckCounter(log: log, failAt: 1).check, onPhase: { _, _ in }
            )
        }
        #expect(log.all == ["check"])
    }

    @Test func autoLanguageResolvesOnFirstSegmentAndLocksTheRest() throws {
        let log = EventLog()
        let stages = FakeWhisperStages(log: log, detected: 50_261)
        var phases: [String] = []
        // 20 s: two Static-15 segments.
        let result = try WhisperWindowPipeline.run(
            stages: stages, samples: [Float](repeating: 0.1, count: 320_000),
            languageToken: WhisperCoreMLRuntime.autoLanguage, alreadyConditioned: true,
            checkCancellation: {}, onPhase: { name, _ in phases.append(name) }
        )
        #expect(stages.decodeLanguages == [WhisperCoreMLRuntime.autoLanguage, 50_261])
        #expect(result.resolvedLanguage == 50_261)
        #expect(result.phaseBreakdownMS["segments"] == 2)
        #expect(phases == ["conditioning", "mel", "encoder", "token_decode", "mel", "encoder", "token_decode"])
        #expect(result.text == "segment1 segment2")
        #expect(stages.conditionedFlags == [true])
    }
}

// MARK: - Cancellation drain and single-flight admission

@Suite struct PredictionExecutorTests {
    @Test func canceledWindowDrainsActivePredictionBeforeNextGenerationStarts() async throws {
        let executor = PredictionExecutor()
        let log = EventLog()
        let prediction = DispatchSemaphore(value: 0)

        let first = Task {
            do {
                _ = try await executor.run { check in
                    log.append("w1-predict-start")
                    prediction.wait() // non-interruptible Core ML prediction
                    log.append("w1-predict-end")
                    try check()
                    log.append("w1-published")
                }
                log.append("w1-returned-value")
            } catch is CancellationError {
                log.append("w1-threw-cancel")
            }
        }
        try await log.waitFor("w1-predict-start")
        first.cancel()
        let second = Task {
            try await executor.run { _ in log.append("w2-start") }
        }
        try await Task.sleep(nanoseconds: 30_000_000)
        #expect(log.index("w2-start") == nil, "next generation waits for the active prediction")
        #expect(log.index("w1-threw-cancel") == nil, "cancel waits for the prediction to drain")

        prediction.signal()
        try await first.value
        try await second.value
        let order = log.all
        #expect(order == ["w1-predict-start", "w1-predict-end", "w1-threw-cancel", "w2-start"]
            || order == ["w1-predict-start", "w1-predict-end", "w2-start", "w1-threw-cancel"])
        #expect(log.index("w1-published") == nil)
        #expect(try #require(log.index("w1-predict-end")) < (try #require(log.index("w2-start"))))
    }

    @Test func callerCancellationCheckPropagates() async throws {
        let executor = PredictionExecutor()
        await #expect(throws: ProviderFailure.self) {
            try await executor.run(checkCancellation: { throw ProviderFailure() }) { check in try check() }
        }
        // The gate was released: the next call is admitted.
        #expect(try await executor.run { _ in 42 } == 42)
    }

    @Test func runToCompletionModeIgnoresCallerCancellation() async throws {
        let executor = PredictionExecutor()
        let started = AsyncGate()
        let prediction = DispatchSemaphore(value: 0)
        let task = Task {
            try await executor.run(honorsCancellation: false) { check -> String in
                started.open()
                prediction.wait()
                try check()
                return "complete"
            }
        }
        await started.wait()
        task.cancel()
        prediction.signal()
        #expect(try await task.value == "complete")
    }

    @Test func concurrentWindowsAreAdmittedOneAtATimeInOrder() async throws {
        let executor = PredictionExecutor()
        let log = EventLog()
        let gate = DispatchSemaphore(value: 0)
        let first = Task { try await executor.run { _ in log.append("a-start"); gate.wait(); log.append("a-end") } }
        try await log.waitFor("a-start")
        let second = Task { try await executor.run { _ in log.append("b-start"); log.append("b-end") } }
        try await Task.sleep(nanoseconds: 20_000_000)
        gate.signal()
        try await first.value
        try await second.value
        #expect(log.all == ["a-start", "a-end", "b-start", "b-end"])
    }
}
