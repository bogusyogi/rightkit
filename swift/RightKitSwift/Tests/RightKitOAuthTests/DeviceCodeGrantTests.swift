import Foundation
import RightKitHTTP
import Testing
@testable import RightKitOAuth

// In-process tests of the real device-code client against a fake transport at the HTTP seam. No
// real time passes: `sleep` and `now` are injected. The real-network e2e lane does not apply to a
// provider that needs a human to approve the code.

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
        return (Data(body.utf8), HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!)
    }

    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) { throw URLError(.unsupportedURL) }
}

/// A transport, a clock that only moves when the client sleeps, and a record of every sleep.
private final class Harness: @unchecked Sendable {
    let fake: FakeTransport
    private let lock = NSLock()
    private var date = Date(timeIntervalSince1970: 1_800_000_000)
    private var _sleeps: [TimeInterval] = []
    private var _requestsAtSleep: [Int] = []
    var sleepError: Error?

    init(_ replies: [(Int, String)]) { fake = FakeTransport(replies) }

    var current: Date { lock.withLock { date } }
    var sleeps: [TimeInterval] { lock.withLock { _sleeps } }
    var requestsAtSleep: [Int] { lock.withLock { _requestsAtSleep } }

    func client(_ config: DeviceCodeConfig = testConfig()) throws -> DeviceCodeClient {
        try DeviceCodeClient(
            config: config, http: HTTPClient(transport: fake),
            now: { self.current },
            sleep: { seconds in
                let count = self.fake.requests.count
                self.lock.withLock {
                    self._sleeps.append(seconds)
                    self._requestsAtSleep.append(count)
                }
                if let error = self.sleepError { throw error }
                self.lock.withLock { self.date = self.date.addingTimeInterval(seconds) }
            }
        )
    }

    func authorization(expiresIn: Int = 900, interval: Int = 5) -> DeviceAuthorization {
        DeviceAuthorization(
            deviceCode: deviceCode, userCode: userCode, verificationURI: URL(string: "https://github.com/login/device")!,
            verificationURIComplete: nil, expiresIn: expiresIn, interval: interval, obtainedAt: current
        )
    }
}

private final class FakePresenter: DeviceCodePresenting, @unchecked Sendable {
    private let lock = NSLock()
    private var _events: [String] = []
    private let failure: OAuthError?
    init(failure: OAuthError? = nil) { self.failure = failure }
    var events: [String] { lock.withLock { _events } }

    func present(_ authorization: DeviceAuthorization) async throws {
        if let failure { throw failure }
        lock.withLock { _events.append("present") }
    }

    func dismiss(_ authorization: DeviceAuthorization) async { lock.withLock { _events.append("dismiss") } }
}

private let deviceCode = "DEV-SECRET-123"
private let userCode = "WDJB-MJHT"

private func testConfig(scopes: [String] = []) -> DeviceCodeConfig {
    DeviceCodeConfig(
        deviceAuthorizationURL: URL(string: "https://idp.example.com/device/code")!,
        tokenURL: URL(string: "https://idp.example.com/token")!,
        clientID: "client-1", scopes: scopes
    )
}

private let authorizationBody = """
{"device_code":"\(deviceCode)","user_code":"\(userCode)","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}
"""
private let pending = (400, #"{"error":"authorization_pending"}"#)
private let slowDown = (400, #"{"error":"slow_down"}"#)
private let success = (200, #"{"access_token":"ghu_ACCESS","refresh_token":"ghr_REFRESH","expires_in":28800,"refresh_token_expires_in":15811200,"token_type":"bearer"}"#)

private func oauthError<T>(_ body: () async throws -> T) async -> OAuthError? {
    do { _ = try await body(); return nil } catch let error as OAuthError { return error } catch { return nil }
}

private func isHTTP(_ error: OAuthError?) -> Bool {
    if case .http = error { return true }
    return false
}

// MARK: Device authorization

@Test func deviceAuthorizationRequestBody() async throws {
    let plain = Harness([(200, authorizationBody)])
    _ = try await plain.client().requestAuthorization()
    let request = try #require(plain.fake.requests.first)
    #expect(request.httpMethod == "POST")
    #expect(request.url?.absoluteString == "https://idp.example.com/device/code")
    #expect(request.value(forHTTPHeaderField: "Content-Type") == "application/x-www-form-urlencoded")
    #expect(request.value(forHTTPHeaderField: "Accept") == "application/json")
    #expect(plain.fake.forms == [["client_id": "client-1"]])

    let scoped = Harness([(200, authorizationBody)])
    _ = try await scoped.client(testConfig(scopes: ["repo", "read:user"])).requestAuthorization()
    #expect(scoped.fake.forms == [["client_id": "client-1", "scope": "repo read:user"]])
}

@Test func deviceAuthorizationDecodes() async throws {
    let full = Harness([(200, """
    {"device_code":"\(deviceCode)","user_code":"\(userCode)","verification_uri":"https://github.com/login/device","verification_uri_complete":"https://github.com/login/device?user_code=\(userCode)","expires_in":600,"interval":10}
    """)])
    let a = try await full.client().requestAuthorization()
    #expect(a.deviceCode == deviceCode)
    #expect(a.userCode == userCode)
    #expect(a.verificationURI.absoluteString == "https://github.com/login/device")
    #expect(a.verificationURIComplete?.absoluteString == "https://github.com/login/device?user_code=\(userCode)")
    #expect(a.expiresIn == 600)
    #expect(a.interval == 10)
    #expect(a.obtainedAt == full.current)

    let bare = Harness([(200, #"{"device_code":"d","user_code":"u","verification_uri":"https://github.com/login/device","expires_in":900}"#)])
    let b = try await bare.client().requestAuthorization()
    #expect(b.verificationURIComplete == nil)
    #expect(b.interval == 5)
}

@Test func invalidAuthorizationRejected() async throws {
    let expected = OAuthError.provider(
        status: 200, error: "invalid_device_authorization", description: "provider returned an invalid device authorization"
    )
    let bodies = [
        #"{"device_code":"d","user_code":"u","verification_uri":"https://github.com/login/device","expires_in":0,"interval":5}"#,
        #"{"device_code":"d","user_code":"u","verification_uri":"https://github.com/login/device","expires_in":900,"interval":0}"#,
        #"{"device_code":"","user_code":"u","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
    ]
    for body in bodies {
        let harness = Harness([(200, body)])
        let client = try harness.client()
        #expect(await oauthError { try await client.requestAuthorization() } == expected)
    }
}

// MARK: Polling

@Test func pollSleepsBeforeFirstRequest() async throws {
    let harness = Harness([success])
    _ = try await harness.client().pollForToken(harness.authorization())
    #expect(harness.sleeps == [5])
    #expect(harness.requestsAtSleep == [0])
    let form = try #require(harness.fake.forms.first)
    #expect(form["grant_type"] == "urn:ietf:params:oauth:grant-type:device_code")
    #expect(form["device_code"] == deviceCode)
    #expect(form["client_id"] == "client-1")
    #expect(harness.fake.requests.first?.value(forHTTPHeaderField: "Accept") == "application/json")
}

@Test func authorizationPendingKeepsInterval() async throws {
    let harness = Harness([pending, pending, pending, success])
    _ = try await harness.client().pollForToken(harness.authorization())
    #expect(harness.sleeps == [5, 5, 5, 5])
    #expect(harness.fake.requests.count == 4)
}

@Test func slowDownAddsFiveSecondsPermanently() async throws {
    let harness = Harness([pending, slowDown, pending, success])
    _ = try await harness.client().pollForToken(harness.authorization())
    #expect(harness.sleeps == [5, 5, 10, 10])
}

@Test func expiredTokenThrowsDeviceCodeExpired() async throws {
    let harness = Harness([(400, #"{"error":"expired_token"}"#)])
    let client = try harness.client()
    let authorization = harness.authorization()
    #expect(await oauthError { try await client.pollForToken(authorization) } == .deviceCodeExpired)
}

@Test func accessDeniedThrowsAccessDenied() async throws {
    let harness = Harness([(400, #"{"error":"access_denied"}"#)])
    let client = try harness.client()
    let authorization = harness.authorization()
    #expect(await oauthError { try await client.pollForToken(authorization) } == .accessDenied)
}

@Test func deviceFlowDisabledThrows() async throws {
    let harness = Harness([(200, #"{"error":"device_flow_disabled"}"#)])
    let client = try harness.client()
    let authorization = harness.authorization()
    #expect(await oauthError { try await client.pollForToken(authorization) } == .deviceFlowDisabled)

    let atRequest = Harness([(400, #"{"error":"device_flow_disabled"}"#)])
    let requestClient = try atRequest.client()
    #expect(await oauthError { try await requestClient.requestAuthorization() } == .deviceFlowDisabled)
}

@Test func unknownErrorIsProviderWithDescription() async throws {
    let harness = Harness([(400, #"{"error":"incorrect_client_credentials","error_description":"The client_id is incorrect."}"#)])
    let client = try harness.client()
    let authorization = harness.authorization()
    #expect(
        await oauthError { try await client.pollForToken(authorization) }
            == .provider(status: 400, error: "incorrect_client_credentials", description: "The client_id is incorrect.")
    )
}

@Test func deadlineElapsedThrowsDeviceCodeExpired() async throws {
    let harness = Harness([pending])
    let client = try harness.client()
    let authorization = harness.authorization(expiresIn: 12, interval: 5)
    #expect(await oauthError { try await client.pollForToken(authorization) } == .deviceCodeExpired)
    #expect(harness.sleeps == [5, 5, 5])
    #expect(harness.fake.requests.count == 2)
}

@Test func successBuildsCredentialExpiries() async throws {
    let harness = Harness([success])
    let credential = try await harness.client().pollForToken(harness.authorization())
    let now = harness.current
    #expect(credential.tokens.accessToken == "ghu_ACCESS")
    #expect(credential.tokens.refreshToken == "ghr_REFRESH")
    #expect(credential.tokens.expiresIn == 28800)
    #expect(credential.tokens.obtainedAt == now)
    #expect(credential.expiresAt == now.addingTimeInterval(28800))
    #expect(credential.refreshExpiresAt == now.addingTimeInterval(15_811_200))

    let bare = Harness([(200, #"{"access_token":"ghu_ONLY"}"#)])
    let plain = try await bare.client().pollForToken(bare.authorization())
    #expect(plain.expiresAt == nil)
    #expect(plain.refreshExpiresAt == nil)
    #expect(plain.tokens.refreshToken == nil)

    let empty = Harness([(200, #"{"token_type":"bearer"}"#)])
    let emptyClient = try empty.client()
    let authorization = empty.authorization()
    #expect(await oauthError { try await emptyClient.pollForToken(authorization) } == .callback("token response omitted access_token"))
}

@Test func http400WithErrorBodyIsDecoded() async throws {
    let harness = Harness([pending, success])
    let credential = try await harness.client().pollForToken(harness.authorization())
    #expect(credential.tokens.accessToken == "ghu_ACCESS")
    #expect(harness.fake.requests.count == 2)
}

@Test func non2xxWithoutBodyIsHttpError() async throws {
    let server = Harness([(500, "")])
    let serverClient = try server.client()
    let serverAuthorization = server.authorization()
    #expect(isHTTP(await oauthError { try await serverClient.pollForToken(serverAuthorization) }))

    let missing = Harness([(404, "")])
    let missingClient = try missing.client()
    let missingAuthorization = missing.authorization()
    #expect(
        await oauthError { try await missingClient.pollForToken(missingAuthorization) }
            == .provider(status: 404, error: "http_error", description: nil)
    )
}

@Test func cancellationDuringSleepThrowsCancelled() async throws {
    let harness = Harness([success])
    harness.sleepError = CancellationError()
    let client = try harness.client()
    let authorization = harness.authorization()
    #expect(await oauthError { try await client.pollForToken(authorization) } == .cancelled)
    #expect(harness.fake.requests.isEmpty)

    // A real cancelled task, with the default-style sleep that honours cancellation.
    let fake = FakeTransport([success])
    let live = try DeviceCodeClient(
        config: testConfig(), http: HTTPClient(transport: fake),
        sleep: { _ in try await Task.sleep(nanoseconds: 60_000_000_000) }
    )
    let task = Task { try await live.pollForToken(authorization) }
    task.cancel()
    do { _ = try await task.value; Issue.record("expected cancellation") } catch {
        #expect(error as? OAuthError == .cancelled)
    }
    #expect(fake.requests.isEmpty)
}

// MARK: Refresh

@Test func refreshSendsGrantAndMapsBadRefreshToken() async throws {
    let harness = Harness([(200, #"{"access_token":"ghu_NEW","expires_in":28800}"#)])
    let client = try harness.client()
    let credential = try await client.refresh(refreshToken: "ghr_OLD")
    #expect(harness.fake.forms == [["client_id": "client-1", "grant_type": "refresh_token", "refresh_token": "ghr_OLD"]])
    #expect(harness.fake.requests.first?.value(forHTTPHeaderField: "Accept") == "application/json")
    #expect(credential.tokens.accessToken == "ghu_NEW")
    #expect(credential.tokens.refreshToken == "ghr_OLD")
    #expect(credential.expiresAt == harness.current.addingTimeInterval(28800))
    #expect(harness.sleeps.isEmpty)

    for body in [#"{"error":"bad_refresh_token"}"#, #"{"error":"invalid_grant"}"#] {
        let rejected = Harness([(400, body)])
        let rejectedClient = try rejected.client()
        #expect(await oauthError { try await rejectedClient.refresh(refreshToken: "ghr_OLD") } == .needsReauth)
    }

    let blank = Harness([success])
    let blankClient = try blank.client()
    #expect(await oauthError { try await blankClient.refresh(refreshToken: "") } == .needsReauth)
    #expect(blank.fake.requests.isEmpty)

    let other = Harness([(400, #"{"error":"unauthorized_client","error_description":"nope"}"#)])
    let otherClient = try other.client()
    #expect(
        await oauthError { try await otherClient.refresh(refreshToken: "ghr_OLD") }
            == .provider(status: 400, error: "unauthorized_client", description: "nope")
    )
}

// MARK: Configuration

@Test func blankOrBuildVariableClientIDRejected() throws {
    for id in ["", "  ", "$(GITHUB_CLIENT_ID)"] {
        #expect(throws: OAuthError.invalidConfig("client_id is empty")) {
            try DeviceCodeClient(config: .github(clientID: id))
        }
    }
    #expect(throws: Never.self) { try DeviceCodeClient(config: .github(clientID: " Iv1.abc \n")) }
}

@Test func httpEndpointRejectedExceptLoopback() throws {
    let insecureDevice = DeviceCodeConfig(
        deviceAuthorizationURL: URL(string: "http://idp.example.com/device")!,
        tokenURL: URL(string: "https://idp.example.com/token")!, clientID: "client-1"
    )
    #expect(throws: OAuthError.invalidConfig("device_authorization_url must use HTTPS")) { try insecureDevice.validate() }
    let insecureToken = DeviceCodeConfig(
        deviceAuthorizationURL: URL(string: "https://idp.example.com/device")!,
        tokenURL: URL(string: "http://idp.example.com/token")!, clientID: "client-1"
    )
    #expect(throws: OAuthError.invalidConfig("token_url must use HTTPS")) { try insecureToken.validate() }
    let loopback = DeviceCodeConfig(
        deviceAuthorizationURL: URL(string: "http://localhost:8080/device")!,
        tokenURL: URL(string: "http://127.0.0.1:8080/token")!, clientID: "client-1"
    )
    #expect(throws: Never.self) { try loopback.validate() }
}

@Test func githubPresetEndpoints() {
    let config = DeviceCodeConfig.github(clientID: "Iv1.abc")
    #expect(config.deviceAuthorizationURL.absoluteString == "https://github.com/login/device/code")
    #expect(config.tokenURL.absoluteString == "https://github.com/login/oauth/access_token")
    #expect(config.clientID == "Iv1.abc")
    #expect(config.scopes.isEmpty)
    #expect(DeviceCodeConfig.github(clientID: "Iv1.abc", scopes: ["repo"]).scopes == ["repo"])
}

// MARK: Secrets

@Test func secretsNeverInErrorsOrDescriptions() async throws {
    var texts: [String] = []
    func note(_ error: OAuthError?) { texts.append(error.map { "\($0) \(String(describing: $0)) \(String(reflecting: $0))" } ?? "") }

    let provider = Harness([(400, #"{"error":"weird","error_description":"bad \#(deviceCode) and \#(userCode)"}"#)])
    let providerClient = try provider.client()
    let providerAuthorization = provider.authorization()
    note(await oauthError { try await providerClient.pollForToken(providerAuthorization) })

    let server = Harness([(500, "oops \(deviceCode) \(userCode)")])
    let serverClient = try server.client()
    let serverAuthorization = server.authorization()
    note(await oauthError { try await serverClient.pollForToken(serverAuthorization) })

    let refresh = Harness([(400, #"{"error":"weird","error_description":"bad ghr_OLD"}"#)])
    let refreshClient = try refresh.client()
    note(await oauthError { try await refreshClient.refresh(refreshToken: "ghr_OLD") })

    let ok = Harness([success])
    let credential = try await ok.client().pollForToken(ok.authorization())
    texts.append("\(credential) \(credential.tokens) \(ok.authorization())")
    texts.append(String(reflecting: credential.tokens))

    for text in texts {
        for secret in [deviceCode, userCode, "ghr_OLD", "ghu_ACCESS", "ghr_REFRESH"] {
            #expect(!text.contains(secret), "leaked \(secret.prefix(4))... in output")
        }
    }
    #expect(texts.count == 5)
}

// MARK: Sign-in

@Test func signInCallsPresenterOnceAndDismissesOnSuccessFailureCancel() async throws {
    let done = Harness([(200, authorizationBody), success])
    let donePresenter = FakePresenter()
    let credential = try await done.client().signIn(presenter: donePresenter)
    #expect(credential.tokens.accessToken == "ghu_ACCESS")
    #expect(donePresenter.events == ["present", "dismiss"])

    let failed = Harness([(200, authorizationBody), (400, #"{"error":"expired_token"}"#)])
    let failedPresenter = FakePresenter()
    let failedClient = try failed.client()
    #expect(await oauthError { try await failedClient.signIn(presenter: failedPresenter) } == .deviceCodeExpired)
    #expect(failedPresenter.events == ["present", "dismiss"])

    let cancelled = Harness([(200, authorizationBody), success])
    cancelled.sleepError = CancellationError()
    let cancelledPresenter = FakePresenter()
    let cancelledClient = try cancelled.client()
    #expect(await oauthError { try await cancelledClient.signIn(presenter: cancelledPresenter) } == .cancelled)
    #expect(cancelledPresenter.events == ["present", "dismiss"])
}

@Test func presenterFailureStopsBeforePolling() async throws {
    let harness = Harness([(200, authorizationBody), success])
    let presenter = FakePresenter(failure: .invalidConfig("could not open the sign-in page"))
    let client = try harness.client()
    #expect(
        await oauthError { try await client.signIn(presenter: presenter) }
            == .invalidConfig("could not open the sign-in page")
    )
    #expect(presenter.events.isEmpty)
    #expect(harness.sleeps.isEmpty)
    #expect(harness.fake.requests.count == 1)
}
