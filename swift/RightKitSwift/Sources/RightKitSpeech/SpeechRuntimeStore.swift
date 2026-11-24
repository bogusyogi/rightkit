#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// Decode telemetry that is safe to persist when output is empty. It carries
/// model/runtime work only; transcript text never enters this value.
public struct SpeechDecodeDiagnostics: Sendable {
    public let model: String
    public let sampleCount: Int?
    public let elapsedMS: Int
    public let outputEmpty: Bool?
    public let phaseBreakdown: [String: Int]
    public let shapeNote: String
}

public struct SpeechDecodeTelemetry: Sendable {
    public let waitMS: Int
    public let diagnostics: SpeechDecodeDiagnostics?
}

/// What the runtime store needs to know about a model a host offers: which engine decodes it and the id
/// that names it in diagnostics.
public protocol SpeechRuntimeModelRepresenting: Sendable {
    var runtimeModelID: String { get }
    var engineKind: SpeechEngineKind { get }
}

/// How the host builds engines. The product binds its canonical conditioning and detokenization here.
public struct SpeechRuntimeProviders: Sendable {
    public var makeParakeet: @Sendable (URL) throws -> ParakeetEngine
    /// Builds a Whisper engine with automatic language detection.
    public var makeWhisper: @Sendable (URL) throws -> WhisperEngine
    /// True when the Whisper model directory holds every required file and stage.
    public var isWhisperInstalled: @Sendable (URL) -> Bool

    public init(
        makeParakeet: @escaping @Sendable (URL) throws -> ParakeetEngine,
        makeWhisper: @escaping @Sendable (URL) throws -> WhisperEngine,
        isWhisperInstalled: @escaping @Sendable (URL) -> Bool
    ) {
        self.makeParakeet = makeParakeet
        self.makeWhisper = makeWhisper
        self.isWhisperInstalled = isWhisperInstalled
    }
}

/// Keeps one loaded engine per model directory and one real prime in flight. Decodes wait for an in-flight
/// prime; a model switch or a changed directory invalidates the cached engine and any prime for it.
public actor SpeechRuntimeStore {
    private let providers: SpeechRuntimeProviders

    public init(providers: SpeechRuntimeProviders) {
        self.providers = providers
    }

    private var parakeetRuntime: ParakeetEngine?
    private var parakeetKey: String?
    private var primedParakeetKey: String?
    private var primedWhisperKey: String?
    private var primingKey: String?
    private var primingEngine: SpeechEngineKind?
    private var primingGeneration: UInt64?
    private var nextPrimeGeneration: UInt64 = 0
    private var primingTask: Task<Int, Error>?
    private var lastDecodeDiagnostics: SpeechDecodeDiagnostics?
    /// Duration of the most recent real inference prime, read by the caller
    /// for telemetry. Kept here rather than traced inline so this file stays
    /// free of app-target-only dependencies.
    private var lastPrimeMS: Int?
    /// How long the most recent decode call had to WAIT for an in-flight
    /// prime before it could start, separate from the decode itself. Read
    /// by the caller immediately after decode returns so telemetry can
    /// split `model_wait_ms` out of `decode_ms`.
    private var lastDecodeWaitMS: Int?
    private var whisperRuntime: WhisperEngine?
    private var whisperKey: String?
    /// AUTO language lock for one scheduled recording. Mac's worker carries
    /// this across disjoint Whisper windows; reset alongside recording gen.
    private var whisperStaticLanguageGeneration: UInt64?
    private var whisperStaticLanguageToken: Int32?
    private var currentTask: Task<String, Error>?

    /// Loading the runtime object is NOT warming: the ANE work happens at the
    /// first `prediction(from:)`, so a cached object could satisfy the old warm
    /// while having executed nothing. Start a real silent inference so the
    /// next decode is genuinely hot. Awaiting detached prime keeps warm's
    /// success truthful without holding this actor: a stop arriving during
    /// prime can re-enter the actor & join the same single-flight task.
    public func warm<M: SpeechRuntimeModelRepresenting>(model: M, at modelDirectory: URL) async throws {
        let key = Self.cacheKey(for: modelDirectory)
        switch model.engineKind {
        case .parakeet:
            if primedParakeetKey == key { return }
            try await awaitActivePrime()
            if primedParakeetKey == key { return }
            let box = RuntimeBox(try parakeet(at: modelDirectory))
            try await startAndAwaitPrime(model: model, key: key) {
                try box.value.prime()
            }
        case .whisper:
            if primedWhisperKey == key { return }
            try await awaitActivePrime()
            if primedWhisperKey == key { return }
            let box = WhisperRuntimeBox(try whisper(at: modelDirectory))
            try await startAndAwaitPrime(model: model, key: key) {
                let silence = [Float](repeating: 0, count: 8_000)
                _ = try await box.value.transcribe(samples: silence)
            }
        }
    }

    public func transcribe<M: SpeechRuntimeModelRepresenting>(
        audioURL: URL,
        model: M,
        modelDirectory: URL,
        vocabulary: [String] = []
    ) async throws -> String {
        switch model.engineKind {
        case .parakeet:
            let runtime = try await parakeetRuntimeForDecode(at: modelDirectory)
            return try measureParakeetDecode(runtime: runtime, model: model, sampleCount: nil) {
                try runtime.transcribeBlocking(samples: AudioSamples.loadMono16k(from: audioURL), vocabulary: vocabulary)
            }
        case .whisper:
            // Keep file cancellation & phase diagnostics on the published
            // window API; SDK's whole-file convenience runs to completion.
            return try await transcribeStaticWindow(
                samples: AudioSamples.loadMono16k(from: audioURL), model: model,
                modelDirectory: modelDirectory, vocabulary: vocabulary
            )
        }
    }

    /// Warm-stream decode: transcribe mono-16k samples already held in memory,
    /// bypassing any capture file. This is the path the live recording uses so
    /// there is no `AVAudioFile` read and no CoreAudio finalization race.
    public func transcribe<M: SpeechRuntimeModelRepresenting>(
        samples: [Float],
        model: M,
        modelDirectory: URL,
        vocabulary: [String] = []
    ) async throws -> String {
        switch model.engineKind {
        case .parakeet:
            let runtime = try await parakeetRuntimeForDecode(at: modelDirectory)
            return try measureParakeetDecode(runtime: runtime, model: model, sampleCount: samples.count) {
                try runtime.transcribeBlocking(samples: samples, vocabulary: vocabulary)
            }
        case .whisper:
            return try await transcribeStaticWindow(
                samples: samples, model: model, modelDirectory: modelDirectory, vocabulary: vocabulary
            )
        }
    }

    public func resetWhisperStaticLanguage(generation: UInt64) {
        whisperStaticLanguageGeneration = generation
        whisperStaticLanguageToken = nil
    }

    /// Decode one disjoint Static-15 window for capture scheduler. Both
    /// backends apply production conditioning at each window boundary.
    public func transcribeStaticWindow<M: SpeechRuntimeModelRepresenting>(
        samples: [Float],
        model: M,
        modelDirectory: URL,
        vocabulary: [String] = [],
        alreadyConditioned: Bool = false,
        generation: UInt64? = nil
    ) async throws -> String {
        switch model.engineKind {
        case .parakeet:
            let runtime = try await parakeetRuntimeForDecode(at: modelDirectory)
            runtime.vocabulary = vocabulary
            return try measureParakeetDecode(runtime: runtime, model: model, sampleCount: samples.count) {
                try runtime.transcribeStaticWindow(samples: samples, alreadyConditioned: alreadyConditioned)
            }
        case .whisper:
            // Initialize before the prime await so a cancellation/newer lease
            // can supersede it while loading. Older queued work must never
            // reset the language selected by a newer recording.
            if let generation,
               whisperStaticLanguageGeneration.map({ generation > $0 }) ?? true {
                resetWhisperStaticLanguage(generation: generation)
            }
            lastDecodeWaitMS = try await awaitActivePrime()
            let runtime = try whisper(at: modelDirectory)
            let languageToken: Int32
            if let generation, whisperStaticLanguageGeneration == generation {
                languageToken = whisperStaticLanguageToken ?? WhisperEngine.autoLanguageToken
            } else {
                languageToken = WhisperEngine.autoLanguageToken
            }
            var resolvedLanguage = languageToken
            let phases = SpeechPhaseRecorder()
            let text = try await measureWhisperDecode(model: model, sampleCount: samples.count, phases: phases) {
                let result = try await runtime.transcribeStaticWindow(
                    samples: samples,
                    languageToken: languageToken,
                    alreadyConditioned: alreadyConditioned,
                    checkCancellation: { try Task.checkCancellation() },
                    onPhase: { name, ms in phases.record(name, ms: ms) }
                )
                resolvedLanguage = result.resolvedLanguage
                return result.text
            }
            if let generation, whisperStaticLanguageGeneration == generation {
                whisperStaticLanguageToken = resolvedLanguage
            }
            return text
        }
    }

    /// Run a decode cooperatively so callers can cancel mid-decode via Task.cancel.
    public func transcribeCancellable<M: SpeechRuntimeModelRepresenting>(
        audioURL: URL,
        model: M,
        modelDirectory: URL,
        vocabulary: [String] = []
    ) async throws -> String {
        let task = Task<String, Error> { [self] in
            try await transcribe(audioURL: audioURL, model: model, modelDirectory: modelDirectory, vocabulary: vocabulary)
        }
        currentTask = task
        defer { currentTask = nil }
        return try await withTaskCancellationHandler {
            try await task.value
        } onCancel: {
            task.cancel()
        }
    }

    public func cancelActiveTranscription() {
        currentTask?.cancel()
        currentTask = nil
    }

    public func invalidate(modelDirectory: URL? = nil) {
        guard let modelDirectory else {
            parakeetRuntime = nil
            parakeetKey = nil
            primedParakeetKey = nil
            primedWhisperKey = nil
            nextPrimeGeneration &+= 1
            if primingTask == nil {
                primingKey = nil
                primingGeneration = nil
                primingEngine = nil
            }
            whisperRuntime = nil
            whisperKey = nil
            return
        }
        let key = Self.cacheKey(for: modelDirectory)
        if parakeetKey == key || primingKey == key {
            parakeetRuntime = nil
            parakeetKey = nil
            primedParakeetKey = nil
            primedWhisperKey = nil
            nextPrimeGeneration &+= 1
            if primingTask == nil {
                primingKey = nil
                primingGeneration = nil
                primingEngine = nil
            }
        }
        if whisperKey == Self.cacheKey(for: modelDirectory) {
            whisperRuntime = nil
            whisperKey = nil
            primedWhisperKey = nil
            if primingEngine == .whisper {
                nextPrimeGeneration &+= 1
                if primingTask == nil {
                    primingKey = nil
                    primingGeneration = nil
                    primingEngine = nil
                }
            }
        }
    }

    /// Consume the pending prime duration, so each real prime is traced once.
    public func takeLastPrimeMS() -> Int? {
        defer { lastPrimeMS = nil }
        return lastPrimeMS
    }

    /// Consume the pending decode-wait duration, so each decode's wait is
    /// reported once.
    public func takeLastDecodeWaitMS() -> Int? {
        defer { lastDecodeWaitMS = nil }
        return lastDecodeWaitMS
    }

    public func takeLastDecodeDiagnostics() -> SpeechDecodeDiagnostics? {
        defer { lastDecodeDiagnostics = nil }
        return lastDecodeDiagnostics
    }

    /// Consume wait & decode diagnostics atomically so another decode cannot
    /// mix telemetry from separate attempts between actor hops.
    public func takeLastDecodeTelemetry() -> SpeechDecodeTelemetry {
        let telemetry = SpeechDecodeTelemetry(
            waitMS: lastDecodeWaitMS ?? 0,
            diagnostics: lastDecodeDiagnostics
        )
        lastDecodeWaitMS = nil
        lastDecodeDiagnostics = nil
        return telemetry
    }

    private func measureParakeetDecode<M: SpeechRuntimeModelRepresenting>(
        runtime: ParakeetEngine,
        model: M,
        sampleCount: Int?,
        decode: () throws -> String
    ) rethrows -> String {
        let started = CFAbsoluteTimeGetCurrent()
        var outputEmpty: Bool?
        defer {
            lastDecodeDiagnostics = SpeechDecodeDiagnostics(
                model: model.runtimeModelID,
                sampleCount: sampleCount,
                elapsedMS: Int((CFAbsoluteTimeGetCurrent() - started) * 1_000),
                outputEmpty: outputEmpty,
                phaseBreakdown: Self.phaseBreakdown(runtime.lastTimings),
                shapeNote: ""
            )
        }
        let output = try decode()
        outputEmpty = output.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return output
    }

    private func measureWhisperDecode<M: SpeechRuntimeModelRepresenting>(
        model: M,
        sampleCount: Int?,
        phases: SpeechPhaseRecorder? = nil,
        decode: () async throws -> String
    ) async rethrows -> String {
        let started = CFAbsoluteTimeGetCurrent()
        var outputEmpty: Bool?
        defer {
            lastDecodeDiagnostics = SpeechDecodeDiagnostics(
                model: model.runtimeModelID,
                sampleCount: sampleCount,
                elapsedMS: Int((CFAbsoluteTimeGetCurrent() - started) * 1_000),
                outputEmpty: outputEmpty,
                phaseBreakdown: phases?.snapshot() ?? [:],
                shapeNote: ""
            )
        }
        let output = try await decode()
        outputEmpty = output.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return output
    }

    private static func phaseBreakdown(_ timing: ParakeetTimings) -> [String: Int] {
        ["total_ms": timing.totalMS, "mel_ms": timing.melMS,
         "encoder_ms": timing.encoderMS, "decoder_ms": timing.decodeStepMS,
         "joint_ms": timing.jointMS, "predictor_calls": timing.predictorCalls,
         "joint_calls": timing.jointCalls, "tokens": timing.tokens,
         "bias_applications": timing.biasApplications]
    }

    public static func cacheKey(for modelDirectory: URL) -> String {
        modelDirectory.standardizedFileURL.path
    }

    private func startAndAwaitPrime<M: SpeechRuntimeModelRepresenting>(
        model: M,
        key: String,
        operation: @escaping @Sendable () async throws -> Void
    ) async throws {
        nextPrimeGeneration &+= 1
        let generation = nextPrimeGeneration
        primingKey = key
        primingEngine = model.engineKind
        primingGeneration = generation
        let work = PrimeWork(operation)
        // A cold Action Button start reaches this same path. Keep the retained
        // single-flight work user-initiated so a launch waiter cannot leave a
        // short recording queued behind a utility-priority ANE compile.
        let task = Task.detached(priority: .userInitiated) {
            let started = CFAbsoluteTimeGetCurrent()
            try await work.run()
            return Int((CFAbsoluteTimeGetCurrent() - started) * 1_000)
        }
        primingTask = task
        try await awaitActivePrime()
    }

    /// Wait for any retained prime before touching either model. This keeps
    /// Whisper from competing with an in-flight Parakeet ANE compile, & makes
    /// all callers observe the same prime failure.
    /// Returns how many milliseconds this call actually waited on the
    /// in-flight prime task (0 when nothing was in flight).
    @discardableResult
    private func awaitActivePrime() async throws -> Int {
        guard let task = primingTask,
              let generation = primingGeneration,
              let key = primingKey,
              let engine = primingEngine else { return 0 }
        let waitStarted = CFAbsoluteTimeGetCurrent()
        do {
            let primeMS = try await task.value
            let waitMS = Int((CFAbsoluteTimeGetCurrent() - waitStarted) * 1_000)
            // Other waiters may already have committed this same prime and cleared its task.
            let committed = engine == .whisper ? primedWhisperKey == key : primedParakeetKey == key
            guard nextPrimeGeneration == generation,
                  committed || (primingGeneration == generation && primingKey == key) else {
                clearPrime(generation: generation)
                throw PrimeError.invalidated
            }
            if engine == .whisper {
                primedWhisperKey = key
            } else {
                primedParakeetKey = key
            }
            if !committed { lastPrimeMS = primeMS }
            clearPrime(generation: generation)
            return waitMS
        } catch {
            clearPrime(generation: generation)
            throw error
        }
    }

    private func clearPrime(generation: UInt64) {
        guard primingGeneration == generation else { return }
        primingKey = nil
        primingEngine = nil
        primingGeneration = nil
        primingTask = nil
    }

    private func parakeetRuntimeForDecode(at modelDirectory: URL) async throws -> ParakeetEngine {
        lastDecodeWaitMS = try await awaitActivePrime()
        return try parakeet(at: modelDirectory)
    }

    private func parakeet(at modelDirectory: URL) throws -> ParakeetEngine {
        let key = Self.cacheKey(for: modelDirectory)
        if let parakeetRuntime, parakeetKey == key {
            return parakeetRuntime
        }
        whisperRuntime = nil
        whisperKey = nil
        primedWhisperKey = nil
        let runtime = try providers.makeParakeet(modelDirectory)
        parakeetRuntime = runtime
        parakeetKey = key
        return runtime
    }

    private func whisper(at modelDirectory: URL) throws -> WhisperEngine {
        let key = Self.cacheKey(for: modelDirectory)
        if let whisperRuntime, whisperKey == key {
            return whisperRuntime
        }
        if parakeetRuntime != nil || primingTask != nil {
            // Switching engines invalidates any Parakeet prime. Keep its
            // task retained until completion so a later replacement cannot
            // compete for ANE resources with this stale work.
            parakeetRuntime = nil
            parakeetKey = nil
            primedParakeetKey = nil
            nextPrimeGeneration &+= 1
            if primingTask == nil {
                primingKey = nil
                primingGeneration = nil
                primingEngine = nil
            }
        }
        guard providers.isWhisperInstalled(modelDirectory) else { throw SpeechError.modelNotReady }
        let runtime = try providers.makeWhisper(modelDirectory)
        whisperRuntime = runtime
        whisperKey = key
        return runtime
    }
}

private final class RuntimeBox: @unchecked Sendable {
    let value: ParakeetEngine

    init(_ value: ParakeetEngine) {
        self.value = value
    }
}

private final class WhisperRuntimeBox: @unchecked Sendable {
    let value: WhisperEngine

    init(_ value: WhisperEngine) {
        self.value = value
    }
}

private final class PrimeWork: @unchecked Sendable {
    let run: @Sendable () async throws -> Void

    init(_ run: @escaping @Sendable () async throws -> Void) {
        self.run = run
    }
}

private enum PrimeError: Error {
    case invalidated
}

private final class SpeechPhaseRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Int] = [:]

    func record(_ name: String, ms: Int) {
        lock.lock(); defer { lock.unlock() }
        values["\(name)_ms", default: 0] += ms
    }

    func snapshot() -> [String: Int] {
        lock.lock(); defer { lock.unlock() }
        return values
    }
}
#endif
