import Foundation
import RightKitHTTP

// OAuth 2.0 device authorization grant (RFC 8628) for native public clients: request a device and
// user code, show the user the verification page, then poll the token endpoint. Nothing is
// persisted here; the caller keeps the credential (for example in a Keychain entry, or by handing
// `DeviceCredential.tokens` to `SecretsTokenStore`, whose record has no `refreshExpiresAt`).
//
// `device_code`, `user_code` and every token are secrets: they never appear in a thrown error,
// a `description`, or a log line.

/// Endpoints and client parameters for a device authorization grant. Endpoints are provided by the
/// caller; ``github(clientID:scopes:)`` is the GitHub preset.
public struct DeviceCodeConfig: Sendable {
    public var deviceAuthorizationURL: URL
    public var tokenURL: URL
    public var clientID: String
    public var scopes: [String]

    public init(deviceAuthorizationURL: URL, tokenURL: URL, clientID: String, scopes: [String] = []) {
        self.deviceAuthorizationURL = deviceAuthorizationURL
        self.tokenURL = tokenURL
        self.clientID = clientID
        self.scopes = scopes
    }

    /// GitHub (App) device flow: `https://github.com/login/device/code` and
    /// `https://github.com/login/oauth/access_token`.
    public static func github(clientID: String, scopes: [String] = []) -> DeviceCodeConfig {
        DeviceCodeConfig(
            deviceAuthorizationURL: URL(string: "https://github.com/login/device/code")!,
            tokenURL: URL(string: "https://github.com/login/oauth/access_token")!,
            clientID: clientID, scopes: scopes
        )
    }

    /// The client id must be non-empty after trimming and must not be an unexpanded build variable
    /// (`$(NAME)`); both endpoints must be HTTPS (loopback HTTP allowed).
    public func validate() throws {
        let id = clientID.trimmingCharacters(in: .whitespacesAndNewlines)
        if id.isEmpty || id.contains("$(") { throw OAuthError.invalidConfig("client_id is empty") }
        guard HTTPClient.isPermitted(deviceAuthorizationURL, allowInsecureLoopback: true) else {
            throw OAuthError.invalidConfig("device_authorization_url must use HTTPS")
        }
        guard HTTPClient.isPermitted(tokenURL, allowInsecureLoopback: true) else {
            throw OAuthError.invalidConfig("token_url must use HTTPS")
        }
    }
}

/// The provider's answer to a device authorization request. `description` redacts both codes.
public struct DeviceAuthorization: Sendable, Equatable, CustomStringConvertible {
    public let deviceCode: String
    /// The short code the user types (or pastes) on the verification page.
    public let userCode: String
    public let verificationURI: URL
    public let verificationURIComplete: URL?
    /// Seconds from ``obtainedAt`` until the device code expires.
    public let expiresIn: Int
    /// Minimum seconds between polls.
    public let interval: Int
    public let obtainedAt: Date

    public var description: String {
        "DeviceAuthorization(deviceCode: [redacted], userCode: [redacted], verificationURI: \(verificationURI.host ?? ""), expiresIn: \(expiresIn), interval: \(interval))"
    }
}

/// A completed device sign-in. `description` redacts the tokens.
public struct DeviceCredential: Sendable, Equatable, CustomStringConvertible {
    public let tokens: TokenSet
    /// Access token expiry (`now + expires_in`), when the provider reports one.
    public let expiresAt: Date?
    /// Refresh token expiry (`now + refresh_token_expires_in`), when the provider reports one.
    public let refreshExpiresAt: Date?

    public var description: String {
        "DeviceCredential(tokens: \(tokens), expiresAt: \(expiresAt.map { "\($0)" } ?? "none"), refreshExpiresAt: \(refreshExpiresAt.map { "\($0)" } ?? "none"))"
    }
}

/// Shows the user where to enter the code. ``DeviceCodeClient/signIn(presenter:)`` calls `present`
/// once, and `dismiss` exactly once after `present` succeeded, whether the sign-in succeeded,
/// failed or was cancelled.
public protocol DeviceCodePresenting: Sendable {
    /// Shows the verification page. Throws ``OAuthError/invalidConfig(_:)`` with
    /// "could not open the sign-in page" when it cannot be shown.
    func present(_ authorization: DeviceAuthorization) async throws
    func dismiss(_ authorization: DeviceAuthorization) async
}

/// Requests a device authorization, polls for the token, and refreshes it.
public struct DeviceCodeClient: Sendable {
    public let config: DeviceCodeConfig
    public let http: HTTPClient
    private let now: @Sendable () -> Date
    private let sleep: @Sendable (TimeInterval) async throws -> Void

    private static let deviceCodeGrantType = "urn:ietf:params:oauth:grant-type:device_code"
    /// RFC 8628 section 3.2: the interval when the provider omits it.
    private static let defaultInterval = 5
    /// RFC 8628 section 3.5: added to the interval on every `slow_down`.
    private static let slowDownStep = 5

    public init(
        config: DeviceCodeConfig, http: HTTPClient = HTTPClient(),
        now: @escaping @Sendable () -> Date = { Date() },
        sleep: @escaping @Sendable (TimeInterval) async throws -> Void = { try await Task.sleep(nanoseconds: UInt64(max(0, $0) * 1_000_000_000)) }
    ) throws {
        try config.validate()
        self.config = config
        self.http = http
        self.now = now
        self.sleep = sleep
    }

    private var clientID: String { config.clientID.trimmingCharacters(in: .whitespacesAndNewlines) }

    // MARK: Device authorization

    /// POSTs `client_id` (and `scope` when scopes are set) to the device authorization endpoint.
    public func requestAuthorization() async throws -> DeviceAuthorization {
        var fields: [(String, String)] = [("client_id", clientID)]
        if !config.scopes.isEmpty { fields.append(("scope", config.scopes.joined(separator: " "))) }
        let response = try await post(config.deviceAuthorizationURL, fields, secrets: [])
        if let object = Self.object(response), let error = object["error"] as? String {
            if error == "device_flow_disabled" { throw OAuthError.deviceFlowDisabled }
            try Self.throwProviderError(object, status: response.status, secrets: [])
        }
        guard (200..<300).contains(response.status) else {
            throw OAuthError.provider(status: response.status, error: "http_error", description: nil)
        }
        let invalid = OAuthError.provider(
            status: 200, error: "invalid_device_authorization",
            description: "provider returned an invalid device authorization"
        )
        guard let object = Self.object(response),
              let deviceCode = object["device_code"] as? String, !deviceCode.isEmpty,
              let userCode = object["user_code"] as? String, !userCode.isEmpty,
              let uriText = object["verification_uri"] as? String, let uri = URL(string: uriText), !uriText.isEmpty,
              let expiresIn = Self.integer(object["expires_in"]), expiresIn > 0
        else { throw invalid }
        var interval = Self.defaultInterval
        if object["interval"] != nil {
            guard let given = Self.integer(object["interval"]) else { throw invalid }
            interval = given
        }
        guard interval > 0 else { throw invalid }
        let complete = (object["verification_uri_complete"] as? String).flatMap { $0.isEmpty ? nil : URL(string: $0) }
        return DeviceAuthorization(
            deviceCode: deviceCode, userCode: userCode, verificationURI: uri, verificationURIComplete: complete,
            expiresIn: expiresIn, interval: interval, obtainedAt: now()
        )
    }

    // MARK: Polling

    /// Polls the token endpoint (RFC 8628 section 3.5): waits one interval before every request,
    /// keeps polling on `authorization_pending`, adds 5 seconds to the interval for good on
    /// `slow_down`, and stops on expiry, denial or any other error. Cancellation is
    /// ``OAuthError/cancelled``, never a provider error.
    public func pollForToken(_ authorization: DeviceAuthorization) async throws -> DeviceCredential {
        let deadline = authorization.obtainedAt.addingTimeInterval(TimeInterval(authorization.expiresIn))
        let secrets = [authorization.deviceCode, authorization.userCode]
        var interval = authorization.interval
        let fields: [(String, String)] = [
            ("grant_type", Self.deviceCodeGrantType),
            ("device_code", authorization.deviceCode),
            ("client_id", clientID),
        ]
        while now() < deadline {
            try await pause(TimeInterval(interval))
            if now() >= deadline { break }
            if Task.isCancelled { throw OAuthError.cancelled }
            let response = try await post(config.tokenURL, fields, secrets: secrets)
            switch try outcome(response, secrets: secrets, keepRefresh: nil, polling: true) {
            case let .credential(credential): return credential
            case .pending: continue
            case .slowDown: interval += Self.slowDownStep
            }
        }
        throw OAuthError.deviceCodeExpired
    }

    /// Requests the authorization, shows it through `presenter`, and polls until the user decides.
    /// `presenter.dismiss` runs exactly once after `present` succeeded, on success, failure or
    /// cancellation; when `present` throws, nothing is dismissed and nothing is polled.
    public func signIn(presenter: any DeviceCodePresenting) async throws -> DeviceCredential {
        let authorization = try await requestAuthorization()
        try await presenter.present(authorization)
        do {
            let credential = try await pollForToken(authorization)
            await presenter.dismiss(authorization)
            return credential
        } catch {
            await presenter.dismiss(authorization)
            throw error
        }
    }

    // MARK: Refresh

    /// Exchanges a refresh token. An empty token, or the provider's `invalid_grant` /
    /// `bad_refresh_token`, is ``OAuthError/needsReauth``. A response without a new refresh token
    /// keeps the old one.
    public func refresh(refreshToken: String) async throws -> DeviceCredential {
        guard !refreshToken.isEmpty else { throw OAuthError.needsReauth }
        let fields: [(String, String)] = [
            ("client_id", clientID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refreshToken),
        ]
        let secrets = [refreshToken]
        let response = try await post(config.tokenURL, fields, secrets: secrets)
        switch try outcome(response, secrets: secrets, keepRefresh: refreshToken, polling: false) {
        case let .credential(credential): return credential
        case .pending, .slowDown: throw OAuthError.callback("token response omitted access_token")
        }
    }

    // MARK: Internals

    private enum Outcome { case credential(DeviceCredential), pending, slowDown }

    private func outcome(_ response: HTTPResponse, secrets: [String], keepRefresh: String?, polling: Bool) throws -> Outcome {
        let object = Self.object(response)
        if let object, let error = object["error"] as? String {
            switch error {
            case "authorization_pending" where polling: return .pending
            case "slow_down" where polling: return .slowDown
            case "expired_token": throw OAuthError.deviceCodeExpired
            case "access_denied": throw OAuthError.accessDenied
            case "device_flow_disabled": throw OAuthError.deviceFlowDisabled
            case "invalid_grant" where !polling: throw OAuthError.needsReauth
            case "bad_refresh_token" where !polling: throw OAuthError.needsReauth
            default: try Self.throwProviderError(object, status: response.status, secrets: secrets)
            }
        }
        guard (200..<300).contains(response.status) else {
            throw OAuthError.provider(status: response.status, error: "http_error", description: nil)
        }
        guard let object, let access = object["access_token"] as? String, !access.isEmpty else {
            throw OAuthError.callback("token response omitted access_token")
        }
        func string(_ key: String) -> String? { (object[key] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        let obtained = now()
        let expires = Self.seconds(object["expires_in"])
        let refreshExpires = Self.seconds(object["refresh_token_expires_in"])
        let tokens = TokenSet(
            accessToken: access, tokenType: (object["token_type"] as? String) ?? "Bearer", expiresIn: expires,
            refreshToken: string("refresh_token") ?? keepRefresh, scope: object["scope"] as? String,
            idToken: string("id_token"), obtainedAt: obtained
        )
        return .credential(DeviceCredential(
            tokens: tokens,
            expiresAt: expires.map { obtained.addingTimeInterval($0) },
            refreshExpiresAt: refreshExpires.map { obtained.addingTimeInterval($0) }
        ))
    }

    /// Waits one interval. A cancelled sleep is ``OAuthError/cancelled``.
    private func pause(_ seconds: TimeInterval) async throws {
        do { try await sleep(seconds) } catch is CancellationError {
            throw OAuthError.cancelled
        } catch {
            if Task.isCancelled { throw OAuthError.cancelled }
            throw error
        }
        if Task.isCancelled { throw OAuthError.cancelled }
    }

    private func post(_ url: URL, _ fields: [(String, String)], secrets: [String]) async throws -> HTTPResponse {
        // GitHub answers form-encoded unless asked for JSON; RFC 8628 errors arrive in 4xx bodies.
        var request = HTTPRequest.form(.post, url, fields: fields, headers: ["Accept": "application/json"])
        request.acceptedStatuses = Set(400..<500)
        do { return try await http.send(request) } catch let error as HTTPError {
            if error == .cancelled { throw OAuthError.cancelled }
            throw OAuthError.http(.init(description: OAuthClient.scrub("\(error)", secrets: secrets)))
        } catch is CancellationError {
            throw OAuthError.cancelled
        }
    }

    private static func throwProviderError(_ object: [String: Any], status: Int, secrets: [String]) throws {
        guard let error = object["error"] as? String else { return }
        let description = (object["error_description"] as? String).map { OAuthClient.scrub($0, secrets: secrets) }
        throw OAuthError.provider(status: status, error: OAuthClient.scrub(error, secrets: secrets), description: description)
    }

    private static func object(_ response: HTTPResponse) -> [String: Any]? {
        try? JSONSerialization.jsonObject(with: response.data) as? [String: Any]
    }

    private static func seconds(_ value: Any?) -> TimeInterval? {
        (value as? NSNumber)?.doubleValue ?? (value as? String).flatMap(TimeInterval.init)
    }

    private static func integer(_ value: Any?) -> Int? {
        if let number = value as? NSNumber { return number.intValue }
        if let text = value as? String { return Int(text) }
        return nil
    }
}
