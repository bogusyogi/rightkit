#if os(iOS)
// RIGHTKIT-SERIOUS: RightKitSwift 0.8.1 LicenseClient posts only activate, refresh and deactivate and leaves the continuity-bridge exchange to the app (LicenseClient.swift:1071-1074, :544-588); deleting it strands paying users whose Pro is activated on another device (report I-1; docs/rightkit-exceptions/M12.md B5)
import Foundation
import RightKitLicense
import UIKit

/// Wire contract for the Pro continuity bridge. Ported from the app `Shared/ProBridge.swift`.
///
/// NOT DOCUMENTED for the app: `docs/architecture/distribution/license-api.md` lists only
/// `/licenses/activate`, `/licenses/refresh`, `/licenses/deactivate` and `/releases/latest.json`
/// on `coderight.cc`. The three paths below are the app's paths (`/v1/licenses/bridge/...`)
/// reused verbatim against the app's host; the backend must implement them for `app: "coderight"`.
///
/// A bridge token is deliberately not an entitlement: it grants nothing until the backend
/// exchanges it for a device-bound signed lease.
enum ProBridge {
    static let app = "coderight"
    /// the app's licence host (license-api.md "Host"). Change here only.
    static let baseURL = URL(string: "https://coderight.cc")!

    static var fromActivationURL: URL { baseURL.appendingPathComponent("v1/licenses/bridge/from-activation") }
    static var fromStoreKitURL: URL { baseURL.appendingPathComponent("v1/licenses/bridge/from-storekit") }
    static var exchangeURL: URL { baseURL.appendingPathComponent("v1/licenses/bridge/exchange") }

    @MainActor static func platform() -> String {
        UIDevice.current.userInterfaceIdiom == .pad ? "ipados" : "ios"
    }

    static func appVersion(bundle: Bundle = .main) -> String {
        (bundle.infoDictionary?["CFBundleShortVersionString"] as? String)
            ?? (bundle.infoDictionary?["CFBundleVersion"] as? String)
            ?? "0.1.0"
    }

    static func fromActivationPayload(
        activationToken: String, deviceId: String, version: String = appVersion()
    ) -> [String: String] {
        ["app": app, "activationToken": activationToken, "deviceId": deviceId, "version": version]
    }

    @MainActor static func fromStoreKitPayload(
        signedTransaction: String,
        deviceId: String,
        publishBridge: Bool,
        signedMobileTransaction: String? = nil,
        version: String = appVersion()
    ) -> [String: String] {
        var payload = [
            "app": app,
            // Apple-signed JWS: the server, not the client, decides the purchase is real.
            "signedTransaction": signedTransaction,
            "deviceId": deviceId,
            "platform": platform(),
            "publishBridge": publishBridge ? "true" : "false",
            "version": version,
        ]
        if let signedMobileTransaction { payload["signedMobileTransaction"] = signedMobileTransaction }
        return payload
    }

    @MainActor static func exchangePayload(
        bridgeToken: String, deviceId: String, version: String = appVersion()
    ) -> [String: String] {
        ["app": app, "bridgeToken": bridgeToken, "deviceId": deviceId, "platform": platform(), "version": version]
    }
}

/// Response to `from-activation` and `from-storekit`.
struct ProBridgeIssueResponse: Decodable {
    var ok: Bool?
    var status: String?
    var message: String?
    var bridgeToken: String?
    var licenseKey: String?
    var activation: LicenseServerResponse?
}

/// Renewable device-lease response returned by `/bridge/exchange`.
struct ProBridgeExchangeResponse: Decodable {
    var ok: Bool?
    var status: String?
    var message: String?
    var activationToken: String?
    var lease: LicenseLeaseEnvelope?
}

/// Why a bridge call did not produce Pro. None of these should read like a user error.
enum ProBridgeFailure: Error, Equatable {
    case noBridgeAvailable
    case sourceInactive(String)
    case seatLimitReached
    case unreachable(String)
    case rejected(String)
}

protocol ProBridgeTransport: Sendable {
    func send(_ request: URLRequest) async throws -> (Data, URLResponse)
}

extension URLSession: ProBridgeTransport {
    func send(_ request: URLRequest) async throws -> (Data, URLResponse) {
        try await data(for: request)
    }
}

protocol ProBridgeStatusReporting {
    var ok: Bool? { get }
    var status: String? { get }
    var message: String? { get }
}

extension ProBridgeIssueResponse: ProBridgeStatusReporting {}
extension ProBridgeExchangeResponse: ProBridgeStatusReporting {}

/// Minimal POST client for the three bridge URLs. Throws `ProBridgeFailure` so the resolver can
/// tell offline apart from seat limit apart from a revoked source.
struct ProBridgeClient: Sendable {
    var transport: any ProBridgeTransport = URLSession.shared

    func publish(url: URL, payload: [String: String]) async throws -> ProBridgeIssueResponse {
        try await post(url: url, payload: payload)
    }

    @MainActor func exchange(bridgeToken: String, deviceId: String) async throws -> ProBridgeExchangeResponse {
        try await post(
            url: ProBridge.exchangeURL,
            payload: ProBridge.exchangePayload(bridgeToken: bridgeToken, deviceId: deviceId)
        )
    }

    private func post<T: Decodable & ProBridgeStatusReporting>(url: URL, payload: [String: String]) async throws -> T {
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 18
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: payload)

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await transport.send(request)
        } catch {
            throw ProBridgeFailure.unreachable(error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw ProBridgeFailure.unreachable("No HTTP response.")
        }
        let decoded: T? = Self.decode(data)
        guard (200..<300).contains(http.statusCode) else {
            throw Self.classify(statusCode: http.statusCode, decoded: decoded, data: data)
        }
        guard let decoded, decoded.ok != false else {
            throw Self.classify(statusCode: http.statusCode, decoded: decoded, data: data)
        }
        return decoded
    }

    /// The backend has been seen sending both snake_case and camelCase bodies.
    private static func decode<T: Decodable>(_ data: Data) -> T? {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        if let decoded = try? decoder.decode(T.self, from: data) { return decoded }
        return try? JSONDecoder().decode(T.self, from: data)
    }

    static func classify(statusCode: Int, decoded: ProBridgeStatusReporting?, data: Data) -> ProBridgeFailure {
        let reason = decoded?.status?.lowercased() ?? ""
        let text = decoded?.message ?? errorMessage(from: data)
        if statusCode == 409 || reason.contains("seat") || text.lowercased().contains("pool is full") {
            return .seatLimitReached
        }
        if statusCode == 422 || reason.contains("inactive") || reason.contains("revoked")
            || reason.contains("refund") || reason.contains("expired") {
            return .sourceInactive(text)
        }
        return .rejected(text)
    }

    private static func errorMessage(from data: Data) -> String {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return "" }
        return (object["message"] as? String) ?? (object["error"] as? String) ?? (object["detail"] as? String) ?? ""
    }
}
#endif
