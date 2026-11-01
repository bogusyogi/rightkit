#if os(iOS)
#if canImport(ActivityKit)
import ActivityKit
import Foundation

/// Typed phase vocabulary for the recording Live Activity. Replaces ad-hoc
/// string compares between the controller and the widget so a typo is a
/// compile error, not a silent UI dead-air.
public enum RecordingPhase: String, Codable, Hashable, CaseIterable, Sendable {
    case recording
    case processing
    case copied
    case ready
    case noSpeech
    case failed
    case cancelled

    public var isTerminal: Bool {
        switch self {
        case .copied, .ready, .noSpeech, .failed, .cancelled:
            return true
        case .recording, .processing:
            return false
        }
    }

    public var userFace: String {
        switch self {
        case .recording: "Recording"
        case .processing: "Processing"
        case .copied: "Copied"
        case .ready: "Ready"
        case .noSpeech: "No words recognized"
        case .failed: "Failed"
        case .cancelled: "Cancelled"
        }
    }

    public var compactLabel: String {
        switch self {
        case .recording: "Recording"
        case .processing: "Processing"
        case .copied: "Copied"
        case .ready: "Ready"
        case .noSpeech: "No text"
        case .failed: "Failed"
        case .cancelled: "Cancelled"
        }
    }
}

struct RecordingActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable, Sendable {
        var status: String
        var startedAt: Date
        var micLevel: Double
        var phase: RecordingPhase
        var transcriptPreview: String? = nil
        /// Makes deferred alert content distinct from immediate result update.
        var resultPresentedAt: Date? = nil
#if DEBUG
        /// Rendering fixture only. System may suppress already-stale Activities
        /// before widget layout, so visual audit tests this branch separately.
        var designAuditIsStale: Bool? = nil
#endif
    }

    var sessionID: String
}
#endif
#endif
