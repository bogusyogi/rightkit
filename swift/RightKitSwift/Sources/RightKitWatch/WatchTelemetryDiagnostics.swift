import Foundation

/// WatchOS has no MetricKit diagnostic subscriber. Keep diagnostics tied to
/// scene lifecycle: an active marker lets the next launch report an unknown
/// prior termination without an always-on watchdog timer.
final class WatchTelemetryDiagnostics {
    static let shared = WatchTelemetryDiagnostics()

    private static let activeSessionKey = "watch.telemetry.active-session.v1"
    // Historical wire names remain documented for server compatibility;
    // `watch_main_thread_stall` and its old hangThreshold watchdog are no
    // longer emitted from an idle Watch process.
    private static let hangThreshold: TimeInterval = 10
    private static let lock = NSLock()
    private static var started = false
    private static var sceneIsActive = false

    private init() {}

    static func register() {
        lock.lock()
        guard !started else {
            lock.unlock()
            return
        }
        started = true
        let previousSessionWasActive = UserDefaults.standard.bool(forKey: activeSessionKey)
        // A process can launch to service a Control/complication request while
        // its scene is still backgrounded. Do not mark that launch active;
        // scenePhase is the only source of truth for an active Watch session.
        UserDefaults.standard.set(false, forKey: activeSessionKey)
        lock.unlock()

        if previousSessionWasActive {
            WatchTelemetry.record("watch_previous_session_terminated", fields: [
                "termination_class": "unknown",
                "possible_memory_pressure": true,
            ])
        }

    }

    static func setActive(_ isActive: Bool) {
        lock.lock()
        sceneIsActive = isActive
        UserDefaults.standard.set(isActive, forKey: activeSessionKey)
        lock.unlock()
    }
}
