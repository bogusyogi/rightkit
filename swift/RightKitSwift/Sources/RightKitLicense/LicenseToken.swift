import CryptoKit
import Foundation

/// A verified entitlement. Only ``LicenseVerifier`` can produce one, so holding a value of this
/// type *is* the proof — there is no way to construct an unverified claim set and pass it around.
public struct LicenseClaims: Equatable, Sendable {
    public init(
        version: Int, id: String, app: String, tier: String,
        features: [String], deviceId: String, issuedAt: Int, expiresAt: Int,
    ) {
        self.version = version
        self.id = id
        self.app = app
        self.tier = tier
        self.features = features
        self.deviceId = deviceId
        self.issuedAt = issuedAt
        self.expiresAt = expiresAt
    }

    public let version: Int
    public let id: String
    public let app: String
    public let tier: String
    public let features: [String]
    public let deviceId: String
    public let issuedAt: Int
    public let expiresAt: Int

    public func hasFeature(_ feature: String) -> Bool { features.contains(feature) }

    public var isPro: Bool { tier == "pro" }

    /// The tiers an entitlement can actually carry. A gate can only ask for one of these, so a
    /// mistyped or renamed tier is a compile error rather than a silently open door.
    public enum Tier: Int, Comparable, Sendable {
        case free = 0
        case plus = 1
        case pro = 2

        public static func < (lhs: Tier, rhs: Tier) -> Bool { lhs.rawValue < rhs.rawValue }

        init(name: String) {
            switch name {
            case "pro": self = .pro
            case "plus": self = .plus
            default: self = .free
            }
        }
    }

    /// Fails CLOSED on a tier it does not know.
    ///
    /// HeardRight ranked an unrecognised tier 0 and compared `rank(mine) >= rank(target)`, so a gate
    /// asking for a tier that does not exist — `tierAtLeast("max")`, a typo, a renamed plan — was
    /// true for EVERY caller, free users included (0 >= 0). It is unused in HeardRight's product
    /// code, so nothing is leaking today; the trap was armed for the next feature gate. The String
    /// overload keeps the old call sites compiling and denies the unknown case.
    public func tierAtLeast(_ target: Tier) -> Bool { Tier(name: tier) >= target }

    public func tierAtLeast(_ target: String) -> Bool {
        guard let requested = knownTier(target) else { return false }
        return tierAtLeast(requested)
    }

    private func knownTier(_ name: String) -> Tier? {
        switch name {
        case "pro": return .pro
        case "plus": return .plus
        case "free": return .free
        default: return nil
        }
    }
}

public enum LicenseError: Error, Equatable, Sendable {
    case malformedToken
    case malformedPublicKey
    case badSignature
    case wrongApp(expected: String, actual: String)
    case wrongDevice(expected: String, actual: String)
    case expired(expiresAt: Int, now: Int)
    case unsupportedVersion(Int)
}

/// Verifies `base64url(payload).base64url(signature)` Ed25519 entitlement tokens — the same wire
/// format and the same `contracts/license-v2.json` vector the Rust and TypeScript verifiers use.
public struct LicenseVerifier: Sendable {
    private let publicKey: Curve25519.Signing.PublicKey

    public init(publicKeyBase64url: String) throws {
        guard let raw = Data(base64urlEncoded: publicKeyBase64url) else { throw LicenseError.malformedPublicKey }
        do {
            publicKey = try Curve25519.Signing.PublicKey(rawRepresentation: raw)
        } catch {
            throw LicenseError.malformedPublicKey
        }
    }

    /// Signature first, then claims. An expired token from the right key and a forged token are very
    /// different failures, and only the signature check decides whether the payload is worth reading.
    public func verify(token: String, app: String, deviceId: String, now: Int) throws -> LicenseClaims {
        let parts = token.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 2,
              let payloadData = Data(base64urlEncoded: String(parts[0])),
              let signature = Data(base64urlEncoded: String(parts[1]))
        else { throw LicenseError.malformedToken }

        // The signature covers the DECODED payload bytes, not the base64url text (verified against
        // the canonical vector). Never re-serialize the JSON before checking — sign what was signed.
        guard publicKey.isValidSignature(signature, for: payloadData) else { throw LicenseError.badSignature }

        let payload: Payload
        do {
            payload = try JSONDecoder().decode(Payload.self, from: payloadData)
        } catch {
            throw LicenseError.malformedToken
        }
        guard payload.v == 2 else { throw LicenseError.unsupportedVersion(payload.v) }
        guard payload.app == app else { throw LicenseError.wrongApp(expected: app, actual: payload.app) }
        guard payload.device_id == deviceId else {
            throw LicenseError.wrongDevice(expected: deviceId, actual: payload.device_id)
        }
        // Behaviour extracted from HeardRight, deliberately: `exp` is inclusive, and `iat` is NOT
        // enforced. Adding a not-yet-valid check here would reject, on a slightly fast clock, tokens
        // the shipping app accepts — an extraction is not the place to change what a licence means.
        guard now <= payload.exp else { throw LicenseError.expired(expiresAt: payload.exp, now: now) }

        return LicenseClaims(
            version: payload.v,
            id: payload.id,
            app: payload.app,
            tier: payload.tier,
            features: payload.features,
            deviceId: payload.device_id,
            issuedAt: payload.iat,
            expiresAt: payload.exp,
        )
    }

    private struct Payload: Decodable {
        let v: Int
        let id: String
        let app: String
        let tier: String
        let features: [String]
        let device_id: String
        let iat: Int
        let exp: Int
    }
}

extension Data {
    /// base64url, per RFC 4648 §5: `-`/`_` for `+`/`/`, padding stripped.
    init?(base64urlEncoded input: String) {
        var text = input.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        text.append(String(repeating: "=", count: (4 - text.count % 4) % 4))
        guard let decoded = Data(base64Encoded: text) else { return nil }
        self = decoded
    }
}
