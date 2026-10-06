import Foundation
#if canImport(Darwin)
import Darwin
#endif

/// Where an owner lease is stored inside the App Group defaults. The lease is ONE record under
/// `record`, replaced atomically; the three legacy keys are read only to migrate old installs.
public struct AppGroupOwnerLeaseKeys: Sendable {
    public var record: String
    public var owner: String
    public var session: String
    public var savedAt: String
    public init(
        record: String = "rightkit.ownerLease", owner: String = "rightkit.owner",
        session: String = "rightkit.ownerSession", savedAt: String = "rightkit.ownerSavedAt"
    ) {
        self.record = record
        self.owner = owner
        self.session = session
        self.savedAt = savedAt
    }
}

/// Proof that one process currently owns a long-running session (a recording, a transfer). A fresh
/// timestamp alone cannot keep a force-quit owner alive: an owner token of the form `name#pid` is
/// probed with signal 0 (never delivers a signal; permission denial stays conservative).
public struct AppGroupOwnerLease: Equatable, Sendable {
    public static let defaultTTL: TimeInterval = 30

    public let owner: String
    public let sessionID: String
    public let savedAt: Date
    /// Increments on every claim by a new owner/session; renew and release compare it, so a stale
    /// holder can never act on a lease that was taken over.
    public let epoch: UInt64

    public init(owner: String, sessionID: String, savedAt: Date, epoch: UInt64 = 0) {
        self.owner = owner
        self.sessionID = sessionID
        self.savedAt = savedAt
        self.epoch = epoch
    }

    private struct Stored: Codable {
        var owner: String
        var sessionID: String
        var savedAt: Date
        var epoch: UInt64
    }

    /// Owner token carrying the PID so other processes can detect a dead owner.
    public static func ownerToken(name: String, pid: Int32 = ProcessInfo.processInfo.processIdentifier) -> String {
        "\(name)#\(pid)"
    }

    public func isLive(at now: Date = Date(), ttl: TimeInterval = AppGroupOwnerLease.defaultTTL) -> Bool {
        let age = now.timeIntervalSince(savedAt)
        return age >= 0 && age <= ttl && ownerProcessMayExist
    }

    public var ownerProcessMayExist: Bool {
        #if canImport(Darwin)
        guard let separator = owner.lastIndex(of: "#"),
              let pid = Int32(owner[owner.index(after: separator)...]), pid > 0 else { return true }
        if kill(pid, 0) == 0 { return true }
        return errno != ESRCH
        #else
        return true
        #endif
    }

    public static func load(_ configuration: AppGroupConfiguration, keys: AppGroupOwnerLeaseKeys = .init()) -> AppGroupOwnerLease? {
        (try? configuration.locked { loadLocked(configuration, keys: keys) }) ?? nil
    }

    private static func loadLocked(_ configuration: AppGroupConfiguration, keys: AppGroupOwnerLeaseKeys) -> AppGroupOwnerLease? {
        let defaults = configuration.defaults
        if let data = defaults?.data(forKey: keys.record) {
            let decoder = JSONDecoder()
            guard let stored = try? decoder.decode(Stored.self, from: data),
                  !stored.owner.isEmpty, !stored.sessionID.isEmpty else { return nil }
            return AppGroupOwnerLease(owner: stored.owner, sessionID: stored.sessionID, savedAt: stored.savedAt, epoch: stored.epoch)
        }
        guard let owner = defaults?.string(forKey: keys.owner),
              let sessionID = defaults?.string(forKey: keys.session),
              let savedAt = defaults?.object(forKey: keys.savedAt) as? Date,
              !owner.isEmpty, !sessionID.isEmpty else { return nil }
        return AppGroupOwnerLease(owner: owner, sessionID: sessionID, savedAt: savedAt)
    }

    private static func store(_ lease: AppGroupOwnerLease, _ configuration: AppGroupConfiguration, keys: AppGroupOwnerLeaseKeys) throws {
        let encoder = JSONEncoder()
        let data = try encoder.encode(Stored(owner: lease.owner, sessionID: lease.sessionID, savedAt: lease.savedAt, epoch: lease.epoch))
        try configuration.durableSet(data, forKey: keys.record)
        for key in [keys.owner, keys.session, keys.savedAt] { configuration.defaults?.removeObject(forKey: key) }
    }

    /// Takes the lease atomically. Succeeds when none exists, when the holder is the same owner
    /// (re-claim bumps the epoch), or when the current holder is no longer live. Throws
    /// `.leaseHeld` when a live different owner holds it.
    public static func claim(
        _ configuration: AppGroupConfiguration, owner: String, sessionID: String,
        now: Date = Date(), ttl: TimeInterval = defaultTTL, keys: AppGroupOwnerLeaseKeys = .init()
    ) throws -> AppGroupOwnerLease {
        try configuration.locked {
            let current = loadLocked(configuration, keys: keys)
            if let current, current.owner != owner, current.isLive(at: now, ttl: ttl) {
                throw AppGroupStoreError.leaseHeld(owner: current.owner)
            }
            let lease = AppGroupOwnerLease(owner: owner, sessionID: sessionID, savedAt: now, epoch: (current?.epoch ?? 0) &+ 1)
            try store(lease, configuration, keys: keys)
            return lease
        }
    }

    /// Refreshes `savedAt` only while this exact lease (owner, session, epoch) is still the stored
    /// one; otherwise throws `.leaseLost` and writes nothing.
    public func renew(
        _ configuration: AppGroupConfiguration, now: Date = Date(), keys: AppGroupOwnerLeaseKeys = .init()
    ) throws -> AppGroupOwnerLease {
        try configuration.locked {
            guard let current = Self.loadLocked(configuration, keys: keys), current.matches(self) else {
                throw AppGroupStoreError.leaseLost
            }
            let renewed = AppGroupOwnerLease(owner: owner, sessionID: sessionID, savedAt: now, epoch: epoch)
            try Self.store(renewed, configuration, keys: keys)
            return renewed
        }
    }

    /// Removes the lease only when it is still this one. Returns false (and leaves a replacement
    /// lease untouched) when another owner or epoch holds it now.
    @discardableResult
    public func release(_ configuration: AppGroupConfiguration, keys: AppGroupOwnerLeaseKeys = .init()) throws -> Bool {
        try configuration.locked {
            guard let current = Self.loadLocked(configuration, keys: keys), current.matches(self) else { return false }
            for key in [keys.record, keys.owner, keys.session, keys.savedAt] { configuration.defaults?.removeObject(forKey: key) }
            guard Self.loadLocked(configuration, keys: keys) == nil else {
                throw AppGroupStoreError.persistenceFailed(key: keys.record)
            }
            return true
        }
    }

    private func matches(_ other: AppGroupOwnerLease) -> Bool {
        owner == other.owner && sessionID == other.sessionID && epoch == other.epoch
    }
}
