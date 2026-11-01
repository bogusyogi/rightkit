import Foundation
import WatchConnectivity

/// Watch-side diagnostics, forwarded to the phone.
///
/// The Watch was a total telemetry blind spot: `DogfoodTraceStore` is not
/// compiled into this target, and it could not help if it were — this target
/// declares no App Group entitlement, so
/// `containerURL(forSecurityApplicationGroupIdentifier:)` returns nil here and
/// every write would silently no-op. Even a successful write would be
/// unreachable: `scripts/device-dogfood-trace.sh` pulls one device's App Group
/// container, and the Watch's container lives on the Watch.
///
/// So events go to the PHONE instead, over the WCSession this app already
/// maintains, and `WatchRecordingInbox` records them into the phone's trace
/// where the existing pull script finds them. `transferUserInfo` is durable and
/// queues while unreachable, which matters because the failures worth capturing
/// are exactly the ones where the phone is not reachable.
enum WatchTelemetry {
    private static let pendingKey = "watch.telemetry.pending.v1"
    private static let enabledKey = "watch.telemetry.transfer-enabled.v1"
    private static let lock = NSLock()

    static func record(_ event: String, fields: [String: Any] = [:]) {
        var payload = fields
        payload["event"] = event
        // Preserve origin identity before the phone relay records this event.
        // The phone's bundle remains useful as relay identity, but must not
        // overwrite Watch version/build in the eventual telemetry envelope.
        payload["watch_app_version"] = Bundle.main.object(
            forInfoDictionaryKey: "CFBundleShortVersionString"
        ) as? String ?? "unknown"
        payload["watch_app_build"] = Bundle.main.object(
            forInfoDictionaryKey: "CFBundleVersion"
        ) as? String ?? "unknown"
        // Stamped on the Watch: the phone may record this much later, and the
        // ordering of watch-side failures is the whole point.
        payload["watch_timestamp"] = ISO8601DateFormatter().string(from: Date())

        guard JSONSerialization.isValidJSONObject(payload) else { return }

        lock.lock()
        defer { lock.unlock() }
        var pending = loadPending()
        pending.append(payload)
        if pending.count > 200 { pending.removeFirst(pending.count - 200) }
        savePending(pending)
        flushPendingLocked()
    }

    static func flushPending() {
        lock.lock()
        defer { lock.unlock() }
        flushPendingLocked()
    }

    /// Phone-controlled radio. App Store builds default to off; TestFlight
    /// can opt in through the paired phone's application context.
    static func setTransferEnabled(_ enabled: Bool) {
        lock.lock()
        UserDefaults.standard.set(enabled, forKey: enabledKey)
        if enabled { flushPendingLocked() }
        lock.unlock()
    }

    private static func flushPendingLocked() {
        guard WCSession.isSupported() else { return }
        guard UserDefaults.standard.bool(forKey: enabledKey) else { return }
        let session = WCSession.default
        guard session.activationState == .activated else { return }
        let pending = loadPending()
        guard !pending.isEmpty else { return }
        for payload in pending {
            session.transferUserInfo([WatchTelemetryKeys.payloadKey: payload])
        }
        savePending([])
    }

    private static func loadPending() -> [[String: Any]] {
        guard let data = UserDefaults.standard.data(forKey: pendingKey),
              let decoded = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return [] }
        return decoded
    }

    private static func savePending(_ pending: [[String: Any]]) {
        guard let data = try? JSONSerialization.data(withJSONObject: pending, options: [.sortedKeys]) else { return }
        UserDefaults.standard.set(data, forKey: pendingKey)
    }
}
