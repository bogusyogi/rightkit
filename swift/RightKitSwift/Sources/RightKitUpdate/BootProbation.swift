import Foundation

/// The record kept between `begin` and the first healthy boot.
public struct BootProbationRecord: Codable, Sendable, Equatable {
    public var fromVersion: String
    public var toVersion: String
    /// Boots of `toVersion` seen without a healthy acknowledgement.
    public var bootAttempts: Int

    public init(fromVersion: String, toVersion: String, bootAttempts: Int = 0) {
        self.fromVersion = fromVersion
        self.toVersion = toVersion
        self.bootAttempts = bootAttempts
    }
}

/// Host-supplied persistence. The policy never touches disk itself.
public protocol BootProbationStorage: Sendable {
    func read() throws -> BootProbationRecord?
    func write(_ record: BootProbationRecord) throws
    func clear() throws
}

/// JSON file persistence (write is atomic).
public struct FileBootProbationStorage: BootProbationStorage {
    public let url: URL
    public init(url: URL) { self.url = url }

    public func read() throws -> BootProbationRecord? {
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        return try JSONDecoder().decode(BootProbationRecord.self, from: Data(contentsOf: url))
    }

    public func write(_ record: BootProbationRecord) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(record).write(to: url, options: .atomic)
    }

    public func clear() throws { try? FileManager.default.removeItem(at: url) }
}

public enum BootProbationDecision: Sendable, Equatable {
    case none
    /// The installed version is on probation; call ``BootProbation/acknowledgeHealthy()`` once it is healthy.
    case probation(toVersion: String, bootAttempts: Int)
    /// The new version never acknowledged within the allowed boots. There is no backup copy to restore
    /// (settled decision D1): the host should tell the user and offer a reinstall of `restoreVersion`
    /// from the release feed.
    case failed(toVersion: String, restoreVersion: String, bootAttempts: Int)
    /// A record for a different version (superseded); it was cleared.
    case stale
}

/// Boot-probation state machine for runtime updates, like `createBootProbation`: pure policy that
/// counts boots and decides. Unlike the TypeScript package it keeps no rollback path, because no
/// install step keeps a `.previous` copy.
public actor BootProbation {
    private let storage: any BootProbationStorage
    private let maxBootAttempts: Int

    /// - Parameter maxBootAttempts: unacknowledged boots tolerated before ``BootProbationDecision/failed``. Default 2.
    public init(storage: any BootProbationStorage, maxBootAttempts: Int = 2) {
        precondition(maxBootAttempts >= 1, "maxBootAttempts must be a positive integer")
        self.storage = storage
        self.maxBootAttempts = maxBootAttempts
    }

    /// Records an install that is about to take effect. Call right before relaunch.
    public func begin(fromVersion: String, toVersion: String) throws {
        try storage.write(BootProbationRecord(fromVersion: fromVersion, toVersion: toVersion))
    }

    /// Call once at every app start with the running version.
    public func evaluateBoot(currentVersion: String) throws -> BootProbationDecision {
        guard var record = try storage.read() else { return .none }
        guard SemanticVersion(record.toVersion, allowShort: true) == SemanticVersion(currentVersion, allowShort: true),
              SemanticVersion(currentVersion, allowShort: true) != nil
        else {
            try storage.clear()
            return .stale
        }
        record.bootAttempts += 1
        if record.bootAttempts > maxBootAttempts {
            try storage.clear()
            return .failed(toVersion: record.toVersion, restoreVersion: record.fromVersion, bootAttempts: record.bootAttempts)
        }
        try storage.write(record)
        return .probation(toVersion: record.toVersion, bootAttempts: record.bootAttempts)
    }

    /// Call when the new version has proven healthy. Clears the record.
    public func acknowledgeHealthy() throws { try storage.clear() }
}
