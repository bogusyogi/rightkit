import Foundation
import RightKitCredentials
import RightKitHTTP
import Testing
@testable import RightKitOAuth

private final class FakeTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [(Int, String)]
    private var _requests: [URLRequest] = []
    init(_ replies: [(Int, String)]) { self.replies = replies }
    var requests: [URLRequest] { lock.withLock { _requests } }
    var forms: [[String: String]] {
        requests.map { request in
            var out: [String: String] = [:]
            for pair in String(decoding: request.httpBody ?? Data(), as: UTF8.self).split(separator: "&") {
                let parts = pair.split(separator: "=", maxSplits: 1).map(String.init)
                out[parts[0]] = (parts.count > 1 ? parts[1] : "").replacingOccurrences(of: "+", with: " ").removingPercentEncoding
            }
            return out
        }
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (status, body) = lock.withLock { () -> (Int, String) in
            _requests.append(request)
            return replies.count > 1 ? replies.removeFirst() : replies[0]
        }
        try await Task.sleep(nanoseconds: 20_000_000)
        return (Data(body.utf8), HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!)
    }

    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) { throw URLError(.unsupportedURL) }
}

private func config(secret: String? = nil, revocation: Bool = false) -> OAuthConfig {
    OAuthConfig(
        authorizationURL: URL(string: "https://idp.example.com/authorize")!,
        tokenURL: URL(string: "https://idp.example.com/token")!,
        clientID: "client-1", redirectURI: URL(string: "myapp://oauth/callback")!,
        clientSecret: secret, scopes: ["openid", "email"],
        authorizationParams: [("prompt", "consent")],
        revocationURL: revocation ? URL(string: "https://idp.example.com/revoke")! : nil,
    )
}

private func client(_ fake: FakeTransport, config: OAuthConfig = config(), now: @escaping @Sendable () -> Date = { Date() }) throws -> OAuthClient {
    try OAuthClient(config: config, http: HTTPClient(transport: fake), now: now)
}

// MARK: PKCE

@Test func pkceMatchesRFC7636Vector() throws {
    let pkce = try PKCE(verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
    #expect(pkce.challenge == "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
    #expect(!pkce.description.contains("dBjft"))
}

@Test func pkceValidatesVerifierAndGeneratesFreshOnes() throws {
    #expect(throws: OAuthError.self) { try PKCE.validate(String(repeating: "a", count: 42)) }
    #expect(throws: OAuthError.self) { try PKCE.validate(String(repeating: "a", count: 129)) }
    #expect(throws: OAuthError.self) { try PKCE.validate(String(repeating: "a", count: 42) + "!") }
    let a = try PKCE.generate(), b = try PKCE.generate()
    #expect(a.verifier.count == 43)
    #expect(a.verifier != b.verifier)
    #expect(try PKCE.generateState().count == 43)
}

// MARK: Authorization

@Test func authorizationURLCarriesPKCEStateAndScopes() throws {
    let request = try client(FakeTransport([(200, "{}")])).authorizationRequest()
    let items = Dictionary(uniqueKeysWithValues: URLComponents(url: request.url, resolvingAgainstBaseURL: false)!.queryItems!.map { ($0.name, $0.value ?? "") })
    #expect(items["response_type"] == "code")
    #expect(items["client_id"] == "client-1")
    #expect(items["redirect_uri"] == "myapp://oauth/callback")
    #expect(items["code_challenge_method"] == "S256")
    #expect(items["code_challenge"] == PKCE.challenge(for: request.codeVerifier))
    #expect(items["state"] == request.state)
    #expect(items["scope"] == "openid email")
    #expect(items["prompt"] == "consent")
    #expect(request.url.absoluteString.contains("scope=openid%20email"))
}

@Test func callbackChecksRedirectStateAndProviderError() throws {
    let oauth = try client(FakeTransport([(200, "{}")]))
    let request = try oauth.authorizationRequest()
    let good = URL(string: "myapp://oauth/callback?code=abc&state=\(request.state)")!
    #expect(try oauth.authorizationCode(from: good, for: request) == "abc")

    let badState = URL(string: "myapp://oauth/callback?code=abc&state=nope")!
    #expect(throws: OAuthError.callback("state does not match")) { try oauth.authorizationCode(from: badState, for: request) }

    let otherPath = URL(string: "myapp://oauth/other?code=abc&state=\(request.state)")!
    #expect(throws: OAuthError.self) { try oauth.authorizationCode(from: otherPath, for: request) }

    let denied = URL(string: "myapp://oauth/callback?error=access_denied&state=\(request.state)")!
    #expect(throws: OAuthError.provider(status: 0, error: "access_denied", description: nil)) {
        try oauth.authorizationCode(from: denied, for: request)
    }
    let noCode = URL(string: "myapp://oauth/callback?state=\(request.state)")!
    #expect(throws: OAuthError.self) { try oauth.authorizationCode(from: noCode, for: request) }
}

// MARK: Token endpoint

@Test func exchangeSendsPKCEFormAndParsesTokens() async throws {
    let fake = FakeTransport([(200, #"{"access_token":"at","token_type":"Bearer","expires_in":3600,"refresh_token":"rt","scope":"openid","id_token":"idt"}"#)])
    let at = Date(timeIntervalSince1970: 1_000)
    let oauth = try client(fake, config: config(secret: "csecret"), now: { at })
    let request = try oauth.authorizationRequest()
    let tokens = try await oauth.exchangeCode(request, code: "the-code")
    #expect(tokens.accessToken == "at" && tokens.refreshToken == "rt" && tokens.idToken == "idt")
    #expect(tokens.expiresIn == 3600 && tokens.obtainedAt == at)
    let form = try #require(fake.forms.first)
    #expect(form["grant_type"] == "authorization_code")
    #expect(form["code"] == "the-code")
    #expect(form["code_verifier"] == request.codeVerifier)
    #expect(form["redirect_uri"] == "myapp://oauth/callback")
    #expect(form["client_id"] == "client-1")
    #expect(form["client_secret"] == "csecret")
    #expect(fake.requests.first?.value(forHTTPHeaderField: "Accept") == "application/json")
    #expect(!tokens.description.contains("at,") && !tokens.description.contains("rt"))
}

@Test func providerErrorsMapIncludingHTTP200AndInvalidGrant() async throws {
    let github = FakeTransport([(200, #"{"error":"bad_verification_code","error_description":"the-code is stale"}"#)])
    let oauth = try client(github)
    let request = try oauth.authorizationRequest()
    do {
        _ = try await oauth.exchangeCode(request, code: "the-code")
        Issue.record("expected failure")
    } catch let OAuthError.provider(status, error, description) {
        #expect(status == 200 && error == "bad_verification_code")
        #expect(description == "[redacted] is stale")
    }
    let grant = try client(FakeTransport([(400, #"{"error":"invalid_grant"}"#)]))
    await #expect(throws: OAuthError.needsReauth) { _ = try await grant.refresh(TokenSet(accessToken: "a", refreshToken: "r")) }
    await #expect(throws: OAuthError.needsReauth) { _ = try await grant.refresh(TokenSet(accessToken: "a")) }
    let server = try client(FakeTransport([(502, "<html>bad gateway</html>")]))
    await #expect(throws: OAuthError.self) { _ = try await server.refresh(TokenSet(accessToken: "a", refreshToken: "r")) }
}

@Test func refreshKeepsOldRefreshAndIdTokenWhenOmitted() async throws {
    let fake = FakeTransport([(200, #"{"access_token":"new","expires_in":"120"}"#)])
    let oauth = try client(fake)
    let fresh = try await oauth.refresh(TokenSet(accessToken: "old", refreshToken: "keep", idToken: "idt"))
    #expect(fresh.accessToken == "new" && fresh.refreshToken == "keep" && fresh.idToken == "idt" && fresh.expiresIn == 120)
    #expect(fake.forms.first?["grant_type"] == "refresh_token")
    #expect(fake.forms.first?["refresh_token"] == "keep")
}

@Test func revokeSendsHintAndNeedsEndpoint() async throws {
    let fake = FakeTransport([(200, "")])
    try await client(fake, config: config(revocation: true)).revoke("rt", hint: .refreshToken)
    #expect(fake.forms.first?["token"] == "rt")
    #expect(fake.forms.first?["token_type_hint"] == "refresh_token")
    await #expect(throws: OAuthError.self) { try await client(fake).revoke("rt") }
}

@Test func configRejectsInsecureEndpointsAndReservedTokenParams() {
    var insecure = config()
    insecure.tokenURL = URL(string: "http://idp.example.com/token")!
    #expect(throws: OAuthError.self) { try insecure.validate() }
    var reserved = config()
    reserved.tokenParams = [("Code_Verifier", "x")]
    #expect(throws: OAuthError.self) { try reserved.validate() }
    var empty = config()
    empty.clientID = " "
    #expect(throws: OAuthError.self) { try empty.validate() }
}

// MARK: Stored record

@Test func tokenRecordRoundTripsAndReadsTheRustFormat() throws {
    let tokens = TokenSet(accessToken: "a", tokenType: "Bearer", expiresIn: 60, refreshToken: "r", scope: "s", idToken: "i", obtainedAt: Date(timeIntervalSince1970: 500))
    #expect(try TokenSet(decoding: tokens.encoded()) == tokens)
    let rust = #"{"v":1,"access":"a2","type":"Bearer","expires_in":null,"renew":"","scope":null,"id":null,"obtained_at":7}"#
    let decoded = try TokenSet(decoding: Data(rust.utf8))
    #expect(decoded.accessToken == "a2" && decoded.refreshToken == nil && decoded.expiresIn == nil)
    #expect(throws: OAuthError.self) { try TokenSet(decoding: Data("{}".utf8)) }
}

@Test func expiryUsesSkew() {
    let t = TokenSet(accessToken: "a", expiresIn: 100, obtainedAt: Date(timeIntervalSince1970: 0))
    #expect(!t.isExpired(skew: 10, now: Date(timeIntervalSince1970: 80)))
    #expect(t.isExpired(skew: 10, now: Date(timeIntervalSince1970: 90)))
    #expect(!TokenSet(accessToken: "a").isExpired())
}

@Test func secretsStoreTokenStorePersistsTheRecord() throws {
    let secrets = try SecretsStore(namespace: "t.\(UUID().uuidString)", backend: MemorySecretsBackend())
    let store = SecretsTokenStore(store: secrets, key: "oauth")
    #expect(try store.load() == nil)
    let tokens = TokenSet(accessToken: "a", refreshToken: "r", obtainedAt: Date(timeIntervalSince1970: 9))
    try store.save(tokens)
    #expect(try store.load() == tokens)
    try store.clear()
    #expect(try store.load() == nil)
}

// MARK: Session

@Test func sessionRefreshesOnceForConcurrentCallersAndPersists() async throws {
    let fake = FakeTransport([(200, #"{"access_token":"fresh","expires_in":3600,"refresh_token":"r2"}"#)])
    let store = MemoryTokenStore(TokenSet(accessToken: "stale", expiresIn: 10, refreshToken: "r1", obtainedAt: Date(timeIntervalSince1970: 0)))
    let session = OAuthSession(client: try client(fake), store: store)
    async let a = session.validAccessToken()
    async let b = session.validAccessToken()
    let (x, y) = try await (a, b)
    #expect(x == "fresh" && y == "fresh")
    #expect(fake.requests.count == 1)
    #expect(try store.load()?.refreshToken == "r2")
}

@Test func sessionClearsStoreWhenRefreshTokenIsRejected() async throws {
    let fake = FakeTransport([(400, #"{"error":"invalid_grant"}"#)])
    let store = MemoryTokenStore(TokenSet(accessToken: "stale", expiresIn: 10, refreshToken: "r1", obtainedAt: Date(timeIntervalSince1970: 0)))
    let session = OAuthSession(client: try client(fake), store: store)
    await #expect(throws: OAuthError.needsReauth) { _ = try await session.validAccessToken() }
    #expect(try store.load() == nil)
    await #expect(throws: OAuthError.needsReauth) { _ = try await session.validAccessToken() }
}

// MARK: Interactive sign-in

private struct FakeRunner: WebAuthRunning {
    var respond: @Sendable (URL) throws -> URL
    func run(url: URL, callbackScheme: String, prefersEphemeralSession: Bool) async throws -> URL {
        #expect(callbackScheme == "myapp")
        return try respond(url)
    }
}

@Test func signInRunsTheFullFlowThroughTheRunner() async throws {
    let fake = FakeTransport([(200, #"{"access_token":"at","refresh_token":"rt"}"#)])
    let runner = FakeRunner { url in
        let state = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems!.first { $0.name == "state" }!.value!
        return URL(string: "myapp://oauth/callback?code=c1&state=\(state)")!
    }
    let tokens = try await OAuthSignIn(client: try client(fake), runner: runner).signIn()
    #expect(tokens.accessToken == "at")
    #expect(fake.forms.first?["code"] == "c1")
}

@Test func signInRefusesWebRedirectAndPropagatesCancel() async throws {
    var web = config()
    web.redirectURI = URL(string: "https://app.example.com/cb")!
    let fake = FakeTransport([(200, "{}")])
    await #expect(throws: OAuthError.self) {
        _ = try await OAuthSignIn(client: try client(fake, config: web), runner: FakeRunner { $0 }).signIn()
    }
    let cancelling = FakeRunner { _ in throw OAuthError.cancelled }
    await #expect(throws: OAuthError.cancelled) { _ = try await OAuthSignIn(client: try client(fake), runner: cancelling).signIn() }
    #expect(fake.requests.isEmpty)
}
