#if os(macOS) && canImport(CloudKit) && canImport(AppKit)
import AppKit
import CloudKit
import Foundation
import Network
import ObjectiveC.runtime
import OSLog

// The AppKit half of a CloudKit "wake host", moved out of HeardRight's macOS pill helper
// (`CloudSyncNotifications.swift`, PK16-2). The sync engine lives elsewhere (Rust); this host keeps one
// zone subscription alive in the user's private database, listens for CloudKit pushes and for
// the moments a sync should be reconsidered (foreground, wake, reconnect, account change), and
// tells the engine WHY through a closure. It never pulls, pushes or mutates history itself, and never
// calls the engine on the main queue.
//
// What stays with the app: the container, zone and subscription identifiers, the C or FFI
// entry points that carry `setEnabled`, `setObserver` and `invalidateSubscription` across to its
// engine, and the signed Development/Production environment gate. Nothing here talks to a
// production container in a test: construct the host only in a build that is entitled to it.
//
// Rules the host enforces (each is a tested part of ``CloudKitWakePolicy``-level logic, not a comment):
// - Disabled, or unentitled, the host constructs no `CKContainer` at all (CloudKit traps when the
//   signature lacks the entitlement).
// - Subscription work is generation-fenced: an account change, an enable flip or a zone-loss report
//   makes every earlier completion inert, and retries are bounded.
// - Only a push for this container's private zone, bearing this subscription (or none), wakes the engine.
// - It requests no notification authorization: the subscription uses silent content-available pushes
//   only, and disabling the host stops its work without unregistering another feature's APNs
//   registration.

/// Lock-protected view of the observer, read by the queue that delivers wakes.
private final class WakeState: @unchecked Sendable {
    private let lock = NSLock()
    private var observer: (@Sendable (CloudKitWakeReason) -> Void)?
    private var enabled = false
    private var generation: UInt64 = 0

    func setObserver(_ value: (@Sendable (CloudKitWakeReason) -> Void)?) {
        lock.withLock { observer = value; generation &+= 1 }
    }

    func setEnabled(_ value: Bool) {
        lock.withLock { enabled = value; generation &+= 1 }
    }

    var hasObserver: Bool { lock.withLock { observer != nil } }

    func armed() -> (observer: @Sendable (CloudKitWakeReason) -> Void, generation: UInt64)? {
        lock.withLock { () -> (observer: @Sendable (CloudKitWakeReason) -> Void, generation: UInt64)? in
            guard enabled, let observer else { return nil }
            return (observer, generation)
        }
    }

    /// Whether a wake armed under `token` may still be delivered.
    func isCurrent(_ token: UInt64) -> Bool {
        lock.withLock { enabled && observer != nil && generation == token }
    }
}

@MainActor
public final class CloudKitWakeHost {
    public typealias Observer = @Sendable (CloudKitWakeReason) -> Void

    public struct Configuration: Sendable, Equatable {
        public var containerIdentifier: String
        public var zoneName: String
        /// Subscription ids are container- and account-scoped: a Mac and an iPhone sharing one private
        /// zone must use different ids, or they overwrite each other's subscription.
        public var subscriptionID: String
        public var logSubsystem: String
        public var logCategory: String

        public init(
            containerIdentifier: String, zoneName: String, subscriptionID: String,
            logSubsystem: String, logCategory: String = "cloud_sync_notifications"
        ) {
            self.containerIdentifier = containerIdentifier
            self.zoneName = zoneName
            self.subscriptionID = subscriptionID
            self.logSubsystem = logSubsystem
            self.logCategory = logCategory
        }
    }

    public let configuration: Configuration
    private let logger: Logger
    private let filter: CloudKitWakeFilter
    private let zoneID: CKRecordZone.ID
    private let entitlement: CloudKitEntitlement
    private let wake = WakeState()
    // Constructed lazily and only behind the enabled and entitlement gates.
    private lazy var container = CKContainer(identifier: configuration.containerIdentifier)

    private var lifecycle = CloudKitSubscriptionLifecycle()
    private var enabled = false
    private var lifecycleTokens: [NSObjectProtocol] = []
    private var pathMonitor = NWPathMonitor()
    private let pathQueue = DispatchQueue(label: "rightkit.cloud-sync-wake.reachability")
    private var pathMonitoring = false
    private var networkWasAvailable = false
    private var hookedDelegateClass: ObjectIdentifier?
    private var loggedCloudKitUnavailable = false

    public init(configuration: Configuration) {
        self.configuration = configuration
        self.logger = Logger(subsystem: configuration.logSubsystem, category: configuration.logCategory)
        self.filter = CloudKitWakeFilter(
            containerIdentifier: configuration.containerIdentifier, zoneName: configuration.zoneName,
            subscriptionID: configuration.subscriptionID, requireSubscriptionID: false
        )
        self.zoneID = CKRecordZone.ID(zoneName: configuration.zoneName, ownerName: CKCurrentUserDefaultName)
        // Signing cannot change during this process's lifetime.
        self.entitlement = CloudKitEntitlement.current(containerIdentifier: configuration.containerIdentifier)
    }

    // MARK: Public surface

    /// Where wakes go. Called off the main queue, never while the host is disabled. nil removes it.
    public func setObserver(_ observer: Observer?) {
        wake.setObserver(observer)
        installDelegateHooksIfNeeded()
        if enabled { ensureSubscription(trigger: "observer") }
    }

    /// Turns the host on or off. Turning it on in a build whose signature lacks the container
    /// entitlement stays off (and logs once): see ``CloudKitEntitlement``.
    public func setEnabled(_ requested: Bool) {
        let value = requested && cloudKitAvailable()
        guard enabled != value else {
            if value { ensureSubscription(trigger: "enabled_existing") }
            return
        }
        lifecycle.reset()
        enabled = value
        wake.setEnabled(value)
        if value {
            installDelegateHooksIfNeeded()
            startReachabilityIfNeeded()
            // No authorization prompt is requested: subscriptions use silent content-available pushes.
            ensureSubscription(trigger: "enabled")
        } else {
            stopReachabilityIfNeeded()
            logger.info("cloud_sync_notifications_disabled")
        }
    }

    /// Drops the cached subscription state after the engine saw a zone-loss error; the next attempt
    /// recreates the zone and the subscription, and every older completion is fenced off.
    public func invalidateSubscription() {
        guard enabled else { return }
        lifecycle.reset()
        ensureSubscription(trigger: "zone_recovery")
    }

    // MARK: Gates

    private func cloudKitAvailable() -> Bool {
        guard entitlement.allowsCloudKit else {
            if !loggedCloudKitUnavailable {
                loggedCloudKitUnavailable = true
                logger.error("CloudKit unavailable: build lacks iCloud entitlement; cloud sync disabled")
            }
            return false
        }
        return true
    }

    // MARK: Wake delivery

    private func emit(_ reason: CloudKitWakeReason) {
        guard let armed = wake.armed() else { return }
        let state = wake
        // The engine does its CloudKit work off the UI queue; a wake armed before the host was
        // disabled or re-pointed is dropped here.
        DispatchQueue.global(qos: .utility).async {
            guard state.isCurrent(armed.generation) else { return }
            armed.observer(reason)
        }
    }

    // MARK: Triggers

    private func startReachabilityIfNeeded() {
        guard !pathMonitoring else { return }
        pathMonitoring = true
        pathMonitor.pathUpdateHandler = { [weak self] path in
            let available = path.status == .satisfied
            Task { @MainActor [weak self] in self?.reachabilityChanged(available: available) }
        }
        pathMonitor.start(queue: pathQueue)
    }

    private func stopReachabilityIfNeeded() {
        guard pathMonitoring else { return }
        pathMonitoring = false
        networkWasAvailable = false
        pathMonitor.cancel()
        pathMonitor = NWPathMonitor()
    }

    private func reachabilityChanged(available: Bool) {
        guard enabled else { return }
        let restored = available && !networkWasAvailable
        networkWasAvailable = available
        guard restored else { return }
        ensureSubscription(trigger: "reachability")
        emit(.lifecycleRecovery)
    }

    private func installLifecycleObserversIfNeeded() {
        guard lifecycleTokens.isEmpty else { return }
        let center = NotificationCenter.default
        let foregroundNames: [Notification.Name] = [
            NSApplication.didBecomeActiveNotification,
            NSApplication.didFinishLaunchingNotification,
            NSApplication.didUnhideNotification,
        ]
        for name in foregroundNames {
            lifecycleTokens.append(center.addObserver(forName: name, object: NSApp, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.foregrounded() }
            })
        }
        lifecycleTokens.append(
            NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.wokeFromSleep() }
            }
        )
        lifecycleTokens.append(
            center.addObserver(forName: .CKAccountChanged, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.accountChanged() }
            }
        )
    }

    private func foregrounded() {
        guard enabled else { return }
        installDelegateHooksIfNeeded()
        ensureSubscription(trigger: "foreground")
        emit(.lifecycleRecovery)
    }

    private func wokeFromSleep() {
        guard enabled else { return }
        ensureSubscription(trigger: "wake")
        emit(.lifecycleRecovery)
    }

    private func accountChanged() {
        guard enabled else { return }
        // An account change invalidates the private database scope. The fence makes every old
        // account's completion inert.
        lifecycle.reset()
        ensureSubscription(trigger: "account_changed")
        emit(.lifecycleRecovery)
    }

    // MARK: Subscription

    private func ensureSubscription(trigger: String) {
        guard enabled, cloudKitAvailable(), wake.hasObserver else { return }
        if !NSApp.isRegisteredForRemoteNotifications { NSApp.registerForRemoteNotifications() }
        guard let attempt = lifecycle.beginAttempt(now: Date()) else { return }
        Task { [weak self] in await self?.runSubscription(trigger: trigger, generation: attempt) }
    }

    private func runSubscription(trigger: String, generation: UInt64) async {
        do {
            let status = try await container.accountStatus()
            guard enabled, lifecycle.isCurrent(generation) else { return }
            guard status == .available else {
                lifecycle.finishWithoutSubscription(generation: generation)
                logger.info("cloud_sync_account_unavailable status=\(String(describing: status), privacy: .public)")
                // Account status can be transient while CloudKit rehydrates credentials after wake or
                // login: keep the host self-healing through the same bounded retry lane. A later
                // successful subscription also wakes the engine, so changes that arrived during the
                // outage are not stranded.
                if status == .temporarilyUnavailable || status == .couldNotDetermine {
                    scheduleRetry(generation: generation, retryAfter: 0, trigger: "account_status")
                }
                return
            }
            // Saving the zone, then the same stable subscription id, is idempotent: it avoids a fetch
            // race and repairs a deleted subscription.
            let zones = try await container.privateCloudDatabase.modifyRecordZones(saving: [CKRecordZone(zoneID: zoneID)], deleting: [])
            for (_, result) in zones.saveResults { _ = try result.get() }
            guard enabled, lifecycle.isCurrent(generation) else { return }

            let subscription = CKRecordZoneSubscription(zoneID: zoneID, subscriptionID: configuration.subscriptionID)
            let info = CKSubscription.NotificationInfo()
            info.shouldSendContentAvailable = true
            info.shouldBadge = false
            info.alertBody = nil
            info.soundName = nil
            subscription.notificationInfo = info
            let saved = try await container.privateCloudDatabase.modifySubscriptions(saving: [subscription], deleting: [])
            for (_, result) in saved.saveResults { _ = try result.get() }

            guard enabled, lifecycle.finishSuccess(generation: generation) else { return }
            logger.info("cloud_sync_subscription_ready trigger=\(trigger, privacy: .public)")
            // APNs is a wake hint, not a durable queue: reconcile after every (re)subscription.
            emit(.lifecycleRecovery)
        } catch {
            subscriptionFailed(error, trigger: trigger, generation: generation)
        }
    }

    private func subscriptionFailed(_ error: Error, trigger: String, generation: UInt64) {
        let nsError = error as NSError
        let failure = CloudKitFailureClass.classify(code: nsError.code)
        logger.error("cloud_sync_subscription_failed category=\(failure.category.rawValue, privacy: .public) domain=\(nsError.domain, privacy: .public) code=\(nsError.code, privacy: .public) trigger=\(trigger, privacy: .public)")
        let delay = lifecycle.finishFailure(
            generation: generation, retryable: failure.retryable,
            retryAfter: CloudKitFailureClass.retryAfter(of: error), now: Date()
        )
        if enabled, let delay { armRetry(after: delay, generation: generation, trigger: "failure") }
    }

    private func scheduleRetry(generation: UInt64, retryAfter: TimeInterval, trigger: String) {
        guard enabled, cloudKitAvailable(),
              let delay = lifecycle.scheduleRetry(generation: generation, retryAfter: retryAfter, now: Date())
        else { return }
        armRetry(after: delay, generation: generation, trigger: trigger)
    }

    private func armRetry(after delay: TimeInterval, generation: UInt64, trigger: String) {
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(0, delay) * 1_000_000_000))
            guard let self, self.enabled, self.lifecycle.retryFired(generation: generation) else { return }
            self.ensureSubscription(trigger: "\(trigger)_retry")
        }
    }

    // MARK: Delegate callbacks (called by the hooks below, on the main thread)

    fileprivate func didReceiveRemoteNotification(_ userInfo: NSDictionary, delegateClass: String) {
        guard enabled, cloudKitAvailable() else {
            logger.info("cloud_sync_apns_callback_ignored reason=disabled")
            return
        }
        logger.info("cloud_sync_apns_callback_received delegate_class=\(delegateClass, privacy: .public) top_level_key_count=\(userInfo.allKeys.count, privacy: .public)")
        guard let payload = userInfo as? [AnyHashable: Any] else {
            logger.info("cloud_sync_apns_callback_rejected reason=payload_bridge")
            return
        }
        switch filter.evaluate(userInfo: payload) {
        case let .reject(reason):
            logger.info("cloud_sync_apns_callback_rejected reason=\(reason.rawValue, privacy: .public)")
        case .accept:
            logger.info("cloud_sync_apns_wake_emitted")
            emit(.remoteChange)
        }
    }

    fileprivate func didRegisterForRemoteNotifications() {
        logger.info("cloud_sync_apns_registered")
    }

    fileprivate func didFailToRegisterForRemoteNotifications(_ error: NSError) {
        // Never log device tokens, userInfo, account ids or notification contents: domain and code are
        // stable typed diagnostics for signing and entitlement failures.
        logger.error("cloud_sync_apns_registration_failed category=\(CloudKitRemoteRegistration.failureCategory(code: error.code), privacy: .public) domain=\(error.domain, privacy: .public) code=\(error.code, privacy: .public)")
    }

    private func installDelegateHooksIfNeeded() {
        installLifecycleObserversIfNeeded()
        guard let delegate = NSApp.delegate else { return }
        let delegateClass: AnyClass = object_getClass(delegate) ?? type(of: delegate)
        let identifier = ObjectIdentifier(delegateClass)
        guard hookedDelegateClass != identifier else { return }
        WakeDelegateHooks.registry.install(on: delegateClass, host: self)
        hookedDelegateClass = identifier
    }
}

/// Runtime hook that leaves the application's existing delegate in place (Tauri, Tao and SwiftUI
/// own `NSApplication.delegate`). Each hook stores and calls the original implementation, and
/// installation is idempotent per delegate class.
private final class WakeHookRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var originalRemote: [ObjectIdentifier: IMP] = [:]
    private var originalRegistered: [ObjectIdentifier: IMP] = [:]
    private var originalFailed: [ObjectIdentifier: IMP] = [:]
    private var installed: Set<ObjectIdentifier> = []
    private var activeHost: CloudKitWakeHost?

    var host: CloudKitWakeHost? { lock.withLock { activeHost } }

    func original(_ kind: WakeDelegateHooks.Kind, for delegateClass: AnyClass) -> IMP? {
        let key = ObjectIdentifier(delegateClass)
        return lock.withLock {
            switch kind {
            case .remote: originalRemote[key]
            case .registered: originalRegistered[key]
            case .failed: originalFailed[key]
            }
        }
    }

    @MainActor
    func install(on delegateClass: AnyClass, host: CloudKitWakeHost) {
        let key = ObjectIdentifier(delegateClass)
        let alreadyInstalled: Bool = lock.withLock {
            activeHost = host
            return !installed.insert(key).inserted
        }
        guard !alreadyInstalled else { return }
        hook(
            selector: Selector("application:didReceiveRemoteNotification:"),
            hookSelector: #selector(WakeDelegateHooks.rk_application(_:didReceiveRemoteNotification:)),
            on: delegateClass, kind: .remote
        )
        hook(
            selector: Selector("application:didRegisterForRemoteNotificationsWithDeviceToken:"),
            hookSelector: #selector(WakeDelegateHooks.rk_application(_:didRegisterForRemoteNotificationsWithDeviceToken:)),
            on: delegateClass, kind: .registered
        )
        hook(
            selector: Selector("application:didFailToRegisterForRemoteNotificationsWithError:"),
            hookSelector: #selector(WakeDelegateHooks.rk_application(_:didFailToRegisterForRemoteNotificationsWithError:)),
            on: delegateClass, kind: .failed
        )
    }

    private func hook(selector: Selector, hookSelector: Selector, on delegateClass: AnyClass, kind: WakeDelegateHooks.Kind) {
        guard let hookMethod = class_getInstanceMethod(WakeDelegateHooks.self, hookSelector) else { return }
        let hookIMP = method_getImplementation(hookMethod)
        let hookTypes = method_getTypeEncoding(hookMethod)
        let key = ObjectIdentifier(delegateClass)
        if let existing = class_getInstanceMethod(delegateClass, selector) {
            let implementation = method_getImplementation(existing)
            lock.withLock {
                switch kind {
                case .remote: originalRemote[key] = implementation
                case .registered: originalRegistered[key] = implementation
                case .failed: originalFailed[key] = implementation
                }
            }
        }
        if !class_addMethod(delegateClass, selector, hookIMP, hookTypes) {
            guard let existing = class_getInstanceMethod(delegateClass, selector) else { return }
            method_setImplementation(existing, hookIMP)
        }
    }
}

private final class WakeDelegateHooks: NSObject {
    enum Kind { case remote, registered, failed }

    static let registry = WakeHookRegistry()

    // AppKit delivers remote notifications to the app delegate on the main thread, so the hook is
    // main-actor isolated: `userInfo` is then used on one actor only and never crosses a boundary.
    @MainActor
    @objc(rk_application:didReceiveRemoteNotification:)
    func rk_application(_ application: NSApplication, didReceiveRemoteNotification userInfo: NSDictionary) {
        let delegateClass: AnyClass = object_getClass(self) ?? WakeDelegateHooks.self
        let name = NSStringFromClass(delegateClass)
        let host = Self.registry.host
        host?.didReceiveRemoteNotification(userInfo, delegateClass: name)
        if let imp = Self.registry.original(.remote, for: delegateClass) {
            typealias Original = @convention(c) (AnyObject, Selector, NSApplication, NSDictionary) -> Void
            unsafeBitCast(imp, to: Original.self)(self, Selector("application:didReceiveRemoteNotification:"), application, userInfo)
        }
    }

    @objc(rk_application:didRegisterForRemoteNotificationsWithDeviceToken:)
    func rk_application(_ application: NSApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: NSData) {
        let delegateClass: AnyClass = object_getClass(self) ?? WakeDelegateHooks.self
        let host = Self.registry.host
        MainActor.assumeIsolated { host?.didRegisterForRemoteNotifications() }
        if let imp = Self.registry.original(.registered, for: delegateClass) {
            typealias Original = @convention(c) (AnyObject, Selector, NSApplication, NSData) -> Void
            unsafeBitCast(imp, to: Original.self)(self, Selector("application:didRegisterForRemoteNotificationsWithDeviceToken:"), application, deviceToken)
        }
    }

    @objc(rk_application:didFailToRegisterForRemoteNotificationsWithError:)
    func rk_application(_ application: NSApplication, didFailToRegisterForRemoteNotificationsWithError error: NSError) {
        let delegateClass: AnyClass = object_getClass(self) ?? WakeDelegateHooks.self
        let host = Self.registry.host
        MainActor.assumeIsolated { host?.didFailToRegisterForRemoteNotifications(error) }
        if let imp = Self.registry.original(.failed, for: delegateClass) {
            typealias Original = @convention(c) (AnyObject, Selector, NSApplication, NSError) -> Void
            unsafeBitCast(imp, to: Original.self)(self, Selector("application:didFailToRegisterForRemoteNotificationsWithError:"), application, error)
        }
    }
}
#endif
