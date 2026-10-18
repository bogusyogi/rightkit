#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// Pure composer-dictation rules (CodeRight `Dictation`). Audio capture lives in
/// ``LiveDictationSession``.
public enum Dictation {
    private static let maximumPreferredLanguages = 3

    /// Replace the spoken part of a composer draft against one frozen base, so partial results
    /// re-render the same draft instead of accumulating.
    public static func draft(base: String, transcript: String) -> String {
        let typed = base.trimmingCharacters(in: .whitespacesAndNewlines)
        let spoken = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        if spoken.isEmpty { return typed }
        if typed.isEmpty { return spoken }
        return "\(typed) \(spoken)"
    }

    /// Preferred languages (first three), the current locale, then an English fallback; de-duplicated.
    public static func localeCandidates(
        preferredLanguages: [String] = Locale.preferredLanguages,
        current: Locale = .current
    ) -> [Locale] {
        var seen = Set<String>()
        var result: [Locale] = []
        func add(_ locale: Locale) {
            guard seen.insert(canonicalIdentifier(locale)).inserted else { return }
            result.append(locale)
        }
        for language in preferredLanguages.prefix(maximumPreferredLanguages) { add(Locale(identifier: language)) }
        add(current)
        add(Locale(identifier: "en-US"))
        return result
    }

    /// Stable lowercased identifier used for candidate de-duplication.
    public static func canonicalIdentifier(_ locale: Locale) -> String {
        locale.identifier.lowercased().replacingOccurrences(of: "-", with: "_")
    }
}
#endif
