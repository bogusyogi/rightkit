#if canImport(AuthenticationServices) && canImport(UIKit) && !os(watchOS)
import AuthenticationServices
import Foundation
import UIKit

/// Shows a device authorization in a system web session: copies the user code to the local
/// pasteboard (so the user can paste it on the verification page) and opens the page in
/// `ASWebAuthenticationSession`. `dismiss` cancels the session and clears the pasteboard, but only
/// while it still holds this code.
///
/// Needs a running app with a window; it cannot be exercised on a headless CI host, so it has no
/// unit test. The user code is never logged.
public final class SystemDeviceCodePresenter: NSObject, DeviceCodePresenting, ASWebAuthenticationPresentationContextProviding, @unchecked Sendable {
    private let anchor: @MainActor @Sendable () -> ASPresentationAnchor
    private let lock = NSLock()
    private var session: ASWebAuthenticationSession?

    public init(anchor: @escaping @MainActor @Sendable () -> ASPresentationAnchor = SystemWebAuthRunner.defaultAnchor) {
        self.anchor = anchor
    }

    public func present(_ authorization: DeviceAuthorization) async throws {
        try await start(authorization)
    }

    public func dismiss(_ authorization: DeviceAuthorization) async {
        await finish(authorization)
    }

    @MainActor
    private func start(_ authorization: DeviceAuthorization) throws {
        // The device page asks for this short-lived code. The item is device-local and expires with it.
        UIPasteboard.general.setItems(
            [[UIPasteboard.typeAutomatic: authorization.userCode]],
            options: [
                .localOnly: true,
                .expirationDate: Date().addingTimeInterval(TimeInterval(authorization.expiresIn)),
            ]
        )
        let session = ASWebAuthenticationSession(
            url: authorization.verificationURIComplete ?? authorization.verificationURI,
            callbackURLScheme: nil
        ) { _, _ in }
        session.presentationContextProvider = self
        session.prefersEphemeralWebBrowserSession = false
        lock.withLock { self.session = session }
        guard session.start() else {
            lock.withLock { self.session = nil }
            clearPasteboard(ifHolding: authorization.userCode)
            throw OAuthError.invalidConfig("could not open the sign-in page")
        }
    }

    @MainActor
    private func finish(_ authorization: DeviceAuthorization) {
        let session = lock.withLock { () -> ASWebAuthenticationSession? in
            defer { self.session = nil }
            return self.session
        }
        session?.cancel()
        clearPasteboard(ifHolding: authorization.userCode)
    }

    @MainActor
    private func clearPasteboard(ifHolding userCode: String) {
        if UIPasteboard.general.string == userCode { UIPasteboard.general.items = [] }
    }

    public func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated { anchor() }
    }
}
#endif
