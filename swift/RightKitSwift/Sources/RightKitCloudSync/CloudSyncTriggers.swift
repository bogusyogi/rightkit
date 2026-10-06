import Foundation
#if canImport(CloudKit)
import CloudKit
#endif
#if canImport(Network)
import Network
#endif

public enum BackgroundFetchOutcome: Equatable, Sendable {
    case newData, noData, failed
}

/// Silent-push entry point. Wire `application(_:didReceiveRemoteNotification:fetchCompletionHandler:)`
/// to `CloudSyncPush.handle`, then map `BackgroundFetchOutcome` to `UIBackgroundFetchResult`.
public struct CloudSyncPush: Sendable {
    public var containerIdentifier: String
    public var subscriptionID: String
    public var zoneName: String
    /// iOS grants ~30s; finish before it so a slow CloudKit never gets the app throttled. The
    /// coalesced request stays resumable through a later foreground/push trigger.
    public var watchdog: TimeInterval

    public init(containerIdentifier: String, subscriptionID: String, zoneName: String, watchdog: TimeInterval = 25) {
        self.containerIdentifier = containerIdentifier
        self.subscriptionID = subscriptionID
        self.zoneName = zoneName
        self.watchdog = watchdog
    }

    #if canImport(CloudKit)
    /// True only for this app's private-database zone notification.
    public func matches(userInfo: [AnyHashable: Any]) -> Bool {
        guard let n = CKNotification(fromRemoteNotificationDictionary: userInfo) as? CKRecordZoneNotification else {
            return false
        }
        return n.containerIdentifier == containerIdentifier && n.databaseScope == .private
            && n.subscriptionID == subscriptionID && n.recordZoneID?.zoneName == zoneName
    }
    #endif

    public static func outcome(for result: SyncResult) -> BackgroundFetchOutcome {
        switch result.status {
        case .ready: result.hasNewData ? .newData : .noData
        case .disabled: .noData
        case .unavailable: .failed
        }
    }

    public func run(
        coordinator: CloudSyncCoordinator,
        sleep: @escaping @Sendable (TimeInterval) async throws -> Void = { try await Task.sleep(nanoseconds: UInt64($0 * 1_000_000_000)) }
    ) async -> BackgroundFetchOutcome {
        let limit = watchdog
        return await withTaskGroup(of: BackgroundFetchOutcome.self) { group in
            group.addTask { Self.outcome(for: await coordinator.request(.remotePush)) }
            group.addTask {
                do { try await sleep(limit) } catch { return .noData }
                return .noData
            }
            let first = await group.next() ?? .noData
            group.cancelAll()
            return first
        }
    }

    #if canImport(CloudKit)
    public func handle(
        userInfo: [AnyHashable: Any], coordinator: CloudSyncCoordinator
    ) async -> BackgroundFetchOutcome {
        guard matches(userInfo: userInfo) else { return .noData }
        return await run(coordinator: coordinator)
    }
    #endif
}

/// Cross-platform trigger sources: CKAccountChanged and "network restored". The app adds its own
/// local-mutation and foreground/launch calls to `coordinator.request`.
public final class CloudSyncTriggerSources: CloudSyncLifecycleObserver, @unchecked Sendable {
    private let coordinator: CloudSyncCoordinator
    private let registerForRemoteNotifications: @Sendable () -> Void
    private let lock = NSLock()
    private var observers: [NSObjectProtocol] = []
    #if canImport(Network)
    private var monitor: NWPathMonitor?
    private var networkWasAvailable = false
    #endif

    /// - Parameter registerForRemoteNotifications: e.g. `{ Task { @MainActor in UIApplication.shared.registerForRemoteNotifications() } }`.
    public init(coordinator: CloudSyncCoordinator, registerForRemoteNotifications: @escaping @Sendable () -> Void = {}) {
        self.coordinator = coordinator
        self.registerForRemoteNotifications = registerForRemoteNotifications
    }

    public func install() async {
        #if canImport(CloudKit)
        let token = NotificationCenter.default.addObserver(forName: .CKAccountChanged, object: nil, queue: nil) { [coordinator] _ in
            Task { _ = await coordinator.request(.accountChanged) }
        }
        lock.withLock { observers.append(token) }
        #endif
        await coordinator.setLifecycleObserver(self)
    }

    public func uninstall() {
        lock.withLock {
            observers.forEach(NotificationCenter.default.removeObserver)
            observers.removeAll()
        }
        stopMonitor()
    }

    public func syncEnabledDidChange(_ enabled: Bool) {
        if enabled {
            registerForRemoteNotifications()
            startMonitor()
        } else {
            stopMonitor()
        }
    }

    public func syncWillRun(_ trigger: SyncTrigger) {
        switch trigger {
        case .launch, .foreground, .networkRestored: registerForRemoteNotifications()
        default: break
        }
    }

    private func startMonitor() {
        #if canImport(Network)
        lock.lock(); defer { lock.unlock() }
        guard monitor == nil else { return }
        let monitor = NWPathMonitor()
        self.monitor = monitor
        monitor.pathUpdateHandler = { [weak self] path in
            guard let self else { return }
            let available = path.status == .satisfied
            let restored = self.lock.withLock { () -> Bool in
                let r = available && !self.networkWasAvailable
                self.networkWasAvailable = available
                return r
            }
            if restored { Task { _ = await self.coordinator.request(.networkRestored) } }
        }
        monitor.start(queue: DispatchQueue(label: "rightkit.cloudsync.network"))
        #endif
    }

    private func stopMonitor() {
        #if canImport(Network)
        lock.withLock {
            monitor?.cancel()
            monitor = nil
            networkWasAvailable = false
        }
        #endif
    }
}
