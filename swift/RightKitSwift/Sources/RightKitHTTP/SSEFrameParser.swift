import Foundation

enum SSEFrameParserError: LocalizedError, Equatable {
    case frameTooLarge(maximumBytes: Int)

    var errorDescription: String? {
        switch self {
        case .frameTooLarge(let maximumBytes):
            return "The daemon sent an event larger than \(maximumBytes) bytes."
        }
    }
}

/// Pure port of `readSseMessages` from `coderight-web/src/contexts/SessionContext.tsx`.
/// Kept free of URLSession so both halves are independently testable.
enum SSEFrameParser {
    static let maximumBufferedFrameBytes = 1_048_576

    static func append(
        _ byte: UInt8,
        to buffer: inout [UInt8],
        maximumBytes: Int = maximumBufferedFrameBytes
    ) throws {
        guard buffer.count < maximumBytes else {
            throw SSEFrameParserError.frameTooLarge(maximumBytes: maximumBytes)
        }
        buffer.append(byte)
    }

    /// Splits a growing buffer into complete frames (separated by a blank line, either
    /// `\n\n` or `\r\n\r\n`), returning the frames found and the unconsumed remainder to
    /// keep buffering against the next chunk.
    static func extractFrames(from buffer: String) -> (frames: [String], remainder: String) {
        var frames: [String] = []
        var remaining = buffer
        while true {
            let lfBreak = remaining.range(of: "\n\n")
            let crlfBreak = remaining.range(of: "\r\n\r\n")
            let candidates = [lfBreak, crlfBreak].compactMap { $0 }
            guard let nextBreak = candidates.min(by: { $0.lowerBound < $1.lowerBound }) else {
                return (frames, remaining)
            }
            frames.append(String(remaining[remaining.startIndex..<nextBreak.lowerBound]))
            remaining = String(remaining[nextBreak.upperBound...])
        }
    }

    /// Joins the `data:` lines within one frame into a single payload, dropping exactly one
    /// leading space after the colon (SSE convention), not the full run of whitespace the TS
    /// `.trimStart()` would remove. Returns nil for frames with no (or empty) data — mirrors
    /// the reference `if (data) onData(data)` guard.
    static func payload(fromFrame frame: String) -> String? {
        // "\r\n" is ONE Character in Swift (grapheme cluster), so splitting on "\n" alone
        // would never break CRLF lines — match any of the three terminators as whole Characters.
        let dataLines: [String] = frame
            .split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" || $0 == "\r\n" || $0 == "\r" })
            .compactMap { rawLine -> String? in
                let line = rawLine
                guard line.hasPrefix("data:") else { return nil }
                var value = line.dropFirst(5)
                if value.first == " " { value = value.dropFirst() }
                return String(value)
            }
        let joined = dataLines.joined(separator: "\n")
        return joined.isEmpty ? nil : joined
    }
}
