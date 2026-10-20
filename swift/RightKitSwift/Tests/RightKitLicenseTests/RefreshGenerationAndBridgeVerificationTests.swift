import CryptoKit
import Foundation
import Testing
@testable import RightKitLicense

// PK16-4 (HeardRight iOS parity): compare-and-save refresh, immediate removal, adoption of removals
// a pre-RightKit controller queued, and verified bridge exchange. These mirror the Rust
// `rightkit-license` race vector "a deactivate racing a slow activate never resurrects the record"
// (tests/client_race.rs) and the bridge wire bodies of tests/client_bridge.rs, minus the Rust-only
// `deviceClass` field the shipped Swift apps do not send.

// MARK: Gated network

/// One test's server: answers every request at once, except the first whose path contains `hold`,
/// which waits until the test releases it (or the client cancels it).
private final class NetworkGate: @unchecked Sendable {
    private let lock = NSLock()
    private let holdSubstring: String
    private let otherBody: String
    private var releaseHandler: (@Sendable (Int, Data) -> Void)?
    private var _held = false
    private var _cancelled = false
    private var _paths: [String] = []

    init(hold: String, otherBody: String = "{}") {
        holdSubstring = hold
        self.otherBody = otherBody
    }

    var held: Bool { lock.withLock { _held } }
    var cancelled: Bool { lock.withLock { _cancelled } }
    var paths: [String] { lock.withLock { _paths } }

    func shouldHold(path: String) -> Bool {
        lock.withLock {
            _paths.append(path)
            return !_held && path.contains(holdSubstring)
        }
    }

    func hold(_ release: @escaping @Sendable (Int, Data) -> Void) {
        lock.withLock { releaseHandler = release; _held = true }
    }

    func markCancelled() { lock.withLock { _cancelled = true } }
    var immediateBody: Data { Data(otherBody.utf8) }

    func release(status: Int = 200, body: String) {
        let handler = lock.withLock { releaseHandler }
        handler?(status, Data(body.utf8))
    }
}

private final class GateRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var gates: [String: NetworkGate] = [:]
    func register(_ gate: NetworkGate, host: String) { lock.withLock { gates[host] = gate } }
    func gate(for host: String) -> NetworkGate? { lock.withLock { gates[host] } }
}

private final class GatedProtocol: URLProtocol, @unchecked Sendable {
    static let registry = GateRegistry()
    private let lock = NSLock()
    private var finished = false

    override class func canInit(with request: URLRequest) -> Bool {
        guard let host = request.url?.host else { return false }
        return registry.gate(for: host) != nil
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url, let gate = Self.registry.gate(for: url.host ?? "") else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        if gate.shouldHold(path: url.path) {
            gate.hold { [weak self] status, data in self?.answer(status: status, data: data) }
        } else {
            answer(status: 200, data: gate.immediateBody)
        }
    }

    override func stopLoading() {
        guard let url = request.url, let gate = Self.registry.gate(for: url.host ?? "") else { return }
        let wasFinished = lock.withLock { finished }
        if !wasFinished { gate.markCancelled() }
    }

    private func answer(status: Int, data: Data) {
        let proceed = lock.withLock { () -> Bool in
            if finished { return false }
            finished = true
            return true
        }
        guard proceed, let url = request.url,
              let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: ["Content-Type": "application/json"])
        else { return }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
}

private struct WaitTimedOut: Error {}

@MainActor
private func waitUntil(timeout: TimeInterval = 5, _ condition: () -> Bool) async throws {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
        guard Date() < deadline else { throw WaitTimedOut() }
        try await Task.sleep(nanoseconds: 5_000_000)
    }
}

// MARK: Fixtures

private func base64URL(_ data: Data) -> String {
    data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
}

private func publicKeyText(_ key: Curve25519.Signing.PrivateKey) -> String { base64URL(key.publicKey.rawRepresentation) }

private func signedLease(_ key: Curve25519.Signing.PrivateKey, leaseId: String, generation: Int, deviceIdHash: String) -> LicenseLeaseEnvelope {
    let unsigned = LicenseLeaseEnvelope(
        leaseId: leaseId, deviceIdHash: deviceIdHash, pool: "mobile", issuedAt: "2026-01-01T00:00:00Z",
        expiresAt: "2033-05-18T03:33:20Z", graceSeconds: 86_400, generation: generation, entitlement: "token",
        entitlementScope: nil
    )
    let signature = base64URL(try! key.signature(for: Data(unsigned.canonicalJSON().utf8)))
    return LicenseLeaseEnvelope(
        leaseId: leaseId, deviceIdHash: deviceIdHash, pool: "mobile", issuedAt: unsigned.issuedAt,
        expiresAt: unsigned.expiresAt, graceSeconds: 86_400, generation: generation, entitlement: "token",
        signature: signature, entitlementScope: nil
    )
}

private func leaseObject(_ lease: LicenseLeaseEnvelope) -> [String: Any] {
    [
        "leaseId": lease.leaseId, "deviceIdHash": lease.deviceIdHash, "pool": lease.pool, "issuedAt": lease.issuedAt,
        "expiresAt": lease.expiresAt, "graceSeconds": lease.graceSeconds, "generation": lease.generation,
        "entitlement": lease.entitlement ?? "", "signature": lease.signature ?? "",
    ]
}

private func renewBody(_ key: Curve25519.Signing.PrivateKey, leaseId: String, generation: Int) -> String {
    let lease = signedLease(key, leaseId: leaseId, generation: generation, deviceIdHash: LicenseActivationClient.deviceIDHash("dev"))
    let object: [String: Any] = ["ok": true, "status": "active", "lease": leaseObject(lease)]
    return String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
}

private func makeClient(host: String, store: InMemoryLicenseStore, publicKey: String? = nil) -> LicenseActivationClient {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [GatedProtocol.self]
    return LicenseActivationClient(
        configuration: LicenseClientConfiguration(
            app: "heardright", baseURL: URL(string: "https://\(host)/v1/licenses")!, deviceID: { "dev" },
            session: URLSession(configuration: configuration), publicKeyBase64url: publicKey
        ),
        store: store
    )
}

private func newHost() -> String { "gate-\(UUID().uuidString.lowercased()).test" }

private let leasedRecord = StoredLicense(status: "active", deviceId: "dev", activationToken: "act", leaseId: "lease-0", leaseGeneration: 1)

// MARK: Shared lease-envelope vector

/// `crates/rightkit-license/test-vectors/lease-envelope-v1.json`, signed by Node's Ed25519 over the
/// seven-field canonical JSON. The values are copied verbatim (that file is read-only here); a change there
/// must be copied here. It proves a lease the Rust, TypeScript and Swift sides all accept, so a bridge or
/// renewal envelope verifies the same on iOS as on the desktop.
@Test func theLeaseEnvelopeVectorSharedWithRustVerifiesAndTamperingFails() throws {
    let canonical = #"{"leaseId":"lease_01HZX","deviceIdHash":"9f2c1a7be04d3f56","pool":"pro","issuedAt":"2026-10-01T00:00:00.000Z","expiresAt":"2026-11-01T00:00:00.000Z","graceSeconds":604800,"generation":3}"#
    let publicKey = "6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw"
    let signature = "oPLiisnLIjJDhXrJVyv5t9paEpBFRChGEsIj_85N-8y6F-ZMFmFJEqxWksNDlmxf_dkrdWp9Tpd1hvIik0PiBw"
    func envelope(generation: Int = 3, pool: String = "pro") -> LicenseLeaseEnvelope {
        LicenseLeaseEnvelope(
            leaseId: "lease_01HZX", deviceIdHash: "9f2c1a7be04d3f56", pool: pool, issuedAt: "2026-10-01T00:00:00.000Z",
            expiresAt: "2026-11-01T00:00:00.000Z", graceSeconds: 604_800, generation: generation,
            entitlement: "ENTITLEMENT.TOKEN.NOT-COVERED-BY-ENVELOPE-SIGNATURE", signature: signature
        )
    }
    #expect(envelope().canonicalJSON() == canonical)
    let key = try Curve25519.Signing.PublicKey(rawRepresentation: try #require(Data(base64urlEncoded: publicKey)))
    try envelope().verifySignature(publicKey: key)
    #expect(throws: LicenseError.badLeaseSignature) { try envelope(generation: 4).verifySignature(publicKey: key) }
    #expect(throws: LicenseError.badLeaseSignature) { try envelope(pool: "mobile").verifySignature(publicKey: key) }
    // The same envelope goes through the shared builder a bridge exchange uses.
    let license = try LicenseActivationClient.license(
        fromLease: envelope(), activationToken: "act", deviceId: "dev", publicKeyBase64url: publicKey
    )
    #expect(license.leaseId == "lease_01HZX" && license.leaseGeneration == 3)
}

// MARK: Compare-and-save refresh

@MainActor
@Test func removeSupersedesAnInFlightRefreshWithoutWaitingForTheNetwork() async throws {
    let host = newHost()
    let gate = NetworkGate(hold: "renew")
    GatedProtocol.registry.register(gate, host: host)
    let store = InMemoryLicenseStore(leasedRecord)
    let controller = LicenseController(client: makeClient(host: host, store: store), deviceState: InMemoryLicenseDeviceState())

    let refresh = Task { @MainActor in await controller.refresh() }
    try await waitUntil { gate.held }
    // The renewal request is still waiting on the server; removal must not queue behind it.
    await controller.remove()
    #expect(controller.license == nil)
    #expect(await store.load() == nil, "Pro ends at once even though the server never answered the refresh")
    #expect(controller.isBridgeConsumptionSuppressed)

    gate.release(body: "{}") // a late answer, if the cancelled request still delivers one
    await refresh.value
    #expect(controller.license == nil)
    #expect(await store.load() == nil, "the superseded refresh never resurrects the removed record")
    #expect(gate.cancelled)
}

@MainActor
@Test func aRefreshOvertakenByPersistNeverOverwritesTheNewerRecord() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let host = newHost()
    let gate = NetworkGate(hold: "renew")
    GatedProtocol.registry.register(gate, host: host)
    let store = InMemoryLicenseStore(leasedRecord)
    let controller = LicenseController(client: makeClient(host: host, store: store, publicKey: publicKeyText(key)))

    let refresh = Task { @MainActor in await controller.refresh(silent: true) }
    try await waitUntil { gate.held }
    let newer = StoredLicense(status: "active", deviceId: "dev", activationToken: "act-new", leaseId: "lease-1", leaseGeneration: 4)
    let before = controller.mutationGeneration
    let persist = Task { @MainActor in try await controller.persist(newer) }
    try await waitUntil { controller.mutationGeneration != before }

    gate.release(body: renewBody(key, leaseId: "lease-0", generation: 9))
    await refresh.value
    try await persist.value
    #expect(await store.load() == newer, "the older refresh answer was dropped, then the newer record was written")
    #expect(controller.license == newer)
}

@MainActor
@Test func aRefreshAnswerIsDroppedWhenSomethingElseWroteTheStoreMeanwhile() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let host = newHost()
    let gate = NetworkGate(hold: "renew")
    GatedProtocol.registry.register(gate, host: host)
    let store = InMemoryLicenseStore(leasedRecord)
    let controller = LicenseController(client: makeClient(host: host, store: store, publicKey: publicKeyText(key)))

    let refresh = Task { @MainActor in await controller.refresh() }
    try await waitUntil { gate.held }
    // Another process (an extension sharing the Keychain) wrote the record; the controller did not.
    let external = StoredLicense(status: "active", deviceId: "dev", activationToken: "act-external", leaseId: "lease-ext", leaseGeneration: 7)
    try await store.save(external)
    gate.release(body: renewBody(key, leaseId: "lease-0", generation: 9))
    await refresh.value
    #expect(await store.load() == external)
    #expect(controller.message.isEmpty, "a dropped refresh leaves no message")
}

@MainActor
@Test func anUndisturbedRefreshStillSavesAndPublishesTheRenewedLease() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let host = newHost()
    let gate = NetworkGate(hold: "never-held", otherBody: renewBody(key, leaseId: "lease-0", generation: 9))
    GatedProtocol.registry.register(gate, host: host)
    let store = InMemoryLicenseStore(leasedRecord)
    let controller = LicenseController(client: makeClient(host: host, store: store, publicKey: publicKeyText(key)))

    await controller.refresh()
    #expect(controller.license?.leaseGeneration == 9)
    #expect(await store.load()?.leaseGeneration == 9)
    #expect(controller.message == "License refreshed.")
}

@Test func fetchRefreshProposesTheRenewedLeaseButPersistsNothing() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let host = newHost()
    GatedProtocol.registry.register(NetworkGate(hold: "never-held", otherBody: renewBody(key, leaseId: "lease-0", generation: 9)), host: host)
    let store = InMemoryLicenseStore(leasedRecord)
    let client = makeClient(host: host, store: store, publicKey: publicKeyText(key))

    let result = try await client.fetchRefresh(for: leasedRecord)
    #expect(result.shouldPersist)
    #expect(!result.deviceRemoved)
    #expect(result.license.leaseGeneration == 9)
    #expect(await store.load() == leasedRecord, "fetchRefresh never writes")
}

@Test func fetchRefreshRoutesLegacyRecordsAndSkipsTheNetworkWithoutAToken() async throws {
    let host = newHost()
    let gate = NetworkGate(hold: "never-held", otherBody: #"{"ok":true,"status":"active","activationToken":"act-2"}"#)
    GatedProtocol.registry.register(gate, host: host)
    let client = makeClient(host: host, store: InMemoryLicenseStore())

    let legacy = StoredLicense(status: "active", deviceId: "dev", activationToken: "act")
    let refreshed = try await client.fetchRefresh(for: legacy)
    #expect(refreshed.shouldPersist && refreshed.license.activationToken == "act-2")
    #expect(gate.paths == ["/v1/licenses/refresh"])

    let tokenless = StoredLicense(status: "active", deviceId: "dev")
    let untouched = try await client.fetchRefresh(for: tokenless)
    #expect(!untouched.shouldPersist && untouched.license == tokenless)
    #expect(gate.paths == ["/v1/licenses/refresh"], "no second request")
}

@Test func fetchRefreshMarksADeviceRemovedLeaseRevoked() async throws {
    let host = newHost()
    let gate = NetworkGate(hold: "renew")
    GatedProtocol.registry.register(gate, host: host)
    let client = makeClient(host: host, store: InMemoryLicenseStore(leasedRecord))
    async let result = client.fetchRefresh(for: leasedRecord)
    while !gate.held { try await Task.sleep(nanoseconds: 5_000_000) }
    gate.release(status: 404, body: #"{"error":"gone"}"#)
    let outcome = try await result
    #expect(outcome.deviceRemoved && outcome.shouldPersist)
    #expect(outcome.license.status == "revoked" && outcome.license.signedToken == nil)
}

// MARK: Legacy removal adoption

@Test func adoptLegacyQueuesLeaseAndTokenRemovalsOnceAndKeepsUnreadableDataUnadopted() async throws {
    let state = InMemoryLicenseDeviceState()
    let queue = PendingLicenseRemovalQueue(storage: state, namespace: "heardright")
    let leaseJSON = Data(#"[{"deviceIdHash":"hash-1","leaseId":"lease-1"},{"deviceIdHash":"","leaseId":"x"}]"#.utf8)

    #expect(await queue.adoptLegacy(leaseRemovalsJSON: leaseJSON, activationTokens: ["  tok-1  ", " "]) == 2)
    #expect(await queue.pending == [.lease(deviceIdHash: "hash-1", leaseId: "lease-1"), .activation(activationToken: "tok-1")])
    #expect(await queue.adoptLegacy(leaseRemovalsJSON: leaseJSON, activationTokens: ["tok-1"]) == 0, "already queued")

    #expect(await queue.adoptLegacy(leaseRemovalsJSON: Data("not json".utf8)) == nil, "unreadable legacy data is not adopted")
    #expect(await queue.pending.count == 2)
    #expect(await queue.adoptLegacy(leaseRemovalsJSON: nil, activationTokens: []) == 0)
}

@Test func legacyMigrationDeletesTheOldKeysOnlyAfterTheQueueTookThem() async throws {
    let suite = "rightkit.test.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    defer { defaults.removePersistentDomain(forName: suite) }
    defaults.set("tok-9", forKey: "legacy.token")
    defaults.set(Data(#"[{"deviceIdHash":"h","leaseId":"l"}]"#.utf8), forKey: "legacy.leases")
    let queue = PendingLicenseRemovalQueue(storage: InMemoryLicenseDeviceState(), namespace: "heardright")

    let added = await LegacyLicenseRemovalMigration.adopt(into: queue, defaults: defaults, activationTokenKey: "legacy.token", leaseRemovalsKey: "legacy.leases")
    #expect(added == 2)
    #expect(defaults.string(forKey: "legacy.token") == nil && defaults.data(forKey: "legacy.leases") == nil)
    #expect(await queue.pending.contains(.activation(activationToken: "tok-9")))

    defaults.set(Data("garbage".utf8), forKey: "legacy.leases")
    let refused = await LegacyLicenseRemovalMigration.adopt(into: queue, defaults: defaults, activationTokenKey: nil, leaseRemovalsKey: "legacy.leases")
    #expect(refused == nil)
    #expect(defaults.data(forKey: "legacy.leases") != nil, "an unreadable legacy value stays for the next launch")
}

@Test func aLegacyBridgeSuppressionFlagMovesIntoTheGateBeforeItsKeyIsDeleted() {
    let suite = "rightkit.test.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    defer { defaults.removePersistentDomain(forName: suite) }
    let gate = BridgeConsumptionGate(storage: InMemoryLicenseDeviceState(), namespace: "heardright")

    // Nothing set: nothing suppressed, nothing to delete.
    #expect(!LegacyLicenseRemovalMigration.adoptBridgeSuppression(into: gate, defaults: defaults, legacyKey: "legacy.suppressed"))
    #expect(!gate.isSuppressed)

    defaults.set(true, forKey: "legacy.suppressed")
    #expect(LegacyLicenseRemovalMigration.adoptBridgeSuppression(into: gate, defaults: defaults, legacyKey: "legacy.suppressed"))
    #expect(gate.isSuppressed, "a device that removed its license keeps refusing the bridge after the move")
    #expect(defaults.object(forKey: "legacy.suppressed") == nil)

    // A gate that could not record it keeps the legacy flag.
    struct Unwritable: LicenseDeviceStateStoring {
        func data(forKey key: String) -> Data? { nil }
        func set(_ data: Data?, forKey key: String) {}
    }
    let stuck = BridgeConsumptionGate(storage: Unwritable(), namespace: "heardright")
    defaults.set(true, forKey: "legacy.suppressed")
    #expect(!LegacyLicenseRemovalMigration.adoptBridgeSuppression(into: stuck, defaults: defaults, legacyKey: "legacy.suppressed"))
    #expect(defaults.bool(forKey: "legacy.suppressed"))
}

// MARK: Verified bridge results

private final class StubBridgeTransport: ContinuityBridgeTransport, @unchecked Sendable {
    private let lock = NSLock()
    private let body: String
    private var _bodies: [[String: String]] = []
    init(body: String) { self.body = body }
    var bodies: [[String: String]] { lock.withLock { _bodies } }
    func send(_ request: URLRequest) async throws -> (Data, URLResponse) {
        let sent = (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String]) ?? [:]
        lock.withLock { _bodies.append(sent) }
        return (Data(body.utf8), HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
    }
}

@Test func heardRightBridgePayloadsAreWireIdenticalToTheShippedAppBodies() {
    // The literal bodies `ios-native/Shared/ProBridge.swift` builds today.
    #expect(ContinuityBridgePayload.fromActivation(app: "heardright", activationToken: "act", deviceId: "dev", version: "1.2") ==
        ["app": "heardright", "activationToken": "act", "deviceId": "dev", "version": "1.2"])
    #expect(ContinuityBridgePayload.fromStoreKit(app: "heardright", signedTransaction: "jws", deviceId: "dev", platform: "ipados", publishBridge: false, signedMobileTransaction: "mobile", version: "1.2") ==
        ["app": "heardright", "signedTransaction": "jws", "deviceId": "dev", "platform": "ipados", "publishBridge": "false", "version": "1.2", "signedMobileTransaction": "mobile"])
    #expect(ContinuityBridgePayload.exchange(app: "heardright", bridgeToken: "bt", deviceId: "dev", platform: "ios", version: "1.2") ==
        ["app": "heardright", "bridgeToken": "bt", "deviceId": "dev", "platform": "ios", "version": "1.2"])
}

@Test func exchangeForLicenseReturnsAVerifiedUnsavedDeviceLicense() async throws {
    let key = Curve25519.Signing.PrivateKey()
    let lease = signedLease(key, leaseId: "lease-b", generation: 3, deviceIdHash: LicenseActivationClient.deviceIDHash("dev"))
    let object: [String: Any] = ["ok": true, "activationToken": "act-b", "lease": leaseObject(lease)]
    let transport = StubBridgeTransport(body: String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self))
    let client = ContinuityBridgeClient(app: "heardright", baseURL: URL(string: "https://example.test/v1/licenses")!, appVersion: "1.2", transport: transport, platform: { "ios" })

    let license = try await client.exchangeForLicense(bridgeToken: "bt", deviceId: "dev", publicKeyBase64url: publicKeyText(key))
    #expect(license.leaseId == "lease-b" && license.leaseGeneration == 3)
    #expect(license.activationToken == "act-b" && license.deviceId == "dev")
    #expect(license.key == nil)
    #expect(transport.bodies == [["app": "heardright", "bridgeToken": "bt", "deviceId": "dev", "platform": "ios", "version": "1.2"]])
}

@Test func aBridgeLeaseThatDoesNotVerifyOrIsMissingIsRejectedNeverPartiallyAccepted() throws {
    let key = Curve25519.Signing.PrivateKey()
    let other = Curve25519.Signing.PrivateKey()
    let lease = signedLease(key, leaseId: "lease-c", generation: 1, deviceIdHash: "h")

    let wrongKey = ContinuityBridgeExchange(ok: true, activationToken: "act", lease: lease)
    #expect(throws: ContinuityBridgeFailure.rejected("Could not save the device entitlement locally.")) {
        _ = try wrongKey.verifiedLicense(deviceId: "dev", publicKeyBase64url: publicKeyText(other))
    }
    let noLease = ContinuityBridgeExchange(ok: true, activationToken: "act", lease: nil)
    #expect(throws: ContinuityBridgeFailure.rejected("The bridge exchange returned no device lease.")) {
        _ = try noLease.verifiedLicense(deviceId: "dev", publicKeyBase64url: publicKeyText(key))
    }
    let noToken = ContinuityBridgeExchange(ok: true, message: "no token", activationToken: nil, lease: lease)
    #expect(throws: ContinuityBridgeFailure.rejected("no token")) {
        _ = try noToken.verifiedLicense(deviceId: "dev", publicKeyBase64url: publicKeyText(key))
    }
    let notOK = ContinuityBridgeExchange(ok: false, message: "refused", activationToken: "act", lease: lease)
    #expect(throws: ContinuityBridgeFailure.rejected("refused")) {
        _ = try notOK.verifiedLicense(deviceId: "dev", publicKeyBase64url: publicKeyText(key))
    }
}

@Test func aStoreKitIssueVerifiesItsActivationAndKeepsTheServerVerifiedKeyReadable() throws {
    let key = Curve25519.Signing.PrivateKey()
    let lease = signedLease(key, leaseId: "lease-d", generation: 2, deviceIdHash: "h")
    let issue = ContinuityBridgeIssue(
        ok: true, bridgeToken: "ignored", licenseKey: "HR-FULL",
        activation: LicenseServerResponse(ok: true, status: "active", activationToken: "act-d", lease: lease)
    )
    #expect(issue.licenseKey == "HR-FULL", "readable before and independent of verification")
    let license = try issue.verifiedLicense(deviceId: "dev", publicKeyBase64url: publicKeyText(key))
    #expect(license.leaseId == "lease-d" && license.activationToken == "act-d")

    let leaseless = ContinuityBridgeIssue(ok: true, licenseKey: "HR-FULL", activation: nil)
    #expect(throws: ContinuityBridgeFailure.rejected("The authorization response contained no device lease.")) {
        _ = try leaseless.verifiedLicense(deviceId: "dev", publicKeyBase64url: publicKeyText(key))
    }
    #expect(leaseless.licenseKey == "HR-FULL")
}
