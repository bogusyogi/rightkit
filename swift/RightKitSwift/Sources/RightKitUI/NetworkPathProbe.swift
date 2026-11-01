import Foundation
import Network

/// One-shot check of how the device is currently reaching the network.
///
/// The model download consent sheet exists to protect a metered connection.
/// Asking "Wi-Fi or cellular?" while the phone is already on Wi-Fi is a
/// question with no consequence — the download uses Wi-Fi either way — so the
/// sheet is skipped when the current path is unmetered.
enum NetworkPathProbe {
    /// True when the current path is cellular or otherwise flagged expensive
    /// (personal hotspot counts). Defaults to `false` on an indeterminate
    /// path: an unnecessary prompt is worse than a Wi-Fi download.
    static func isOnMeteredConnection() -> Bool {
        let monitor = NWPathMonitor()
        let queue = DispatchQueue(label: "app.heardright.network-path-probe")
        let semaphore = DispatchSemaphore(value: 0)
        var metered = false
        var resolved = false

        monitor.pathUpdateHandler = { path in
            guard !resolved else { return }
            resolved = true
            metered = path.usesInterfaceType(.cellular) || path.isExpensive
            semaphore.signal()
        }
        monitor.start(queue: queue)
        // NWPathMonitor delivers the current path almost immediately; the
        // timeout only guards against a stalled callback.
        _ = semaphore.wait(timeout: .now() + 1.0)
        monitor.cancel()
        return metered
    }
}
