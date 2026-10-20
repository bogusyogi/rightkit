import Foundation
import RightKitCredentials

/// The one stored account session.
public struct StoredAccountSession: Codable, Equatable, Sendable {
    public var token: String
    public var email: String?
    public var issuedAtUnix: Int64

    public init(token: String, email: String?, issuedAtUnix: Int64) {
        self.token = token
        self.email = email
        self.issuedAtUnix = issuedAtUnix
    }
}

/// `{ signedIn, email, expiresAt }`: what an account screen reads.
public struct AccountSessionStatus: Equatable, Sendable {
    public var signedIn: Bool
    public var email: String?
    public var expiresAt: Int64?

    public init(signedIn: Bool, email: String?, expiresAt: Int64?) {
        self.signedIn = signedIn
        self.email = email
        self.expiresAt = expiresAt
    }

    public static let signedOut = AccountSessionStatus(signedIn: false, email: nil, expiresAt: nil)
}

public protocol AccountSessionKeyStoring: Sendable {
    func read() -> StoredAccountSession?
    func write(_ session: StoredAccountSession) throws
    func clear()
}

/// Device-local Keychain item for the session, on RightKitCredentials: `ThisDeviceOnly`, data-protection
/// keychain, explicitly not synchronizable. It must never go to an iCloud-synced store.
public struct AccountSessionKeychainStore: AccountSessionKeyStoring {
    public static let account = "session"
    private let store: SecretsStore
    private let key: String

    /// Use a `SecretsStore` built with the default (`afterFirstUnlockThisDeviceOnly`) accessibility.
    public init(store: SecretsStore, key: String = AccountSessionKeychainStore.account) {
        self.store = store
        self.key = key
    }

    /// Keychain service `service` (for example `app.coderight.ios.account-session`).
    public init(service: String, accessGroup: String? = nil) throws {
        self.init(store: try SecretsStore(namespace: service, accessGroup: accessGroup))
    }

    public func read() -> StoredAccountSession? {
        guard let data = try? store.get(key) else { return nil }
        return try? JSONDecoder().decode(StoredAccountSession.self, from: data)
    }

    public func write(_ session: StoredAccountSession) throws {
        do { try store.set(key, try JSONEncoder().encode(session)) } catch {
            throw AccountSessionError.storage(String(describing: type(of: error)))
        }
    }

    public func clear() { _ = try? store.delete(key) }
}

/// Session lifecycle over a client and a key store. UI-free: wrap it in your own observable model.
///
/// An account is optional. Free use and a locally verified Pro entitlement never read it, and
/// signing out clears this session and nothing else (not the device lease, StoreKit, or synced keys).
///
/// Every call suspends on the network, and the actor runs other calls meanwhile. So a sign-in or a
/// renewal that was already in flight when the user signed out (or the server expired the session,
/// or the account was deleted) never writes its session back: each such clearing advances a fence,
/// and a round trip that started before the fence moved drops its answer.
public actor AccountSessionStore {
    /// Mirrors the server's one-year session TTL; a display estimate, never authoritative.
    public static let sessionTTLSeconds: Int64 = 365 * 24 * 60 * 60

    private let client: AccountSessionClient
    private let keyStore: any AccountSessionKeyStoring
    private let now: @Sendable () -> Date
    private let onChange: (@Sendable (AccountSessionStatus) -> Void)?
    /// Advanced by every local clearing (sign-out, expiry, deletion). See the type documentation.
    private var clearings: UInt64 = 0

    /// - Parameter onChange: called, on this actor, after every local change of the session
    ///   (sign-in, renewal, sign-out, expiry, deletion) with the new status. An app turns it into its
    ///   own notification or observable update; RightKit names no notification.
    public init(
        client: AccountSessionClient, keyStore: any AccountSessionKeyStoring,
        now: @escaping @Sendable () -> Date = { Date() },
        onChange: (@Sendable (AccountSessionStatus) -> Void)? = nil,
    ) {
        self.client = client
        self.keyStore = keyStore
        self.now = now
        self.onChange = onChange
    }

    /// Local read only. Nothing that gates Free or Pro use may depend on the network.
    public func status() -> AccountSessionStatus { Self.status(of: keyStore.read()) }

    /// Account-screen refresh: renews an active server session. A transient failure keeps the locally
    /// known state; an expired session signs out locally. Failures are not reported: use
    /// ``renewChecked()`` to learn why a renewal did not happen.
    @discardableResult
    public func renew() async -> AccountSessionStatus {
        do { return try await renewChecked() } catch { return status() }
    }

    /// ``renew()`` that throws what went wrong (``AccountSessionError``). With no session it
    /// returns the signed-out status without a network call. An expired session (401) is cleared locally
    /// and rethrown as ``AccountSessionError/sessionExpired``; any other failure keeps the session.
    @discardableResult
    public func renewChecked() async throws -> AccountSessionStatus {
        guard let session = keyStore.read() else { return status() }
        let started = clearings
        do {
            let email = try await client.renewSession(token: session.token)
            // Signed out, expired elsewhere or replaced while the request was out: the answer is stale.
            guard started == clearings, keyStore.read()?.token == session.token else { return status() }
            try persist(token: session.token, email: email ?? session.email)
        } catch {
            throw clearOnExpiry(error, expectedToken: session.token)
        }
        return status()
    }

    @discardableResult
    public func signInWithApple(identityToken: String) async throws -> AccountSessionStatus {
        let started = clearings
        let result = try await client.signInWithApple(identityToken: identityToken)
        guard started == clearings else { return status() }
        try persist(token: result.token, email: result.email)
        return status()
    }

    /// Asks the server to mail a sign-in link. Nothing is stored.
    public func requestMagicLink(email: String) async throws {
        try await client.requestMagicLink(email: email)
    }

    /// Redeems the token of a sign-in link and stores the session it opens.
    @discardableResult
    public func completeMagicLink(token: String) async throws -> AccountSessionStatus {
        let started = clearings
        let result = try await client.consumeMagicLink(token: token)
        guard started == clearings else { return status() }
        try persist(token: result.token, email: result.email)
        return status()
    }

    /// The Bearer for calls such as link-storekit. Requires an existing session.
    public func bearerToken() throws -> String {
        guard let session = keyStore.read() else { throw AccountSessionError.notSignedIn }
        return session.token
    }

    /// Local-first and non-destructive: the Keychain item goes before the network call.
    public func signOut() async {
        let session = keyStore.read()
        clearSession()
        if let session { await client.signOut(token: session.token) }
    }

    /// Permanent server-side deletion. A thrown error is a failure, never success; on
    /// ``AccountSessionError/activeSubscription`` the local session is kept.
    public func deleteAccount() async throws {
        let token = try bearerToken()
        do { try await client.deleteAccount(token: token) } catch { throw clearOnExpiry(error, expectedToken: token) }
        clearSession()
    }

    /// Every device leased to the signed-in account. A 401 clears the local session.
    public func devices() async throws -> [AccountDeviceRow] {
        let token = try bearerToken()
        do { return try await client.devices(token: token) } catch { throw clearOnExpiry(error, expectedToken: token) }
    }

    /// Renames a device (`AccountDeviceRow.id`). A 401 clears the local session.
    public func renameDevice(id: String, label: String) async throws {
        let token = try bearerToken()
        do { try await client.renameDevice(id: id, label: label, token: token) } catch { throw clearOnExpiry(error, expectedToken: token) }
    }

    /// Removes a device (`AccountDeviceRow.id`), freeing its seat. A 401 clears the local session.
    public func removeDevice(id: String) async throws {
        let token = try bearerToken()
        do { try await client.removeDevice(id: id, token: token) } catch { throw clearOnExpiry(error, expectedToken: token) }
    }

    /// Links the StoreKit purchase proven by `signedTransaction` to the signed-in account.
    public func linkStoreKit(
        signedTransaction: String, deviceId: String, app: String, platform: String = AccountSessionClient.defaultPlatform,
    ) async throws -> LinkedPurchase {
        let token = try bearerToken()
        do {
            return try await client.linkStoreKit(
                sessionToken: token, signedTransaction: signedTransaction, deviceId: deviceId, app: app, platform: platform,
            )
        } catch { throw clearOnExpiry(error, expectedToken: token) }
    }

    /// A ``AccountSessionError/sessionExpired`` from any bearer call means the server forgot the
    /// token: drop it locally. Returns the error so callers can `throw` it.
    @discardableResult
    public func clearOnExpiry(_ error: Error, expectedToken: String) -> Error {
        if case AccountSessionError.sessionExpired = error, keyStore.read()?.token == expectedToken { clearSession() }
        return error
    }

    private func clearSession() {
        clearings &+= 1
        keyStore.clear()
        onChange?(.signedOut)
    }

    private func persist(token: String, email: String?) throws {
        try keyStore.write(StoredAccountSession(token: token, email: email, issuedAtUnix: Int64(now().timeIntervalSince1970)))
        onChange?(status())
    }

    private static func status(of session: StoredAccountSession?) -> AccountSessionStatus {
        guard let session else { return .signedOut }
        return AccountSessionStatus(signedIn: true, email: session.email, expiresAt: session.issuedAtUnix + sessionTTLSeconds)
    }
}
