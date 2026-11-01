#if os(iOS)
import Foundation
#if canImport(UIKit)
import UIKit
#endif

/// Wraps a post-stop decode with a UIApplication background task so iOS does
/// not suspend the app while local transcription is finishing.
///
/// Usage:
/// ```swift
/// let outcome = await BackgroundTranscriptionGuard.shared.withBackgroundTask {
///     await SharedRecordingTranscriber.transcribeCapturedAudio(audioURL, sessionID: id, store: store)
/// }
/// ```
///
/// The guard is intentionally cheap: a single shared instance, no caching, no
/// background queue. Decode is async, so contention is low.
final class BackgroundTranscriptionGuard: @unchecked Sendable {
    static let shared = BackgroundTranscriptionGuard()

    private let lock = NSLock()
    private var activeTaskID: UIBackgroundTaskIdentifier = .invalid
    private var activeReason: String = ""

    private init() {}

    func withBackgroundTask<T>(name: String = "post-stop-transcription", _ work: @escaping @Sendable () async -> T) async -> T {
#if canImport(UIKit)
        let taskID = beginBackgroundTask(named: name)
        defer { endBackgroundTask(taskID) }
#endif
        return await work()
    }

    /// Runs independent post-stop work after an owning stop task may return.
    /// Shared task IDs cannot be reference-counted: the owner would end the
    /// task while this child still awaits CloudKit.
    func withIndependentBackgroundTask<T>(name: String, _ work: @escaping @Sendable () async -> T) async -> T {
        // A fresh guard owns a distinct task ID and its expiration handler;
        // its lifetime is retained by this await until CloudKit completes.
        let independentGuard = BackgroundTranscriptionGuard()
        return await independentGuard.withBackgroundTask(name: name, work)
    }

    /// Acquire execution before returning to Shortcuts, so its completion
    /// cannot suspend independently owned terminal presentation.
    @MainActor
    func startIndependentBackgroundTask(name: String, _ work: @escaping @MainActor @Sendable () async -> Void) {
        let independentGuard = BackgroundTranscriptionGuard()
#if canImport(UIKit)
        let taskID = independentGuard.beginBackgroundTask(named: name)
#endif
        Task { @MainActor in
#if canImport(UIKit)
            defer { independentGuard.endBackgroundTask(taskID) }
#endif
            await work()
        }
    }

#if canImport(UIKit)
    func beginBackgroundTask(named name: String) -> UIBackgroundTaskIdentifier {
        lock.lock()
        let identifier = activeTaskID
        if identifier == .invalid {
            let id = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in
                self?.endActiveTask(reason: "expiration-handler")
            }
            activeTaskID = id
            activeReason = name
            lock.unlock()
            return id
        }
        lock.unlock()
        return identifier
    }

    func endBackgroundTask(_ identifier: UIBackgroundTaskIdentifier) {
        lock.lock()
        guard activeTaskID == identifier, identifier != .invalid else {
            lock.unlock()
            return
        }
        activeTaskID = .invalid
        activeReason = ""
        lock.unlock()
        UIApplication.shared.endBackgroundTask(identifier)
    }

    private func endActiveTask(reason: String) {
        lock.lock()
        let identifier = activeTaskID
        let previousReason = activeReason
        activeTaskID = .invalid
        activeReason = ""
        lock.unlock()
        guard identifier != .invalid else { return }
        NSLog("BackgroundTranscriptionGuard expiring background task (reason: %@, was: %@)", reason, previousReason)
        UIApplication.shared.endBackgroundTask(identifier)
    }
#endif
}
#endif
