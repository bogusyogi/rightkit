import Foundation

public enum WatchRecordingFailureKind: Sendable { case modelRequired, noSpeech, transient }
public enum WatchRecordingProcessingEvent: Sendable {
    case processed(String)
    case terminal(String, reason: String)
    case deferred(String, error: String)
}
public struct WatchRecordingDrainResult: Sendable {
    public let pendingCount: Int
    public let wasCancelled: Bool
    public let alreadyRunning: Bool
    public init(pendingCount: Int, wasCancelled: Bool = false, alreadyRunning: Bool = false) {
        self.pendingCount = pendingCount; self.wasCancelled = wasCancelled; self.alreadyRunning = alreadyRunning
    }
    public var succeeded: Bool { pendingCount == 0 && !wasCancelled && !alreadyRunning }
}

/// One instance per storage namespace. A second drain returns a typed busy result.
/// Host preparation composes ASR/polish; synchronous commit saves a stable record ID atomically
/// with transcript deduplication, so replay after interruption cannot insert duplicates.
@MainActor
public final class WatchRecordingProcessor<Prepared> {
    public typealias Prepare = @MainActor (WatchReceivedRecording, URL, [String: Any]) async throws -> Prepared
    public typealias Commit = @MainActor (WatchReceivedRecording, Prepared) throws -> Void
    private let storage: WatchRecordingStorage
    private let prepare: Prepare
    private let commit: Commit
    private let classify: (Error) -> WatchRecordingFailureKind
    private let notify: (WatchRecordingProcessingEvent) -> Void
    private let maximumTransientRetries: Int
    private var running = false

    public init(storage: WatchRecordingStorage, maximumTransientRetries: Int = 2,
                classify: @escaping (Error) -> WatchRecordingFailureKind,
                prepare: @escaping Prepare, commit: @escaping Commit,
                notify: @escaping (WatchRecordingProcessingEvent) -> Void = { _ in }) {
        self.storage = storage; self.maximumTransientRetries = max(0, maximumTransientRetries)
        self.classify = classify; self.prepare = prepare; self.commit = commit; self.notify = notify
    }
    public func processPending() async throws -> WatchRecordingDrainResult {
        guard !running else { return try result(cancelled: Task.isCancelled, busy: true) }
        running = true
        defer { running = false }
        let storage = self.storage
        // File/defaults I/O leaves main actor; storage owns its process-wide lock.
        let records = try await Task.detached(priority: .utility) { try storage.records() }.value
        for record in records where !storage.isFinished(record.id) {
            guard !Task.isCancelled else { return try result(cancelled: true) }
            let url = storage.audioURL(for: record)
            guard FileManager.default.fileExists(atPath: url.path) else {
                try finishTerminal(record.id, reason: "missing_audio")
                continue
            }
            var prepared: Prepared?
            while true {
                do {
                    try Task.checkCancellation()
                    let metadataData = try await Task.detached(priority: .utility) {
                        try storage.metadataData(for: record)
                    }.value
                    try Task.checkCancellation()
                    let metadata = try JSONSerialization.jsonObject(with: metadataData) as? [String: Any] ?? [:]
                    prepared = try await prepare(record, url, metadata)
                    try Task.checkCancellation()
                    break
                } catch {
                    if Task.isCancelled || error is CancellationError { return try result(cancelled: true) }
                    switch classify(error) {
                    case .modelRequired:
                        storage.lastError = error.localizedDescription
                        notify(.deferred(record.id, error: error.localizedDescription))
                        return try result()
                    case .noSpeech:
                        try finishTerminal(record.id, reason: "no_speech")
                    case .transient:
                        let previous = storage.retryCount(record.id)
                        guard previous < maximumTransientRetries else {
                            try finishTerminal(record.id, reason: "decode_exhausted")
                            break
                        }
                        // Persist retry only after delay succeeds; cancellation consumes no retry.
                        do { try await Task.sleep(nanoseconds: previous == 0 ? 250_000_000 : 500_000_000) }
                        catch { return try result(cancelled: true) }
                        guard !Task.isCancelled else { return try result(cancelled: true) }
                        storage.setRetryCount(previous + 1, for: record.id)
                        continue
                    }
                    break
                }
            }
            guard let prepared else { continue }
            guard !Task.isCancelled else { return try result(cancelled: true) }
            do {
                // No suspension between host commit & processed marker. Commit must be idempotent
                // using record.id because process termination may occur between these writes.
                try commit(record, prepared)
                try storage.markProcessed(record.id)
                storage.setRetryCount(nil, for: record.id)
                storage.lastError = nil
                notify(.processed(record.id))
            } catch {
                if Task.isCancelled || error is CancellationError { return try result(cancelled: true) }
                storage.lastError = error.localizedDescription
                notify(.deferred(record.id, error: error.localizedDescription))
                // Persistence failures remain pending; they never consume decode retry budget.
            }
        }
        return try result(cancelled: Task.isCancelled)
    }
    private func finishTerminal(_ id: String, reason: String) throws {
        try storage.markTerminal(id, reason: reason)
        storage.setRetryCount(nil, for: id)
        notify(.terminal(id, reason: reason))
    }
    private func result(cancelled: Bool = false, busy: Bool = false) throws -> WatchRecordingDrainResult {
        WatchRecordingDrainResult(pendingCount: try storage.status().pendingCount,
            wasCancelled: cancelled, alreadyRunning: busy)
    }
}
