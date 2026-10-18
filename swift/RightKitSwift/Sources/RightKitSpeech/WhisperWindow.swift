#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// One decoded Whisper static window.
public struct WhisperWindowResult: Sendable, Equatable {
    /// Window transcript after the shared seam rule and N0 filler strip.
    public let text: String
    /// Language token the window decoded with. For an AUTO request this is the language detected on
    /// the window's first segment; pass it back as `languageToken` for the recording's remaining
    /// windows to lock the language.
    public let resolvedLanguage: Int32
    /// Accumulated phase timings in milliseconds, keyed `<phase>_ms` (see `WhisperWindowPhase`)
    /// plus `total_ms` and `segments`.
    public let phaseBreakdownMS: [String: Int]

    public init(text: String, resolvedLanguage: Int32, phaseBreakdownMS: [String: Int]) {
        self.text = text
        self.resolvedLanguage = resolvedLanguage
        self.phaseBreakdownMS = phaseBreakdownMS
    }
}

/// Phase names reported through `onPhase` and, suffixed `_ms`, in `phaseBreakdownMS`.
public enum WhisperWindowPhase {
    public static let conditioning = "conditioning"
    public static let mel = "mel"
    public static let encoder = "encoder"
    public static let tokenDecode = "token_decode"
}

/// The model-facing steps of one window. Production is `WhisperCoreMLRuntime`; tests substitute a
/// fake so ordering, cancellation and timing are verified without Core ML.
protocol WhisperWindowStages {
    associatedtype Encoded
    func condition(_ samples: [Float], alreadyConditioned: Bool) throws -> [Float]
    func logMel(_ segment: [Float]) throws -> [Float]
    func encode(_ mel: [Float]) throws -> Encoded
    func decodeTokens(
        _ encoded: Encoded, languageToken: Int32, checkCancellation: () throws -> Void
    ) throws -> (tokens: [Int], resolvedLanguage: Int32)
    func text(_ tokens: [Int]) throws -> String
}

enum WhisperWindowPipeline {
    /// Condition once, cut into Static-15 quiet-cut segments, then mel/encoder/token-decode each.
    /// Cancellation is checked before conditioning, before each segment's mel and encoder, between
    /// token predictions (inside `decodeTokens`), and before the result is published.
    static func run<Stages: WhisperWindowStages>(
        stages: Stages,
        samples: [Float],
        languageToken: Int32,
        alreadyConditioned: Bool,
        checkCancellation: () throws -> Void,
        onPhase: (String, Int) -> Void
    ) throws -> WhisperWindowResult {
        let started = DispatchTime.now().uptimeNanoseconds
        var breakdown: [String: Int] = [:]
        func timed<T>(_ phase: String, _ body: () throws -> T) rethrows -> T {
            let begin = DispatchTime.now().uptimeNanoseconds
            let value = try body()
            let ms = Int((DispatchTime.now().uptimeNanoseconds - begin) / 1_000_000)
            breakdown["\(phase)_ms", default: 0] += ms
            onPhase(phase, ms)
            return value
        }

        try checkCancellation()
        let conditioned = try timed(WhisperWindowPhase.conditioning) {
            try stages.condition(samples, alreadyConditioned: alreadyConditioned)
        }
        var text = ""
        var activeLanguage = languageToken
        var segments = 0
        for range in WhisperCoreMLRuntime.windowRanges(samples: conditioned) {
            try checkCancellation()
            let segment = range.isEmpty ? [] : Array(conditioned[range])
            let mel = try timed(WhisperWindowPhase.mel) { try stages.logMel(segment) }
            try checkCancellation()
            let encoded = try timed(WhisperWindowPhase.encoder) { try stages.encode(mel) }
            let decoded = try timed(WhisperWindowPhase.tokenDecode) { () throws -> (String, Int32) in
                let result = try stages.decodeTokens(encoded, languageToken: activeLanguage, checkCancellation: checkCancellation)
                return (try stages.text(result.tokens), result.resolvedLanguage)
            }
            segments += 1
            // Shared seam rule instead of a blind space: de-duplicates repeated
            // boundary wording and still separates disjoint windows.
            ParakeetCoreMLRuntime.appendWithOverlap(buffer: &text, next: decoded.0)
            if languageToken == WhisperCoreMLRuntime.autoLanguage, activeLanguage == WhisperCoreMLRuntime.autoLanguage {
                // Lock the first segment's detected language for the rest of the window.
                activeLanguage = decoded.1
            }
        }
        try checkCancellation()
        breakdown["segments"] = segments
        breakdown["total_ms"] = Int((DispatchTime.now().uptimeNanoseconds - started) / 1_000_000)
        // N0. Whisper has its own tokenizer, so it never passes through the Parakeet piece-level
        // strip; same two words, same whole-word rule as every other path.
        return WhisperWindowResult(
            text: ParakeetCoreMLRuntime.stripMandatoryFillers(text: text)
                .trimmingCharacters(in: .whitespacesAndNewlines),
            resolvedLanguage: activeLanguage,
            phaseBreakdownMS: breakdown
        )
    }
}

/// FIFO admission for one engine's Core ML work: at most one window runs at a time, in call order.
/// Acquisition is deliberately not cancellable — a canceled waiter is admitted, sees its
/// cancellation at the first check, and releases — so a release is never lost.
actor SerialPredictionGate {
    private var busy = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func acquire() async {
        guard busy else {
            busy = true
            return
        }
        await withCheckedContinuation { waiters.append($0) }
    }

    func release() {
        if waiters.isEmpty {
            busy = false
        } else {
            waiters.removeFirst().resume()
        }
    }
}

final class CancellationFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    var isSet: Bool { lock.withLock { value } }
    func set() { lock.withLock { value = true } }
}

/// Runs synchronous, non-interruptible model work off the caller's executor. Canceling the caller
/// trips the work's cancellation check, then waits for the in-flight prediction to return before
/// the call completes and before the gate admits the next window: two generations never overlap.
final class PredictionExecutor: Sendable {
    let gate = SerialPredictionGate()

    func run<T: Sendable>(
        priority: TaskPriority = .userInitiated,
        honorsCancellation: Bool = true,
        checkCancellation: @escaping @Sendable () throws -> Void = {},
        work: sending @escaping (_ check: () throws -> Void) throws -> T
    ) async throws -> T {
        await gate.acquire()
        let flag = CancellationFlag()
        let task = Task.detached(priority: priority) { () throws -> T in
            try work {
                if honorsCancellation, flag.isSet || Task.isCancelled { throw CancellationError() }
                try checkCancellation()
            }
        }
        let result = await withTaskCancellationHandler {
            await task.result
        } onCancel: {
            guard honorsCancellation else { return }
            flag.set()
            task.cancel()
        }
        await gate.release()
        if honorsCancellation { try Task.checkCancellation() }
        return try result.get()
    }
}
#endif
