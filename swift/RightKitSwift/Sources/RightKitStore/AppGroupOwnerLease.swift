import Foundation
#if canImport(Darwin)
import Darwin
#endif

/// Where an owner lease is stored inside the App Group defaults.
public struct AppGroupOwnerLeaseKeys: Sendable {
    public var owner: String
    public var session: String
    public var savedAt: String
    public init(owner: String = "rightkit.owner", session: String = "rightkit.ownerSession", savedAt: String = "rightkit.ownerSavedAt") {
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

    public init(owner: String, sessionID: String, savedAt: Date) {
        self.owner = owner
        self.sessionID = sessionID
        self.savedAt = savedAt
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
        let defaults = configuration.defaults
        guard let owner = defaults?.string(forKey: keys.owner),
              let sessionID = defaults?.string(forKey: keys.session),
              let savedAt = defaults?.object(forKey: keys.savedAt) as? Date,
              !owner.isEmpty, !sessionID.isEmpty else { return nil }
        return AppGroupOwnerLease(owner: owner, sessionID: sessionID, savedAt: savedAt)
    }

    public func save(_ configuration: AppGroupConfiguration, keys: AppGroupOwnerLeaseKeys = .init()) {
        let defaults = configuration.defaults
        defaults?.set(owner, forKey: keys.owner)
        defaults?.set(sessionID, forKey: keys.session)
        defaults?.set(savedAt, forKey: keys.savedAt)
    }

    public static func clear(_ configuration: AppGroupConfiguration, keys: AppGroupOwnerLeaseKeys = .init()) {
        let defaults = configuration.defaults
        for key in [keys.owner, keys.session, keys.savedAt] { defaults?.removeObject(forKey: key) }
    }
}
