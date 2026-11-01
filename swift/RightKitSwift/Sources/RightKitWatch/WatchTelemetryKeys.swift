import Foundation

/// The single wire key for watch-to-phone diagnostics. Shared so the Watch's
/// `WatchTelemetry` and the phone's `WatchRecordingInbox` cannot drift apart —
/// a mismatch here would silently drop every watch event with no error.
enum WatchTelemetryKeys {
    static let payloadKey = "watchTelemetry"
    static let processingResultKey = "watchRecordingResult"
    static let telemetryEnabledKey = "watchTelemetryEnabled"
}
