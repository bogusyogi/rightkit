import Foundation

// Continuity-bridge client, lifted from ScrapeRight's `ProBridge`/`ProBridgeClient` and CodeRight's
// identical copy (rightkit#701, #604, #762).
//
// A bridge token is NOT an entitlement. It only says "this Apple Account owns a valid Pro source and
// may request activation for a device". Nothing here grants Pro: callers must pass the lease from an
// exchange or activation through ``LicenseActivationClient/license(fromLease:activationToken:deviceId:publicKeyBase64url:key:activatedAt:allowedScopes:)``,
// which verifies the Ed25519 signature and scope, before persisting anything.
//
// Wire format: the deployed device-lease contract (camelCase JSON string-to-string bodies, no bearer
// header). It is deliberately not the `rightkit-license` Rust `subject` module's bearer-authenticated
// entitlement-object flow, whose from-storekit / from-activation bodies are marked "assumed" there.

/// Why a bridge call did not produce a device lease. Each case maps to non-alarming user copy.
public enum ContinuityBridgeFailure: Error, Equatable, Sendable, LocalizedError {
    /// There is no bridge token to exchange. Raised by an app's resolver when no source published one;
    /// the client itself never throws it.
    case noBridgeAvailable
    /// Reachable backend, but the Pro source is no longer valid (refunded, revoked, expired).
    case sourceInactive(String)
    /// All device seats of the license are taken.
    case seatLimitReached
    /// Offline, timed out, or a non-HTTP response. Existing local entitlement is untouched.
    case unreachable(String)
    /// Anything else the backend reported, or a 2xx body that is not usable.
    case rejected(String)

    public var errorDescription: String? {
        switch self {
        case .noBridgeAvailable: return "No purchase from another device is available to restore."
        case let .sourceInactive(text): return text.isEmpty ? "The Pro purchase is no longer active." : text
        case .seatLimitReached: return "All device seats for this license are in use."
        case let .unreachable(text): return text.isEmpty ? "The license service could not be reached." : text
        case let .rejected(text): return text.isEmpty ? "The license service rejected the request." : text
        }
    }
}

/// Response to `bridge/from-activation` and `bridge/from-storekit`. `from-storekit` also returns the
/// canonical activation for the purchasing device in `activation`.
public struct ContinuityBridgeIssue: Decodable, Equatable, Sendable {
    public var ok: Bool?
    public var status: String?
    public var message: String?
    public var bridgeToken: String?
    /// Returned only after server verification of full/upgrade StoreKit proof.
    public var licenseKey: String?
    public var activation: LicenseServerResponse?

    public init(
        ok: Bool? = nil, status: String? = nil, message: String? = nil, bridgeToken: String? = nil,
        licenseKey: String? = nil, activation: LicenseServerResponse? = nil
    ) {
        self.ok = ok; self.status = status; self.message = message; self.bridgeToken = bridgeToken
        self.licenseKey = licenseKey; self.activation = activation
    }
}

/// Response to `bridge/exchange`: this device's renewable lease. The lease is unverified transport data:
/// pass it through ``LicenseActivationClient/license(fromLease:activationToken:deviceId:publicKeyBase64url:key:activatedAt:allowedScopes:)``
/// before persisting anything.
public struct ContinuityBridgeExchange: Decodable, Equatable, Sendable {
    public var ok: Bool?
    public var status: String?
    public var message: String?
    public var activationToken: String?
    public var lease: LicenseLeaseEnvelope?

    public init(
        ok: Bool? = nil, status: String? = nil, message: String? = nil,
        activationToken: String? = nil, lease: LicenseLeaseEnvelope? = nil
    ) {
        self.ok = ok; self.status = status; self.message = message
        self.activationToken = activationToken; self.lease = lease
    }
}

/// The send seam of ``ContinuityBridgeClient``: a fake can be injected without a `URLProtocol`.
/// `URLSession` conforms.
public protocol ContinuityBridgeTransport: Sendable {
    func send(_ request: URLRequest) async throws -> (Data, URLResponse)
}

extension URLSession: ContinuityBridgeTransport {
    public func send(_ request: URLRequest) async throws -> (Data, URLResponse) {
        try await data(for: request)
    }
}

/// Pure payload builders for the three bridge endpoints. Bodies are flat string maps.
public enum ContinuityBridgePayload {
    public static func fromActivation(app: String, activationToken: String, deviceId: String, version: String) -> [String: String] {
        ["app": app, "activationToken": activationToken, "deviceId": deviceId, "version": version]
    }

    /// `signedTransaction` is the Apple-signed JWS handed over verbatim: the server, not the client,
    /// decides the purchase is real. `signedMobileTransaction` is the optional second proof slot.
    public static func fromStoreKit(
        app: String, signedTransaction: String, deviceId: String, platform: String, publishBridge: Bool,
        signedMobileTransaction: String? = nil, version: String
    ) -> [String: String] {
        var payload = [
            "app": app, "signedTransaction": signedTransaction, "deviceId": deviceId,
            "platform": platform, "publishBridge": publishBridge ? "true" : "false", "version": version,
        ]
        if let signedMobileTransaction { payload["signedMobileTransaction"] = signedMobileTransaction }
        return payload
    }

    public static func exchange(app: String, bridgeToken: String, deviceId: String, platform: String, version: String) -> [String: String] {
        ["app": app, "bridgeToken": bridgeToken, "deviceId": deviceId, "platform": platform, "version": version]
    }
}

/// HTTP client for `bridge/from-activation`, `bridge/from-storekit` and `bridge/exchange`.
/// Transport is the injected ``ContinuityBridgeTransport`` (a `URLSession` by default); failures are
/// typed as ``ContinuityBridgeFailure``. Bodies are POST JSON with string values only, 18 s timeout.
public struct ContinuityBridgeClient: Sendable {
    public let app: String
    /// The licenses API root, e.g. `https://scraperight.cc/v1/licenses`. Endpoints are `bridge/<name>` beneath it.
    public let baseURL: URL
    public let appVersion: String
    public let transport: any ContinuityBridgeTransport
    /// The lease `platform` wire value sent with `from-storekit` and `exchange`; defaults to the host's.
    public let platform: @Sendable () async -> String
    public let timeout: TimeInterval

    /// - Parameters:
    ///   - session: the `URLSession` used when `transport` is nil.
    ///   - transport: replaces the network, e.g. a fake in tests.
    public init(
        app: String, baseURL: URL, appVersion: String = "0.0.0", session: URLSession = .shared,
        transport: (any ContinuityBridgeTransport)? = nil,
        platform: @escaping @Sendable () async -> String = { await LicenseActivationClient.platform() },
        timeout: TimeInterval = 18
    ) {
        self.app = app; self.baseURL = baseURL; self.appVersion = appVersion
        self.transport = transport ?? session; self.platform = platform; self.timeout = timeout
    }

    /// Convenience over ``LicenseClientConfiguration`` (same app, base URL, version and session).
    public init(configuration: LicenseClientConfiguration, transport: (any ContinuityBridgeTransport)? = nil) {
        self.init(app: configuration.app, baseURL: configuration.baseURL, appVersion: configuration.appVersion,
                  session: configuration.session, transport: transport)
    }

    public var fromActivationURL: URL { baseURL.appendingPathComponent("bridge/from-activation") }
    public var fromStoreKitURL: URL { baseURL.appendingPathComponent("bridge/from-storekit") }
    public var exchangeURL: URL { baseURL.appendingPathComponent("bridge/exchange") }

    /// Mint a bridge from an already-activated custom license on this device.
    public func publishFromActivation(activationToken: String, deviceId: String) async throws -> ContinuityBridgeIssue {
        try await post(
            url: fromActivationURL,
            payload: ContinuityBridgePayload.fromActivation(app: app, activationToken: activationToken, deviceId: deviceId, version: appVersion)
        )
    }

    /// Authorize this device from a verified StoreKit transaction, optionally minting a bridge.
    public func publishFromStoreKit(
        signedTransaction: String, deviceId: String, publishBridge: Bool, signedMobileTransaction: String? = nil
    ) async throws -> ContinuityBridgeIssue {
        try await post(
            url: fromStoreKitURL,
            payload: ContinuityBridgePayload.fromStoreKit(
                app: app, signedTransaction: signedTransaction, deviceId: deviceId, platform: await platform(),
                publishBridge: publishBridge, signedMobileTransaction: signedMobileTransaction, version: appVersion
            )
        )
    }

    /// Trade a bridge token for this device's own lease. Enforces source status and the seat limit.
    public func exchange(bridgeToken: String, deviceId: String) async throws -> ContinuityBridgeExchange {
        try await post(
            url: exchangeURL,
            payload: ContinuityBridgePayload.exchange(app: app, bridgeToken: bridgeToken, deviceId: deviceId, platform: await platform(), version: appVersion)
        )
    }

    // MARK: Transport

    private func post<T: Decodable & ContinuityBridgeStatusReporting>(url: URL, payload: [String: String]) async throws -> T {
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = timeout
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        do { request.httpBody = try JSONSerialization.data(withJSONObject: payload) }
        catch { throw ContinuityBridgeFailure.rejected("Could not encode the request.") }

        let data: Data
        let response: URLResponse
        do { (data, response) = try await transport.send(request) }
        catch { throw ContinuityBridgeFailure.unreachable(error.localizedDescription) }
        guard let http = response as? HTTPURLResponse else { throw ContinuityBridgeFailure.unreachable("No HTTP response.") }

        let decoded: T? = Self.decode(data)
        guard (200..<300).contains(http.statusCode) else {
            throw Self.classify(statusCode: http.statusCode, decoded: decoded, data: data)
        }
        guard let decoded, decoded.ok != false else {
            throw Self.classify(statusCode: http.statusCode, decoded: decoded, data: data)
        }
        return decoded
    }

    /// Two-pass decode: the backend has sent both snake_case and camelCase bodies.
    private static func decode<T: Decodable>(_ data: Data) -> T? {
        let snake = JSONDecoder(); snake.keyDecodingStrategy = .convertFromSnakeCase
        if let value = try? snake.decode(T.self, from: data) { return value }
        return try? JSONDecoder().decode(T.self, from: data)
    }

    static func classify(statusCode: Int, decoded: (any ContinuityBridgeStatusReporting)?, data: Data) -> ContinuityBridgeFailure {
        let reason = decoded?.status?.lowercased() ?? ""
        let text = decoded?.message ?? errorMessage(from: data)
        if statusCode == 409 || reason.contains("seat") || text.lowercased().contains("pool is full") {
            return .seatLimitReached
        }
        if statusCode == 422 || reason.contains("inactive") || reason.contains("revoked") || reason.contains("refund") || reason.contains("expired") {
            return .sourceInactive(text)
        }
        return .rejected(text)
    }

    private static func errorMessage(from data: Data) -> String {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return "" }
        return (object["message"] as? String) ?? (object["error"] as? String) ?? (object["detail"] as? String) ?? ""
    }
}

/// Common shape the generic `post` needs to classify a failed body of either response type.
protocol ContinuityBridgeStatusReporting {
    var ok: Bool? { get }
    var status: String? { get }
    var message: String? { get }
}

extension ContinuityBridgeIssue: ContinuityBridgeStatusReporting {}
extension ContinuityBridgeExchange: ContinuityBridgeStatusReporting {}
