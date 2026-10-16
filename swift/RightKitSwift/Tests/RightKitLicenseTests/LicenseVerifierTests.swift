import CryptoKit
import Foundation
import Testing

@testable import RightKitLicense

/// The bundled vector is byte-identical to the one the Rust and TypeScript verifiers consume
/// (`rightkit/swift/contracts/license-v2.json`, asserted by the suite contract). Verifying it
/// here is what proves a token accepted on one platform is not rejected on another.
private struct Vector: Decodable {
    let publicKeyBase64url: String
    let token: String
    let expectedApp: String
    let expectedDeviceId: String
    let nowUnix: Int
    let expectedTier: String
    let expectedFeatures: [String]
}

private func loadVector() throws -> Vector {
    let url = try #require(Bundle.module.url(forResource: "license-v2", withExtension: "json"))
    return try JSONDecoder().decode(Vector.self, from: Data(contentsOf: url))
}

@Test func acceptsTheSharedPublicVector() throws {
    let vector = try loadVector()
    let verifier = try LicenseVerifier(publicKeyBase64url: vector.publicKeyBase64url)

    let claims = try verifier.verify(
        token: vector.token,
        app: vector.expectedApp,
        deviceId: vector.expectedDeviceId,
        now: vector.nowUnix,
    )

    #expect(claims.app == vector.expectedApp)
    #expect(claims.tier == vector.expectedTier)
    #expect(claims.features == vector.expectedFeatures)
    #expect(claims.deviceId == vector.expectedDeviceId)
    #expect(claims.hasFeature("wake_word"))
    #expect(claims.issuedAt <= vector.nowUnix)
    #expect(claims.expiresAt > vector.nowUnix)
    #expect(claims.isPro)
}

@Test func onlyProIsPro() {
    // Two states, so the entitlement question is a boolean. Anything that is not the pro tier —
    // free, a typo, a tier someone invents later — is not pro, and cannot become pro by ranking
    // above something.
    func claims(tier: String) -> LicenseClaims {
        LicenseClaims(
            version: 2, id: "lic", app: "heardright", tier: tier,
            features: [], deviceId: "dev-1", issuedAt: 0, expiresAt: .max,
        )
    }

    #expect(claims(tier: "pro").isPro)
    #expect(!claims(tier: "free").isPro)
    #expect(!claims(tier: "").isPro)
    #expect(!claims(tier: "Pro").isPro)
    #expect(!claims(tier: "max").isPro)
}

@Test func rejectsATamperedPayload() throws {
    let vector = try loadVector()
    let verifier = try LicenseVerifier(publicKeyBase64url: vector.publicKeyBase64url)

    // Re-sign nothing, just upgrade the tier — the whole point of signing the token.
    let parts = vector.token.split(separator: ".")
    let payload = try #require(Data(base64urlEncoded: String(parts[0])))
    let forged = String(data: payload, encoding: .utf8)!.replacingOccurrences(of: "\"tier\":\"pro\"", with: "\"tier\":\"max\"")
    let forgedToken = forged.data(using: .utf8)!.base64urlEncodedString() + "." + parts[1]

    #expect(throws: LicenseError.badSignature) {
        try verifier.verify(
            token: forgedToken,
            app: vector.expectedApp,
            deviceId: vector.expectedDeviceId,
            now: vector.nowUnix,
        )
    }
}

@Test func rejectsAnExpiredOrForeignToken() throws {
    let vector = try loadVector()
    let verifier = try LicenseVerifier(publicKeyBase64url: vector.publicKeyBase64url)

    // exp is EXCLUSIVE (Rust parity, `now < exp`): the second before verifies, exp itself does not.
    #expect(throws: Never.self) {
        try verifier.verify(
            token: vector.token,
            app: vector.expectedApp,
            deviceId: vector.expectedDeviceId,
            now: 9_999_999_998,
        )
    }
    #expect(throws: LicenseError.expired(expiresAt: 9_999_999_999, now: 9_999_999_999)) {
        try verifier.verify(
            token: vector.token,
            app: vector.expectedApp,
            deviceId: vector.expectedDeviceId,
            now: 9_999_999_999,
        )
    }

    #expect(throws: LicenseError.wrongDevice(expected: "other-device", actual: "dev-1")) {
        try verifier.verify(
            token: vector.token,
            app: vector.expectedApp,
            deviceId: "other-device",
            now: vector.nowUnix,
        )
    }

    // A HeardRight token must not unlock ScrapeRight, even though the signature is genuine.
    #expect(throws: LicenseError.wrongApp(expected: "scraperight", actual: "heardright")) {
        try verifier.verify(
            token: vector.token,
            app: "scraperight",
            deviceId: vector.expectedDeviceId,
            now: vector.nowUnix,
        )
    }
}

@Test func rejectsFutureIssuedBeyondSkewAndExpiryBoundary() throws {
    // scopedToken: iat 1_000, exp 9_999.
    let (token, key) = try scopedToken([:])
    let v = try LicenseVerifier(publicKeyBase64url: key)
    func verify(_ now: Int) throws { _ = try v.verify(token: token, app: "heardright", deviceId: "dev-1", now: now) }
    try verify(1_000 - 300)  // exactly at tolerance: accepted
    #expect(throws: LicenseError.issuedInTheFuture(issuedAt: 1_000, now: 699)) { try verify(699) }
    try verify(9_998)
    #expect(throws: LicenseError.expired(expiresAt: 9_999, now: 9_999)) { try verify(9_999) }
}

// MARK: - entitlement scope claim (0.3.4)

/// Signs `extra` into an otherwise fixed v2 payload with a throwaway key.
private func scopedToken(_ extra: [String: Any]) throws -> (token: String, publicKey: String) {
    let key = Curve25519.Signing.PrivateKey()
    var payload: [String: Any] = [
        "v": 2, "id": "lic", "app": "heardright", "tier": "pro", "features": [String](),
        "device_id": "dev-1", "iat": 1_000, "exp": 9_999,
    ]
    payload.merge(extra) { $1 }
    let data = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    let token = data.base64urlEncodedString() + "." + (try key.signature(for: data)).base64urlEncodedString()
    return (token, key.publicKey.rawRepresentation.base64urlEncodedString())
}

private func verifiedScope(_ extra: [String: Any]) throws -> String? {
    let (token, publicKey) = try scopedToken(extra)
    return try LicenseVerifier(publicKeyBase64url: publicKey).verify(token: token, app: "heardright", deviceId: "dev-1", now: 2_000).scope
}

@Test func decodesTheSignedEntitlementScopeClaim() throws {
    // What the server signs (and ScrapeRight/HeardRight assert): `entitlement_scope`.
    #expect(try verifiedScope(["entitlement_scope": "mobile"]) == "mobile")
    #expect(try verifiedScope(["entitlement_scope": "full"]) == "full")
}

@Test func fallsBackToTheLegacyScopeClaim() throws {
    #expect(try verifiedScope(["scope": "mobile"]) == "mobile")
    // Both present: the signed `entitlement_scope` wins.
    #expect(try verifiedScope(["entitlement_scope": "full", "scope": "mobile"]) == "full")
}

@Test func aTokenWithNoScopeClaimHasNilScope() throws {
    #expect(try verifiedScope([:]) == nil)
    // The shared contract vector carries no scope either.
    let vector = try loadVector()
    let claims = try LicenseVerifier(publicKeyBase64url: vector.publicKeyBase64url).verify(
        token: vector.token, app: vector.expectedApp, deviceId: vector.expectedDeviceId, now: vector.nowUnix
    )
    #expect(claims.scope == nil)
}

extension Data {
    func base64urlEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
