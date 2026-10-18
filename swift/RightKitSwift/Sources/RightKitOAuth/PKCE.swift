import CryptoKit
import Foundation
import Security

// OAuth 2.0 authorization-code flow with PKCE (RFC 7636, S256 only): the Swift counterpart of the
// Rust owner `rightkit-oauth`. Same wire: S256 challenge, 32-octet state, form-encoded token
// requests with `Accept: application/json`, `invalid_grant` meaning "sign in again", and the
// stored-token JSON record (`v`, `access`, `type`, `expires_in`, `renew`, `scope`, `id`,
// `obtained_at`) so a token set written by one platform reads on the other.

public enum OAuthError: Error, Equatable, Sendable {
    case invalidConfig(String)
    /// The system random source failed.
    case randomUnavailable
    /// The callback URL is not for this flow: wrong state, missing code, or no match.
    case callback(String)
    /// The provider reported an `error` (authorization callback or token endpoint).
    case provider(status: Int, error: String, description: String?)
    /// The refresh token is gone or was rejected (`invalid_grant`); the user must sign in again.
    case needsReauth
    case http(HTTPFailure)
    case cancelled
    case storage(String)

    /// An HTTP-layer failure carried without importing RightKitHTTP into your code.
    public struct HTTPFailure: Equatable, Sendable { public var description: String }
}

/// A PKCE code verifier with its S256 challenge. `description` never prints the verifier.
public struct PKCE: Sendable, Equatable, CustomStringConvertible {
    public static let verifierRange = 43...128

    public let verifier: String
    public let challenge: String
    public static let challengeMethod = "S256"

    /// Adopts an existing verifier after checking RFC 7636 section 4.1.
    public init(verifier: String) throws {
        try Self.validate(verifier)
        self.verifier = verifier
        self.challenge = Self.challenge(for: verifier)
    }

    /// A fresh verifier: 32 octets from the system CSPRNG, base64url (43 characters).
    public static func generate() throws -> PKCE {
        try PKCE(verifier: base64URL(try randomBytes(32)))
    }

    public var description: String { "PKCE(challenge: \(challenge), verifier: [redacted])" }

    /// `BASE64URL(SHA256(ASCII(verifier)))` without padding.
    public static func challenge(for verifier: String) -> String {
        base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
    }

    public static func validate(_ verifier: String) throws {
        guard verifierRange.contains(verifier.utf8.count) else {
            throw OAuthError.invalidConfig("PKCE verifier must be 43 to 128 characters")
        }
        let ok = verifier.utf8.allSatisfy {
            ($0 >= 0x30 && $0 <= 0x39) || ($0 >= 0x41 && $0 <= 0x5A) || ($0 >= 0x61 && $0 <= 0x7A)
                || $0 == UInt8(ascii: "-") || $0 == UInt8(ascii: ".") || $0 == UInt8(ascii: "_") || $0 == UInt8(ascii: "~")
        }
        guard ok else { throw OAuthError.invalidConfig("PKCE verifier contains characters outside [A-Za-z0-9-._~]") }
    }

    /// A fresh unguessable `state`: 32 octets, base64url.
    public static func generateState() throws -> String { base64URL(try randomBytes(32)) }
}

func randomBytes(_ count: Int) throws -> Data {
    var data = Data(count: count)
    let status = data.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, count, $0.baseAddress!) }
    guard status == errSecSuccess else { throw OAuthError.randomUnavailable }
    return data
}

func base64URL(_ data: Data) -> String {
    data.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}
