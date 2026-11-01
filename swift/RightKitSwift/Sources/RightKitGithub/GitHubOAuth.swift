#if os(iOS)
// RIGHTKIT-GAP: OAuth PKCE sign-in stays local until RightKitSwift 0.9.0 (RightKitOAuth, held; bogusyogi/rightkit#627) is tagged; BLOCKED-UNPUB, see docs/rightkit-exceptions/outside-M-rows.md B058
import AuthenticationServices
import Foundation
import UIKit

struct GitHubOAuthCredential: Codable, Sendable {
    let accessToken: String
    let refreshToken: String?
    let expiresAt: Date?
    let refreshExpiresAt: Date?
}

/// GitHub App device authorization for a native public client. Device flow
/// keeps a client secret out of app while granting only repositories &
/// permissions selected during GitHub App installation.
@MainActor
final class GitHubOAuth: NSObject, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?

    func signIn(clientID: String) async throws -> GitHubOAuthCredential {
        let clientID = try Self.validClientID(clientID)
        let device = try await requestDeviceCode(clientID: clientID)

        // GitHub device page asks for this short-lived code. Copying it lets
        // user paste immediately inside secure browser sheet. The item is
        // device-local and expires with the code; it is also cleared when the
        // flow finishes, fails, or is cancelled (only if it still holds the code).
        let userCode = device.userCode
        UIPasteboard.general.setItems(
            [[UIPasteboard.typeAutomatic: userCode]],
            options: [
                .localOnly: true,
                .expirationDate: Date().addingTimeInterval(TimeInterval(device.expiresIn))
            ]
        )
        defer {
            if UIPasteboard.general.string == userCode {
                UIPasteboard.general.items = []
            }
        }
        let session = ASWebAuthenticationSession(
            url: device.verificationURI,
            callbackURLScheme: nil
        ) { _, _ in }
        session.presentationContextProvider = self
        session.prefersEphemeralWebBrowserSession = false
        self.session = session
        guard session.start() else {
            throw GitHubOAuthFailure("Could not open GitHub login.")
        }
        defer {
            session.cancel()
            self.session = nil
        }

        return try await pollForToken(clientID: clientID, device: device)
    }

    func refresh(clientID: String, refreshToken: String) async throws -> GitHubOAuthCredential {
        let clientID = try Self.validClientID(clientID)
        let response = try await tokenRequest([
            "client_id": clientID,
            "grant_type": "refresh_token",
            "refresh_token": refreshToken
        ])
        return try Self.credential(from: response)
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow) ?? ASPresentationAnchor()
    }

    private func requestDeviceCode(clientID: String) async throws -> DeviceCodeResponse {
        var request = Self.formRequest(
            url: URL(string: "https://github.com/login/device/code")!,
            values: ["client_id": clientID]
        )
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, response) = try await URLSession.shared.data(for: request)
        try Self.requireSuccess(response)
        let value = try JSONDecoder().decode(DeviceCodeResponse.self, from: data)
        guard value.expiresIn > 0, value.interval > 0 else {
            throw GitHubOAuthFailure("GitHub returned an invalid device authorization.")
        }
        return value
    }

    private func pollForToken(
        clientID: String,
        device: DeviceCodeResponse
    ) async throws -> GitHubOAuthCredential {
        let deadline = Date().addingTimeInterval(TimeInterval(device.expiresIn))
        var interval = device.interval

        while Date() < deadline {
            try await Task.sleep(for: .seconds(interval))
            let response = try await tokenRequest([
                "client_id": clientID,
                "device_code": device.deviceCode,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
            ])
            switch response.error {
            case nil:
                return try Self.credential(from: response)
            case "authorization_pending":
                continue
            case "slow_down":
                interval += 5
            case "expired_token":
                throw GitHubOAuthFailure("GitHub login code expired. Try again.")
            case "access_denied":
                throw GitHubOAuthFailure("GitHub login was cancelled.")
            case "device_flow_disabled":
                throw GitHubOAuthFailure("GitHub App device login is disabled.")
            default:
                throw GitHubOAuthFailure(response.errorDescription ?? response.error ?? "GitHub login failed.")
            }
        }
        throw GitHubOAuthFailure("GitHub login code expired. Try again.")
    }

    private func tokenRequest(_ values: [String: String]) async throws -> TokenResponse {
        var request = Self.formRequest(
            url: URL(string: "https://github.com/login/oauth/access_token")!,
            values: values
        )
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, response) = try await URLSession.shared.data(for: request)
        try Self.requireSuccess(response)
        return try JSONDecoder().decode(TokenResponse.self, from: data)
    }

    private static func credential(from response: TokenResponse) throws -> GitHubOAuthCredential {
        guard let accessToken = response.accessToken, !accessToken.isEmpty else {
            throw GitHubOAuthFailure(response.errorDescription ?? response.error ?? "GitHub returned no access token.")
        }
        let now = Date()
        return GitHubOAuthCredential(
            accessToken: accessToken,
            refreshToken: response.refreshToken,
            expiresAt: response.expiresIn.map { now.addingTimeInterval(TimeInterval($0)) },
            refreshExpiresAt: response.refreshTokenExpiresIn.map { now.addingTimeInterval(TimeInterval($0)) }
        )
    }

    static func validClientID(_ value: String) throws -> String {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, !value.contains("$(") else {
            throw GitHubOAuthFailure("GitHub App login is not configured in this build.")
        }
        return value
    }

    private static func formRequest(url: URL, values: [String: String]) -> URLRequest {
        var components = URLComponents()
        components.queryItems = values.map { URLQueryItem(name: $0.key, value: $0.value) }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.httpBody = components.percentEncodedQuery?.data(using: .utf8)
        return request
    }

    private static func requireSuccess(_ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw GitHubOAuthFailure("GitHub returned HTTP \(status).")
        }
    }
}

private struct DeviceCodeResponse: Decodable {
    let deviceCode: String
    let userCode: String
    let verificationURI: URL
    let expiresIn: Int
    let interval: Int

    enum CodingKeys: String, CodingKey {
        case deviceCode = "device_code"
        case userCode = "user_code"
        case verificationURI = "verification_uri"
        case expiresIn = "expires_in"
        case interval
    }
}

private struct TokenResponse: Decodable {
    let accessToken: String?
    let refreshToken: String?
    let expiresIn: Int?
    let refreshTokenExpiresIn: Int?
    let error: String?
    let errorDescription: String?

    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case expiresIn = "expires_in"
        case refreshTokenExpiresIn = "refresh_token_expires_in"
        case error
        case errorDescription = "error_description"
    }
}

private struct GitHubOAuthFailure: LocalizedError {
    let value: String
    init(_ value: String) { self.value = value }
    var errorDescription: String? { value }
}
#endif
