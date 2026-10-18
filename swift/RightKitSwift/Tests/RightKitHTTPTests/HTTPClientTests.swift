import CryptoKit
import Foundation
import Testing
@testable import RightKitHTTP

final class FakeTransport: HTTPTransport, @unchecked Sendable {
    typealias Reply = (status: Int, headers: [String: String], body: Data)
    private let lock = NSLock()
    private var queue: [Result<Reply, Error>]
    private var _requests: [URLRequest] = []

    init(_ replies: [Result<Reply, Error>]) { queue = replies }

    var requests: [URLRequest] { lock.withLock { _requests } }

    static func ok(_ json: String, status: Int = 200, headers: [String: String] = [:]) -> Result<Reply, Error> {
        .success((status, headers, Data(json.utf8)))
    }

    private func next(_ request: URLRequest) throws -> Reply {
        try lock.withLock {
            _requests.append(request)
            let item = queue.count > 1 ? queue.removeFirst() : queue[0]
            return try item.get()
        }
    }

    private func response(_ request: URLRequest, _ reply: Reply) -> HTTPURLResponse {
        var headers = reply.headers
        if headers["Content-Length"] == nil { headers["Content-Length"] = String(reply.body.count) }
        return HTTPURLResponse(url: request.url!, statusCode: reply.status, httpVersion: "HTTP/1.1", headerFields: headers)!
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let reply = try next(request)
        return (reply.body, response(request, reply))
    }

    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) {
        let reply = try next(request)
        let body = reply.body
        let stream = AsyncThrowingStream<Data, Error> { continuation in
            var offset = 0
            while offset < body.count {
                let end = min(offset + 4, body.count)
                continuation.yield(body.subdata(in: offset..<end))
                offset = end
            }
            continuation.finish()
        }
        return (response(request, reply), stream)
    }
}

private let api = URL(string: "https://api.example.com/v1/thing")!

@Test func refusesPlainHTTPAndEmbeddedCredentials() async {
    let client = HTTPClient(transport: FakeTransport([FakeTransport.ok("{}")]))
    await #expect(throws: HTTPError.insecureURL("api.example.com")) {
        _ = try await client.send(HTTPRequest(url: URL(string: "http://api.example.com/x")!))
    }
    await #expect(throws: HTTPError.self) {
        _ = try await client.send(HTTPRequest(url: URL(string: "https://user:pw@api.example.com/x")!))
    }
}

@Test func allowsLoopbackHTTPUnlessDisabled() async throws {
    let fake = FakeTransport([FakeTransport.ok("{}")])
    let url = URL(string: "http://127.0.0.1:8080/x")!
    _ = try await HTTPClient(transport: fake).send(HTTPRequest(url: url))
    await #expect(throws: HTTPError.self) {
        _ = try await HTTPClient(transport: fake, allowInsecureLoopback: false).send(HTTPRequest(url: url))
    }
}

@Test func sendsBearerQueryAndAgentAndDecodesJSON() async throws {
    struct Out: Decodable, Equatable { var ok: Bool }
    let fake = FakeTransport([FakeTransport.ok(#"{"ok":true}"#)])
    let client = HTTPClient(transport: fake, userAgent: "app/1")
    let request = HTTPRequest(url: api, query: [URLQueryItem(name: "a", value: "b c")], bearer: "tok")
    let out = try await client.json(request, as: Out.self)
    #expect(out == Out(ok: true))
    let sent = try #require(fake.requests.first)
    #expect(sent.value(forHTTPHeaderField: "Authorization") == "Bearer tok")
    #expect(sent.value(forHTTPHeaderField: "User-Agent") == "app/1")
    #expect(sent.url?.query == "a=b%20c")
}

@Test func statusErrorCarriesServerMessageWithoutBearer() async {
    let fake = FakeTransport([FakeTransport.ok(#"{"error":"bad token sekret"}"#, status: 401)])
    let client = HTTPClient(transport: fake)
    do {
        _ = try await client.send(HTTPRequest(url: api, bearer: "sekret"))
        Issue.record("expected a throw")
    } catch let error as HTTPError {
        #expect(error == .status(code: 401, message: "bad token [redacted]"))
        #expect(error.statusCode == 401)
    } catch { Issue.record("wrong error \(error)") }
}

@Test func acceptedStatusIsReturnedNotThrown() async throws {
    let fake = FakeTransport([FakeTransport.ok("", status: 304)])
    let response = try await HTTPClient(transport: fake).send(HTTPRequest(url: api, acceptedStatuses: [304]))
    #expect(response.status == 304)
}

@Test func messageExtractionHandlesNestedAndPlainBodies() {
    #expect(HTTPClient.message(from: Data(#"{"error":{"message":"nope"}}"#.utf8)) == "nope")
    #expect(HTTPClient.message(from: Data(#"{"detail":"d"}"#.utf8)) == "d")
    #expect(HTTPClient.message(from: Data("plain text".utf8)) == "plain text")
    #expect(HTTPClient.message(from: Data("<html>x</html>".utf8)) == "")
}

@Test func oversizedBodyIsRefused() async {
    let fake = FakeTransport([FakeTransport.ok(String(repeating: "x", count: 100))])
    let client = HTTPClient(transport: fake, maxResponseBytes: 10)
    await #expect(throws: HTTPError.tooLarge(limit: 10)) { _ = try await client.send(HTTPRequest(url: api)) }
}

@Test func retriesIdempotentVerbsOnly() async throws {
    let fast = HTTPRetryPolicy(maxAttempts: 3, baseDelay: 0, maxDelay: 0)
    let getFake = FakeTransport([FakeTransport.ok("{}", status: 503), FakeTransport.ok("{}", status: 503), FakeTransport.ok(#"{"a":1}"#)])
    let client = HTTPClient(transport: getFake, retry: fast, sleep: { _ in })
    let response = try await client.send(HTTPRequest(url: api))
    #expect(response.status == 200)
    #expect(getFake.requests.count == 3)

    let postFake = FakeTransport([FakeTransport.ok("{}", status: 503)])
    let postClient = HTTPClient(transport: postFake, retry: fast, sleep: { _ in })
    await #expect(throws: HTTPError.status(code: 503, message: "")) {
        _ = try await postClient.send(HTTPRequest(method: .post, url: api))
    }
    #expect(postFake.requests.count == 1)
}

@Test func mapsTransportFailures() async {
    let timeout = HTTPClient(transport: FakeTransport([.failure(URLError(.timedOut))]))
    await #expect(throws: HTTPError.timeout) { _ = try await timeout.send(HTTPRequest(url: api)) }
    let cancelled = HTTPClient(transport: FakeTransport([.failure(URLError(.cancelled))]))
    await #expect(throws: HTTPError.cancelled) { _ = try await cancelled.send(HTTPRequest(url: api)) }
    let offline = HTTPClient(transport: FakeTransport([.failure(URLError(.notConnectedToInternet))]))
    await #expect(throws: HTTPError.transport("URLError -1009")) { _ = try await offline.send(HTTPRequest(url: api)) }
}

@Test func formBodyIsPercentEncoded() throws {
    let request = HTTPRequest.form(.post, api, fields: [("a b", "c&d=é"), ("k", "v")])
    #expect(String(decoding: try #require(request.body), as: UTF8.self) == "a+b=c%26d%3D%C3%A9&k=v")
    #expect(request.headers["Content-Type"] == "application/x-www-form-urlencoded")
}

@Test func jsonBodySetsContentType() throws {
    let request = try HTTPRequest.json(.post, api, body: ["a": "b"])
    #expect(request.headers["Content-Type"] == "application/json")
    #expect(String(decoding: try #require(request.body), as: UTF8.self) == #"{"a":"b"}"#)
}

// MARK: Downloads

private func tempFile() -> URL {
    FileManager.default.temporaryDirectory.appendingPathComponent("rk-http-\(UUID().uuidString)/out.bin")
}

private func hex(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }

@Test func downloadWritesVerifiedFile() async throws {
    let body = Data((0..<1000).map { UInt8($0 % 251) })
    let fake = FakeTransport([.success((200, [:], body))])
    let dest = tempFile()
    defer { try? FileManager.default.removeItem(at: dest.deletingLastPathComponent()) }
    let result = try await HTTPClient(transport: fake).download(
        HTTPRequest(url: api), to: dest, expectedSize: 1000, expectedSHA256: hex(body).uppercased())
    #expect(try Data(contentsOf: dest) == body)
    #expect(result.byteCount == 1000)
    #expect(result.sha256 == hex(body))
    #expect(!FileManager.default.fileExists(atPath: dest.appendingPathExtension("part").path))
}

@Test func downloadFailsClosedOnDigestSizeAndStatus() async throws {
    let body = Data("hello world".utf8)
    let dest = tempFile()
    defer { try? FileManager.default.removeItem(at: dest.deletingLastPathComponent()) }
    let wrongDigest = HTTPClient(transport: FakeTransport([.success((200, [:], body))]))
    await #expect(throws: HTTPError.integrity("SHA-256 mismatch")) {
        try await wrongDigest.download(HTTPRequest(url: api), to: dest, expectedSHA256: String(repeating: "0", count: 64))
    }
    let wrongSize = HTTPClient(transport: FakeTransport([.success((200, [:], body))]))
    await #expect(throws: HTTPError.self) {
        try await wrongSize.download(HTTPRequest(url: api), to: dest, expectedSize: 5)
    }
    let notFound = HTTPClient(transport: FakeTransport([.success((404, [:], Data()))]))
    await #expect(throws: HTTPError.status(code: 404, message: "")) {
        try await notFound.download(HTTPRequest(url: api), to: dest)
    }
    #expect(!FileManager.default.fileExists(atPath: dest.path))
    #expect(!FileManager.default.fileExists(atPath: dest.appendingPathExtension("part").path))
}

// MARK: Redirect policy

@Test func redirectGuardRefusesDowngradeAndStripsCrossHostCredentials() {
    var downgrade = URLRequest(url: URL(string: "http://evil.example.com/x")!)
    downgrade.setValue("Bearer t", forHTTPHeaderField: "Authorization")
    #expect(RedirectGuard.filter(newRequest: downgrade, originalHost: "api.example.com") == nil)

    var cross = URLRequest(url: URL(string: "https://cdn.example.net/x")!)
    cross.setValue("Bearer t", forHTTPHeaderField: "Authorization")
    #expect(RedirectGuard.filter(newRequest: cross, originalHost: "api.example.com")?.value(forHTTPHeaderField: "Authorization") == nil)

    var same = URLRequest(url: URL(string: "https://api.example.com/y")!)
    same.setValue("Bearer t", forHTTPHeaderField: "Authorization")
    #expect(RedirectGuard.filter(newRequest: same, originalHost: "api.example.com")?.value(forHTTPHeaderField: "Authorization") == "Bearer t")
}
