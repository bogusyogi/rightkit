import Foundation
import Combine
import CryptoKit
import Network
import Security
import Testing
@testable import RightKitLicense

@Test func storedLicenseDropsRawKeyAfterTokenActivation() {
    let body = LicenseServerResponse(
        ok: true, status: "active", message: nil, activationToken: "act-1", licenseId: "lic-1",
        validUntil: nil, signedToken: "signed", licenseKind: "lifetime", entitlementStage: "final",
        licenseIssuedAt: nil, confirmationDueAt: nil, entitlements: LicenseEntitlements(v: 1, tier: "pro", features: ["routing"]), lease: nil, error: nil
    )
    let license = LicenseActivationClient.license(from: body, deviceId: "dev", key: "raw-key")
    #expect(license.key == nil)
    #expect(license.activationToken == "act-1")
    #expect(license.entitlements?.hasFeature("routing") == true)
}

@Test
func activationUsesLoopbackHTTPAndVerifiesRealEd25519Token() async throws {
    let signingKey = Curve25519.Signing.PrivateKey()
    let deviceID = "ios-test-device"
    let server = try await LoopbackLicenseServer(signingKey: signingKey, deviceID: deviceID)
    defer { server.stop() }

    let store = InMemoryLicenseStore()
    let publicKey = signingKey.publicKey.rawRepresentation.base64URLEncodedString()
    let client = LicenseActivationClient(
        configuration: LicenseClientConfiguration(
            app: "coderight", baseURL: server.baseURL, appVersion: "1.2.3",
            deviceID: { deviceID }, publicKeyBase64url: publicKey
        ),
        store: store
    )
    let license = try await client.activate(key: "RK-LOOPBACK")

    #expect(license.activationToken == "activation-loopback")
    #expect(license.key == nil)
    #expect(license.verifiedClaims(publicKeyBase64url: publicKey, app: "coderight", now: 1_700_000_010)?.isPro == true)
    #expect(server.lastPath == "/activate")
    #expect(server.lastLicenseKey == "RK-LOOPBACK")
}

private final class LoopbackLicenseServer: @unchecked Sendable {
    private(set) var baseURL: URL
    private let listener: NWListener
    private let signingKey: Curve25519.Signing.PrivateKey
    private let deviceID: String
    private let lock = NSLock()
    private var _lastPath = ""
    private var _lastLicenseKey = ""
    private var _lastBody: [String: String] = [:]
    private var _requestCount = 0
    private var _status = 200
    private var _tokenScope: String?
    private var _leaseScope: String?
    private var _paths: [String] = []

    var lastPath: String { lock.withLock { _lastPath } }
    var lastLicenseKey: String { lock.withLock { _lastLicenseKey } }
    var lastBody: [String: String] { lock.withLock { _lastBody } }
    var requestCount: Int { lock.withLock { _requestCount } }
    var paths: [String] { lock.withLock { _paths } }
    /// `entitlement_scope` claim signed into every token; nil omits the claim.
    var tokenScope: String? {
        get { lock.withLock { _tokenScope } }
        set { lock.withLock { _tokenScope = newValue } }
    }
    /// `entitlementScope` of the signed lease envelope served on lease paths; nil omits it.
    var leaseScope: String? {
        get { lock.withLock { _leaseScope } }
        set { lock.withLock { _leaseScope = newValue } }
    }
    /// HTTP status every later response carries.
    var status: Int {
        get { lock.withLock { _status } }
        set { lock.withLock { _status = newValue } }
    }

    init(signingKey: Curve25519.Signing.PrivateKey, deviceID: String) async throws {
        self.signingKey = signingKey
        self.deviceID = deviceID
        self.listener = try NWListener(using: .tcp, on: .any)
        self.baseURL = URL(string: "http://127.0.0.1:0")!
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            listener.stateUpdateHandler = { [weak self] state in
                guard let self else { return }
                switch state {
                case .ready:
                    guard let port = listener.port else {
                        continuation.resume(throwing: LicenseError.invalidResponse)
                        return
                    }
                    self.baseURL = URL(string: "http://127.0.0.1:\(port.rawValue)")!
                    continuation.resume()
                case .failed(let error): continuation.resume(throwing: error)
                case .cancelled: continuation.resume(throwing: LicenseError.transport("loopback server stopped"))
                default: break
                }
            }
            listener.newConnectionHandler = { [weak self] connection in self?.accept(connection) }
            listener.start(queue: DispatchQueue(label: "rightkit-license-loopback"))
        }
    }

    /// Detach the startup handler first: its continuation already resumed at `.ready`, and a
    /// `.cancelled` callback must not resume it again when a test stops the server mid-test.
    func stop() { listener.stateUpdateHandler = nil; listener.cancel() }

    private func accept(_ connection: NWConnection) {
        connection.stateUpdateHandler = { state in
            if case .ready = state { self.receive(connection) }
        }
        connection.start(queue: DispatchQueue(label: "rightkit-license-loopback-connection"))
    }

    /// URLSession may send headers and body in separate segments; read until Content-Length is met.
    private func receive(_ connection: NWConnection, buffer: Data = Data()) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, isComplete, _ in
            guard let self else { connection.cancel(); return }
            var buffer = buffer
            if let data { buffer.append(data) }
            let raw = String(decoding: buffer, as: UTF8.self)
            if let split = raw.range(of: "\r\n\r\n") {
                let head = raw[..<split.lowerBound].lowercased()
                let length = head.components(separatedBy: "\r\n").compactMap { line -> Int? in
                    line.hasPrefix("content-length:") ? Int(line.dropFirst(15).trimmingCharacters(in: .whitespaces)) : nil
                }.first ?? 0
                if buffer.count - raw[..<split.upperBound].utf8.count >= length { self.respond(connection, request: buffer); return }
            }
            if isComplete || data == nil { connection.cancel(); return }
            self.receive(connection, buffer: buffer)
        }
    }

    private func respond(_ connection: NWConnection, request: Data) {
        let raw = String(decoding: request, as: UTF8.self)
        let requestLine = raw.split(separator: "\r\n", maxSplits: 1).first.map(String.init) ?? ""
        let path = requestLine.split(separator: " ").dropFirst().first.map(String.init) ?? ""
        let body = raw.components(separatedBy: "\r\n\r\n").dropFirst().first ?? ""
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        lock.withLock {
            _lastPath = path
            _lastLicenseKey = json?["licenseKey"] as? String ?? ""
            _lastBody = (json ?? [:]).compactMapValues { $0 as? String }
            _requestCount += 1
            _paths.append(path)
        }
        let status = self.status
        var payload: [String: Any] = [
            "v": 2, "id": "loopback-license", "app": "coderight", "tier": "pro",
            "features": ["routing"], "device_id": deviceID, "iat": 1_700_000_000, "exp": 2_000_000_000,
        ]
        if let tokenScope { payload["entitlement_scope"] = tokenScope }
        let payloadData = try! JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
        let token = payloadData.base64URLEncodedString() + "." + (try! signingKey.signature(for: payloadData)).base64URLEncodedString()
        var response: [String: Any] = [
            "ok": true, "status": "active", "activationToken": "activation-loopback",
            "signedToken": token, "entitlements": ["v": 1, "tier": "pro", "features": ["routing"]],
        ]
        if path.contains("/devices/lease") {
            let lease = signedLease(signingKey, deviceIdHash: deviceID, entitlement: token, scope: leaseScope)
            var object: [String: Any] = [
                "leaseId": lease.leaseId, "deviceIdHash": lease.deviceIdHash, "pool": lease.pool,
                "issuedAt": lease.issuedAt, "expiresAt": lease.expiresAt, "graceSeconds": lease.graceSeconds,
                "generation": lease.generation, "entitlement": lease.entitlement, "signature": lease.signature!,
            ]
            if let scope = lease.entitlementScope { object["entitlementScope"] = scope }
            response["lease"] = object
        }
        let bytes = try! JSONSerialization.data(withJSONObject: response, options: [.sortedKeys])
        let header = "HTTP/1.1 \(status) \(status == 200 ? "OK" : "Error")\r\nContent-Type: application/json\r\nContent-Length: \(bytes.count)\r\nConnection: close\r\n\r\n"
        connection.send(content: Data(header.utf8) + bytes, completion: .contentProcessed { _ in connection.cancel() })
    }
}

private extension Data {
    func base64URLEncodedString() -> String {
        base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}

private func signedToken(_ key: Curve25519.Signing.PrivateKey, app: String = "coderight", tier: String = "pro", deviceID: String = "dev", exp: Int = 2_000_000_000, extraClaims: [String: Any] = [:]) -> String {
    var payload: [String: Any] = ["v": 2, "id": "lic", "app": app, "tier": tier, "features": ["routing"], "device_id": deviceID, "iat": 1_700_000_000, "exp": exp]
    payload.merge(extraClaims) { $1 }
    let data = try! JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    return data.base64URLEncodedString() + "." + (try! key.signature(for: data)).base64URLEncodedString()
}

@Test func accessIsDerivedOnlyFromVerifiedClaims() {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let now = 1_800_000_000
    let entitlements = LicenseEntitlements(v: 1, tier: "pro", features: ["routing"])

    // Unsigned / forged Pro record: active, no validUntil, entitlements say pro. Display helper
    // is fooled by design; authorization must deny.
    let unsigned = StoredLicense(status: "active", deviceId: "dev", entitlements: entitlements)
    #expect(displayIsPro(unsigned))
    #expect(!isPro(unsigned, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    #expect(!satisfiesRequirement(unsigned, .pro, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))

    let forgedBy = Curve25519.Signing.PrivateKey()
    let forged = StoredLicense(status: "active", deviceId: "dev", entitlements: entitlements, signedToken: signedToken(forgedBy))
    #expect(!isPro(forged, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))

    let good = StoredLicense(status: "active", deviceId: "dev", entitlements: LicenseEntitlements(v: 1, tier: "free"), signedToken: signedToken(key))
    #expect(isPro(good, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now), "claims, not cached entitlements, decide")
    #expect(satisfiesRequirement(good, LicenseRequirement(feature: "routing"), publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    #expect(!satisfiesRequirement(good, LicenseRequirement(tier: "max"), publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    #expect(!satisfiesRequirement(good, LicenseRequirement(), publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))

    // Bound to app, device and expiry; record status must still be active.
    #expect(!isPro(good, publicKeyBase64url: pub, app: "otherapp", deviceId: "dev", now: now))
    #expect(!isPro(good, publicKeyBase64url: pub, app: "coderight", deviceId: "other-device", now: now))
    #expect(isPro(good, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    #expect(!isPro(good, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: 2_000_000_001))
    var revoked = good; revoked.status = "revoked"
    #expect(!isPro(revoked, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    let wrongDevice = StoredLicense(status: "active", deviceId: "dev", signedToken: signedToken(key, deviceID: "someone-else"))
    #expect(!isPro(wrongDevice, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))

    // A record copied from device A (token + matching cached device ID) is denied on device B.
    let copiedFromA = StoredLicense(status: "active", deviceId: "device-a", signedToken: signedToken(key, deviceID: "device-a"))
    #expect(isPro(copiedFromA, publicKeyBase64url: pub, app: "coderight", deviceId: "device-a", now: now))
    #expect(!isPro(copiedFromA, publicKeyBase64url: pub, app: "coderight", deviceId: "device-b", now: now))
    #expect(!isPro(copiedFromA, publicKeyBase64url: pub, app: "coderight", deviceId: "", now: now))
}

@Test func licenseAndDeviceIDRecordsAreDeviceOnlyAndNonSynchronizable() {
    // Attribute construction only: no Keychain item is created or read.
    let store = KeychainLicenseStore(service: "test.\(UUID().uuidString)", account: "activation")
    let identity = store.identity(service: store.service, account: "device-id")
    #expect(identity[kSecAttrSynchronizable as String] as? Bool == false)
    #expect(KeychainLicenseStore.deviceOnlyAccessibility as String == kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
}

@Test func inMemoryStoreRoundTripsAndRemoves() async throws {
    let store = InMemoryLicenseStore()
    let license = StoredLicense(status: "active", deviceId: "d")
    try await store.save(license)
    #expect(await store.load() == license)
    try await store.remove()
    #expect(await store.load() == nil)
}

@Test func storedLicenseResolverDeniesRecordCopiedFromAnotherDevice() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let copied = StoredLicense(status: "active", deviceId: "device-a", signedToken: signedToken(key, deviceID: "device-a"))
    func resolver(liveDevice: String, record: StoredLicense) async throws -> StoredLicenseResolver {
        let store = InMemoryLicenseStore()
        try await store.save(record)
        let client = LicenseActivationClient(
            configuration: LicenseClientConfiguration(app: "coderight", baseURL: URL(string: "http://127.0.0.1:9")!, deviceID: { liveDevice }, publicKeyBase64url: pub),
            store: store
        )
        return StoredLicenseResolver(client: client, publicKeyBase64url: pub, app: "coderight", now: { 1_700_000_010 })
    }
    #expect(try await resolver(liveDevice: "device-a", record: copied).resolve()?.isPro == true)
    #expect(try await resolver(liveDevice: "device-b", record: copied).resolve() == nil)
    var revoked = copied; revoked.status = "revoked"
    #expect(try await resolver(liveDevice: "device-a", record: revoked).resolve() == nil)
}

// MARK: - Lease removal by original identity

private final class DeviceIDProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0
    var count: Int { lock.withLock { calls } }
    func read() -> String { lock.withLock { calls += 1 }; return "current-device-after-reset" }
}

private func leaseClient(server: LoopbackLicenseServer, store: InMemoryLicenseStore, probe: DeviceIDProbe) -> LicenseActivationClient {
    LicenseActivationClient(
        configuration: LicenseClientConfiguration(
            app: "heardright", baseURL: server.baseURL.appendingPathComponent("v1/licenses/"),
            deviceID: { probe.read() }
        ),
        store: store
    )
}

private let currentActivation = StoredLicense(
    status: "active", deviceId: "current-device-after-reset", activationToken: "current-token",
    signedToken: "current-signed", leaseId: "current-lease"
)

@Test func removeLeaseByIdentitySendsOriginalHashAndKeepsLocalActivation() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    let store = InMemoryLicenseStore()
    try await store.save(currentActivation)
    let probe = DeviceIDProbe()
    let client = leaseClient(server: server, store: store, probe: probe)

    try await client.removeLease(deviceIdHash: "original-hash-from-old-identity", leaseId: "queued-lease-7")

    #expect(server.lastPath.hasSuffix("/pro/heardright/devices/lease/remove"))
    #expect(server.lastBody == [
        "appKey": "heardright", "deviceIdHash": "original-hash-from-old-identity", "leaseId": "queued-lease-7",
    ])
    #expect(server.lastBody["deviceIdHash"] != LicenseActivationClient.deviceIDHash(currentActivation.deviceId))
    #expect(probe.count == 0, "never derives identity from the current device")
    #expect(await store.load() == currentActivation, "current local activation is untouched")
}

@Test func removeLeaseByIdentityPropagatesServerFailureWithoutTouchingStore() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    server.status = 503
    let store = InMemoryLicenseStore()
    try await store.save(currentActivation)
    let client = leaseClient(server: server, store: store, probe: DeviceIDProbe())

    await #expect(throws: LicenseError.self) {
        try await client.removeLease(deviceIdHash: "original-hash", leaseId: "queued-lease")
    }
    #expect(server.requestCount == 1)
    #expect(await store.load() == currentActivation)
}

@Test func removeLeaseByIdentityWorksWithNoStoredLicense() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    let store = InMemoryLicenseStore()
    let client = leaseClient(server: server, store: store, probe: DeviceIDProbe())

    try await client.removeLease(deviceIdHash: "original-hash", leaseId: "queued-lease")

    #expect(server.lastBody["leaseId"] == "queued-lease")
    #expect(await store.load() == nil, "no activation is created or removed")
}

@Test func currentRecordRemoveLeaseStillRemovesLocalActivation() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    let store = InMemoryLicenseStore()
    try await store.save(currentActivation)
    let client = leaseClient(server: server, store: store, probe: DeviceIDProbe())

    try await client.removeLease()

    #expect(server.lastBody == [
        "appKey": "heardright",
        "deviceIdHash": LicenseActivationClient.deviceIDHash(currentActivation.deviceId),
        "leaseId": "current-lease",
    ])
    #expect(await store.load() == nil)
}

// MARK: - 0.3.3 device-side removal housekeeping

private final class DeliveryLog: @unchecked Sendable {
    private let lock = NSLock()
    private var entries: [PendingLicenseRemoval] = []
    private var failing: Set<PendingLicenseRemoval> = []
    var delivered: [PendingLicenseRemoval] { lock.withLock { entries } }
    func fail(_ set: Set<PendingLicenseRemoval>) { lock.withLock { failing = set } }
    func deliver(_ removal: PendingLicenseRemoval) throws {
        try lock.withLock {
            if failing.contains(removal) { throw LicenseError.transport("offline") }
            entries.append(removal)
        }
    }
}

@Test func pendingRemovalQueuePersistsDedupesAndDropsOnlyAcknowledgedEntries() async throws {
    let storage = InMemoryLicenseDeviceState()
    let lease = PendingLicenseRemoval.lease(deviceIdHash: "h1", leaseId: "l1")
    let activation = PendingLicenseRemoval.activation(activationToken: "act-1")
    let queue = PendingLicenseRemovalQueue(storage: storage, namespace: "app")
    await queue.enqueue(lease)
    await queue.enqueue(lease)
    await queue.enqueue(activation)
    #expect(await queue.pending == [lease, activation])

    // Survives a relaunch (new queue over the same storage).
    let relaunched = PendingLicenseRemovalQueue(storage: storage, namespace: "app")
    #expect(await relaunched.pending == [lease, activation])
    #expect(await PendingLicenseRemovalQueue(storage: storage, namespace: "other").pending.isEmpty)

    let log = DeliveryLog()
    log.fail([activation])
    let first = await relaunched.retry { try log.deliver($0) }
    #expect(first == [lease])
    #expect(await relaunched.pending == [activation], "an unacknowledged removal stays queued")

    log.fail([])
    let second = await relaunched.retry { try log.deliver($0) }
    #expect(second == [activation])
    #expect(await relaunched.pending.isEmpty)
    #expect(storage.data(forKey: "app.license.pendingRemovals") == nil)
}

@Test func pendingRemovalQueueKeepsEntriesQueuedDuringARetry() async throws {
    let queue = PendingLicenseRemovalQueue(storage: InMemoryLicenseDeviceState(), namespace: "app")
    let old = PendingLicenseRemoval.lease(deviceIdHash: "h-old", leaseId: "l-old")
    let late = PendingLicenseRemoval.lease(deviceIdHash: "h-new", leaseId: "l-new")
    await queue.enqueue(old)
    let delivered = await queue.retry { _ in await queue.enqueue(late) }
    #expect(delivered == [old])
    #expect(await queue.pending == [late])
}

@Test func owedRemovalPrefersLeaseThenActivationToken() {
    let leased = StoredLicense(status: "active", deviceId: "dev", activationToken: "act", leaseId: "lease-1")
    #expect(PendingLicenseRemoval.owed(by: leased) == .lease(deviceIdHash: LicenseActivationClient.deviceIDHash("dev"), leaseId: "lease-1"))
    let legacy = StoredLicense(status: "active", deviceId: "dev", activationToken: "act")
    #expect(PendingLicenseRemoval.owed(by: legacy) == .activation(activationToken: "act"))
    #expect(PendingLicenseRemoval.owed(by: StoredLicense(status: "active", deviceId: "dev", activationToken: "  ")) == nil)
}

@Test func deactivateByTokenSendsExactlyThatTokenAndThrowsOnFailure() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    let store = InMemoryLicenseStore()
    try await store.save(currentActivation)
    let client = leaseClient(server: server, store: store, probe: DeviceIDProbe())

    try await client.deactivate(activationToken: "queued-token")
    #expect(server.lastPath.hasSuffix("/deactivate"))
    #expect(server.lastBody == ["app": "heardright", "activationToken": "queued-token"])
    #expect(await store.load() == currentActivation, "never touches the stored license")

    server.status = 503
    await #expect(throws: LicenseError.self) { try await client.deactivate(activationToken: "queued-token") }
}

@Test func queueRetryDeliversThroughTheClientEndpoints() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    let client = leaseClient(server: server, store: InMemoryLicenseStore(), probe: DeviceIDProbe())
    let queue = PendingLicenseRemovalQueue(storage: InMemoryLicenseDeviceState(), namespace: "heardright")
    await queue.enqueue(.lease(deviceIdHash: "orig-hash", leaseId: "lease-9"))

    server.status = 503
    #expect(await queue.retry(using: client).isEmpty)
    #expect(await queue.pending.count == 1)

    server.status = 200
    #expect(await queue.retry(using: client).count == 1)
    #expect(server.lastBody == ["appKey": "heardright", "deviceIdHash": "orig-hash", "leaseId": "lease-9"])
    #expect(await queue.pending.isEmpty)
}

@Test func bridgeConsumptionGateIsNamespacedAndPersisted() {
    let storage = InMemoryLicenseDeviceState()
    let gate = BridgeConsumptionGate(storage: storage, namespace: "app")
    #expect(!gate.isSuppressed)
    gate.suppress()
    #expect(BridgeConsumptionGate(storage: storage, namespace: "app").isSuppressed)
    #expect(!BridgeConsumptionGate(storage: storage, namespace: "other").isSuppressed)
    gate.allow()
    #expect(!gate.isSuppressed)
}

@Test func needsReconnectOnlyForNonProRecordsWithoutAnActivationToken() {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let now = 1_800_000_000
    let pro = StoredLicense(status: "active", deviceId: "dev", signedToken: signedToken(key))
    #expect(!pro.needsReconnect(publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    let expiredNoToken = StoredLicense(status: "active", deviceId: "dev", signedToken: signedToken(key, exp: 1_700_000_001))
    #expect(expiredNoToken.needsReconnect(publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    var refreshable = expiredNoToken; refreshable.activationToken = "act"
    #expect(!refreshable.needsReconnect(publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
}

@MainActor
@Test func queuedControllerRemovalSuppressesBridgeQueuesAndDeliversLater() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    server.status = 503
    let store = InMemoryLicenseStore()
    try await store.save(currentActivation)
    let state = InMemoryLicenseDeviceState()
    let client = leaseClient(server: server, store: store, probe: DeviceIDProbe())
    let controller = LicenseController(client: client, deviceState: state)

    await controller.remove()
    #expect(controller.license == nil)
    #expect(await store.load() == nil, "local Pro ends immediately, even offline")
    #expect(controller.isBridgeConsumptionSuppressed)
    let owed = PendingLicenseRemoval.lease(
        deviceIdHash: LicenseActivationClient.deviceIDHash(currentActivation.deviceId), leaseId: "current-lease"
    )
    #expect(await controller.pendingRemovals?.pending == [owed])

    // Relaunch: a fresh controller over the same device state still owes the removal.
    server.status = 200
    let relaunched = LicenseController(client: client, deviceState: state)
    // The init-time retry may already be in flight (a concurrent retry returns []): settle both.
    var attempts = 0
    while await relaunched.pendingRemovals?.pending.isEmpty != true, attempts < 100 {
        _ = await relaunched.retryPendingRemovals()
        attempts += 1
        try await Task.sleep(nanoseconds: 5_000_000)
    }
    #expect(await relaunched.pendingRemovals?.pending.isEmpty == true)
    #expect(server.lastBody["leaseId"] == "current-lease")
    #expect(relaunched.isBridgeConsumptionSuppressed, "only an explicit activation clears suppression")

    await relaunched.activate("RK-AGAIN")
    #expect(!relaunched.isBridgeConsumptionSuppressed)
}

@MainActor
@Test func legacyControllerRemovalIsUnchanged() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "unused")
    defer { server.stop() }
    let store = InMemoryLicenseStore()
    try await store.save(StoredLicense(status: "active", deviceId: "dev", activationToken: "act-legacy"))
    let controller = LicenseController(client: leaseClient(server: server, store: store, probe: DeviceIDProbe()))
    await controller.remove()
    #expect(controller.pendingRemovals == nil && controller.bridgeConsumption == nil)
    #expect(!controller.isBridgeConsumptionSuppressed)
    #expect(server.lastBody == ["app": "heardright", "activationToken": "act-legacy"])
    #expect(await store.load() == nil)
}

@MainActor
@Test func controllerNeedsReconnectReflectsTheStoredRecord() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let store = InMemoryLicenseStore()
    let client = LicenseActivationClient(
        configuration: LicenseClientConfiguration(app: "coderight", baseURL: URL(string: "http://127.0.0.1:9")!, deviceID: { "dev" }, publicKeyBase64url: pub),
        store: store
    )
    let controller = LicenseController(client: client, deviceState: InMemoryLicenseDeviceState())
    #expect(!controller.needsReconnect, "no record is plain free, not reconnect")
    try await controller.persist(StoredLicense(status: "active", deviceId: "dev", signedToken: signedToken(key, deviceID: "dev", exp: 1_000)))
    #expect(controller.needsReconnect)
    try await controller.persist(StoredLicense(status: "active", deviceId: "dev", signedToken: signedToken(key, deviceID: "dev")))
    #expect(!controller.needsReconnect)
}

// MARK: - 0.3.4

/// A lease envelope signed over its canonical JSON, as the server signs it.
private func signedLease(
    _ key: Curve25519.Signing.PrivateKey, deviceIdHash: String, entitlement: String, scope: String?,
    leaseId: String = "lease-1", generation: Int = 1
) -> LicenseLeaseEnvelope {
    let unsigned = LicenseLeaseEnvelope(
        leaseId: leaseId, deviceIdHash: deviceIdHash, pool: "mobile", issuedAt: "2026-01-01T00:00:00Z",
        expiresAt: "2033-05-18T03:33:20Z", graceSeconds: 86_400, generation: generation, entitlement: entitlement,
        entitlementScope: scope
    )
    let signature = try! key.signature(for: Data(unsigned.canonicalJSON().utf8)).base64URLEncodedString()
    return LicenseLeaseEnvelope(
        leaseId: leaseId, deviceIdHash: deviceIdHash, pool: "mobile", issuedAt: unsigned.issuedAt,
        expiresAt: unsigned.expiresAt, graceSeconds: 86_400, generation: generation, entitlement: entitlement,
        signature: signature, entitlementScope: scope
    )
}

private func client(
    _ server: LoopbackLicenseServer, store: InMemoryLicenseStore, app: String = "coderight", device: String = "dev",
    publicKey: String? = nil, deviceClass: String? = nil, includeAppKeyInLeaseBody: Bool = true,
    leasePathStyle: LicenseLeasePathStyle = .siblingOfBase,
    refreshErrorPolicy: LicenseRefreshErrorPolicy = .keepRecordOnServerError
) -> LicenseActivationClient {
    LicenseActivationClient(
        configuration: LicenseActivationClient.Configuration(
            app: app, baseURL: server.baseURL.appendingPathComponent("v1/licenses"), deviceID: { device },
            publicKeyBase64url: publicKey, deviceClass: deviceClass, includeAppKeyInLeaseBody: includeAppKeyInLeaseBody,
            leasePathStyle: leasePathStyle, refreshErrorPolicy: refreshErrorPolicy
        ),
        store: store
    )
}

// Item 1: the scope claim is `entitlement_scope`; lease records keep Pro.

@Test func leaseRecordsWithFullOrMobileScopeKeepPro() {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let hash = LicenseActivationClient.deviceIDHash("dev")
    let now = 1_800_000_000
    for scope in ["full", "mobile"] {
        let token = signedToken(key, deviceID: hash, extraClaims: ["entitlement_scope": scope])
        let leased = StoredLicense(status: "active", deviceId: "dev", signedToken: token, leaseId: "lease-1", entitlementScope: scope)
        #expect(leased.authorizedClaims(publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now)?.scope == scope)
        #expect(isPro(leased, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now), "lease scope \(scope) keeps Pro")
    }
    // The envelope-scope guard still fails closed when the envelope and the signed claim disagree.
    let mismatched = StoredLicense(
        status: "active", deviceId: "dev", signedToken: signedToken(key, deviceID: hash, extraClaims: ["entitlement_scope": "full"]),
        leaseId: "lease-1", entitlementScope: "mobile"
    )
    #expect(!isPro(mismatched, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
    // A legacy (non-lease) record has no envelope scope: the claim alone is reported.
    let legacy = StoredLicense(status: "active", deviceId: "dev", signedToken: signedToken(key, extraClaims: ["entitlement_scope": "mobile"]))
    #expect(legacy.authorizedClaims(publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now)?.scope == "mobile")
    #expect(isPro(legacy, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: now))
}

@MainActor
@Test func leaseActivationOverTheWireWithScopedTokenIsPro() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let server = try await LoopbackLicenseServer(signingKey: key, deviceID: LicenseActivationClient.deviceIDHash("dev"))
    defer { server.stop() }
    server.tokenScope = "mobile"; server.leaseScope = "mobile"
    let controller = LicenseController(client: client(server, store: InMemoryLicenseStore(), publicKey: pub))
    await controller.activateLease("RK-LEASE")
    #expect(controller.license?.entitlementScope == "mobile")
    #expect(controller.isProCandidate, "signed entitlement_scope matches the envelope scope")
}

// Item 2: refresh routing and silent mode.

@MainActor
@Test func controllerRefreshRoutesLeaseRecordsToRenewAndOthersToRefresh() async throws {
    let leased = StoredLicense(status: "active", deviceId: "dev", activationToken: "act", leaseId: "lease-0")
    let legacy = StoredLicense(status: "active", deviceId: "dev", activationToken: "act")
    for housekeeping in [false, true] {
        for (record, expected) in [(leased, "/v1/pro/coderight/devices/lease/renew"), (legacy, "/v1/licenses/refresh")] {
            let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "dev")
            defer { server.stop() }
            let c = client(server, store: InMemoryLicenseStore(record))
            let controller = housekeeping ? LicenseController(client: c, deviceState: InMemoryLicenseDeviceState()) : LicenseController(client: c)
            await controller.refresh()
            // Only the routed endpoint is called (housekeeping has nothing queued to deliver).
            #expect(server.paths == [expected], "housekeeping=\(housekeeping)")
        }
    }
}

@MainActor
@Test func silentRefreshSetsNoMessageAndNoBusyFlicker() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "dev")
    defer { server.stop() }
    server.status = 401
    let store = InMemoryLicenseStore(StoredLicense(status: "active", deviceId: "dev", activationToken: "act"))
    for controller in [
        LicenseController(client: client(server, store: store)),
        LicenseController(client: client(server, store: store), deviceState: InMemoryLicenseDeviceState()),
    ] {
        var busy: [Bool] = []
        let watch = controller.$isBusy.sink { busy.append($0) }
        await controller.refresh(silent: true)
        #expect(controller.message == "", "a failing silent refresh shows nothing")
        server.status = 200
        await controller.refresh(silent: true)
        #expect(controller.message == "", "a successful silent refresh shows nothing")
        #expect(controller.license?.activationToken == "activation-loopback", "the record still updates")
        #expect(!busy.contains(true), "isBusy never flips during a silent refresh")
        watch.cancel()

        server.status = 401
        await controller.refresh()
        #expect(controller.message == LicenseError.serverStatus(401, "").localizedDescription, "a visible refresh still reports")
        server.status = 200
        await controller.refresh()
        #expect(controller.message == "License refreshed.")
    }
}

// Item 3: wire differences are configurable; defaults unchanged.

@Test func deviceClassIsSentOnlyWhenConfigured() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "dev")
    defer { server.stop() }
    _ = try await client(server, store: InMemoryLicenseStore()).activate(key: "RK-1")
    #expect(server.lastBody["deviceClass"] == nil)
    #expect(Set(server.lastBody.keys) == ["app", "licenseKey", "deviceId", "version"])
    _ = try await client(server, store: InMemoryLicenseStore(), deviceClass: "iphone").activate(key: "RK-1")
    #expect(server.lastBody["deviceClass"] == "iphone")
}

@Test func leaseBodiesOmitAppKeyWhenConfigured() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "dev")
    defer { server.stop() }
    let leased = StoredLicense(status: "active", deviceId: "dev", activationToken: "act", leaseId: "lease-0")
    let hash = LicenseActivationClient.deviceIDHash("dev")

    try await client(server, store: InMemoryLicenseStore(), includeAppKeyInLeaseBody: false).removeLease(deviceIdHash: "h", leaseId: "l")
    #expect(server.lastBody == ["deviceIdHash": "h", "leaseId": "l"])
    _ = try await client(server, store: InMemoryLicenseStore(leased), includeAppKeyInLeaseBody: false).renewLease()
    #expect(server.lastBody == ["deviceIdHash": hash, "leaseId": "lease-0"])
    try await client(server, store: InMemoryLicenseStore(leased), includeAppKeyInLeaseBody: false).removeLease()
    #expect(server.lastBody == ["deviceIdHash": hash, "leaseId": "lease-0"])
    _ = try? await client(server, store: InMemoryLicenseStore(), includeAppKeyInLeaseBody: false).activateLease(key: "RK-L")
    #expect(server.lastBody["appKey"] == nil && server.lastBody["licenseKey"] == "RK-L")

    // Default keeps appKey.
    _ = try await client(server, store: InMemoryLicenseStore(leased)).renewLease()
    #expect(server.lastBody == ["appKey": "coderight", "deviceIdHash": hash, "leaseId": "lease-0"])
}

@Test func defaultLeaseURLResolvesExactlyAsTheOriginalDotDotPath() {
    let bases = [
        "https://heardright.app/v1/licenses", "https://heardright.app/v1/licenses/",
        "http://127.0.0.1:8080/v1/licenses/", "https://host.test/api/v1/licenses?x=1",
    ]
    for base in bases {
        let config = LicenseClientConfiguration(app: "heardright", baseURL: URL(string: base)!, deviceID: { "d" })
        for suffix in [[], ["renew"], ["remove"]] as [[String]] {
            let original = URL(string: base)!.appendingPathComponent(
                (["../pro/heardright/devices/lease"] + suffix).joined(separator: "/")
            )
            #expect(config.leaseURL(suffix) == original.standardized, "\(base) \(suffix)")
        }
    }
    #expect(LicenseClientConfiguration(app: "heardright", baseURL: URL(string: "https://heardright.app/v1/licenses")!, deviceID: { "d" })
        .leaseURL([]).absoluteString == "https://heardright.app/v1/pro/heardright/devices/lease")
}

@Test func originRootedLeasePathMatchesScrapeRightURLs() {
    for base in ["https://scraperight.cc/v1/licenses", "https://scraperight.cc/some/other/base/", "https://scraperight.cc"] {
        let config = LicenseClientConfiguration(
            app: "scraperight", baseURL: URL(string: base)!, deviceID: { "d" }, leasePathStyle: .originRooted(apiPrefix: "/v1")
        )
        #expect(config.leaseURL([]).absoluteString == "https://scraperight.cc/v1/pro/scraperight/devices/lease")
        #expect(config.leaseURL(["renew"]).absoluteString == "https://scraperight.cc/v1/pro/scraperight/devices/lease/renew")
        #expect(config.leaseURL(["remove"]).absoluteString == "https://scraperight.cc/v1/pro/scraperight/devices/lease/remove")
    }
}

@Test func leaseRequestsGoToTheNormalizedPathOnTheWire() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "dev")
    defer { server.stop() }
    try await client(server, store: InMemoryLicenseStore(), app: "heardright").removeLease(deviceIdHash: "h", leaseId: "l")
    #expect(server.lastPath == "/v1/pro/heardright/devices/lease/remove")
    try await client(server, store: InMemoryLicenseStore(), app: "heardright", leasePathStyle: .originRooted(apiPrefix: "/api"))
        .removeLease(deviceIdHash: "h", leaseId: "l")
    #expect(server.lastPath == "/api/pro/heardright/devices/lease/remove")
}

@Test func refreshErrorPolicyDecidesWhatSurfaces() async throws {
    let server = try await LoopbackLicenseServer(signingKey: Curve25519.Signing.PrivateKey(), deviceID: "dev")
    defer { server.stop() }
    let record = StoredLicense(status: "active", deviceId: "dev", activationToken: "act")
    func refresh(_ policy: LicenseRefreshErrorPolicy) async throws -> StoredLicense? {
        try await client(server, store: InMemoryLicenseStore(record), refreshErrorPolicy: policy).refresh()
    }
    server.status = 503
    #expect(try await refresh(.keepRecordOnServerError) == record, "default: 5xx keeps the record")
    await #expect(throws: LicenseError.serverStatus(503, "")) { try await refresh(.surfaceAll) }
    #expect(try await refresh(.keepRecordOnAnyError) == record)
    server.status = 401
    await #expect(throws: LicenseError.serverStatus(401, "")) { try await refresh(.keepRecordOnServerError) }
    await #expect(throws: LicenseError.serverStatus(401, "")) { try await refresh(.surfaceAll) }
    #expect(try await refresh(.keepRecordOnAnyError) == record)
    // Transport failure: default and surfaceAll throw (as before), keepRecordOnAnyError keeps the record.
    server.stop()
    let offline = LicenseActivationClient(
        configuration: LicenseClientConfiguration(app: "coderight", baseURL: URL(string: "http://127.0.0.1:9/v1/licenses")!, deviceID: { "dev" }, refreshErrorPolicy: .keepRecordOnAnyError),
        store: InMemoryLicenseStore(record)
    )
    #expect(try await offline.refresh() == record)
}

// Item 4: public lease builder with the fail-closed scope check.

@Test func publicLeaseBuilderVerifiesSignatureThenScope() throws {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let hash = LicenseActivationClient.deviceIDHash("dev")
    let token = signedToken(key, deviceID: hash, extraClaims: ["entitlement_scope": "mobile"])

    let lease = signedLease(key, deviceIdHash: hash, entitlement: token, scope: "mobile", leaseId: "lease-9", generation: 3)
    let built = try LicenseActivationClient.license(
        fromLease: lease, activationToken: "act", deviceId: "dev", publicKeyBase64url: pub, key: "RK-KEPT", activatedAt: "2026-02-02T00:00:00Z"
    )
    #expect(built.status == "active" && built.activationToken == "act" && built.key == "RK-KEPT")
    #expect(built.leaseId == "lease-9" && built.leaseGeneration == 3 && built.leaseGraceSeconds == 86_400)
    #expect(built.leaseExpiresAtUnix == 2_000_000_000 && built.validUntil == lease.expiresAt)
    #expect(built.entitlementScope == "mobile" && built.signedToken == token && built.activatedAt == "2026-02-02T00:00:00Z")
    #expect(isPro(built, publicKeyBase64url: pub, app: "coderight", deviceId: "dev", now: 1_800_000_000))

    // Legacy envelope without a scope passes; full passes.
    _ = try LicenseActivationClient.license(fromLease: signedLease(key, deviceIdHash: hash, entitlement: token, scope: nil), activationToken: "a", deviceId: "dev", publicKeyBase64url: pub)
    _ = try LicenseActivationClient.license(fromLease: signedLease(key, deviceIdHash: hash, entitlement: token, scope: "full"), activationToken: "a", deviceId: "dev", publicKeyBase64url: pub)

    // Unknown scope fails closed, even when correctly signed.
    let desktop = signedLease(key, deviceIdHash: hash, entitlement: token, scope: "desktop")
    #expect(throws: LicenseScopeError.unsupportedEntitlementScope("desktop")) {
        try LicenseActivationClient.license(fromLease: desktop, activationToken: "a", deviceId: "dev", publicKeyBase64url: pub)
    }
    #expect(throws: LicenseScopeError.unsupportedEntitlementScope("desktop")) { try desktop.validateEntitlementScope() }
    try desktop.validateEntitlementScope(allowed: ["desktop"])

    // Signature is checked first: a forged envelope with an unknown scope reports the signature.
    let forged = signedLease(Curve25519.Signing.PrivateKey(), deviceIdHash: hash, entitlement: token, scope: "desktop")
    #expect(throws: LicenseError.badLeaseSignature) {
        try LicenseActivationClient.license(fromLease: forged, activationToken: "a", deviceId: "dev", publicKeyBase64url: pub)
    }
    #expect(throws: LicenseError.malformedPublicKey) {
        try LicenseActivationClient.license(fromLease: lease, activationToken: "a", deviceId: "dev", publicKeyBase64url: "!!")
    }
}

// Item 5: controller message overrides.

@MainActor
@Test func controllerDefaultMessagesAreUnchanged() async throws {
    #expect(LicenseMessages().activated == "License activated.")
    #expect(LicenseMessages().refreshed == "License refreshed.")
    #expect(LicenseMessages().removed == "License removed.")
    #expect(LicenseMessages().activatedPro == nil && LicenseMessages().refreshedPro == nil)

    let key = Curve25519.Signing.PrivateKey()
    let server = try await LoopbackLicenseServer(signingKey: key, deviceID: "dev")
    defer { server.stop() }
    let controller = LicenseController(
        client: client(server, store: InMemoryLicenseStore(), publicKey: key.publicKey.rawRepresentation.base64URLEncodedString()),
        deviceState: InMemoryLicenseDeviceState()
    )
    await controller.activate("RK-1")
    #expect(controller.message == "License activated.", "Pro does not change the default message")
    await controller.refresh()
    #expect(controller.message == "License refreshed.")
    await controller.remove()
    #expect(controller.message == "License removed.")
}

@MainActor
@Test func controllerMessagesAreInjectable() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let pub = key.publicKey.rawRepresentation.base64URLEncodedString()
    let server = try await LoopbackLicenseServer(signingKey: key, deviceID: "dev")
    defer { server.stop() }
    var messages = LicenseMessages()
    messages.activated = "License saved, but no signed Pro entitlement was returned."
    messages.activatedPro = "Pro entitlement restored."
    messages.refreshed = "License refreshed, but Pro is not active."
    messages.refreshedPro = "License refreshed."
    messages.removed = "License removed. Free ingestion remains available."

    let pro = LicenseController(client: client(server, store: InMemoryLicenseStore(), publicKey: pub), messages: messages)
    #expect(pro.messages == messages)
    await pro.activate("RK-1")
    #expect(pro.message == "Pro entitlement restored.")
    await pro.refresh()
    #expect(pro.message == "License refreshed.")
    await pro.remove()
    #expect(pro.message == "License removed. Free ingestion remains available.")

    // Not Pro (token bound to another device): the non-Pro strings apply.
    let other = try await LoopbackLicenseServer(signingKey: key, deviceID: "someone-else")
    defer { other.stop() }
    let notPro = LicenseController(client: client(other, store: InMemoryLicenseStore(), publicKey: pub), deviceState: InMemoryLicenseDeviceState(), messages: messages)
    await notPro.activate("RK-1")
    #expect(notPro.message == "License saved, but no signed Pro entitlement was returned.")
    await notPro.refresh()
    #expect(notPro.message == "License refreshed, but Pro is not active.")
    await notPro.activateLease("RK-L")
    #expect(notPro.message == "License saved, but no signed Pro entitlement was returned.")
}
