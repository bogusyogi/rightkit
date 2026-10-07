import Foundation

/// Everything the engine must remember between runs, in ONE document so the change token, version
/// cache and tombstones are always mutually consistent (HeardRight kept them in three stores and
/// needed a "force full replay" repair when they drifted).
public struct CloudSyncState: Codable, Equatable, Sendable {
    public struct RemoteVersion: Codable, Equatable, Sendable {
        public var recordName: String
        public var version: Date
        public init(recordName: String, version: Date) {
            self.recordName = recordName
            self.version = version
        }
    }

    public var changeToken: SyncChangeToken?
    /// item id -> last known server version.
    public var remoteVersions: [String: RemoteVersion] = [:]
    /// item ids deleted anywhere. Block upload and download.
    public var tombstones: [String: Date] = [:]
    /// Tombstones not yet written to the server.
    public var pendingDeletions: Set<String> = []
    /// item id -> every record name seen for it (legacy duplicates, hard-delete resolution).
    public var aliases: [String: Set<String>] = [:]
    /// One-shot migration/repair markers.
    public var markers: Set<String> = []
    /// Opaque identity of the iCloud account this ledger belongs to. A different account never
    /// reuses the token, versions, aliases or tombstones recorded here.
    public var accountID: String?
    /// record names that were fetched but can never be decoded (permanent), with the time they were
    /// quarantined. Transient fetch failures are never recorded here. A scheduled full replay
    /// retries them.
    public var quarantined: [String: Date] = [:]
    /// Set by `CloudSyncEngine.adoptLegacyLedger`: this ledger was imported without an account
    /// binding. The first pass that reads the account identity binds it and KEEPS the imported
    /// token, versions, aliases and pending deletions (an unbound ledger without this flag has
    /// them dropped instead).
    public var pendingLegacyAdoption: Bool = false

    public init() {}

    // Hand-written so ledgers saved before the account/failure fields existed still load.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        changeToken = try c.decodeIfPresent(SyncChangeToken.self, forKey: .changeToken)
        remoteVersions = try c.decodeIfPresent([String: RemoteVersion].self, forKey: .remoteVersions) ?? [:]
        tombstones = try c.decodeIfPresent([String: Date].self, forKey: .tombstones) ?? [:]
        pendingDeletions = try c.decodeIfPresent(Set<String>.self, forKey: .pendingDeletions) ?? []
        aliases = try c.decodeIfPresent([String: Set<String>].self, forKey: .aliases) ?? [:]
        markers = try c.decodeIfPresent(Set<String>.self, forKey: .markers) ?? []
        accountID = try c.decodeIfPresent(String.self, forKey: .accountID)
        quarantined = try c.decodeIfPresent([String: Date].self, forKey: .quarantined) ?? [:]
        pendingLegacyAdoption = try c.decodeIfPresent(Bool.self, forKey: .pendingLegacyAdoption) ?? false
    }
}

public protocol CloudSyncStateStoring: Sendable {
    /// Missing state is a fresh install. Corrupt state must also return a fresh state (the engine
    /// then replays the full zone) after preserving the bad bytes for diagnosis.
    func load() throws -> CloudSyncState
    /// Must be durable; throw if read-back does not match.
    func save(_ state: CloudSyncState) throws
}

public final class InMemorySyncStateStore: CloudSyncStateStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var state = CloudSyncState()
    public init() {}
    public func load() throws -> CloudSyncState { lock.withLock { state } }
    public func save(_ state: CloudSyncState) throws { lock.withLock { self.state = state } }
}

/// Atomic, fsynced JSON file with read-back verification. Point `fileURL` into the app-group
/// container when a share/keyboard extension needs the same ledger.
public struct FileSyncStateStore: CloudSyncStateStoring {
    public let fileURL: URL

    public init(fileURL: URL) { self.fileURL = fileURL }

    public func load() throws -> CloudSyncState {
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return CloudSyncState() }
        let data = try Data(contentsOf: fileURL)
        do {
            return try Self.decoder.decode(CloudSyncState.self, from: data)
        } catch {
            let quarantine = fileURL.deletingLastPathComponent()
                .appendingPathComponent(fileURL.lastPathComponent + ".corrupt")
            try? FileManager.default.removeItem(at: quarantine)
            try? FileManager.default.moveItem(at: fileURL, to: quarantine)
            return CloudSyncState()
        }
    }

    public func save(_ state: CloudSyncState) throws {
        let data = try Self.encoder.encode(state)
        let directory = fileURL.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let temporary = directory.appendingPathComponent(".\(fileURL.lastPathComponent).\(UUID().uuidString).tmp")
        defer { try? FileManager.default.removeItem(at: temporary) }
        try data.write(to: temporary, options: .atomic)
        let handle = try FileHandle(forWritingTo: temporary)
        try handle.synchronize()
        try handle.close()
        if FileManager.default.fileExists(atPath: fileURL.path) {
            _ = try FileManager.default.replaceItemAt(fileURL, withItemAt: temporary)
        } else {
            try FileManager.default.moveItem(at: temporary, to: fileURL)
        }
        guard (try? Data(contentsOf: fileURL)) == data else {
            throw CloudSyncError.persistence("sync state did not persist")
        }
    }

    private static var encoder: JSONEncoder {
        let e = JSONEncoder()
        e.outputFormatting = [.sortedKeys]
        e.dateEncodingStrategy = .millisecondsSince1970
        return e
    }

    private static var decoder: JSONDecoder {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .millisecondsSince1970
        return d
    }
}

/// The user-facing on/off switch (OFF by default for apps that promise nothing leaves the device).
public protocol SyncEnabledStoring: Sendable {
    var isEnabled: Bool { get }
    func setEnabled(_ enabled: Bool)
}

public final class UserDefaultsSyncEnabledStore: SyncEnabledStoring, @unchecked Sendable {
    private let defaults: UserDefaults
    private let key: String
    public init(key: String, defaults: UserDefaults = .standard) {
        self.key = key
        self.defaults = defaults
    }
    public var isEnabled: Bool { defaults.bool(forKey: key) }
    public func setEnabled(_ enabled: Bool) { defaults.set(enabled, forKey: key) }
}

public final class InMemorySyncEnabledStore: SyncEnabledStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var value: Bool
    public init(enabled: Bool = false) { value = enabled }
    public var isEnabled: Bool { lock.withLock { value } }
    public func setEnabled(_ enabled: Bool) { lock.withLock { value = enabled } }
}

public enum CloudSyncError: LocalizedError, Equatable {
    case persistence(String)
    case malformedRecord(String)

    public var errorDescription: String? {
        switch self {
        case let .persistence(m), let .malformedRecord(m): m
        }
    }
}
