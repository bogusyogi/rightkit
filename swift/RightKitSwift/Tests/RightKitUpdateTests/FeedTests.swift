import Foundation
import RightKitHTTP
import Testing
@testable import RightKitUpdate

final class FeedTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [(Int, [String: String], Data)]
    private var _requests: [URLRequest] = []
    init(_ replies: [(Int, [String: String], Data)]) { self.replies = replies }
    convenience init(status: Int = 200, headers: [String: String] = [:], body: String) { self.init([(status, headers, Data(body.utf8))]) }
    var requests: [URLRequest] { lock.withLock { _requests } }

    private func next(_ request: URLRequest) -> (Int, [String: String], Data) {
        lock.withLock {
            _requests.append(request)
            return replies.count > 1 ? replies.removeFirst() : replies[0]
        }
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (status, headers, body) = next(request)
        return (body, HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: headers)!)
    }

    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) {
        let (status, headers, body) = next(request)
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: headers)!
        return (response, AsyncThrowingStream { continuation in
            var offset = 0
            while offset < body.count {
                let end = min(offset + 7, body.count)
                continuation.yield(body.subdata(in: offset..<end))
                offset = end
            }
            continuation.finish()
        })
    }
}

private let githubRelease = """
{"tag_name":"v0.3.0","body":"notes","html_url":"https://github.com/Orthic-Labs/pulse/releases/tag/v0.3.0","assets":[
 {"name":"Pulse-0.3.0.dmg","browser_download_url":"https://github.com/x/Pulse-0.3.0.dmg","size":10},
 {"name":"Pulse.dmg","browser_download_url":"https://github.com/x/Pulse.dmg","size":12,"digest":"sha256:\(String(repeating: "ab", count: 32))"},
 {"name":"Pulse.zip","browser_download_url":"http://insecure.example.com/Pulse.zip","size":5}]}
"""

private func ghFeed(_ transport: FeedTransport) -> GitHubReleaseFeed {
    GitHubReleaseFeed(owner: "Orthic-Labs", repo: "pulse", assetNames: ["Pulse.dmg"], assetPrefix: "Pulse-", assetSuffix: ".dmg", http: HTTPClient(transport: transport))
}

@Test func githubFeedPicksExactAssetWithDigestAndSendsEtag() async throws {
    let transport = FeedTransport(headers: ["ETag": "\"e1\""], body: githubRelease)
    guard case let .fresh(release?, etag) = try await ghFeed(transport).latest(etag: "\"old\"") else { Issue.record("no release"); return }
    #expect(release.version == "0.3.0" && release.notes == "notes")
    #expect(release.asset?.name == "Pulse.dmg" && release.asset?.size == 12)
    #expect(release.asset?.sha256 == String(repeating: "ab", count: 32))
    #expect(etag == "\"e1\"")
    let sent = try #require(transport.requests.first)
    #expect(sent.url?.absoluteString == "https://api.github.com/repos/Orthic-Labs/pulse/releases/latest")
    #expect(sent.value(forHTTPHeaderField: "If-None-Match") == "\"old\"")
    #expect(sent.value(forHTTPHeaderField: "Authorization") == nil)
}

@Test func githubFeedFallsBackToPrefixAndIgnoresInsecureAssets() async throws {
    let body = githubRelease.replacingOccurrences(of: "\"Pulse.dmg\"", with: "\"Other.dmg\"")
    guard case let .fresh(release?, _) = try await ghFeed(FeedTransport(body: body)).latest(etag: nil) else { Issue.record("no release"); return }
    #expect(release.asset?.name == "Pulse-0.3.0.dmg")
    let onlyInsecure = """
    {"tag_name":"1.0.0","assets":[{"name":"Pulse.dmg","browser_download_url":"http://x.example.com/Pulse.dmg","size":1}]}
    """
    guard case let .fresh(bare?, _) = try await ghFeed(FeedTransport(body: onlyInsecure)).latest(etag: nil) else { Issue.record("no release"); return }
    #expect(bare.asset == nil)
}

@Test func githubFeedMapsNotModifiedRateLimitAndGarbage() async throws {
    #expect(try await ghFeed(FeedTransport(status: 304, body: "")).latest(etag: "x") == .unchanged)
    await #expect(throws: UpdateError.rateLimited) { _ = try await ghFeed(FeedTransport(status: 429, body: "{}")).latest(etag: nil) }
    await #expect(throws: UpdateError.rateLimited) { _ = try await ghFeed(FeedTransport(status: 403, body: "{}")).latest(etag: nil) }
    await #expect(throws: UpdateError.self) { _ = try await ghFeed(FeedTransport(body: "{}")).latest(etag: nil) }
}

private func manifest(tier: String? = "\"patch\"", platforms: String = #""darwin-aarch64":{"url":"https://dl.example.com/App.app.tar.gz","signature":"sig"}"#) -> String {
    #"{"version":"1.4.0","notes":"n","tier":\#(tier ?? "null"),"platforms":{\#(platforms)}}"#
}

@Test func staticManifestRequiresTierAndHttpsAsset() throws {
    let ok = try StaticManifestFeed.parse(Data(manifest().utf8), platformKey: "darwin-aarch64", requireTier: true)
    #expect(ok?.tier == .patch && ok?.version == "1.4.0")
    #expect(ok?.asset?.url.absoluteString == "https://dl.example.com/App.app.tar.gz")
    #expect(throws: UpdateError.self) { try StaticManifestFeed.parse(Data(manifest(tier: "\"major\"").utf8), platformKey: "darwin-aarch64", requireTier: true) }
    #expect(throws: UpdateError.self) {
        try StaticManifestFeed.parse(Data(#"{"version":"1.4.0","platforms":{}}"#.utf8), platformKey: "darwin-aarch64", requireTier: true)
    }
    #expect(try StaticManifestFeed.parse(Data(#"{"version":"1.4.0","platforms":{}}"#.utf8), platformKey: "darwin-aarch64", requireTier: false) == nil)
    let http = manifest(platforms: #""darwin-aarch64":{"url":"http://dl.example.com/a.tgz","signature":"s"}"#)
    #expect(throws: UpdateError.self) { try StaticManifestFeed.parse(Data(http.utf8), platformKey: "darwin-aarch64", requireTier: true) }
    #expect(throws: UpdateError.self) { try StaticManifestFeed.parse(Data(#"{"version":"x"}"#.utf8), platformKey: "darwin-aarch64", requireTier: true) }
}

@Test func appStoreLookupReturnsVersionAndStorePage() async throws {
    let body = #"{"resultCount":1,"results":[{"version":"2.1","trackViewUrl":"https://apps.apple.com/app/id1","releaseNotes":"fixes"}]}"#
    let transport = FeedTransport(body: body)
    guard case let .fresh(release?, _) = try await AppStoreLookupFeed(bundleID: "com.example.app", http: HTTPClient(transport: transport)).latest(etag: nil) else {
        Issue.record("no release"); return
    }
    #expect(release.version == "2.1" && release.pageURL?.host == "apps.apple.com" && release.asset == nil)
    #expect(transport.requests.first?.url?.query == "bundleId=com.example.app")
    let none = AppStoreLookupFeed(bundleID: "x", http: HTTPClient(transport: FeedTransport(body: #"{"results":[]}"#)))
    #expect(try await none.latest(etag: nil) == .fresh(nil, etag: nil))
}

@Test func releaseStatusIsStrict() throws {
    let ok = try ReleaseStatus.parse(Data(#"{"schema":1,"appKey":"app","platform":null,"patchVersion":"1.0.1","updateVersion":null}"#.utf8))
    #expect(ok == ReleaseStatus(appKey: "app", platform: nil, patchVersion: "1.0.1", updateVersion: nil))
    #expect(throws: UpdateError.self) { try ReleaseStatus.parse(Data(#"{"schema":2,"appKey":"app"}"#.utf8)) }
    #expect(throws: UpdateError.self) { try ReleaseStatus.parse(Data(#"{"schema":1,"appKey":"app","extra":1}"#.utf8)) }
    #expect(throws: UpdateError.self) { try ReleaseStatus.parse(Data(#"{"schema":1,"appKey":""}"#.utf8)) }
    #expect(throws: UpdateError.self) { try ReleaseStatus.parse(Data(#"{"schema":1,"appKey":"a","patchVersion":3}"#.utf8)) }
}

private let releasesList = """
[{"prerelease":true,"draft":false,"assets":[{"name":"latest.json","browser_download_url":"https://github.com/n3/latest.json"}]},
 {"prerelease":false,"draft":false,"assets":[{"name":"latest.json","browser_download_url":"https://github.com/stable/latest.json"}]},
 {"prerelease":true,"draft":true,"assets":[{"name":"latest.json","browser_download_url":"https://github.com/draft/latest.json"}]},
 {"prerelease":true,"draft":false,"assets":[{"name":"other.json","browser_download_url":"https://github.com/n2/other.json"}]},
 {"prerelease":true,"draft":false,"assets":[{"name":"latest.json","browser_download_url":"http://github.com/insecure/latest.json"}]},
 {"prerelease":true,"draft":false,"assets":[{"name":"latest.json","browser_download_url":"https://github.com/n1/latest.json"}]},
 {"prerelease":true,"draft":false,"assets":[{"name":"latest.json","browser_download_url":"https://github.com/n0/latest.json"}]}]
"""

@Test func nightlyDiscoveryTakesTopNPrereleaseManifests() throws {
    let urls = try NightlyDiscovery.manifestURLs(releasesJSON: Data(releasesList.utf8), limit: 2)
    #expect(urls.map(\.absoluteString) == ["https://github.com/n3/latest.json", "https://github.com/n1/latest.json"])
    #expect(throws: UpdateError.self) { try NightlyDiscovery.manifestURLs(releasesJSON: Data("{}".utf8), limit: 2) }
}

private final class Warnings: @unchecked Sendable {
    private let lock = NSLock()
    private(set) var messages: [String] = []
    func add(_ m: String) { lock.withLock { messages.append(m) } }
}

@Test func nightlyDiscoveryWarnsOnceAcrossFailuresThenRecovers() async {
    let gate = WarnOnceGate()
    let warnings = Warnings()
    let down = HTTPClient(transport: FeedTransport(status: 500, body: "{}"))
    for _ in 0..<3 {
        let urls = await NightlyDiscovery.discover(owner: "o", repo: "r", limit: 2, http: down, gate: gate, onWarning: { warnings.add($0) })
        #expect(urls.isEmpty)
    }
    #expect(warnings.messages.count == 1)
    let up = HTTPClient(transport: FeedTransport(body: releasesList))
    let urls = await NightlyDiscovery.discover(owner: "o", repo: "r", limit: 1, http: up, gate: gate, onWarning: { warnings.add($0) })
    #expect(urls.count == 1)
    _ = await NightlyDiscovery.discover(owner: "o", repo: "r", limit: 2, http: down, gate: gate, onWarning: { warnings.add($0) })
    #expect(warnings.messages.count == 2)
}
