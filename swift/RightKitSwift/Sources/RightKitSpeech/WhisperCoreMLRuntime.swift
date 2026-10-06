import CoreML
import Foundation


struct WhisperCoreMLRuntime {
    /// Whisper's native window: the encoder always sees 30 s, because
    /// `WhisperMelSpectrogram.logMel` zero-pads whatever it is given up to this
    /// length. It is the TENSOR size, not the segmentation policy.
    static let windowSamples = 480_000

    /// Static15: the segment length dictation is cut into, 15 s.
    ///
    /// This is deliberately SHORTER than `windowSamples`. Measured on the
    /// canonical corpus, 15 s segments beat decoding the whole utterance in 30 s
    /// windows on both other platforms:
    ///
    ///   Windows (whisper.cpp)  deferred30 8.82% -> checkpoint-15 5.50%
    ///   macOS   (CoreML)       deferred30 6.43% -> checkpoint-15 5.61%
    ///
    /// It cannot go much lower: every decode is padded to 30 s regardless, so a
    /// 5 s segment asks Whisper to transcribe a window that is 83% silence —
    /// far outside its training distribution. 15 s sits at a 50/50 real-to-pad
    /// ratio, and 10 s and 5 s both measured materially worse.
    static let segmentSamples = 240_000
    static let kvDim = 5_120
    static let kvMax = 448
    static let vocabSize = 51_866

    static let sot: Int32 = 50_258
    static let eot: Int32 = 50_257
    static let langEN: Int32 = 50_259
    static let langLast: Int32 = 50_358
    static let transcribeToken: Int32 = 50_360
    static let noTimestamps: Int32 = 50_364
    static let timestampBegin: Int32 = 50_365
    static let autoLanguage: Int32 = -1

    static let beginSuppressTokens = [220, Int(eot)]

    private let encoder: ParakeetCoreMLStage
    private let decoder: ParakeetCoreMLStage
    private let tokenizer: WhisperTokenizer

    /// Loads `AudioEncoder`, `TextDecoder` and `tokenizer.json` from an installed Whisper Core ML
    /// directory (the HeardRight `whisper-multi` artifact layout).
    static func loadInstalled(at modelDirectory: URL, computeUnits: MLComputeUnits = .cpuAndNeuralEngine) throws -> WhisperCoreMLRuntime {
        let tokenizerURL = modelDirectory.appendingPathComponent("tokenizer.json")
        guard FileManager.default.fileExists(atPath: tokenizerURL.path) else {
            throw SpeechError.missingFile("tokenizer.json")
        }
        return WhisperCoreMLRuntime(
            encoder: try ParakeetCoreMLStage.load(CoreMLModelValidator.stageURL(for: "AudioEncoder", in: modelDirectory), computeUnits: computeUnits),
            decoder: try ParakeetCoreMLStage.load(CoreMLModelValidator.stageURL(for: "TextDecoder", in: modelDirectory), computeUnits: computeUnits),
            tokenizer: try WhisperTokenizer(tokenizerURL: tokenizerURL)
        )
    }

    func languageToken(_ language: String) -> Int32 {
        if language == "auto" {
            return Self.autoLanguage
        }
        return tokenizer.tokenID("<|\(language)|>").map(Int32.init) ?? Self.langEN
    }

    func transcribe(audioURL: URL, languageToken: Int32 = Self.langEN) throws -> String {
        try transcribe(samples: ParakeetCoreMLRuntime.loadMono16kSamples(from: audioURL), languageToken: languageToken)
    }

    func transcribe(samples: [Float], languageToken: Int32 = Self.langEN) throws -> String {
        try transcribeStaticWindow(samples: samples, languageToken: languageToken).text
    }

    /// Decode one capture scheduler segment with the same 30 s tensor,
    /// timestamped token loop, tokenizer seam, & N0 filtering as deferred
    /// Whisper. Caller supplies a recording language lock so AUTO detection
    /// runs only on first segment, matching macOS worker behavior.
    func transcribeStaticWindow(
        samples: [Float],
        languageToken: Int32 = Self.autoLanguage,
        alreadyConditioned: Bool = false
    ) throws -> (text: String, resolvedLanguage: Int32) {
        let conditioned = alreadyConditioned ? samples : AudioConditioning.conditionForASR(samples)
        var text = ""
        var activeLanguage = languageToken
        for range in Self.windowRanges(samples: conditioned) {
            let segment = range.isEmpty ? [] : Array(conditioned[range])
            let mel = try WhisperMelSpectrogram.logMel(windowedSamples: segment)
            let result = try transcribeMel(mel: mel, languageToken: activeLanguage)
            // Shared seam rule instead of a blind space: de-duplicates repeated
            // boundary wording and still separates disjoint windows.
            ParakeetCoreMLRuntime.appendWithOverlap(buffer: &text, next: result.text)
            if languageToken == Self.autoLanguage {
                activeLanguage = result.resolvedLanguage
            }
        }
        // N0. Whisper has its own tokenizer, so it never passes through
        // `ParakeetCoreMLRuntime.detokenize` where the piece-level strip lives —
        // without this, Whisper dictation still carries every "uh" and "um" into
        // AI polish. Same two words, same whole-word rule as every other path.
        return (
            text: ParakeetCoreMLRuntime.stripMandatoryFillers(text: text)
                .trimmingCharacters(in: .whitespacesAndNewlines),
            resolvedLanguage: activeLanguage
        )
    }

    func transcribe(mel: [Float], languageToken: Int32 = Self.langEN) throws -> String {
        ParakeetCoreMLRuntime.stripMandatoryFillers(text: try transcribeMel(mel: mel, languageToken: languageToken).text)
    }

    private func transcribeMel(mel: [Float], languageToken: Int32) throws -> (text: String, resolvedLanguage: Int32) {
        guard mel.count == 128 * 3000 else {
            throw SpeechError.invalidMelShape(mel.count)
        }
        let encoded = try encoder.predict(
            inputs: [
                "melspectrogram_features": ParakeetCoreMLArray.float16(shape: [1, 128, 1, 3000], values: mel),
            ],
            outputName: "encoder_output_embeds"
        )
        let result = try decodeFromEncoded(encoded, languageToken: languageToken, timestamps: true)
        return (try tokenizer.decode(result.tokens), result.resolvedLanguage)
    }

    func decodeFromEncoded(
        _ encoded: MLMultiArray,
        languageToken: Int32,
        timestamps: Bool
    ) throws -> (tokens: [Int], resolvedLanguage: Int32) {
        let shape = encoded.shape.map(\.intValue)
        guard shape == [1, 1280, 1, 1500] else {
            throw SpeechError.invalidEncodedOutput
        }

        var resolvedLanguage = languageToken
        var prompt = Self.prompt(languageToken: languageToken, timestamps: timestamps)
        var current = prompt
        var next = prompt[0]
        let initialPromptLength = prompt.count

        let inputIDs = try ParakeetCoreMLArray.int32(shape: [1], values: [next])
        let cacheLength = try ParakeetCoreMLArray.int32(shape: [1], values: [0])
        let keyCache = try ParakeetCoreMLArray.float16(
            shape: [1, Self.kvDim, 1, Self.kvMax],
            values: Array(repeating: 0, count: Self.kvDim * Self.kvMax)
        )
        let valueCache = try ParakeetCoreMLArray.float16(
            shape: [1, Self.kvDim, 1, Self.kvMax],
            values: Array(repeating: 0, count: Self.kvDim * Self.kvMax)
        )
        let keyPaddingMask = try ParakeetCoreMLArray.float16(
            shape: [1, Self.kvMax],
            values: Array(repeating: -10_000, count: Self.kvMax)
        )
        try ParakeetCoreMLArray.setFloat16(keyPaddingMask, at: 0, value: 0)

        let updateMask = try ParakeetCoreMLArray.int32(
            shape: [1, Self.kvMax],
            values: Array(repeating: 0, count: Self.kvMax)
        )
        try ParakeetCoreMLArray.setInt32(updateMask, at: 0, value: 1)

        for tokenIndex in 0..<(Self.kvMax - 1) {
            if tokenIndex < initialPromptLength {
                next = prompt[tokenIndex]
            }
            try ParakeetCoreMLArray.setInt32(inputIDs, at: 0, value: next)
            try ParakeetCoreMLArray.setInt32(cacheLength, at: 0, value: Int32(tokenIndex))

            let outputs = try decoder.predict(
                inputs: [
                    "input_ids": inputIDs,
                    "cache_length": cacheLength,
                    "key_cache": keyCache,
                    "value_cache": valueCache,
                    "decoder_key_padding_mask": keyPaddingMask,
                    "kv_cache_update_mask": updateMask,
                    "encoder_output_embeds": encoded,
                ],
                outputNames: ["logits", "key_cache_updates", "value_cache_updates"]
            )

            if resolvedLanguage == Self.autoLanguage, tokenIndex == 0 {
                let logits = try ParakeetCoreMLArray.readFloats(outputs[0])
                resolvedLanguage = Self.bestLanguageToken(in: logits)
                prompt[1] = resolvedLanguage
                current[1] = resolvedLanguage
            }

            if tokenIndex >= initialPromptLength - 1 {
                var logits = try ParakeetCoreMLArray.readFloats(outputs[0])
                guard logits.count == Self.vocabSize else {
                    throw SpeechError.invalidLogitShape
                }
                Self.applyFilters(
                    to: &logits,
                    currentTokens: current,
                    initialPromptLength: initialPromptLength,
                    timestamps: timestamps
                )
                let sampled = Int32(Self.argmax(logits))
                if sampled == Self.eot {
                    break
                }
                current.append(sampled)
                next = sampled
            }

            let keyUpdate = try ParakeetCoreMLArray.readFloats(outputs[1])
            let valueUpdate = try ParakeetCoreMLArray.readFloats(outputs[2])
            guard keyUpdate.count >= Self.kvDim, valueUpdate.count >= Self.kvDim else {
                throw SpeechError.invalidEncodedOutput
            }
            for channel in 0..<Self.kvDim {
                try ParakeetCoreMLArray.setFloat16(keyCache, at: channel * Self.kvMax + tokenIndex, value: keyUpdate[channel])
                try ParakeetCoreMLArray.setFloat16(valueCache, at: channel * Self.kvMax + tokenIndex, value: valueUpdate[channel])
            }
            try ParakeetCoreMLArray.setFloat16(keyPaddingMask, at: tokenIndex + 1, value: 0)
            try ParakeetCoreMLArray.setInt32(updateMask, at: tokenIndex, value: 0)
            try ParakeetCoreMLArray.setInt32(updateMask, at: tokenIndex + 1, value: 1)
        }

        return (
            tokens: Self.contentTokenIDs(from: current, initialPromptLength: initialPromptLength),
            resolvedLanguage: resolvedLanguage
        )
    }

    static func prompt(languageToken: Int32, timestamps: Bool) -> [Int32] {
        let promptLanguage = languageToken == autoLanguage ? langEN : languageToken
        return [
            sot,
            promptLanguage,
            transcribeToken,
            timestamps ? timestampBegin : noTimestamps,
        ]
    }

    /// Window boundaries for a long dictation, following the padded-window
    /// contract (`docs/ASR_DECODE_CONTRACT.md`) with Whisper's native 30 s window.
    ///
    /// This used to advance by hard `start += windowSamples` cuts, so a word
    /// straddling a 30 s boundary was split across two decodes with nothing to
    /// repair it. Each boundary is now moved onto the quietest 200 ms span in the
    /// trailing 2.24 s (`ParakeetCoreMLRuntime.quietCut`, the same routine and the
    /// same constants the Parakeet path uses, so the two backends cut alike).
    ///
    /// Takes the CONDITIONED samples themselves rather than a count: the cut
    /// search needs the audio, and a separate count parameter could disagree with
    /// it and silently fall back to hard cuts.
    static func windowRanges(samples: [Float]) -> [Range<Int>] {
        let sampleCount = samples.count
        let loopCount = max(sampleCount, 1)
        guard sampleCount > Self.segmentSamples else {
            // One segment or less: no seam to place.
            var ranges: [Range<Int>] = []
            var start = 0
            while start < loopCount {
                ranges.append(start..<min(start + Self.segmentSamples, sampleCount))
                start += Self.segmentSamples
            }
            return ranges
        }

        let padding = ParakeetCoreMLRuntime.msToSamples(ParakeetCoreMLRuntime.paddedWindowPaddingMs)
        let target = Self.segmentSamples > padding ? Self.segmentSamples - padding : Self.segmentSamples
        var ranges: [Range<Int>] = []
        var start = 0
        while start < sampleCount {
            let windowEnd = min(start + Self.segmentSamples, sampleCount)
            if windowEnd >= sampleCount { break }
            let targetEnd = min(start + target, sampleCount)
            let cut = ParakeetCoreMLRuntime.quietCut(in: samples, lo: targetEnd, hi: windowEnd) ?? windowEnd
            ranges.append(start..<cut)
            start = max(cut, start + 1)
        }
        if start < sampleCount {
            ranges.append(start..<sampleCount)
        }
        return ranges
    }

    static func applyFilters(
        to logits: inout [Float],
        currentTokens: [Int32],
        initialPromptLength: Int,
        timestamps: Bool
    ) {
        guard logits.count >= vocabSize else { return }

        if currentTokens.count == initialPromptLength {
            for token in beginSuppressTokens {
                logits[token] = -.infinity
            }
        }

        if timestamps {
            applyTimestampRules(to: &logits, sampledTokens: Array(currentTokens.dropFirst(initialPromptLength)))
        } else {
            for token in Int(timestampBegin)..<vocabSize {
                logits[token] = -.infinity
            }
        }
    }

    static func applyTimestampRules(to logits: inout [Float], sampledTokens: [Int32]) {
        guard logits.count >= vocabSize else { return }

        logits[Int(noTimestamps)] = -.infinity
        let count = sampledTokens.count
        if count > 0 {
            let lastWasTimestamp = sampledTokens[count - 1] >= timestampBegin
            let penultimateWasTimestamp = count < 2 || sampledTokens[count - 2] >= timestampBegin

            if lastWasTimestamp {
                if penultimateWasTimestamp {
                    for token in Int(timestampBegin)..<vocabSize {
                        logits[token] = -.infinity
                    }
                } else {
                    for token in 0..<Int(eot) {
                        logits[token] = -.infinity
                    }
                }
            }

            if let lastTimestamp = sampledTokens.reversed().first(where: { $0 >= timestampBegin }) {
                let lowerBound = lastWasTimestamp && !penultimateWasTimestamp
                    ? lastTimestamp
                    : lastTimestamp + 1
                if lowerBound > timestampBegin {
                    for token in Int(timestampBegin)..<min(Int(lowerBound), vocabSize) {
                        logits[token] = -.infinity
                    }
                }
            }
        }

        // Keep timestamp pairing/monotonicity above, but do not force a
        // timestamp from aggregate probability mass. That can suppress the
        // best speech token and terminate dictation before spoken words.
        // This is the same targeted policy as macOS Whisper CoreML; see
        // docs/experiments/2026-09-27-whisper-policy-comparison.md.
    }

    static func contentTokenIDs(from tokens: [Int32], initialPromptLength: Int) -> [Int] {
        tokens
            .dropFirst(initialPromptLength)
            .filter { $0 < eot }
            .map(Int.init)
    }

    static func bestLanguageToken(in logits: [Float]) -> Int32 {
        var bestToken = langEN
        var bestValue = -Float.infinity
        for token in Int(langEN)...Int(langLast) where token < logits.count {
            if logits[token] > bestValue {
                bestValue = logits[token]
                bestToken = Int32(token)
            }
        }
        return bestToken
    }

    static func argmax(_ values: [Float]) -> Int {
        var bestIndex = 0
        var bestValue = -Float.infinity
        for (index, value) in values.enumerated() where value > bestValue {
            bestIndex = index
            bestValue = value
        }
        return bestIndex
    }
}
