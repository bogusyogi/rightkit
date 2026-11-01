#if os(macOS)
import RightKitNotchCore
import Foundation
import os
import RightKitDiagnostics

/// An agent app has no window to print into, so anything worth diagnosing has
/// to go somewhere you can read it:
///
///     log stream --predicate 'subsystem == "dev.orthic.pulse"' --level debug
///
/// Two sinks sit behind this one facade. The `Logger`s above are unchanged:
/// every call site keeps its message and its privacy annotations. `record`
/// adds a durable one, a size-capped JSON Lines file under
/// `~/Library/Logs/Pulse`, for the things worth reading after the unified log
/// has rolled over. It takes an event NAME and COUNTS and nothing else, so it
/// cannot carry a message body, a token, clipboard contents or a path.
enum Log {
    static let usage = Logger(subsystem: "dev.orthic.pulse", category: "usage")
    static let sessions = Logger(subsystem: "dev.orthic.pulse", category: "sessions")

    /// The `Logger` category an event belongs to.
    enum Category: String {
        case usage
        case sessions
        case diskImage
        case settings
    }

    /// The durable file's cap. The store trims on a record boundary once the
    /// file passes `maxBytes`, down to `retainBytes`.
    static let durableMaxBytes = 512 * 1024
    static let durableRetainBytes = 256 * 1024

    private static let durableStore = JSONLDiagnosticStore(
        fileURL: URL(fileURLWithPath: AccountHome.path, isDirectory: true)
            .appendingPathComponent("Library/Logs/Pulse/diagnostics.jsonl"),
        options: StoreOptions(
            redaction: .standard,
            maxBytes: durableMaxBytes,
            retainBytes: durableRetainBytes
        )
    )

    /// Appends one event to the durable log, off the main actor.
    ///
    /// `event` is a fixed dotted name written at the call site (for example
    /// `usage.refresh_abandoned`); keep it free of anything dynamic. `counts`
    /// are integers only (a number of profiles, an OSStatus, seconds). There is
    /// deliberately no string-valued field, so a caller cannot put a message,
    /// a service name, a path or a secret into the file. A test host writes
    /// nothing.
    static func record(_ category: Category, _ event: String, counts: [String: Int] = [:]) {
        guard !Runtime.isUnderTest else { return }
        var fields = ["category": category.rawValue]
        for (name, value) in counts { fields[name] = String(value) }
        // The time is taken here, at the call site, not when the background job writes the line.
        let entry = DiagnosticEvent(name: event, fields: fields, timestamp: Date())
        let store = durableStore
        Task.detached(priority: .utility) {
            _ = await store.append(entry)
        }
    }

    /// `record` for a line written just before the process exits (Quit): the call returns once the
    /// line is appended, or after `timeout` seconds, so the exit does not lose it. It blocks the
    /// calling thread (the store is not main-actor isolated), so use it only at Quit.
    static func recordBeforeExit(
        _ category: Category, _ event: String, counts: [String: Int] = [:], timeout: TimeInterval = 2
    ) {
        guard !Runtime.isUnderTest else { return }
        var fields = ["category": category.rawValue]
        for (name, value) in counts { fields[name] = String(value) }
        let entry = DiagnosticEvent(name: event, fields: fields, timestamp: Date())
        let store = durableStore
        let done = DispatchSemaphore(value: 0)
        Task.detached(priority: .userInitiated) {
            _ = await store.append(entry)
            done.signal()
        }
        _ = done.wait(timeout: .now() + timeout)
    }
}
#endif
