import Foundation
import CryptoKit
import Network
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

    private func receive(_ connection: NWConnection) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, _, _ in
            guard let self, let data else { connection.cancel(); return }
            self.respond(connection, request: data)
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

@Test func accessRequirementsFailClosedForEmptyOrUnknownRequirements() {
    let license = StoredLicense(status: "active", deviceId: "d", validUntil: nil, entitlements: LicenseEntitlements(v: 1, tier: "pro", features: ["routing"]))
    #expect(satisfiesRequirement(license, .pro))
    #expect(satisfiesRequirement(license, LicenseRequirement(feature: "routing")))
    #expect(!satisfiesRequirement(license, LicenseRequirement(tier: "max")))
    #expect(!satisfiesRequirement(license, LicenseRequirement()))
}

@Test func inMemoryStoreRoundTripsAndRemoves() async throws {
    let store = InMemoryLicenseStore()
    let license = StoredLicense(status: "active", deviceId: "d")
    try await store.save(license)
    #expect(await store.load() == license)
    try await store.remove()
    #expect(await store.load() == nil)
}
