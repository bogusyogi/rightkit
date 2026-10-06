import Foundation

/// Diagnostic events the shared App Group primitives raise. The package never logs on its own; the
/// app maps these onto its own diagnostics store (for example `RightKitDiagnostics`).
public enum AppGroupEvent: Sendable, Equatable {
    case lockOpenFallback(errno: Int32)
    case lockAcquireFailed(reason: String, errno: Int32, waitedMilliseconds: Int)
    case lockTimeout(waitedMilliseconds: Int)
    case commandSubmitFailed(requestID: UUID)
    case commandAcknowledgeFailed(requestIDs: [UUID])
    case persistenceFailed(key: String)
}

/// Why an App Group mutation did not happen. Nothing is reported as done unless it was verified.
public enum AppGroupStoreError: Error, Equatable, Sendable {
    /// The cross-process lock could not be taken (no entitled container, open failure, timeout).
    case lockUnavailable
    /// App Group defaults are not available (bad group ID or missing entitlement).
    case defaultsUnavailable
    /// The value written did not read back identically.
    case persistenceFailed(key: String)
    case leaseHeld(owner: String)
    case leaseLost
}

/// Everything an app injects to bind the primitives to its own App Group.
public struct AppGroupConfiguration: @unchecked Sendable {
    public var groupID: String
    public var lockName: String
    /// When set, the lock file lives here and no container/temporary fallback is used. Tests and
    /// directory-scoped locks (a queue directory) use this.
    public var lockDirectory: URL?
    /// TEST ONLY. When true and the App Group container is unavailable, the lock lives in the
    /// temporary directory. Production leaves this false: two processes that cannot reach the
    /// shared container would otherwise lock different files and believe they exclude each other.
    public var allowTemporaryLockFallback: Bool
    /// Replaces `UserDefaults(suiteName: groupID)`. Tests inject an isolated suite.
    public var defaultsOverride: UserDefaults?
    /// Total time a contended lock is retried before the operation fails closed.
    public var lockTimeoutMicroseconds: UInt32
    public var lockPollMicroseconds: UInt32
    public var report: @Sendable (AppGroupEvent) -> Void

    public init(
        groupID: String,
        lockName: String = "rightkit-cross-process.lock",
        lockDirectory: URL? = nil,
        allowTemporaryLockFallback: Bool = false,
        defaultsOverride: UserDefaults? = nil,
        lockTimeoutMicroseconds: UInt32 = 250_000,
        lockPollMicroseconds: UInt32 = 2_000,
        report: @escaping @Sendable (AppGroupEvent) -> Void = { _ in }
    ) {
        self.groupID = groupID
        self.lockName = lockName
        self.lockDirectory = lockDirectory
        self.allowTemporaryLockFallback = allowTemporaryLockFallback
        self.defaultsOverride = defaultsOverride
        self.lockTimeoutMicroseconds = lockTimeoutMicroseconds
        self.lockPollMicroseconds = lockPollMicroseconds
        self.report = report
    }

    public var defaults: UserDefaults? {
        defaultsOverride ?? UserDefaults(suiteName: groupID)
    }

    public var containerURL: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: groupID)
    }

    /// Writes `data` and reads it back; throws unless the stored bytes match. Call under the lock.
    func durableSet(_ data: Data, forKey key: String) throws {
        guard let defaults else { throw AppGroupStoreError.defaultsUnavailable }
        defaults.set(data, forKey: key)
        guard defaults.data(forKey: key) == data else {
            report(.persistenceFailed(key: key))
            throw AppGroupStoreError.persistenceFailed(key: key)
        }
    }

    /// Runs `body` under the cross-process lock; throws `.lockUnavailable` instead of running unlocked.
    func locked<T>(_ body: () throws -> T) throws -> T {
        guard let result = AppGroupFileLock.withExclusiveLock(self, { Result { try body() } }) else {
            throw AppGroupStoreError.lockUnavailable
        }
        return try result.get()
    }
}
