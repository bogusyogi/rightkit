import Foundation

// Device-side license housekeeping shared by every app that lets a user remove a license from a
// device: a durable queue of server-side removals that must survive app kills and offline
// periods, a device-only "do not re-consume a continuity bridge" flag, and the `needsReconnect`
// predicate. Product policy (which bridge endpoints exist, pricing, UI copy) stays in the app.

/// Device-only key-value persistence for license housekeeping. Never synced: a removal queued on
/// one device must not be replayed by another. `UserDefaultsLicenseDeviceState` is the default;
/// supply a Keychain-backed implementation when queued activation tokens must not sit in
/// `UserDefaults`.
public protocol LicenseDeviceStateStoring: Sendable {
    func data(forKey key: String) -> Data?
    /// `nil` removes the value.
    func set(_ data: Data?, forKey key: String)
}

public final class UserDefaultsLicenseDeviceState: LicenseDeviceStateStoring, @unchecked Sendable {
    private let defaults: UserDefaults
    public init(defaults: UserDefaults = .standard) { self.defaults = defaults }
    public func data(forKey key: String) -> Data? { defaults.data(forKey: key) }
    public func set(_ data: Data?, forKey key: String) {
        if let data { defaults.set(data, forKey: key) } else { defaults.removeObject(forKey: key) }
    }
}

public final class InMemoryLicenseDeviceState: LicenseDeviceStateStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]
    public init() {}
    public func data(forKey key: String) -> Data? { lock.withLock { values[key] } }
    public func set(_ data: Data?, forKey key: String) { lock.withLock { values[key] = data } }
}

/// One server-side removal owed by this device. Each carries the ORIGINAL identity of what it
/// removes, so a later activation, device-id change or account change can never redirect it.
public enum PendingLicenseRemoval: Codable, Equatable, Hashable, Sendable {
    /// A renewable device lease (`LicenseActivationClient.removeLease(deviceIdHash:leaseId:)`).
    case lease(deviceIdHash: String, leaseId: String)
    /// A legacy activation (`LicenseActivationClient.deactivate(activationToken:)`).
    case activation(activationToken: String)

    /// The removal owed for `license`: its lease when it has one, else its activation token, else
    /// nothing (a record the server never issued a token for has no server seat to free).
    public static func owed(by license: StoredLicense) -> PendingLicenseRemoval? {
        if let leaseId = license.leaseId?.trimmingCharacters(in: .whitespacesAndNewlines), !leaseId.isEmpty {
            return .lease(deviceIdHash: LicenseActivationClient.deviceIDHash(license.deviceId), leaseId: leaseId)
        }
        if let token = license.activationToken?.trimmingCharacters(in: .whitespacesAndNewlines), !token.isEmpty {
            return .activation(activationToken: token)
        }
        return nil
    }
}

/// Durable, idempotent queue of server-side removals. Enqueue BEFORE the first network attempt:
/// endpoint idempotency makes a retry safe, but a dropped request is not an acknowledgement and
/// does not free the server seat. Entries leave the queue only after the server accepted them.
///
/// Retries are single-flight. Concurrent `retry` calls on one queue join the wave already in
/// flight and return its result. Waves of different queue instances over the same persisted key
/// (an app relaunch, or two controllers in one process) run one after another, and each re-checks
/// the persisted queue before every post. So a queued removal is posted at most once per wave,
/// and an acknowledged entry is never posted again.
public actor PendingLicenseRemovalQueue {
    private let storage: any LicenseDeviceStateStoring
    private let key: String
    private var retrying = false
    private var joiners: [CheckedContinuation<[PendingLicenseRemoval], Never>] = []

    /// Persisted keys whose wave is running in the current task, so a `deliver` closure that
    /// calls `retry` again returns `[]` instead of waiting on its own wave.
    @TaskLocal private static var activeWaveKeys: Set<String> = []

    /// - Parameter namespace: prefix for the persisted key (e.g. the app id), so two apps or an app
    ///   and its extension sharing a suite never collide.
    public init(storage: any LicenseDeviceStateStoring, namespace: String) {
        self.storage = storage
        self.key = "\(namespace).license.pendingRemovals"
    }

    public var pending: [PendingLicenseRemoval] { load() }

    /// Adds `removal` unless an identical entry is already queued. Persisted before returning.
    public func enqueue(_ removal: PendingLicenseRemoval) {
        var current = load()
        guard !current.contains(removal) else { return }
        current.append(removal)
        save(current)
    }

    /// Delivers every queued removal through `client`, removing each one as soon as the server
    /// accepts it. Returns the delivered entries. Safe to call repeatedly (launch, foreground,
    /// after a successful refresh); a call while a retry is in flight joins that retry and
    /// returns its result instead of posting again.
    @discardableResult
    public func retry(using client: LicenseActivationClient) async -> [PendingLicenseRemoval] {
        await retry { removal in
            switch removal {
            case let .lease(hash, leaseId): try await client.removeLease(deviceIdHash: hash, leaseId: leaseId)
            case let .activation(token): try await client.deactivate(activationToken: token)
            }
        }
    }

    /// `retry(using:)` with an injected delivery (tests, custom transports). A joining caller's
    /// `deliver` is not used: the wave in flight delivers with its own.
    @discardableResult
    public func retry(deliver: @Sendable (PendingLicenseRemoval) async throws -> Void) async -> [PendingLicenseRemoval] {
        if Self.activeWaveKeys.contains(key) { return [] }
        if retrying {
            return await withCheckedContinuation { joiners.append($0) }
        }
        retrying = true
        await RemovalDeliveryLock.shared.acquire(key)
        let delivered = await Self.$activeWaveKeys.withValue(Self.activeWaveKeys.union([key])) {
            await wave(deliver: deliver)
        }
        RemovalDeliveryLock.shared.release(key)
        retrying = false
        let waiting = joiners
        joiners = []
        for joiner in waiting { joiner.resume(returning: delivered) }
        return delivered
    }

    /// One pass over the entries queued when the wave started. Each is re-checked against
    /// storage right before it is posted (so an entry acknowledged elsewhere is skipped), attempted
    /// once, and dropped from storage as soon as the server accepts it. An entry queued mid-wave is
    /// kept for the next wave.
    private func wave(deliver: @Sendable (PendingLicenseRemoval) async throws -> Void) async -> [PendingLicenseRemoval] {
        var delivered: [PendingLicenseRemoval] = []
        for removal in load() where load().contains(removal) {
            do {
                try await deliver(removal)
            } catch {
                continue // Offline or rejected for now: stays queued for the next retry.
            }
            delivered.append(removal)
            // Re-read after the await: a removal queued meanwhile must survive.
            save(load().filter { $0 != removal })
        }
        return delivered
    }

    private func load() -> [PendingLicenseRemoval] {
        guard let data = storage.data(forKey: key),
              let decoded = try? JSONDecoder().decode([PendingLicenseRemoval].self, from: data) else { return [] }
        return decoded
    }

    private func save(_ removals: [PendingLicenseRemoval]) {
        guard !removals.isEmpty else { storage.set(nil, forKey: key); return }
        storage.set(try? JSONEncoder().encode(removals), forKey: key)
    }
}

/// Process-wide FIFO lock per persisted queue key. A holder never awaits another key's lock, and
/// a nested wave on the same key is short-circuited by `activeWaveKeys`, so it cannot deadlock.
final class RemovalDeliveryLock: @unchecked Sendable {
    static let shared = RemovalDeliveryLock()
    private let lock = NSLock()
    private var held: Set<String> = []
    private var waiters: [String: [CheckedContinuation<Void, Never>]] = [:]

    func acquire(_ key: String) async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            let granted: Bool = lock.withLock {
                if held.contains(key) { waiters[key, default: []].append(continuation); return false }
                held.insert(key); return true
            }
            if granted { continuation.resume() }
        }
    }

    /// Hands the lock straight to the next waiter, or frees it.
    func release(_ key: String) {
        let next: CheckedContinuation<Void, Never>? = lock.withLock {
            guard var queue = waiters[key], !queue.isEmpty else { held.remove(key); return nil }
            let first = queue.removeFirst()
            waiters[key] = queue.isEmpty ? nil : queue
            return first
        }
        next?.resume()
    }
}

/// Device-only flag set when the user removes a license from this device. While set, the app must
/// not exchange a still-valid continuity bridge credential (e.g. one synced through iCloud
/// Keychain from another device) for a new activation, which would hand straight back the seat
/// the user just gave up. Cleared only by an explicit activation on this device.
public struct BridgeConsumptionGate: Sendable {
    private let storage: any LicenseDeviceStateStoring
    private let key: String

    public init(storage: any LicenseDeviceStateStoring, namespace: String) {
        self.storage = storage
        self.key = "\(namespace).license.bridgeConsumptionSuppressed"
    }

    public var isSuppressed: Bool { storage.data(forKey: key) != nil }
    public func suppress() { storage.set(Data([1]), forKey: key) }
    public func allow() { storage.set(nil, forKey: key) }
}

public extension StoredLicense {
    /// True when this record cannot recover on its own: it does not authorize Pro for
    /// `deviceId` and holds no activation token to refresh or exchange with. The only way out is
    /// an explicit re-activation (re-entering the key). Apps surface this as a distinct
    /// "reconnect" state instead of plain "free".
    /// `requireActiveStatus` and `acceptedTokenVersions` are the same knobs as
    /// ``authorizedClaims(publicKeyBase64url:app:deviceId:now:requireActiveStatus:acceptedTokenVersions:)``.
    func needsReconnect(
        publicKeyBase64url: String, app: String, deviceId: String, now: Int = Int(Date().timeIntervalSince1970),
        requireActiveStatus: Bool = true, acceptedTokenVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions
    ) -> Bool {
        if authorizedClaims(
            publicKeyBase64url: publicKeyBase64url, app: app, deviceId: deviceId, now: now,
            requireActiveStatus: requireActiveStatus, acceptedTokenVersions: acceptedTokenVersions
        )?.isPro == true {
            return false
        }
        return activationToken?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true
    }
}
