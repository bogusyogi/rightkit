import Foundation
import RightKitCredentials
import RightKitHTTP
import RightKitLicense

// Suite shop/account API client and device-local session store (issue 365): Sign in with Apple,
// session renewal, sign-out, account deletion, and linking a StoreKit purchase to the account.
// The Windows link code call (`requestWindowsLinkCode`) belongs to RightKitLicense.

/// Typed failure for every account call.
public enum AccountSessionError: Error, Equatable, Sendable {
    case notSignedIn
    /// The server answered something this client cannot read.
    case badResponse
    /// The server refused the request; the message is its own text (possibly empty).
    case serverRejected(String)
    /// 401 on a bearer call: the server forgot the token.
    case sessionExpired
    /// `DELETE /v1/shop/account` answered 409: an active subscription blocks deletion.
    case activeSubscription
    /// 422 from link-storekit: Apple refunded or revoked the purchase.
    case purchaseRevoked
    /// No answer from the server (offline, timeout, TLS).
    case network(String)
    /// The device-local session could not be saved (raw error text, no secrets).
    case storage(String)
}

/// What the Apple authorization sheet hands back, as the account API needs it.
public struct AppleIdentity: Sendable, Equatable {
    public var identityToken: String

    public init?(identityToken: Data?) {
        guard let identityToken, let text = String(data: identityToken, encoding: .utf8), !text.isEmpty else { return nil }
        self.identityToken = text
    }

    public init(identityToken: String) { self.identityToken = identityToken }
}

/// The lease and activation token that link-storekit returns. Persist them only after
/// `RightKitLicense` verifies the lease signature.
public struct LinkedPurchase: Sendable, Equatable {
    public var activationToken: String
    public var lease: LicenseLeaseEnvelope
}

/// Network half of the account session. Paths are the suite shop API's.
public struct AccountSessionClient: Sendable {
    public let baseURL: URL
    public let http: HTTPClient

    public init(baseURL: URL, http: HTTPClient = HTTPClient()) {
        self.baseURL = baseURL
        self.http = http
    }

    /// `macos` on a Mac, `ios` elsewhere. An iPad app passes `ipados`.
    public static var defaultPlatform: String {
        #if os(macOS)
        return "macos"
        #else
        return "ios"
        #endif
    }

    func url(_ path: String) -> URL { baseURL.appendingPathComponent(path) }

    private struct Customer: Decodable { var email: String? }
    private struct AuthResponse: Decodable { var token: String; var customer: Customer? }

    /// `POST /v1/shop/auth/apple` `{identityToken}` -> `{token, customer.email}`.
    public func signInWithApple(identityToken: String) async throws -> (token: String, email: String?) {
        let request = try HTTPRequest.json(.post, url("v1/shop/auth/apple"), body: ["identityToken": identityToken], timeout: 18)
        let response: AuthResponse = try await decode(request, bearer: false)
        guard !response.token.isEmpty else { throw AccountSessionError.badResponse }
        return (response.token, response.customer?.email)
    }

    /// `GET /v1/shop/auth/me` (Bearer). Returns the account email.
    public func renewSession(token: String) async throws -> String? {
        let request = HTTPRequest(method: .get, url: url("v1/shop/auth/me"), bearer: token, timeout: 18)
        let response: Customer = try await decode(request, bearer: true)
        return response.email
    }

    /// `POST /v1/shop/auth/logout`. Best effort: the local session is already gone when this runs.
    public func signOut(token: String) async {
        guard let request = try? HTTPRequest.json(.post, url("v1/shop/auth/logout"), body: [String: String](), bearer: token, timeout: 18) else { return }
        _ = try? await http.send(request)
    }

    /// `DELETE /v1/shop/account` (GDPR erasure). 409 is ``AccountSessionError/activeSubscription``.
    public func deleteAccount(token: String) async throws {
        let request = HTTPRequest(method: .delete, url: url("v1/shop/account"), bearer: token, timeout: 18)
        _ = try await send(request, bearer: true, statusMap: [409: .activeSubscription])
    }

    /// `POST /v1/shop/pro/link-storekit` (Bearer) `{appKey, signedTransactionInfo, deviceIdHash, platform}`.
    /// Apple's signed JWS is the proof; the server decides the purchase is real. 401 is
    /// ``AccountSessionError/sessionExpired``, 422 ``AccountSessionError/purchaseRevoked``.
    public func linkStoreKit(
        sessionToken: String, signedTransaction: String, deviceId: String, app: String,
        platform: String = AccountSessionClient.defaultPlatform,
    ) async throws -> LinkedPurchase {
        guard !sessionToken.isEmpty else { throw AccountSessionError.notSignedIn }
        let payload = [
            "appKey": app,
            "signedTransactionInfo": signedTransaction,
            "deviceIdHash": LicenseActivationClient.deviceIDHash(deviceId),
            "platform": platform,
        ]
        let request = try HTTPRequest.json(.post, url("v1/shop/pro/link-storekit"), body: payload, bearer: sessionToken, timeout: 18)
        let response: LicenseServerResponse = try await decode(request, bearer: true, statusMap: [422: .purchaseRevoked])
        guard let token = response.activationToken, !token.isEmpty, let lease = response.lease else {
            throw AccountSessionError.serverRejected(response.errorText ?? "The link response was incomplete.")
        }
        return LinkedPurchase(activationToken: token, lease: lease)
    }

    // MARK: Plumbing

    private func decode<T: Decodable>(
        _ request: HTTPRequest, bearer: Bool, statusMap: [Int: AccountSessionError] = [:],
    ) async throws -> T {
        let response = try await send(request, bearer: bearer, statusMap: statusMap)
        do { return try JSONDecoder().decode(T.self, from: response.data) } catch { throw AccountSessionError.badResponse }
    }

    private func send(
        _ request: HTTPRequest, bearer: Bool, statusMap: [Int: AccountSessionError] = [:],
    ) async throws -> HTTPResponse {
        do { return try await http.send(request) } catch let error as HTTPError {
            throw Self.map(error, bearer: bearer, statusMap: statusMap)
        }
    }

    static func map(_ error: HTTPError, bearer: Bool, statusMap: [Int: AccountSessionError] = [:]) -> AccountSessionError {
        switch error {
        case let .status(code, message):
            if code == 401, bearer { return .sessionExpired }
            return statusMap[code] ?? .serverRejected(message)
        case .cancelled: return .network("cancelled")
        case let .transport(text): return .network(text)
        case .timeout: return .network("timeout")
        case .insecureURL: return .network("insecure URL")
        default: return .badResponse
        }
    }
}
