import Foundation

/// A text segment and its UTF-8 source span before speech synthesis.
public struct TextSegment: Sendable, Equatable {
    /// Text passed to one per-segment synthesis operation.
    public let text: String
    /// UTF-8 byte range in the original input, excluding outer whitespace.
    public let sourceRange: Range<Int>

    public init(text: String, sourceRange: Range<Int>) {
        self.text = text
        self.sourceRange = sourceRange
    }
}

/// Splits text into speakable chunks: sentences first, then word boundaries, then characters.
public enum TextChunker {
    private static let abbreviations: Set<String> = ["mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e"]

    /// Splits `text` into chunks of at most `maxCharacters` characters. Adjacent short sentences are packed
    /// together; a sentence longer than the limit falls back to words, and a word longer than the limit to
    /// characters.
    ///
    /// The returned ranges are UTF-8 offsets into the original string after only its outer whitespace has
    /// been removed. Abbreviations ("Dr.", "e.g.") and decimal points do not end a sentence.
    public static func chunk(_ text: String, maxCharacters: Int = 240) -> [TextSegment] {
        let limit = max(1, maxCharacters)
        guard let first = text.firstIndex(where: { !$0.isWhitespace }),
              let last = text.lastIndex(where: { !$0.isWhitespace })
        else { return [] }
        let trimmedRange = first ..< text.index(after: last)
        let trimmed = text[trimmedRange]

        var ranges: [Range<Substring.Index>] = []
        var accumulated: Range<Substring.Index>?

        for sentence in sentenceRanges(in: trimmed) {
            let sentenceLength = trimmed.distance(from: sentence.lowerBound, to: sentence.upperBound)
            if sentenceLength > limit {
                if let accumulated { ranges.append(accumulated) }
                accumulated = nil
                ranges.append(contentsOf: splitOversized(sentence, in: trimmed, maxCharacters: limit))
                continue
            }
            guard let current = accumulated else {
                accumulated = sentence
                continue
            }
            if trimmed.distance(from: current.lowerBound, to: sentence.upperBound) <= limit {
                accumulated = current.lowerBound ..< sentence.upperBound
            } else {
                ranges.append(current)
                accumulated = sentence
            }
        }
        if let accumulated { ranges.append(accumulated) }

        let base = text.utf8.distance(from: text.startIndex, to: trimmedRange.lowerBound)
        return ranges.compactMap { range in
            let piece = trimmed[range]
            // A sentence range can carry trailing whitespace; the segment text and span exclude it.
            guard let end = piece.lastIndex(where: { !$0.isWhitespace }) else { return nil }
            let trimmedPiece = piece[piece.startIndex ... end]
            let lower = base + trimmed.utf8.distance(from: trimmed.startIndex, to: trimmedPiece.startIndex)
            let upper = base + trimmed.utf8.distance(from: trimmed.startIndex, to: trimmedPiece.endIndex)
            return TextSegment(text: String(trimmedPiece), sourceRange: lower ..< upper)
        }
    }

    private static func sentenceRanges(in text: Substring) -> [Range<Substring.Index>] {
        var ranges: [Range<Substring.Index>] = []
        var sentenceStart = text.startIndex
        var index = text.startIndex
        while index < text.endIndex {
            let character = text[index]
            let isTerminator = character == "!" || character == "?" || character == "。" || character == "！" || character == "？"
            let isPeriod = character == "."
            if (isTerminator || isPeriod) && isSentenceEnd(text, at: index, isPeriod: isPeriod) {
                var end = text.index(after: index)
                while end < text.endIndex, text[end].isWhitespace { end = text.index(after: end) }
                ranges.append(sentenceStart ..< end)
                sentenceStart = end
                index = end
            } else {
                index = text.index(after: index)
            }
        }
        if sentenceStart < text.endIndex { ranges.append(sentenceStart ..< text.endIndex) }
        return ranges
    }

    private static func isSentenceEnd(_ text: Substring, at index: Substring.Index, isPeriod: Bool) -> Bool {
        let next = text.index(after: index)
        if next < text.endIndex, text[next].isNumber || text[next] == "." { return false }
        if isPeriod, index > text.startIndex {
            let previous = text[text.index(before: index)]
            if previous.isNumber, next < text.endIndex, text[next].isNumber { return false }
            let wordStart = text[..<index].lastIndex(where: { $0.isWhitespace }).map { text.index(after: $0) } ?? text.startIndex
            let word = text[wordStart ..< index].lowercased()
            if abbreviations.contains(word) || (word.count == 1 && word.first?.isLetter == true) { return false }
        }
        return next == text.endIndex || text[next].isWhitespace || !text[next].isLetter
    }

    private static func splitOversized(
        _ range: Range<Substring.Index>, in text: Substring, maxCharacters: Int
    ) -> [Range<Substring.Index>] {
        let words = text[range].split(whereSeparator: { $0.isWhitespace })
        guard !words.isEmpty else { return [] }
        var result: [Range<Substring.Index>] = []
        var current: Range<Substring.Index>?
        for word in words {
            if word.count > maxCharacters {
                if let current { result.append(current) }
                current = nil
                var start = word.startIndex
                while start < word.endIndex {
                    let end = word.index(start, offsetBy: maxCharacters, limitedBy: word.endIndex) ?? word.endIndex
                    result.append(start ..< end)
                    start = end
                }
                continue
            }
            guard let currentRange = current else {
                current = word.startIndex ..< word.endIndex
                continue
            }
            if text.distance(from: currentRange.lowerBound, to: word.endIndex) <= maxCharacters {
                current = currentRange.lowerBound ..< word.endIndex
            } else {
                result.append(currentRange)
                current = word.startIndex ..< word.endIndex
            }
        }
        if let current { result.append(current) }
        return result
    }
}
