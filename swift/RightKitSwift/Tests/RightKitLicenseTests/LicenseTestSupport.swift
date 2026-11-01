import CryptoKit
import Foundation
import RightKitLicense
@testable import RightKitLicense
/// Throwaway signing key plus builders for signed entitlement tokens and lease
/// envelopes. No production signatures are fabricated and no server is contacted.
struct LicenseTestKeys {
    let signingKey = Curve25519.Signing.PrivateKey()
    var publicKeyBase64url: String { Self.base64url(signingKey.publicKey.rawRepresentation) }

    static func base64url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    /// `base64url(payload).base64url(signature)` with the claim names the server signs.
    func entitlementToken(
        app: String = "scraperight", deviceId: String, tier: String = "pro",
        scope: String? = "mobile", exp: Int64 = 4_102_444_800, version: Int = 2
    ) -> String {
        var payload = """
        {"v":\(version),"id":"lic_test","app":"\(app)","tier":"\(tier)","features":["pro"],"device_id":"\(deviceId)","iat":1700000000,"exp":\(exp)
        """
        if let scope { payload += ",\"entitlement_scope\":\"\(scope)\"" }
        payload += "}"
        let signature = try! signingKey.signature(for: Data(payload.utf8))
        return "\(Self.base64url(Data(payload.utf8))).\(Self.base64url(Data(signature)))"
    }

    /// A lease envelope signed over its canonical bytes, carrying a token bound to `deviceId`.
    func signedLease(
        deviceId: String, leaseId: String = "lease-test-1", scope: String? = "mobile",
        tokenScope: String?? = nil, generation: Int = 1, signed: Bool = true
    ) -> LicenseLeaseEnvelope {
        let hash = LicenseActivationClient.deviceIDHash(deviceId)
        let token = entitlementToken(deviceId: hash, scope: tokenScope ?? scope)
        let unsigned = LicenseLeaseEnvelope(
            leaseId: leaseId, deviceIdHash: hash, pool: "mobile", issuedAt: "2026-01-01T00:00:00Z",
            expiresAt: "2030-01-01T00:00:00Z", graceSeconds: 604_800, generation: generation,
            entitlement: token, signature: nil, entitlementScope: scope
        )
        guard signed else { return unsigned }
        let signature = try! signingKey.signature(for: Data(unsigned.canonicalJSON().utf8))
        return LicenseLeaseEnvelope(
            leaseId: leaseId, deviceIdHash: hash, pool: "mobile", issuedAt: unsigned.issuedAt,
            expiresAt: unsigned.expiresAt, graceSeconds: 604_800, generation: generation,
            entitlement: token, signature: Self.base64url(Data(signature)), entitlementScope: scope
        )
    }

    func json(_ lease: LicenseLeaseEnvelope) -> [String: Any] {
        try! JSONSerialization.jsonObject(with: JSONEncoder().encode(lease)) as! [String: Any]
    }
}
