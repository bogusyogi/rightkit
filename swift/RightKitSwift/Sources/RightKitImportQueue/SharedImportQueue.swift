import Foundation
import RightKitStore

/// Durable, lock-protected, file-per-item queue between a share extension (writer) and its app
/// (reader). Items survive extension and app death until `acknowledge` removes them.
public struct SharedImportQueue: @unchecked Sendable {
    public enum QueueError: LocalizedError, Equatable {
        case lockUnavailable
        case storageUnavailable
        case unreadable
        case writeFailed

        public var errorDescription: String? {
            switch self {
            case .lockUnavailable: "Shared imports are busy. Try again."
            case .storageUnavailable: "Shared import storage is unavailable."
            case .unreadable: "Shared imports could not be read."
            case .writeFailed: "The shared import could not be queued."
            }
        }
    }

    private let configuration: ShareIntakeConfiguration
    private let directory: URL?

    public init(configuration: ShareIntakeConfiguration) {
        self.configuration = configuration
        directory = configuration.containerRoot?
            .appendingPathComponent(configuration.queueDirectoryName, isDirectory: true)
    }

    public func enqueue(_ payload: PendingSharePayload) throws {
        try withLock {
            try migrateLegacyLocked()
            try write(payload)
        }
    }

    /// Oldest first. Throws `.unreadable` rather than silently skipping a corrupt item.
    public func pending() throws -> [PendingSharePayload] {
        try withLock {
            try migrateLegacyLocked()
            guard let directory else { throw QueueError.storageUnavailable }
            let urls = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
                .filter { $0.pathExtension == "json" }
                .sorted { $0.lastPathComponent < $1.lastPathComponent }
            return try urls.map { url in
                guard let data = try? Data(contentsOf: url), let payload = decode(data) else { throw QueueError.unreadable }
                return payload
            }
        }
    }

    public func acknowledge(_ id: UUID) throws {
        try withLock {
            let url = fileURL(for: id)
            if FileManager.default.fileExists(atPath: url.path) {
                do { try FileManager.default.removeItem(at: url) } catch { throw QueueError.writeFailed }
            }
        }
    }

    public func markFailed(_ id: UUID, error: Error, now: Date = Date()) throws {
        try withLock {
            guard let data = try? Data(contentsOf: fileURL(for: id)), var payload = decode(data) else {
                throw QueueError.unreadable
            }
            payload.lastError = String(error.localizedDescription.prefix(300))
            payload.lastAttemptAt = now
            try write(payload)
        }
    }

    private func fileURL(for id: UUID) -> URL {
        directory!.appendingPathComponent(id.uuidString).appendingPathExtension("json")
    }

    private func write(_ payload: PendingSharePayload) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        guard let data = try? encoder.encode(payload) else { throw QueueError.writeFailed }
        let url = fileURL(for: payload.id)
        let temporary = url.appendingPathExtension("tmp")
        do {
            try durableWrite(data, to: temporary)
            try step("rename")
            // rename(2) is atomic and replaces an existing file in one step.
            guard rename(temporary.path, url.path) == 0 else { throw QueueError.writeFailed }
            try step("fsync-directory")
            try fsyncDirectory(url.deletingLastPathComponent())
        } catch {
            try? FileManager.default.removeItem(at: temporary)
            throw QueueError.writeFailed
        }
    }

    /// Test seam: throws at the named step to prove no partial file survives. Nil in production.
    var faultInjection: (@Sendable (String) throws -> Void)?

    private func step(_ name: String) throws { try faultInjection?(name) }

    private func durableWrite(_ data: Data, to url: URL) throws {
        let fd = open(url.path, O_WRONLY | O_CREAT | O_TRUNC, 0o600)
        guard fd >= 0 else { throw QueueError.writeFailed }
        defer { close(fd) }
        try step("write")
        var offset = 0
        try data.withUnsafeBytes { raw in
            while offset < raw.count {
                let n = Foundation.write(fd, raw.baseAddress! + offset, raw.count - offset)
                if n < 0 && errno == EINTR { continue }
                guard n > 0 else { throw QueueError.writeFailed }
                offset += n
            }
        }
        try step("fsync-file")
        guard fsync(fd) == 0 else { throw QueueError.writeFailed }
    }

    private func fsyncDirectory(_ directory: URL) throws {
        let fd = open(directory.path, O_RDONLY)
        guard fd >= 0 else { throw QueueError.writeFailed }
        defer { close(fd) }
        guard fsync(fd) == 0 else { throw QueueError.writeFailed }
    }

    private func decode(_ data: Data) -> PendingSharePayload? {
        for strategy in [JSONDecoder.DateDecodingStrategy.iso8601, .deferredToDate] {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = strategy
            if let payload = try? decoder.decode(PendingSharePayload.self, from: data) { return payload }
        }
        return nil
    }

    private var legacyKeys: [String] {
        [configuration.legacyQueueKey, configuration.legacyPayloadKey].compactMap { $0 }
    }

    private func migrateLegacyLocked() throws {
        guard let defaults = configuration.defaults,
              legacyKeys.contains(where: { defaults.data(forKey: $0) != nil }) else { return }
        guard let payloads = legacyPayloads(defaults: defaults) else { throw QueueError.unreadable }
        for payload in payloads where !FileManager.default.fileExists(atPath: fileURL(for: payload.id).path) {
            try write(payload)
        }
        for key in legacyKeys { defaults.removeObject(forKey: key) }
    }

    private func legacyPayloads(defaults: UserDefaults) -> [PendingSharePayload]? {
        guard let data = legacyKeys.lazy.compactMap({ defaults.data(forKey: $0) }).first else { return nil }
        for strategy in [JSONDecoder.DateDecodingStrategy.deferredToDate, .iso8601] {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = strategy
            if let payloads = try? decoder.decode([PendingSharePayload].self, from: data) { return payloads }
            if let payload = try? decoder.decode(PendingSharePayload.self, from: data) { return [payload] }
        }
        return nil
    }

    private func withLock<T>(_ body: () throws -> T) throws -> T {
        guard let directory else { throw QueueError.storageUnavailable }
        do { try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true) }
        catch { throw QueueError.storageUnavailable }
        let lock = AppGroupConfiguration(
            groupID: configuration.groupID,
            lockName: ".queue.lock",
            lockDirectory: directory
        )
        guard let result = AppGroupFileLock.withExclusiveLock(lock, { Result { try body() } }) else {
            throw QueueError.lockUnavailable
        }
        return try result.get()
    }
}
