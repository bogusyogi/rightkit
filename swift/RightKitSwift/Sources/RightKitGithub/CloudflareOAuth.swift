#if os(iOS)
// RIGHTKIT-GAP: OAuth PKCE sign-in stays local until RightKitSwift 0.9.0 (RightKitOAuth, held; bogusyogi/rightkit#627) is tagged; BLOCKED-UNPUB, see docs/rightkit-exceptions/outside-M-rows.md B058
import AuthenticationServices
import CryptoKit
import Foundation
import UIKit

/// One Cloudflare OAuth grant: an access token good for API calls, the
/// refresh token used to mint the next one, its expiry, and the scopes
/// Cloudflare actually granted (may be narrower than what was requested).
public struct CloudflareOAuthCredential: Codable, Sendable, Equatable {
    public let accessToken: String
    public let refreshToken: String
    public let expiresAt: Date
    public let scopes: [String]

    public init(accessToken: String, refreshToken: String, expiresAt: Date, scopes: [String]) {
        self.accessToken = accessToken
        self.refreshToken = refreshToken
        self.expiresAt = expiresAt
        self.scopes = scopes
    }
}

/// Slim dependency surface `CloudflareAccountStore` holds, so tests (and A2's
/// lane) can substitute a stub without touching `ASWebAuthenticationSession`.
public protocol CloudflareOAuthClientProtocol: Sendable {
    func authorize() async throws -> CloudflareOAuthCredential
    func refresh(_ credential: CloudflareOAuthCredential) async throws -> CloudflareOAuthCredential
    func revoke(_ credential: CloudflareOAuthCredential) async
}

/// Transport seam over `URLSession` so tests can stub Cloudflare's token
/// endpoint without a live network call.
public protocol CloudflareOAuthTransport: Sendable {
    func data(for request: URLRequest) async throws -> (Data, URLResponse)
}

extension URLSession: CloudflareOAuthTransport {}

/// Cloudflare OAuth (Authorization Code + PKCE, S256) for a native public
/// client — no client secret ships in the app. Mirrors `GitHubOAuth`'s shape:
/// a thin `@MainActor` presenter around `ASWebAuthenticationSession`, plus a
/// couple of plain token-endpoint calls.
@MainActor
public final class CloudflareOAuth: NSObject, CloudflareOAuthClientProtocol, ASWebAuthenticationPresentationContextProviding {
    private static let authorizeURL = URL(string: "https://dash.cloudflare.com/oauth2/auth")!
    private static let tokenURL = URL(string: "https://dash.cloudflare.com/oauth2/token")!
    private static let revokeURL = URL(string: "https://dash.cloudflare.com/oauth2/revoke")!
    private static let callbackScheme = "coderight"
    private static let redirectURI = "coderight://oauth/cloudflare"
    private static let scopes = ["account:read", "workers:write", "pages:write", "d1:write", "r2:write"]

    private let transport: CloudflareOAuthTransport
    private var session: ASWebAuthenticationSession?

    public init(transport: CloudflareOAuthTransport = URLSession.shared) {
        self.transport = transport
    }

    public func authorize() async throws -> CloudflareOAuthCredential {
        let clientID = try Self.validClientID(Self.configuredClientID())
        let verifier = try Self.makeCodeVerifier()
        let challenge = Self.codeChallenge(forVerifier: verifier)
        let state = try Self.makeState()
        let authorizeURL = try Self.buildAuthorizeURL(
            clientID: clientID,
            codeChallenge: challenge,
            state: state
        )

        let callbackURL = try await presentAuthenticationSession(url: authorizeURL)
        let code = try Self.authorizationCode(from: callbackURL, expectedState: state)

        let response = try await tokenRequest([
            "grant_type": "authorization_code",
            "client_id": clientID,
            "code": code,
            "redirect_uri": Self.redirectURI,
            "code_verifier": verifier
        ])
        return try Self.credential(from: response)
    }

    public func refresh(_ credential: CloudflareOAuthCredential) async throws -> CloudflareOAuthCredential {
        let clientID = try Self.validClientID(Self.configuredClientID())
        let response = try await tokenRequest([
            "grant_type": "refresh_token",
            "client_id": clientID,
            "refresh_token": credential.refreshToken
        ])
        return try Self.credential(from: response)
    }

    public func revoke(_ credential: CloudflareOAuthCredential) async {
        // Best-effort: revocation failing (network down, token already
        // expired) should never block a local disconnect, so this is async
        // rather than async throws and swallows every error.
        guard let clientID = try? Self.validClientID(Self.configuredClientID()) else { return }
        var request = Self.formRequest(
            url: Self.revokeURL,
            values: ["client_id": clientID, "token": credential.refreshToken]
        )
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        _ = try? await transport.data(for: request)
    }

    public func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow) ?? ASPresentationAnchor()
    }

    // MARK: - Authorization Code presentation

    private func presentAuthenticationSession(url: URL) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            let session = ASWebAuthenticationSession(
                url: url,
                callbackURLScheme: Self.callbackScheme
            ) { callbackURL, error in
                if let callbackURL {
                    continuation.resume(returning: callbackURL)
                } else if let error {
                    continuation.resume(throwing: CloudflareOAuthFailure.canceled(error.localizedDescription))
                } else {
                    continuation.resume(throwing: CloudflareOAuthFailure.message("Cloudflare login did not return a result."))
                }
            }
            session.presentationContextProvider = self
            session.prefersEphemeralWebBrowserSession = true
            self.session = session
            guard session.start() else {
                continuation.resume(throwing: CloudflareOAuthFailure.message("Could not open Cloudflare login."))
                return
            }
        }
    }

    // MARK: - Token endpoint

    private func tokenRequest(_ values: [String: String]) async throws -> TokenResponse {
        var request = Self.formRequest(url: Self.tokenURL, values: values)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, response) = try await transport.data(for: request)
        try Self.requireSuccess(response, data: data)
        do {
            return try Self.decoder.decode(TokenResponse.self, from: data)
        } catch {
            throw CloudflareOAuthFailure.message("Cloudflare returned an unreadable token response.")
        }
    }

    private static func credential(from response: TokenResponse) throws -> CloudflareOAuthCredential {
        guard let accessToken = response.accessToken, !accessToken.isEmpty else {
            throw CloudflareOAuthFailure.message(response.firstErrorMessage ?? "Cloudflare returned no access token.")
        }
        guard let refreshToken = response.refreshToken, !refreshToken.isEmpty else {
            throw CloudflareOAuthFailure.message("Cloudflare returned no refresh token.")
        }
        let expiresIn = response.expiresIn ?? 0
        let grantedScopes = (response.scope ?? Self.scopes.joined(separator: " "))
            .split(separator: " ")
            .map(String.init)
        return CloudflareOAuthCredential(
            accessToken: accessToken,
            refreshToken: refreshToken,
            expiresAt: Date().addingTimeInterval(TimeInterval(expiresIn)),
            scopes: grantedScopes
        )
    }

    // MARK: - Authorize URL + PKCE

    static func buildAuthorizeURL(clientID: String, codeChallenge: String, state: String) throws -> URL {
        var components = URLComponents(url: authorizeURL, resolvingAgainstBaseURL: false)
        components?.queryItems = [
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "client_id", value: clientID),
            URLQueryItem(name: "redirect_uri", value: redirectURI),
            URLQueryItem(name: "scope", value: scopes.joined(separator: " ")),
            URLQueryItem(name: "state", value: state),
            URLQueryItem(name: "code_challenge", value: codeChallenge),
            URLQueryItem(name: "code_challenge_method", value: "S256")
        ]
        guard let url = components?.url else {
            throw CloudflareOAuthFailure.message("Could not construct Cloudflare login URL.")
        }
        return url
    }

    /// Form-decode a query value: `URLComponents` handles percent-escapes but
    /// not the `+`-as-space convention OAuth providers use in error text.
    private static func formDecoded(_ value: String) -> String {
        value.replacingOccurrences(of: "+", with: " ")
    }

    static func authorizationCode(from callbackURL: URL, expectedState: String) throws -> String {
        let components = URLComponents(url: callbackURL, resolvingAgainstBaseURL: false)
        let items = components?.queryItems ?? []
        if let error = items.first(where: { $0.name == "error" })?.value {
            let description = items.first(where: { $0.name == "error_description" })?.value
            // OAuth error parameters are form-encoded, where `+` means a space.
            // `URLComponents` percent-decodes but leaves `+` alone, so without
            // this the user is shown literal text like "User+cancelled".
            throw CloudflareOAuthFailure.message(formDecoded(description ?? error))
        }
        guard let state = items.first(where: { $0.name == "state" })?.value, state == expectedState else {
            throw CloudflareOAuthFailure.message("Cloudflare login response failed state verification.")
        }
        guard let code = items.first(where: { $0.name == "code" })?.value, !code.isEmpty else {
            throw CloudflareOAuthFailure.message("Cloudflare login did not return an authorization code.")
        }
        return code
    }

    /// RFC 7636 §4.1: 43–128 char unreserved-character string. 32 random
    /// bytes, base64url-encoded, comfortably lands in that range.
    static func makeCodeVerifier() throws -> String {
        base64URLEncode(try randomBytes(count: 32))
    }

    /// RFC 7636 §4.2 S256: `BASE64URL-ENCODE(SHA256(ASCII(verifier)))`.
    static func codeChallenge(forVerifier verifier: String) -> String {
        let digest = SHA256.hash(data: Data(verifier.utf8))
        return base64URLEncode(Data(digest))
    }

    static func makeState() throws -> String {
        base64URLEncode(try randomBytes(count: 16))
    }

    private static func randomBytes(count: Int) throws -> Data {
        var bytes = [UInt8](repeating: 0, count: count)
        let status = SecRandomCopyBytes(kSecRandomDefault, count, &bytes)
        guard status == errSecSuccess else {
            throw CloudflareOAuthFailure.message("Couldn't generate secure random bytes (status \(status)).")
        }
        return Data(bytes)
    }

    private static func base64URLEncode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    // MARK: - Client ID

    static func configuredClientID() -> String {
        Bundle.main.object(forInfoDictionaryKey: "CodeRightCloudflareClientID") as? String ?? ""
    }

    static func validClientID(_ value: String) throws -> String {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, !value.contains("$(") else {
            throw CloudflareOAuthFailure.message("Cloudflare login is not configured in this build.")
        }
        return value
    }

    // MARK: - HTTP plumbing

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        return decoder
    }()

    private static func formRequest(url: URL, values: [String: String]) -> URLRequest {
        var components = URLComponents()
        components.queryItems = values.map { URLQueryItem(name: $0.key, value: $0.value) }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.httpBody = components.percentEncodedQuery?.data(using: .utf8)
        return request
    }

    private static func requireSuccess(_ response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            if let body = try? decoder.decode(TokenResponse.self, from: data),
               let message = body.firstErrorMessage {
                throw CloudflareOAuthFailure.message(message)
            }
            throw CloudflareOAuthFailure.message("Cloudflare returned HTTP \(status).")
        }
    }
}

/// Decodes both a success payload (`access_token`, …) and Cloudflare's
/// standard error envelope (`errors: [{ message }]`, or OAuth-style
/// `error`/`error_description`), so one type covers every response shape the
/// token and revoke endpoints can return.
private struct TokenResponse: Decodable {
    let accessToken: String?
    let refreshToken: String?
    let expiresIn: Int?
    let scope: String?
    let error: String?
    let errorDescription: String?
    let errors: [CloudflareAPIError]?

    var firstErrorMessage: String? {
        errors?.first?.message ?? errorDescription ?? error
    }

    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case expiresIn = "expires_in"
        case scope
        case error
        case errorDescription = "error_description"
        case errors
    }
}

private struct CloudflareAPIError: Decodable {
    let code: Int?
    let message: String
}

enum CloudflareOAuthFailure: LocalizedError, Equatable {
    case message(String)
    case canceled(String)

    var errorDescription: String? {
        switch self {
        case let .message(value), let .canceled(value):
            return value
        }
    }
}
#endif
