import Foundation

/// Opens the provider's page, waits for the redirect, and returns the callback URL. The seam under
/// ``OAuthSignIn``: production uses ``SystemWebAuthRunner`` (ASWebAuthenticationSession); tests inject a fake.
public protocol WebAuthRunning: Sendable {
    func run(url: URL, callbackScheme: String, prefersEphemeralSession: Bool) async throws -> URL
}

/// Interactive sign-in: authorization request, system web session, code exchange.
///
/// The redirect URI must use a custom URL scheme the app registers; ASWebAuthenticationSession
/// reports it back by scheme.
public struct OAuthSignIn: Sendable {
    public let client: OAuthClient
    public let runner: any WebAuthRunning

    public init(client: OAuthClient, runner: any WebAuthRunning) {
        self.client = client
        self.runner = runner
    }

    /// Runs the whole flow and returns the tokens. The user closing the sheet is ``OAuthError/cancelled``.
    public func signIn(
        extra: [(name: String, value: String)] = [], prefersEphemeralSession: Bool = false,
    ) async throws -> TokenSet {
        guard let scheme = client.config.redirectURI.scheme?.lowercased(), scheme != "http", scheme != "https" else {
            throw OAuthError.invalidConfig("redirect_uri must use a custom URL scheme for the system web session")
        }
        let request = try client.authorizationRequest(extra: extra)
        let callback = try await runner.run(url: request.url, callbackScheme: scheme, prefersEphemeralSession: prefersEphemeralSession)
        let code = try client.authorizationCode(from: callback, for: request)
        return try await client.exchangeCode(request, code: code)
    }
}

#if canImport(AuthenticationServices) && !os(watchOS) // no presentation-anchor web auth on watchOS
import AuthenticationServices
#if canImport(UIKit)
import UIKit
#endif
#if canImport(AppKit)
import AppKit
#endif

/// ASWebAuthenticationSession runner. Needs a running app with a window; it cannot be exercised on
/// a headless CI host.
public struct SystemWebAuthRunner: WebAuthRunning {
    /// Supplies the window the sheet attaches to; default is the key window.
    public typealias AnchorProvider = @MainActor @Sendable () -> ASPresentationAnchor
    private let anchor: AnchorProvider

    public init(anchor: @escaping AnchorProvider = SystemWebAuthRunner.defaultAnchor) { self.anchor = anchor }

    @MainActor public static func defaultAnchor() -> ASPresentationAnchor {
        #if os(iOS)
        let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap(\.windows)
        return windows.first(where: \.isKeyWindow) ?? windows.first ?? ASPresentationAnchor()
        #elseif os(macOS)
        return NSApplication.shared.keyWindow ?? NSApplication.shared.mainWindow ?? ASPresentationAnchor()
        #else
        return ASPresentationAnchor()
        #endif
    }

    public func run(url: URL, callbackScheme: String, prefersEphemeralSession: Bool) async throws -> URL {
        let anchor = anchor
        let box = await MainActor.run { SessionBox(anchor: anchor) }
        return try await withTaskCancellationHandler {
            try await box.start(url: url, scheme: callbackScheme, ephemeral: prefersEphemeralSession)
        } onCancel: {
            Task { @MainActor in box.cancel() }
        }
    }

    @MainActor
    private final class SessionBox: NSObject, ASWebAuthenticationPresentationContextProviding {
        private let anchor: AnchorProvider
        private var session: ASWebAuthenticationSession?
        private var continuation: CheckedContinuation<URL, Error>?

        init(anchor: @escaping AnchorProvider) { self.anchor = anchor }

        func start(url: URL, scheme: String, ephemeral: Bool) async throws -> URL {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                self.continuation = continuation
                let session = ASWebAuthenticationSession(url: url, callbackURLScheme: scheme) { [weak self] callback, error in
                    Task { @MainActor in self?.finish(callback: callback, error: error) }
                }
                session.presentationContextProvider = self
                session.prefersEphemeralWebBrowserSession = ephemeral
                self.session = session
                if !session.start() { finish(callback: nil, error: OAuthError.callback("the system web session could not start")) }
            }
        }

        func cancel() {
            session?.cancel()
            finish(callback: nil, error: OAuthError.cancelled)
        }

        private func finish(callback: URL?, error: Error?) {
            guard let continuation else { return }
            self.continuation = nil
            session = nil
            if let callback {
                continuation.resume(returning: callback)
            } else if let error = error as? ASWebAuthenticationSessionError, error.code == .canceledLogin {
                continuation.resume(throwing: OAuthError.cancelled)
            } else if let error = error as? OAuthError {
                continuation.resume(throwing: error)
            } else {
                continuation.resume(throwing: OAuthError.callback("the system web session failed"))
            }
        }

        func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor { anchor() }
    }
}
#endif
