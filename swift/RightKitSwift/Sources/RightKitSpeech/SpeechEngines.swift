import AVFoundation
import CoreML
import Foundation

/// Which recognizer produced a transcript. Persist it next to the text so a result is never
/// attributed to an engine that did not run.
public enum SpeechEngineKind: String, Codable, CaseIterable, Sendable {
    case parakeet
    case whisper
    case apple
}

/// A recognizer that turns an audio file into text. Every engine owns its decode path from the file.
public protocol SpeechTranscribing: Sendable {
    var kind: SpeechEngineKind { get }
    func transcribe(fileURL: URL) async throws -> String
}

public extension SpeechTranscribing {
    /// Decode, resample to 16 kHz mono and transcribe samples held in memory.
    func transcribe(samples: [Float]) async throws -> String {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("rightkit-speech-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: url) }
        try AudioSamples.writeWAV(samples, to: url)
        return try await transcribe(fileURL: url)
    }
}

/// 16 kHz mono float PCM helpers shared by every engine.
public enum AudioSamples {
    public static let sampleRate = 16_000

    /// Reads any AVFoundation-readable audio file and converts it to 16 kHz mono Float32.
    public static func loadMono16k(from url: URL) throws -> [Float] {
        try ParakeetCoreMLRuntime.loadMono16kSamples(from: url)
    }

    /// Writes 16 kHz mono 16-bit PCM WAV (the fixture / hand-off format).
    public static func writeWAV(_ samples: [Float], to url: URL) throws {
        guard let format = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(sampleRate), channels: 1, interleaved: true),
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(max(samples.count, 1)))
        else { throw SpeechError.unsupportedAudioFormat }
        buffer.frameLength = AVAudioFrameCount(samples.count)
        if let channel = buffer.int16ChannelData?.pointee {
            for index in samples.indices {
                channel[index] = Int16(max(-1, min(1, samples[index])) * Float(Int16.max))
            }
        }
        let file = try AVAudioFile(forWriting: url, settings: format.settings, commonFormat: .pcmFormatInt16, interleaved: true)
        try file.write(from: buffer)
    }
}

// MARK: - Parakeet

/// On-device Parakeet TDT (v3) transcription through Core ML: MelSpectrogram, AudioEncoder,
/// TextDecoder and Joint stages with TDT beam search, vocabulary biasing and padded-window seams.
/// Based on HeardRight iOS (`Parakeet*.swift`), the most mature copy; ScrapeRight's was a marked
/// temporary copy of it.
public final class ParakeetEngine: SpeechTranscribing, @unchecked Sendable {
    public var kind: SpeechEngineKind { .parakeet }
    public let modelDirectory: URL
    public let options: ParakeetOptions
    private let runtime: ParakeetCoreMLRuntime

    /// Loads an installed model directory (`pipeline.json`, `vocab.txt`, the four `*.mlmodelc` or
    /// `*.mlpackage` stages). Pass `requireReadyMarker: true` to demand a prior `validate(...)`.
    ///
    /// `conditioning` and `detokenization` bind the product's canonical implementations (see
    /// `ConditioningProvider` / `DetokenizationProvider`). Every decode path — file, samples,
    /// blocking, static window and prime — routes through them, and their errors propagate.
    /// Omitted, the built-in Swift conditioning and detokenizer parity port are used.
    public init(
        modelDirectory: URL, options: ParakeetOptions = ParakeetOptions(), requireReadyMarker: Bool = false,
        conditioning: ConditioningProvider? = nil, detokenization: DetokenizationProvider? = nil
    ) throws {
        self.modelDirectory = modelDirectory
        self.options = options
        runtime = requireReadyMarker
            ? try ParakeetCoreMLRuntime.loadInstalled(
                at: modelDirectory, options: options, conditioning: conditioning, detokenization: detokenization)
            : try ParakeetCoreMLRuntime.load(
                at: modelDirectory, options: options, conditioning: conditioning, detokenization: detokenization)
    }

    /// Vocabulary phrases (proper nouns, jargon) to bias the decode toward.
    public var vocabulary: [String] = []

    public func transcribe(fileURL: URL) async throws -> String {
        let phrases = vocabulary
        let runtime = runtime
        return try await Task.detached(priority: .userInitiated) {
            try runtime.transcribe(audioURL: fileURL, vocabulary: phrases)
        }.value
    }

    public func transcribe(samples: [Float]) async throws -> String {
        let phrases = vocabulary
        let runtime = runtime
        return try await Task.detached(priority: .userInitiated) {
            try runtime.transcribe(samples: samples, vocabulary: phrases)
        }.value
    }

    /// Synchronous variant for callers that already own a worker queue.
    public func transcribeBlocking(samples: [Float], vocabulary phrases: [String]? = nil) throws -> String {
        try runtime.transcribe(samples: samples, vocabulary: phrases ?? vocabulary)
    }

    /// Decode one scheduled 15 s window (Static-15 capture scheduling).
    public func transcribeStaticWindow(samples: [Float], alreadyConditioned: Bool = false) throws -> String {
        try runtime.transcribeStaticWindow(samples: samples, vocabulary: vocabulary, alreadyConditioned: alreadyConditioned)
    }

    /// Runs a short silent window through the real decode path so the next decode is hot (ANE
    /// compile and first-inference setup happen at the first prediction, not at load).
    public func prime() throws { try runtime.prime() }

    /// Timings of the most recent decode on this engine.
    public var lastTimings: ParakeetTimings { runtime.timings.get() }

    /// Validates every stage loads and writes the `coreml-ready.json` marker.
    @discardableResult
    public static func validate(modelDirectory: URL) throws -> ParakeetModelReport {
        let report = try CoreMLModelValidator().validateInstalledModel(at: modelDirectory)
        return ParakeetModelReport(
            stages: report.stages, vocabularySize: report.vocabSize,
            windowSeconds: report.pipeline.windowSec, blankID: report.pipeline.blankID, isTDT: report.pipeline.isTDT
        )
    }

    public static func isReady(modelDirectory: URL) -> Bool {
        CoreMLModelValidator.hasReadyMarker(at: modelDirectory)
    }

    /// Detokenizes piece ids with the canonical Rust policy (specials dropped, digit boundaries,
    /// `uh`/`um` fillers stripped).
    public static func detokenize(ids: [Int], pieces: [String]) -> String {
        ParakeetCoreMLRuntime.detokenize(ids: ids, pieces: pieces)
    }
}

public struct ParakeetModelReport: Sendable, Equatable {
    public let stages: [String]
    public let vocabularySize: Int
    public let windowSeconds: Double
    public let blankID: Int
    public let isTDT: Bool
}

// MARK: - Whisper

/// On-device Whisper (multilingual, large-v3-turbo class) through Core ML: 128-bin log-mel,
/// timestamped greedy decode with the HeardRight timestamp rules, and the shared quiet-cut
/// segmentation so both backends place window seams alike.
public final class WhisperEngine: SpeechTranscribing, @unchecked Sendable {
    public var kind: SpeechEngineKind { .whisper }
    private let runtime: WhisperCoreMLRuntime
    /// Admits one Core ML window at a time per engine and drains canceled work before the next.
    private let executor = PredictionExecutor()
    /// `"en"`, any Whisper language code, or `"auto"` to detect on the first segment.
    public var language: String

    /// The AUTO-detect sentinel accepted as `languageToken`; never a real token id.
    public static let autoLanguageToken: Int32 = WhisperCoreMLRuntime.autoLanguage

    /// `conditioning` binds the product's canonical conditioning (see `ConditioningProvider`); every
    /// decode path routes through it unless the caller passes `alreadyConditioned: true`, and its
    /// errors propagate. Omitted, the built-in Swift conditioning is used.
    public init(
        modelDirectory: URL, language: String = "en", computeUnits: MLComputeUnits = .cpuAndNeuralEngine,
        conditioning: ConditioningProvider? = nil
    ) throws {
        runtime = try WhisperCoreMLRuntime.loadInstalled(
            at: modelDirectory, computeUnits: computeUnits, conditioning: conditioning)
        self.language = language
    }

    /// Language code (`"en"`, `"de"`, ...) to decoder token; `"auto"` returns `autoLanguageToken`.
    /// A code the tokenizer does not know resolves to English, as `transcribe` always has.
    public func languageToken(for language: String) -> Int32 {
        runtime.languageToken(language)
    }

    /// Decoder token to language code; `"auto"` for `autoLanguageToken`, `nil` for a non-language token.
    public func languageCode(for token: Int32) -> String? {
        runtime.languageCode(token)
    }

    /// Decodes one Static-15 scheduler window (any length; longer input is cut at quiet points into
    /// 15 s segments). Pass `autoLanguageToken` for the recording's first window and the returned
    /// `resolvedLanguage` for the rest so AUTO detection runs once per recording.
    ///
    /// Cancellation — of the calling task or via `checkCancellation` — is checked before
    /// conditioning, before each mel/encoder, between decoder token predictions and before the
    /// result is published. A Core ML prediction already running cannot be interrupted: the call
    /// waits for it to return before throwing, and the engine admits the next window only after,
    /// so a canceled generation never overlaps the next one.
    public func transcribeStaticWindow(
        samples: [Float], languageToken: Int32,
        alreadyConditioned: Bool,
        checkCancellation: @escaping @Sendable () throws -> Void,
        onPhase: @escaping @Sendable (_ name: String, _ elapsedMS: Int) -> Void
    ) async throws -> WhisperWindowResult {
        let runtime = runtime
        return try await executor.run(checkCancellation: checkCancellation) { check in
            try runtime.transcribeWindow(
                samples: samples, languageToken: languageToken, alreadyConditioned: alreadyConditioned,
                checkCancellation: check, onPhase: onPhase
            )
        }
    }

    public func transcribe(fileURL: URL) async throws -> String {
        let samples = try AudioSamples.loadMono16k(from: fileURL)
        return try await transcribe(samples: samples)
    }

    public func transcribe(samples: [Float]) async throws -> String {
        let runtime = runtime
        let token = runtime.languageToken(language)
        // Same single-flight admission as static windows; whole-file decode keeps its historic
        // run-to-completion semantics.
        return try await executor.run(honorsCancellation: false) { _ in
            try runtime.transcribe(samples: samples, languageToken: token)
        }
    }
}

// MARK: - Engine selection

/// Resolves a requested engine to one that can actually run, falling back only when the requested
/// one cannot, and reports which engine did the work (never silently mis-attributes a transcript).
public struct SpeechEngineSelector: Sendable {
    public var parakeetModelDirectory: URL?
    public var whisperModelDirectory: URL?
    public var parakeetOptions = ParakeetOptions()
    /// Canonical providers handed to every engine this selector builds.
    public var conditioning: ConditioningProvider?
    public var parakeetDetokenization: DetokenizationProvider?

    public init(parakeetModelDirectory: URL? = nil, whisperModelDirectory: URL? = nil) {
        self.parakeetModelDirectory = parakeetModelDirectory
        self.whisperModelDirectory = whisperModelDirectory
    }

    /// The requested engine, or an error naming why it cannot run. Never substitutes another engine.
    public func resolve(_ requested: SpeechEngineKind) throws -> any SpeechTranscribing {
        do { return try make(requested) } catch {
            throw SpeechError.engineUnavailable("\(requested.rawValue): \(error.localizedDescription)")
        }
    }

    /// The requested engine, else the first of Parakeet, Whisper, Apple Speech that can run.
    /// Check `kind` on the result to attribute the transcript.
    public func resolveWithFallback(_ requested: SpeechEngineKind) throws -> any SpeechTranscribing {
        var failures: [String] = []
        for kind in [requested] + SpeechEngineKind.allCases.filter({ $0 != requested }) {
            do { return try make(kind) } catch { failures.append("\(kind.rawValue): \(error.localizedDescription)") }
        }
        throw SpeechError.engineUnavailable(failures.joined(separator: "; "))
    }

    private func make(_ kind: SpeechEngineKind) throws -> any SpeechTranscribing {
        switch kind {
        case .parakeet:
            guard let dir = parakeetModelDirectory else { throw SpeechError.modelNotReady }
            return try ParakeetEngine(
                modelDirectory: dir, options: parakeetOptions,
                conditioning: conditioning, detokenization: parakeetDetokenization)
        case .whisper:
            guard let dir = whisperModelDirectory else { throw SpeechError.modelNotReady }
            return try WhisperEngine(modelDirectory: dir, conditioning: conditioning)
        case .apple:
            return AppleSpeechEngine()
        }
    }
}
