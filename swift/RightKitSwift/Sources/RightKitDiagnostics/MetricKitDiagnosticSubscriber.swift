#if canImport(MetricKit) && os(iOS) && !targetEnvironment(macCatalyst)
import Foundation
import MetricKit

/// Content-free postmortem events. Host owns bundle/distribution policy & retains this object.
/// Register/unregister on main actor; MetricKit callbacks may arrive on any queue.
public final class MetricKitDiagnosticSubscriber: NSObject, MXMetricManagerSubscriber, Sendable {
    private let sink: @Sendable (DiagnosticEvent) -> Void
    @MainActor private var registered = false

    public init(sink: @escaping @Sendable (DiagnosticEvent) -> Void) {
        self.sink = sink
        super.init()
    }

    @MainActor public func register() {
        guard !registered else { return }
        registered = true
        MXMetricManager.shared.add(self)
    }

    @MainActor public func unregister() {
        guard registered else { return }
        MXMetricManager.shared.remove(self)
        registered = false
    }

    public func didReceive(_ payloads: [MXDiagnosticPayload]) {
        // Per-callback formatter avoids sharing mutable Foundation state across delivery queues.
        let formatter = ISO8601DateFormatter()
        for payload in payloads {
            let window = [
                "window_start": formatter.string(from: payload.timeStampBegin),
                "window_end": formatter.string(from: payload.timeStampEnd),
            ]
            for diagnostic in payload.crashDiagnostics ?? [] {
                var fields = window
                let category = Self.terminationClass(diagnostic.terminationReason)
                fields["termination_class"] = category
                fields["application_version"] = diagnostic.applicationVersion
                if let signal = diagnostic.signal { fields["signal"] = signal.stringValue }
                sink(DiagnosticEvent(name: "ios_crash_diagnostic", fields: fields))
                if category == "memory_pressure" {
                    var memoryFields = window
                    memoryFields["application_version"] = diagnostic.applicationVersion
                    sink(DiagnosticEvent(name: "ios_oom_diagnostic", fields: memoryFields))
                }
            }
            for diagnostic in payload.hangDiagnostics ?? [] {
                var fields = window
                fields["application_version"] = diagnostic.applicationVersion
                let milliseconds = diagnostic.hangDuration.converted(to: .seconds).value * 1_000
                // Guard malformed/overflow values before floating-point-to-integer conversion.
                fields["duration_ms"] = milliseconds.isFinite
                    ? String(Int(min(max(0, milliseconds), Double(Int.max).nextDown))) : "0"
                sink(DiagnosticEvent(name: "ios_hang_diagnostic", fields: fields))
            }
        }
    }

    /// Raw termination text never crosses telemetry boundary.
    private static func terminationClass(_ reason: String?) -> String {
        guard let reason else { return "unknown" }
        let text = reason.lowercased()
        if text.contains("jetsam") || text.contains("memory") || text.contains("out of memory") {
            return "memory_pressure"
        }
        if text.contains("watchdog") { return "watchdog" }
        if text.contains("signal") { return "signal" }
        return "other"
    }
}
#endif
