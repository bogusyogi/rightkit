#if os(iOS)
import Foundation
import NaturalLanguage
import UIKit

/// One flagged span in checked text: a possible misspelling or a proofreading note, plus
/// candidate replacements when available.
public struct TextCheckingIssue: Identifiable, Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        case spelling
        case grammar
    }

    public var id: String { "\(kind)-\(range.location)-\(range.length)" }
    public let range: NSRange
    public let kind: Kind
    public let word: String
    public let suggestions: [String]

    public init(range: NSRange, kind: Kind, word: String, suggestions: [String]) {
        self.range = range
        self.kind = kind
        self.word = word
        self.suggestions = suggestions
    }
}

/// On-device spell-check and light proofreading for composer/editor text (APP-20).
///
/// Uses `UITextChecker` for spelling (the same on-device dictionary UIKit's text fields use —
/// no network call, no third-party dependency) and `NaturalLanguage`'s tokenizer/language
/// recognizer to scope spelling checks to the text's dominant language and to skip tokens
/// that are not natural-language words (URLs, code identifiers with digits, etc. still pass
/// through `UITextChecker` itself, which already ignores most of those).
///
/// Integration note (left for the owning lane/orchestrator): this is a standalone service,
/// not wired into any composer view. A composer would call `check(text:)` on debounce as the
/// user types and render `TextCheckingIssue.range`s as underlines, offering `suggestions` in
/// a tap menu — no existing composer view is edited by this lane.
@MainActor
public struct TextCheckingService {
    private let checker: UITextChecker

    public init(checker: UITextChecker? = nil) {
        self.checker = checker ?? UITextChecker()
    }

    /// Flags likely misspellings across the full string. Language is auto-detected via
    /// `NLLanguageRecognizer`; when detection is inconclusive, `UITextChecker` falls back to
    /// its own default-locale behavior.
    public func check(text: String) -> [TextCheckingIssue] {
        guard !text.isEmpty else { return [] }

        let language = detectedLanguage(for: text)
        let nsText = text as NSString
        var issues: [TextCheckingIssue] = []
        var searchRange = NSRange(location: 0, length: nsText.length)

        while searchRange.length > 0 {
            let misspelledRange = checker.rangeOfMisspelledWord(
                in: text,
                range: searchRange,
                startingAt: searchRange.location,
                wrap: false,
                language: language
            )
            guard misspelledRange.location != NSNotFound else { break }

            let word = nsText.substring(with: misspelledRange)
            let guesses = checker.guesses(
                forWordRange: misspelledRange,
                in: text,
                language: language
            ) ?? []

            issues.append(
                TextCheckingIssue(
                    range: misspelledRange,
                    kind: .spelling,
                    word: word,
                    suggestions: guesses
                )
            )

            let nextLocation = misspelledRange.location + misspelledRange.length
            guard nextLocation < nsText.length else { break }
            searchRange = NSRange(location: nextLocation, length: nsText.length - nextLocation)
        }

        return issues
    }

    private func detectedLanguage(for text: String) -> String {
        let recognizer = NLLanguageRecognizer()
        recognizer.processString(text)
        return recognizer.dominantLanguage?.rawValue ?? Locale.current.language.languageCode?.identifier ?? "en"
    }
}
#endif
