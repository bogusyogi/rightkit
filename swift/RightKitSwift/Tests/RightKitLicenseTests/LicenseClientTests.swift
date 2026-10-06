import Foundation
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

    var lastPath: String { lock.withLock { _lastPath } }
    var lastLicenseKey: String { lock.withLock { _lastLicenseKey } }

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

    func stop() { listener.cancel() }

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
        }
        let payload: [String: Any] = [
            "v": 2, "id": "loopback-license", "app": "coderight", "tier": "pro",
            "features": ["routing"], "device_id": deviceID, "iat": 1_700_000_000, "exp": 2_000_000_000,
        ]
        let payloadData = try! JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
        let token = payloadData.base64URLEncodedString() + "." + (try! signingKey.signature(for: payloadData)).base64URLEncodedString()
        let response: [String: Any] = [
            "ok": true, "status": "active", "activationToken": "activation-loopback",
            "signedToken": token, "entitlements": ["v": 1, "tier": "pro", "features": ["routing"]],
        ]
        let bytes = try! JSONSerialization.data(withJSONObject: response, options: [.sortedKeys])
        let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(bytes.count)\r\nConnection: close\r\n\r\n"
        connection.send(content: Data(header.utf8) + bytes, completion: .contentProcessed { _ in connection.cancel() })
    }
}

private extension Data {
    func base64URLEncodedString() -> String {
        base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}

private func signedToken(_ key: Curve25519.Signing.PrivateKey, app: String = "coderight", tier: String = "pro", deviceID: String = "dev", exp: Int = 2_000_000_000) -> String {
    let payload: [String: Any] = ["v": 2, "id": "lic", "app": app, "tier": tier, "features": ["routing"], "device_id": deviceID, "iat": 1_700_000_000, "exp": exp]
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
