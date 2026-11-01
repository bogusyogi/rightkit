import XCTest
@testable import RightKitGithub
@testable import RightKitMedia
@MainActor
final class CloudflareOAuthTests: XCTestCase {
    // MARK: - Client ID validation

    @MainActor
    func testClientIDRejectsEmptyAndUnexpandedBuildSetting() {
        XCTAssertThrowsError(try CloudflareOAuth.validClientID("  "))
        XCTAssertThrowsError(try CloudflareOAuth.validClientID("$(CODERIGHT_CLOUDFLARE_CLIENT_ID)"))
    }

    @MainActor
    func testClientIDTrimsValidPublicIdentifier() throws {
        XCTAssertEqual(try CloudflareOAuth.validClientID("  example-client-id  "), "example-client-id")
    }

    // MARK: - PKCE

    func testCodeVerifierIsWithinRfc7636LengthBounds() throws {
        let verifier = try CloudflareOAuth.makeCodeVerifier()
        XCTAssertGreaterThanOrEqual(verifier.count, 43)
        XCTAssertLessThanOrEqual(verifier.count, 128)
    }

    func testCodeVerifierUsesOnlyUrlSafeCharacters() throws {
        let verifier = try CloudflareOAuth.makeCodeVerifier()
        let allowed = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_")
        XCTAssertTrue(verifier.unicodeScalars.allSatisfy(allowed.contains))
    }

    func testCodeVerifierIsNotConstantAcrossCalls() throws {
        XCTAssertNotEqual(try CloudflareOAuth.makeCodeVerifier(), try CloudflareOAuth.makeCodeVerifier())
    }

    /// RFC 7636 Appendix B worked example: verifier
    /// "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk" must derive challenge
    /// "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".
    func testCodeChallengeMatchesRfc7636WorkedExample() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        let expectedChallenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        XCTAssertEqual(CloudflareOAuth.codeChallenge(forVerifier: verifier), expectedChallenge)
    }

    func testCodeChallengeIsBase64UrlEncodedWithoutPadding() throws {
        let challenge = CloudflareOAuth.codeChallenge(forVerifier: try CloudflareOAuth.makeCodeVerifier())
        XCTAssertFalse(challenge.contains("+"))
        XCTAssertFalse(challenge.contains("/"))
        XCTAssertFalse(challenge.contains("="))
    }

    // MARK: - Authorize URL construction

    func testAuthorizeURLIncludesRequiredPkceAndOAuthParameters() throws {
        let url = try CloudflareOAuth.buildAuthorizeURL(
            clientID: "client-123",
            codeChallenge: "challenge-abc",
            state: "state-xyz"
        )
        let items = try queryItems(of: url)

        XCTAssertEqual(url.scheme, "https")
        XCTAssertEqual(url.host, "dash.cloudflare.com")
        XCTAssertEqual(items["response_type"], "code")
        XCTAssertEqual(items["client_id"], "client-123")
        XCTAssertEqual(items["redirect_uri"], "coderight://oauth/cloudflare")
        XCTAssertEqual(items["code_challenge"], "challenge-abc")
        XCTAssertEqual(items["code_challenge_method"], "S256")
        XCTAssertEqual(items["state"], "state-xyz")
        XCTAssertNotNil(items["scope"])
    }

    func testAuthorizeURLStateVariesPerCall() throws {
        XCTAssertNotEqual(try CloudflareOAuth.makeState(), try CloudflareOAuth.makeState())
    }

    // MARK: - Callback parsing

    func testAuthorizationCodeExtractsCodeWhenStateMatches() throws {
        let callback = URL(string: "coderight://oauth/cloudflare?code=auth-code-1&state=expected-state")!
        XCTAssertEqual(
            try CloudflareOAuth.authorizationCode(from: callback, expectedState: "expected-state"),
            "auth-code-1"
        )
    }

    func testAuthorizationCodeRejectsMismatchedState() {
        let callback = URL(string: "coderight://oauth/cloudflare?code=auth-code-1&state=wrong-state")!
        XCTAssertThrowsError(
            try CloudflareOAuth.authorizationCode(from: callback, expectedState: "expected-state")
        )
    }

    func testAuthorizationCodeSurfacesProviderErrorDescription() {
        let callback = URL(
            string: "coderight://oauth/cloudflare?error=access_denied&error_description=User+cancelled&state=expected-state"
        )!
        XCTAssertThrowsError(
            try CloudflareOAuth.authorizationCode(from: callback, expectedState: "expected-state")
        ) { error in
            XCTAssertEqual((error as? LocalizedError)?.errorDescription, "User cancelled")
        }
    }

    func testAuthorizationCodeRejectsMissingCode() {
        let callback = URL(string: "coderight://oauth/cloudflare?state=expected-state")!
        XCTAssertThrowsError(
            try CloudflareOAuth.authorizationCode(from: callback, expectedState: "expected-state")
        )
    }

    // MARK: - Token / error decode paths via stubbed transport

    @MainActor
    func testAuthorizeDecodesSuccessfulTokenResponse() async throws {
        let transport = StubTransport(
            response: .init(
                statusCode: 200,
                json: """
                {
                    "access_token": "access-token-1",
                    "refresh_token": "refresh-token-1",
                    "expires_in": 3600,
                    "scope": "account:read workers:write"
                }
                """
            )
        )
        let oauth = CloudflareOAuth(transport: transport)
        let before = Date()

        let credential = try await oauth.refresh(
            .init(accessToken: "old", refreshToken: "old-refresh", expiresAt: Date(), scopes: [])
        )

        XCTAssertEqual(credential.accessToken, "access-token-1")
        XCTAssertEqual(credential.refreshToken, "refresh-token-1")
        XCTAssertEqual(credential.scopes, ["account:read", "workers:write"])
        XCTAssertGreaterThanOrEqual(credential.expiresAt.timeIntervalSince(before), 3600 - 1)
    }

    @MainActor
    func testRefreshSurfacesCloudflareErrorsArrayMessage() async {
        let transport = StubTransport(
            response: .init(
                statusCode: 400,
                json: """
                {
                    "success": false,
                    "errors": [{ "code": 1105, "message": "Invalid refresh token" }]
                }
                """
            )
        )
        let oauth = CloudflareOAuth(transport: transport)

        await XCTAssertThrowsErrorAsync(
            try await oauth.refresh(.init(accessToken: "old", refreshToken: "bad", expiresAt: Date(), scopes: []))
        ) { error in
            XCTAssertEqual((error as? LocalizedError)?.errorDescription, "Invalid refresh token")
        }
    }

    @MainActor
    func testRefreshFailsWhenTokenResponseOmitsAccessToken() async {
        let transport = StubTransport(
            response: .init(statusCode: 200, json: "{}")
        )
        let oauth = CloudflareOAuth(transport: transport)

        await XCTAssertThrowsErrorAsync(
            try await oauth.refresh(.init(accessToken: "old", refreshToken: "old-refresh", expiresAt: Date(), scopes: []))
        )
    }

    @MainActor
    func testRefreshFailsWhenTokenResponseOmitsRefreshToken() async {
        let transport = StubTransport(
            response: .init(
                statusCode: 200,
                json: """
                { "access_token": "access-token-1", "expires_in": 3600 }
                """
            )
        )
        let oauth = CloudflareOAuth(transport: transport)

        await XCTAssertThrowsErrorAsync(
            try await oauth.refresh(.init(accessToken: "old", refreshToken: "old-refresh", expiresAt: Date(), scopes: []))
        )
    }

    @MainActor
    func testRefreshFailsOnUnreadableResponseBody() async {
        let transport = StubTransport(response: .init(statusCode: 200, json: "not json"))
        let oauth = CloudflareOAuth(transport: transport)

        await XCTAssertThrowsErrorAsync(
            try await oauth.refresh(.init(accessToken: "old", refreshToken: "old-refresh", expiresAt: Date(), scopes: []))
        )
    }

    @MainActor
    func testRevokeNeverThrowsEvenWhenTransportFails() async {
        let transport = StubTransport(error: URLError(.notConnectedToInternet))
        let oauth = CloudflareOAuth(transport: transport)

        // `revoke` is `async`, not `async throws` — this line would fail to
        // compile if it could throw. Reaching the assertion is the test.
        await oauth.revoke(.init(accessToken: "a", refreshToken: "r", expiresAt: Date(), scopes: []))
        XCTAssertTrue(true)
    }

    // MARK: - Helpers

    private func queryItems(of url: URL) throws -> [String: String] {
        let components = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
        let items = components.queryItems ?? []
        return Dictionary(uniqueKeysWithValues: items.map { ($0.name, $0.value ?? "") })
    }
}

// MARK: - Stub transport

private struct StubTransport: CloudflareOAuthTransport {
    struct Response {
        let statusCode: Int
        let json: String
    }

    var response: Response?
    var error: Error?

    func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        if let error {
            throw error
        }
        guard let response, let url = request.url else {
            throw URLError(.badURL)
        }
        let httpResponse = HTTPURLResponse(
            url: url,
            statusCode: response.statusCode,
            httpVersion: nil,
            headerFields: ["Content-Type": "application/json"]
        )!
        return (Data(response.json.utf8), httpResponse)
    }
}

// MARK: - Async throw assertion

/// `XCTAssertThrowsError` has no async overload; this mirrors it for the
/// `async throws` calls above.
@MainActor
func XCTAssertThrowsErrorAsync<T>(
    _ expression: @autoclosure () async throws -> T,
    file: StaticString = #filePath,
    line: UInt = #line,
    _ errorHandler: (Error) -> Void = { _ in }
) async {
    do {
        _ = try await expression()
        XCTFail("Expected an error to be thrown", file: file, line: line)
    } catch {
        errorHandler(error)
    }
}
