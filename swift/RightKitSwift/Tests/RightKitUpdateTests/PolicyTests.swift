import Foundation
import RightKitHTTP
import Testing
@testable import RightKitUpdate

// MARK: Versions (vectors from packages/app-update/src/version.test.ts)

private func v(_ text: String) -> SemanticVersion { SemanticVersion(text)! }

@Test func versionOrdersLikeTheTypeScriptPackage() {
    #expect(v("1.10.0") > v("1.9.9"))
    #expect(v("1.2.3") == v("1.2.3"))
    #expect(v("0.9.0") < v("1.0.0"))
    #expect(v("1.0.0-beta.2") < v("1.0.0"))
    #expect(v("1.0.0-beta.2") < v("1.0.0-beta.10"))
    #expect(v("1.0.0-alpha") > v("1.0.0-1"))
    #expect(v("1.0.0-alpha") < v("1.0.0-alpha.1"))
    #expect(v("v1.2.3+abc") == v("1.2.3"))
}

@Test func versionRejectsGarbageAndOnlyPadsWhenAsked() {
    #expect(SemanticVersion("1.2") == nil)
    #expect(SemanticVersion("x") == nil)
    #expect(SemanticVersion("1.2.3.4") == nil)
    #expect(SemanticVersion("1.2.3-") == nil)
    #expect(SemanticVersion("1..3") == nil)
    #expect(SemanticVersion("1.2", allowShort: true) == v("1.2.0"))
    #expect(SemanticVersion("3", allowShort: true) == v("3.0.0"))
    #expect(SemanticVersion("1.2.3-beta.1")?.description == "1.2.3-beta.1")
}

@Test func isNewerIsStrictAndFalseOnGarbage() {
    #expect(SemanticVersion.isNewer("1.0.1", than: "1.0.0"))
    #expect(!SemanticVersion.isNewer("1.0.0", than: "1.0.0"))
    #expect(!SemanticVersion.isNewer("nope", than: "1.0.0"))
}

// MARK: Channels (vectors from channels.test.ts)

private let channelMap = ["stable": ["https://updates.example.com/stable.json"], "nightly": ["https://updates.example.com/nightly.json"]]

@Test func channelsResolveDefaultNamedAndFallback() throws {
    let channels = try UpdateChannels(channelMap)
    #expect(try channels.resolve(requested: nil, defaultChannel: "stable").channel == "stable")
    #expect(try channels.resolve(requested: "nightly", defaultChannel: "stable").endpoints[0].absoluteString.contains("nightly"))
    #expect(throws: UpdateChannelError.unknownChannel("beta")) { try channels.resolve(requested: "beta", defaultChannel: "stable") }
    #expect(try channels.resolve(requested: "beta", defaultChannel: "stable", allowFallback: true).channel == "stable")
    #expect(throws: UpdateChannelError.undefinedDefault("x")) { try channels.resolve(requested: nil, defaultChannel: "x") }
}

@Test func channelsRejectBadInput() {
    #expect(throws: UpdateChannelError.noChannels) { try UpdateChannels([:]) }
    #expect(throws: UpdateChannelError.invalidName("Bad Name")) { try UpdateChannels(["Bad Name": channelMap["stable"]!]) }
    #expect(throws: UpdateChannelError.noEndpoints(channel: "a")) { try UpdateChannels(["a": []]) }
    #expect(throws: UpdateChannelError.insecureEndpoint(channel: "a")) { try UpdateChannels(["a": ["http://example.com/x"]]) }
    #expect(throws: UpdateChannelError.invalidEndpoint(channel: "a")) { try UpdateChannels(["a": ["not a url"]]) }
    #expect(throws: UpdateChannelError.invalidName(String(repeating: "a", count: 33))) { try UpdateChannels([String(repeating: "a", count: 33): channelMap["stable"]!]) }
}

@Test func channelsAcceptGitHubLatestAndLoopback() throws {
    _ = try UpdateChannels(["stable": ["https://github.com/Orthic-Labs/ember/releases/latest/download/latest.json"]])
    _ = try UpdateChannels(["a": ["http://localhost:8080/x"]])
}

// MARK: Check gate

private actor Counter {
    var value = 0
    func next() -> Int { value += 1; return value }
}

private final class Clock: @unchecked Sendable {
    private let lock = NSLock()
    private var t = Date(timeIntervalSince1970: 0)
    func now() -> Date { lock.withLock { t } }
    func advance(_ s: TimeInterval) { lock.withLock { t = t.addingTimeInterval(s) } }
}

@Test func gateCachesInsideWindowAndRerunsAfterOrWhenForced() async throws {
    let clock = Clock()
    let gate = CheckGate<Int>(minInterval: 60, now: { clock.now() })
    let counter = Counter()
    let first = try await gate.run { await counter.next() }
    let second = try await gate.run { await counter.next() }
    #expect(first == 1 && second == 1)
    #expect(try await gate.run(force: true) { await counter.next() } == 2)
    clock.advance(61)
    #expect(try await gate.run { await counter.next() } == 3)
    await gate.reset()
    #expect(try await gate.run { await counter.next() } == 4)
}

@Test func gateSharesOneInflightRunAndNeverCachesFailure() async throws {
    let gate = CheckGate<Int>(minInterval: 60)
    let counter = Counter()
    async let a = gate.run { try await Task.sleep(nanoseconds: 50_000_000); return await counter.next() }
    async let b = gate.run { await counter.next() }
    let (x, y) = try await (a, b)
    #expect(x == y)
    #expect(await counter.value == 1)

    struct Boom: Error {}
    let failing = CheckGate<Int>(minInterval: 60)
    await #expect(throws: Boom.self) { _ = try await failing.run { throw Boom() } }
    #expect(try await failing.run { 7 } == 7)
}

@Test func warnOnceReportsUntilSuccess() async {
    let gate = WarnOnceGate()
    #expect(await gate.shouldWarn())
    #expect(await !gate.shouldWarn())
    await gate.recordSuccess()
    #expect(await gate.shouldWarn())
}

// MARK: Probation

private final class MemoryProbationStorage: BootProbationStorage, @unchecked Sendable {
    private let lock = NSLock()
    var record: BootProbationRecord?
    func read() throws -> BootProbationRecord? { lock.withLock { record } }
    func write(_ record: BootProbationRecord) throws { lock.withLock { self.record = record } }
    func clear() throws { lock.withLock { record = nil } }
}

@Test func probationCountsBootsThenFailsWithoutAnyRollbackPath() async throws {
    let storage = MemoryProbationStorage()
    let probation = BootProbation(storage: storage, maxBootAttempts: 2)
    #expect(try await probation.evaluateBoot(currentVersion: "1.0.0") == .none)
    try await probation.begin(fromVersion: "1.0.0", toVersion: "1.1.0")
    #expect(try await probation.evaluateBoot(currentVersion: "1.1.0") == .probation(toVersion: "1.1.0", bootAttempts: 1))
    #expect(try await probation.evaluateBoot(currentVersion: "1.1.0") == .probation(toVersion: "1.1.0", bootAttempts: 2))
    #expect(try await probation.evaluateBoot(currentVersion: "1.1.0") == .failed(toVersion: "1.1.0", restoreVersion: "1.0.0", bootAttempts: 3))
    #expect(storage.record == nil)
}

@Test func probationClearsOnHealthyAndOnSupersededVersion() async throws {
    let storage = MemoryProbationStorage()
    let probation = BootProbation(storage: storage)
    try await probation.begin(fromVersion: "1.0.0", toVersion: "1.1.0")
    _ = try await probation.evaluateBoot(currentVersion: "1.1.0")
    try await probation.acknowledgeHealthy()
    #expect(storage.record == nil)
    try await probation.begin(fromVersion: "1.0.0", toVersion: "1.1.0")
    #expect(try await probation.evaluateBoot(currentVersion: "1.2.0") == .stale)
    #expect(storage.record == nil)
}

@Test func fileProbationStorageRoundTrips() throws {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rk-probation-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: dir) }
    let storage = FileBootProbationStorage(url: dir.appendingPathComponent("p.json"))
    #expect(try storage.read() == nil)
    let record = BootProbationRecord(fromVersion: "1.0.0", toVersion: "1.1.0", bootAttempts: 1)
    try storage.write(record)
    #expect(try storage.read() == record)
    try storage.clear()
    #expect(try storage.read() == nil)
}

@Test func tierEligibilityMatchesHostContractVectors() {
    #expect(UpdateTier.patch.isEligible(pro: false))
    #expect(UpdateTier.patch.isEligible(pro: true))
    #expect(!UpdateTier.update.isEligible(pro: false))
    #expect(UpdateTier.update.isEligible(pro: true))
}
