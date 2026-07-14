import Foundation

/// Redaction for anything that reaches a diagnostics log. Extracted from HeardRight's
/// `DiagnosticsReport.safeIdentifier`, which is deny-list *then* allow-list: a value carrying any
/// sensitive marker is dropped whole, and whatever survives is reduced to a known-safe alphabet.
///
/// The order matters. Sanitising first and screening second would let `https://x/?token=abc` pass as
/// a "clean" string, because sanitising removes nothing that the marker check is looking for.
public struct PrivacyRedactor: Sendable {
    public static let redacted = "[redacted]"
    public static let unknown = "unknown"
    public static let maxLength = 80

    /// Substrings that mean "this value is not ours to log": URLs and file schemes, audio capture
    /// paths, and anything that smells like a credential.
    private static let sensitiveMarkers = [
        "http://",
        "https://",
        "file://",
        "/tmp/",
        ".caf",
        ".wav",
        "authorization",
        "bearer",
        "secret",
        "token",
    ]

    private static let allowed = CharacterSet(
        charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 ._:-+/()",
    )

    public init() {}

    public func redact(_ value: String) -> String {
        let lowered = value.lowercased()
        if Self.sensitiveMarkers.contains(where: lowered.contains) { return Self.redacted }

        let scalars = value.unicodeScalars.map { Self.allowed.contains($0) ? Character($0) : "-" }
        let cleaned = String(scalars).trimmingCharacters(in: .whitespacesAndNewlines)
        if cleaned.isEmpty { return Self.unknown }
        return String(cleaned.prefix(Self.maxLength))
    }
}
