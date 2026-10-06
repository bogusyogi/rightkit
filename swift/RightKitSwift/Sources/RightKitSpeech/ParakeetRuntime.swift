import AVFoundation
import CoreML
import Foundation

/// Tunables for the Parakeet Core ML decode. Values are the production-tuned HeardRight defaults;
/// the old environment-variable overrides (`HR_ASR_BEAM_WIDTH`, ...) are explicit fields here so a
/// library never reads process state.
public struct ParakeetOptions: Sendable, Equatable {
    /// Clamped to `1...8`. Width 1 is bit-identical to greedy decode.
    public var beamWidth = 2
    public var emitReward: Float = parakeetDefaultEmitReward
    public var predictorCacheEnabled = true
    /// Score added to the next token of a user-vocabulary phrase being matched.
    public var biasBoost: Float = 1.15
    /// `asr_simple_gain_hpf` (production) or `raw` (input is already conditioned).
    public var conditioningPolicy = "asr_simple_gain_hpf"
    /// CPU + Neural Engine is the measured production route; `.all` orphans an ANE bundle per load.
    public var computeUnits: MLComputeUnits = .cpuAndNeuralEngine
    public var maxSymbolsPerFrame = 10
    public init() {}
}

/// Per-decode phase timings in milliseconds, for diagnostics and benchmarks.
public struct ParakeetTimings: Sendable, Equatable {
    public var totalMS = 0, melMS = 0, encoderMS = 0, decodeStepMS = 0, jointMS = 0
    /// Number of Joint steps that received at least one contextual vocabulary bonus.
    public var biasApplications = 0
    public var predictorCalls = 0, jointCalls = 0, tokens = 0
}

struct ParakeetCoreMLRuntime {
    static let sampleRate = 16_000
    /// TDT frame period: 160-sample hop x8 subsample at 16 kHz.
    static let frameSeconds: Float = 0.08
    /// Trailing region searched for the quiet cut. Shared with the Whisper runtime so both backends
    /// place window boundaries identically.
    static let paddedWindowPaddingMs = 2_240
    static let paddedWindowSilenceMs = 200
    static let paddedWindowSilenceHopMs = 100
    static let paddedWindowOverlapChars = 16

    let pipeline: ParakeetPipelineConfig
    let vocabulary: ParakeetVocabulary
    let contract: ParakeetCoreMLContract
    let options: ParakeetOptions

    let mel: ParakeetCoreMLStage
    let encoder: ParakeetCoreMLStage
    let decoder: ParakeetCoreMLStage
    let joint: ParakeetCoreMLStage
    let timings: TimingsBox

    final class TimingsBox: @unchecked Sendable {
        private let lock = NSLock()
        private var value = ParakeetTimings()
        func set(_ timings: ParakeetTimings) { lock.withLock { value = timings } }
        func get() -> ParakeetTimings { lock.withLock { value } }
    }

    static func loadInstalled(at modelDirectory: URL, options: ParakeetOptions = ParakeetOptions()) throws -> ParakeetCoreMLRuntime {
        guard CoreMLModelValidator.hasReadyMarker(at: modelDirectory) else {
            throw SpeechError.modelNotReady
        }
        return try load(at: modelDirectory, options: options)
    }

    static func load(at modelDirectory: URL, options: ParakeetOptions = ParakeetOptions()) throws -> ParakeetCoreMLRuntime {
        let pipeline = try ParakeetPipelineConfig.load(from: modelDirectory)
        let vocabulary = try ParakeetVocabulary.load(from: modelDirectory, blankID: pipeline.blankID)
        let units = options.computeUnits
        let mel = try ParakeetCoreMLStage.load(CoreMLModelValidator.stageURL(for: "MelSpectrogram", in: modelDirectory), computeUnits: units)
        let encoder = try ParakeetCoreMLStage.load(CoreMLModelValidator.stageURL(for: "AudioEncoder", in: modelDirectory), computeUnits: units)
        let decoder = try ParakeetCoreMLStage.load(CoreMLModelValidator.stageURL(for: "TextDecoder", in: modelDirectory), computeUnits: units)
        let joint = try ParakeetCoreMLStage.load(CoreMLModelValidator.stageURL(for: "Joint", in: modelDirectory), computeUnits: units)
        return ParakeetCoreMLRuntime(
            pipeline: pipeline,
            vocabulary: vocabulary,
            contract: try ParakeetCoreMLContract.resolve(pipeline: pipeline, mel: mel, encoder: encoder, decoder: decoder, joint: joint),
            options: options,
            mel: mel, encoder: encoder, decoder: decoder, joint: joint,
            timings: TimingsBox()
        )
    }

    func transcribe(audioURL: URL, vocabulary phrases: [String] = []) throws -> String {
        try transcribe(samples: Self.loadMono16kSamples(from: audioURL), vocabulary: phrases)
    }

    final class PhaseTimer {
        var melMS = 0.0, encoderMS = 0.0, encoderReadMS = 0.0
        var decoderMS = 0.0, jointMS = 0.0, logitsMS = 0.0, gatherMS = 0.0
        var predictorCalls = 0, jointCalls = 0, biasApplications = 0
        func emit(totalMS: Double, tokens: Int, sink: TimingsBox) {
            sink.set(ParakeetTimings(
                totalMS: Int(totalMS), melMS: Int(melMS), encoderMS: Int(encoderMS + encoderReadMS),
                decodeStepMS: Int(decoderMS), jointMS: Int(jointMS),
                biasApplications: biasApplications, predictorCalls: predictorCalls, jointCalls: jointCalls,
                tokens: tokens
            ))
        }
    }

    /// Force real Core ML/ANE execution ahead of a decode. Constructing the runtime only opens the
    /// models; ANE compilation and first-inference setup happen at `prediction(from:)`, so a short
    /// silent window through the real decode path is what makes the next decode hot.
    func prime() throws {
        _ = try transcribe(samples: [Float](repeating: 0, count: Self.sampleRate / 2))
    }

    func bias(for phrases: [String]) -> ASRVocabularyBias {
        ASRVocabularyBias(phrases: ASRVocabularyBias.normalizedVocabulary(phrases), pieces: vocabulary.pieces, score: options.biasBoost)
    }

    func transcribe(samples: [Float], vocabulary phrases: [String] = []) throws -> String {
        let conditioned = AudioConditioning.conditionForASR(samples, policy: options.conditioningPolicy)
        return try decodePaddedWindow(conditioned, bias: bias(for: phrases))
    }

    /// Decode one scheduled Static-15 window; the caller passes raw window samples by default.
    func transcribeStaticWindow(samples: [Float], vocabulary phrases: [String] = [], alreadyConditioned: Bool = false) throws -> String {
        let input = alreadyConditioned ? samples : AudioConditioning.conditionForASR(samples, policy: options.conditioningPolicy)
        return try decodeText(input, bias: bias(for: phrases))
    }

    /// Parity port of Rust's `heardright_parakeet_core::detok::assemble_pieces`
    /// under `DetokPolicy::canonical_rust()`. A bare join-and-replace silently
    /// diverges from the other two platforms in two ways: special tokens leak
    /// into the transcript, and a digit piece following a word gets glued to
    /// it (`at60` rather than `at 60`).
    ///
    /// The digit rule, which the Rust comment calls out explicitly:
    ///   * word + digit          -> insert a boundary   (`at60`  -> `at 60`)
    ///   * lowercase article `a` -> insert a boundary   (`a24`   -> `a 24`)
    ///   * single capital letter -> keep the identifier (`A4`    -> `A4`)
    ///   * digit + digit         -> keep the number     (`1`+`3` -> `13`)
    static func detokenize(ids: [Int], pieces: [String]) -> String {
        var fullText = ""
        for id in stripMandatoryFillers(ids: ids, pieces: pieces) {
            guard pieces.indices.contains(id) else { continue }
            let piece = pieces[id]
            if isSkippableSpecial(piece) { continue }
            var display = piece.replacingOccurrences(of: "\u{2581}", with: " ")
            guardDigitBoundary(fullText: fullText, display: &display)
            fullText += display
        }
        return fullText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// N0, the parity port of Rust's
    /// `heardright_parakeet_core::detok::strip_mandatory_fillers`.
    ///
    /// `uh` and `um` only: those are the two the canonical corpus actually
    /// contains (733 and 60). `hmm`, `mmm` and `ah` occur zero times and are
    /// real words in some contexts, so removing them would be an unmeasured
    /// guess — which is exactly what the old seven-variant regex in
    /// `TranscriptPolish` was doing, in a third place with a third word list.
    ///
    /// Whole words only: pieces are grouped from one `▁` boundary to the next,
    /// so a group is a word plus any punctuation trailing it. Dropping the group
    /// takes the filler's bracketing comma with it ("Uh, hello" -> "hello") and
    /// leaves the next word's own `▁` intact, so spacing needs no repair.
    static let mandatoryFillers: Set<String> = ["uh", "um"]

    static func isMandatoryFiller(_ word: String) -> Bool {
        let stripped = word.replacingOccurrences(of: "\u{2581}", with: "")
        let trimmed = stripped.trimmingCharacters(
            in: CharacterSet.alphanumerics.inverted
        )
        return mandatoryFillers.contains(trimmed.lowercased())
    }

    /// Text entry point for N0, for backends with no piece stream to filter.
    ///
    /// Whisper decodes through its own tokenizer and returns a plain string, so
    /// the piece-level strip cannot reach it. Same word list, same whole-word
    /// rule, applied at the backend's output — mirrors Rust's
    /// `strip_mandatory_fillers_text`.
    static func stripMandatoryFillers(text: String) -> String {
        text.split(separator: " ", omittingEmptySubsequences: true)
            .filter { !isMandatoryFiller(String($0)) }
            .joined(separator: " ")
    }

    static func stripMandatoryFillers(ids: [Int], pieces: [String]) -> [Int] {
        var out: [Int] = []
        var group: [Int] = []

        func flush() {
            guard !group.isEmpty else { return }
            let word = group
                .compactMap { pieces.indices.contains($0) ? pieces[$0] : nil }
                .joined()
            if !isMandatoryFiller(word) { out.append(contentsOf: group) }
            group.removeAll()
        }

        for id in ids {
            guard pieces.indices.contains(id) else { continue }
            if pieces[id].hasPrefix("\u{2581}") { flush() }
            group.append(id)
        }
        flush()
        return out
    }

    /// `<unk>` is real output; every other `<...>` marker is not.
    static func isSkippableSpecial(_ piece: String) -> Bool {
        piece.hasPrefix("<") && piece.hasSuffix(">") && piece != "<unk>"
    }

    private static func guardDigitBoundary(fullText: String, display: inout String) {
        guard !fullText.isEmpty,
              !display.isEmpty,
              !display.hasPrefix(" "),
              display.allSatisfy({ $0.isASCII && $0.isNumber })
        else { return }

        let trailingLetters = fullText.reversed().prefix(while: { $0.isLetter }).count
        let isArticleA = trailingLetters == 1 && fullText.last == "a"
        if trailingLetters > 1 || isArticleA {
            display = " " + display
        }
    }

    static func argmax(_ values: ArraySlice<Float>) -> Int {
        var bestIndex = values.startIndex
        var bestValue = -Float.infinity
        for index in values.indices where values[index] > bestValue {
            bestIndex = index
            bestValue = values[index]
        }
        return bestIndex - values.startIndex
    }

}
