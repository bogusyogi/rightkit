import CryptoKit
import Foundation

/// A verified entitlement. Only ``LicenseVerifier`` can produce one, so holding a value of this
/// type *is* the proof — there is no way to construct an unverified claim set and pass it around.
public struct LicenseClaims: Equatable, Sendable {
    public init(
        version: Int, id: String, app: String, tier: String,
        features: [String], deviceId: String, issuedAt: Int, expiresAt: Int,
        scope: String? = nil,
        licenseKind: String? = nil,
        entitlementStage: String? = nil,
        licenseIssuedAt: Int? = nil,
        confirmationDueAt: Int? = nil,
    ) {
        self.version = version
        self.id = id
        self.app = app
        self.tier = tier
        self.features = features
        self.deviceId = deviceId
        self.issuedAt = issuedAt
        self.expiresAt = expiresAt
        self.scope = scope
        self.licenseKind = licenseKind
        self.entitlementStage = entitlementStage
        self.licenseIssuedAt = licenseIssuedAt
        self.confirmationDueAt = confirmationDueAt
    }

    public let version: Int
    public let id: String
    public let app: String
    public let tier: String
    public let features: [String]
    public let deviceId: String
    public let issuedAt: Int
    public let expiresAt: Int
    /// Server-owned entitlement scope, decoded from the signed `entitlement_scope` claim (falling back
    /// to a legacy `scope` claim). Missing means legacy full scope.
    public let scope: String?
    public let licenseKind: String?
    public let entitlementStage: String?
    public let licenseIssuedAt: Int?
    public let confirmationDueAt: Int?

    public func hasFeature(_ feature: String) -> Bool { features.contains(feature) }

    /// Pro or not. That is the whole entitlement ladder — HeardRight sells one paid tier.
    ///
    /// The extracted code had a `tierAtLeast` ranking (`pro` 2, `plus` 1, unknown 0, compared with
    /// `>=`). There is no `plus` plan, so it ranked a tier that does not exist, and an unknown
    /// target ranked 0 — meaning `tierAtLeast("max")` was true for EVERY caller, free users
    /// included. It was dead code with a hole in it, so it is gone rather than fixed.
    public var isPro: Bool { tier == "pro" }
}

public enum LicenseError: Error, Equatable, Sendable {
    case malformedToken
    case malformedPublicKey
    case badSignature
    case wrongApp(expected: String, actual: String)
    case wrongDevice(expected: String, actual: String)
    case expired(expiresAt: Int, now: Int)
    case unsupportedVersion(Int)
    case malformedLeaseSignature
    case badLeaseSignature
    case invalidResponse
    case serverStatus(Int, String)
    case transport(String)
}

extension LicenseError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .malformedToken: return "License token is malformed."
        case .malformedPublicKey: return "License public key is malformed."
        case .badSignature, .badLeaseSignature: return "License signature is invalid."
        case let .wrongApp(expected, actual): return "License belongs to \(actual), not \(expected)."
        case let .wrongDevice(expected, actual): return "License belongs to \(actual), not \(expected)."
        case let .expired(expiresAt, now): return "License expired at \(expiresAt) (now \(now))."
        case let .unsupportedVersion(version): return "License version \(version) is unsupported."
        case .malformedLeaseSignature: return "License lease signature is missing or malformed."
        case .invalidResponse: return "License server returned an unreadable response."
        case let .serverStatus(status, message): return message.isEmpty ? "License server returned HTTP \(status)." : message
        case let .transport(message): return message
        }
    }
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

    /// Token schema versions accepted by default: only v2, the current signer's format.
    public static let defaultAcceptedVersions: Set<Int> = [2]

    /// Signature first, then claims. An expired token from the right key and a forged token are very
    /// different failures, and only the signature check decides whether the payload is worth reading.
    ///
    /// - Parameter acceptedVersions: the `v` claims accepted; anything else throws
    ///   ``LicenseError/unsupportedVersion(_:)``. Default `[2]`. ScrapeRight passes `[1, 2]`: its
    ///   original verifier never checked `v`. A v1 token is decoded by the same payload decoder, so it
    ///   must still carry the core claims `v`, `id`, `app`, `tier`, `features`, `device_id`, `iat`
    ///   and `exp` (exactly what ScrapeRight's decoder required). v1 has none of the additive claims
    ///   (`entitlement_scope`/`scope`, `license_kind`, `entitlement_stage`, `license_issued_at`,
    ///   `confirmation_due_at`); they decode as nil, and a nil scope means legacy full rights.
    public func verify(
        token: String, app: String, deviceId: String, now: Int,
        acceptedVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions
    ) throws -> LicenseClaims {
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
        guard acceptedVersions.contains(payload.v) else { throw LicenseError.unsupportedVersion(payload.v) }
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
            scope: payload.entitlement_scope ?? payload.scope,
            licenseKind: payload.license_kind,
            entitlementStage: payload.entitlement_stage,
            licenseIssuedAt: payload.license_issued_at,
            confirmationDueAt: payload.confirmation_due_at,
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
        /// The claim signed tokens carry. Preferred over `scope` when both are present.
        let entitlement_scope: String?
        /// Legacy spelling some fixtures and older issuers used.
        let scope: String?
        let license_kind: String?
        let entitlement_stage: String?
        let license_issued_at: Int?
        let confirmation_due_at: Int?
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
