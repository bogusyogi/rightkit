import Foundation

/// Unsent Watch recordings are never auto-deleted (#170). They stay until the
/// iPhone acks, the user purges them from the iPhone, or (>= 7 days old) the
/// user confirms the Watch escape hatch. Pure Foundation so both targets and
/// the swiftc smoke share one policy.
struct WatchPendingItem: Codable, Equatable, Identifiable {
    let id: String
    let startedAt: Date
    let duration: TimeInterval
    let byteCount: Int64

    init(id: String, startedAt: Date, duration: TimeInterval, byteCount: Int64) {
        self.id = id
        self.startedAt = startedAt
        self.duration = duration
        self.byteCount = byteCount
    }

    var wire: [String: Any] {
        ["id": id, "startedAt": startedAt.timeIntervalSince1970, "duration": duration, "byteCount": byteCount]
    }

    init?(wire: [String: Any]) {
        guard let id = wire["id"] as? String,
              let started = wire["startedAt"] as? TimeInterval else { return nil }
        self.init(
            id: id,
            startedAt: Date(timeIntervalSince1970: started),
            duration: wire["duration"] as? TimeInterval ?? 0,
            byteCount: (wire["byteCount"] as? Int64) ?? Int64(wire["byteCount"] as? Int ?? 0)
        )
    }
}

struct WatchPendingSummary: Equatable {
    let count: Int
    let bytes: Int64
    let oldestAge: TimeInterval
}

enum WatchPendingPolicy {
    static let capBytes: Int64 = 1_073_741_824
    static let recoveryPromptAge: TimeInterval = 24 * 3600
    static let escapeHatchAge: TimeInterval = 7 * 24 * 3600

    /// Wire keys (Watch <-> iPhone over the existing WCSession channel).
    static let listKey = "watchPendingList"          // Watch -> phone: [[String: Any]]
    static let listUpdatedKey = "watchPendingListAt"  // Watch -> phone: TimeInterval
    static let retryKey = "watchPendingRetry"         // phone -> Watch: [String] ids
    static let purgeKey = "watchPendingPurge"         // phone -> Watch: [String] ids
    static let purgedKey = "watchPendingPurged"       // Watch -> phone: [String] ids
    static let listRequestKey = "watchPendingListRequest" // phone -> Watch: Bool

    static let capMessage = "Watch storage full: open the app on iPhone to send waiting recordings."

    /// A new recording is refused once the unsent total reaches the cap.
    static func admitsNewRecording(unsentBytes: Int64) -> Bool {
        unsentBytes < capBytes
    }

    static func summary(_ items: [WatchPendingItem], now: Date) -> WatchPendingSummary {
        let oldest = items.map(\.startedAt).min()
        return WatchPendingSummary(
            count: items.count,
            bytes: items.reduce(0) { $0 + $1.byteCount },
            oldestAge: oldest.map { max(0, now.timeIntervalSince($0)) } ?? 0
        )
    }

    static func needsRecoveryPrompt(_ items: [WatchPendingItem], now: Date) -> Bool {
        !items.isEmpty && summary(items, now: now).oldestAge >= recoveryPromptAge
    }

    static func recoveryMessage(_ items: [WatchPendingItem], now: Date) -> String? {
        guard needsRecoveryPrompt(items, now: now) else { return nil }
        return "\(items.count) recording\(items.count == 1 ? "" : "s") waiting: open the app on your iPhone"
    }

    /// Watch-only destructive action, gated on the oldest unsent recording.
    static func escapeHatchAvailable(_ items: [WatchPendingItem], now: Date) -> Bool {
        !items.isEmpty && summary(items, now: now).oldestAge >= escapeHatchAge
    }

    /// IDs a purge command may delete: only ones actually unsent here.
    static func purgeTargets(requested: [String], pending: [WatchPendingItem]) -> [String] {
        let have = Set(pending.map(\.id))
        return requested.filter { have.contains($0) }
    }
}
