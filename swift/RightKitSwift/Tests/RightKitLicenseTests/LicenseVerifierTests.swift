import Foundation
import Testing

@testable import RightKitLicense

/// The bundled vector is byte-identical to the one the Rust and TypeScript verifiers consume
/// (`tools/rightkit/swift/contracts/license-v2.json`, asserted by the suite contract). Verifying it
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
    #expect(claims.tierAtLeast("plus"))
    #expect(claims.tierAtLeast("pro"))

    // Extracted quirk, pinned rather than quietly changed: an unknown tier ranks 0, so asking for a
    // tier that does not exist SUCCEEDS. A typo'd gate ("max") grants access instead of denying it.
    // Preserved to keep the extraction behaviour-identical; flagged for a deliberate fix upstream.
    #expect(claims.tierAtLeast("max"))
    #expect(claims.tierAtLeast("typo"))
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

    // exp is INCLUSIVE in HeardRight: the last valid second must still verify, and the next must not.
    #expect(throws: Never.self) {
        try verifier.verify(
            token: vector.token,
            app: vector.expectedApp,
            deviceId: vector.expectedDeviceId,
            now: 9_999_999_999,
        )
    }
    #expect(throws: LicenseError.expired(expiresAt: 9_999_999_999, now: 10_000_000_000)) {
        try verifier.verify(
            token: vector.token,
            app: vector.expectedApp,
            deviceId: vector.expectedDeviceId,
            now: 10_000_000_000,
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

extension Data {
    func base64urlEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
