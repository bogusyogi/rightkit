import Foundation

/// Small cross-process state shared by Watch app & its WidgetKit extension.
/// Audio, transcript text, & telemetry never enter this store.
struct WatchRecordingStateSnapshot: Equatable {
    let isRecording: Bool
    let startedAt: Date?
    let pendingCount: Int
}

enum WatchRecordingStateStore {
    static let appGroup = "group.app.heardright.ios.native"
    static let key = "watch.recording-state.v1"

    private static var defaults: UserDefaults {
        UserDefaults(suiteName: appGroup) ?? .standard
    }

    static var snapshot: WatchRecordingStateSnapshot {
        guard let values = defaults.dictionary(forKey: key) else {
            return WatchRecordingStateSnapshot(
                isRecording: false,
                startedAt: nil,
                pendingCount: 0
            )
        }
        guard values["isRecording"] as? Bool == true else {
            return WatchRecordingStateSnapshot(
                isRecording: false,
                startedAt: nil,
                pendingCount: max(0, values["pendingCount"] as? Int ?? 0)
            )
        }
        let startedAt = (values["startedAt"] as? TimeInterval).map(Date.init(timeIntervalSince1970:))
        return WatchRecordingStateSnapshot(
            isRecording: true,
            startedAt: startedAt,
            pendingCount: max(0, values["pendingCount"] as? Int ?? 0)
        )
    }

    static func setRecording(_ isRecording: Bool, startedAt: Date? = nil) {
        let pendingCount = snapshot.pendingCount
        if isRecording {
            defaults.set([
                "isRecording": true,
                "startedAt": (startedAt ?? Date()).timeIntervalSince1970,
                "pendingCount": pendingCount,
            ], forKey: key)
        } else {
            defaults.set(["isRecording": false, "pendingCount": pendingCount], forKey: key)
        }
        defaults.synchronize()
    }

    static func setPendingCount(_ count: Int) {
        let current = snapshot
        var values: [String: Any] = [
            "isRecording": current.isRecording,
            "pendingCount": max(0, count),
        ]
        if let startedAt = current.startedAt {
            values["startedAt"] = startedAt.timeIntervalSince1970
        }
        defaults.set(values, forKey: key)
        defaults.synchronize()
    }
}
