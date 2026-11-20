import Foundation

public struct WatchRecordingStateSnapshot: Equatable, Sendable {
    public let isRecording: Bool
    public let startedAt: Date?
    public let pendingCount: Int
}

/// Each process owns its writer. Hosts supply their entitled suite; no silent suite fallback.
public final class WatchRecordingStateStore: @unchecked Sendable {
    private let defaults: UserDefaults
    private let key: String
    private static let lock = NSLock()
    public init(defaults: UserDefaults, key: String) { self.defaults = defaults; self.key = key }
    public var snapshot: WatchRecordingStateSnapshot { Self.lock.withLock { read() } }
    private func read() -> WatchRecordingStateSnapshot {
        let values = defaults.dictionary(forKey: key) ?? [:]
        let recording = values["isRecording"] as? Bool == true
        return WatchRecordingStateSnapshot(isRecording: recording,
            startedAt: recording ? (values["startedAt"] as? TimeInterval).map(Date.init(timeIntervalSince1970:)) : nil,
            pendingCount: max(0, values["pendingCount"] as? Int ?? 0))
    }
    public func setRecording(_ recording: Bool, startedAt: Date? = nil) {
        Self.lock.withLock {
            write(WatchRecordingStateSnapshot(isRecording: recording,
                startedAt: recording ? startedAt ?? Date() : nil, pendingCount: read().pendingCount))
        }
    }
    public func setPendingCount(_ count: Int) {
        Self.lock.withLock {
            let current = read()
            write(WatchRecordingStateSnapshot(isRecording: current.isRecording,
                startedAt: current.startedAt, pendingCount: max(0, count)))
        }
    }
    private func write(_ snapshot: WatchRecordingStateSnapshot) {
        var values: [String: Any] = ["isRecording": snapshot.isRecording, "pendingCount": snapshot.pendingCount]
        if let started = snapshot.startedAt { values["startedAt"] = started.timeIntervalSince1970 }
        defaults.set(values, forKey: key)
        defaults.synchronize()
    }
}
