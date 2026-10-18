import Foundation
import RightKitCredentials
import RightKitHTTP
import RightKitLicense
import Testing
@testable import RightKitAccount

private final class FakeTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [(Int, String)]
    private var _requests: [URLRequest] = []
    init(_ replies: [(Int, String)]) { self.replies = replies }
    var requests: [URLRequest] { lock.withLock { _requests } }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (status, body) = lock.withLock { () -> (Int, String) in
            _requests.append(request)
            return replies.count > 1 ? replies.removeFirst() : replies[0]
        }
        return (Data(body.utf8), HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!)
    }

    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) { throw URLError(.unsupportedURL) }
}

private final class MemoryKeyStore: AccountSessionKeyStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var session: StoredAccountSession?
    init(_ session: StoredAccountSession? = nil) { self.session = session }
    func read() -> StoredAccountSession? { lock.withLock { session } }
    func write(_ session: StoredAccountSession) throws { lock.withLock { self.session = session } }
    func clear() { lock.withLock { session = nil } }
}

private let base = URL(string: "https://licence.example.com")!
private func client(_ fake: FakeTransport) -> AccountSessionClient { AccountSessionClient(baseURL: base, http: HTTPClient(transport: fake)) }
private let signedIn = StoredAccountSession(token: "tok", email: "a@b.c", issuedAtUnix: 1_000)

private func jsonBody(_ request: URLRequest) -> [String: String] {
    (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String]) ?? [:]
}

@Test func appleSignInPostsIdentityTokenAndReadsSession() async throws {
    let fake = FakeTransport([(200, #"{"token":"t1","customer":{"email":"me@x.y"}}"#)])
    let result = try await client(fake).signInWithApple(identityToken: "jwt")
    #expect(result.token == "t1" && result.email == "me@x.y")
    let sent = try #require(fake.requests.first)
    #expect(sent.url?.absoluteString == "https://licence.example.com/v1/shop/auth/apple")
    #expect(sent.httpMethod == "POST")
    #expect(jsonBody(sent) == ["identityToken": "jwt"])
    #expect(sent.value(forHTTPHeaderField: "Authorization") == nil)
}

@Test func emptyOrUnreadableSignInIsBadResponse() async {
    await #expect(throws: AccountSessionError.badResponse) { _ = try await client(FakeTransport([(200, #"{"token":""}"#)])).signInWithApple(identityToken: "j") }
    await #expect(throws: AccountSessionError.badResponse) { _ = try await client(FakeTransport([(200, "nope")])).signInWithApple(identityToken: "j") }
    await #expect(throws: AccountSessionError.serverRejected("no")) {
        _ = try await client(FakeTransport([(400, #"{"error":"no"}"#)])).signInWithApple(identityToken: "j")
    }
}

@Test func renewUsesBearerAndMaps401ToExpired() async throws {
    let fake = FakeTransport([(200, #"{"email":"e@x.y"}"#)])
    #expect(try await client(fake).renewSession(token: "tok") == "e@x.y")
    #expect(fake.requests.first?.value(forHTTPHeaderField: "Authorization") == "Bearer tok")
    await #expect(throws: AccountSessionError.sessionExpired) { _ = try await client(FakeTransport([(401, "{}")])).renewSession(token: "tok") }
}

@Test func deleteMapsConflictToActiveSubscription() async throws {
    let ok = FakeTransport([(204, "")])
    try await client(ok).deleteAccount(token: "tok")
    #expect(ok.requests.first?.httpMethod == "DELETE")
    #expect(ok.requests.first?.url?.path == "/v1/shop/account")
    await #expect(throws: AccountSessionError.activeSubscription) { try await client(FakeTransport([(409, "{}")])).deleteAccount(token: "tok") }
    await #expect(throws: AccountSessionError.sessionExpired) { try await client(FakeTransport([(401, "{}")])).deleteAccount(token: "tok") }
}

private let linkJSON = """
{"activationToken":"act","lease":{"leaseId":"L1","deviceIdHash":"h","pool":"mobile","issuedAt":"2026-01-01T00:00:00Z","expiresAt":"2026-02-01T00:00:00Z","graceSeconds":86400,"generation":1,"entitlement":"e","signature":"s"}}
"""

@Test func linkStoreKitSendsHashedDeviceAndReturnsLease() async throws {
    let fake = FakeTransport([(200, linkJSON)])
    let linked = try await client(fake).linkStoreKit(sessionToken: "tok", signedTransaction: "jws", deviceId: "dev-1", app: "coderight", platform: "ios")
    #expect(linked.activationToken == "act" && linked.lease.leaseId == "L1")
    let sent = try #require(fake.requests.first)
    #expect(sent.url?.path == "/v1/shop/pro/link-storekit")
    #expect(sent.value(forHTTPHeaderField: "Authorization") == "Bearer tok")
    let body = jsonBody(sent)
    #expect(body["appKey"] == "coderight" && body["signedTransactionInfo"] == "jws" && body["platform"] == "ios")
    #expect(body["deviceIdHash"] == LicenseActivationClient.deviceIDHash("dev-1"))
    #expect(body["deviceIdHash"] != "dev-1")
}

@Test func linkStoreKitMapsStatusesAndIncompleteResponses() async {
    func link(_ status: Int, _ body: String, token: String = "tok") async throws -> LinkedPurchase {
        try await client(FakeTransport([(status, body)])).linkStoreKit(sessionToken: token, signedTransaction: "j", deviceId: "d", app: "a")
    }
    await #expect(throws: AccountSessionError.sessionExpired) { _ = try await link(401, "{}") }
    await #expect(throws: AccountSessionError.purchaseRevoked) { _ = try await link(422, "{}") }
    await #expect(throws: AccountSessionError.notSignedIn) { _ = try await link(200, linkJSON, token: "") }
    await #expect(throws: AccountSessionError.serverRejected("nope")) { _ = try await link(200, #"{"error":"nope"}"#) }
    await #expect(throws: AccountSessionError.serverRejected("The link response was incomplete.")) { _ = try await link(200, "{}") }
}

@Test func transportFailuresAreNetworkErrors() async {
    struct Down: HTTPTransport {
        func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) { throw URLError(.notConnectedToInternet) }
        func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) { throw URLError(.notConnectedToInternet) }
    }
    let down = AccountSessionClient(baseURL: base, http: HTTPClient(transport: Down()))
    await #expect(throws: AccountSessionError.network("URLError -1009")) { _ = try await down.renewSession(token: "t") }
}

// MARK: Store

@Test func storeSignInPersistsAndReportsStatus() async throws {
    let keys = MemoryKeyStore()
    let store = AccountSessionStore(client: client(FakeTransport([(200, #"{"token":"t1","customer":{"email":"m@x.y"}}"#)])), keyStore: keys, now: { Date(timeIntervalSince1970: 5_000) })
    #expect(await store.status() == .signedOut)
    let status = try await store.signInWithApple(identityToken: "jwt")
    #expect(status == AccountSessionStatus(signedIn: true, email: "m@x.y", expiresAt: 5_000 + AccountSessionStore.sessionTTLSeconds))
    #expect(keys.read()?.token == "t1")
}

@Test func storeSignOutClearsLocallyBeforeTheNetworkCall() async throws {
    let keys = MemoryKeyStore(signedIn)
    let fake = FakeTransport([(500, "{}")])
    let store = AccountSessionStore(client: client(fake), keyStore: keys)
    await store.signOut()
    #expect(keys.read() == nil)
    #expect(fake.requests.first?.url?.path == "/v1/shop/auth/logout")
    #expect(fake.requests.first?.value(forHTTPHeaderField: "Authorization") == "Bearer tok")
    await #expect(throws: AccountSessionError.notSignedIn) { _ = try await store.bearerToken() }
}

@Test func storeRenewKeepsSessionOnTransientFailureAndDropsItOnExpiry() async {
    let keys = MemoryKeyStore(signedIn)
    let transient = AccountSessionStore(client: client(FakeTransport([(503, "{}")])), keyStore: keys)
    #expect(await transient.renew().signedIn)
    let expired = AccountSessionStore(client: client(FakeTransport([(401, "{}")])), keyStore: keys)
    #expect(await expired.renew() == .signedOut)
    #expect(keys.read() == nil)
}

@Test func storeDeleteKeepsSessionOnActiveSubscriptionAndClearsOnSuccess() async throws {
    let keys = MemoryKeyStore(signedIn)
    let blocked = AccountSessionStore(client: client(FakeTransport([(409, "{}")])), keyStore: keys)
    await #expect(throws: AccountSessionError.activeSubscription) { try await blocked.deleteAccount() }
    #expect(keys.read() != nil)
    let done = AccountSessionStore(client: client(FakeTransport([(204, "")])), keyStore: keys)
    try await done.deleteAccount()
    #expect(keys.read() == nil)
}

@Test func storeLinkRequiresSessionAndDropsExpiredToken() async {
    let none = AccountSessionStore(client: client(FakeTransport([(200, linkJSON)])), keyStore: MemoryKeyStore())
    await #expect(throws: AccountSessionError.notSignedIn) { _ = try await none.linkStoreKit(signedTransaction: "j", deviceId: "d", app: "a") }
    let keys = MemoryKeyStore(signedIn)
    let expired = AccountSessionStore(client: client(FakeTransport([(401, "{}")])), keyStore: keys)
    await #expect(throws: AccountSessionError.sessionExpired) { _ = try await expired.linkStoreKit(signedTransaction: "j", deviceId: "d", app: "a") }
    #expect(keys.read() == nil)
}

@Test func keychainStoreRoundTripsThroughSecretsStore() throws {
    let secrets = try SecretsStore(namespace: "acct.\(UUID().uuidString)", backend: MemorySecretsBackend())
    let store = AccountSessionKeychainStore(store: secrets)
    #expect(store.read() == nil)
    try store.write(signedIn)
    #expect(store.read() == signedIn)
    store.clear()
    #expect(store.read() == nil)
}

@Test func appleIdentityNeedsNonEmptyUTF8() {
    #expect(AppleIdentity(identityToken: nil) == nil)
    #expect(AppleIdentity(identityToken: Data()) == nil)
    #expect(AppleIdentity(identityToken: Data("jwt".utf8))?.identityToken == "jwt")
}
