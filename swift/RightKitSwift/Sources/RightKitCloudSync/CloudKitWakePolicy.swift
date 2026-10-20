import Foundation
#if canImport(CloudKit)
import CloudKit
#endif
#if os(macOS)
import Security
#endif

// The pure rules behind a "wake host": a native shell that does not run the sync engine itself (a
// Tauri or Rust app whose engine lives in Rust) but wakes it when CloudKit says something changed.
// Moved out of HeardRight's macOS pill helper (`CloudSyncNotifications.swift`, PK16-2). Everything here
// is value logic so it can be proven without CloudKit, a signed build or the network; the AppKit
// host that applies it is `CloudKitWakeHost` (macOS).

/// Why the host wakes the engine. The numbers are the wire values the Rust side already reads.
public enum CloudKitWakeReason: Int32, Sendable, Equatable {
    /// CloudKit pushed a change: pull.
    case remoteChange = 1
    /// Foreground, reconnect, wake from sleep, account change or (re)subscription: pull, then push.
    case lifecycleRecovery = 2
}

// MARK: Entitlement

/// Whether this process may construct CloudKit. CloudKit TRAPS, it does not throw, when the signature
/// lacks the container or service entitlement, so a host asks first.
public enum CloudKitEntitlement: Equatable, Sendable {
    case entitled
    /// The signature does not list the container.
    case containerNotEntitled
    /// The container is listed but the `CloudKit` service is not.
    case cloudKitServiceMissing
    /// This platform cannot read its own entitlements (iOS, watchOS: the provisioning profile is the
    /// authority, and there is no API to ask). Nothing is claimed either way; the caller proceeds as
    /// a provisioned build would and needs native evidence on a device.
    case unsupportedPlatform

    /// False only when the entitlements were read and do not allow `containerIdentifier`.
    public var allowsCloudKit: Bool { self == .entitled || self == .unsupportedPlatform }

    /// The decision over already-read entitlement values (`com.apple.developer.icloud-container-identifiers`
    /// and `com.apple.developer.icloud-services`). A signature that lists no services at all is
    /// treated as entitled to all of them, as it always was.
    public static func evaluate(containers: [String]?, services: [String]?, containerIdentifier: String) -> CloudKitEntitlement {
        guard let containers, containers.contains(containerIdentifier) else { return .containerNotEntitled }
        guard let services else { return .entitled }
        return services.contains("CloudKit") || services.contains("CloudKit-Anonymous") ? .entitled : .cloudKitServiceMissing
    }

    /// The running process's own entitlements. Signing cannot change during a process's lifetime, so
    /// callers may cache the answer.
    public static func current(containerIdentifier: String) -> CloudKitEntitlement {
        #if os(macOS)
        guard let task = SecTaskCreateFromSelf(nil) else { return .containerNotEntitled }
        let containers = SecTaskCopyValueForEntitlement(task, "com.apple.developer.icloud-container-identifiers" as CFString, nil) as? [String]
        let services = SecTaskCopyValueForEntitlement(task, "com.apple.developer.icloud-services" as CFString, nil) as? [String]
        return evaluate(containers: containers, services: services, containerIdentifier: containerIdentifier)
        #else
        return .unsupportedPlatform
        #endif
    }
}

// MARK: Push filter

/// What a CloudKit push says about itself, reduced to the fields the filter decides on.
public struct CloudKitWakeCandidate: Sendable, Equatable {
    public enum Kind: Sendable, Equatable { case recordZone, other }
    public var containerIdentifier: String?
    public var kind: Kind
    public var isPrivateScope: Bool
    public var zoneName: String?
    /// The subscription the push names, when it names one (CloudKit may prune it).
    public var subscriptionID: String?

    public init(containerIdentifier: String?, kind: Kind, isPrivateScope: Bool, zoneName: String?, subscriptionID: String?) {
        self.containerIdentifier = containerIdentifier
        self.kind = kind
        self.isPrivateScope = isPrivateScope
        self.zoneName = zoneName
        self.subscriptionID = subscriptionID
    }
}

/// Accepts only the push for this app's private-database zone. A forged or foreign push never
/// wakes the engine.
public struct CloudKitWakeFilter: Sendable, Equatable {
    public enum Reason: String, Sendable, Equatable {
        case unreadablePayload = "payload"
        case containerMismatch = "container_mismatch"
        case notRecordZone = "notification_type"
        case notPrivateScope = "database_scope"
        case zoneMissing = "record_zone_missing"
        case zoneMismatch = "record_zone_mismatch"
        case subscriptionMismatch = "subscription_mismatch"
    }

    public enum Decision: Sendable, Equatable {
        case accept
        case reject(Reason)
    }

    public var containerIdentifier: String
    public var zoneName: String
    public var subscriptionID: String
    /// False (the default for a wake host): a push that names no subscription is accepted, because
    /// CloudKit may prune the id, and one that names ANOTHER subscription is still refused. True
    /// refuses a push that does not name ours.
    public var requireSubscriptionID: Bool

    public init(containerIdentifier: String, zoneName: String, subscriptionID: String, requireSubscriptionID: Bool = false) {
        self.containerIdentifier = containerIdentifier
        self.zoneName = zoneName
        self.subscriptionID = subscriptionID
        self.requireSubscriptionID = requireSubscriptionID
    }

    /// Checks run in this order, and the first failure is the reason.
    public func evaluate(_ candidate: CloudKitWakeCandidate) -> Decision {
        guard candidate.containerIdentifier == containerIdentifier else { return .reject(.containerMismatch) }
        guard candidate.kind == .recordZone else { return .reject(.notRecordZone) }
        guard candidate.isPrivateScope else { return .reject(.notPrivateScope) }
        // In private scope CloudKit can materialize the current-user owner name as an account-specific
        // id while the local zone id uses the sentinel, so the zone is matched by name; the container,
        // type and scope checks above keep that match bound to this app's zone.
        guard let received = candidate.zoneName else { return .reject(.zoneMissing) }
        guard received == zoneName else { return .reject(.zoneMismatch) }
        if let sent = candidate.subscriptionID {
            guard sent == subscriptionID else { return .reject(.subscriptionMismatch) }
        } else if requireSubscriptionID {
            return .reject(.subscriptionMismatch)
        }
        return .accept
    }

    #if canImport(CloudKit)
    /// The candidate a raw remote-notification payload describes, or nil when CloudKit cannot parse it.
    public static func candidate(userInfo: [AnyHashable: Any]) -> CloudKitWakeCandidate? {
        guard let notification = CKNotification(fromRemoteNotificationDictionary: userInfo) else { return nil }
        let zone = notification as? CKRecordZoneNotification
        let raw = (userInfo["ck"] as? [AnyHashable: Any])?["sub"] as? String
        return CloudKitWakeCandidate(
            containerIdentifier: notification.containerIdentifier,
            kind: zone == nil ? .other : .recordZone,
            isPrivateScope: zone?.databaseScope == .private,
            zoneName: zone?.recordZoneID?.zoneName,
            subscriptionID: raw ?? notification.subscriptionID
        )
    }

    /// ``evaluate(_:)`` over a raw payload; one that CloudKit cannot parse is
    /// ``Reason/unreadablePayload``.
    public func evaluate(userInfo: [AnyHashable: Any]) -> Decision {
        guard let candidate = Self.candidate(userInfo: userInfo) else { return .reject(.unreadablePayload) }
        return evaluate(candidate)
    }
    #endif
}

// MARK: Failure classes

/// How a CloudKit failure of the subscription setup is worded in logs and whether it is worth retrying.
public struct CloudKitFailureClass: Sendable, Equatable {
    public enum Category: String, Sendable, Equatable {
        case missingEntitlement = "missing_entitlement"
        case notAuthenticated = "not_authenticated"
        case network
        case serviceBackoff = "service_backoff"
        case zoneRecreate = "zone_recreate"
        case cloudKitError = "cloudkit_error"
    }

    public var category: Category
    public var retryable: Bool

    public init(category: Category, retryable: Bool) {
        self.category = category
        self.retryable = retryable
    }

    #if canImport(CloudKit)
    /// The class of the `CKError.Code` raw value `code`. The domain is not consulted.
    public static func classify(code: Int) -> CloudKitFailureClass {
        let category: Category
        switch code {
        case CKError.Code.missingEntitlement.rawValue: category = .missingEntitlement
        case CKError.Code.notAuthenticated.rawValue: category = .notAuthenticated
        case CKError.Code.networkUnavailable.rawValue, CKError.Code.networkFailure.rawValue: category = .network
        case CKError.Code.serviceUnavailable.rawValue, CKError.Code.requestRateLimited.rawValue: category = .serviceBackoff
        case CKError.Code.zoneNotFound.rawValue, CKError.Code.userDeletedZone.rawValue: category = .zoneRecreate
        default: category = .cloudKitError
        }
        let retryable: Set<Int> = [
            CKError.Code.networkUnavailable.rawValue, CKError.Code.networkFailure.rawValue,
            CKError.Code.serviceUnavailable.rawValue, CKError.Code.requestRateLimited.rawValue,
            CKError.Code.zoneBusy.rawValue, CKError.Code.zoneNotFound.rawValue,
            CKError.Code.userDeletedZone.rawValue, CKError.Code.operationCancelled.rawValue,
        ]
        return CloudKitFailureClass(category: category, retryable: retryable.contains(code))
    }

    /// The server's retry guidance in seconds (`CKErrorRetryAfterKey`), 0 when it gave none.
    public static func retryAfter(of error: Error) -> TimeInterval {
        ((error as NSError).userInfo[CKErrorRetryAfterKey] as? NSNumber)?.doubleValue ?? 0
    }
    #endif
}

/// Typed diagnostics for a failed `registerForRemoteNotifications` (NSCocoaErrorDomain-style codes the
/// system reports to the application delegate). Never carries a device token.
public enum CloudKitRemoteRegistration {
    public static func failureCategory(code: Int) -> String {
        switch code {
        case 3010: return "simulator_or_unsupported"
        case 3000: return "missing_aps_entitlement"
        default: return "registration_failed"
        }
    }
}

// MARK: Subscription lifecycle

/// Generation-fenced bookkeeping for "keep one zone subscription alive": the state machine of the
/// HeardRight wake host without any CloudKit call. A completion carries the generation it started
/// under; anything that arrives after the fence moved (account change, enable flip, zone loss) is
/// inert. Failure retries are bounded (two, after 2 s and 10 s, never earlier than the server's
/// guidance) and a healthy subscription arms no timer.
public struct CloudKitSubscriptionLifecycle: Sendable, Equatable {
    public static let retryDelays: [TimeInterval] = [2, 10]

    public private(set) var generation: UInt64 = 0
    public private(set) var inFlight = false
    public private(set) var readyGeneration: UInt64?
    public private(set) var retryPending = false
    public private(set) var retryAttempt = 0
    public private(set) var retryNotBefore: Date?

    public init() {}

    /// Moves the fence: every earlier completion becomes inert and the retry budget is renewed.
    public mutating func reset() {
        generation &+= 1
        inFlight = false
        readyGeneration = nil
        retryPending = false
        retryAttempt = 0
        retryNotBefore = nil
    }

    public func isCurrent(_ candidate: UInt64) -> Bool { candidate == generation }

    /// Starts an attempt when one is due: not already subscribed under this generation, not waiting
    /// out a retry delay, not already running. Returns the generation to fence the completion with.
    public mutating func beginAttempt(now: Date) -> UInt64? {
        guard readyGeneration != generation, !(retryNotBefore.map { $0 > now } ?? false), !inFlight else { return nil }
        inFlight = true
        return generation
    }

    /// Records a finished subscription. False (and nothing recorded) when the fence moved meanwhile.
    @discardableResult
    public mutating func finishSuccess(generation attempt: UInt64) -> Bool {
        guard isCurrent(attempt) else { return false }
        inFlight = false
        readyGeneration = attempt
        retryPending = false
        retryAttempt = 0
        retryNotBefore = nil
        return true
    }

    /// Ends an attempt that did not subscribe and will not retry by itself (the account is not
    /// available). A stale attempt changes nothing.
    public mutating func finishWithoutSubscription(generation attempt: UInt64) {
        guard isCurrent(attempt) else { return }
        inFlight = false
    }

    /// Ends a failed attempt. When `retryable`, schedules one retry and returns its delay (see
    /// ``scheduleRetry(generation:retryAfter:now:)``); nil otherwise or for a stale attempt.
    @discardableResult
    public mutating func finishFailure(generation attempt: UInt64, retryable: Bool, retryAfter: TimeInterval, now: Date) -> TimeInterval? {
        guard isCurrent(attempt) else { return nil }
        inFlight = false
        guard retryable else { return nil }
        return scheduleRetry(generation: attempt, retryAfter: retryAfter, now: now)
    }

    /// Reserves the next retry and returns how long to wait, or nil when one is already pending, the
    /// budget is spent, or the attempt is stale. The delay is the budgeted one or the server's
    /// guidance, whichever is longer.
    @discardableResult
    public mutating func scheduleRetry(generation attempt: UInt64, retryAfter: TimeInterval, now: Date) -> TimeInterval? {
        guard isCurrent(attempt), !retryPending, retryAttempt < Self.retryDelays.count else { return nil }
        retryPending = true
        retryAttempt += 1
        let delay = max(Self.retryDelays[retryAttempt - 1], retryAfter.isFinite ? retryAfter : 0)
        retryNotBefore = now.addingTimeInterval(delay)
        return delay
    }

    /// The scheduled retry came due. False when the fence moved meanwhile.
    @discardableResult
    public mutating func retryFired(generation attempt: UInt64) -> Bool {
        guard isCurrent(attempt) else { return false }
        retryPending = false
        retryNotBefore = nil
        return true
    }
}
