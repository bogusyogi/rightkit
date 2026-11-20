import Foundation

/// Accepts or rejects what a model answered to a transcript-cleanup request, and tidies what it accepts.
///
/// A cleanup model is asked to return the transcript and nothing else. This guard drops answers that are
/// not that: a code fence, an assistant preamble, an echo of the request envelope, an answer that grew
/// far beyond the input, or one that changed a digit. What it accepts has em and en dashes removed (`to`
/// between numbers, a comma otherwise). It holds no prompt and no provider; use it as the `accept` hook of
/// ``LLMClient/complete(_:routes:accept:)``.
public struct PolishOutputGuard: Sendable, Equatable {
    /// Lower-case start of the request envelope the app sends (for example its schema line). An answer that
    /// starts with it AND contains every one of ``envelopeMarkers`` is an echo of the request. Empty: no echo check.
    public var envelopeSchemaPrefix: String
    /// Lower-case fragments that must all appear for ``envelopeSchemaPrefix`` to count as an echo.
    public var envelopeMarkers: [String]
    /// How many characters longer than the input an answer may be.
    public var maxGrowth: Int

    public init(envelopeSchemaPrefix: String = "", envelopeMarkers: [String] = [], maxGrowth: Int = 160) {
        self.envelopeSchemaPrefix = envelopeSchemaPrefix
        self.envelopeMarkers = envelopeMarkers
        self.maxGrowth = maxGrowth
    }

    /// The accepted, tidied text, or nil when the answer is not a transcript.
    public func normalized(_ output: String, input: String) -> String? {
        var trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !trimmed.hasPrefix("```") else { return nil }
        var lower = trimmed.lowercased()
        if lower.hasPrefix("transcript:") || lower.contains("the user wants") ||
            lower.hasPrefix("here is") || lower.hasPrefix("sure,") || lower.contains("<transcript>") {
            return nil
        }
        for prefix in ["cleaned transcript:", "polished output:"] where lower.hasPrefix(prefix) {
            trimmed = String(trimmed.dropFirst(prefix.count)).trimmingCharacters(in: .whitespacesAndNewlines)
            lower = trimmed.lowercased()
        }
        guard !trimmed.isEmpty, trimmed.count <= input.count + maxGrowth else { return nil }
        let envelope = lower.replacingOccurrences(of: "\\_", with: "_")
        if !envelopeSchemaPrefix.isEmpty, envelope.hasPrefix(envelopeSchemaPrefix),
           envelopeMarkers.allSatisfy({ envelope.contains($0) }) {
            return nil
        }
        let inputDigits = input.filter(\.isNumber)
        let outputDigits = trimmed.filter(\.isNumber)
        guard inputDigits == outputDigits else { return nil }
        return Self.scrubDashes(trimmed)
    }

    /// Removes em and en dashes: `to` between two numbers, otherwise a comma.
    public static func scrubDashes(_ text: String) -> String {
        let characters = Array(text)
        var output = ""
        var index = 0
        while index < characters.count {
            guard characters[index] == "—" || characters[index] == "–" else {
                output.append(characters[index]); index += 1; continue
            }
            while output.last == " " { output.removeLast() }
            var next = index + 1
            while next < characters.count, characters[next] == " " { next += 1 }
            if output.last?.isNumber == true, next < characters.count, characters[next].isNumber {
                output += " to "
            } else if !output.isEmpty, next < characters.count {
                output += ", "
            }
            index = next
        }
        return output.replacingOccurrences(of: ", ,", with: ",")
            .replacingOccurrences(of: ",,", with: ",")
            .trimmingCharacters(in: .whitespacesAndNewlines.union(CharacterSet(charactersIn: ",")))
    }
}
