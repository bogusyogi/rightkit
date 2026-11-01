import Foundation

/// A small, single-line payload for ActivityKit; views choose their own line cap.
enum RecordingResultPreview {
    static func text(_ transcript: String?) -> String? {
        guard let transcript else { return nil }
        // Bound work before normalizing a potentially long dictation. Character
        // slicing preserves composed emoji & non-Latin graphemes.
        let normalized = transcript.prefix(512)
            .split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        guard !normalized.isEmpty else { return nil }
        if normalized.count > 140 || transcript.count > 512 {
            return String(normalized.prefix(139)) + "…"
        }
        return normalized
    }
}
