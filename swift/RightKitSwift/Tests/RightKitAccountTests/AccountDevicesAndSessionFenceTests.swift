import Foundation
import RightKitHTTP
import Testing
@testable import RightKitAccount

// PK16-4 (HeardRight iOS parity): magic-link sign-in, device list/rename/remove, a throwing renewal,
// the change observer, and the clearing fence (a sign-in or renewal already in flight never brings
// back a session the user signed out of). Wire shapes are the ones `AccountSessionStore.swift` in
// heardright/ios-native sends today.

private final class RecordingTransport: HTTPTransport, @unchecked Sendable {
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

/// Holds the first request until `release()`; every later request is answered at once with `laterBody`.
private final class HoldFirstTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private let heldBody: String
    private let laterBody: String
    private var waiter: CheckedContinuation<Void, Never>?
    private var _arrived = false
    private var released = false

    init(heldBody: String, laterBody: String = "{}") {
        self.heldBody = heldBody
        self.laterBody = laterBody
    }

    var arrived: Bool { lock.withLock { _arrived } }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let isFirst: Bool = lock.withLock {
            if _arrived { return false }
            _arrived = true
            return true
        }
        if isFirst {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let resumeNow: Bool = lock.withLock {
                    if released { return true }
                    waiter = continuation
                    return false
                }
                if resumeNow { continuation.resume() }
            }
        }
        let body = isFirst ? heldBody : laterBody
        return (Data(body.utf8), HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
    }

    func release() {
        let pending: CheckedContinuation<Void, Never>? = lock.withLock {
            released = true
            let pending = waiter
            waiter = nil
            return pending
        }
        pending?.resume()
    }

    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) { throw URLError(.unsupportedURL) }
}

private final class MemoryKeys: AccountSessionKeyStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var session: StoredAccountSession?
    init(_ session: StoredAccountSession? = nil) { self.session = session }
    func read() -> StoredAccountSession? { lock.withLock { session } }
    func write(_ session: StoredAccountSession) throws { lock.withLock { self.session = session } }
    func clear() { lock.withLock { session = nil } }
}

private final class StatusLog: @unchecked Sendable {
    private let lock = NSLock()
    private var _values: [AccountSessionStatus] = []
    var values: [AccountSessionStatus] { lock.withLock { _values } }
    func append(_ status: AccountSessionStatus) { lock.withLock { _values.append(status) } }
}

private let base = URL(string: "https://account.example.com")!
private let live = StoredAccountSession(token: "tok", email: "a@b.c", issuedAtUnix: 1_000)

private func client(_ transport: any HTTPTransport) -> AccountSessionClient {
    AccountSessionClient(baseURL: base, http: HTTPClient(transport: transport))
}

private func jsonBody(_ request: URLRequest) -> [String: String] {
    (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String]) ?? [:]
}

private struct WaitTimedOut: Error {}

private func waitUntil(timeout: TimeInterval = 5, _ condition: @Sendable () -> Bool) async throws {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
        guard Date() < deadline else { throw WaitTimedOut() }
        try await Task.sleep(nanoseconds: 5_000_000)
    }
}

// MARK: Client

@Test func magicLinkRequestPostsTheTrimmedEmailWithoutABearer() async throws {
    let fake = RecordingTransport([(200, #"{"ok":true}"#)])
    try await client(fake).requestMagicLink(email: "  me@x.y ")
    let sent = try #require(fake.requests.first)
    #expect(sent.httpMethod == "POST")
    #expect(sent.url?.path == "/v1/shop/auth/magic-link/request")
    #expect(jsonBody(sent) == ["email": "me@x.y"])
    #expect(sent.value(forHTTPHeaderField: "Authorization") == nil)

    let silent = RecordingTransport([(204, "")])
    try await client(silent).requestMagicLink(email: "me@x.y")
    #expect(silent.requests.count == 1, "an empty success body is still success")
}

@Test func anEmptyMagicLinkEmailOrTokenNeverReachesTheNetwork() async {
    let fake = RecordingTransport([(200, "{}")])
    await #expect(throws: AccountSessionError.serverRejected("Enter an email address first.")) { try await client(fake).requestMagicLink(email: "   ") }
    await #expect(throws: AccountSessionError.serverRejected("The sign-in link is incomplete.")) { _ = try await client(fake).consumeMagicLink(token: " ") }
    #expect(fake.requests.isEmpty)
}

@Test func magicLinkConsumeReturnsTheSessionTokenAndEmail() async throws {
    let fake = RecordingTransport([(200, #"{"token":"s1","customer":{"email":"me@x.y","id":"c1"}}"#)])
    let result = try await client(fake).consumeMagicLink(token: " link-token ")
    #expect(result.token == "s1" && result.email == "me@x.y")
    let sent = try #require(fake.requests.first)
    #expect(sent.url?.path == "/v1/shop/auth/magic-link/consume")
    #expect(jsonBody(sent) == ["token": "link-token"])
    #expect(sent.value(forHTTPHeaderField: "Authorization") == nil)
    await #expect(throws: AccountSessionError.badResponse) {
        _ = try await client(RecordingTransport([(200, #"{"token":""}"#)])).consumeMagicLink(token: "t")
    }
    // A 401 on a call with no bearer is a refusal, not an expired session.
    await #expect(throws: AccountSessionError.serverRejected("expired link")) {
        _ = try await client(RecordingTransport([(401, #"{"error":"expired link"}"#)])).consumeMagicLink(token: "t")
    }
}

private let devicesJSON = """
{"items":[{"id":"d1","licenseKey":"HR-1","appKey":"heardright","platform":"ios","pool":"mobile","label":"Phone","state":"active","lastSeenAt":"2026-01-02T00:00:00Z","activatedAt":"2026-01-01T00:00:00Z","generation":3},
{"id":"d2","licenseKey":"HR-1","appKey":"heardright","state":"removed","activatedAt":"2025-12-01T00:00:00Z","generation":1}]}
"""

@Test func devicesListDecodesTheServerRowsWithABearer() async throws {
    let fake = RecordingTransport([(200, devicesJSON)])
    let rows = try await client(fake).devices(token: "tok")
    #expect(rows.map(\.id) == ["d1", "d2"])
    #expect(rows[0] == AccountDeviceRow(
        id: "d1", licenseKey: "HR-1", appKey: "heardright", platform: "ios", pool: "mobile", label: "Phone",
        state: "active", lastSeenAt: "2026-01-02T00:00:00Z", activatedAt: "2026-01-01T00:00:00Z", generation: 3
    ))
    #expect(rows[1].platform == nil && rows[1].label == nil && rows[1].state == "removed")
    let sent = try #require(fake.requests.first)
    #expect(sent.httpMethod == "GET" && sent.url?.path == "/v1/shop/account/devices")
    #expect(sent.value(forHTTPHeaderField: "Authorization") == "Bearer tok")
    await #expect(throws: AccountSessionError.sessionExpired) { _ = try await client(RecordingTransport([(401, "{}")])).devices(token: "tok") }
    await #expect(throws: AccountSessionError.badResponse) { _ = try await client(RecordingTransport([(200, #"{"items":"x"}"#)])).devices(token: "tok") }
    await #expect(throws: AccountSessionError.notSignedIn) { _ = try await client(fake).devices(token: "") }
}

@Test func renameAndRemoveAddressTheRowByItsIDWithABearer() async throws {
    let fake = RecordingTransport([(200, #"{"ok":true}"#)])
    try await client(fake).renameDevice(id: "d1", label: "Kitchen iPad", token: "tok")
    try await client(fake).removeDevice(id: "d1", token: "tok")
    let (rename, remove) = (fake.requests[0], fake.requests[1])
    #expect(rename.httpMethod == "PATCH" && rename.url?.path == "/v1/shop/account/devices/d1")
    #expect(jsonBody(rename) == ["label": "Kitchen iPad"])
    #expect(remove.httpMethod == "DELETE" && remove.url?.path == "/v1/shop/account/devices/d1")
    #expect(rename.value(forHTTPHeaderField: "Authorization") == "Bearer tok" && remove.value(forHTTPHeaderField: "Authorization") == "Bearer tok")
    await #expect(throws: AccountSessionError.serverRejected("Missing device id.")) { try await client(fake).removeDevice(id: "", token: "tok") }
    await #expect(throws: AccountSessionError.serverRejected("Missing device id.")) { try await client(fake).renameDevice(id: "a/b", label: "x", token: "tok") }
    #expect(fake.requests.count == 2, "a bad id sends nothing")
    await #expect(throws: AccountSessionError.sessionExpired) { try await client(RecordingTransport([(401, "{}")])).removeDevice(id: "d1", token: "tok") }
}

// MARK: Store

@Test func deviceCallsClearTheSessionOnA401AndNotifyOnce() async throws {
    let keys = MemoryKeys(live)
    let log = StatusLog()
    let store = AccountSessionStore(client: client(RecordingTransport([(401, "{}")])), keyStore: keys, onChange: { log.append($0) })
    await #expect(throws: AccountSessionError.sessionExpired) { _ = try await store.devices() }
    #expect(keys.read() == nil)
    #expect(log.values == [.signedOut])
    await #expect(throws: AccountSessionError.notSignedIn) { _ = try await store.devices() }
}

@Test func deviceCallsKeepTheSessionOnOtherFailuresAndThroughSuccess() async throws {
    let keys = MemoryKeys(live)
    let ok = AccountSessionStore(client: client(RecordingTransport([(200, devicesJSON)])), keyStore: keys)
    #expect(try await ok.devices().count == 2)
    let down = AccountSessionStore(client: client(RecordingTransport([(503, "{}")])), keyStore: keys)
    await #expect(throws: AccountSessionError.serverRejected("")) { try await down.renameDevice(id: "d1", label: "x") }
    #expect(keys.read() == live)
}

@Test func completingAMagicLinkStoresTheSessionAndNotifies() async throws {
    let keys = MemoryKeys()
    let log = StatusLog()
    let fake = RecordingTransport([(200, #"{"token":"s2","customer":{"email":"me@x.y"}}"#)])
    let store = AccountSessionStore(client: client(fake), keyStore: keys, now: { Date(timeIntervalSince1970: 9_000) }, onChange: { log.append($0) })
    let status = try await store.completeMagicLink(token: "link")
    #expect(status == AccountSessionStatus(signedIn: true, email: "me@x.y", expiresAt: 9_000 + AccountSessionStore.sessionTTLSeconds))
    #expect(keys.read()?.token == "s2")
    #expect(log.values == [status])
}

@Test func renewCheckedReportsWhyARenewalDidNotHappen() async throws {
    let none = RecordingTransport([(200, "{}")])
    let signedOut = AccountSessionStore(client: client(none), keyStore: MemoryKeys())
    #expect(try await signedOut.renewChecked() == .signedOut)
    #expect(none.requests.isEmpty, "no session, no request")

    let keys = MemoryKeys(live)
    let transient = AccountSessionStore(client: client(RecordingTransport([(503, #"{"error":"busy"}"#)])), keyStore: keys)
    await #expect(throws: AccountSessionError.serverRejected("busy")) { _ = try await transient.renewChecked() }
    #expect(keys.read() == live, "a transient failure keeps the session")
    #expect(await transient.renew().signedIn, "the quiet form still reports the kept state")

    let expired = AccountSessionStore(client: client(RecordingTransport([(401, "{}")])), keyStore: keys)
    await #expect(throws: AccountSessionError.sessionExpired) { _ = try await expired.renewChecked() }
    #expect(keys.read() == nil)
}

@Test func aRenewalThatFinishesAfterSignOutDoesNotResurrectTheSession() async throws {
    let gate = HoldFirstTransport(heldBody: #"{"email":"renewed@x.y"}"#)
    let keys = MemoryKeys(live)
    let store = AccountSessionStore(client: client(gate), keyStore: keys)

    let renewal = Task { await store.renew() }
    try await waitUntil { gate.arrived }
    await store.signOut()
    #expect(keys.read() == nil)
    gate.release()
    let status = await renewal.value
    #expect(status == .signedOut)
    #expect(keys.read() == nil, "the late renewal wrote nothing back")
}

@Test func aSignInThatFinishesAfterSignOutDoesNotBringTheSessionBack() async throws {
    let gate = HoldFirstTransport(heldBody: #"{"token":"late","customer":{"email":"me@x.y"}}"#)
    let keys = MemoryKeys(live)
    let store = AccountSessionStore(client: client(gate), keyStore: keys)

    let signIn = Task { try await store.signInWithApple(identityToken: "jwt") }
    try await waitUntil { gate.arrived }
    await store.signOut()
    gate.release()
    let status = try await signIn.value
    #expect(status == .signedOut)
    #expect(keys.read() == nil)
}

@Test func signOutAndDeletionNotifyTheObserverAndAnUndisturbedRenewalPersists() async throws {
    let keys = MemoryKeys(live)
    let log = StatusLog()
    let renewing = AccountSessionStore(
        client: client(RecordingTransport([(200, #"{"email":"new@x.y"}"#)])), keyStore: keys,
        now: { Date(timeIntervalSince1970: 7_000) }, onChange: { log.append($0) }
    )
    let renewed = try await renewing.renewChecked()
    #expect(renewed == AccountSessionStatus(signedIn: true, email: "new@x.y", expiresAt: 7_000 + AccountSessionStore.sessionTTLSeconds))
    await renewing.signOut()
    #expect(log.values == [renewed, .signedOut])

    let deleteLog = StatusLog()
    let deleting = AccountSessionStore(client: client(RecordingTransport([(204, "")])), keyStore: MemoryKeys(live), onChange: { deleteLog.append($0) })
    try await deleting.deleteAccount()
    #expect(deleteLog.values == [.signedOut])
}
