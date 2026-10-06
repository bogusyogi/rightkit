import Foundation

/// Server-guided retry bookkeeping: the next attempt is never earlier than the latest requested time.
public struct SyncRetryGate: Sendable {
    public private(set) var notBefore: Date?
    public init() {}

    public mutating func note(delay: TimeInterval, now: Date) {
        guard delay > 0, delay.isFinite else { return }
        let candidate = now.addingTimeInterval(delay)
        if let notBefore, notBefore > candidate { return }
        notBefore = candidate
    }

    public mutating func note(_ error: SyncTransportError, now: Date) {
        if let delay = error.retryDelay { note(delay: delay, now: now) }
    }

    /// Raise (never lower) the gate to at least `minimum` seconds from now.
    public mutating func raise(toAtLeast minimum: TimeInterval, now: Date) {
        note(delay: minimum, now: now)
    }

    public mutating func remaining(now: Date) -> TimeInterval? {
        guard let notBefore else { return nil }
        let delay = notBefore.timeIntervalSince(now)
        if delay <= 0 {
            self.notBefore = nil
            return nil
        }
        return delay
    }

    public mutating func clear() { notBefore = nil }
}

/// Exponential backoff for the coordinator's failed-work retries. Healthy idle apps never poll.
public struct SyncBackoffPolicy: Sendable {
    public var base: TimeInterval
    public var cap: TimeInterval
    public var maxAttempts: Int

    public init(base: TimeInterval = 15, cap: TimeInterval = 300, maxAttempts: Int = 8) {
        self.base = base
        self.cap = cap
        self.maxAttempts = maxAttempts
    }

    public static let `default` = SyncBackoffPolicy()

    /// `attempt` starts at 1. The server hint may only lengthen the delay.
    public func delay(attempt: Int, serverHint: TimeInterval) -> TimeInterval {
        let exponent = Double(max(attempt, 1) - 1)
        let backoff = min(cap, base * pow(2, exponent))
        return max(serverHint.isFinite ? serverHint : 0, backoff)
    }
}

/// User-facing strings. Defaults are brand-neutral; apps pass their own name or full copy.
public struct SyncMessages: Sendable {
    public var noAccount: String
    public var restricted: String
    public var undetermined: String
    public var temporarilyUnavailable: String
    public var unavailable: String
    public var noNetwork: String
    public var quotaExceeded: String
    public var permissionDenied: String
    public var busy: String
    public var pendingChanges: String
    public var localSaveFailed: String
    public var retryWhenConnected: String
    public var genericFailurePrefix: String

    public init(appName: String = "this app") {
        noAccount = "Sign in to iCloud on this device to sync \(appName)."
        restricted = "iCloud is restricted on this device."
        undetermined = "iCloud status could not be determined yet."
        temporarilyUnavailable = "iCloud is temporarily unavailable."
        unavailable = "iCloud is unavailable right now."
        noNetwork = "No network. iCloud sync will retry when you're back online."
        quotaExceeded = "Your iCloud storage is full. Free up space to sync."
        permissionDenied = "iCloud denied access. Check iCloud settings for \(appName)."
        busy = "iCloud is busy. Sync will retry shortly."
        pendingChanges = "Some changes are pending. iCloud sync will retry."
        localSaveFailed = "Couldn't save synced changes on this device. Will retry."
        retryWhenConnected = "iCloud sync will retry when connection is available."
        genericFailurePrefix = "iCloud sync will retry."
    }

    public func failure(for error: Error) -> String {
        guard let error = error as? SyncTransportError else {
            return "iCloud sync failed: \(String(error.localizedDescription.prefix(120)))"
        }
        switch error {
        case .networkUnavailable: return noNetwork
        case .notAuthenticated: return noAccount
        case .quotaExceeded: return quotaExceeded
        case .permissionDenied: return permissionDenied
        case .busy: return busy
        case let .other(code, message):
            // Name the real failure so a persistent cause (schema, provisioning) is visible on screen.
            return "\(genericFailurePrefix) (\(code): \(String(message.prefix(100))))"
        case .changeTokenExpired, .zoneNotFound, .unknownItem:
            return "\(genericFailurePrefix) (\(error))"
        }
    }
}
