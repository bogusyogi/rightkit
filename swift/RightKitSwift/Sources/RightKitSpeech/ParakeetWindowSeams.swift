import AVFoundation
import CoreML
import Foundation

extension ParakeetCoreMLRuntime {
    /// Padded-window decode. MEASURED 2026-07-19 in HeardRight: this deliberately does NOT match
    /// Rust's `transcribe_padded_window` (full 15 s decode, commit by token time). Decoding exactly
    /// `[start, stableEnd)` and de-duplicating the seam in text won 7.08% WER vs 7.36% on the
    /// canonical corpus, because the boundary is chosen in silence and the 80 ms token-time
    /// quantisation never has to adjudicate a straddling word. Rust should adopt this.
    func decodePaddedWindow(_ samples: [Float], bias: ASRVocabularyBias) throws -> String {
        let window = pipeline.windowSamples
        let count = max(samples.count, 1)
        guard count > window else {
            return try decodeText(Array(samples.prefix(window)), bias: bias)
        }

        let padding = Self.msToSamples(Self.paddedWindowPaddingMs)
        let target = window > padding ? window - padding : window
        var text = ""
        var start = 0
        while start < count {
            let end = min(start + window, samples.count)
            guard end < samples.count else { break }

            let targetEnd = min(start + target, samples.count)
            let stableEnd = Self.quietCut(in: samples, lo: targetEnd, hi: end) ?? end
            let segment = start < stableEnd ? Array(samples[start..<stableEnd]) : []
            let nextText = try decodeText(segment, bias: bias)
            Self.appendWithOverlap(buffer: &text, next: nextText)
            start = max(stableEnd, start + 1)
        }
        if start < samples.count {
            let segment = Array(samples[start..<samples.count])
            let nextText = try decodeText(segment, bias: bias)
            Self.appendWithOverlap(buffer: &text, next: nextText)
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    func decodeTimed(
        _ segment: [Float], bias: ASRVocabularyBias, windowOffset: Float
    ) throws -> [(Int, Float)] {
        var hits: [(Int, Float)] = []
        let timer = PhaseTimer()
        let totalStart = CFAbsoluteTimeGetCurrent()
        try decodeWindow(segment, maxSymbols: options.maxSymbolsPerFrame, hits: &hits, bias: bias,
                         timer: timer, windowOffset: windowOffset)
        timer.emit(totalMS: (CFAbsoluteTimeGetCurrent() - totalStart) * 1000,
                   tokens: hits.count, sink: timings)
        return hits
    }

    func decodeText(_ segment: [Float], bias: ASRVocabularyBias) throws -> String {
        let hits = try decodeTimed(segment, bias: bias, windowOffset: 0)
        return try Self.detokenize(ids: hits.map(\.0), pieces: vocabulary.pieces, provider: detokenizationProvider)
    }

    static func quietCut(in samples: [Float], lo: Int, hi: Int) -> Int? {
        let win = msToSamples(paddedWindowSilenceMs)
        let hop = msToSamples(paddedWindowSilenceHopMs)
        guard hi > lo, hi - lo >= win else { return nil }

        var best: (cut: Int, value: Float)?
        var index = lo
        while index + win <= hi {
            let value = AudioConditioning.rms(Array(samples[index..<(index + win)]))
            if best.map({ value < $0.value }) ?? true {
                best = (index + win / 2, value)
            }
            index += hop
        }
        return best?.cut
    }

    static func appendWithOverlap(buffer: inout String, next: String) {
        let incoming = next.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !incoming.isEmpty else { return }
        guard !buffer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            buffer = incoming
            return
        }

        let characterOverlap = suffixPrefixOverlap(buffer, incoming, maxCharacters: paddedWindowOverlapChars)
        if characterOverlap > 0 {
            let index = incoming.index(incoming.startIndex, offsetBy: characterOverlap)
            buffer.append(contentsOf: incoming[index...])
            return
        }

        if let prefixEnd = suffixPrefixWordOverlap(buffer, incoming, maxWords: 8) {
            let remainder = String(incoming[prefixEnd...]).trimmingCharacters(in: .whitespacesAndNewlines)
            if !remainder.isEmpty {
                removeTrailingSentencePunctuation(from: &buffer)
            }
            appendJoined(buffer: &buffer, next: remainder)
            return
        }

        appendJoined(buffer: &buffer, next: incoming)
    }

    static func suffixPrefixOverlap(_ left: String, _ right: String, maxCharacters: Int) -> Int {
        let maxCount = min(left.count, right.count, maxCharacters)
        guard maxCount > 0 else { return 0 }
        var best = 0
        for count in 1...maxCount {
            let leftSuffix = String(left.suffix(count))
            let rightPrefix = String(right.prefix(count))
            if leftSuffix.caseInsensitiveCompare(rightPrefix) == .orderedSame {
                best = count
            }
        }
        return best
    }

    static func suffixPrefixWordOverlap(_ left: String, _ right: String, maxWords: Int) -> String.Index? {
        let leftWords = wordTokens(in: left)
        let rightWords = wordTokens(in: right)
        let maxCount = min(maxWords, leftWords.count, rightWords.count)
        guard maxCount > 0 else { return nil }

        var best: String.Index?
        for count in 1...maxCount {
            let leftSuffix = leftWords.suffix(count).map(\.normalized)
            let rightPrefix = rightWords.prefix(count).map(\.normalized)
            if Array(leftSuffix) == Array(rightPrefix) {
                best = rightWords[count - 1].range.upperBound
            }
        }
        return best
    }

    static func appendJoined(buffer: inout String, next: String) {
        guard !next.isEmpty else { return }
        let noLeadingSpaceBefore: Set<Character> = [".", ",", "!", "?", ";", ":", ")", "]", "}"]
        let noTrailingSpaceAfter: Set<Character> = ["(", "[", "{"]
        if let last = buffer.last,
           let first = next.first,
           !last.isWhitespace,
           !noTrailingSpaceAfter.contains(last),
           !noLeadingSpaceBefore.contains(first) {
            buffer.append(" ")
        }
        buffer.append(contentsOf: next)
    }

    static func removeTrailingSentencePunctuation(from buffer: inout String) {
        while buffer.last?.isWhitespace == true {
            buffer.removeLast()
        }
        if let last = buffer.last, ".?!".contains(last) {
            buffer.removeLast()
        }
    }

    struct WordToken {
        let normalized: String
        let range: Range<String.Index>
    }

    static func wordTokens(in text: String) -> [WordToken] {
        var tokens: [WordToken] = []
        var tokenStart: String.Index?
        var normalized = ""
        var index = text.startIndex
        while index < text.endIndex {
            let character = text[index]
            if character.isLetter || character.isNumber {
                if tokenStart == nil {
                    tokenStart = index
                }
                normalized.append(contentsOf: character.lowercased())
            } else if let start = tokenStart {
                tokens.append(WordToken(normalized: normalized, range: start..<index))
                tokenStart = nil
                normalized = ""
            }
            index = text.index(after: index)
        }
        if let start = tokenStart {
            tokens.append(WordToken(normalized: normalized, range: start..<text.endIndex))
        }
        return tokens
    }

    static func msToSamples(_ ms: Int) -> Int {
        sampleRate * ms / 1_000
    }
}
