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
    /// Runs `body` as ONE exclusive read-modify-write of `key` and returns `true`; returns `false`
    /// WITHOUT running `body` when the exclusion cannot be obtained. `body` must not call back into
    /// `transact`. The default excludes every caller in this process, so separate store or queue
    /// instances over one key cannot lose each other's updates. A store shared with another process
    /// (an app group with an extension) must override this with a cross-process lock; see
    /// ``UserDefaultsLicenseDeviceState/init(defaults:lockFileURL:)``.
    func transact(forKey key: String, _ body: () -> Void) -> Bool
}

public extension LicenseDeviceStateStoring {
    func transact(forKey key: String, _ body: () -> Void) -> Bool {
        LicenseDeviceStateLock.process.withLock { body() }
        return true
    }
}

enum LicenseDeviceStateLock {
    /// One lock for every store in the process; critical sections are a decode and an encode.
    static let process = NSLock()

    /// `flock` on `url`: excludes other threads and other processes that open the same file. The
    /// wait is bounded and fails closed, so a suspended holder cannot wedge every other process.
    static func withFileLock(at url: URL, timeout: TimeInterval = 5, _ body: () -> Void) -> Bool {
        let fd = open(url.path, O_RDWR | O_CREAT, 0o600)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        let deadline = Date().addingTimeInterval(timeout)
        while flock(fd, LOCK_EX | LOCK_NB) != 0 {
            guard errno == EWOULDBLOCK || errno == EINTR, Date() < deadline else { return false }
            usleep(1_000)
        }
        defer { flock(fd, LOCK_UN) }
        body()
        return true
    }
}

public final class UserDefaultsLicenseDeviceState: LicenseDeviceStateStoring, @unchecked Sendable {
    private let defaults: UserDefaults
    private let lockFileURL: URL?
    /// - Parameter lockFileURL: when the suite is shared with another process (an app group read
    ///   by an app and its extension), a lock file in the same shared container makes
    ///   ``transact(forKey:_:)`` exclude those processes too. Without it, exclusion covers this
    ///   process only.
    public init(defaults: UserDefaults = .standard, lockFileURL: URL? = nil) {
        self.defaults = defaults
        self.lockFileURL = lockFileURL
    }
    public func transact(forKey key: String, _ body: () -> Void) -> Bool {
        guard let lockFileURL else {
            LicenseDeviceStateLock.process.withLock { body() }
            return true
        }
        return LicenseDeviceStateLock.withFileLock(at: lockFileURL, body)
    }
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

    /// Adds `removal` unless an identical entry is already queued. Persisted before returning, as
    /// one exclusive read-modify-write so a concurrent writer (another queue instance, or another
    /// process when the storage provides a cross-process lock) cannot lose it. Returns `false` only
    /// when the storage could not grant that exclusion, in which case nothing was written.
    @discardableResult
    public func enqueue(_ removal: PendingLicenseRemoval) -> Bool {
        let (storage, key) = (self.storage, self.key)
        return storage.transact(forKey: key) {
            var current = Self.load(storage, key)
            guard !current.contains(removal) else { return }
            current.append(removal)
            Self.save(current, storage, key)
        }
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
            // Re-read after the await, inside the exclusive section: a removal queued meanwhile by
            // anyone must survive. A refused lock leaves the entry queued; redelivery is idempotent.
            let (storage, key) = (self.storage, self.key)
            _ = storage.transact(forKey: key) {
                Self.save(Self.load(storage, key).filter { $0 != removal }, storage, key)
            }
        }
        return delivered
    }

    /// Imports removals a pre-RightKit controller queued in its own format: a JSON array of
    /// `{"deviceIdHash": ..., "leaseId": ...}` rows and any number of bare activation tokens. Each
    /// becomes the same entry a new removal would be, and one already queued is not duplicated. Persisted as
    /// one exclusive read-modify-write, like ``enqueue(_:)``.
    ///
    /// Returns how many entries were newly queued, or `nil` when NOTHING was imported (the storage could
    /// not grant its exclusion, or `leaseRemovalsJSON` is present but unreadable). The caller deletes its
    /// legacy copy only after a non-nil result, so an owed removal is never lost in the move.
    @discardableResult
    public func adoptLegacy(leaseRemovalsJSON: Data?, activationTokens: [String] = []) -> Int? {
        struct LegacyLeaseRemoval: Decodable { let deviceIdHash: String; let leaseId: String }
        var incoming: [PendingLicenseRemoval] = []
        if let data = leaseRemovalsJSON {
            guard let rows = try? JSONDecoder().decode([LegacyLeaseRemoval].self, from: data) else { return nil }
            for row in rows where !row.deviceIdHash.isEmpty && !row.leaseId.isEmpty {
                incoming.append(.lease(deviceIdHash: row.deviceIdHash, leaseId: row.leaseId))
            }
        }
        for token in activationTokens {
            let trimmed = token.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { incoming.append(.activation(activationToken: trimmed)) }
        }
        guard !incoming.isEmpty else { return 0 }
        let (storage, key) = (self.storage, self.key)
        var added = 0
        let granted = storage.transact(forKey: key) {
            var current = Self.load(storage, key)
            for removal in incoming where !current.contains(removal) {
                current.append(removal)
                added += 1
            }
            Self.save(current, storage, key)
        }
        return granted ? added : nil
    }

    private func load() -> [PendingLicenseRemoval] { Self.load(storage, key) }

    private static func load(_ storage: any LicenseDeviceStateStoring, _ key: String) -> [PendingLicenseRemoval] {
        guard let data = storage.data(forKey: key),
              let decoded = try? JSONDecoder().decode([PendingLicenseRemoval].self, from: data) else { return [] }
        return decoded
    }

    private static func save(_ removals: [PendingLicenseRemoval], _ storage: any LicenseDeviceStateStoring, _ key: String) {
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

/// One-time move of removals that an earlier controller kept in `UserDefaults` (an activation token
/// stored as a string, lease removals stored as a JSON array) into the shared durable queue.
public enum LegacyLicenseRemovalMigration {
    /// Reads the legacy values, hands them to ``PendingLicenseRemovalQueue/adoptLegacy(leaseRemovalsJSON:activationTokens:)``
    /// and deletes the legacy keys only after it reported success. Returns the number of entries newly
    /// queued, or `nil` when nothing was moved (the legacy values are left untouched for the next launch).
    @discardableResult
    public static func adopt(
        into queue: PendingLicenseRemovalQueue, defaults: UserDefaults = .standard,
        activationTokenKey: String?, leaseRemovalsKey: String?
    ) async -> Int? {
        let leaseData = leaseRemovalsKey.flatMap { defaults.data(forKey: $0) }
        let token = activationTokenKey.flatMap { defaults.string(forKey: $0) }
        guard let added = await queue.adoptLegacy(leaseRemovalsJSON: leaseData, activationTokens: token.map { [$0] } ?? []) else { return nil }
        if leaseData != nil, let leaseRemovalsKey { defaults.removeObject(forKey: leaseRemovalsKey) }
        if token != nil, let activationTokenKey { defaults.removeObject(forKey: activationTokenKey) }
        return added
    }

    /// Moves a legacy "this device gave up its license, do not re-consume the continuity bridge" boolean
    /// (a `UserDefaults` flag) into `gate`. Without this, a device that removed its license before the app
    /// adopted ``BridgeConsumptionGate`` would consume the bridge again and take its seat back. The legacy key
    /// is deleted only once the gate reads suppressed. Returns whether the gate is suppressed afterwards.
    @discardableResult
    public static func adoptBridgeSuppression(into gate: BridgeConsumptionGate, defaults: UserDefaults = .standard, legacyKey: String) -> Bool {
        guard defaults.bool(forKey: legacyKey) else { return gate.isSuppressed }
        gate.suppress()
        guard gate.isSuppressed else { return false }
        defaults.removeObject(forKey: legacyKey)
        return true
    }
}
