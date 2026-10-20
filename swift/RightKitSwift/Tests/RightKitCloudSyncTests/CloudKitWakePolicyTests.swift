import CloudKit
import Foundation
import Testing
@testable import RightKitCloudSync

// PK16-2 (HeardRight macOS CloudKit hosting): the value rules of the wake host. Nothing here
// constructs a CKContainer or touches the network.

private let container = "iCloud.example.app"
private let zone = "ExampleZone"
private let subscription = "example-mac-v1"

private func filter(require: Bool = false) -> CloudKitWakeFilter {
    CloudKitWakeFilter(containerIdentifier: container, zoneName: zone, subscriptionID: subscription, requireSubscriptionID: require)
}

private func candidate(
    container: String? = container, kind: CloudKitWakeCandidate.Kind = .recordZone, isPrivate: Bool = true,
    zone: String? = zone, subscription: String? = subscription
) -> CloudKitWakeCandidate {
    CloudKitWakeCandidate(containerIdentifier: container, kind: kind, isPrivateScope: isPrivate, zoneName: zone, subscriptionID: subscription)
}

// MARK: Entitlement

@Test func entitlementIsDecidedFromTheContainerAndServiceLists() {
    #expect(CloudKitEntitlement.evaluate(containers: [container], services: ["CloudKit"], containerIdentifier: container) == .entitled)
    #expect(CloudKitEntitlement.evaluate(containers: [container], services: ["CloudKit-Anonymous"], containerIdentifier: container) == .entitled)
    #expect(CloudKitEntitlement.evaluate(containers: [container], services: nil, containerIdentifier: container) == .entitled, "no service list is entitled to all, as before")
    #expect(CloudKitEntitlement.evaluate(containers: [container], services: ["CloudDocuments"], containerIdentifier: container) == .cloudKitServiceMissing)
    #expect(CloudKitEntitlement.evaluate(containers: ["iCloud.other"], services: ["CloudKit"], containerIdentifier: container) == .containerNotEntitled)
    #expect(CloudKitEntitlement.evaluate(containers: nil, services: ["CloudKit"], containerIdentifier: container) == .containerNotEntitled)
}

@Test func onlyAnUnreadableOrMissingEntitlementBlocksCloudKit() {
    #expect(CloudKitEntitlement.entitled.allowsCloudKit)
    #expect(CloudKitEntitlement.unsupportedPlatform.allowsCloudKit, "nothing is claimed where entitlements cannot be read")
    #expect(!CloudKitEntitlement.containerNotEntitled.allowsCloudKit)
    #expect(!CloudKitEntitlement.cloudKitServiceMissing.allowsCloudKit)
}

@Test func theRunningProcessAnswersWithATypedResultNeverACrash() {
    let result = CloudKitEntitlement.current(containerIdentifier: "iCloud.invalid.rightkit.probe")
    #if os(macOS)
    #expect(result != .unsupportedPlatform && result != .entitled, "an unsigned test runner holds no such container")
    #else
    #expect(result == .unsupportedPlatform)
    #endif
}

// MARK: Push filter

@Test func theMatchingPushIsAccepted() {
    #expect(filter().evaluate(candidate()) == .accept)
}

@Test func aPushIsRefusedInTheStatedOrderWithTheFirstFailureAsTheReason() {
    #expect(filter().evaluate(candidate(container: "iCloud.other")) == .reject(.containerMismatch))
    #expect(filter().evaluate(candidate(container: nil)) == .reject(.containerMismatch))
    #expect(filter().evaluate(candidate(kind: .other)) == .reject(.notRecordZone))
    #expect(filter().evaluate(candidate(isPrivate: false)) == .reject(.notPrivateScope))
    #expect(filter().evaluate(candidate(zone: nil)) == .reject(.zoneMissing))
    #expect(filter().evaluate(candidate(zone: "OtherZone")) == .reject(.zoneMismatch))
    #expect(filter().evaluate(candidate(subscription: "someone-elses")) == .reject(.subscriptionMismatch))
    // Several failures at once report the earliest check.
    #expect(filter().evaluate(candidate(container: "iCloud.other", kind: .other, isPrivate: false, zone: nil)) == .reject(.containerMismatch))
    #expect(filter().evaluate(candidate(kind: .other, isPrivate: false)) == .reject(.notRecordZone))
}

@Test func aPrunedSubscriptionIdIsAcceptedUnlessTheCallerRequiresIt() {
    #expect(filter(require: false).evaluate(candidate(subscription: nil)) == .accept)
    #expect(filter(require: true).evaluate(candidate(subscription: nil)) == .reject(.subscriptionMismatch))
    #expect(filter(require: true).evaluate(candidate()) == .accept)
    #expect(filter(require: false).evaluate(candidate(subscription: "other")) == .reject(.subscriptionMismatch), "a named other subscription never wakes the engine")
}

@Test func anUnparseablePayloadIsRefusedAndTheIOSPushHelperAgrees() {
    #expect(filter().evaluate(userInfo: ["aps": ["alert": "not a CloudKit push"]]) == .reject(.unreadablePayload))
    let push = CloudSyncPush(containerIdentifier: container, subscriptionID: subscription, zoneName: zone)
    #expect(!push.matches(userInfo: [:]))
    #expect(push.requireSubscriptionID, "the iOS helper keeps its strict default")
    #expect(!CloudSyncPush(containerIdentifier: container, subscriptionID: subscription, zoneName: zone, requireSubscriptionID: false).requireSubscriptionID)
}

// MARK: Failure classes

@Test func failureClassesMatchTheShippedLogCategoriesAndRetryRules() {
    func cls(_ code: CKError.Code) -> CloudKitFailureClass { CloudKitFailureClass.classify(code: code.rawValue) }
    #expect(cls(.missingEntitlement) == CloudKitFailureClass(category: .missingEntitlement, retryable: false))
    #expect(cls(.notAuthenticated) == CloudKitFailureClass(category: .notAuthenticated, retryable: false))
    #expect(cls(.networkUnavailable) == CloudKitFailureClass(category: .network, retryable: true))
    #expect(cls(.networkFailure) == CloudKitFailureClass(category: .network, retryable: true))
    #expect(cls(.serviceUnavailable) == CloudKitFailureClass(category: .serviceBackoff, retryable: true))
    #expect(cls(.requestRateLimited) == CloudKitFailureClass(category: .serviceBackoff, retryable: true))
    #expect(cls(.zoneNotFound) == CloudKitFailureClass(category: .zoneRecreate, retryable: true))
    #expect(cls(.userDeletedZone) == CloudKitFailureClass(category: .zoneRecreate, retryable: true))
    #expect(cls(.zoneBusy) == CloudKitFailureClass(category: .cloudKitError, retryable: true))
    #expect(cls(.operationCancelled) == CloudKitFailureClass(category: .cloudKitError, retryable: true))
    #expect(cls(.quotaExceeded) == CloudKitFailureClass(category: .cloudKitError, retryable: false))
    #expect(CloudKitFailureClass.Category.zoneRecreate.rawValue == "zone_recreate")
}

@Test func theServersRetryGuidanceIsReadFromTheError() {
    let guided = NSError(domain: CKErrorDomain, code: CKError.Code.requestRateLimited.rawValue, userInfo: [CKErrorRetryAfterKey: NSNumber(value: 7.5)])
    #expect(CloudKitFailureClass.retryAfter(of: guided) == 7.5)
    #expect(CloudKitFailureClass.retryAfter(of: NSError(domain: CKErrorDomain, code: 1)) == 0)
}

@Test func remoteRegistrationFailuresHaveStableCategories() {
    #expect(CloudKitRemoteRegistration.failureCategory(code: 3010) == "simulator_or_unsupported")
    #expect(CloudKitRemoteRegistration.failureCategory(code: 3000) == "missing_aps_entitlement")
    #expect(CloudKitRemoteRegistration.failureCategory(code: 1) == "registration_failed")
}

@Test func wakeReasonsKeepTheirWireValues() {
    #expect(CloudKitWakeReason.remoteChange.rawValue == 1)
    #expect(CloudKitWakeReason.lifecycleRecovery.rawValue == 2)
}

// MARK: Subscription lifecycle

private let t0 = Date(timeIntervalSince1970: 1_000)

@Test func aHealthySubscriptionStartsOnceAndThenStaysQuiet() {
    var lifecycle = CloudKitSubscriptionLifecycle()
    let attempt = lifecycle.beginAttempt(now: t0)
    #expect(attempt == 0)
    do { let result = lifecycle.beginAttempt(now: t0); #expect(result == nil, "one attempt at a time") }
    do { let result = lifecycle.finishSuccess(generation: 0); #expect(result) }
    do { let result = lifecycle.beginAttempt(now: t0); #expect(result == nil, "already subscribed under this generation: no timer, no work") }
    #expect(lifecycle.retryAttempt == 0 && !lifecycle.retryPending && lifecycle.retryNotBefore == nil)
}

@Test func theFenceMakesEveryEarlierCompletionInert() {
    var lifecycle = CloudKitSubscriptionLifecycle()
    let old = lifecycle.beginAttempt(now: t0)!
    lifecycle.reset() // an account change, an enable flip or a zone-loss report
    do { let result = lifecycle.isCurrent(old); #expect(!result) }
    do { let result = lifecycle.finishSuccess(generation: old); #expect(!result, "a stale success records nothing") }
    #expect(lifecycle.readyGeneration == nil)
    let current = lifecycle.beginAttempt(now: t0)!
    #expect(current == lifecycle.generation && current != old)
    // A stale failure neither clears the live attempt's in-flight flag nor schedules a retry.
    do { let result = lifecycle.finishFailure(generation: old, retryable: true, retryAfter: 0, now: t0); #expect(result == nil) }
    #expect(lifecycle.inFlight)
    do { let result = lifecycle.beginAttempt(now: t0); #expect(result == nil) }
}

@Test func retriesAreBoundedHonourTheServerAndRenewAfterAReset() {
    var lifecycle = CloudKitSubscriptionLifecycle()
    var attempt = lifecycle.beginAttempt(now: t0)!
    do { let result = lifecycle.finishFailure(generation: attempt, retryable: true, retryAfter: 0, now: t0); #expect(result == 2, "first retry after 2 s") }
    do { let result = lifecycle.beginAttempt(now: t0.addingTimeInterval(1)); #expect(result == nil, "not before the delay") }
    do { let result = lifecycle.retryFired(generation: attempt); #expect(result) }
    attempt = lifecycle.beginAttempt(now: t0.addingTimeInterval(2))!
    // The second failure waits the longer of the budget (10 s) and the server's guidance (30 s).
    do { let result = lifecycle.finishFailure(generation: attempt, retryable: true, retryAfter: 30, now: t0.addingTimeInterval(2)); #expect(result == 30) }
    do { let result = lifecycle.retryFired(generation: attempt); #expect(result) }
    attempt = lifecycle.beginAttempt(now: t0.addingTimeInterval(40))!
    do { let result = lifecycle.finishFailure(generation: attempt, retryable: true, retryAfter: 0, now: t0.addingTimeInterval(40)); #expect(result == nil, "the budget of two is spent") }
    do { let result = lifecycle.beginAttempt(now: t0.addingTimeInterval(41)); #expect(result != nil || lifecycle.retryNotBefore == nil) }

    lifecycle.reset()
    let fresh = lifecycle.beginAttempt(now: t0.addingTimeInterval(50))!
    do { let result = lifecycle.finishFailure(generation: fresh, retryable: true, retryAfter: 0, now: t0.addingTimeInterval(50)); #expect(result == 2, "a reset renews the budget") }
}

@Test func aFailureThatCannotSucceedOnRetryNeverSchedulesOne() {
    var lifecycle = CloudKitSubscriptionLifecycle()
    let attempt = lifecycle.beginAttempt(now: t0)!
    do { let result = lifecycle.finishFailure(generation: attempt, retryable: false, retryAfter: 60, now: t0); #expect(result == nil) }
    #expect(!lifecycle.inFlight && !lifecycle.retryPending && lifecycle.retryNotBefore == nil)
    do { let result = lifecycle.beginAttempt(now: t0); #expect(result != nil, "the next trigger may try again") }
}

@Test func onlyOneRetryIsPendingAndAStaleTimerDoesNothing() {
    var lifecycle = CloudKitSubscriptionLifecycle()
    let attempt = lifecycle.beginAttempt(now: t0)!
    do { let result = lifecycle.scheduleRetry(generation: attempt, retryAfter: 0, now: t0); #expect(result == 2) }
    do { let result = lifecycle.scheduleRetry(generation: attempt, retryAfter: 0, now: t0); #expect(result == nil, "one pending retry at a time") }
    lifecycle.reset()
    do { let result = lifecycle.retryFired(generation: attempt); #expect(!result, "the old timer fires into a moved fence and does nothing") }
    do { let result = lifecycle.scheduleRetry(generation: attempt, retryAfter: 0, now: t0); #expect(result == nil) }
    do { let result = lifecycle.scheduleRetry(generation: lifecycle.generation, retryAfter: .infinity, now: t0); #expect(result == 2, "a non-finite guidance is ignored") }
}

@Test func anUnavailableAccountEndsTheAttemptWithoutSubscribingOrBurningARetry() {
    var lifecycle = CloudKitSubscriptionLifecycle()
    let attempt = lifecycle.beginAttempt(now: t0)!
    lifecycle.finishWithoutSubscription(generation: attempt)
    #expect(!lifecycle.inFlight && lifecycle.readyGeneration == nil && lifecycle.retryAttempt == 0)
    do { let result = lifecycle.beginAttempt(now: t0); #expect(result != nil) }
}
