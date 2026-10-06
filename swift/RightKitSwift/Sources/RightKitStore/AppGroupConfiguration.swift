import Foundation

/// Diagnostic events the shared App Group primitives raise. The package never logs on its own; the
/// app maps these onto its own diagnostics store (for example `RightKitDiagnostics`).
public enum AppGroupEvent: Sendable, Equatable {
    case lockOpenFallback(errno: Int32)
    case lockAcquireFailed(reason: String, errno: Int32, waitedMilliseconds: Int)
    case lockTimeout(waitedMilliseconds: Int)
    case commandSubmitFailed(requestID: UUID)
    case commandAcknowledgeFailed(requestIDs: [UUID])
}

/// Everything an app injects to bind the primitives to its own App Group.
public struct AppGroupConfiguration: @unchecked Sendable {
    public var groupID: String
    public var lockName: String
    /// When set, the lock file lives here and no container/temporary fallback is used. Tests and
    /// directory-scoped locks (a queue directory) use this.
    public var lockDirectory: URL?
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
        defaultsOverride: UserDefaults? = nil,
        lockTimeoutMicroseconds: UInt32 = 250_000,
        lockPollMicroseconds: UInt32 = 2_000,
        report: @escaping @Sendable (AppGroupEvent) -> Void = { _ in }
    ) {
        self.groupID = groupID
        self.lockName = lockName
        self.lockDirectory = lockDirectory
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
}
