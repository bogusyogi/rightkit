import CryptoKit
import Foundation
import RightKitHTTP
import Testing
@testable import RightKitUpdate

private final class FakeFeed: UpdateFeed, @unchecked Sendable {
    private let lock = NSLock()
    var results: [Result<FeedResult, UpdateError>]
    private(set) var etagsSeen: [String?] = []
    init(_ results: [Result<FeedResult, UpdateError>]) { self.results = results }

    func latest(etag: String?) async throws -> FeedResult {
        try lock.withLock {
            etagsSeen.append(etag)
            return try (results.count > 1 ? results.removeFirst() : results[0]).get()
        }
    }
}

private final class FakeInstaller: UpdateInstalling, @unchecked Sendable {
    private let lock = NSLock()
    private(set) var staged: [URL] = []
    private(set) var installed: [String] = []
    private(set) var relaunched = 0
    var rejectStage = false

    func stage(artifact: URL, release: UpdateRelease, currentVersion: String, workDirectory: URL) async throws -> StagedBundle {
        if rejectStage { throw UpdateError.verificationFailed("rejected") }
        lock.withLock { staged.append(artifact) }
        return StagedBundle(appURL: workDirectory.appendingPathComponent("App.app"), version: release.version, workDirectory: workDirectory)
    }

    func install(_ staged: StagedBundle) async throws { lock.withLock { installed.append(staged.version) } }
    func relaunch(_ staged: StagedBundle) throws { lock.withLock { relaunched += 1 } }
}

private let payload = Data("artifact-bytes".utf8)
private func sha(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }

private func release(_ version: String = "1.1.0", tier: UpdateTier? = .patch, sha256: String? = nil, https: Bool = true) -> UpdateRelease {
    UpdateRelease(
        version: version, tier: tier,
        asset: UpdateAsset(name: "App.dmg", url: URL(string: https ? "https://dl.example.com/App.dmg" : "http://dl.example.com/App.dmg")!, size: Int64(payload.count), sha256: sha256 ?? sha(payload)),
    )
}

private final class Statuses: @unchecked Sendable {
    private let lock = NSLock()
    private(set) var all: [UpdateStatus] = []
    func add(_ s: UpdateStatus) { lock.withLock { all.append(s) } }
}

private func makeClient(
    feed: FakeFeed, installer: FakeInstaller? = FakeInstaller(), current: String = "1.0.0", body: Data = payload,
    eligible: @escaping UpdateClient.Eligibility = { $0.tier != .update }, canInstall: @escaping @Sendable () async -> Bool = { true },
    statuses: Statuses = Statuses(),
) -> (UpdateClient, FeedTransport, URL) {
    let transport = FeedTransport([(200, [:], body)])
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("rk-upd-\(UUID().uuidString)")
    let client = UpdateClient(
        currentVersion: current, feed: feed, installer: installer, http: HTTPClient(transport: transport), minCheckInterval: 0,
        workRoot: root, isEligible: eligible, canInstall: canInstall, onStatus: { statuses.add($0) },
    )
    return (client, transport, root)
}

@Test func checkReportsAvailableUpToDateAndIneligible() async {
    let newer = FakeFeed([.success(.fresh(release("1.1.0"), etag: "e"))])
    let (client, _, _) = makeClient(feed: newer)
    #expect(await client.checkOnce(force: true) == .available(version: "1.1.0", tier: .patch, eligible: true))

    let same = FakeFeed([.success(.fresh(release("1.0.0"), etag: nil))])
    #expect(await makeClient(feed: same).0.checkOnce() == .upToDate)

    let notListed = FakeFeed([.success(.fresh(nil, etag: nil))])
    #expect(await makeClient(feed: notListed).0.checkOnce() == .upToDate)

    let proOnly = FakeFeed([.success(.fresh(release("1.2.0", tier: .update), etag: nil))])
    #expect(await makeClient(feed: proOnly).0.checkOnce() == .available(version: "1.2.0", tier: .update, eligible: false))
}

@Test func checkSurfacesFeedErrorsAndGarbageVersions() async {
    let limited = FakeFeed([.failure(.rateLimited)])
    guard case .error = await makeClient(feed: limited).0.checkOnce() else { Issue.record("expected error"); return }
    let garbage = FakeFeed([.success(.fresh(release("not-a-version"), etag: nil))])
    guard case .error = await makeClient(feed: garbage).0.checkOnce() else { Issue.record("expected error"); return }
}

@Test func unchangedFeedReusesLastReleaseAndEtag() async {
    let feed = FakeFeed([.success(.fresh(release("1.1.0"), etag: "e1")), .success(.unchanged)])
    let (client, _, _) = makeClient(feed: feed)
    _ = await client.checkOnce(force: true)
    #expect(await client.checkOnce(force: true) == .available(version: "1.1.0", tier: .patch, eligible: true))
    #expect(feed.etagsSeen == [nil, "e1"])
}

@Test func downloadVerifiesDigestStagesAndInstallsOnlyTheCurrentHandle() async throws {
    let installer = FakeInstaller()
    let statuses = Statuses()
    let feed = FakeFeed([.success(.fresh(release(), etag: nil))])
    let (client, _, root) = makeClient(feed: feed, installer: installer, statuses: statuses)
    defer { try? FileManager.default.removeItem(at: root) }
    _ = await client.checkOnce(force: true)
    let handle = try await client.downloadAvailable()
    #expect(installer.staged.count == 1)
    #expect(try Data(contentsOf: installer.staged[0]) == payload)
    #expect(installer.installed.isEmpty)
    #expect(statuses.all.contains(.staged(version: "1.1.0")))

    try await client.installStaged(handle)
    #expect(installer.installed == ["1.1.0"])
    #expect(await client.status == .installed(version: "1.1.0"))
    try await client.relaunch(handle)
    #expect(installer.relaunched == 1)
}

@Test func aNewerCheckInvalidatesTheStagedHandle() async throws {
    let installer = FakeInstaller()
    let feed = FakeFeed([.success(.fresh(release(), etag: nil))])
    let (client, _, root) = makeClient(feed: feed, installer: installer)
    defer { try? FileManager.default.removeItem(at: root) }
    _ = await client.checkOnce(force: true)
    let handle = try await client.downloadAvailable()
    _ = await client.checkOnce(force: true)
    await #expect(throws: UpdateError.staleHandle) { try await client.installStaged(handle) }
    #expect(installer.installed.isEmpty)
}

@Test func downloadFailsClosedOnDigestMismatchInsecureAssetAndRejectedStage() async throws {
    let installer = FakeInstaller()
    let (bad, _, root1) = makeClient(feed: FakeFeed([.success(.fresh(release(sha256: String(repeating: "0", count: 64)), etag: nil))]), installer: installer)
    defer { try? FileManager.default.removeItem(at: root1) }
    _ = await bad.checkOnce(force: true)
    await #expect(throws: UpdateError.self) { _ = try await bad.downloadAvailable() }
    #expect(installer.staged.isEmpty)

    let (insecure, _, root2) = makeClient(feed: FakeFeed([.success(.fresh(release(https: false), etag: nil))]), installer: installer)
    defer { try? FileManager.default.removeItem(at: root2) }
    _ = await insecure.checkOnce(force: true)
    await #expect(throws: UpdateError.insecureURL) { _ = try await insecure.downloadAvailable() }

    let rejecting = FakeInstaller()
    rejecting.rejectStage = true
    let (rejected, _, root3) = makeClient(feed: FakeFeed([.success(.fresh(release(), etag: nil))]), installer: rejecting)
    defer { try? FileManager.default.removeItem(at: root3) }
    _ = await rejected.checkOnce(force: true)
    await #expect(throws: UpdateError.verificationFailed("rejected")) { _ = try await rejected.downloadAvailable() }
    #expect(rejecting.installed.isEmpty)
}

@Test func ineligibleReleaseIsNeverDownloadedAndHostCanDeferInstall() async throws {
    let installer = FakeInstaller()
    let (pro, _, rootA) = makeClient(feed: FakeFeed([.success(.fresh(release(tier: .update), etag: nil))]), installer: installer)
    defer { try? FileManager.default.removeItem(at: rootA) }
    _ = await pro.checkOnce(force: true)
    await #expect(throws: UpdateError.nothingAvailable) { _ = try await pro.downloadAvailable() }

    let (busy, _, rootB) = makeClient(feed: FakeFeed([.success(.fresh(release(), etag: nil))]), installer: installer, canInstall: { false })
    defer { try? FileManager.default.removeItem(at: rootB) }
    _ = await busy.checkOnce(force: true)
    let handle = try await busy.downloadAvailable()
    await #expect(throws: UpdateError.deferred) { try await busy.installStaged(handle) }
    #expect(installer.installed.isEmpty)
}

@Test func withoutAnInstallerTheClientOnlyChecks() async {
    let (client, _, _) = makeClient(feed: FakeFeed([.success(.fresh(release(), etag: nil))]), installer: nil)
    _ = await client.checkOnce(force: true)
    await #expect(throws: UpdateError.self) { _ = try await client.downloadAvailable() }
}

@Test func assetNamesCannotEscapeTheWorkDirectory() {
    #expect(UpdateClient.safeFileName("../../etc/passwd") == "passwd")
    #expect(UpdateClient.safeFileName("..") == "_")
    #expect(UpdateClient.safeFileName("App.dmg") == "App.dmg")
}
