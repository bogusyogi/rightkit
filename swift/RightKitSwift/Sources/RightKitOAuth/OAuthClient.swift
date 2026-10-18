import Foundation
import RightKitHTTP

/// Provider endpoints and client parameters.
public struct OAuthConfig: Sendable {
    public var authorizationURL: URL
    public var tokenURL: URL
    public var clientID: String
    /// Only for providers that require one for a native client; most do not (PKCE replaces it).
    public var clientSecret: String?
    public var scopes: [String]
    /// Extra authorization query parameters (consent behaviour, `login_hint`, ...).
    public var authorizationParams: [(name: String, value: String)]
    /// Extra token-endpoint parameters (for example RFC 8707 `resource`).
    public var tokenParams: [(name: String, value: String)]
    /// Where the provider sends the user back. For ``OAuthSignIn`` this is a custom scheme
    /// registered by the app (`myapp://oauth/callback`).
    public var redirectURI: URL
    /// RFC 7009 revocation endpoint, when the provider has one.
    public var revocationURL: URL?

    public init(
        authorizationURL: URL, tokenURL: URL, clientID: String, redirectURI: URL,
        clientSecret: String? = nil, scopes: [String] = [],
        authorizationParams: [(name: String, value: String)] = [], tokenParams: [(name: String, value: String)] = [],
        revocationURL: URL? = nil,
    ) {
        self.authorizationURL = authorizationURL
        self.tokenURL = tokenURL
        self.clientID = clientID
        self.redirectURI = redirectURI
        self.clientSecret = clientSecret
        self.scopes = scopes
        self.authorizationParams = authorizationParams
        self.tokenParams = tokenParams
        self.revocationURL = revocationURL
    }

    private static let reservedTokenParams: Set<String> = [
        "grant_type", "code", "redirect_uri", "client_id", "client_secret", "code_verifier", "refresh_token", "token", "token_type_hint",
    ]

    /// Endpoints must be HTTPS (loopback HTTP allowed), the client id non-empty, and no extra token
    /// parameter may override one the client sets.
    public func validate() throws {
        if clientID.trimmingCharacters(in: .whitespaces).isEmpty { throw OAuthError.invalidConfig("client_id is empty") }
        var endpoints: [(String, URL)] = [("authorization_url", authorizationURL), ("token_url", tokenURL)]
        if let revocationURL { endpoints.append(("revocation_url", revocationURL)) }
        for (name, url) in endpoints {
            guard HTTPClient.isPermitted(url, allowInsecureLoopback: true) else {
                throw OAuthError.invalidConfig("\(name) must use HTTPS")
            }
        }
        guard let scheme = redirectURI.scheme, !scheme.isEmpty else { throw OAuthError.invalidConfig("redirect_uri has no scheme") }
        for param in tokenParams where Self.reservedTokenParams.contains(param.name.lowercased()) {
            throw OAuthError.invalidConfig("token parameter \(param.name) is set by the client and cannot be overridden")
        }
    }
}

/// One in-flight authorization: the URL to open, and the secrets needed to finish it.
public struct AuthorizationRequest: Sendable, Equatable, CustomStringConvertible {
    public let url: URL
    public let state: String
    public let redirectURI: URL
    /// PKCE code verifier; sent only in the token request.
    public let codeVerifier: String

    public var description: String { "AuthorizationRequest(url: \(url.host ?? ""), state: [redacted], verifier: [redacted])" }
}

public enum TokenTypeHint: String, Sendable { case accessToken = "access_token", refreshToken = "refresh_token" }

/// Builds authorization requests and talks to the token endpoint.
public struct OAuthClient: Sendable {
    public let config: OAuthConfig
    public let http: HTTPClient
    private let now: @Sendable () -> Date

    public init(config: OAuthConfig, http: HTTPClient = HTTPClient(), now: @escaping @Sendable () -> Date = { Date() }) throws {
        try config.validate()
        self.config = config
        self.http = http
        self.now = now
    }

    // MARK: Authorization

    /// A fresh PKCE + state authorization. `extra` is appended after the configured parameters.
    public func authorizationRequest(extra: [(name: String, value: String)] = []) throws -> AuthorizationRequest {
        let pkce = try PKCE.generate()
        let state = try PKCE.generateState()
        return try authorizationRequest(pkce: pkce, state: state, extra: extra)
    }

    func authorizationRequest(pkce: PKCE, state: String, extra: [(name: String, value: String)] = []) throws -> AuthorizationRequest {
        var pairs: [(String, String)] = [
            ("response_type", "code"),
            ("client_id", config.clientID),
            ("redirect_uri", config.redirectURI.absoluteString),
            ("state", state),
            ("code_challenge", pkce.challenge),
            ("code_challenge_method", PKCE.challengeMethod),
        ]
        if !config.scopes.isEmpty { pairs.append(("scope", config.scopes.joined(separator: " "))) }
        pairs += config.authorizationParams.map { ($0.name, $0.value) }
        pairs += extra.map { ($0.name, $0.value) }
        guard var components = URLComponents(url: config.authorizationURL, resolvingAgainstBaseURL: false) else {
            throw OAuthError.invalidConfig("authorization_url is not a URL")
        }
        let added = pairs.map { "\(Self.queryEncode($0.0))=\(Self.queryEncode($0.1))" }.joined(separator: "&")
        components.percentEncodedQuery = [components.percentEncodedQuery, added].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "&")
        guard let url = components.url else { throw OAuthError.invalidConfig("authorization_url is not a URL") }
        return AuthorizationRequest(url: url, state: state, redirectURI: config.redirectURI, codeVerifier: pkce.verifier)
    }

    /// Percent-encoding for a query component (space is `%20`, a literal `+` is `%2B`).
    static func queryEncode(_ text: String) -> String { formEncode(text).replacingOccurrences(of: "+", with: "%20") }

    /// Reads the authorization code from the provider's redirect. Checks that the URL is this flow's
    /// redirect, that `state` matches, and surfaces a provider `error`.
    public func authorizationCode(from callback: URL, for request: AuthorizationRequest) throws -> String {
        guard let got = URLComponents(url: callback, resolvingAgainstBaseURL: false),
              let want = URLComponents(url: request.redirectURI, resolvingAgainstBaseURL: false),
              got.scheme?.lowercased() == want.scheme?.lowercased(),
              (got.host ?? "").lowercased() == (want.host ?? "").lowercased(),
              got.path == want.path
        else { throw OAuthError.callback("callback is not this sign-in's redirect") }
        let items = got.queryItems ?? []
        func value(_ name: String) -> String? { items.first { $0.name == name }?.value }
        guard let state = value("state"), Self.constantTimeEqual(state, request.state) else {
            throw OAuthError.callback("state does not match")
        }
        if let error = value("error") {
            throw OAuthError.provider(status: 0, error: error, description: value("error_description"))
        }
        guard let code = value("code"), !code.isEmpty else { throw OAuthError.callback("callback has no authorization code") }
        return code
    }

    static func constantTimeEqual(_ a: String, _ b: String) -> Bool {
        let x = Array(a.utf8), y = Array(b.utf8)
        guard x.count == y.count else { return false }
        var diff: UInt8 = 0
        for i in 0..<x.count { diff |= x[i] ^ y[i] }
        return diff == 0
    }

    // MARK: Token endpoint

    public func exchangeCode(_ request: AuthorizationRequest, code: String) async throws -> TokenSet {
        guard !code.isEmpty else { throw OAuthError.invalidConfig("authorization code is empty") }
        let fields: [(String, String)] = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", request.redirectURI.absoluteString),
            ("client_id", config.clientID),
            ("code_verifier", request.codeVerifier),
        ]
        return try await tokenRequest(fields, secrets: [code, request.codeVerifier], keepRefresh: nil)
    }

    /// Refreshes the access token. A missing refresh token, or the provider's `invalid_grant`, is
    /// ``OAuthError/needsReauth``. A refresh response without a new refresh token keeps the old one,
    /// and without an ID token keeps the sign-in's.
    public func refresh(_ current: TokenSet) async throws -> TokenSet {
        guard let refresh = current.refreshToken, !refresh.isEmpty else { throw OAuthError.needsReauth }
        let fields: [(String, String)] = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", config.clientID),
        ]
        var fresh = try await tokenRequest(fields, secrets: [refresh], keepRefresh: refresh)
        if fresh.idToken == nil { fresh.idToken = current.idToken }
        return fresh
    }

    /// RFC 7009 revocation. Success is any 2xx; a provider that no longer knows the token also
    /// answers success. Without ``OAuthConfig/revocationURL`` this is ``OAuthError/invalidConfig(_:)``.
    public func revoke(_ token: String, hint: TokenTypeHint? = nil) async throws {
        guard let url = config.revocationURL else { throw OAuthError.invalidConfig("revocation_url is not configured") }
        guard !token.isEmpty else { throw OAuthError.invalidConfig("token is empty") }
        var fields: [(String, String)] = [("token", token)]
        if let hint { fields.append(("token_type_hint", hint.rawValue)) }
        fields.append(("client_id", config.clientID))
        if let secret = config.clientSecret { fields.append(("client_secret", secret)) }
        fields += config.tokenParams.map { ($0.name, $0.value) }
        let response = try await post(url, fields, secrets: [token])
        try Self.throwIfProviderError(response, secrets: [token])
    }

    private func tokenRequest(_ base: [(String, String)], secrets: [String], keepRefresh: String?) async throws -> TokenSet {
        var fields = base
        if let secret = config.clientSecret { fields.append(("client_secret", secret)) }
        fields += config.tokenParams.map { ($0.name, $0.value) }
        let response = try await post(config.tokenURL, fields, secrets: secrets + [config.clientSecret].compactMap { $0 })
        let all = secrets + [config.clientSecret].compactMap { $0 }
        try Self.throwIfProviderError(response, secrets: all)
        guard (200..<300).contains(response.status) else {
            throw OAuthError.provider(status: response.status, error: "http_error", description: nil)
        }
        guard let object = try? JSONSerialization.jsonObject(with: response.data) as? [String: Any],
              let access = object["access_token"] as? String, !access.isEmpty
        else { throw OAuthError.callback("token response omitted access_token") }
        func string(_ key: String) -> String? { (object[key] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        let expires: TimeInterval? = (object["expires_in"] as? NSNumber)?.doubleValue
            ?? (object["expires_in"] as? String).flatMap(TimeInterval.init)
        return TokenSet(
            accessToken: access, tokenType: (object["token_type"] as? String) ?? "Bearer", expiresIn: expires,
            refreshToken: string("refresh_token") ?? keepRefresh, scope: object["scope"] as? String,
            idToken: string("id_token"), obtainedAt: now(),
        )
    }

    private func post(_ url: URL, _ fields: [(String, String)], secrets: [String]) async throws -> HTTPResponse {
        // GitHub answers form-encoded unless asked for JSON, and reports some errors with HTTP 200.
        let request = HTTPRequest.form(.post, url, fields: fields, headers: ["Accept": "application/json"])
        var accepted = request
        accepted.acceptedStatuses = Set(400..<500)
        do { return try await http.send(accepted) } catch let error as HTTPError {
            if error == .cancelled { throw OAuthError.cancelled }
            throw OAuthError.http(.init(description: Self.scrub("\(error)", secrets: secrets)))
        }
    }

    /// A body with an `error` field is a provider error whatever the HTTP status says.
    private static func throwIfProviderError(_ response: HTTPResponse, secrets: [String]) throws {
        guard let object = try? JSONSerialization.jsonObject(with: response.data) as? [String: Any],
              let error = object["error"] as? String else { return }
        if error == "invalid_grant" || error == "bad_refresh_token" { throw OAuthError.needsReauth }
        let description = (object["error_description"] as? String).map { scrub($0, secrets: secrets) }
        throw OAuthError.provider(status: response.status, error: scrub(error, secrets: secrets), description: description)
    }

    static func scrub(_ text: String, secrets: [String]) -> String {
        var out = text
        for secret in secrets where !secret.isEmpty { out = out.replacingOccurrences(of: secret, with: "[redacted]") }
        return out
    }
}

/// Keeps one sign-in fresh: loads it from a store, refreshes an expiring access token once even
/// when many callers ask at the same time, and persists the result.
public actor OAuthSession {
    private let client: OAuthClient
    private let store: any OAuthTokenStore
    private var refreshing: Task<TokenSet, Error>?

    public init(client: OAuthClient, store: any OAuthTokenStore) {
        self.client = client
        self.store = store
    }

    public func save(_ tokens: TokenSet) throws { try store.save(tokens) }

    public func current() throws -> TokenSet? { try store.load() }

    /// An access token valid for at least `skew` seconds. ``OAuthError/needsReauth`` when there is
    /// no sign-in or the refresh token was rejected (the stored sign-in is then cleared).
    public func validAccessToken(skew: TimeInterval = 60) async throws -> String {
        guard let tokens = try store.load() else { throw OAuthError.needsReauth }
        if !tokens.isExpired(skew: skew) { return tokens.accessToken }
        if let refreshing { return try await refreshing.value.accessToken }
        let client = client
        let store = store
        let task = Task<TokenSet, Error> {
            do {
                let fresh = try await client.refresh(tokens)
                try store.save(fresh)
                return fresh
            } catch OAuthError.needsReauth {
                try? store.clear()
                throw OAuthError.needsReauth
            }
        }
        refreshing = task
        defer { refreshing = nil }
        return try await task.value.accessToken
    }

    /// Clears the stored sign-in, after a best-effort revocation of the refresh token (or access
    /// token) when the provider has a revocation endpoint.
    public func signOut(revoke: Bool = true) async {
        if revoke, client.config.revocationURL != nil, let tokens = try? store.load() {
            if let refresh = tokens.refreshToken {
                try? await client.revoke(refresh, hint: .refreshToken)
            } else {
                try? await client.revoke(tokens.accessToken, hint: .accessToken)
            }
        }
        try? store.clear()
    }
}
